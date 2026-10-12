//! Apple II hi-res (`.hgr`, 8192 bytes) and double hi-res (`.dhr`, 16384
//! bytes) screen dumps.
//!
//! The Apple II makes colour from the television signal itself.  In
//! hi-res, a byte holds 7 dots (the lowest bit leftmost) and a "palette"
//! bit (bit 7); a lone lit dot on an even column shows violet or blue, on an
//! odd column green or orange, depending on the palette bit, and two lit
//! dots side by side blend to white.  This reproduces that classic
//! "artifact colour" look.  Double hi-res stores 560 dots per line in
//! alternating bytes of auxiliary and main memory (the `.dhr` file holds
//! auxiliary memory first), and four dots make one of 16 colours.
//!
//! Hi-res is drawn at twice size in both directions (560x384), double
//! hi-res twice as tall (560x384), so pixels come out square.

use crate::pic::{Pic, Rgb};
use slerp_scene::LoadError;

const BLACK: Rgb = [0, 0, 0];
const WHITE: Rgb = [255, 255, 255];
const VIOLET: Rgb = [255, 68, 253];
const GREEN: Rgb = [20, 245, 60];
const BLUE: Rgb = [20, 207, 253];
const ORANGE: Rgb = [255, 106, 60];

/// The Apple II's 16 low-resolution colours, which double hi-res reuses.
pub const LORES: [Rgb; 16] = [
    [0, 0, 0],
    [227, 30, 96],
    [96, 78, 189],
    [255, 68, 253],
    [0, 163, 96],
    [156, 156, 156],
    [20, 207, 253],
    [208, 195, 255],
    [96, 114, 3],
    [255, 106, 60],
    [156, 156, 156],
    [255, 160, 208],
    [20, 245, 60],
    [208, 221, 141],
    [114, 255, 208],
    [255, 255, 255],
];

/// The offset of text-style row group `y` (0 to 191) in a hi-res page.
pub fn line_offset(y: usize) -> usize {
    ((y & 7) << 10) | (((y >> 3) & 7) << 7) | ((y >> 6) * 40)
}

pub fn plausible_hgr(len: u64) -> bool {
    // DOS 3.3 and ProDOS save 8184 bytes (the last 8 of a page are unused),
    // and a BSAVE header adds 4.
    matches!(len, 8184 | 8188 | 8192 | 8196)
}

pub fn plausible_dhr(len: u64) -> bool {
    matches!(len, 16384 | 16388)
}

/// The 8192 bytes of a hi-res page from a file that may lack the page's last
/// 8 unused bytes or start with a 4-byte BSAVE header (load address and
/// length).
fn page(bytes: &[u8]) -> Option<Vec<u8>> {
    let body = match bytes.len() {
        8184 | 8192 => bytes,
        8188 | 8196 => &bytes[4..],
        _ => return None,
    };
    let mut page = body.to_vec();
    page.resize(8192, 0);
    Some(page)
}

/// Hi-res: `page` is the 8192 bytes of the screen.
pub fn hgr(bytes: &[u8]) -> Result<(Pic, String), LoadError> {
    let page = page(bytes).ok_or_else(|| LoadError::invalid("a hi-res page is 8192 bytes"))?;
    let mut pic = Pic::new(280, 192);
    for y in 0..192usize {
        let line = &page[line_offset(y)..line_offset(y) + 40];
        let mut dots = [false; 280];
        let mut palette = [false; 280];
        for (c, &b) in line.iter().enumerate() {
            for bit in 0..7 {
                dots[c * 7 + bit] = b & (1 << bit) != 0;
                palette[c * 7 + bit] = b & 0x80 != 0;
            }
        }
        for x in 0..280usize {
            if !dots[x] {
                continue;
            }
            let near = (x > 0 && dots[x - 1]) || (x < 279 && dots[x + 1]);
            let colour = if near {
                WHITE
            } else {
                match (x % 2 == 0, palette[x]) {
                    (true, false) => VIOLET,
                    (false, false) => GREEN,
                    (true, true) => BLUE,
                    (false, true) => ORANGE,
                }
            };
            pic.set(x as u32, y as u32, colour);
        }
    }
    Ok((pic.stretch(2, 2), "280x192, hi-res with NTSC artifact colours".into()))
}

