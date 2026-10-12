//! Pictures in memory: scaling with hard pixel edges or an alpha-aware box
//! filter, reading neighbouring image files safely, and turning decoded
//! textures into plain 8-bit RGBA.

use slerp_scene::textures::{DecodeOptions, Pixels, Texture};
use slerp_scene::{Image8, LoadError};
use std::path::Path;

/// The most pixels a composed picture may have: 64 million, a quarter of a
/// gigabyte.  Larger canvases compose at a reduced scale.
pub const MAX_PIXELS: u64 = 1 << 26;

/// The largest image file read from beside a map or sprite.
const MAX_IMAGE_FILE: u64 = 256 << 20;

/// A transparent picture.
pub fn blank(width: u32, height: u32) -> Image8 {
    Image8 { width, height, pixels: vec![0; width as usize * height as usize * 4] }
}

/// The pixel at `x`, `y`, which the caller has checked is inside.
#[inline]
pub fn get(img: &Image8, x: u32, y: u32) -> [u8; 4] {
    let i = (y as usize * img.width as usize + x as usize) * 4;
    [img.pixels[i], img.pixels[i + 1], img.pixels[i + 2], img.pixels[i + 3]]
}

/// Fits `width` x `height` into a square of `size`, keeping proportions:
/// shrunk when larger, enlarged by a whole factor when it fits at least
/// twice, so small pixel art keeps square, even pixels.
pub fn fit_size(width: u32, height: u32, size: u32) -> (u32, u32) {
    let long = width.max(height).max(1);
    let size = size.max(1);
    if long > size {
        let s = size as f64 / long as f64;
        (((width as f64 * s).round() as u32).max(1), ((height as f64 * s).round() as u32).max(1))
    } else {
        let k = size / long;
        (width * k, height * k)
    }
}

/// Resizes to `width` x `height`: nearest neighbour when enlarging, so
/// pixels stay hard-edged squares, and a box filter that weighs colour by
/// alpha when shrinking, so transparent pixels don't darken edges.
pub fn resize(img: &Image8, width: u32, height: u32) -> Image8 {
    let (sw, sh) = (img.width as usize, img.height as usize);
    let (tw, th) = (width.max(1) as usize, height.max(1) as usize);
    if (sw, sh) == (tw, th) || sw == 0 || sh == 0 {
        return if sw == 0 || sh == 0 { blank(width.max(1), height.max(1)) } else { img.clone() };
    }
    let mut out = blank(tw as u32, th as u32);
    for y in 0..th {
        // The source rows and columns this pixel covers: at least one, so
        // enlarging repeats pixels.
        let y0 = y * sh / th;
        let y1 = ((y + 1) * sh).div_ceil(th).max(y0 + 1).min(sh);
        for x in 0..tw {
            let x0 = x * sw / tw;
            let x1 = ((x + 1) * sw).div_ceil(tw).max(x0 + 1).min(sw);
            let mut sum = [0u64; 4];
            for sy in y0..y1 {
                let row = sy * sw;
                for sx in x0..x1 {
                    let p = &img.pixels[(row + sx) * 4..(row + sx) * 4 + 4];
                    let a = p[3] as u64;
                    sum[0] += p[0] as u64 * a;
                    sum[1] += p[1] as u64 * a;
                    sum[2] += p[2] as u64 * a;
                    sum[3] += a;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            let o = &mut out.pixels[(y * tw + x) * 4..(y * tw + x) * 4 + 4];
            if sum[3] > 0 {
                for c in 0..3 {
                    o[c] = ((sum[c] + sum[3] / 2) / sum[3]) as u8;
                }
                o[3] = ((sum[3] + n / 2) / n) as u8;
            }
        }
    }
    out
}

/// Makes a thumbnail of a composed picture no larger than `size`.
pub fn thumbnail(img: &Image8, size: u32) -> Image8 {
    let (w, h) = fit_size(img.width, img.height, size);
    resize(img, w, h)
}

/// Reads and decodes an image file next to the one being loaded: a
/// tileset's picture, an image layer.  Only regular files are read, and
/// only up to a size, so a path in a downloaded map can't make Slerp read
/// a device or a huge file.
pub fn read_image(path: &Path, max_pixels: u64) -> Result<Image8, LoadError> {
    decode(&read_neighbour(path, MAX_IMAGE_FILE)?, max_pixels)
}

/// Decodes a picture held in memory (a PNG inside a document), refusing
/// one larger than `max_pixels`.
pub fn decode(bytes: &[u8], max_pixels: u64) -> Result<Image8, LoadError> {
    // PNG, by far the usual case, says its size up front: refuse before
    // decoding rather than after.
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() >= 24 {
        let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]) as u64;
        let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]) as u64;
        if w * h > max_pixels {
            return Err(LoadError::TooLarge(format!("{w} × {h} pixels")));
        }
    }
    let tex = slerp_scene::textures::decode(bytes, &DecodeOptions::full())?;
    if tex.width() as u64 * tex.height() as u64 > max_pixels {
        return Err(LoadError::TooLarge(format!("{} × {} pixels", tex.width(), tex.height())));
    }
    Ok(to_rgba8(&tex))
}

/// Reads a regular file of at most `max` bytes.
pub fn read_neighbour(path: &Path, max: u64) -> Result<Vec<u8>, LoadError> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(LoadError::invalid(format!("{} is not a file", path.display())));
    }
    if meta.len() > max {
        return Err(LoadError::TooLarge(format!("{} has {} bytes", path.display(), meta.len())));
    }
    Ok(std::fs::read(path)?)
}

/// The first surface of a decoded image as 8-bit RGBA.  Images keep their
/// stored values; 16-bit ones are cut to 8 bits.
pub fn to_rgba8(tex: &Texture) -> Image8 {
    let (w, h) = (tex.width(), tex.height());
    let n = w as usize * h as usize * 4;
    let pixels = match tex.base() {
        Pixels::Rgba8(v) => v.get(..n).map(<[u8]>::to_vec).unwrap_or_else(|| vec![0; n]),
        Pixels::Rgba32F(v) => v.iter().take(n).map(|x| (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect(),
        // Only textures decoded for a GPU are in blocks.
        Pixels::Blocks(..) => match slerp_scene::textures::decompress(tex).base() {
            Pixels::Rgba8(v) => v.get(..n).map(<[u8]>::to_vec).unwrap_or_else(|| vec![0; n]),
            _ => vec![0; n],
        },
    };
    let mut img = Image8 { width: w, height: h, pixels };
    img.pixels.resize(n, 0);
    img
}

/// Turns pixels of one colour into transparency, as Tiled's `trans`
/// attribute asks for images without alpha.
pub fn key_out(img: &mut Image8, rgb: [u8; 3]) {
    for p in img.pixels.chunks_exact_mut(4) {
        if p[..3] == rgb {
            p.copy_from_slice(&[0, 0, 0, 0]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enlarges_hard_and_shrinks_by_alpha() {
        assert_eq!(fit_size(16, 8, 256), (256, 128));
        assert_eq!(fit_size(100, 50, 256), (200, 100));
        assert_eq!(fit_size(1000, 500, 256), (256, 128));
        let img = Image8 { width: 2, height: 1, pixels: vec![255, 0, 0, 255, 0, 0, 255, 0] };
        let big = resize(&img, 4, 2);
        assert_eq!(&big.pixels[..16], &[255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0]);
        // A red pixel beside a transparent blue one shrinks to half-covered
        // red, not purple.
        let small = resize(&img, 1, 1);
        assert_eq!(small.pixels, vec![255, 0, 0, 128]);
    }
}
