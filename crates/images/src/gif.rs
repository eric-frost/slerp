//! GIF (87a and 89a): every frame with its disposal method, transparency,
//! local or global palette, interlacing and delay, and the Netscape loop
//! count.  The reader indexes the file first (no pixel is decoded), so a
//! frame decodes on request and an animation with thousands of frames opens
//! at once.
//!
//! Where browsers and the specification differ, this follows the browsers:
//! the background colour is ignored (disposing to background clears to
//! transparent), a frame that reaches outside the logical screen makes the
//! screen bigger, delays of 10 ms or less play at 100 ms, and a file without
//! a loop extension plays once.

use crate::animation::{
    Animation, Blend, Dispose, MAX_CANVAS_PIXELS, MAX_FRAMES, Patch, PatchSource, Rect, delay_seconds,
};
use std::ops::Range;
use std::sync::Arc;

/// A frame, located in the file.
#[derive(Clone, Debug)]
pub struct Frame {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub interlaced: bool,
    /// The local colour table's bytes in the file.
    local: Option<Range<usize>>,
    min_code: u8,
    /// The frame's data sub-blocks, terminator included.
    data: Range<usize>,
    pub delay_cs: u16,
    pub dispose: Dispose,
    pub transparent: Option<u8>,
}

impl Frame {
    pub fn has_local_palette(&self) -> bool {
        self.local.is_some()
    }
}

/// An indexed GIF file.
pub struct Gif {
    data: Arc<Vec<u8>>,
    pub version: String,
    /// The canvas: the logical screen, grown to fit every frame.
    pub width: u32,
    pub height: u32,
    /// The logical screen as the file states it.
    pub screen: (u32, u32),
    global: Option<Range<usize>>,
    pub frames: Vec<Frame>,
    /// The Netscape extension's count: 0 for ever, `None` when it is absent.
    pub loop_count: Option<u16>,
    pub comments: Vec<String>,
    pub warnings: Vec<String>,
}

/// Skips data sub-blocks from `pos`; returns where they end and whether the
/// terminator was found.
fn skip_blocks(d: &[u8], mut pos: usize) -> (usize, bool) {
    loop {
        let Some(&n) = d.get(pos) else { return (d.len(), false) };
        pos += 1;
        if n == 0 {
            return (pos, true);
        }
        pos += n as usize;
        if pos > d.len() {
            return (d.len(), false);
        }
    }
}

/// The payload of the data sub-blocks starting at `pos`, joined.
fn join_blocks(d: &[u8], mut pos: usize, end: usize) -> Vec<u8> {
    let mut out = Vec::new();
    while pos < end {
        let n = d[pos] as usize;
        pos += 1;
        if n == 0 {
            break;
        }
        let stop = (pos + n).min(end);
        out.extend_from_slice(&d[pos..stop]);
        pos = stop;
    }
    out
}

impl Gif {
    pub fn is_gif(head: &[u8]) -> bool {
        head.len() >= 6 && &head[..4] == b"GIF8" && matches!(head[4], b'7' | b'9') && head[5] == b'a'
    }

