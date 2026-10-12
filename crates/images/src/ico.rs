//! Windows icons (`.ico`) and cursors (`.cur`): a directory of pictures of
//! different sizes, each either a PNG file or a device-independent bitmap
//! (a BMP without its file header) with a one-bit AND mask giving the
//! transparency that old icons without an alpha channel need.
//!
//! Every entry becomes a picture of its own, largest first, so the viewer
//! can show a favicon's 16, 32 and 48 pixel versions side by side.

use crate::animation::MAX_CANVAS_PIXELS;
use crate::apng::expand;
use std::io::Cursor;

/// One picture of an icon file.
pub struct Entry {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Bits per pixel as the entry's bitmap says, or the PNG's depth times
    /// channels.
    pub bits: u16,
    pub png: bool,
    /// A cursor's hot spot.
    pub hotspot: Option<(u16, u16)>,
}

pub struct IconFile {
    pub cursor: bool,
    pub entries: Vec<Entry>,
    pub warnings: Vec<String>,
}

/// The most pixels all of one file's pictures may add up to.
const MAX_TOTAL_PIXELS: u64 = 64 << 20;

pub fn is_icon(head: &[u8]) -> bool {
    // Reserved 0, type 1 (icon) or 2 (cursor), at least one picture.  The
    // first entry has to be plausible too: this shape is easy to meet by
    // chance in other formats.
    if head.len() < 22 || head[0] != 0 || head[1] != 0 || !matches!(head[2], 1 | 2) || head[3] != 0 {
        return false;
    }
    let count = u16::from_le_bytes([head[4], head[5]]);
    if count == 0 || count > 1024 {
        return false;
    }
    let size = u32::from_le_bytes([head[14], head[15], head[16], head[17]]);
    let offset = u32::from_le_bytes([head[18], head[19], head[20], head[21]]);
    // The first picture starts after the directory and is not empty.
    head[9] == 0 && size >= 8 && offset as usize >= 6 + 16 * count as usize
}

