//! TIC-80 cartridges (`.tic`).
//!
//! A cartridge is a list of chunks, each a four-byte header (the type in the
//! low five bits of the first byte, the bank in the top three, then a
//! little-endian size and a spare byte) and its data.  The picture comes
//! from, in order: the screen chunk (240x136, 4 bits per pixel), the older
//! cover chunk (a GIF or PNG), and last the sprite sheet drawn with the
//! cartridge's palette.

use crate::gif;
use crate::pic::{Pic, Rgb, le16};
use slerp_scene::LoadError;
use slerp_scene::textures::{DecodeOptions, Pixels, decode as decode_image};

const TILES: u8 = 1;
const SPRITES: u8 = 2;
const COVER: u8 = 3;
const PALETTE: u8 = 12;
const DEFAULT: u8 = 17;
const SCREEN: u8 = 18;

/// Sweetie 16 by GrafxKid (CC0), TIC-80's default palette.
pub const SWEETIE_16: [Rgb; 16] = [
    [0x1a, 0x1c, 0x2c],
    [0x5d, 0x27, 0x5d],
    [0xb1, 0x3e, 0x53],
    [0xef, 0x7d, 0x57],
    [0xff, 0xcd, 0x75],
    [0xa7, 0xf0, 0x70],
    [0x38, 0xb7, 0x64],
    [0x25, 0x71, 0x79],
    [0x29, 0x36, 0x6f],
    [0x3b, 0x5d, 0xc9],
    [0x41, 0xa6, 0xf6],
    [0x73, 0xef, 0xf7],
    [0xf4, 0xf4, 0xf4],
    [0x94, 0xb0, 0xc2],
    [0x56, 0x6c, 0x86],
    [0x33, 0x3c, 0x57],
];

/// The DawnBringer 16 palette, which the oldest cartridges were drawn with.
pub const DB16: [Rgb; 16] = [
    [0x14, 0x0c, 0x1c],
    [0x44, 0x24, 0x34],
    [0x30, 0x34, 0x6d],
    [0x4e, 0x4a, 0x4e],
    [0x85, 0x4c, 0x30],
    [0x34, 0x65, 0x24],
    [0xd0, 0x46, 0x48],
    [0x75, 0x71, 0x61],
    [0x59, 0x7d, 0xce],
    [0xd2, 0x7d, 0x2c],
    [0x85, 0x95, 0xa1],
    [0x6d, 0xaa, 0x2c],
    [0xd2, 0xaa, 0x99],
    [0x6d, 0xc2, 0xca],
    [0xda, 0xd4, 0x5e],
    [0xde, 0xee, 0xd6],
];

struct Chunk<'a> {
    kind: u8,
    bank: u8,
    data: &'a [u8],
}

fn chunks(bytes: &[u8]) -> Vec<Chunk<'_>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 4 <= bytes.len() && out.len() < 4096 {
        let (kind, bank) = (bytes[at] & 0x1f, bytes[at] >> 5);
        let size = le16(bytes, at + 1) as usize;
        let end = (at + 4 + size).min(bytes.len());
        out.push(Chunk { kind, bank, data: &bytes[at + 4..end] });
        at += 4 + size;
    }
    out
}

/// Whether the bytes look like a cartridge: a chunk list whose first types
/// are known ones and whose sizes fit the file.
pub fn sniff(bytes: &[u8], file_len: u64) -> bool {
    if bytes.len() < 4 {
        return false;
    }
    let mut at = 0usize;
    let mut seen = 0;
    while at + 4 <= bytes.len() && seen < 6 {
        let kind = bytes[at] & 0x1f;
        if kind > 20 || bytes[at + 3] != 0 {
            return false;
        }
        at += 4 + le16(bytes, at + 1) as usize;
        seen += 1;
    }
    seen >= 1 && (at as u64) <= file_len + 0xffff
}