/// Double hi-res.
pub fn dhgr(bytes: &[u8]) -> Result<(Pic, String), LoadError> {
    let pages = bytes.get(bytes.len().saturating_sub(16384)..).filter(|p| p.len() == 16384);
    let pages = pages.ok_or_else(|| LoadError::invalid("a double hi-res dump is 16384 bytes"))?;
    let (aux, main) = pages.split_at(8192);
    let mut pic = Pic::new(560, 192);
    for y in 0..192usize {
        let at = line_offset(y);
        // 560 dots: seven from each byte, auxiliary then main, 40 times.
        let mut dots = [false; 560];
        for c in 0..40usize {
            for bit in 0..7 {
                dots[c * 14 + bit] = aux[at + c] & (1 << bit) != 0;
                dots[c * 14 + 7 + bit] = main[at + c] & (1 << bit) != 0;
            }
        }
        for p in 0..140usize {
            let mut v = 0usize;
            for d in 0..4 {
                if dots[p * 4 + d] {
                    v |= 1 << d;
                }
            }
            for d in 0..4 {
                pic.set((p * 4 + d) as u32, y as u32, if v == 0 { BLACK } else { LORES[v] });
            }
        }
    }
    Ok((pic.stretch(1, 2), "560x192 dots, double hi-res, 16 colours (auxiliary memory first)".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_interleaved_like_the_hardware() {
        assert_eq!(line_offset(0), 0);
        assert_eq!(line_offset(1), 0x400);
        assert_eq!(line_offset(8), 0x80);
        assert_eq!(line_offset(64), 0x28);
        assert_eq!(line_offset(191), 0x1fd0);
    }

    #[test]
    fn lone_dots_take_artifact_colours() {
        let mut page = vec![0u8; 8192];
        // Row 0: dot 0 (even) lit, palette 0 -> violet; dot 3 (odd) -> green.
        page[0] = 0b0000_1001;
        // Next byte: palette bit set, dot 0 (x = 7, odd) -> orange.
        page[1] = 0b1000_0001;
        // Row 1 (offset 0x400): two adjacent dots -> white.
        page[0x400] = 0b0000_0011;
        let (pic, _) = hgr(&page).unwrap();
        assert_eq!((pic.width, pic.height), (560, 384));
        assert_eq!(pic.get(0, 0), VIOLET);
        assert_eq!(pic.get(1, 1), VIOLET);
        assert_eq!(pic.get(6, 0), GREEN);
        assert_eq!(pic.get(14, 0), ORANGE);
        assert_eq!(pic.get(0, 2), WHITE);
        assert_eq!(pic.get(2, 2), WHITE);
        assert_eq!(pic.get(100, 100), BLACK);
    }

    #[test]
    fn double_hi_res_groups_four_dots() {
        let mut dump = vec![0u8; 16384];
        // Dots 0-3 of line 0 are auxiliary bits 0-3: pattern 0b1111 = white;
        // dots 4-6 and main dot 0 (dot 7) give the next colour: bits 4,5,6
        // set and bit 7 clear in the group -> value 0b0111 = 7.
        dump[0] = 0b0111_1111;
        let (pic, _) = dhgr(&dump).unwrap();
        assert_eq!((pic.width, pic.height), (560, 384));
        assert_eq!(pic.get(0, 0), LORES[15]);
        assert_eq!(pic.get(3, 1), LORES[15]);
        assert_eq!(pic.get(4, 0), LORES[7]);
    }

    #[test]
    fn bsave_headers_are_skipped() {
        let mut f = vec![0x00, 0x20, 0x00, 0x20];
        f.extend(vec![0u8; 8192]);
        assert!(hgr(&f).is_ok());
        // DOS 3.3 binaries leave out the last 8 bytes of the page.
        assert!(hgr(&[0u8; 8184]).is_ok());
        assert!(plausible_hgr(8184) && !plausible_hgr(8000));
        assert!(hgr(&[0; 100]).is_err());
        assert!(dhgr(&[0; 8192]).is_err());
    }
}
