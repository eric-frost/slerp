//! Colour lookup tables: Adobe/Resolve `.cube` files (3D tables, 1D tables,
//! and Resolve's 1D shaper before a 3D table, with their input ranges) and
//! Autodesk `.3dl` files (Lustre and Flame meshes, with their input shaper
//! line and integer outputs).
//!
//! A table grades a picture by looking each pixel's colour up in a lattice
//! of colours.  Between lattice points this crate interpolates
//! tetrahedrally: the cube around a colour is cut into six tetrahedra and
//! the colour is blended from the four corners of the one it falls in.
//! That is exact for any linear transform (an identity table gives back
//! its input), smoother than trilinear blending along the grey axis, and
//! what DaVinci Resolve and OpenColorIO use.
//!
//! Thumbnails show a reference photo split diagonally: the original at the
//! top left, graded through the table at the bottom right.

mod parse;
mod photos;

pub use photos::{TestImage, graded, reference_photo, split_diagonal, split_vertical, test_images};

use rayon::prelude::*;
use slerp_scene::{Asset, ColorLut, Image8, Info, Limits, LoadError};
use std::path::Path;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LutFormat {
    /// Adobe's and Resolve's `.cube`.
    Cube,
    /// Autodesk's `.3dl`.
    ThreeDl,
}

/// Recognises a table by its name and first lines: `.cube` files must
/// declare a table size (the extension is shared with Gaussian cube files
/// from chemistry), `.3dl` files must start with a mesh or numbers.
pub fn sniff(path: &Path, head: &[u8]) -> Option<LutFormat> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    let text = String::from_utf8_lossy(head);
    let lines = || text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'));
    let sized = lines().take(64).any(|l| l.starts_with("LUT_3D_SIZE") || l.starts_with("LUT_1D_SIZE"));
    if sized && (ext == "cube" || !head.contains(&0)) {
        return Some(LutFormat::Cube);
    }
    if ext == "3dl" {
        let first = lines().next()?;
        let numeric = first.split_whitespace().all(|t| t.parse::<f64>().is_ok());
        if first.starts_with("3DMESH") || first.starts_with("Mesh") || numeric {
            return Some(LutFormat::ThreeDl);
        }
    }
    None
}

/// Reads a table file.
pub fn load(format: LutFormat, _path: &Path, bytes: &[u8], limits: &Limits) -> Result<Asset, LoadError> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{} bytes", bytes.len())));
    }
    Ok(Asset::Lut(Arc::new(parse(format, bytes)?)))
}

/// Parses a table file in memory.
pub fn parse(format: LutFormat, bytes: &[u8]) -> Result<Lut, LoadError> {
    let parsed = match format {
        LutFormat::Cube => parse::cube(bytes)?,
        LutFormat::ThreeDl => parse::three_dl(bytes)?,
    };
    Ok(Lut::new(parsed))
}

/*┌─────────────────╮
| Tables          |
└─────────────────╯*/
//region Tables

/// A per-channel curve: `size` entries spread evenly over `min..max` of
/// each input channel.
#[derive(Clone, Debug)]
pub struct Curves {
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub entries: Vec<[f32; 3]>,
}

/// A 3D lattice of `size` points along each axis, red changing fastest.
#[derive(Clone, Debug)]
pub struct Lattice {
    pub size: usize,
    pub min: [f32; 3],
    pub max: [f32; 3],
    /// Where the lattice's points sit along each input axis, as fractions
    /// of the domain, when not evenly spaced (a `.3dl` shaper line).
    pub axis: Option<Vec<f32>>,
    pub entries: Vec<[f32; 3]>,
}

/// What a file holds, before the 8-bit tables are built.
pub struct Parsed {
    pub info: Info,
    pub curves: Option<Curves>,
    pub lattice: Option<Lattice>,
}

/// Where a channel value lands in the lattice: the cell's lower index and
/// how far along it is.
#[derive(Clone, Copy, Debug, Default)]
struct Coord {
    index: u16,
    frac: f32,
}

pub struct Lut {
    info: Info,
    curves: Option<Curves>,
    lattice: Option<Lattice>,
    /// For each channel and 8-bit input, its lattice coordinate (with the
    /// curves already applied), when there is a lattice.
    coords: Option<Box<[[Coord; 256]; 3]>>,
    /// For a curves-only table, each channel's 8-bit output.
    bytes: Option<Box<[[u8; 256]; 3]>>,
}

impl Lut {
    pub fn new(p: Parsed) -> Lut {
        let mut lut = Lut { info: p.info, curves: p.curves, lattice: p.lattice, coords: None, bytes: None };
        match &lut.lattice {
            Some(l) => {
                let mut coords = Box::new([[Coord::default(); 256]; 3]);
                for (c, table) in coords.iter_mut().enumerate() {
                    for (v, slot) in table.iter_mut().enumerate() {
                        let x = lut.curve(c, v as f32 / 255.0);
                        *slot = l.coord(c, x);
                    }
                }
                lut.coords = Some(coords);
            }
            None => {
                let mut bytes = Box::new([[0u8; 256]; 3]);
                for (c, table) in bytes.iter_mut().enumerate() {
                    for (v, slot) in table.iter_mut().enumerate() {
                        *slot = to_byte(lut.curve(c, v as f32 / 255.0));
                    }
                }
                lut.bytes = Some(bytes);
            }
        }
        lut
    }