fn nibbles(data: &[u8], width: usize, height: usize, pal: &[Rgb]) -> Pic {
    let mut idx = vec![0u8; width * height];
    for (i, px) in idx.iter_mut().enumerate() {
        if let Some(&b) = data.get(i / 2) {
            *px = if i & 1 == 0 { b & 15 } else { b >> 4 };
        }
    }
    Pic::from_indexed(width as u32, height as u32, &idx, pal)
}

pub fn decode(bytes: &[u8]) -> Result<(Pic, String), LoadError> {
    let list = chunks(bytes);
    if list.is_empty() {
        return Err(LoadError::invalid("not a TIC-80 cartridge"));
    }
    // Carts from before the default-palette chunk existed carry neither a
    // palette nor that marker, and were drawn with DB16.
    let marked_default = list.iter().any(|c| c.kind == DEFAULT);
    let mut pal: Vec<Rgb> =
        if marked_default || list.iter().any(|c| c.kind == PALETTE) { SWEETIE_16 } else { DB16 }.to_vec();
    let mut custom = false;
    if let Some(c) = list.iter().find(|c| c.kind == PALETTE && c.bank == 0)
        && c.data.len() >= 48
    {
        pal = c.data[..48].chunks_exact(3).map(|p| [p[0], p[1], p[2]]).collect();
        custom = true;
    }
    let palette_note = if custom {
        "the cartridge's palette"
    } else if marked_default || list.iter().any(|c| c.kind == PALETTE) {
        "the default Sweetie 16 palette"
    } else {
        "the DB16 palette of early cartridges"
    };

    // 1. A cover screen.
    if let Some(c) = list.iter().find(|c| c.kind == SCREEN && c.data.len() > 64) {
        return Ok((nibbles(c.data, 240, 136, &pal), format!("240x136, cover screen, {palette_note}")));
    }
    // 2. An older cover picture.
    if let Some(c) = list.iter().find(|c| c.kind == COVER)
        && let Some(pic) = cover_picture(c.data)
    {
        return Ok((pic, "cover picture stored in the cartridge".into()));
    }
    // 3. The sprite sheet (the "sprites" chunk, or the tiles when only those
    //    were drawn), 256 sprites of 8x8 pixels arranged 16 across.
    let sheet = list
        .iter()
        .find(|c| c.kind == SPRITES && c.bank == 0 && c.data.iter().any(|&b| b != 0))
        .or_else(|| list.iter().find(|c| c.kind == TILES && c.bank == 0));
    let mut idx = vec![0u8; 128 * 128];
    let mut what = "no cover; empty sprite sheet";
    if let Some(c) = sheet {
        what = if c.kind == SPRITES { "no cover; sprite sheet" } else { "no cover; tile sheet" };
        for i in 0..(c.data.len() * 2).min(256 * 64) {
            let b = c.data[i / 2];
            let v = if i & 1 == 0 { b & 15 } else { b >> 4 };
            let (sprite, within) = (i / 64, i % 64);
            let (x, y) = ((sprite % 16) * 8 + within % 8, (sprite / 16) * 8 + within / 8);
            idx[y * 128 + x] = v;
        }
    }
    Ok((Pic::from_indexed(128, 128, &idx, &pal), format!("128x128, {what}, {palette_note}")))
}