    /// Indexes a GIF file.
    pub fn parse(data: Arc<Vec<u8>>) -> Result<Gif, String> {
        let d = &data[..];
        if !Gif::is_gif(d) {
            return Err("this isn't a GIF file".into());
        }
        if d.len() < 13 {
            return Err("the file ends inside the GIF header".into());
        }
        let version = String::from_utf8_lossy(&d[3..6]).into_owned();
        let screen = (u16::from_le_bytes([d[6], d[7]]) as u32, u16::from_le_bytes([d[8], d[9]]) as u32);
        let packed = d[10];
        let mut pos = 13;
        let mut global = None;
        if packed & 0x80 != 0 {
            let size = 3usize << ((packed & 7) + 1);
            if pos + size > d.len() {
                return Err("the file ends inside the global colour table".into());
            }
            global = Some(pos..pos + size);
            pos += size;
        }
        let mut gif = Gif {
            data: data.clone(),
            version,
            width: screen.0,
            height: screen.1,
            screen,
            global,
            frames: Vec::new(),
            loop_count: None,
            comments: Vec::new(),
            warnings: Vec::new(),
        };
        // The graphic control extension waiting for its image.
        let mut control: Option<(u16, Dispose, Option<u8>)> = None;
        while pos < d.len() {
            match d[pos] {
                0x21 => {
                    let Some(&label) = d.get(pos + 1) else { break };
                    pos += 2;
                    match label {
                        0xf9 => {
                            // Block size 4: flags, delay, transparent index.
                            let size = d.get(pos).copied().unwrap_or(0) as usize;
                            if size >= 4 && pos + 5 <= d.len() {
                                let flags = d[pos + 1];
                                let delay = u16::from_le_bytes([d[pos + 2], d[pos + 3]]);
                                let dispose = match (flags >> 2) & 7 {
                                    2 => Dispose::Background,
                                    3 => Dispose::Previous,
                                    _ => Dispose::None,
                                };
                                control = Some((delay, dispose, (flags & 1 != 0).then_some(d[pos + 4])));
                            }
                            pos = skip_blocks(d, pos).0;
                        }
                        0xff => {
                            let name = d.get(pos + 1..pos + 12);
                            if matches!(name, Some(b"NETSCAPE2.0" | b"ANIMEXTS1.0")) {
                                // The data sub-block: 1, then the count.
                                let at = pos + 12;
                                if d.get(at) == Some(&3) && d.get(at + 1) == Some(&1) && at + 4 <= d.len() {
                                    gif.loop_count = Some(u16::from_le_bytes([d[at + 2], d[at + 3]]));
                                }
                            }
                            pos = skip_blocks(d, pos).0;
                        }
                        0xfe => {
                            let end = skip_blocks(d, pos).0;
                            if gif.comments.len() < 8 {
                                let text = join_blocks(d, pos, end);
                                let text = String::from_utf8_lossy(&text).trim().to_string();
                                if !text.is_empty() {
                                    gif.comments.push(text.chars().take(500).collect());
                                }
                            }
                            pos = end;
                        }
                        _ => pos = skip_blocks(d, pos).0,
                    }
                }
                0x2c => {
                    if pos + 10 > d.len() {
                        gif.warnings.push(format!("The file ends inside frame {}'s header", gif.frames.len() + 1));
                        break;
                    }
                    let u = |at: usize| u16::from_le_bytes([d[at], d[at + 1]]) as u32;
                    let (x, y, w, h) = (u(pos + 1), u(pos + 3), u(pos + 5), u(pos + 7));
                    let packed = d[pos + 9];
                    pos += 10;
                    let mut local = None;
                    if packed & 0x80 != 0 {
                        let size = 3usize << ((packed & 7) + 1);
                        if pos + size > d.len() {
                            gif.warnings
                                .push(format!("The file ends inside frame {}'s colour table", gif.frames.len() + 1));
                            break;
                        }
                        local = Some(pos..pos + size);
                        pos += size;
                    }
                    let Some(&min_code) = d.get(pos) else { break };
                    pos += 1;
                    let start = pos;
                    let (end, complete) = skip_blocks(d, pos);
                    pos = end;
                    let (delay_cs, dispose, transparent) = control.take().unwrap_or((0, Dispose::None, None));
                    if gif.frames.len() >= MAX_FRAMES {
                        gif.warnings.push(format!("Only the first {MAX_FRAMES} frames are shown"));
                        break;
                    }
                    if start < end {
                        gif.frames.push(Frame {
                            x,
                            y,
                            w,
                            h,
                            interlaced: packed & 0x40 != 0,
                            local,
                            min_code,
                            data: start..end,
                            delay_cs,
                            dispose,
                            transparent,
                        });
                    }
                    if !complete {
                        gif.warnings.push(format!("The file ends inside frame {}", gif.frames.len()));
                        break;
                    }
                }
                0x3b => break,
                // A stray zero between blocks.
                0x00 => pos += 1,
                _ => {
                    if gif.frames.is_empty() {
                        return Err("the file has no GIF data where the first image should be".into());
                    }
                    gif.warnings.push("Unrecognised data follows the last frame".into());
                    break;
                }
            }
        }
        if gif.frames.is_empty() {
            return Err("the GIF holds no images".into());
        }
        // A first frame that reaches outside the screen makes it bigger, as
        // browsers do; later frames are clipped to it.
        if let Some(f) = gif.frames.first() {
            gif.width = gif.width.max(f.x + f.w);
            gif.height = gif.height.max(f.y + f.h);
        }
        if gif.width == 0 || gif.height == 0 {
            return Err("the GIF has no size".into());
        }
        let pixels = gif.width as u64 * gif.height as u64;
        if pixels > MAX_CANVAS_PIXELS {
            return Err(format!(
                "the picture is {} × {} pixels ({} megapixels); animations over {} megapixels aren't shown",
                gif.width,
                gif.height,
                pixels >> 20,
                MAX_CANVAS_PIXELS >> 20
            ));
        }
        Ok(gif)
    }

