//! Quake and Half-Life sprites (`.spr`) and Quake 2 sprites (`.sp2`),
//! shown as galleries of their frames.
//!
//! `IDSP` version 1 is Quake's: frames are 8-bit pictures in Quake's
//! palette, index 255 transparent.  Version 2 is Half-Life's, which stores
//! its own 256-colour palette after the header and a texture format that
//! says how colours blend: normal and alpha-tested (255 transparent),
//! additive (black is see-through) and index-alpha (the index is the
//! opacity of the palette's last colour).  Frames are single pictures or
//! groups of timed pictures; groups are listed frame by frame.
//!
//! Quake 2's `IDS2` sprites hold no pixels: each frame names a `.pcx` or
//! `.wal` file.  The gallery shows the PCX (or PNG, TGA, ...) frames it finds in the sprite's
//! folder or above it in the game's folder tree (`.wal` files need Quake
//! 2's palette, which Slerp does not carry, so they are listed in the
//! error).

use crate::pixels::{self, QUAKE_PALETTE};
use crate::read::{Bytes, Result, cap, cstr};
use crate::{base_name, decode_image, find_in, find_relative, read_neighbour};
use slerp_scene::{Limits, LoadError, NamedImage};
use std::path::Path;
use std::sync::Arc;

/// Frames beyond this are ignored; Quake's sprites have a few dozen.
const MAX_FRAMES: usize = 1024;

/// The most texels decoded for one sprite.
const MAX_TEXELS: usize = 64 << 20;

pub fn plausible_spr(head: &[u8]) -> bool {
    head.len() >= 12
        && &head[..4] == b"IDSP"
        && matches!(i32::from_le_bytes([head[4], head[5], head[6], head[7]]), 1 | 2)
}

pub fn plausible_sp2(head: &[u8]) -> bool {
    head.len() >= 12 && &head[..4] == b"IDS2" && i32::from_le_bytes([head[4], head[5], head[6], head[7]]) == 2
}

const TYPES: [&str; 5] = ["parallel upright", "facing upright", "parallel", "oriented", "parallel oriented"];
const FORMATS: [&str; 4] = ["normal", "additive", "index alpha", "alpha test"];

pub fn load_spr(bytes: &[u8], limits: &Limits) -> Result<Vec<NamedImage>> {
    let b = Bytes::new(bytes);
    if !plausible_spr(bytes) {
        return Err(LoadError::NotMine);
    }
    let version = b.i32(4)?;
    let half_life = version == 2;
    let kind = b.i32(8)?;
    // Version 1: type, radius, width, height, frames, beam length, sync.
    // Version 2 adds the texture format after the type.
    let (format, mut at) = if half_life { (b.i32(12)?, 16) } else { (0, 12) };
    at += 4; // bounding radius
    let (width, height) = (b.i32(at)?, b.i32(at + 4)?);
    let frames = cap(b.index(at + 8)?, MAX_FRAMES, "sprite frames")?;
    at += 20; // width, height, frames, beam length, sync type
    let palette: Vec<u8> = if half_life {
        let n = b.u16(at)? as usize;
        at += 2;
        let raw = b.slice(at, n.min(256) * 3)?;
        at += n * 3;
        let mut p = raw.to_vec();
        p.resize(768, 0);
        p
    } else {
        QUAKE_PALETTE.to_vec()
    };
    let _ = (width, height);
    let kind_name = TYPES.get(kind.max(0) as usize).copied().unwrap_or("unknown type");
    let format_name = FORMATS.get(format.max(0) as usize).copied().unwrap_or("unknown format");
    let container = if half_life { "Half-Life sprite" } else { "Quake sprite" };
    let notes = if half_life { format!("{kind_name}, {format_name} blending") } else { kind_name.to_string() };

    let mut items: Vec<NamedImage> = Vec::new();
    let mut texels = 0usize;
    let mut one = |at: usize, name: String| -> Result<(usize, NamedImage)> {
        let (ox, oy, w, h) = (b.i32(at)?, b.i32(at + 4)?, b.i32(at + 8)? as i64, b.i32(at + 12)? as i64);
        if w <= 0 || h <= 0 {
            return Err(LoadError::invalid("sprite frame has no size"));
        }
        let (w, h) = (w as u32, h as u32);
        let n = pixels::check_size(w, h)?;
        texels += n;
        if texels > MAX_TEXELS {
            return Err(LoadError::TooLarge("the sprite's frames hold too many texels".into()));
        }
        let data = b.slice(at + 16, n)?;
        let rgba = sprite_rgba(data, w, h, &palette, half_life, format);
        let detail = format!("{notes}, frame {w}x{h}, origin {ox},{oy}");
        let texture = Arc::new(pixels::texture(w, h, rgba, limits.max_texture_size, container, &detail));
        Ok((at + 16 + n, NamedImage { name, texture }))
    };
    for f in 0..frames {
        let group = b.i32(at)?;
        at += 4;
        match group {
            0 => {
                let (next, item) = one(at, format!("frame {f}"))?;
                at = next;
                items.push(item);
            }
            1 => {
                let n = cap(b.index(at)?, MAX_FRAMES, "sprite group frames")?;
                at += 4;
                b.slice(at, n * 4)?; // the intervals
                at += n * 4;
                for k in 0..n {
                    let (next, item) = one(at, format!("group {f} frame {k}"))?;
                    at = next;
                    items.push(item);
                }
            }
            n => return Err(LoadError::invalid(format!("sprite frame type {n}"))),
        }
        if items.len() > MAX_FRAMES {
            break;
        }
    }
    if items.is_empty() {
        return Err(LoadError::invalid("the sprite has no frames"));
    }
    Ok(items)
}

