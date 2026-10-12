//! Quake and Half-Life "miptex" textures, as stored in BSP files and WADs:
//! a 16-byte name, a size, and four offsets to 8-bit mip levels.  Half-Life
//! appends its own 256-colour palette after the last level; Quake uses the
//! global one.

use crate::pixels::{self, QUAKE_PALETTE};
use crate::read::{Bytes, Result};
use slerp_scene::{LoadError, Texture};

/// A miptex header.  `offsets` are relative to the header's start; all
/// zero means the pixels live in a WAD the map names instead.
#[derive(Clone, Debug)]
pub struct Header {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub offsets: [u32; 4],
}

impl Header {
    pub fn read(b: Bytes, at: usize) -> Result<Header> {
        let name = b.name(at, 16)?;
        let width = b.u32(at + 16)?;
        let height = b.u32(at + 20)?;
        let mut offsets = [0; 4];
        for (i, o) in offsets.iter_mut().enumerate() {
            *o = b.u32(at + 24 + 4 * i)?;
        }
        Ok(Header { name, width, height, offsets })
    }

    /// True when the texture's pixels are elsewhere (a WAD).
    pub fn is_external(&self) -> bool {
        self.offsets[0] == 0
    }
}

/// The colour form of a miptex: its base texture and, for Quake textures
/// with fullbright colours, an emissive one.
pub struct Decoded {
    pub base: Texture,
    pub emissive: Option<Texture>,
    /// True when the texture has transparent texels (`{` textures).
    pub masked: bool,
}

/// Where the colours of a texture come from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Palette<'a> {
    /// Quake's palette, or a pack's replacement for it, with Quake's
    /// fullbright colours.
    Global(&'a [u8]),
    /// A palette stored after the pixels, as Half-Life does.
    Embedded,
}

