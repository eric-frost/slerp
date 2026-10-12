//! JPEG in DICOM.  8-bit baseline, extended and progressive JPEG and
//! lossless JPEG (processes 14 and 15, 2 to 16 bits) go through
//! `jpeg-decoder`, which Slerp already uses for textures.  12-bit extended
//! JPEG (process 4, common in older CT and MR), which `jpeg-decoder`
//! doesn't read, has its own sequential Huffman decoder here.

use super::{Decoded, Samples};
use crate::image::{MAX_FRAME_SAMPLES as MAX_SAMPLES, Photometric};

/// What a JPEG stream's headers say before decoding.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Header {
    /// The start-of-frame marker (0xC0 baseline, 0xC1 extended, 0xC2
    /// progressive, 0xC3 lossless, ...).
    sof: u8,
    precision: u8,
    width: u16,
    height: u16,
    comps: u8,
    /// Component identifiers are 'R', 'G', 'B'.
    rgb_ids: bool,
    jfif: bool,
    /// The Adobe APP14 marker's transform flag (0 none, 1 YCbCr).
    adobe: Option<u8>,
}

pub(crate) fn header(data: &[u8]) -> Result<Header, String> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return Err("not a JPEG stream".into());
    }
    let mut h = Header::default();
    let mut pos = 2;
    while pos + 4 <= data.len() {
        if data[pos] != 0xFF {
            pos += 1;
            continue;
        }
        let marker = data[pos + 1];
        if marker == 0xFF || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            pos += if marker == 0xFF { 1 } else { 2 };
            continue;
        }
        let len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        let seg = data.get(pos + 4..pos + 2 + len.max(2)).unwrap_or(&[]);
        match marker {
            0xC0..=0xCF if marker != 0xC4 && marker != 0xC8 && marker != 0xCC => {
                if seg.len() < 6 {
                    return Err("short JPEG frame header".into());
                }
                h.sof = marker;
                h.precision = seg[0];
                h.height = u16::from_be_bytes([seg[1], seg[2]]);
                h.width = u16::from_be_bytes([seg[3], seg[4]]);
                h.comps = seg[5];
                h.rgb_ids = seg.len() >= 6 + 9 && seg[6] == b'R' && seg[9] == b'G' && seg[12] == b'B';
                return Ok(h);
            }
            0xE0 if seg.starts_with(b"JFIF\0") => h.jfif = true,
            0xEE if seg.starts_with(b"Adobe") && seg.len() >= 12 => h.adobe = Some(seg[11]),
            0xDA | 0xD9 => break,
            _ => {}
        }
        pos += 2 + len;
    }
    Err("JPEG stream without a frame header".into())
}

/// Decodes a JPEG frame.  `photometric` settles whether three components
/// hold YCbCr or RGB when the stream doesn't say.  `max_size` lets
/// baseline JPEG decode at 1/2, 1/4 or 1/8 scale for thumbnails.
pub fn decode(data: &[u8], photometric: &Photometric, max_size: Option<u32>) -> Result<Decoded, String> {
    let h = header(data)?;
    if (h.width as u64) * (h.height as u64) * (h.comps.max(1) as u64) > MAX_SAMPLES {
        return Err("JPEG frame too large".into());
    }
    if matches!(h.sof, 0xC9..=0xCF) {
        return Err("arithmetic-coded JPEG isn't supported".into());
    }
    match (h.sof, h.precision) {
        // Lossless JPEG of any precision, and 8-bit DCT JPEG.
        (0xC3 | 0xC7, _) | (_, 8) => with_jpeg_decoder(data, &h, photometric, max_size),
        (0xC0 | 0xC1, 12) => decode12(data, &h, photometric, max_size),
        (_, 12) => Err("12-bit progressive JPEG isn't supported".into()),
        (_, p) => Err(format!("{p}-bit DCT JPEG isn't a valid JPEG")),
    }
}