fn le16(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn le32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// Reads every picture of an icon or cursor file.
pub fn parse(d: &[u8]) -> Result<IconFile, String> {
    if !is_icon(d.get(..22).unwrap_or(d)) {
        return Err("this isn't an icon file".into());
    }
    let cursor = d[2] == 2;
    let count = le16(d, 4).unwrap_or(0) as usize;
    let mut file = IconFile { cursor, entries: Vec::new(), warnings: Vec::new() };
    let mut total = 0u64;
    let mut skipped = 0;
    for i in 0..count {
        let e = 6 + i * 16;
        let (Some(_stated_size), Some(offset)) = (le32(d, e + 8), le32(d, e + 12)) else {
            file.warnings.push("The file ends inside the directory".into());
            break;
        };
        let hotspot = cursor.then(|| (le16(d, e + 4).unwrap_or(0), le16(d, e + 6).unwrap_or(0)));
        let offset = offset as usize;
        // The size the directory states is wrong often enough that the bitmap's
        // own header is trusted instead: the picture is read from its offset on.
        let Some(data) = d.get(offset..).filter(|r| !r.is_empty()) else {
            skipped += 1;
            continue;
        };
        // The directory's height (0 means 256) says whether the bitmap holds a mask.
        let hint = match d.get(e + 1).copied() {
            Some(0) | None => 256,
            Some(h) => h as usize,
        };
        match decode_entry(data, hint) {
            Ok(mut entry) => {
                let pixels = entry.width as u64 * entry.height as u64;
                total += pixels;
                if total > MAX_TOTAL_PIXELS {
                    file.warnings.push("The icon holds more pictures than are shown".into());
                    break;
                }
                entry.hotspot = hotspot;
                file.entries.push(entry);
            }
            Err(msg) => {
                skipped += 1;
                if file.warnings.len() < 4 {
                    file.warnings.push(format!("Picture {} couldn't be read: {msg}", i + 1));
                }
            }
        }
    }
    if file.entries.is_empty() {
        return Err(file.warnings.first().cloned().unwrap_or_else(|| "the icon holds no readable pictures".into()));
    }
    if skipped > 4 {
        file.warnings.push(format!("{skipped} pictures were skipped"));
    }
    // Largest first, and among equals the deepest.
    file.entries.sort_by_key(|e| std::cmp::Reverse((e.width as u64 * e.height as u64, e.bits)));
    Ok(file)
}

fn decode_entry(data: &[u8], hint_height: usize) -> Result<Entry, String> {
    if data.starts_with(b"\x89PNG") {
        return decode_png(data);
    }
    decode_dib(data, hint_height)
}

pub(crate) fn decode_png(data: &[u8]) -> Result<Entry, String> {
    let limits = png::Limits { bytes: 1 << 28 };
    let mut dec = png::Decoder::new_with_limits(Cursor::new(data), limits);
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let info = reader.info();
    let (w, h) = (info.width, info.height);
    let bits = info.bit_depth as u16 * info.color_type.samples() as u16;
    if w == 0 || h == 0 || w as u64 * h as u64 > MAX_CANVAS_PIXELS {
        return Err(format!("{w} × {h} is not a plausible icon size"));
    }
    let mut buf = vec![0u8; reader.output_buffer_size().ok_or("too large")?];
    let out = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(out.buffer_size());
    let rgba = expand(&buf, reader.output_color_type().0.samples(), w as usize * h as usize)?;
    Ok(Entry { width: w, height: h, rgba, bits, png: true, hotspot: None })
}

fn decode_dib(d: &[u8], hint_height: usize) -> Result<Entry, String> {
    let header = le32(d, 0).ok_or("short bitmap")? as usize;
    if header < 40 || header > d.len() {
        return Err("an unsupported bitmap header".into());
    }
    let width = le32(d, 4).ok_or("short bitmap")? as i32;
    let raw_height = le32(d, 8).ok_or("short bitmap")? as i32;
    let bits = le16(d, 14).ok_or("short bitmap")?;
    let compression = le32(d, 16).ok_or("short bitmap")?;
    let used = le32(d, 32).ok_or("short bitmap")? as usize;
    if width <= 0 || width > 4096 || raw_height == 0 || raw_height.unsigned_abs() > 8192 {
        return Err(format!("a bitmap of {width} × {raw_height} is not a plausible icon"));
    }
    if !matches!(bits, 1 | 4 | 8 | 16 | 24 | 32) {
        return Err(format!("{bits}-bit bitmaps aren't supported"));
    }
    if !matches!(compression, 0 | 3) {
        return Err("compressed icon bitmaps aren't supported".into());
    }
    let top_down = raw_height < 0;
    let w = width as usize;
    let total_h = raw_height.unsigned_abs() as usize;
    // 1. Where the pieces are.
    let mut at = header;
    // Bit fields for 16 and 32-bit bitmaps follow a 40-byte header.
    let mut masks = [0u32; 4];
    if compression == 3 {
        if header == 40 {
            for (i, m) in masks.iter_mut().take(3).enumerate() {
                *m = le32(d, 40 + i * 4).ok_or("short bitmap")?;
            }
            at += 12;
        } else {
            for (i, m) in masks.iter_mut().enumerate() {
                *m = le32(d, 40 + i * 4).unwrap_or(0);
            }
        }
    }
    let palette_len = if bits <= 8 { if used == 0 { 1usize << bits } else { used.min(1 << bits) } } else { 0 };
    let palette = d.get(at..at + palette_len * 4).ok_or("short colour table")?;
    at += palette_len * 4;
    let xor_stride = (w * bits as usize).div_ceil(32) * 4;
    let and_stride = w.div_ceil(32) * 4;
    // 2. The height: icons store twice the picture's height, the second half
    //    being the mask; a few tools store only the picture.
    let rest = d.len().saturating_sub(at);
    let h = if total_h.is_multiple_of(2) && rest >= (xor_stride + and_stride) * (total_h / 2) {
        total_h / 2
    } else if total_h.is_multiple_of(2) && total_h / 2 == hint_height {
        // The directory agrees that the picture is half as tall: the mask is missing.
        total_h / 2
    } else {
        total_h
    };
    if h == 0 || w as u64 * h as u64 > 1 << 24 {
        return Err("an implausible bitmap size".into());
    }
    let xor = d.get(at..).unwrap_or(&[]);
    // Pixels the file lacks stay transparent; a short bitmap keeps its rows.
    let mask_at = xor_stride * h;
    let has_mask = xor.len() >= mask_at + and_stride * h;
    let row_of = |y: usize| if top_down { y } else { h - 1 - y };
    let mut rgba = vec![0u8; w * h * 4];
    let mut any_alpha = false;
    for y in 0..h {
        let r = row_of(y);
        let Some(row) = xor.get(r * xor_stride..(r + 1) * xor_stride) else { continue };
        for x in 0..w {
            let px = &mut rgba[(y * w + x) * 4..][..4];
            match bits {
                1 | 4 | 8 => {
                    let idx = match bits {
                        8 => row[x] as usize,
                        4 => ((row[x / 2] >> (if x % 2 == 0 { 4 } else { 0 })) & 15) as usize,
                        _ => ((row[x / 8] >> (7 - x % 8)) & 1) as usize,
                    };
                    if let Some(c) = palette.get(idx * 4..idx * 4 + 3) {
                        px.copy_from_slice(&[c[2], c[1], c[0], 255]);
                    }
                }
                16 => {
                    let v = u16::from_le_bytes([row[x * 2], row[x * 2 + 1]]) as u32;
                    let (r5, g5, b5) = if compression == 3 && masks[1] == 0x07e0 {
                        ((v >> 11) & 31, (v >> 5) & 63, v & 31)
                    } else {
                        ((v >> 10) & 31, (v >> 5) & 31, v & 31)
                    };
                    let (gmax, gv) = if compression == 3 && masks[1] == 0x07e0 { (63, g5) } else { (31, g5) };
                    px.copy_from_slice(&[(r5 * 255 / 31) as u8, (gv * 255 / gmax) as u8, (b5 * 255 / 31) as u8, 255]);
                }
                24 => px.copy_from_slice(&[row[x * 3 + 2], row[x * 3 + 1], row[x * 3], 255]),
                _ => {
                    let p = &row[x * 4..x * 4 + 4];
                    any_alpha |= p[3] != 0;
                    px.copy_from_slice(&[p[2], p[1], p[0], p[3]]);
                }
            }
        }
    }
    // 3. Transparency: a 32-bit bitmap with an alpha channel uses it; the
    //    others use the mask, where a set bit is transparent.
    if bits == 32 && any_alpha {
        // Alpha is all there is.
    } else {
        for y in 0..h {
            let r = row_of(y);
            let mask_row =
                if has_mask { xor.get(mask_at + r * and_stride..mask_at + (r + 1) * and_stride) } else { None };
            for x in 0..w {
                let px = &mut rgba[(y * w + x) * 4..][..4];
                if bits == 32 {
                    px[3] = 255;
                }
                if let Some(m) = mask_row
                    && (m[x / 8] >> (7 - x % 8)) & 1 == 1
                {
                    px[3] = 0;
                }
            }
        }
    }
    Ok(Entry { width: w as u32, height: h as u32, rgba, bits, png: false, hotspot: None })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A DIB of `w` x `h` with the given pixel rows (top row first, each
    /// pixel BGRA), at `bits`, plus an AND mask (top row first, one `bool`
    /// per pixel, true = transparent).
    pub fn dib(w: usize, h: usize, bits: u16, palette: &[[u8; 3]], rows: &[Vec<u8>], mask: &[Vec<bool>]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend(40u32.to_le_bytes());
        d.extend((w as u32).to_le_bytes());
        d.extend((h as u32 * 2).to_le_bytes());
        d.extend(1u16.to_le_bytes());
        d.extend(bits.to_le_bytes());
        d.extend([0u8; 24]);
        if bits <= 8 {
            // A full colour table, as real files write.
            for i in 0..1usize << bits {
                let c = palette.get(i).copied().unwrap_or([0, 0, 0]);
                d.extend([c[2], c[1], c[0], 0]);
            }
        }
        let stride = (w * bits as usize).div_ceil(32) * 4;
        for row in rows.iter().rev() {
            let mut r = row.clone();
            r.resize(stride, 0);
            d.extend(r);
        }
        let mstride = w.div_ceil(32) * 4;
        for row in mask.iter().rev() {
            let mut r = vec![0u8; mstride];
            for (x, &t) in row.iter().enumerate() {
                if t {
                    r[x / 8] |= 0x80 >> (x % 8);
                }
            }
            d.extend(r);
        }
        d
    }

    /// An icon file around the given entry data: (width, height, data).
    pub fn ico(kind: u16, entries: &[(u8, u8, Vec<u8>)]) -> Vec<u8> {
        let mut f = vec![0, 0];
        f.extend(kind.to_le_bytes());
        f.extend((entries.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * entries.len();
        for (w, h, d) in entries {
            f.extend([*w, *h, 0, 0]);
            f.extend(1u16.to_le_bytes());
            f.extend(32u16.to_le_bytes());
            f.extend((d.len() as u32).to_le_bytes());
            f.extend((offset as u32).to_le_bytes());
            offset += d.len();
        }
        for (_, _, d) in entries {
            f.extend(d);
        }
        f
    }

    fn png_of(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(rgba).unwrap();
        out
    }

    #[test]
    fn a_24_bit_bitmap_with_a_mask() {
        // 2 x 2: red, green / blue, white; the white pixel is masked out.
        let rows = vec![vec![0, 0, 255, 0, 255, 0], vec![255, 0, 0, 255, 255, 255]];
        let mask = vec![vec![false, false], vec![false, true]];
        let f = ico(1, &[(2, 2, dib(2, 2, 24, &[], &rows, &mask))]);
        let icon = parse(&f).unwrap();
        assert!(!icon.cursor);
        let e = &icon.entries[0];
        assert_eq!((e.width, e.height, e.bits, e.png), (2, 2, 24, false));
        assert_eq!(e.rgba, [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 0]);
    }

    #[test]
    fn a_32_bit_bitmap_uses_its_alpha_or_its_mask() {
        // With alpha the mask is ignored.
        let rows = vec![vec![0, 0, 255, 128, 0, 255, 0, 255]];
        let mask = vec![vec![true, true]];
        let f = ico(1, &[(2, 1, dib(2, 1, 32, &[], &rows, &mask))]);
        let e = &parse(&f).unwrap().entries[0];
        assert_eq!(e.rgba, [255, 0, 0, 128, 0, 255, 0, 255]);
        // All alpha zero means no alpha channel: opaque except where masked.
        let rows = vec![vec![0, 0, 255, 0, 0, 255, 0, 0]];
        let mask = vec![vec![false, true]];
        let f = ico(1, &[(2, 1, dib(2, 1, 32, &[], &rows, &mask))]);
        let e = &parse(&f).unwrap().entries[0];
        assert_eq!(e.rgba, [255, 0, 0, 255, 0, 255, 0, 0]);
    }

    #[test]
    fn palette_bitmaps_of_1_4_and_8_bits() {
        let pal = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]];
        // 8-bit: indices 3, 2, 1, 0.
        let f = ico(1, &[(2, 2, dib(2, 2, 8, &pal, &[vec![3, 2], vec![1, 0]], &[vec![false; 2], vec![false; 2]]))]);
        let e = &parse(&f).unwrap().entries[0];
        assert_eq!(&e.rgba[..4], &[255, 255, 255, 255]);
        assert_eq!(&e.rgba[4..8], &[0, 0, 255, 255]);
        assert_eq!(&e.rgba[8..12], &[0, 255, 0, 255]);
        assert_eq!(&e.rgba[12..16], &[255, 0, 0, 255]);
        // 4-bit: high nibble first.
        let f = ico(1, &[(2, 1, dib(2, 1, 4, &pal, &[vec![0x21]], &[vec![false; 2]]))]);
        let e = &parse(&f).unwrap().entries[0];
        assert_eq!(&e.rgba[..8], &[0, 0, 255, 255, 0, 255, 0, 255]);
        // 1-bit: the leftmost pixel is the top bit.
        let f = ico(1, &[(8, 1, dib(8, 1, 1, &pal[..2], &[vec![0b1010_0000]], &[vec![false; 8]]))]);
        let e = &parse(&f).unwrap().entries[0];
        assert_eq!(&e.rgba[..4], &[0, 255, 0, 255]);
        assert_eq!(&e.rgba[4..8], &[255, 0, 0, 255]);
        assert_eq!(&e.rgba[8..12], &[0, 255, 0, 255]);
    }

    #[test]
    fn png_entries_and_sorting() {
        let small = png_of(2, 2, &[10, 20, 30, 255].repeat(4));
        let big = png_of(4, 4, &[1, 2, 3, 77].repeat(16));
        // 256 is stored as 0.
        let f = ico(1, &[(2, 2, small), (4, 4, big)]);
        let icon = parse(&f).unwrap();
        assert_eq!(icon.entries.len(), 2);
        // Largest first.
        assert_eq!((icon.entries[0].width, icon.entries[0].png), (4, true));
        assert_eq!(&icon.entries[0].rgba[..4], &[1, 2, 3, 77]);
        assert_eq!(icon.entries[1].width, 2);
    }

    #[test]
    fn cursors_have_hot_spots() {
        let rows = vec![vec![1, 2, 3]];
        let mut f = ico(2, &[(1, 1, dib(1, 1, 24, &[], &rows, &[vec![false]]))]);
        f[10..12].copy_from_slice(&5u16.to_le_bytes());
        f[12..14].copy_from_slice(&7u16.to_le_bytes());
        let icon = parse(&f).unwrap();
        assert!(icon.cursor);
        assert_eq!(icon.entries[0].hotspot, Some((5, 7)));
    }

    #[test]
    fn damaged_icons_keep_what_can_be_read() {
        let rows = vec![vec![1, 2, 3, 4, 5, 6], vec![7, 8, 9, 10, 11, 12]];
        let good = dib(2, 2, 24, &[], &rows, &[vec![false; 2], vec![false; 2]]);
        let f = ico(1, &[(2, 2, good.clone()), (2, 2, vec![9; 10])]);
        let icon = parse(&f).unwrap();
        assert_eq!(icon.entries.len(), 1);
        assert!(!icon.warnings.is_empty());
        // Cut anywhere: no panic, and once the first picture is whole it is shown.
        let f = ico(1, &[(2, 2, good)]);
        for cut in 0..f.len() {
            let _ = parse(&f[..cut]);
        }
        for at in 0..f.len() {
            let mut bad = f.clone();
            bad[at] ^= 0xff;
            let _ = parse(&bad);
        }
        assert!(parse(b"").is_err());
        assert!(!is_icon(b"\0\0\x01\0\x01\0\x10\x10\0\0\x01\0\x20\0\x04\0\0\0\x01\0\0\0"));
    }

    #[test]
    fn hostile_icons_are_bounded() {
        // A bitmap claiming 100000 x 100000 pixels.
        let mut d = dib(1, 1, 24, &[], &[vec![0, 0, 0]], &[vec![false]]);
        d[4..8].copy_from_slice(&100_000u32.to_le_bytes());
        assert!(parse(&ico(1, &[(1, 1, d)])).is_err());
        // A PNG entry claiming a huge size.
        let big = png_of(1, 1, &[0; 4]);
        let mut bad = big.clone();
        bad[16..20].copy_from_slice(&60_000u32.to_be_bytes());
        bad[20..24].copy_from_slice(&60_000u32.to_be_bytes());
        assert!(parse(&ico(1, &[(1, 1, bad)])).is_err());
        // 1000 entries all pointing at the same 2000 x 2000 picture stop at the pixel budget.
        let pic = png_of(2000, 2000, &vec![0u8; 2000 * 2000 * 4]);
        let mut f = vec![0, 0, 1, 0];
        f.extend(1000u16.to_le_bytes());
        let offset = 6 + 16 * 1000;
        for _ in 0..1000 {
            f.extend([0, 0, 0, 0, 1, 0, 32, 0]);
            f.extend((pic.len() as u32).to_le_bytes());
            f.extend((offset as u32).to_le_bytes());
        }
        f.extend(&pic);
        let icon = parse(&f).unwrap();
        assert!(icon.entries.len() as u64 * 4_000_000 <= MAX_TOTAL_PIXELS);
        assert!(icon.warnings.iter().any(|w| w.contains("more pictures")));
    }
}
