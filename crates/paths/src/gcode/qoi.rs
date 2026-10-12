//! The Quite OK Image format, which PrusaSlicer embeds for Prusa's own
//! printers (their firmware draws QOI fast).  Written from the format's
//! one-page specification (qoiformat.org).

use slerp_scene::textures::{ColorSpace, Texture};

/// The largest picture accepted, in pixels: far beyond any embedded
/// thumbnail.
const MAX_PIXELS: u64 = 16 << 20;

pub fn is_qoi(data: &[u8]) -> bool {
    data.starts_with(b"qoif")
}

/// Decodes a QOI picture, or `None` when it is damaged.  A stream that ends
/// early leaves the rest of the picture transparent.
pub fn decode(data: &[u8]) -> Option<Texture> {
    if data.len() < 14 || !is_qoi(data) {
        return None;
    }
    let width = u32::from_be_bytes(data[4..8].try_into().ok()?);
    let height = u32::from_be_bytes(data[8..12].try_into().ok()?);
    let pixels = width as u64 * height as u64;
    if width == 0 || height == 0 || pixels > MAX_PIXELS {
        return None;
    }
    let mut out = vec![0u8; pixels as usize * 4];
    let mut index = [[0u8; 4]; 64];
    let mut px = [0u8, 0, 0, 255];
    let mut i = 14;
    let mut o = 0;
    let hash = |p: [u8; 4]| (p[0] as usize * 3 + p[1] as usize * 5 + p[2] as usize * 7 + p[3] as usize * 11) % 64;
    while o < out.len() {
        let Some(&b) = data.get(i) else { break };
        i += 1;
        let mut run = 1usize;
        match b {
            0xfe => {
                let Some(rgb) = data.get(i..i + 3) else { break };
                px[..3].copy_from_slice(rgb);
                i += 3;
            }
            0xff => {
                let Some(rgba) = data.get(i..i + 4) else { break };
                px.copy_from_slice(rgba);
                i += 4;
            }
            _ => match b >> 6 {
                0 => px = index[(b & 63) as usize],
                1 => {
                    px[0] = px[0].wrapping_add(((b >> 4) & 3).wrapping_sub(2));
                    px[1] = px[1].wrapping_add(((b >> 2) & 3).wrapping_sub(2));
                    px[2] = px[2].wrapping_add((b & 3).wrapping_sub(2));
                }
                2 => {
                    let Some(&b2) = data.get(i) else { break };
                    i += 1;
                    let dg = (b & 63).wrapping_sub(32);
                    px[0] = px[0].wrapping_add(dg.wrapping_add((b2 >> 4).wrapping_sub(8)));
                    px[1] = px[1].wrapping_add(dg);
                    px[2] = px[2].wrapping_add(dg.wrapping_add((b2 & 15).wrapping_sub(8)));
                }
                _ => run = (b & 63) as usize + 1,
            },
        }
        if b >> 6 != 3 || b >= 0xfe {
            index[hash(px)] = px;
        }
        for _ in 0..run {
            if o >= out.len() {
                break;
            }
            out[o..o + 4].copy_from_slice(&px);
            o += 4;
        }
    }
    let mut t = Texture::from_rgba8(width, height, out, ColorSpace::Srgb);
    t.source.container = "QOI".into();
    t.source.format = "QOI".into();
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x2 picture written by hand: an RGB pixel, a run, an index hit
    /// after an RGBA pixel.
    #[test]
    fn decodes_each_op() {
        let mut q = b"qoif".to_vec();
        q.extend_from_slice(&3u32.to_be_bytes());
        q.extend_from_slice(&2u32.to_be_bytes());
        q.extend_from_slice(&[4, 0]);
        q.extend_from_slice(&[0xfe, 10, 20, 30]); // (10,20,30,255)
        q.push(0xc0); // run of 1
        q.extend_from_slice(&[0xff, 1, 2, 3, 4]); // (1,2,3,4)
        let h = (10 * 3 + 20 * 5 + 30 * 7 + 255 * 11) % 64;
        q.push(h as u8); // index: back to (10,20,30,255)
        q.push(0x40 | (3 << 4) | (2 << 2) | 1); // diff +1, 0, -1
        q.extend_from_slice(&[0x80 | (32 + 2), (8 + 1) << 4 | 8]); // luma dg+2, dr-dg+1, db-dg 0
        q.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        let t = decode(&q).unwrap();
        let slerp_scene::textures::Pixels::Rgba8(p) = t.base() else { panic!() };
        assert_eq!(&p[0..4], &[10, 20, 30, 255]);
        assert_eq!(&p[4..8], &[10, 20, 30, 255]);
        assert_eq!(&p[8..12], &[1, 2, 3, 4]);
        assert_eq!(&p[12..16], &[10, 20, 30, 255]);
        assert_eq!(&p[16..20], &[11, 20, 29, 255]);
        assert_eq!(&p[20..24], &[14, 22, 31, 255]);
    }

    #[test]
    fn damaged_streams_never_panic() {
        assert!(decode(b"qoif").is_none());
        let mut q = b"qoif".to_vec();
        q.extend_from_slice(&u32::MAX.to_be_bytes());
        q.extend_from_slice(&u32::MAX.to_be_bytes());
        q.extend_from_slice(&[4, 0]);
        assert!(decode(&q).is_none());
        let mut q = b"qoif".to_vec();
        q.extend_from_slice(&4u32.to_be_bytes());
        q.extend_from_slice(&4u32.to_be_bytes());
        q.extend_from_slice(&[4, 0, 0xff, 1]);
        assert!(decode(&q).is_some());
    }
}
