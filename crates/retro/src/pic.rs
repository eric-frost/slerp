//! The small picture type every retro decoder fills in, and the steps that
//! turn it into a `Texture`: palette lookup and stretching to the pixel
//! shape of the original display.
//!
//! Home computers drew pixels that were not square.  An Atari ST in medium
//! resolution has 640 columns but only 200 lines on a 4:3 screen, so each
//! pixel is twice as tall as wide; a C64 multicolour pixel is twice as wide
//! as a hires one.  Decoders therefore repeat pixels by whole factors until
//! the picture has the shape it had on the original monitor, which keeps
//! every pixel a crisp rectangle for thumbnails and the viewer's
//! nearest-neighbour filter alike.

use slerp_scene::{ColorSpace, LoadError, Texture};

/// The most pixels a retro picture may have.  The largest real ones are
/// Amiga IFF files; this keeps a damaged header from asking for gigabytes.
pub const MAX_PIXELS: u64 = 1 << 25;

/// One RGB colour.
pub type Rgb = [u8; 3];

/// An opaque RGBA picture.
pub struct Pic {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Pic {
    /// A black picture.  Callers check the size with [`check_size`] first
    /// when it comes from a file.
    pub fn new(width: u32, height: u32) -> Pic {
        let mut rgba = vec![0u8; width as usize * height as usize * 4];
        for p in rgba.chunks_exact_mut(4) {
            p[3] = 255;
        }
        Pic { width, height, rgba }
    }

    pub fn set(&mut self, x: u32, y: u32, c: Rgb) {
        if x < self.width && y < self.height {
            let i = (y as usize * self.width as usize + x as usize) * 4;
            self.rgba[i..i + 3].copy_from_slice(&c);
        }
    }

    #[cfg(test)]
    pub fn get(&self, x: u32, y: u32) -> Rgb {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2]]
    }

    /// A picture of palette indices: indices past the palette's end are
    /// black.
    pub fn from_indexed(width: u32, height: u32, indices: &[u8], palette: &[Rgb]) -> Pic {
        let mut pic = Pic::new(width, height);
        for (p, &i) in pic.rgba.chunks_exact_mut(4).zip(indices) {
            if let Some(c) = palette.get(i as usize) {
                p[..3].copy_from_slice(c);
            }
        }
        pic
    }

    /// Repeats every pixel `x` times across and `y` times down.
    pub fn stretch(self, x: u32, y: u32) -> Pic {
        if (x, y) == (1, 1) {
            return self;
        }
        let (w, h) = (self.width as usize, self.height as usize);
        let (nw, nh) = (w * x as usize, h * y as usize);
        let mut rgba = Vec::with_capacity(nw * nh * 4);
        for row in self.rgba.chunks_exact(w * 4) {
            let start = rgba.len();
            for px in row.chunks_exact(4) {
                for _ in 0..x {
                    rgba.extend_from_slice(px);
                }
            }
            let line = rgba[start..].to_vec();
            for _ in 1..y {
                rgba.extend_from_slice(&line);
            }
        }
        Pic { width: nw as u32, height: nh as u32, rgba }
    }

    /// Wraps the picture for the shared model.  `container` names the
    /// format and `detail` says what was decoded (size, colours, anything
    /// that was guessed), both shown in the viewer's inspector.
    pub fn into_texture(self, container: &str, detail: &str) -> Texture {
        let (w, h) = (self.width, self.height);
        let mut tex = Texture::from_rgba8(w, h, self.rgba, ColorSpace::Srgb);
        tex.source.container = container.into();
        tex.source.format = detail.into();
        tex
    }
}

/// Rejects zero and absurd sizes before anything is allocated for them.
pub fn check_size(width: u32, height: u32) -> Result<(), LoadError> {
    if width == 0 || height == 0 || width as u64 * height as u64 > MAX_PIXELS {
        return Err(LoadError::invalid(format!("picture size {width}x{height} is out of range")));
    }
    Ok(())
}

/// Scales a 3-bit colour channel (Atari ST, Amiga OCS-style) to 8 bits.
pub fn expand3(v: u8) -> u8 {
    let v = v & 7;
    (v << 5) | (v << 2) | (v >> 1)
}

/// Scales a 4-bit colour channel to 8 bits.
pub fn expand4(v: u8) -> u8 {
    (v & 15) * 17
}

/// The bytes at `at`, or a "truncated" error that names the format.
pub fn slice<'a>(bytes: &'a [u8], at: usize, len: usize, what: &str) -> Result<&'a [u8], LoadError> {
    at.checked_add(len)
        .and_then(|end| bytes.get(at..end))
        .ok_or_else(|| LoadError::invalid(format!("{what} is cut off")))
}

pub fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b.get(at).copied().unwrap_or(0), b.get(at + 1).copied().unwrap_or(0)])
}

pub fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b.get(at).copied().unwrap_or(0), b.get(at + 1).copied().unwrap_or(0)])
}

pub fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([
        b.get(at).copied().unwrap_or(0),
        b.get(at + 1).copied().unwrap_or(0),
        b.get(at + 2).copied().unwrap_or(0),
        b.get(at + 3).copied().unwrap_or(0),
    ])
}