/// Decodes the miptex whose header starts at `at` in `b`, choosing the
/// smallest stored mip level that still covers `max` before downscaling.
pub fn decode(b: Bytes, at: usize, palette: Palette<'_>, max: Option<u32>, container: &str) -> Result<Decoded> {
    let h = Header::read(b, at)?;
    let count = pixels::check_size(h.width, h.height)?;
    if h.is_external() {
        return Err(LoadError::invalid(format!("texture {} has no pixels here", h.name)));
    }

    // 1. Pick the level: the smallest one whose longer side is still at
    //    least `max`, falling back to level 0 if that level's data is bad.
    let mut level = 0;
    if let Some(max) = max {
        while level < 3
            && (h.width.max(h.height) >> (level + 1)) >= max
            && (h.width >> (level + 1)) > 0
            && (h.height >> (level + 1)) > 0
        {
            level += 1;
        }
    }
    let level_data = |l: usize| -> Result<(u32, u32, &[u8])> {
        let (w, hh) = (h.width >> l, h.height >> l);
        let start =
            at.checked_add(h.offsets[l] as usize).ok_or_else(|| LoadError::invalid("miptex offset overflows"))?;
        Ok((w, hh, b.slice(start, w as usize * hh as usize)?))
    };
    let (w, hh, indices) = match level_data(level) {
        Ok(v) if v.0 > 0 && v.1 > 0 && h.offsets[level] != 0 => v,
        _ => level_data(0)?,
    };

    // 2. The palette: Quake's, or the one after the last mip level.
    let mut own = [0u8; 768];
    let colours: &[u8] = match palette {
        Palette::Global(p) if p.len() >= 768 => p,
        Palette::Global(_) => QUAKE_PALETTE,
        Palette::Embedded => {
            let pixel_bytes = count + count / 4 + count / 16 + count / 64;
            let after = if h.offsets[3] != 0 {
                h.offsets[3] as usize + (h.width as usize >> 3) * (h.height as usize >> 3)
            } else {
                h.offsets[0] as usize + pixel_bytes
            };
            let pal_at = at.checked_add(after).ok_or_else(|| LoadError::invalid("palette offset overflows"))?;
            let n = (b.u16(pal_at)? as usize).min(256);
            let src = b.slice(pal_at + 2, n * 3)?;
            own[..n * 3].copy_from_slice(src);
            &own
        }
    };

    // 3. Colours, with `{` textures keyed on index 255 as both games do.
    let masked = h.name.starts_with('{');
    let transparent = masked.then_some(255u8);
    let rgba = pixels::expand(indices, w, hh, colours, transparent);
    let format = format!("{} ({}x{} 8-bit)", h.name, h.width, h.height);
    let lname = h.name.to_ascii_lowercase();
    let unlit = lname.starts_with("sky") || lname.starts_with('*');
    let emissive = if matches!(palette, Palette::Global(_)) && !unlit {
        pixels::fullbrights(indices, colours, transparent).map(|fb| pixels::texture(w, hh, fb, max, container, &format))
    } else {
        None
    };
    let mut base = pixels::texture(w, hh, rgba, max, container, &format);
    base.source.width = h.width;
    base.source.height = h.height;
    base.source.mip_count = 4;
    let masked = masked && base.has_alpha;
    Ok(Decoded { base, emissive, masked })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a miptex: a `w`x`h` level 0 filled by `pixel`, its three
    /// smaller levels, and optionally a palette after them.
    pub fn build(name: &str, w: u32, h: u32, pixel: impl Fn(u32, u32) -> u8, palette: Option<&[u8; 768]>) -> Vec<u8> {
        let mut out = vec![0u8; 40];
        out[..name.len()].copy_from_slice(name.as_bytes());
        out[16..20].copy_from_slice(&w.to_le_bytes());
        out[20..24].copy_from_slice(&h.to_le_bytes());
        for l in 0..4 {
            let off = out.len() as u32;
            out[24 + 4 * l..28 + 4 * l].copy_from_slice(&off.to_le_bytes());
            for y in 0..h >> l {
                for x in 0..w >> l {
                    out.push(pixel(x << l, y << l));
                }
            }
        }
        if let Some(p) = palette {
            out.extend_from_slice(&256u16.to_le_bytes());
            out.extend_from_slice(p);
            out.extend_from_slice(&[0, 0]);
        }
        out
    }

    #[test]
    fn decodes_quake_and_half_life() {
        let q = build("wall", 16, 8, |x, _| if x < 8 { 15 } else { 240 }, None);
        let d = decode(Bytes::new(&q), 0, Palette::Global(QUAKE_PALETTE), None, "test").unwrap();
        assert_eq!((d.base.width(), d.base.height()), (16, 8));
        assert!(d.emissive.is_some());
        assert!(!d.masked);

        let mut pal = [0u8; 768];
        pal[3 * 7..3 * 7 + 3].copy_from_slice(&[10, 20, 30]);
        let hl = build("{fence", 16, 16, |x, _| if x < 8 { 7 } else { 255 }, Some(&pal));
        let d = decode(Bytes::new(&hl), 0, Palette::Embedded, None, "test").unwrap();
        assert!(d.masked && d.emissive.is_none());
        let slerp_scene::textures::Pixels::Rgba8(px) = d.base.base() else { panic!() };
        assert_eq!(&px[0..4], &[10, 20, 30, 255]);
        assert_eq!(px[8 * 4 + 3], 0);
    }

    #[test]
    fn picks_a_smaller_level_for_small_limits() {
        let q = build("big", 256, 128, |_, _| 15, None);
        let d = decode(Bytes::new(&q), 0, Palette::Global(QUAKE_PALETTE), Some(64), "test").unwrap();
        assert_eq!((d.base.width(), d.base.height()), (64, 32));
        assert_eq!(d.base.source.width, 256);
    }

    #[test]
    fn rejects_truncated_and_corrupt() {
        let q = build("wall", 16, 16, |_, _| 1, None);
        for cut in [0, 10, 39, 41, 200] {
            assert!(decode(Bytes::new(&q[..cut]), 0, Palette::Global(QUAKE_PALETTE), None, "t").is_err());
        }
        let mut bad = q.clone();
        bad[16..20].copy_from_slice(&0x7fff_ffffu32.to_le_bytes());
        assert!(decode(Bytes::new(&bad), 0, Palette::Global(QUAKE_PALETTE), None, "t").is_err());
        let mut bad = q.clone();
        bad[24..28].copy_from_slice(&0xffff_fff0u32.to_le_bytes());
        assert!(decode(Bytes::new(&bad), 0, Palette::Global(QUAKE_PALETTE), None, "t").is_err());
        let hl = build("hl", 16, 16, |_, _| 1, None);
        assert!(decode(Bytes::new(&hl), 0, Palette::Embedded, None, "t").is_err());
    }
}