    /// Whether any frame is transparent.
    pub fn has_transparency(&self) -> bool {
        self.frames.iter().any(|f| f.transparent.is_some())
    }

    /// Decodes frame `i` into a patch.
    pub fn patch(&self, i: usize) -> Result<Patch, String> {
        let f = self.frames.get(i).ok_or("no such frame")?;
        let (w, h) = (f.w as usize, f.h as usize);
        let rect = Rect { x: f.x, y: f.y, w: f.w, h: f.h };
        let blend = Blend::Binary;
        if w == 0 || h == 0 {
            return Ok(Patch { rect, rgba: Vec::new(), blend, dispose: f.dispose });
        }
        let n = w * h;
        if n as u64 > MAX_CANVAS_PIXELS {
            return Err("the frame is too large".into());
        }
        let palette = f.local.clone().or(self.global.clone()).map(|r| &self.data[r]);
        // 1. Inflate the pixel indices.
        let payload = join_blocks(&self.data, f.data.start, f.data.end);
        let mut indices = vec![0u8; n];
        let written = lzw_decode(f.min_code, &payload, &mut indices);
        // 2. Colour them, row by row in file order (interlaced rows are
        //    scattered), leaving pixels the data didn't reach transparent.
        let mut rgba = vec![0u8; n * 4];
        for r in 0..h {
            let y = if f.interlaced { interlaced_row(r, h) } else { r };
            for x in 0..w {
                let p = r * w + x;
                if p >= written {
                    break;
                }
                let idx = indices[p];
                if f.transparent == Some(idx) {
                    continue;
                }
                let px = &mut rgba[(y * w + x) * 4..][..4];
                match palette {
                    Some(pal) => {
                        let Some(c) = pal.get(idx as usize * 3..idx as usize * 3 + 3) else { continue };
                        px.copy_from_slice(&[c[0], c[1], c[2], 255]);
                    }
                    // No colour table at all: a grey ramp, which is what
                    // viewers show for it.
                    None => px.copy_from_slice(&[idx, idx, idx, 255]),
                }
            }
        }
        // "Restore the previous picture" has no previous picture on the first
        // frame; viewers leave it as it is.
        let dispose = if i == 0 && f.dispose == Dispose::Previous { Dispose::None } else { f.dispose };
        Ok(Patch { rect, rgba, blend, dispose })
    }

    /// Frame delays in seconds.
    pub fn delays(&self) -> Vec<f32> {
        self.frames.iter().map(|f| delay_seconds(f.delay_cs as u32 * 10)).collect()
    }

    /// How many times the animation plays: `None` for ever.
    pub fn plays(&self) -> Option<u32> {
        match self.loop_count {
            None => Some(1),
            Some(0) => None,
            // Browsers play the animation once and then repeat it N times.
            Some(n) => Some(n as u32 + 1),
        }
    }

    pub fn into_animation(self: Arc<Gif>, info: slerp_scene::Info) -> Animation {
        Animation {
            info,
            size: (self.width, self.height),
            delays: self.delays(),
            plays: self.plays(),
            source: Box::new(GifSource(self)),
        }
    }
}

struct GifSource(Arc<Gif>);

impl PatchSource for GifSource {
    fn patch(&mut self, index: usize) -> Result<Patch, String> {
        self.0.patch(index)
    }
}

