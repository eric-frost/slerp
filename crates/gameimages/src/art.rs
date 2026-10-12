//! Build engine tile sets (`.art`): Duke Nukem 3D, Shadow Warrior, Blood and
//! the games and source ports built on the same engine.
//!
//! A file starts with a version (always 1), a tile count, and the numbers of
//! the first and last tile it holds.  Three tables follow, one entry per
//! tile: width, height, and an animation word; then the pixels of every
//! tile, one byte per pixel (an index into the game's palette), stored
//! column by column.  Palette index 255 is transparent.
//!
//! The palette is not in the tile file.  It is `PALETTE.DAT` in the same
//! folder when there is one: 768 bytes of 6-bit colours, followed by shade
//! and translucency tables that are not needed here.  Without it the tiles
//! are drawn in a grey ramp, and the gallery says so.  No game's palette is
//! included in Slerp.

use crate::common::{Cursor, texture};
use slerp_scene::{LoadError, NamedImage};
use std::sync::Arc;

/// The most tiles one file may declare.
const MAX_TILES: usize = 9216;

/// The most tile pixels decoded in one go: 64 MB.
const MAX_PIXELS: u64 = 64 << 20;

/// EDuke32 puts the eight characters `BUILDART` before the usual header.
fn strip_marker(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(b"BUILDART").unwrap_or(bytes)
}

pub fn sniff(head: &[u8]) -> bool {
    let head = strip_marker(head);
    if head.len() < 16 || head[..4] != [1, 0, 0, 0] {
        return false;
    }
    let (start, end) = (i32_at(head, 8), i32_at(head, 12));
    start >= 0 && end >= start && ((end as i64 - start as i64) as usize) < MAX_TILES
}

