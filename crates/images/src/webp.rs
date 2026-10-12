//! Animated WebP: a RIFF file whose `VP8X` chunk has the animation flag and
//! whose `ANMF` chunks each hold one frame's rectangle, duration, blend and
//! disposal and a plain lossy or lossless WebP bitstream.
//!
//! Frames are decoded by libwebp, which the texture crate already links for
//! still WebP, from a small standalone WebP built around each frame's
//! chunks.  Placing, blending and disposing is the shared compositor, with
//! libwebp's own integer blend so frames match what other WebP decoders
//! show.  The `ANIM` background colour is ignored, as browsers and libwebp's
//! animation decoder do: disposing to background clears to transparent.

use crate::animation::{
    Animation, Blend, Dispose, MAX_CANVAS_PIXELS, MAX_FRAMES, Patch, PatchSource, Rect, delay_seconds,
};
use libwebp_sys as webp;
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Frame {
    pub rect: Rect,
    pub duration_ms: u32,
    pub blend: Blend,
    pub dispose: Dispose,
    /// libwebp's "key frame": a frame that starts from a cleared canvas, so
    /// its pixels are copied as decoded instead of blended with nothing.
    key: bool,
    /// The frame's own chunks (`ALPH`, `VP8 ` or `VP8L`) in the file.
    payload: Range<usize>,
}

pub struct WebpAnim {
    data: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    /// Plays: 0 for ever.
    pub loop_count: u16,
    pub background: [u8; 4],
    pub frames: Vec<Frame>,
    pub warnings: Vec<String>,
}

/// Whether a frame's chunks have transparency: an `ALPH` chunk, or a lossless
/// bitstream whose header says it uses alpha.
fn has_alpha(chunks: &[u8]) -> bool {
    let mut at = 0;
    while at + 8 <= chunks.len() {
        let len = le32(chunks, at + 4).unwrap_or(0) as usize;
        match &chunks[at..at + 4] {
            b"ALPH" => return true,
            // After the chunk header: the 0x2f signature, then 14 bits of
            // width, 14 of height and the alpha flag.
            b"VP8L" => return le32(chunks, at + 9).is_some_and(|v| v >> 28 & 1 == 1),
            _ => {}
        }
        at = at.saturating_add(8).saturating_add(len).saturating_add(len & 1);
    }
    false
}

/// libwebp's rule for frames that start from a cleared canvas: the first,
/// a full-size frame that doesn't blend, and the one after a full-size or
/// key frame that was disposed to background.
fn is_key_frame(f: &Frame, alpha: bool, prev: Option<&Frame>, canvas: (u32, u32)) -> bool {
    let full = |f: &Frame| (f.rect.w, f.rect.h) == canvas;
    let Some(p) = prev else { return true };
    if (!alpha || f.blend == Blend::Replace) && full(f) {
        return true;
    }
    p.dispose == Dispose::Background && (full(p) || p.key)
}

fn le24(d: &[u8], at: usize) -> Option<u32> {
    let b = d.get(at..at + 3)?;
    Some(b[0] as u32 | (b[1] as u32) << 8 | (b[2] as u32) << 16)
}

fn le32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// Whether a file's first bytes are a WebP whose `VP8X` chunk has the
/// animation flag.
pub fn is_animated(head: &[u8]) -> bool {
    head.len() >= 21
        && &head[..4] == b"RIFF"
        && &head[8..12] == b"WEBP"
        && &head[12..16] == b"VP8X"
        && head[20] & 2 != 0
}