/// The picture row that the `r`th stored row of an interlaced frame of
/// height `h` belongs to: rows 0, 8, 16, ..., then 4, 12, ..., then 2, 6,
/// ..., then the odd ones.
fn interlaced_row(r: usize, h: usize) -> usize {
    let pass1 = h.div_ceil(8);
    let pass2 = (h + 3) / 8;
    let pass3 = (h + 1) / 4;
    if r < pass1 {
        r * 8
    } else if r < pass1 + pass2 {
        (r - pass1) * 8 + 4
    } else if r < pass1 + pass2 + pass3 {
        (r - pass1 - pass2) * 4 + 2
    } else {
        (r - pass1 - pass2 - pass3) * 2 + 1
    }
}

/// Writes the string of `code` (and `extra` after it) at `pos`, walking the
/// code's chain backwards from the end of the string; whatever falls beyond
/// `out` is dropped.  Returns false once `out` is full.
fn emit(
    out: &mut [u8],
    pos: &mut usize,
    code: usize,
    extra: Option<u8>,
    length: &[u16],
    prefix: &[u16],
    suffix: &[u8],
) -> bool {
    let n = length[code] as usize + extra.is_some() as usize;
    let end = *pos + n;
    let mut i = end;
    if let Some(e) = extra {
        i -= 1;
        if i < out.len() {
            out[i] = e;
        }
    }
    let mut c = code;
    for _ in 0..length[code] {
        i -= 1;
        if i < out.len() {
            out[i] = suffix[c];
        }
        c = prefix[c] as usize;
    }
    *pos = end;
    *pos < out.len()
}