fn i32_at(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Palette entries from `PALETTE.DAT` bytes: 6-bit values widened to 8 bits.
pub fn palette_from_dat(dat: &[u8]) -> Option<Vec<[u8; 3]>> {
    let raw = dat.get(..768)?;
    if raw.iter().any(|&v| v > 63) {
        return None;
    }
    let wide = |v: u8| (v << 2) | (v >> 4);
    Some(raw.chunks_exact(3).map(|c| [wide(c[0]), wide(c[1]), wide(c[2])]).collect())
}

fn grey_ramp() -> Vec<[u8; 3]> {
    (0..256).map(|i| [i as u8; 3]).collect()
}

/// Decodes every non-empty tile.  `palette` is `PALETTE.DAT` if it was found
/// beside the file.
pub fn decode(bytes: &[u8], palette: Option<&[u8]>) -> Result<Vec<NamedImage>, LoadError> {
    let bytes = strip_marker(bytes);
    if !sniff(bytes) {
        return Err(LoadError::invalid("not a Build ART file"));
    }
    let mut c = Cursor::new(bytes);
    c.take(8)?;
    let (start, end) = (c.i32()?, c.i32()?);
    let count = (end - start + 1) as usize;
    let widths = c.take(count * 2)?;
    let heights = c.take(count * 2)?;
    let anims = c.take(count * 4)?;
    let (pal, pal_note) = match palette.and_then(palette_from_dat) {
        Some(p) => (p, "palette from PALETTE.DAT"),
        None => (grey_ramp(), "no PALETTE.DAT beside the file, so the tiles are shown in grey"),
    };
    let mut items = Vec::new();
    let mut pixels_left = MAX_PIXELS;
    for i in 0..count {
        let w = u16::from_le_bytes([widths[i * 2], widths[i * 2 + 1]]) as usize;
        let h = u16::from_le_bytes([heights[i * 2], heights[i * 2 + 1]]) as usize;
        let n = w * h;
        if n == 0 {
            continue;
        }
        if n as u64 > pixels_left {
            return Err(LoadError::TooLarge("the tiles hold too many pixels".into()));
        }
        pixels_left -= n as u64;
        let data = match c.take(n) {
            Ok(d) => d,
            // A cut-off file shows the tiles it has.
            Err(_) => break,
        };
        // Stored by column; the picture is by row.
        let mut rgba = vec![0u8; n * 4];
        for x in 0..w {
            for y in 0..h {
                let idx = data[x * h + y] as usize;
                let o = (y * w + x) * 4;
                // Index 255 is see-through; its colour is black, so a view that
                // ignores alpha (a thumbnail) shows a dark gap rather than the
                // palette's magenta.
                let col = if idx == 255 { [0, 0, 0] } else { pal[idx] };
                rgba[o..o + 4].copy_from_slice(&[col[0], col[1], col[2], if idx == 255 { 0 } else { 255 }]);
            }
        }
        let number = start as usize + i;
        let frames = anims[i * 4] & 63;
        let detail = format!(
            "Build tile {number}, {w}x{h}, {pal_note}{}",
            if frames > 0 { format!(", {frames} animation frames follow") } else { String::new() }
        );
        items.push(NamedImage {
            name: format!("tile {number}"),
            texture: Arc::new(texture(w as u32, h as u32, rgba, "Build engine ART", &detail)),
        });
    }
    if items.is_empty() {
        return Err(LoadError::invalid("the ART file holds no tiles"));
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use slerp_scene::Texture;
    use slerp_scene::textures::Pixels;

    fn px(t: &Texture) -> &[u8] {
        let Pixels::Rgba8(v) = t.base() else { panic!() };
        v
    }

    /// An ART file with the given tiles (width, height, by-column pixels).
    fn art(first: i32, tiles: &[(u16, u16, Vec<u8>)]) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend(1i32.to_le_bytes());
        f.extend((tiles.len() as i32).to_le_bytes());
        f.extend(first.to_le_bytes());
        f.extend((first + tiles.len() as i32 - 1).to_le_bytes());
        for t in tiles {
            f.extend(t.0.to_le_bytes());
        }
        for t in tiles {
            f.extend(t.1.to_le_bytes());
        }
        for _ in tiles {
            f.extend(0u32.to_le_bytes());
        }
        for t in tiles {
            f.extend(&t.2);
        }
        f
    }

    #[test]
    fn tiles_are_stored_by_column() {
        // A 2x3 tile: columns [1,2,3] and [4,5,255].
        let f = art(100, &[(2, 3, vec![1, 2, 3, 4, 5, 255])]);
        assert!(sniff(&f));
        let items = decode(&f, None).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "tile 100");
        let t = &items[0].texture;
        assert_eq!((t.width(), t.height()), (2, 3));
        let p = px(t);
        // Row 0: indices 1 and 4; row 2: indices 3 and 255 (transparent).
        assert_eq!(p[0], 1);
        assert_eq!(p[4], 4);
        assert_eq!(p[2 * 8], 3);
        assert_eq!(p[2 * 8 + 7], 0);
        assert!(t.has_alpha);
        assert!(t.source.format.contains("no PALETTE.DAT"));
    }

    #[test]
    fn palette_dat_widens_six_bit_colours() {
        let mut dat = vec![0u8; 768 + 100];
        dat[3..6].copy_from_slice(&[63, 32, 0]);
        let f = art(0, &[(1, 1, vec![1])]);
        let items = decode(&f, Some(&dat)).unwrap();
        assert_eq!(&px(&items[0].texture)[..4], &[255, 130, 0, 255]);
        assert!(items[0].texture.source.format.contains("from PALETTE.DAT"));
        // Values above 63 are not a PALETTE.DAT.
        dat[0] = 200;
        assert!(palette_from_dat(&dat).is_none());
    }

    #[test]
    fn empty_tiles_are_skipped_and_cut_off_files_keep_what_they_have() {
        let f = art(0, &[(0, 0, vec![]), (1, 2, vec![9, 9]), (4, 4, vec![7; 16])]);
        let items = decode(&f, None).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].name, "tile 2");
        let cut = &f[..f.len() - 8];
        assert_eq!(decode(cut, None).unwrap().len(), 1);
    }

    #[test]
    fn eduke32_files_start_with_a_marker() {
        let plain = art(3022, &[(2, 1, vec![5, 6])]);
        let mut marked = b"BUILDART".to_vec();
        marked.extend(&plain);
        assert!(sniff(&marked));
        let items = decode(&marked, None).unwrap();
        assert_eq!(items[0].name, "tile 3022");
    }

    #[test]
    fn hostile_headers_are_bounded() {
        let mut f = Vec::new();
        f.extend(1i32.to_le_bytes());
        f.extend(0i32.to_le_bytes());
        f.extend(0i32.to_le_bytes());
        f.extend(i32::MAX.to_le_bytes());
        f.extend([0u8; 64]);
        assert!(!sniff(&f));
        assert!(decode(&f, None).is_err());
        // Huge tiles run out of the pixel budget before allocating.
        let big = art(0, &[(65535, 65535, vec![]), (65535, 65535, vec![])]);
        assert!(decode(&big, None).is_err());
        for len in 0..40 {
            let _ = decode(&big[..len.min(big.len())], None);
        }
    }
}
