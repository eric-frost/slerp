//! Animated PNG: a PNG whose `acTL` chunk announces frames, each described
//! by an `fcTL` chunk and stored in `IDAT` (the first frame, when its `fcTL`
//! comes before the image data) or `fdAT` chunks.
//!
//! The file is indexed once, without decoding anything.  A frame is decoded
//! by building a small plain PNG from its chunks and handing that to the
//! `png` crate, which therefore does the inflating, unfiltering,
//! de-interlacing and palette work for every colour type and bit depth; this
//! code only does the animation: placing frames, blending and disposing.
//!
//! An image whose `IDAT` has no `fcTL` before it is a default image shown by
//! viewers that don't know animation; it is not part of the animation and is
//! skipped here, as the specification says.

use crate::animation::{
    Animation, Blend, Dispose, MAX_CANVAS_PIXELS, MAX_FRAMES, Patch, PatchSource, Rect, delay_seconds,
};
use std::io::Cursor;
use std::ops::Range;
use std::sync::Arc;

const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

#[derive(Clone, Debug)]
pub struct Frame {
    pub rect: Rect,
    pub delay_num: u16,
    pub delay_den: u16,
    pub dispose: Dispose,
    pub blend: Blend,
    /// The compressed data, in pieces: `IDAT` payloads or `fdAT` payloads
    /// without their sequence number.
    data: Vec<Range<usize>>,
}

impl Frame {
    /// The delay in milliseconds.
    pub fn delay_ms(&self) -> u32 {
        let den = if self.delay_den == 0 { 100 } else { self.delay_den as u32 };
        self.delay_num as u32 * 1000 / den
    }
}

pub struct Apng {
    data: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub color_type: u8,
    interlace: u8,
    plte: Option<Range<usize>>,
    trns: Option<Range<usize>>,
    /// The number of frames the `acTL` chunk announces.
    pub announced: u32,
    /// How many times it plays; 0 for ever.
    pub num_plays: u32,
    pub frames: Vec<Frame>,
    /// Whether the `IDAT` image is the first frame; otherwise it is a
    /// default image outside the animation.
    pub default_is_frame: bool,
    pub warnings: Vec<String>,
}