/// Whether three-component DCT data is YCbCr: the Adobe marker if there
/// is one, then component identifiers, then the photometric
/// interpretation (DICOM's own statement), then JFIF.
fn is_ycbcr(h: &Header, photometric: &Photometric) -> bool {
    if let Some(t) = h.adobe {
        return t != 0;
    }
    if h.rgb_ids {
        return false;
    }
    match photometric {
        Photometric::Rgb => h.jfif,
        _ => true,
    }
}

pub(crate) fn with_jpeg_decoder(
    data: &[u8],
    h: &Header,
    photometric: &Photometric,
    max_size: Option<u32>,
) -> Result<Decoded, String> {
    use jpeg_decoder::{ColorTransform, Decoder, PixelFormat};
    let mut dec = Decoder::new(data);
    dec.set_max_decoding_buffer_size(MAX_SAMPLES as usize * 2);
    if h.comps == 3 && h.sof != 0xC3 && h.sof != 0xC7 {
        dec.set_color_transform(if is_ycbcr(h, photometric) { ColorTransform::YCbCr } else { ColorTransform::RGB });
    }
    dec.read_info().map_err(|e| format!("JPEG: {e}"))?;
    if let Some(size) = max_size
        && h.sof != 0xC3
    {
        let _ = dec.scale(size.min(u16::MAX as u32) as u16, size.min(u16::MAX as u32) as u16);
    }
    let pixels = dec.decode().map_err(|e| format!("JPEG: {e}"))?;
    let info = dec.info().ok_or("JPEG without image information")?;
    let (w, h_) = (info.width as u32, info.height as u32);
    let n = (w as usize) * (h_ as usize);
    let (comps, data) = match info.pixel_format {
        PixelFormat::L8 => (1, Samples::U8(pixels)),
        PixelFormat::L16 => (1, Samples::U16(ne_words(&pixels))),
        PixelFormat::RGB24 if pixels.len() == n * 6 => (3, Samples::U16(ne_words(&pixels))),
        PixelFormat::RGB24 => (3, Samples::U8(pixels)),
        PixelFormat::CMYK32 => return Err("CMYK JPEG isn't a DICOM colour space".into()),
    };
    let expected = n * comps as usize;
    let got = match &data {
        Samples::U8(v) => v.len(),
        Samples::U16(v) => v.len(),
        _ => 0,
    };
    if got != expected {
        return Err("JPEG decoded to an unexpected size".into());
    }
    // Lossless JPEG keeps the samples as they were (YBR stays YBR); DCT
    // JPEG was converted to RGB above.
    let lossless = h.sof == 0xC3 || h.sof == 0xC7;
    let precision = if lossless { h.precision as u32 } else { 8 };
    Ok(Decoded { width: w, height: h_, comps, precision, data, rgb: !lossless })
}

fn ne_words(b: &[u8]) -> Vec<u16> {
    b.chunks_exact(2).map(|c| u16::from_ne_bytes([c[0], c[1]])).collect()
}

/*┌─────────────────╮
| 12-bit DCT      |
└─────────────────╯*/
//region 12-bit DCT

#[derive(Clone, Default)]
struct Huffman {
    /// Fast lookup by the next 9 bits: (code length, symbol), length 0
    /// when the code is longer.
    fast: Vec<(u8, u8)>,
    /// Canonical decoding tables (JPEG F.2.2.3): largest code of each
    /// length, offset into `values`, and the symbols.
    maxcode: [i32; 18],
    valptr: [i32; 17],
    mincode: [i32; 17],
    values: Vec<u8>,
}

const FAST_BITS: u32 = 9;