    /// Channel `c` of `x` through the curves, or `x` without them.
    fn curve(&self, c: usize, x: f32) -> f32 {
        let Some(cv) = &self.curves else { return x };
        let n = cv.entries.len();
        if n < 2 {
            return cv.entries.first().map_or(x, |e| e[c]);
        }
        let t = ((x - cv.min[c]) / (cv.max[c] - cv.min[c])).clamp(0.0, 1.0) * (n - 1) as f32;
        let i = (t as usize).min(n - 2);
        let f = t - i as f32;
        cv.entries[i][c] * (1.0 - f) + cv.entries[i + 1][c] * f
    }

    /// Grades one colour, in floating point.
    pub fn apply_rgb(&self, rgb: [f32; 3]) -> [f32; 3] {
        let shaped = [self.curve(0, rgb[0]), self.curve(1, rgb[1]), self.curve(2, rgb[2])];
        match &self.lattice {
            Some(l) => l.sample([l.coord(0, shaped[0]), l.coord(1, shaped[1]), l.coord(2, shaped[2])]),
            None => shaped,
        }
    }

    pub fn lattice_size(&self) -> Option<usize> {
        self.lattice.as_ref().map(|l| l.size)
    }

    pub fn curve_size(&self) -> Option<usize> {
        self.curves.as_ref().map(|c| c.entries.len())
    }
}

impl Lattice {
    /// Channel `c`'s lattice coordinate for the value `x`.
    fn coord(&self, c: usize, x: f32) -> Coord {
        let n = self.size;
        let range = self.max[c] - self.min[c];
        let t = if range > 0.0 { ((x - self.min[c]) / range).clamp(0.0, 1.0) } else { 0.0 };
        let t = if t.is_nan() { 0.0 } else { t };
        let pos = match &self.axis {
            None => t * (n - 1) as f32,
            Some(axis) => {
                // The last point at or below t, and how far towards the next.
                let i = axis.partition_point(|&a| a <= t).saturating_sub(1).min(n - 2);
                let span = axis[i + 1] - axis[i];
                i as f32 + if span > 0.0 { ((t - axis[i]) / span).clamp(0.0, 1.0) } else { 0.0 }
            }
        };
        let index = (pos as usize).min(n - 2);
        Coord { index: index as u16, frac: (pos - index as f32).clamp(0.0, 1.0) }
    }

    /// Tetrahedral interpolation inside the cell at `coords`.
    #[inline]
    fn sample(&self, [r, g, b]: [Coord; 3]) -> [f32; 3] {
        let n = self.size;
        let at = |dr: usize, dg: usize, db: usize| {
            self.entries[(r.index as usize + dr) + n * ((g.index as usize + dg) + n * (b.index as usize + db))]
        };
        let (fr, fg, fb) = (r.frac, g.frac, b.frac);
        let c000 = at(0, 0, 0);
        let c111 = at(1, 1, 1);
        // The path from the cell's black corner to its white corner, one
        // axis at a time in order of how far along each the colour is.
        let (w0, a, w1, b2, w2, w3) = if fr > fg {
            if fg > fb {
                (1.0 - fr, at(1, 0, 0), fr - fg, at(1, 1, 0), fg - fb, fb)
            } else if fr > fb {
                (1.0 - fr, at(1, 0, 0), fr - fb, at(1, 0, 1), fb - fg, fg)
            } else {
                (1.0 - fb, at(0, 0, 1), fb - fr, at(1, 0, 1), fr - fg, fg)
            }
        } else if fb > fg {
            (1.0 - fb, at(0, 0, 1), fb - fg, at(0, 1, 1), fg - fr, fr)
        } else if fb > fr {
            (1.0 - fg, at(0, 1, 0), fg - fb, at(0, 1, 1), fb - fr, fr)
        } else {
            (1.0 - fg, at(0, 1, 0), fg - fr, at(1, 1, 0), fr - fb, fb)
        };
        std::array::from_fn(|c| w0 * c000[c] + w1 * a[c] + w2 * b2[c] + w3 * c111[c])
    }
}

fn to_byte(v: f32) -> u8 {
    if v.is_nan() { 0 } else { (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8 }
}

impl ColorLut for Lut {
    fn info(&self) -> &Info {
        &self.info
    }

    fn apply(&self, rgba: &mut [u8]) {
        let grade = |px: &mut [u8]| {
            if let (Some(l), Some(coords)) = (&self.lattice, &self.coords) {
                let out = l.sample([coords[0][px[0] as usize], coords[1][px[1] as usize], coords[2][px[2] as usize]]);
                px[0] = to_byte(out[0]);
                px[1] = to_byte(out[1]);
                px[2] = to_byte(out[2]);
            } else if let Some(bytes) = &self.bytes {
                for c in 0..3 {
                    px[c] = bytes[c][px[c] as usize];
                }
            }
        };
        rgba.par_chunks_mut(4 * 4096).for_each(|chunk| chunk.chunks_exact_mut(4).for_each(grade));
    }
}

//endregion

/// The thumbnail of a table: the reference photo, no larger than `size`
/// (and never enlarged), the bottom right half graded.
pub fn thumbnail(lut: &dyn ColorLut, size: u32) -> Image8 {
    let original = photos::photo_at(size);
    let mut graded = original.clone();
    lut.apply(&mut graded.pixels);
    split_diagonal(&original, &graded)
}

#[cfg(test)]
mod tests;