fn be32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn be16(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

/// Whether the start of a PNG file (a few kilobytes are enough unless a big
/// profile comes first) has an `acTL` chunk announcing more than one frame.
/// `None` when the head ends before the image data starts and so can't say.
pub fn head_is_animated(head: &[u8]) -> Option<bool> {
    if !head.starts_with(SIGNATURE) {
        return Some(false);
    }
    let mut at = 8;
    loop {
        let len = be32(head, at)? as usize;
        let kind = head.get(at + 4..at + 8)?;
        match kind {
            b"acTL" => return Some(be32(head, at + 8)? > 1),
            b"IDAT" | b"IEND" => return Some(false),
            _ => {}
        }
        at = at.checked_add(12)?.checked_add(len)?;
    }
}

impl Apng {
    /// Indexes an animated PNG.
    pub fn parse(data: Arc<Vec<u8>>) -> Result<Apng, String> {
        let d = &data[..];
        if !d.starts_with(SIGNATURE) {
            return Err("this isn't a PNG file".into());
        }
        let mut apng = Apng {
            data: data.clone(),
            width: 0,
            height: 0,
            bit_depth: 0,
            color_type: 0,
            interlace: 0,
            plte: None,
            trns: None,
            announced: 0,
            num_plays: 0,
            frames: Vec::new(),
            default_is_frame: false,
            warnings: Vec::new(),
        };
        let mut at = 8usize;
        let mut seen_idat = false;
        // The frame the next image data belongs to.
        let mut current: Option<usize> = None;
        let mut truncated = false;
        while at + 8 <= d.len() {
            let len = be32(d, at).unwrap_or(0) as usize;
            let kind: [u8; 4] = d[at + 4..at + 8].try_into().unwrap_or([0; 4]);
            let body = at + 8;
            let end = body.saturating_add(len);
            if end > d.len() {
                truncated = true;
            }
            let end = end.min(d.len());
            match &kind {
                b"IHDR" if len >= 13 && end - body >= 13 => {
                    apng.width = be32(d, body).unwrap_or(0);
                    apng.height = be32(d, body + 4).unwrap_or(0);
                    apng.bit_depth = d[body + 8];
                    apng.color_type = d[body + 9];
                    apng.interlace = d[body + 12];
                }
                b"PLTE" if !seen_idat => apng.plte = Some(body..end),
                b"tRNS" if !seen_idat => apng.trns = Some(body..end),
                b"acTL" if !seen_idat && end - body >= 8 => {
                    apng.announced = be32(d, body).unwrap_or(0);
                    apng.num_plays = be32(d, body + 4).unwrap_or(0);
                }
                b"fcTL" if end - body >= 26 => {
                    if apng.frames.len() >= MAX_FRAMES {
                        apng.warnings.push(format!("Only the first {MAX_FRAMES} frames are shown"));
                        break;
                    }
                    let (w, h) = (be32(d, body + 4).unwrap_or(0), be32(d, body + 8).unwrap_or(0));
                    let (x, y) = (be32(d, body + 12).unwrap_or(0), be32(d, body + 16).unwrap_or(0));
                    let dispose = match d[body + 24] {
                        1 => Dispose::Background,
                        2 => Dispose::Previous,
                        _ => Dispose::None,
                    };
                    let blend = if d[body + 25] == 1 { Blend::Over } else { Blend::Replace };
                    if !seen_idat {
                        apng.default_is_frame = true;
                    }
                    apng.frames.push(Frame {
                        rect: Rect { x, y, w, h },
                        delay_num: be16(d, body + 20).unwrap_or(0),
                        delay_den: be16(d, body + 22).unwrap_or(0),
                        dispose,
                        blend,
                        data: Vec::new(),
                    });
                    current = Some(apng.frames.len() - 1);
                }
                b"IDAT" => {
                    // Image data belongs to a frame only when its fcTL came first.
                    if !seen_idat {
                        seen_idat = true;
                        if !apng.default_is_frame {
                            current = None;
                        }
                    }
                    if apng.default_is_frame
                        && let Some(f) = current.and_then(|c| apng.frames.get_mut(c))
                    {
                        f.data.push(body..end);
                    }
                }
                b"fdAT" if end - body >= 4 => {
                    if let Some(f) = current.and_then(|c| apng.frames.get_mut(c)) {
                        f.data.push(body + 4..end);
                    }
                }
                b"IEND" => break,
                _ => {}
            }
            at = match end.checked_add(4) {
                Some(next) => next,
                None => break,
            };
        }
        if truncated {
            apng.warnings.push("The file ends early".into());
        }
        if apng.width == 0 || apng.height == 0 {
            return Err("the PNG header is missing or empty".into());
        }
        let pixels = apng.width as u64 * apng.height as u64;
        if pixels > MAX_CANVAS_PIXELS {
            return Err(format!(
                "the picture is {} × {} pixels ({} megapixels); animations over {} megapixels aren't shown",
                apng.width,
                apng.height,
                pixels >> 20,
                MAX_CANVAS_PIXELS >> 20
            ));
        }
        // Frames without data (an fcTL with nothing after it, as in a
        // truncated file) and frames outside the canvas can't be shown.
        let (w, h) = (apng.width, apng.height);
        let before = apng.frames.len();
        apng.frames.retain(|f| {
            !f.data.is_empty()
                && f.rect.w > 0
                && f.rect.h > 0
                && f.rect.x.checked_add(f.rect.w).is_some_and(|e| e <= w)
                && f.rect.y.checked_add(f.rect.h).is_some_and(|e| e <= h)
        });
        if apng.frames.len() < before {
            apng.warnings.push(format!(
                "{} frames were skipped because they are empty or outside the picture",
                before - apng.frames.len()
            ));
        }
        Ok(apng)
    }

    /// The text for a colour type and bit depth.
    pub fn pixel_format(&self) -> String {
        let kind = match self.color_type {
            0 => "grey",
            2 => "RGB",
            3 => "palette",
            4 => "grey with alpha",
            6 => "RGBA",
            _ => "unknown colour type",
        };
        format!("{kind}, {}-bit", self.bit_depth)
    }

    pub fn has_transparency(&self) -> bool {
        matches!(self.color_type, 4 | 6) || self.trns.is_some() || self.frames.iter().any(|f| f.blend == Blend::Over)
    }

    pub fn delays(&self) -> Vec<f32> {
        self.frames.iter().map(|f| delay_seconds(f.delay_ms())).collect()
    }

    pub fn plays(&self) -> Option<u32> {
        (self.num_plays != 0).then_some(self.num_plays)
    }

    /// Decodes frame `i` into a patch.
    pub fn patch(&self, i: usize) -> Result<Patch, String> {
        let f = self.frames.get(i).ok_or("no such frame")?;
        let d = &self.data[..];
        // 1. A plain PNG of just this frame.
        let mut png = Vec::with_capacity(f.data.iter().map(|r| r.len()).sum::<usize>() + 128);
        png.extend_from_slice(SIGNATURE);
        let chunk = |png: &mut Vec<u8>, kind: &[u8; 4], body: &[&[u8]]| {
            let len: usize = body.iter().map(|b| b.len()).sum();
            png.extend((len as u32).to_be_bytes());
            png.extend(kind);
            for b in body {
                png.extend_from_slice(b);
            }
            // The checksum is not checked when decoding below.
            png.extend([0, 0, 0, 0]);
        };
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend(f.rect.w.to_be_bytes());
        ihdr.extend(f.rect.h.to_be_bytes());
        ihdr.extend([self.bit_depth, self.color_type, 0, 0, self.interlace]);
        chunk(&mut png, b"IHDR", &[&ihdr]);
        if let Some(r) = &self.plte {
            chunk(&mut png, b"PLTE", &[&d[r.clone()]]);
        }
        if let Some(r) = &self.trns {
            chunk(&mut png, b"tRNS", &[&d[r.clone()]]);
        }
        let pieces: Vec<&[u8]> = f.data.iter().map(|r| &d[r.clone()]).collect();
        chunk(&mut png, b"IDAT", &pieces);
        chunk(&mut png, b"IEND", &[]);
        // 2. Decode it to 8-bit RGBA.
        let limits = png::Limits { bytes: f.rect.w as usize * f.rect.h as usize * 8 + (1 << 16) };
        let mut dec = png::Decoder::new_with_limits(Cursor::new(&png[..]), limits);
        dec.ignore_checksums(true);
        dec.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = dec.read_info().map_err(|e| e.to_string())?;
        let size = reader.output_buffer_size().ok_or("the frame is too large")?;
        let mut buf = vec![0u8; size];
        let out = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
        buf.truncate(out.buffer_size());
        let n = f.rect.w as usize * f.rect.h as usize;
        let channels = reader.output_color_type().0.samples();
        let rgba = expand(&buf, channels, n)?;
        // The specification: restoring the previous picture on the first frame means clearing.
        let dispose = if i == 0 && f.dispose == Dispose::Previous { Dispose::Background } else { f.dispose };
        Ok(Patch { rect: f.rect, rgba, blend: f.blend, dispose })
    }

    pub fn into_animation(self: Arc<Apng>, info: slerp_scene::Info) -> Animation {
        Animation {
            info,
            size: (self.width, self.height),
            delays: self.delays(),
            plays: self.plays(),
            source: Box::new(ApngSource(self)),
        }
    }
}

struct ApngSource(Arc<Apng>);

impl PatchSource for ApngSource {
    fn patch(&mut self, index: usize) -> Result<Patch, String> {
        self.0.patch(index)
    }
}

/// Grey, grey and alpha, RGB or RGBA bytes as RGBA.
pub(crate) fn expand(src: &[u8], channels: usize, pixels: usize) -> Result<Vec<u8>, String> {
    if src.len() < pixels * channels {
        return Err("the frame's data is short".into());
    }
    let src = &src[..pixels * channels];
    Ok(match channels {
        1 => src.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        2 => src.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        3 => src.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        _ => src.to_vec(),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut c = (body.len() as u32).to_be_bytes().to_vec();
        c.extend(kind);
        c.extend(body);
        c.extend([0, 0, 0, 0]);
        c
    }

    /// The compressed image data of an 8-bit RGBA picture of `w` x `h`,
    /// every pixel `px`, as one zlib stream with filter type 0 on each row.
    fn zdata(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
        let row: Vec<u8> = std::iter::once(0).chain((0..w).flat_map(|_| px)).collect();
        let raw = row.repeat(h as usize);
        miniz_oxide::deflate::compress_to_vec_zlib(&raw, 1)
    }

    pub struct F {
        pub w: u32,
        pub h: u32,
        pub x: u32,
        pub y: u32,
        pub px: [u8; 4],
        pub delay: (u16, u16),
        pub dispose: u8,
        pub blend: u8,
    }

    pub fn frame(w: u32, h: u32, px: [u8; 4]) -> F {
        F { w, h, x: 0, y: 0, px, delay: (1, 10), dispose: 0, blend: 0 }
    }

    fn fctl(seq: u32, f: &F) -> Vec<u8> {
        let mut b = seq.to_be_bytes().to_vec();
        for v in [f.w, f.h, f.x, f.y] {
            b.extend(v.to_be_bytes());
        }
        b.extend(f.delay.0.to_be_bytes());
        b.extend(f.delay.1.to_be_bytes());
        b.extend([f.dispose, f.blend]);
        chunk(b"fcTL", &b)
    }

    /// An 8-bit RGBA animated PNG.  When `default_apart` is set the IDAT
    /// image is a default image outside the animation (red), and the
    /// animation's frames all come in fdAT chunks.
    pub fn build(w: u32, h: u32, frames: &[F], plays: u32, default_apart: bool) -> Vec<u8> {
        let mut png = SIGNATURE.to_vec();
        let mut ihdr = w.to_be_bytes().to_vec();
        ihdr.extend(h.to_be_bytes());
        ihdr.extend([8, 6, 0, 0, 0]);
        png.extend(chunk(b"IHDR", &ihdr));
        let mut actl = (frames.len() as u32).to_be_bytes().to_vec();
        actl.extend(plays.to_be_bytes());
        png.extend(chunk(b"acTL", &actl));
        let mut seq = 0u32;
        for (i, f) in frames.iter().enumerate() {
            if i == 0 && default_apart {
                // The default image: not part of the animation.
                png.extend(chunk(b"IDAT", &zdata(w, h, [255, 0, 0, 255])));
            }
            png.extend(fctl(seq, f));
            seq += 1;
            let z = zdata(f.w, f.h, f.px);
            if i == 0 && !default_apart {
                png.extend(chunk(b"IDAT", &z));
            } else {
                let mut b = seq.to_be_bytes().to_vec();
                seq += 1;
                b.extend(z);
                png.extend(chunk(b"fdAT", &b));
            }
        }
        png.extend(chunk(b"IEND", &[]));
        png
    }

    fn red(a: &Apng, i: usize) -> Vec<u8> {
        a.patch(i).unwrap().rgba
    }

    #[test]
    fn frames_with_their_delays_and_plays() {
        let mut a = frame(2, 2, [10, 20, 30, 255]);
        a.delay = (0, 0);
        let mut b = frame(2, 2, [40, 50, 60, 255]);
        b.delay = (1, 4);
        let png = build(2, 2, &[a, b], 3, false);
        assert_eq!(head_is_animated(&png), Some(true));
        let apng = Apng::parse(Arc::new(png)).unwrap();
        assert_eq!(apng.frames.len(), 2);
        assert!(apng.default_is_frame);
        // Zero is replaced by 100 ms as browsers do; 1/4 s is 250 ms.
        assert_eq!(apng.delays(), [0.1, 0.25]);
        assert_eq!(apng.plays(), Some(3));
        assert_eq!(red(&apng, 0)[..4], [10, 20, 30, 255]);
        assert_eq!(red(&apng, 1)[..4], [40, 50, 60, 255]);
        let forever =
            Apng::parse(Arc::new(build(2, 2, &[frame(2, 2, [0; 4]), frame(2, 2, [0; 4])], 0, false))).unwrap();
        assert_eq!(forever.plays(), None);
    }

    #[test]
    fn a_default_image_outside_the_animation_is_skipped() {
        let png = build(2, 2, &[frame(2, 2, [0, 255, 0, 255]), frame(2, 2, [0, 0, 255, 255])], 0, true);
        let apng = Apng::parse(Arc::new(png)).unwrap();
        assert!(!apng.default_is_frame);
        assert_eq!(apng.frames.len(), 2);
        // The first frame of the animation is green, not the red default.
        assert_eq!(red(&apng, 0)[..4], [0, 255, 0, 255]);
        assert_eq!(red(&apng, 1)[..4], [0, 0, 255, 255]);
    }

    #[test]
    fn partial_frames_carry_their_place_and_ops() {
        let first = frame(4, 4, [0, 0, 255, 255]);
        let mut dot = frame(2, 2, [255, 0, 0, 128]);
        dot.x = 1;
        dot.y = 2;
        dot.blend = 1;
        dot.dispose = 1;
        let apng = Apng::parse(Arc::new(build(4, 4, &[first, dot], 0, false))).unwrap();
        let p = apng.patch(1).unwrap();
        assert_eq!(p.rect, Rect { x: 1, y: 2, w: 2, h: 2 });
        assert_eq!((p.blend, p.dispose), (Blend::Over, Dispose::Background));
    }

    #[test]
    fn palette_and_16_bit_frames_decode() {
        // A 1x1 16-bit RGB picture.
        let mut png = SIGNATURE.to_vec();
        let mut ihdr = 1u32.to_be_bytes().to_vec();
        ihdr.extend(1u32.to_be_bytes());
        ihdr.extend([16, 2, 0, 0, 0]);
        png.extend(chunk(b"IHDR", &ihdr));
        png.extend(chunk(b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]));
        let raw = [0u8, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc];
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 1);
        let f = frame(1, 1, [0; 4]);
        png.extend(fctl(0, &f));
        png.extend(chunk(b"IDAT", &z));
        png.extend(fctl(1, &f));
        let mut fd = 2u32.to_be_bytes().to_vec();
        fd.extend(&z);
        png.extend(chunk(b"fdAT", &fd));
        png.extend(chunk(b"IEND", &[]));
        let apng = Apng::parse(Arc::new(png)).unwrap();
        assert_eq!(apng.frames.len(), 2);
        assert_eq!(red(&apng, 1), [0x12, 0x56, 0x9a, 255]);
        // A palette picture with a transparent first entry.
        let mut png = SIGNATURE.to_vec();
        let mut ihdr = 2u32.to_be_bytes().to_vec();
        ihdr.extend(1u32.to_be_bytes());
        ihdr.extend([8, 3, 0, 0, 0]);
        png.extend(chunk(b"IHDR", &ihdr));
        png.extend(chunk(b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]));
        png.extend(chunk(b"PLTE", &[1, 2, 3, 4, 5, 6]));
        png.extend(chunk(b"tRNS", &[0]));
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&[0u8, 0, 1], 1);
        let f = frame(2, 1, [0; 4]);
        png.extend(fctl(0, &f));
        png.extend(chunk(b"IDAT", &z));
        png.extend(fctl(1, &f));
        let mut fd = 2u32.to_be_bytes().to_vec();
        fd.extend(&z);
        png.extend(chunk(b"fdAT", &fd));
        png.extend(chunk(b"IEND", &[]));
        let apng = Apng::parse(Arc::new(png)).unwrap();
        assert!(apng.has_transparency());
        assert_eq!(red(&apng, 0), [1, 2, 3, 0, 4, 5, 6, 255]);
    }

    #[test]
    fn a_head_that_cannot_tell_says_so() {
        let png = build(2, 2, &[frame(2, 2, [0; 4]), frame(2, 2, [0; 4])], 0, false);
        // Cut inside the IHDR chunk or before acTL: cannot tell.
        assert_eq!(head_is_animated(&png[..20]), None);
        assert_eq!(head_is_animated(&png[..8]), None);
        // A plain PNG is not animated.
        let mut plain = SIGNATURE.to_vec();
        plain.extend(chunk(b"IHDR", &[0; 13]));
        plain.extend(chunk(b"IDAT", &[0; 4]));
        assert_eq!(head_is_animated(&plain), Some(false));
        // One frame is a still.
        let one = build(2, 2, &[frame(2, 2, [0; 4])], 0, false);
        assert_eq!(head_is_animated(&one), Some(false));
        assert_eq!(head_is_animated(b"GIF89a"), Some(false));
    }

    #[test]
    fn damaged_and_hostile_files() {
        let png = build(3, 3, &[frame(3, 3, [1, 2, 3, 255]), frame(3, 3, [4, 5, 6, 255])], 0, false);
        for cut in 0..png.len() {
            if let Ok(a) = Apng::parse(Arc::new(png[..cut].to_vec())) {
                for i in 0..a.frames.len() {
                    let _ = a.patch(i);
                }
            }
        }
        // Flipped bytes everywhere.
        for at in 8..png.len() {
            let mut bad = png.clone();
            bad[at] ^= 0xff;
            if let Ok(a) = Apng::parse(Arc::new(bad)) {
                for i in 0..a.frames.len() {
                    let _ = a.patch(i);
                }
            }
        }
        // A canvas of 100000 x 100000 pixels is refused before anything is allocated.
        let big = build(100_000, 100_000, &[frame(1, 1, [0; 4]), frame(1, 1, [0; 4])], 0, false);
        assert!(Apng::parse(Arc::new(big)).is_err());
        // A frame larger than the canvas is dropped, not decoded.
        let wide = build(2, 2, &[frame(2, 2, [0; 4]), frame(9, 9, [0; 4])], 0, false);
        let a = Apng::parse(Arc::new(wide)).unwrap();
        assert_eq!(a.frames.len(), 1);
        assert!(!a.warnings.is_empty());
        // An announced frame count in the billions, with two real frames.
        let mut many = build(2, 2, &[frame(2, 2, [0; 4]), frame(2, 2, [0; 4])], 0, false);
        let at = many.windows(4).position(|w| w == b"acTL").unwrap() + 4;
        many[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(Apng::parse(Arc::new(many)).unwrap().frames.len(), 2);
    }
}
