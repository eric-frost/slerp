//! Chunk decompression for TIFF: LZW, Deflate, PackBits, Zstandard, CCITT
//! fax, JPEG and JPEG 2000, and the predictors.  Every decoder is given the size it must
//! produce and refuses to produce more, so a damaged file can't make a chunk
//! allocate without bound.

/// Decodes TIFF's LZW (most significant bit first, "early change").  Stops at
/// `expected` bytes.
pub fn lzw(input: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    // The table holds (previous code, byte) for codes from 258.
    let mut prefix = vec![0u16; 4096];
    let mut suffix = vec![0u8; 4096];
    let mut out: Vec<u8> = Vec::with_capacity(expected.min(1 << 26));
    let (mut bitbuf, mut nbits, mut pos) = (0u32, 0u32, 0usize);
    let mut width = 9u32;
    let mut next = 258usize;
    let mut prev: Option<usize> = None;
    let mut stack: Vec<u8> = Vec::with_capacity(4096);
    // Writes the string of `code` and returns its first byte.
    let emit = |code: usize, out: &mut Vec<u8>, stack: &mut Vec<u8>, prefix: &[u16], suffix: &[u8]| -> u8 {
        stack.clear();
        let mut c = code;
        while c >= 258 {
            stack.push(suffix[c]);
            c = prefix[c] as usize;
        }
        stack.push(c as u8);
        let first = *stack.last().unwrap_or(&0);
        out.extend(stack.iter().rev());
        first
    };
    while out.len() < expected {
        while nbits < width {
            let Some(&b) = input.get(pos) else {
                // Ran out without an end code: keep what was decoded.
                return if out.is_empty() { Err("the LZW data is empty".into()) } else { Ok(out) };
            };
            bitbuf = (bitbuf << 8) | b as u32;
            nbits += 8;
            pos += 1;
        }
        let code = ((bitbuf >> (nbits - width)) & ((1 << width) - 1)) as usize;
        nbits -= width;
        if code == 257 {
            break;
        }
        if code == 256 {
            next = 258;
            width = 9;
            prev = None;
            continue;
        }
        match prev {
            None => {
                if code >= 256 {
                    return Err("the LZW data starts with a table code".into());
                }
                out.push(code as u8);
            }
            Some(p) => {
                let first;
                if code < next {
                    first = emit(code, &mut out, &mut stack, &prefix, &suffix);
                } else if code == next {
                    // The string of the previous code plus its own first byte.
                    let f = emit(p, &mut out, &mut stack, &prefix, &suffix);
                    out.push(f);
                    first = f;
                } else {
                    return Err("an LZW code is past the table".into());
                }
                if next < 4096 {
                    prefix[next] = p as u16;
                    suffix[next] = first;
                    next += 1;
                    width = match next {
                        0..=510 => 9,
                        511..=1022 => 10,
                        1023..=2046 => 11,
                        _ => 12,
                    };
                }
            }
        }
        prev = Some(code);
    }
    out.truncate(expected);
    Ok(out)
}

/// Decodes PackBits run-length data.
pub fn packbits(input: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(expected.min(1 << 26));
    let mut i = 0;
    while i < input.len() && out.len() < expected {
        let n = input[i] as i8;
        i += 1;
        if n >= 0 {
            let len = n as usize + 1;
            let end = (i + len).min(input.len());
            out.extend_from_slice(&input[i..end]);
            i = end;
        } else if n != -128 {
            let Some(&b) = input.get(i) else { break };
            i += 1;
            out.extend(std::iter::repeat_n(b, (1 - n as isize) as usize));
        }
    }
    out.truncate(expected);
    Ok(out)
}