impl Huffman {
    fn new(counts: &[u8; 16], values: &[u8]) -> Result<Huffman, String> {
        let total: usize = counts.iter().map(|&c| c as usize).sum();
        if total > 256 || total > values.len() {
            return Err("bad Huffman table".into());
        }
        let mut h =
            Huffman { fast: vec![(0, 0); 1 << FAST_BITS], values: values[..total].to_vec(), ..Default::default() };
        let mut code = 0i32;
        let mut k = 0usize;
        for len in 1..=16usize {
            let n = counts[len - 1] as usize;
            h.valptr[len] = k as i32;
            h.mincode[len] = code;
            for _ in 0..n {
                if len as u32 <= FAST_BITS {
                    let shift = FAST_BITS - len as u32;
                    let base = (code as usize) << shift;
                    for f in 0..(1usize << shift) {
                        if let Some(slot) = h.fast.get_mut(base + f) {
                            *slot = (len as u8, h.values[k]);
                        }
                    }
                }
                code += 1;
                k += 1;
            }
            h.maxcode[len] = if n > 0 { code - 1 } else { -1 };
            if code > (1 << len) {
                return Err("bad Huffman table".into());
            }
            code <<= 1;
        }
        h.maxcode[17] = i32::MAX;
        Ok(h)
    }
}

/// Reads entropy-coded bits, removing stuffed zero bytes and stopping at
/// markers (feeding zeros past them).
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    count: u32,
    /// Reading ran into a marker or the end.
    hit_marker: bool,
}

impl<'a> BitReader<'a> {
    fn fill(&mut self) {
        while self.count <= 56 {
            let mut byte = 0u8;
            if !self.hit_marker && self.pos < self.data.len() {
                let b = self.data[self.pos];
                if b == 0xFF {
                    let next = self.data.get(self.pos + 1).copied().unwrap_or(0xD9);
                    if next == 0x00 {
                        self.pos += 2;
                        byte = 0xFF;
                    } else {
                        self.hit_marker = true;
                    }
                } else {
                    self.pos += 1;
                    byte = b;
                }
            } else {
                self.hit_marker = true;
            }
            self.acc |= (byte as u64) << (56 - self.count);
            self.count += 8;
        }
    }

    fn peek(&mut self, n: u32) -> u32 {
        if self.count < n {
            self.fill();
        }
        (self.acc >> (64 - n)) as u32
    }

    fn skip(&mut self, n: u32) {
        self.acc = self.acc.checked_shl(n).unwrap_or(0);
        self.count = self.count.saturating_sub(n);
    }

    fn bits(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let v = self.peek(n);
        self.skip(n);
        v
    }

    fn decode(&mut self, h: &Huffman) -> Result<u8, String> {
        let look = self.peek(FAST_BITS) as usize;
        let (len, sym) = h.fast[look];
        if len > 0 {
            self.skip(len as u32);
            return Ok(sym);
        }
        let mut code = self.bits(FAST_BITS) as i32;
        for len in FAST_BITS as usize + 1..=16 {
            code = (code << 1) | self.bits(1) as i32;
            if code <= h.maxcode[len] {
                let i = h.valptr[len] + code - h.mincode[len];
                return h.values.get(i as usize).copied().ok_or_else(|| "bad Huffman code".to_string());
            }
        }
        Err("bad Huffman code".into())
    }

    /// A value of `size` bits, sign-extended the JPEG way (F.2.2.1).
    fn receive_extend(&mut self, size: u8) -> i32 {
        if size == 0 || size > 16 {
            return 0;
        }
        let v = self.bits(size as u32) as i32;
        if v < 1 << (size - 1) { v - (1 << size) + 1 } else { v }
    }

    /// Skips to just past the next restart marker and clears the bits.
    fn restart(&mut self) {
        let mut p = self.pos;
        while p + 1 < self.data.len() && !(self.data[p] == 0xFF && (0xD0..=0xD7).contains(&self.data[p + 1])) {
            p += 1;
        }
        self.pos = (p + 2).min(self.data.len());
        self.acc = 0;
        self.count = 0;
        self.hit_marker = false;
    }
}

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54,
    47, 55, 62, 63,
];

struct Component {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    /// Blocks across and down, padded to whole MCUs.
    bw: usize,
    bh: usize,
    /// Decoded samples, `bw * 8` wide.
    plane: Vec<i32>,
}

