//! PICO-8 text cartridges (`.p8`).
//!
//! A cartridge is a text file of sections.  `__label__` holds the 128x128
//! cover picture the player takes with F7, one hex digit per pixel, and
//! `__gfx__` the sprite sheet in the same form.  The label is shown when
//! it exists, else the sprite sheet.  (The `.p8.png` cartridges are PNG
//! pictures already, and need nothing here.)

use crate::pic::{Pic, Rgb};
use slerp_scene::LoadError;

/// PICO-8's 16 colours, then the 16 "secret" ones (`pal(i, 128 + n, 1)`
/// reaches them).  A cartridge's pixel digits only ever name the first 16.
pub const PALETTE: [Rgb; 32] = [
    [0x00, 0x00, 0x00],
    [0x1d, 0x2b, 0x53],
    [0x7e, 0x25, 0x53],
    [0x00, 0x87, 0x51],
    [0xab, 0x52, 0x36],
    [0x5f, 0x57, 0x4f],
    [0xc2, 0xc3, 0xc7],
    [0xff, 0xf1, 0xe8],
    [0xff, 0x00, 0x4d],
    [0xff, 0xa3, 0x00],
    [0xff, 0xec, 0x27],
    [0x00, 0xe4, 0x36],
    [0x29, 0xad, 0xff],
    [0x83, 0x76, 0x9c],
    [0xff, 0x77, 0xa8],
    [0xff, 0xcc, 0xaa],
    [0x29, 0x18, 0x14],
    [0x11, 0x1d, 0x35],
    [0x42, 0x21, 0x36],
    [0x12, 0x53, 0x59],
    [0x74, 0x2f, 0x29],
    [0x49, 0x33, 0x3b],
    [0xa2, 0x88, 0x79],
    [0xf3, 0xef, 0x7d],
    [0xbe, 0x12, 0x50],
    [0xff, 0x6c, 0x24],
    [0xa8, 0xe7, 0x2e],
    [0x00, 0xb5, 0x43],
    [0x06, 0x5a, 0xb5],
    [0x75, 0x46, 0x65],
    [0xff, 0x6e, 0x59],
    [0xff, 0x9d, 0x81],
];

pub fn sniff(head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(&head[..head.len().min(64)]).to_ascii_lowercase();
    text.trim_start_matches('\u{feff}').starts_with("pico-8 cartridge")
}

/// The 128 rows of a section as pixel indices, rows or digits that are
/// missing counting as 0.
fn section(text: &str, name: &str) -> Option<Vec<u8>> {
    let mut lines = text.lines();
    lines.find(|l| l.trim_end() == name)?;
    let mut px = vec![0u8; 128 * 128];
    let mut any = false;
    for (y, line) in lines.take_while(|l| !l.starts_with("__")).take(128).enumerate() {
        for (x, c) in line.trim_end().chars().take(128).enumerate() {
            let v = c.to_digit(16).unwrap_or(0) as u8;
            any |= v != 0;
            px[y * 128 + x] = v;
        }
    }
    any.then_some(px)
}

pub fn decode(bytes: &[u8]) -> Result<(Pic, String), LoadError> {
    let text = String::from_utf8_lossy(bytes);
    if !sniff(bytes) {
        return Err(LoadError::invalid("not a PICO-8 cartridge"));
    }
    let (px, what) = match (section(&text, "__label__"), section(&text, "__gfx__")) {
        (Some(label), _) => (label, "label picture"),
        (None, Some(gfx)) => (gfx, "sprite sheet (the cartridge has no label)"),
        (None, None) => (vec![0; 128 * 128], "empty (no label and no sprites)"),
    };
    let pic = Pic::from_indexed(128, 128, &px, &PALETTE);
    Ok((pic, format!("128x128, {what}, PICO-8 palette")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cart(label: bool, gfx: bool) -> String {
        let mut s = String::from("pico-8 cartridge // http://www.pico-8.com\nversion 41\n__lua__\nprint(1)\n");
        if gfx {
            s.push_str("__gfx__\n");
            for y in 0..128 {
                s.push_str(&(if y == 0 { "8".repeat(128) } else { "0".repeat(128) }));
                s.push('\n');
            }
        }
        if label {
            s.push_str("__label__\n");
            for y in 0..128 {
                s.push_str(&(if y == 1 { "c".repeat(128) } else { "0".repeat(128) }));
                s.push('\n');
            }
        }
        s
    }

    #[test]
    fn label_wins_over_the_sprite_sheet() {
        let (pic, detail) = decode(cart(true, true).as_bytes()).unwrap();
        assert_eq!(pic.get(5, 1), PALETTE[12]);
        assert_eq!(pic.get(5, 0), PALETTE[0]);
        assert!(detail.contains("label"));
    }

    #[test]
    fn sprite_sheet_is_the_fallback() {
        let (pic, detail) = decode(cart(false, true).as_bytes()).unwrap();
        assert_eq!(pic.get(0, 0), PALETTE[8]);
        assert_eq!(pic.get(127, 0), PALETTE[8]);
        assert!(detail.contains("no label"));
    }

    #[test]
    fn carts_without_pictures_are_black() {
        let (pic, _) = decode(cart(false, false).as_bytes()).unwrap();
        assert_eq!(pic.get(3, 3), PALETTE[0]);
    }

    #[test]
    fn short_and_junk_sections_do_not_panic() {
        let s = "pico-8 cartridge // x\n__label__\n12\nzzzz\n\n__gfx__\n";
        let (pic, _) = decode(s.as_bytes()).unwrap();
        assert_eq!(pic.get(0, 0), PALETTE[1]);
        assert!(decode(b"hello").is_err());
    }

    #[test]
    fn the_secret_palette_is_complete() {
        assert_eq!(PALETTE.len(), 32);
        assert_eq!(PALETTE[16], [0x29, 0x18, 0x14]);
    }
}