/// Decodes GIF's variable-width LZW into `out`, returning how many pixels
/// were written.  Damaged data ends the output early rather than failing:
/// browsers show what decoded.
pub fn lzw_decode(min_code: u8, input: &[u8], out: &mut [u8]) -> usize {
    // Some files say 1; the codes still need two bits.  Nine to eleven are
    // out of spec but harmless.
    let min = (min_code as u32).clamp(2, 11);
    let clear = 1usize << min;
    let eoi = clear + 1;
    let mut prefix = vec![0u16; 4096];
    let mut suffix = vec![0u8; 4096];
    let mut first = vec![0u8; 4096];
    let mut length = vec![0u16; 4096];
    for i in 0..clear.min(256) {
        suffix[i] = i as u8;
        first[i] = i as u8;
        length[i] = 1;
    }
    let (mut size, mut next, mut prev): (u32, usize, Option<usize>) = (min + 1, eoi + 1, None);
    let (mut acc, mut bits, mut at, mut pos) = (0u32, 0u32, 0usize, 0usize);

    loop {
        while bits < size {
            let Some(&b) = input.get(at) else { return pos.min(out.len()) };
            at += 1;
            acc |= (b as u32) << bits;
            bits += 8;
        }
        let code = (acc & ((1 << size) - 1)) as usize;
        acc >>= size;
        bits -= size;
        if code == clear {
            size = min + 1;
            next = eoi + 1;
            prev = None;
            continue;
        }
        if code == eoi {
            break;
        }
        let Some(p) = prev else {
            // The first code after a clear is a literal.
            if code >= clear {
                break;
            }
            if !emit(out, &mut pos, code, None, &length, &prefix, &suffix) {
                break;
            }
            prev = Some(code);
            continue;
        };
        let first_byte;
        if code < next && (code < clear || code > eoi) {
            if !emit(out, &mut pos, code, None, &length, &prefix, &suffix) {
                break;
            }
            first_byte = first[code];
        } else if code == next && next < 4096 {
            // The string being defined by this very code: the previous
            // string and its own first byte.
            let f = first[p];
            if !emit(out, &mut pos, p, Some(f), &length, &prefix, &suffix) {
                break;
            }
            first_byte = f;
        } else {
            break;
        }
        if next < 4096 {
            prefix[next] = p as u16;
            suffix[next] = first_byte;
            first[next] = first[p];
            length[next] = length[p] + 1;
            next += 1;
            if next == (1 << size) && size < 12 {
                size += 1;
            }
        }
        prev = Some(code);
    }
    pos.min(out.len())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Encodes `pixels` with GIF's LZW.
    pub fn lzw_encode(min: u8, pixels: &[u8]) -> Vec<u8> {
        use std::collections::HashMap;
        struct Bits {
            out: Vec<u8>,
            acc: u32,
            n: u32,
        }
        impl Bits {
            fn put(&mut self, code: u16, size: u32) {
                self.acc |= (code as u32) << self.n;
                self.n += size;
                while self.n >= 8 {
                    self.out.push(self.acc as u8);
                    self.acc >>= 8;
                    self.n -= 8;
                }
            }
        }
        let clear = 1u16 << min;
        let eoi = clear + 1;
        let mut w = Bits { out: Vec::new(), acc: 0, n: 0 };
        let mut dict: HashMap<(u16, u8), u16> = HashMap::new();
        let (mut next, mut size) = (eoi + 1, min as u32 + 1);
        w.put(clear, size);
        let mut cur: Option<u16> = None;
        for &p in pixels {
            let Some(c) = cur else {
                cur = Some(p as u16);
                continue;
            };
            if let Some(&code) = dict.get(&(c, p)) {
                cur = Some(code);
                continue;
            }
            w.put(c, size);
            if next < 4096 {
                dict.insert((c, p), next);
                if next as u32 == (1 << size) && size < 12 {
                    size += 1;
                }
                next += 1;
            } else {
                w.put(clear, size);
                dict.clear();
                next = eoi + 1;
                size = min as u32 + 1;
            }
            cur = Some(p as u16);
        }
        if let Some(c) = cur {
            w.put(c, size);
        }
        w.put(eoi, size);
        if w.n > 0 {
            w.out.push(w.acc as u8);
        }
        w.out
    }

    /// One frame to write into a test GIF.
    pub struct F {
        pub x: u16,
        pub y: u16,
        pub w: u16,
        pub h: u16,
        pub indices: Vec<u8>,
        pub delay: u16,
        pub dispose: u8,
        pub transparent: Option<u8>,
        pub interlaced: bool,
        pub palette: Option<Vec<[u8; 3]>>,
    }

    impl F {
        pub fn full(w: u16, h: u16, index: u8) -> F {
            F {
                x: 0,
                y: 0,
                w,
                h,
                indices: vec![index; w as usize * h as usize],
                delay: 10,
                dispose: 0,
                transparent: None,
                interlaced: false,
                palette: None,
            }
        }
    }

    fn sub_blocks(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for c in data.chunks(255) {
            out.push(c.len() as u8);
            out.extend(c);
        }
        out.push(0);
        out
    }

    /// A GIF with a 4-colour global palette (black, red, green, blue).
    pub fn build(w: u16, h: u16, frames: &[F], loops: Option<u16>) -> Vec<u8> {
        let mut g = b"GIF89a".to_vec();
        g.extend(w.to_le_bytes());
        g.extend(h.to_le_bytes());
        g.extend([0x80 | 1, 0, 0]);
        g.extend([0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]);
        if let Some(n) = loops {
            g.extend([0x21, 0xff, 11]);
            g.extend(b"NETSCAPE2.0");
            g.extend([3, 1]);
            g.extend(n.to_le_bytes());
            g.push(0);
        }
        for f in frames {
            g.extend([0x21, 0xf9, 4, (f.dispose << 2) | f.transparent.is_some() as u8]);
            g.extend(f.delay.to_le_bytes());
            g.extend([f.transparent.unwrap_or(0), 0]);
            g.push(0x2c);
            for v in [f.x, f.y, f.w, f.h] {
                g.extend(v.to_le_bytes());
            }
            let mut packed = if f.interlaced { 0x40 } else { 0 };
            if let Some(p) = &f.palette {
                packed |= 0x80 | 1;
                g.push(packed);
                for c in p {
                    g.extend(c);
                }
            } else {
                g.push(packed);
            }
            g.push(2);
            g.extend(sub_blocks(&lzw_encode(2, &f.indices)));
        }
        g.push(0x3b);
        g
    }

    fn patch_rgba(g: &Gif, i: usize) -> Vec<u8> {
        g.patch(i).unwrap().rgba
    }

    #[test]
    fn lzw_round_trips_everything() {
        let mut state = 7u32;
        let mut rnd = move || {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (state >> 24) as u8
        };
        for min in [2u8, 3, 4, 8] {
            let mask = ((1u16 << min) - 1) as u8;
            for len in [0usize, 1, 2, 100, 5000, 70000] {
                // Random runs and repeats, so the table fills and resets.
                let mut px = Vec::new();
                while px.len() < len {
                    let v = rnd() & mask;
                    let run = (rnd() % 9) as usize;
                    px.extend(std::iter::repeat_n(v, run + 1));
                }
                px.truncate(len);
                let enc = lzw_encode(min, &px);
                let mut out = vec![0u8; len];
                assert_eq!(lzw_decode(min, &enc, &mut out), len, "min {min} len {len}");
                assert_eq!(out, px, "min {min} len {len}");
            }
        }
    }

    #[test]
    fn lzw_survives_damage() {
        let px: Vec<u8> = (0..3000).map(|i| (i * 7 % 4) as u8).collect();
        let enc = lzw_encode(2, &px);
        let mut out = vec![0u8; 3000];
        // Truncation gives a prefix of the picture.
        let n = lzw_decode(2, &enc[..enc.len() / 2], &mut out);
        assert!(n > 0 && n < 3000);
        assert_eq!(&out[..n], &px[..n]);
        // Random bytes never panic, whatever they claim.
        let mut seed = 99u32;
        for _ in 0..200 {
            let junk: Vec<u8> = (0..300)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    (seed >> 24) as u8
                })
                .collect();
            for min in [0u8, 1, 2, 7, 8, 11, 12, 255] {
                let mut o = vec![0u8; 500];
                assert!(lzw_decode(min, &junk, &mut o) <= 500);
            }
        }
        // An output smaller than the data ends the decode at the end.
        let mut small = vec![0u8; 10];
        assert_eq!(lzw_decode(2, &enc, &mut small), 10);
        assert_eq!(&small[..], &px[..10]);
    }

    #[test]
    fn interlaced_rows() {
        for h in [1usize, 2, 3, 5, 8, 9, 17, 100, 1001] {
            let mut seen = vec![false; h];
            for r in 0..h {
                let y = interlaced_row(r, h);
                assert!(y < h && !seen[y], "h {h} row {r} -> {y}");
                seen[y] = true;
            }
        }
        assert_eq!((0..10).map(|r| interlaced_row(r, 10)).collect::<Vec<_>>(), [0, 8, 4, 2, 6, 1, 3, 5, 7, 9]);
    }

    #[test]
    fn a_frame_decodes_with_its_palette_and_transparency() {
        let mut f = F::full(2, 2, 1);
        f.indices = vec![1, 2, 3, 0];
        f.transparent = Some(0);
        let g = Gif::parse(Arc::new(build(2, 2, &[f], None))).unwrap();
        assert_eq!((g.width, g.height), (2, 2));
        assert_eq!(patch_rgba(&g, 0), [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0, 0, 0, 0]);
        // A local palette replaces the global one.
        let mut f = F::full(2, 1, 1);
        f.palette = Some(vec![[1, 2, 3], [9, 8, 7], [0, 0, 0], [0, 0, 0]]);
        let g = Gif::parse(Arc::new(build(2, 1, &[f], None))).unwrap();
        assert_eq!(patch_rgba(&g, 0), [9, 8, 7, 255, 9, 8, 7, 255]);
    }

    #[test]
    fn interlaced_frames_are_put_in_order() {
        let mut f = F::full(1, 10, 0);
        // Stored order is rows 0, 8, 4, 2, 6, 1, 3, 5, 7, 9; give row r colour r % 4.
        f.indices = [0usize, 8, 4, 2, 6, 1, 3, 5, 7, 9].iter().map(|&r| (r % 4) as u8).collect();
        f.interlaced = true;
        let g = Gif::parse(Arc::new(build(1, 10, &[f], None))).unwrap();
        let rgba = patch_rgba(&g, 0);
        let pal = [[0u8, 0, 0], [255, 0, 0], [0, 255, 0], [0, 0, 255]];
        for y in 0..10 {
            assert_eq!(&rgba[y * 4..y * 4 + 3], &pal[y % 4], "row {y}");
        }
    }

    #[test]
    fn delays_loops_and_disposal() {
        let mut a = F::full(2, 2, 1);
        a.delay = 0;
        let mut b = F::full(2, 2, 2);
        b.delay = 1;
        b.dispose = 2;
        let mut c = F::full(2, 2, 3);
        c.delay = 25;
        c.dispose = 3;
        let frames = [a, b, c];
        let g = Gif::parse(Arc::new(build(2, 2, &frames, Some(0)))).unwrap();
        assert_eq!(g.delays(), [0.1, 0.1, 0.25]);
        assert_eq!(g.plays(), None);
        assert_eq!(g.frames[1].dispose, Dispose::Background);
        assert_eq!(g.frames[2].dispose, Dispose::Previous);
        // Without the extension a GIF plays once; with a count of N, N + 1 times.
        let once = Gif::parse(Arc::new(build(2, 2, &[F::full(2, 2, 1), F::full(2, 2, 2)], None))).unwrap();
        assert_eq!(once.plays(), Some(1));
        let thrice = Gif::parse(Arc::new(build(2, 2, &[F::full(2, 2, 1), F::full(2, 2, 2)], Some(2)))).unwrap();
        assert_eq!(thrice.plays(), Some(3));
    }

    #[test]
    fn a_first_frame_outside_the_screen_grows_it() {
        let mut f = F::full(2, 2, 1);
        f.x = 3;
        f.y = 1;
        let mut later = F::full(2, 2, 2);
        later.x = 20;
        let g = Gif::parse(Arc::new(build(2, 2, &[f, later], None))).unwrap();
        // Only the first frame counts; later ones are clipped.
        assert_eq!((g.width, g.height), (5, 3));
        // A screen of zero takes the frame's size.
        let g = Gif::parse(Arc::new(build(0, 0, &[F::full(4, 3, 1)], None))).unwrap();
        assert_eq!((g.width, g.height), (4, 3));
    }

    #[test]
    fn truncated_files_keep_their_frames() {
        let frames = [F::full(8, 8, 1), F::full(8, 8, 2), F::full(8, 8, 3)];
        let bytes = build(8, 8, &frames, None);
        let mut best = 0;
        for cut in 0..bytes.len() {
            if let Ok(g) = Gif::parse(Arc::new(bytes[..cut].to_vec())) {
                assert!(g.frames.len() >= best, "frames are not lost when more of the file arrives");
                best = g.frames.len();
                for i in 0..g.frames.len() {
                    let _ = g.patch(i);
                }
            }
        }
        assert_eq!(best, 3);
        // The cut file says so.
        let g = Gif::parse(Arc::new(bytes[..bytes.len() - 9].to_vec())).unwrap();
        assert!(!g.warnings.is_empty());
    }

    #[test]
    fn hostile_files_are_bounded() {
        // A logical screen of 65535 x 65535 is refused, not allocated.
        let mut g = build(1, 1, &[F::full(1, 1, 1)], None);
        g[6..10].copy_from_slice(&[0xff, 0xff, 0xff, 0xff]);
        let err = Gif::parse(Arc::new(g)).err().unwrap();
        assert!(err.contains("megapixels"), "{err}");
        // A frame claiming 65535 x 65535 pixels in a 1x1 screen likewise.
        let mut f = F::full(1, 1, 1);
        f.w = 65535;
        f.h = 65535;
        f.indices = vec![0];
        assert!(Gif::parse(Arc::new(build(1, 1, &[f], None))).is_err());
        // Thousands of tiny frames index quickly and stay under the cap.
        let many: Vec<F> = (0..5000).map(|_| F::full(1, 1, 1)).collect();
        let g = Gif::parse(Arc::new(build(1, 1, &many, Some(0)))).unwrap();
        assert_eq!(g.frames.len(), 5000);
        // Garbage and empties are errors, not panics.
        assert!(Gif::parse(Arc::new(b"GIF89a".to_vec())).is_err());
        assert!(Gif::parse(Arc::new(b"not a gif at all".to_vec())).is_err());
        let mut junk = build(2, 2, &[F::full(2, 2, 1)], None);
        for b in junk.iter_mut().skip(13) {
            *b = b.wrapping_mul(31).wrapping_add(7);
        }
        let _ = Gif::parse(Arc::new(junk));
    }
}