/// A double-precision IDCT of one block with the level shift and
/// clamping.  Halves round up, as libjpeg's integer IDCT rounds them; the
/// tiny nudge keeps floating-point error from turning an exact half (a
/// flat block's DC times its quantiser over 8 often is one) into a lower
/// value.
fn idct(coef: &[i32; 64], out: &mut [i32], stride: usize, shift: i32, max: i32, cos: &[[f64; 8]; 8]) {
    let mut tmp = [0f64; 64];
    for u in 0..8 {
        for y in 0..8 {
            let mut s = 0.0;
            for v in 0..8 {
                s += cos[y][v] * coef[v * 8 + u] as f64;
            }
            tmp[y * 8 + u] = s;
        }
    }
    for y in 0..8 {
        for x in 0..8 {
            let mut s = 0.0;
            for u in 0..8 {
                s += cos[x][u] * tmp[y * 8 + u];
            }
            out[y * stride + x] = ((s / 4.0 + 0.5 + 1e-6).floor() as i32 + shift).clamp(0, max);
        }
    }
}

fn cos_table() -> [[f64; 8]; 8] {
    let mut t = [[0f64; 8]; 8];
    for (x, row) in t.iter_mut().enumerate() {
        for (u, c) in row.iter_mut().enumerate() {
            let cu = if u == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
            *c = cu * (((2 * x + 1) as f64 * u as f64 * std::f64::consts::PI) / 16.0).cos();
        }
    }
    t
}