impl WebpAnim {
    pub fn parse(data: Arc<Vec<u8>>) -> Result<WebpAnim, String> {
        let d = &data[..];
        if !is_animated(d) {
            return Err("this isn't an animated WebP file".into());
        }
        let width = le24(d, 24).ok_or("the file ends inside the VP8X chunk")? + 1;
        let height = le24(d, 27).ok_or("the file ends inside the VP8X chunk")? + 1;
        let pixels = width as u64 * height as u64;
        if pixels > MAX_CANVAS_PIXELS {
            return Err(format!(
                "the picture is {width} × {height} pixels ({} megapixels); animations over {} megapixels aren't shown",
                pixels >> 20,
                MAX_CANVAS_PIXELS >> 20
            ));
        }
        let mut anim = WebpAnim {
            data: data.clone(),
            width,
            height,
            loop_count: 0,
            background: [0; 4],
            frames: Vec::new(),
            warnings: Vec::new(),
        };
        // RIFF header (12 bytes), then chunks: fourcc, little-endian length,
        // data padded to an even length.
        let mut at = 12;
        let mut dropped = 0;
        while at + 8 <= d.len() {
            let kind = &d[at..at + 4];
            let len = le32(d, at + 4).unwrap_or(0) as usize;
            let body = at + 8;
            let end = body.saturating_add(len).min(d.len());
            if body.saturating_add(len) > d.len() {
                anim.warnings.push("The file ends early".into());
            }
            match kind {
                b"ANIM" if end - body >= 6 => {
                    anim.background = [d[body + 2], d[body + 1], d[body], d[body + 3]];
                    anim.loop_count = u16::from_le_bytes([d[body + 4], d[body + 5]]);
                }
                b"ANMF" if end - body > 16 => {
                    if anim.frames.len() >= MAX_FRAMES {
                        anim.warnings.push(format!("Only the first {MAX_FRAMES} frames are shown"));
                        break;
                    }
                    let rect = Rect {
                        x: le24(d, body).unwrap_or(0) * 2,
                        y: le24(d, body + 3).unwrap_or(0) * 2,
                        w: le24(d, body + 6).unwrap_or(0) + 1,
                        h: le24(d, body + 9).unwrap_or(0) + 1,
                    };
                    let flags = d[body + 15];
                    let inside = rect.x.checked_add(rect.w).is_some_and(|e| e <= width)
                        && rect.y.checked_add(rect.h).is_some_and(|e| e <= height);
                    if inside {
                        let mut frame = Frame {
                            rect,
                            duration_ms: le24(d, body + 12).unwrap_or(0),
                            blend: if flags & 2 == 0 { Blend::WebpOver } else { Blend::Replace },
                            dispose: if flags & 1 != 0 { Dispose::Background } else { Dispose::None },
                            key: false,
                            payload: body + 16..end,
                        };
                        frame.key =
                            is_key_frame(&frame, has_alpha(&d[body + 16..end]), anim.frames.last(), (width, height));
                        anim.frames.push(frame);
                    } else {
                        dropped += 1;
                    }
                }
                _ => {}
            }
            at = end.saturating_add(len & 1);
        }
        if dropped > 0 {
            anim.warnings.push(format!("{dropped} frames lie outside the picture and are skipped"));
        }
        Ok(anim)
    }

    pub fn delays(&self) -> Vec<f32> {
        self.frames.iter().map(|f| delay_seconds(f.duration_ms)).collect()
    }

    pub fn plays(&self) -> Option<u32> {
        (self.loop_count != 0).then_some(self.loop_count as u32)
    }

    /// Whether any frame can be transparent.
    pub fn has_alpha(&self) -> bool {
        self.data.get(20).is_some_and(|f| f & 0x10 != 0)
    }

    /// Decodes frame `i` into a patch.
    pub fn patch(&self, i: usize) -> Result<Patch, String> {
        let f = self.frames.get(i).ok_or("no such frame")?;
        let payload = &self.data[f.payload.clone()];
        // 1. A standalone WebP around the frame's chunks: a VP8X chunk (so
        //    that an ALPH chunk is honoured) and the chunks themselves.
        let mut file = Vec::with_capacity(payload.len() + 30);
        file.extend(b"RIFF");
        file.extend(((payload.len() + 22) as u32).to_le_bytes());
        file.extend(b"WEBPVP8X");
        file.extend(10u32.to_le_bytes());
        file.extend([0x10, 0, 0, 0]);
        file.extend(&(f.rect.w - 1).to_le_bytes()[..3]);
        file.extend(&(f.rect.h - 1).to_le_bytes()[..3]);
        file.extend(payload);
        // 2. What it holds must be what the frame says.
        let (mut w, mut h) = (0, 0);
        // SAFETY: libwebp reads at most `file.len()` bytes and fills the two ints.
        if unsafe { webp::WebPGetInfo(file.as_ptr(), file.len(), &mut w, &mut h) } == 0 {
            return Err("the frame isn't a WebP image".into());
        }
        if (w as u32, h as u32) != (f.rect.w, f.rect.h) {
            return Err(format!("the frame is {w} × {h} pixels, not the {} × {} its header says", f.rect.w, f.rect.h));
        }
        let mut rgba = vec![0u8; f.rect.w as usize * f.rect.h as usize * 4];
        // SAFETY: the output buffer is ours, `stride * h` bytes long, and
        // libwebp checks the size it is given.
        let out = unsafe {
            webp::WebPDecodeRGBAInto(file.as_ptr(), file.len(), rgba.as_mut_ptr(), rgba.len(), f.rect.w as i32 * 4)
        };
        if out.is_null() {
            return Err("libwebp couldn't decode the frame".into());
        }
        let blend = if f.key { Blend::Replace } else { f.blend };
        Ok(Patch { rect: f.rect, rgba, blend, dispose: f.dispose })
    }