/// A cover chunk holds a GIF or PNG, possibly after a length word.
fn cover_picture(data: &[u8]) -> Option<Pic> {
    for skip in [0usize, 2] {
        let d = data.get(skip..)?;
        if gif::is_gif(d) {
            return gif::decode(d).ok();
        }
        if d.starts_with(b"\x89PNG") {
            let tex = decode_image(d, &DecodeOptions::full()).ok()?;
            let (w, h) = (tex.width(), tex.height());
            let Pixels::Rgba8(rgba) = tex.base() else { return None };
            return Some(Pic { width: w, height: h, rgba: rgba.clone() });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(kind: u8, data: &[u8]) -> Vec<u8> {
        let mut v = vec![kind, data.len() as u8, (data.len() >> 8) as u8, 0];
        v.extend(data);
        v
    }

    #[test]
    fn sprite_sheet_uses_the_default_palette() {
        let mut sprites = vec![0u8; 8192];
        // Sprite 0, pixel (0,0) = colour 3, pixel (1,0) = colour 5.
        sprites[0] = 0x53;
        // Sprite 17 (x 8, y 8 in sprite units: column 1, row 1), pixel (2,1).
        sprites[17 * 32 + (8 + 2) / 2] = 0x0c;
        let f = [chunk(1, &[0; 32]), chunk(DEFAULT, &[]), chunk(SPRITES, &sprites)].concat();
        assert!(sniff(&f[..64], f.len() as u64));
        let (pic, detail) = decode(&f).unwrap();
        assert_eq!((pic.width, pic.height), (128, 128));
        assert_eq!(pic.get(0, 0), SWEETIE_16[3]);
        assert_eq!(pic.get(1, 0), SWEETIE_16[5]);
        assert_eq!(pic.get(8 + 2, 8 + 1), SWEETIE_16[12]);
        assert!(detail.contains("sprite sheet") && detail.contains("Sweetie"));
    }

    #[test]
    fn early_cartridges_use_db16() {
        let mut sprites = vec![0u8; 8192];
        sprites[0] = 0x01;
        let f = [chunk(1, &[0; 32]), chunk(SPRITES, &sprites)].concat();
        let (pic, detail) = decode(&f).unwrap();
        assert_eq!(pic.get(0, 0), DB16[1]);
        assert!(detail.contains("DB16"));
    }

    #[test]
    fn cartridge_palette_replaces_the_default() {
        let mut pal = vec![0u8; 48];
        pal[9..12].copy_from_slice(&[10, 20, 30]);
        let mut sprites = vec![0u8; 8192];
        sprites[0] = 0x03;
        let f = [chunk(PALETTE, &pal), chunk(SPRITES, &sprites)].concat();
        let (pic, _) = decode(&f).unwrap();
        assert_eq!(pic.get(0, 0), [10, 20, 30]);
    }

    #[test]
    fn screen_chunk_is_the_cover() {
        let mut screen = vec![0u8; 16320];
        screen[0] = 0xa2;
        let f = [chunk(DEFAULT, &[]), chunk(SPRITES, &[1; 100]), chunk(SCREEN, &screen)].concat();
        let (pic, detail) = decode(&f).unwrap();
        assert_eq!((pic.width, pic.height), (240, 136));
        assert_eq!(pic.get(0, 0), SWEETIE_16[2]);
        assert_eq!(pic.get(1, 0), SWEETIE_16[10]);
        assert!(detail.contains("cover screen"));
    }

    #[test]
    fn gif_covers_are_read() {
        let gif: &[u8] = &[
            b'G', b'I', b'F', b'8', b'9', b'a', 2, 0, 2, 0, 0x81, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0,
            0x2c, 0, 0, 0, 0, 2, 0, 2, 0, 0, 2, 3, 0x44, 0x34, 0x05, 0, 0x3b,
        ];
        let mut data = (gif.len() as u16).to_le_bytes().to_vec();
        data.extend(gif);
        let f = chunk(COVER, &data);
        let (pic, detail) = decode(&f).unwrap();
        assert_eq!((pic.width, pic.height), (2, 2));
        assert_eq!(pic.get(1, 0), [0, 255, 0]);
        assert!(detail.contains("cover"));
    }

    #[test]
    fn junk_is_rejected_or_survives() {
        assert!(decode(b"").is_err());
        assert!(!sniff(&[0xff; 40], 40));
        let _ = decode(&[SPRITES, 0xff, 0xff, 0, 1, 2]);
        let _ = decode(&[COVER, 5, 0, 0, b'G', b'I', b'F', b'8', b'9']);
    }
}