/// Inflates zlib data, up to `expected` bytes.  A stream cut short still
/// gives the rows it had.
pub fn deflate(input: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    // A zlib header: deflate with a window up to 32 KB, and a check value.
    let valid_header = input.len() >= 2
        && input[0] & 0x0f == 8
        && input[0] >> 4 <= 7
        && (u16::from(input[0]) << 8 | u16::from(input[1])) % 31 == 0;
    if !valid_header {
        return Err("deflate: the data doesn't start with a zlib header".into());
    }
    match miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(input, expected) {
        Ok(v) => Ok(v),
        Err(e) if !e.output.is_empty() => Ok(e.output),
        Err(e) => Err(format!("deflate: {:?}", e.status)),
    }
}

/// Decompresses Zstandard data to at most `expected` bytes.
pub fn zstd(input: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    zstd::bulk::decompress(input, expected).map_err(|e| format!("zstd: {e}"))
}

/*┌─────────────────╮
| Predictors      |
└─────────────────╯*/
//region Predictors

/// Undoes the horizontal predictor (2) on rows of `row_samples` samples of
/// `bytes` bytes each (1, 2, 4 or 8, little-endian), `spp` samples to a
/// pixel.
pub fn unpredict_horizontal(data: &mut [u8], row_samples: usize, spp: usize, bytes: usize) {
    let row_len = row_samples * bytes;
    if row_len == 0 {
        return;
    }
    for row in data.chunks_exact_mut(row_len) {
        match bytes {
            1 => {
                for i in spp..row_samples {
                    row[i] = row[i].wrapping_add(row[i - spp]);
                }
            }
            2 => {
                for i in spp..row_samples {
                    let a = u16::from_le_bytes([row[i * 2], row[i * 2 + 1]]);
                    let b = u16::from_le_bytes([row[(i - spp) * 2], row[(i - spp) * 2 + 1]]);
                    row[i * 2..i * 2 + 2].copy_from_slice(&a.wrapping_add(b).to_le_bytes());
                }
            }
            4 => {
                for i in spp..row_samples {
                    let a = u32::from_le_bytes(row[i * 4..i * 4 + 4].try_into().unwrap_or([0; 4]));
                    let b = u32::from_le_bytes(row[(i - spp) * 4..(i - spp) * 4 + 4].try_into().unwrap_or([0; 4]));
                    row[i * 4..i * 4 + 4].copy_from_slice(&a.wrapping_add(b).to_le_bytes());
                }
            }
            8 => {
                for i in spp..row_samples {
                    let a = u64::from_le_bytes(row[i * 8..i * 8 + 8].try_into().unwrap_or([0; 8]));
                    let b = u64::from_le_bytes(row[(i - spp) * 8..(i - spp) * 8 + 8].try_into().unwrap_or([0; 8]));
                    row[i * 8..i * 8 + 8].copy_from_slice(&a.wrapping_add(b).to_le_bytes());
                }
            }
            _ => {}
        }
    }
}

/// Undoes the floating-point predictor (3): each row is stored as byte planes,
/// most significant first, with the bytes differenced.  Result is
/// little-endian samples.
pub fn unpredict_float(data: &mut [u8], row_samples: usize, spp: usize, bytes: usize) {
    let row_len = row_samples * bytes;
    if row_len == 0 || bytes == 0 {
        return;
    }
    let mut tmp = vec![0u8; row_len];
    for row in data.chunks_exact_mut(row_len) {
        for i in spp..row_len {
            row[i] = row[i].wrapping_add(row[i - spp]);
        }
        tmp.copy_from_slice(row);
        for s in 0..row_samples {
            for b in 0..bytes {
                row[s * bytes + (bytes - 1 - b)] = tmp[b * row_samples + s];
            }
        }
    }
}

/// Reverses the byte order of every `bytes`-wide sample.
pub fn swap_bytes(data: &mut [u8], bytes: usize) {
    if bytes > 1 {
        for s in data.chunks_exact_mut(bytes) {
            s.reverse();
        }
    }
}

//endregion

/*┌─────────────────╮
| JPEG            |
└─────────────────╯*/
//region JPEG