    pub fn into_animation(self: Arc<WebpAnim>, info: slerp_scene::Info) -> Animation {
        Animation {
            info,
            size: (self.width, self.height),
            delays: self.delays(),
            plays: self.plays(),
            source: Box::new(WebpSource(self)),
        }
    }
}

struct WebpSource(Arc<WebpAnim>);

impl PatchSource for WebpSource {
    fn patch(&mut self, index: usize) -> Result<Patch, String> {
        self.0.patch(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lossless WebP (VP8L) of 1x1 pixels is 26 bytes: tiny hand-made
    /// bitstreams are not practical, so the tests that decode pixels use
    /// the sample files; these cover the container.
    fn riff(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut body = b"WEBP".to_vec();
        for (k, d) in chunks {
            body.extend(*k);
            body.extend((d.len() as u32).to_le_bytes());
            body.extend(d);
            if d.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut f = b"RIFF".to_vec();
        f.extend((body.len() as u32).to_le_bytes());
        f.extend(body);
        f
    }

    fn vp8x(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0x12, 0, 0, 0];
        v.extend(&(w - 1).to_le_bytes()[..3]);
        v.extend(&(h - 1).to_le_bytes()[..3]);
        v
    }

    fn anmf(x: u32, y: u32, w: u32, h: u32, ms: u32, flags: u8, data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        for n in [x / 2, y / 2, w - 1, h - 1, ms] {
            v.extend(&n.to_le_bytes()[..3]);
        }
        v.push(flags);
        v.extend(data);
        v
    }

    #[test]
    fn the_container_is_indexed() {
        let mut anim = vec![0, 0, 0, 0xff]; // background
        anim.extend(3u16.to_le_bytes());
        let junk = b"VP8 \x04\0\0\0abcd".to_vec();
        let file = riff(&[
            (b"VP8X", vp8x(8, 6)),
            (b"ANIM", anim),
            (b"ANMF", anmf(2, 2, 4, 4, 80, 0b11, &junk)),
            (b"ANMF", anmf(0, 0, 8, 6, 5, 0, &junk)),
            // A frame outside the canvas is dropped.
            (b"ANMF", anmf(6, 0, 4, 4, 5, 0, &junk)),
        ]);
        assert!(is_animated(&file));
        let a = WebpAnim::parse(Arc::new(file)).unwrap();
        assert_eq!((a.width, a.height), (8, 6));
        assert_eq!(a.frames.len(), 2);
        assert_eq!(a.frames[0].rect, Rect { x: 2, y: 2, w: 4, h: 4 });
        assert_eq!((a.frames[0].blend, a.frames[0].dispose), (Blend::Replace, Dispose::Background));
        assert_eq!((a.frames[1].blend, a.frames[1].dispose), (Blend::WebpOver, Dispose::None));
        assert_eq!(a.delays(), [0.08, 0.1]);
        assert_eq!(a.plays(), Some(3));
        assert!(a.has_alpha());
        assert!(a.warnings.iter().any(|w| w.contains("outside")));
        // Junk payloads are errors, not panics.
        assert!(a.patch(0).is_err());
        assert!(a.patch(5).is_err());
    }

    #[test]
    fn still_and_broken_files_are_not_animations() {
        let still = riff(&[(b"VP8X", vec![0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0])]);
        assert!(!is_animated(&still));
        assert!(!is_animated(b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(WebpAnim::parse(Arc::new(still)).is_err());
        // A canvas of 16 million x 16 million is refused.
        let mut v = vp8x(2, 2);
        v[4..10].copy_from_slice(&[0xff; 6]);
        assert!(WebpAnim::parse(Arc::new(riff(&[(b"VP8X", v)]))).is_err());
        // Cuts and flips never panic.
        let junk = b"VP8 \x04\0\0\0abcd".to_vec();
        let file = riff(&[(b"VP8X", vp8x(8, 6)), (b"ANMF", anmf(0, 0, 8, 6, 5, 0, &junk))]);
        for cut in 0..file.len() {
            if let Ok(a) = WebpAnim::parse(Arc::new(file[..cut].to_vec())) {
                for i in 0..a.frames.len() {
                    let _ = a.patch(i);
                }
            }
        }
        for at in 12..file.len() {
            let mut bad = file.clone();
            bad[at] ^= 0xa5;
            if let Ok(a) = WebpAnim::parse(Arc::new(bad)) {
                for i in 0..a.frames.len() {
                    let _ = a.patch(i);
                }
            }
        }
    }
}