/// Palette indices to RGBA the way each texture format blends.
fn sprite_rgba(indices: &[u8], w: u32, h: u32, palette: &[u8], half_life: bool, format: i32) -> Vec<u8> {
    match (half_life, format) {
        (true, 1) => {
            // Additive: black adds nothing, so brightness is the opacity.
            let mut out = pixels::expand(indices, w, h, palette, None);
            for p in out.chunks_exact_mut(4) {
                p[3] = p[0].max(p[1]).max(p[2]);
            }
            out
        }
        (true, 2) => {
            // Index alpha: every pixel is the palette's last colour, with
            // the index as its opacity.
            let last = &palette[255 * 3..256 * 3];
            let mut out = Vec::with_capacity(indices.len() * 4);
            for &i in indices {
                out.extend_from_slice(&[last[0], last[1], last[2], i]);
            }
            out
        }
        (true, 0) => pixels::expand(indices, w, h, palette, None),
        _ => pixels::expand(indices, w, h, palette, Some(255)),
    }
}

/// Quake 2's sprite: the frames' pictures found next to it.
pub fn load_sp2(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Vec<NamedImage>> {
    if !plausible_sp2(bytes) {
        return Err(LoadError::NotMine);
    }
    let b = Bytes::new(bytes);
    let count = cap(b.index(8)?, MAX_FRAMES, "sprite frames")?;
    let mut items = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for f in 0..count {
        let at = 12 + f * 80;
        let (w, h, ox, oy) = (b.i32(at)?, b.i32(at + 4)?, b.i32(at + 8)?, b.i32(at + 12)?);
        let name = cstr(b.slice(at + 16, 64)?);
        if name.is_empty() {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        let readable = [".pcx", ".png", ".tga", ".jpg", ".jpeg", ".bmp"].iter().any(|e| lower.ends_with(e));
        let found = if limits.neighbours && readable { find_picture(path, &name) } else { None };
        let decoded =
            found.and_then(|p| read_neighbour(&p)).and_then(|d| decode_image(&d, limits.max_texture_size).ok());
        match decoded {
            Some(mut tex) => {
                tex.source.container = "Quake 2 sprite frame".into();
                tex.source.format = format!("{name}, frame {w}x{h}, origin {ox},{oy}, picture found beside the sprite");
                items.push(NamedImage { name: format!("frame {f}: {}", base_name(&name)), texture: Arc::new(tex) });
            }
            None => missing.push(name),
        }
    }
    if items.is_empty() {
        let list = missing.iter().take(4).cloned().collect::<Vec<_>>().join(", ");
        return Err(LoadError::Unsupported(format!(
            "the sprite's frames are pictures in other files ({list}), none of which could be read"
        )));
    }
    Ok(items)
}

/// A frame's picture: in the sprite's own folder by file name, or at its
/// stored path below the sprite's folder or any folder above it (the
/// `baseq2` root).
fn find_picture(sprite: &Path, stored: &str) -> Option<std::path::PathBuf> {
    let dir = sprite.parent()?;
    if let Some(p) = find_in(dir, base_name(stored)) {
        return Some(p);
    }
    dir.ancestors().take(6).find_map(|root| find_relative(root, stored))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spr_v1(frames: &[Vec<u8>]) -> Vec<u8> {
        let mut f = b"IDSP".to_vec();
        for v in [1i32, 2] {
            f.extend(v.to_le_bytes());
        }
        // Radius, width, height, frame count, beam length, sync type.
        f.extend(0f32.to_le_bytes());
        f.extend(2i32.to_le_bytes());
        f.extend(2i32.to_le_bytes());
        f.extend((frames.len() as i32).to_le_bytes());
        f.extend(0f32.to_le_bytes());
        f.extend(0i32.to_le_bytes());
        for fr in frames {
            f.extend(fr);
        }
        f
    }

    fn frame(w: i32, h: i32, px: &[u8]) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend(0i32.to_le_bytes()); // single frame
        for v in [-1i32, 1, w, h] {
            f.extend(v.to_le_bytes());
        }
        f.extend(px);
        f
    }

    #[test]
    fn quake_frames_use_the_quake_palette_with_transparent_255() {
        let f = spr_v1(&[frame(2, 2, &[1, 2, 255, 4]), frame(1, 1, &[7])]);
        assert!(plausible_spr(&f));
        let items = load_spr(&f, &Limits::viewer()).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "frame 0");
        assert_eq!(items[0].texture.source.container, "Quake sprite");
        let slerp_scene::textures::Pixels::Rgba8(p) = items[0].texture.base() else { panic!() };
        assert_eq!(&p[..3], &QUAKE_PALETTE[3..6]);
        assert_eq!(p[2 * 4 + 3], 0);
        assert_eq!(p[3], 255);
    }

    #[test]
    fn groups_list_their_frames() {
        let mut g = Vec::new();
        g.extend(1i32.to_le_bytes());
        g.extend(2i32.to_le_bytes());
        g.extend(0.1f32.to_le_bytes());
        g.extend(0.2f32.to_le_bytes());
        for _ in 0..2 {
            g.extend([0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0]);
            g.push(9);
        }
        let f = spr_v1(&[g]);
        let items = load_spr(&f, &Limits::viewer()).unwrap();
        assert_eq!(items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["group 0 frame 0", "group 0 frame 1"]);
    }

    #[test]
    fn half_life_sprites_carry_their_palette() {
        let mut f = b"IDSP".to_vec();
        for v in [2i32, 3, 3] {
            f.extend(v.to_le_bytes()); // version, type, texture format: alpha test
        }
        f.extend(0f32.to_le_bytes());
        f.extend(1i32.to_le_bytes());
        f.extend(1i32.to_le_bytes());
        f.extend(1i32.to_le_bytes());
        f.extend(0f32.to_le_bytes());
        f.extend(0i32.to_le_bytes());
        f.extend(256u16.to_le_bytes());
        let mut pal = vec![0u8; 768];
        pal[3..6].copy_from_slice(&[10, 20, 30]);
        f.extend(&pal);
        f.extend(frame(2, 1, &[1, 255]));
        let items = load_spr(&f, &Limits::viewer()).unwrap();
        let slerp_scene::textures::Pixels::Rgba8(p) = items[0].texture.base() else { panic!() };
        assert_eq!(&p[..4], &[10, 20, 30, 255]);
        assert_eq!(p[7], 0);
        assert!(items[0].texture.source.format.contains("alpha test"));
        assert_eq!(items[0].texture.source.container, "Half-Life sprite");
    }

    #[test]
    fn damaged_sprites_fail_cleanly() {
        let f = spr_v1(&[frame(2, 2, &[1, 2, 3, 4])]);
        for len in 0..f.len() {
            let _ = load_spr(&f[..len], &Limits::viewer());
        }
        let mut huge = f.clone();
        huge[36 + 4 + 8..36 + 4 + 12].copy_from_slice(&i32::MAX.to_le_bytes());
        assert!(load_spr(&huge, &Limits::viewer()).is_err());
        let mut many = f.clone();
        many[24..28].copy_from_slice(&1_000_000i32.to_le_bytes());
        assert!(load_spr(&many, &Limits::viewer()).is_err());
    }

    #[test]
    fn sp2_without_its_pictures_says_so() {
        let mut f = b"IDS2".to_vec();
        f.extend(2i32.to_le_bytes());
        f.extend(1i32.to_le_bytes());
        for v in [32i32, 32, 16, 16] {
            f.extend(v.to_le_bytes());
        }
        let mut name = b"sprites/s_bubble.pcx".to_vec();
        name.resize(64, 0);
        f.extend(name);
        assert!(plausible_sp2(&f));
        let e = load_sp2(Path::new("/nonexistent/s_bubble.sp2"), &f, &Limits::viewer()).err().unwrap().to_string();
        assert!(e.contains("s_bubble.pcx"), "{e}");
        for len in 0..f.len() {
            let _ = load_sp2(Path::new("/nonexistent/x.sp2"), &f[..len], &Limits::viewer());
        }
    }
}