/// A decoded JPEG or JPEG 2000 chunk: interleaved 8-bit samples (16-bit
/// little-endian where `bytes` is 2).
pub struct Pixels {
    pub width: usize,
    pub height: usize,
    pub comps: usize,
    pub bytes: usize,
    pub data: Vec<u8>,
}

/// Joins a TIFF `JPEGTables` stream and a chunk's abbreviated stream into one
/// the decoder reads.
pub fn join_jpeg(tables: &[u8], data: &[u8]) -> Vec<u8> {
    let t = tables.strip_suffix(&[0xff, 0xd9]).unwrap_or(tables);
    let d = data.strip_prefix(&[0xff, 0xd8]).unwrap_or(data);
    let mut v = Vec::with_capacity(t.len() + d.len());
    v.extend_from_slice(t);
    v.extend_from_slice(d);
    v
}

/// Decodes CCITT fax data (Group 3 one-dimensional, or Group 4) into `height`
/// rows of `width` bytes, 1 for black and 0 for white.  A chunk that ends
/// early leaves the rest white.
pub fn ccitt(data: &[u8], group4: bool, t4_options: u32, width: usize, height: usize) -> Result<Vec<u8>, String> {
    use fax::Color;
    use fax::decoder::{decode_g3, decode_g4, pels};
    if width == 0 || width > u16::MAX as usize || height == 0 || height > u16::MAX as usize {
        return Err("a fax strip's size is out of range".into());
    }
    let mut out = vec![0u8; width * height];
    let mut row = 0usize;
    let mut line = |t: &[u16]| {
        if row < height {
            for (px, c) in out[row * width..(row + 1) * width].iter_mut().zip(pels(t, width as u16)) {
                *px = (c == Color::Black) as u8;
            }
            row += 1;
        }
    };
    let input = data.iter().copied();
    // Damaged data ends the picture where it breaks: the rows before are kept.
    let ended = if group4 {
        decode_g4(input, width as u16, Some(height as u16), &mut line)
    } else {
        if t4_options & 1 != 0 {
            return Err("two-dimensional Group 3 fax data isn't supported".into());
        }
        decode_g3(input, &mut line)
    };
    if ended.is_none() && row == 0 {
        return Err(format!("the Group {} fax data is damaged", if group4 { 4 } else { 3 }));
    }
    Ok(out)
}