/// Decodes sequential Huffman DCT JPEG of 8 or 12 bits.  With a small
/// `max_size`, only DC coefficients are kept (a 1/8 scale picture),
/// though every coefficient is still read.
pub(crate) fn decode12(
    data: &[u8],
    h: &Header,
    photometric: &Photometric,
    max_size: Option<u32>,
) -> Result<Decoded, String> {
    let (width, height) = (h.width as usize, h.height as usize);
    if width == 0 || height == 0 {
        return Err("JPEG frame without a size".into());
    }
    let dc_only = max_size.is_some_and(|s| (s as usize) * 8 <= width.max(height));
    let precision = h.precision as i32;
    let max = (1 << precision) - 1;
    let shift = 1 << (precision - 1);
    let mut quant = [[0i32; 64]; 4];
    let mut dc_tables: [Option<Huffman>; 4] = Default::default();
    let mut ac_tables: [Option<Huffman>; 4] = Default::default();
    let mut comps: Vec<Component> = Vec::new();
    let (mut hmax, mut vmax) = (1, 1);
    let mut restart_interval = 0usize;
    let cos = cos_table();
    let mut pos = 2;
    let mut scans = 0;
    while pos + 4 <= data.len() {
        if data[pos] != 0xFF {
            pos += 1;
            continue;
        }
        let marker = data[pos + 1];
        if marker == 0xFF || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            pos += if marker == 0xFF { 1 } else { 2 };
            continue;
        }
        if marker == 0xD9 {
            break;
        }
        let len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        if len < 2 {
            return Err("bad JPEG marker length".into());
        }
        let seg = data.get(pos + 4..pos + 2 + len).ok_or("truncated JPEG marker")?;
        match marker {
            0xDB => {
                let mut i = 0;
                while i < seg.len() {
                    let (pq, tq) = (seg[i] >> 4, (seg[i] & 3) as usize);
                    i += 1;
                    for k in 0..64 {
                        let v = if pq == 0 {
                            *seg.get(i + k).ok_or("short quantisation table")? as i32
                        } else {
                            let b = seg.get(i + 2 * k..i + 2 * k + 2).ok_or("short quantisation table")?;
                            u16::from_be_bytes([b[0], b[1]]) as i32
                        };
                        quant[tq][ZIGZAG[k]] = v;
                    }
                    i += if pq == 0 { 64 } else { 128 };
                }
            }
            0xC4 => {
                let mut i = 0;
                while i + 17 <= seg.len() {
                    let (tc, th) = (seg[i] >> 4, (seg[i] & 3) as usize);
                    let counts: [u8; 16] = seg[i + 1..i + 17].try_into().map_err(|_| "short Huffman table")?;
                    let n: usize = counts.iter().map(|&c| c as usize).sum();
                    let values = seg.get(i + 17..i + 17 + n).ok_or("short Huffman table")?;
                    let table = Huffman::new(&counts, values)?;
                    if tc == 0 {
                        dc_tables[th] = Some(table);
                    } else {
                        ac_tables[th] = Some(table);
                    }
                    i += 17 + n;
                }
            }
            0xDD if seg.len() >= 2 => restart_interval = u16::from_be_bytes([seg[0], seg[1]]) as usize,
            0xC0 | 0xC1 => {
                let n = seg[5] as usize;
                if n == 0 || n > 4 || seg.len() < 6 + 3 * n {
                    return Err("bad JPEG frame header".into());
                }
                for i in 0..n {
                    let s = &seg[6 + 3 * i..9 + 3 * i];
                    let (hs, vs) = ((s[1] >> 4) as usize, (s[1] & 15) as usize);
                    if !(1..=4).contains(&hs) || !(1..=4).contains(&vs) {
                        return Err("bad JPEG sampling factors".into());
                    }
                    hmax = hmax.max(hs);
                    vmax = vmax.max(vs);
                    comps.push(Component {
                        id: s[0],
                        h: hs,
                        v: vs,
                        tq: (s[2] & 3) as usize,
                        bw: 0,
                        bh: 0,
                        plane: Vec::new(),
                    });
                }
                let (mcux, mcuy) = (width.div_ceil(8 * hmax), height.div_ceil(8 * vmax));
                for c in &mut comps {
                    c.bw = mcux * c.h;
                    c.bh = mcuy * c.v;
                    c.plane = vec![0; c.bw * 8 * c.bh * 8];
                }
            }
            0xDA => {
                let ns = *seg.first().ok_or("empty JPEG scan header")? as usize;
                if comps.is_empty() || ns == 0 || ns > comps.len() || seg.len() < 1 + 2 * ns {
                    return Err("bad JPEG scan header".into());
                }
                let mut scan = Vec::with_capacity(ns);
                for i in 0..ns {
                    let id = seg[1 + 2 * i];
                    let t = seg[2 + 2 * i];
                    let ci = comps.iter().position(|c| c.id == id).ok_or("scan names an unknown component")?;
                    scan.push((ci, (t >> 4) as usize & 3, (t & 3) as usize));
                }
                for &(_, td, ta) in &scan {
                    if dc_tables[td].is_none() || ac_tables[ta].is_none() {
                        return Err("JPEG scan uses a missing Huffman table".into());
                    }
                }
                let mut br = BitReader { data, pos: pos + 2 + len, acc: 0, count: 0, hit_marker: false };
                let mut pred = vec![0i32; comps.len()];
                // MCUs: interleaved scans step through MCUs of all scan
                // components; a one-component scan steps through that
                // component's blocks.
                let (mcus_x, mcus_y) = if ns == 1 {
                    let c = &comps[scan[0].0];
                    ((width * c.h).div_ceil(8 * hmax), (height * c.v).div_ceil(8 * vmax))
                } else {
                    (width.div_ceil(8 * hmax), height.div_ceil(8 * vmax))
                };
                let mut block = [0i32; 64];
                let mut done = 0usize;
                for my in 0..mcus_y {
                    for mx in 0..mcus_x {
                        if restart_interval > 0 && done > 0 && done.is_multiple_of(restart_interval) {
                            br.restart();
                            pred.iter_mut().for_each(|p| *p = 0);
                        }
                        done += 1;
                        for &(ci, td, ta) in &scan {
                            let (bh_, bv_) = if ns == 1 { (1, 1) } else { (comps[ci].h, comps[ci].v) };
                            for by in 0..bv_ {
                                for bx in 0..bh_ {
                                    let dc = dc_tables[td].as_ref().ok_or("missing table")?;
                                    let ac = ac_tables[ta].as_ref().ok_or("missing table")?;
                                    block.fill(0);
                                    let s = br.decode(dc)?;
                                    pred[ci] += br.receive_extend(s);
                                    block[0] = pred[ci] * quant[comps[ci].tq][0];
                                    let mut k = 1;
                                    while k < 64 {
                                        let rs = br.decode(ac)?;
                                        let (r, s) = ((rs >> 4) as usize, rs & 15);
                                        if s == 0 {
                                            if r == 15 {
                                                k += 16;
                                                continue;
                                            }
                                            break;
                                        }
                                        k += r;
                                        if k > 63 {
                                            break;
                                        }
                                        let v = br.receive_extend(s);
                                        if !dc_only {
                                            block[ZIGZAG[k]] = v * quant[comps[ci].tq][ZIGZAG[k]];
                                        }
                                        k += 1;
                                    }
                                    let c = &mut comps[ci];
                                    let (bxi, byi) = if ns == 1 { (mx, my) } else { (mx * c.h + bx, my * c.v + by) };
                                    if bxi >= c.bw || byi >= c.bh {
                                        continue;
                                    }
                                    let stride = c.bw * 8;
                                    let at = byi * 8 * stride + bxi * 8;
                                    if dc_only {
                                        // The block's mean: DC / 8 in this scaling.
                                        let v = ((block[0] as f64 / 8.0 + 0.5).floor() as i32 + shift).clamp(0, max);
                                        c.plane[at] = v;
                                    } else {
                                        idct(&block, &mut c.plane[at..], stride, shift, max, &cos);
                                    }
                                }
                            }
                        }
                    }
                }
                scans += 1;
                // Continue after the scan's data: find the next marker.
                let mut p = br.pos;
                while p + 1 < data.len()
                    && !(data[p] == 0xFF && data[p + 1] != 0 && !(0xD0..=0xD7).contains(&data[p + 1]))
                {
                    p += 1;
                }
                pos = p;
                continue;
            }
            0xC2 | 0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                return Err("only sequential 12-bit JPEG is supported".into());
            }
            _ => {}
        }
        pos += 2 + len;
    }
    if scans == 0 || comps.is_empty() {
        return Err("JPEG without image data".into());
    }

    // Assemble: upsample each component to full size (nearest), and turn
    // YCbCr into RGB.
    let step = if dc_only { 8 } else { 1 };
    let (ow, oh) = (width.div_ceil(step), height.div_ceil(step));
    let n = comps.len().min(3);
    let mut out = vec![0u16; ow * oh * n];
    for (ci, c) in comps.iter().take(n).enumerate() {
        let stride = c.bw * 8;
        for y in 0..oh {
            let sy = (y * step * c.v / vmax).min(c.bh * 8 - 1);
            let sy = if dc_only { sy / 8 * 8 } else { sy };
            for x in 0..ow {
                let sx = (x * step * c.h / hmax).min(stride - 1);
                let sx = if dc_only { sx / 8 * 8 } else { sx };
                out[(y * ow + x) * n + ci] = c.plane[sy * stride + sx] as u16;
            }
        }
    }
    if n == 3 && is_ycbcr(h, photometric) {
        let half = (1 << (precision - 1)) as f32;
        for p in out.chunks_exact_mut(3) {
            let (y, cb, cr) = (p[0] as f32, p[1] as f32 - half, p[2] as f32 - half);
            let m = max as f32;
            p[0] = (y + 1.402 * cr).round().clamp(0.0, m) as u16;
            p[1] = (y - 0.344136 * cb - 0.714136 * cr).round().clamp(0.0, m) as u16;
            p[2] = (y + 1.772 * cb).round().clamp(0.0, m) as u16;
        }
    }
    Ok(Decoded {
        width: ow as u32,
        height: oh as u32,
        comps: n as u32,
        precision: precision as u32,
        data: Samples::U16(out),
        rgb: true,
    })
}

//endregion