/// Converts subsampled YCbCr data (the units TIFF stores: `h` by `v` luma
/// samples, then one Cb and one Cr) to interleaved RGB.  `coefficients`
/// are the luma weights of red, green and blue and `rbw` the reference black
/// and white points of Y, Cb and Cr.
pub fn ycbcr_to_rgb(
    raw: &[u8],
    width: usize,
    height: usize,
    (h, v): (usize, usize),
    coefficients: [f64; 3],
    rbw: [f64; 6],
) -> Result<Vec<u8>, String> {
    if !matches!(h, 1 | 2 | 4) || !matches!(v, 1 | 2 | 4) {
        return Err(format!("YCbCr subsampling {h} by {v} isn't supported"));
    }
    let (units_x, units_y) = (width.div_ceil(h), height.div_ceil(v));
    let unit = h * v + 2;
    if raw.len() < units_x * units_y * unit {
        return Err("the YCbCr data is short".into());
    }
    let [kr, kg, kb] = coefficients;
    // Scale each component from its reference range to 0..255 (Y) or -127..127 (chroma).
    let y_scale = 255.0 / (rbw[1] - rbw[0]).max(1.0);
    let cb_scale = 127.0 / (rbw[3] - rbw[2]).max(1.0);
    let cr_scale = 127.0 / (rbw[5] - rbw[4]).max(1.0);
    let mut out = vec![0u8; width * height * 3];
    for uy in 0..units_y {
        for ux in 0..units_x {
            let u = &raw[(uy * units_x + ux) * unit..][..unit];
            let cb = (u[h * v] as f64 - rbw[2]) * cb_scale;
            let cr = (u[h * v + 1] as f64 - rbw[4]) * cr_scale;
            for dy in 0..v {
                for dx in 0..h {
                    let (x, y) = (ux * h + dx, uy * v + dy);
                    if x >= width || y >= height {
                        continue;
                    }
                    let yy = (u[dy * h + dx] as f64 - rbw[0]) * y_scale;
                    let r = yy + cr * (2.0 - 2.0 * kr);
                    let b = yy + cb * (2.0 - 2.0 * kb);
                    let g = (yy - kb * b - kr * r) / kg;
                    let o = &mut out[(y * width + x) * 3..][..3];
                    o[0] = r.round().clamp(0.0, 255.0) as u8;
                    o[1] = g.round().clamp(0.0, 255.0) as u8;
                    o[2] = b.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
    Ok(out)
}

/// Decodes a JPEG chunk, at 1/`scale` size when the decoder can (`scale` 1,
/// 2, 4 or 8).  `max_samples` caps the output.  `rgb` says three-component
/// data is RGB, not YCbCr.
pub fn jpeg(tables: Option<&[u8]>, data: &[u8], scale: usize, max_samples: usize, rgb: bool) -> Result<Pixels, String> {
    use jpeg_decoder::{ColorTransform, Decoder, PixelFormat};
    let joined;
    let bytes: &[u8] = match tables {
        Some(t) if !t.is_empty() => {
            joined = join_jpeg(t, data);
            &joined
        }
        _ => data,
    };
    let mut dec = Decoder::new(bytes);
    dec.set_max_decoding_buffer_size(max_samples.saturating_mul(2));
    // A page the TIFF calls RGB holds RGB samples in its JPEG (Aperio writes
    // "JPEG/RGB"); the decoder would otherwise guess YCbCr.
    if rgb {
        dec.set_color_transform(ColorTransform::RGB);
    }
    dec.read_info().map_err(|e| format!("JPEG: {e}"))?;
    if scale > 1 {
        let info = dec.info().ok_or("JPEG without image information")?;
        let (w, h) = (info.width as usize, info.height as usize);
        let _ = dec.scale(
            w.div_ceil(scale).max(1).min(u16::MAX as usize) as u16,
            h.div_ceil(scale).max(1).min(u16::MAX as usize) as u16,
        );
    }
    let pixels = dec.decode().map_err(|e| format!("JPEG: {e}"))?;
    let info = dec.info().ok_or("JPEG without image information")?;
    let (comps, bytes_per) = match info.pixel_format {
        PixelFormat::L8 => (1, 1),
        PixelFormat::L16 => (1, 2),
        PixelFormat::RGB24 => (3, 1),
        PixelFormat::CMYK32 => (4, 1),
    };
    let (w, h) = (info.width as usize, info.height as usize);
    if pixels.len() != w * h * comps * bytes_per {
        return Err("JPEG decoded to an unexpected size".into());
    }
    let mut data = pixels;
    if bytes_per == 2 {
        // jpeg-decoder gives 16-bit samples big-endian.
        swap_bytes(&mut data, 2);
    }
    Ok(Pixels { width: w, height: h, comps, bytes: bytes_per, data })
}

/// Decodes a JPEG 2000 codestream (Aperio's compression 33003 and 33005, and
/// 34712).  `ycc` means the three components are Y, Cb and Cr.
pub fn jpeg2000(data: &[u8], ycc: bool, scale: usize, max_samples: usize) -> Result<Pixels, String> {
    use hayro_jpeg2000::{DecodeSettings, DecoderContext, Image};
    let start = data.windows(4).take(256).position(|w| w == [0xFF, 0x4F, 0xFF, 0x51] || w == [0, 0, 0, 0x0C]);
    let data = &data[start.unwrap_or(0)..];
    let settings = DecodeSettings { resolve_palette_indices: true, strict: false, target_resolution: None };
    let image = Image::new(data, &settings).map_err(|e| format!("JPEG 2000: {e}"))?;
    let (w, h) = (image.width() as usize, image.height() as usize);
    let _ = scale;
    if w == 0 || h == 0 || w.saturating_mul(h) > max_samples {
        return Err("JPEG 2000 tile too large".into());
    }
    let mut ctx = DecoderContext::default();
    let decoded = image.decode(&mut ctx).map_err(|e| format!("JPEG 2000: {e}"))?;
    let planes = decoded.components();
    let n = planes.len().min(4);
    if n == 0 || n == 2 {
        return Err(format!("JPEG 2000 with {} components", planes.len()));
    }
    if planes.iter().take(n).any(|p| p.samples().len() != w * h) {
        return Err("JPEG 2000 components of different sizes".into());
    }
    let precision = planes[0].bit_depth() as u32;
    let sixteen = precision > 8;
    let mut out = vec![0u8; w * h * n * if sixteen { 2 } else { 1 }];
    let maxv = ((1u64 << precision.min(16)) - 1) as f32;
    for (c, plane) in planes.iter().take(n).enumerate() {
        for (i, &v) in plane.samples().iter().enumerate() {
            let v = v.round().clamp(0.0, maxv);
            if sixteen {
                let o = (i * n + c) * 2;
                out[o..o + 2].copy_from_slice(&(v as u16).to_le_bytes());
            } else {
                out[i * n + c] = v as u8;
            }
        }
    }
    if ycc && n >= 3 && !sixteen {
        for px in out.chunks_exact_mut(n) {
            let (y, cb, cr) = (px[0] as f32, px[1] as f32 - 128.0, px[2] as f32 - 128.0);
            px[0] = (y + 1.402 * cr).round().clamp(0.0, 255.0) as u8;
            px[1] = (y - 0.344136 * cb - 0.714136 * cr).round().clamp(0.0, 255.0) as u8;
            px[2] = (y + 1.772 * cb).round().clamp(0.0, 255.0) as u8;
        }
    }
    Ok(Pixels { width: w, height: h, comps: n, bytes: if sixteen { 2 } else { 1 }, data: out })
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    /// TIFF LZW, encoded here the way libtiff writes it.
    fn lzw_encode(input: &[u8]) -> Vec<u8> {
        use std::collections::HashMap;
        let mut out = Vec::new();
        let (mut buf, mut nb) = (0u64, 0u32);
        let mut put = |code: u32, width: u32, out: &mut Vec<u8>| {
            buf = (buf << width) | code as u64;
            nb += width;
            while nb >= 8 {
                out.push((buf >> (nb - 8)) as u8);
                nb -= 8;
            }
        };
        let mut table: HashMap<(u32, u8), u32> = HashMap::new();
        let mut next = 258u32;
        let mut width = 9u32;
        put(256, width, &mut out);
        let mut cur: Option<u32> = None;
        for &b in input {
            match cur {
                None => cur = Some(b as u32),
                Some(c) => {
                    if let Some(&code) = table.get(&(c, b)) {
                        cur = Some(code);
                    } else {
                        put(c, width, &mut out);
                        table.insert((c, b), next);
                        next += 1;
                        // The encoder is one code ahead of the decoder, so it widens a code later.
                        if next == 512 {
                            width = 10
                        } else if next == 1024 {
                            width = 11
                        } else if next == 2048 {
                            width = 12
                        }
                        if next >= 4094 {
                            put(256, width, &mut out);
                            table.clear();
                            next = 258;
                            width = 9;
                        }
                        cur = Some(b as u32);
                    }
                }
            }
        }
        if let Some(c) = cur {
            put(c, width, &mut out);
        }
        put(257, width, &mut out);
        if nb > 0 {
            out.push((buf << (8 - nb)) as u8);
        }
        out
    }

    #[test]
    fn lzw_round_trips_text_noise_and_long_runs() {
        let text: Vec<u8> = b"to be or not to be, that is the question ".repeat(300);
        let mut noise = vec![0u8; 20000];
        let mut s = 7u32;
        for b in &mut noise {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            *b = (s >> 24) as u8;
        }
        let run = vec![9u8; 100_000];
        for data in [text, noise, run] {
            let enc = lzw_encode(&data);
            assert_eq!(lzw(&enc, data.len()).unwrap(), data);
            // A smaller limit stops early without error.
            assert_eq!(lzw(&enc, 10).unwrap(), data[..10]);
        }
    }

    #[test]
    fn lzw_survives_garbage() {
        let mut s = 99u32;
        for len in [0usize, 1, 2, 5, 100, 5000] {
            let junk: Vec<u8> = (0..len)
                .map(|_| {
                    s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                    (s >> 24) as u8
                })
                .collect();
            let _ = lzw(&junk, 1 << 16);
        }
    }

    #[test]
    fn packbits_decodes_runs_and_literals() {
        let enc = [2, 1, 2, 3, 0xfd, 7, 128, 0, 9];
        assert_eq!(packbits(&enc, 100).unwrap(), vec![1, 2, 3, 7, 7, 7, 7, 9]);
        assert_eq!(packbits(&enc, 4).unwrap(), vec![1, 2, 3, 7]);
        assert!(packbits(&[5], 10).is_ok());
    }

    #[test]
    fn horizontal_predictor_accumulates_per_channel() {
        // Two RGB pixels per row, 8-bit: differences from the previous pixel.
        let mut d = vec![10, 20, 30, 1, 2, 3, 100, 100, 100, 255, 0, 1];
        unpredict_horizontal(&mut d, 6, 3, 1);
        assert_eq!(d, vec![10, 20, 30, 11, 22, 33, 100, 100, 100, 99, 100, 101]);
        // 16-bit single channel.
        let mut d: Vec<u8> = [100u16, 5, 0xffff].iter().flat_map(|v| v.to_le_bytes()).collect();
        unpredict_horizontal(&mut d, 3, 1, 2);
        let v: Vec<u16> = d.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(v, vec![100, 105, 104]);
    }

    #[test]
    fn float_predictor_rebuilds_samples() {
        // Encode two f32 samples with the predictor by hand.
        let vals = [1.5f32, 2.25];
        let be: Vec<[u8; 4]> = vals.iter().map(|v| v.to_be_bytes()).collect();
        let mut planes = vec![0u8; 8];
        for b in 0..4 {
            for s in 0..2 {
                planes[b * 2 + s] = be[s][b];
            }
        }
        // Difference neighbouring bytes (stride 1 sample).
        let mut enc = planes.clone();
        for i in (1..8).rev() {
            enc[i] = enc[i].wrapping_sub(enc[i - 1]);
        }
        unpredict_float(&mut enc, 2, 1, 4);
        let out: Vec<f32> = enc.chunks(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        assert_eq!(out, vals);
    }

    #[test]
    fn deflate_and_zstd_respect_the_expected_size() {
        let data = vec![5u8; 10_000];
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&data, 6);
        assert_eq!(deflate(&z, 10_000).unwrap(), data);
        assert_eq!(deflate(&z, 100).unwrap().len(), 100);
        let zs = zstd::bulk::compress(&data, 3).unwrap();
        assert_eq!(zstd(&zs, 10_000).unwrap(), data);
        assert!(zstd(&zs, 100).is_err());
        assert!(deflate(b"nonsense", 100).is_err());
    }

    #[test]
    fn jpeg_tables_join_cleanly() {
        let t = [0xff, 0xd8, 1, 2, 3, 0xff, 0xd9];
        let d = [0xff, 0xd8, 9, 9, 0xff, 0xd9];
        assert_eq!(join_jpeg(&t, &d), vec![0xff, 0xd8, 1, 2, 3, 9, 9, 0xff, 0xd9]);
    }
}
