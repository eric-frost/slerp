//! ASTC blocks with 3D footprints (3x3x3 to 6x6x6), which volume textures
//! use through Vulkan's VK_EXT_texture_compression_astc_3d and OpenGL ES's
//! OES_texture_compression_astc.  basisu decodes only 2D footprints, so
//! this module decodes whole blocks itself, following the ASTC chapter of
//! the Khronos Data Format Specification.  Three things differ in 3D: the
//! block mode layout, the void-extent coordinates, and the weight infill,
//! which blends four grid weights picked by a simplex split of each grid
//! cell instead of bilinear interpolation.  The colour endpoint modes and
//! the partition hash are shared with 2D.  basisu's BISE reader,
//! dequantisation tables, partition hash (which already takes z) and half
//! float helpers are reused.
//!
//! It decodes 2D footprints too, though Slerp sends those to basisu: the
//! tests compare the two decoders on 2D blocks, which checks the parts both
//! dimensions share against an independent implementation, and compare 3D
//! files with ARM's astcenc.

use crate::color::half_to_f32;
use crate::util::{check_size, invalid};
use crate::{Pixels, TextureError};
use basisu::astc::decode::compute_texel_partition;
use basisu::astc::dequant::dequant_tables;
use basisu::astc::half::{half_from_unorm16, is_half_inf_or_nan, qlog16_to_half};
use basisu::astc::unpack::{Bits128, decode_bise, ise_sequence_bits};
use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// A block footprint in texels; `d` is 1 for 2D blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Footprint {
    pub w: u32,
    pub h: u32,
    pub d: u32,
}

impl Footprint {
    fn texels(self) -> usize {
        (self.w * self.h * self.d) as usize
    }
}

/// The most texels a block holds (6x6x6).
pub(crate) const MAX_TEXELS: usize = 216;

/// The colour a malformed block decodes to, as the specification asks:
/// magenta, in 8-bit and half-float form.
const ERROR_LDR: [u8; 4] = [255, 0, 255, 255];
const ERROR_HDR: [u16; 4] = [0x3C00, 0, 0x3C00, 0x3C00];

/*┌─────────────────╮
| Surfaces        |
└─────────────────╯*/
//region Surfaces

/// Decodes `depth` slices of `w` x `h` texels stored as blocks of `fp`:
/// slices of blocks one after another, each a row-major grid.  The result
/// is 8-bit RGBA unless `hdr` is set or a block turns out to hold HDR
/// values, in which case the whole surface decodes again to floats, since
/// only the SFLOAT formats promise HDR and raw `.astc` files say nothing.
pub(crate) fn decode_surface(
    fp: Footprint,
    srgb: bool,
    hdr: bool,
    w: u32,
    h: u32,
    depth: u32,
    data: &[u8],
) -> Result<Pixels, TextureError> {
    let grid = [w.div_ceil(fp.w), h.div_ceil(fp.h), depth.div_ceil(fp.d)];
    let blocks = grid.iter().map(|&n| n as u64).product::<u64>();
    if (data.len() as u64) < blocks.saturating_mul(16) {
        return Err(invalid(format!("ASTC: {} bytes of blocks for a surface that needs {}", data.len(), blocks * 16)));
    }
    let layout = Layout { fp, grid, w, h, depth };
    if !hdr {
        check_size(w, h, depth, 4)?;
        let found_hdr = AtomicBool::new(false);
        let out = layout.decode(data, ERROR_LDR, |block, out| {
            let ok = !found_hdr.load(Ordering::Relaxed) && decode_ldr(block, fp, srgb, out);
            if !ok {
                found_hdr.store(true, Ordering::Relaxed);
            }
            ok
        });
        if !found_hdr.into_inner() {
            return Ok(Pixels::Rgba8(out.into_flattened()));
        }
    }
    check_size(w, h, depth, 16)?;
    let out = layout.decode(data, ERROR_HDR, |block, out| {
        decode_hdr(block, fp, out);
        true
    });
    Ok(Pixels::Rgba32F(out.into_iter().flat_map(|t| t.map(half_to_f32)).collect()))
}

/// Where a surface's blocks and texels lie.
struct Layout {
    fp: Footprint,
    /// Blocks across, down and deep.
    grid: [u32; 3],
    w: u32,
    h: u32,
    depth: u32,
}

impl Layout {
    /// Decodes every block with `block(bytes, texels)`, which returns false
    /// to stop (an LDR pass that met HDR), into texels in slice, row,
    /// column order.  A slice of blocks at a time, its rows of blocks in
    /// parallel, each into a brick of whole rows that is then copied out.
    fn decode<T: Copy + Send + Sync>(
        &self,
        data: &[u8],
        fill: [T; 4],
        block: impl Fn(&[u8; 16], &mut [[T; 4]]) -> bool + Sync,
    ) -> Vec<[T; 4]> {
        let Layout { fp, grid: [gx, gy, gz], w, h, depth } = *self;
        let mut out = vec![fill; w as usize * h as usize * depth as usize];
        let brick = (fp.w * gx) as usize * fp.h as usize * fp.d as usize;
        for bz in 0..gz {
            let rows: Vec<Vec<[T; 4]>> = (0..gy)
                .into_par_iter()
                .map(|by| {
                    let mut texels = vec![fill; brick];
                    let mut tmp = [fill; MAX_TEXELS];
                    for bx in 0..gx {
                        let at = (((bz * gy + by) * gx + bx) as usize) * 16;
                        let bytes: &[u8; 16] = data[at..at + 16].try_into().unwrap();
                        if !block(bytes, &mut tmp) {
                            break;
                        }
                        // Brick layout: z, then y, then x across the whole row.
                        for (i, t) in tmp[..fp.texels()].iter().enumerate() {
                            let i = i as u32;
                            let (x, y, z) = (i % fp.w, i / fp.w % fp.h, i / (fp.w * fp.h));
                            texels[((z * fp.h + y) * fp.w * gx + bx * fp.w + x) as usize] = *t;
                        }
                    }
                    texels
                })
                .collect();
            for (by, texels) in rows.iter().enumerate() {
                let by = by as u32;
                for z in 0..fp.d.min(depth - bz * fp.d) {
                    for y in 0..fp.h.min(h - by * fp.h) {
                        let src = ((z * fp.h + y) * fp.w * gx) as usize;
                        let dst = (((bz * fp.d + z) * h + by * fp.h + y) * w) as usize;
                        out[dst..dst + w as usize].copy_from_slice(&texels[src..src + w as usize]);
                    }
                }
            }
        }
        out
    }
}

//endregion

/*┌─────────────────╮
| Blocks          |
└─────────────────╯*/
//region Blocks

/// Decodes a block to 8-bit RGBA, in the order x, then y, then z.  The
/// 16-bit result of each texel is cut to its top byte (the decode_unorm8
/// behaviour, as basisu's 2D decoder does).  Returns false, leaving `out`
/// alone, when the block holds HDR values, which need `decode_hdr`.
pub(crate) fn decode_ldr(block: &[u8; 16], fp: Footprint, srgb: bool, out: &mut [[u8; 4]]) -> bool {
    let n = fp.texels();
    match unpack(block, fp) {
        None => out[..n].fill(ERROR_LDR),
        Some(Block::Solid { hdr: true, .. }) => return false,
        Some(Block::Solid { hdr: false, color }) => out[..n].fill(color.map(|c| (c >> 8) as u8)),
        Some(Block::Weighted(b)) => {
            if b.hdr[..b.partitions as usize].iter().flatten().any(|&h| h) {
                return false;
            }
            b.texels(fp, |t, p, w| {
                for c in 0..4 {
                    let [lo, hi] = [b.endpoints[p][0][c], b.endpoints[p][1][c]];
                    let (lo, hi) = if srgb { ((lo << 8) | 0x80, (hi << 8) | 0x80) } else { (lo * 257, hi * 257) };
                    out[t][c] = (interpolate(lo, hi, w[c]) >> 8) as u8;
                }
            });
        }
    }
    true
}

/// Decodes a block to half floats, in the order x, then y, then z.  LDR
/// values become halves of their 16-bit value; HDR ones go through the
/// specification's logarithmic-to-half mapping.
pub(crate) fn decode_hdr(block: &[u8; 16], fp: Footprint, out: &mut [[u16; 4]]) {
    let n = fp.texels();
    let unorm = |k: i32| if k >= 0xFFFF { 0x3C00 } else { half_from_unorm16(k.max(0) as u32) };
    match unpack(block, fp) {
        None => out[..n].fill(ERROR_HDR),
        Some(Block::Solid { hdr: true, color }) => out[..n].fill(color),
        Some(Block::Solid { hdr: false, color }) => out[..n].fill(color.map(|c| unorm(c as i32))),
        Some(Block::Weighted(b)) => b.texels(fp, |t, p, w| {
            for c in 0..4 {
                let [lo, hi] = [b.endpoints[p][0][c], b.endpoints[p][1][c]];
                out[t][c] = if b.hdr[p][c] {
                    let v = qlog16_to_half(interpolate(lo << 4, hi << 4, w[c]));
                    if is_half_inf_or_nan(v) { 0x7BFF } else { v }
                } else {
                    unorm(interpolate(lo * 257, hi * 257, w[c]))
                };
            }
        }),
    }
}

/// Blends two 16-bit endpoint values by a weight of 0..64.
fn interpolate(lo: i32, hi: i32, w: u32) -> i32 {
    (lo * (64 - w as i32) + hi * w as i32 + 32) >> 6
}

/// A block's contents, unpacked from its 128 bits.  It lives on the stack
/// for one block at a time, so the size difference between variants costs
/// nothing.
#[allow(clippy::large_enum_variant)]
enum Block {
    /// A void-extent block: one colour, as 16-bit UNORM (LDR) or half
    /// floats (HDR).
    Solid {
        hdr: bool,
        color: [u16; 4],
    },
    Weighted(Weighted),
}

/// A block with colour endpoints and a grid of weights between them.
struct Weighted {
    /// The weight grid's size along x, y and z.
    grid: [u32; 3],
    /// Dequantised weights (0..64), one grid per plane.
    planes: [[u8; 64]; 2],
    /// The component that takes its weights from the second plane.
    ccs: Option<usize>,
    partitions: u32,
    seed: u32,
    /// Each partition's low and high endpoint, RGBA: 8-bit values for LDR
    /// components and 12-bit logarithmic ones for HDR components.
    endpoints: [[[i32; 4]; 2]; 4],
    /// Which components of each partition are HDR.
    hdr: [[bool; 4]; 4],
}

impl Weighted {
    /// Calls `f(texel, partition, weights)` for every texel of the block, in
    /// the order x, then y, then z, with each component's weight.
    fn texels(&self, fp: Footprint, mut f: impl FnMut(usize, usize, [u32; 4])) {
        let small = fp.texels() < 31;
        let mut t = 0;
        for z in 0..fp.d {
            for y in 0..fp.h {
                for x in 0..fp.w {
                    let taps = taps(fp, self.grid, x, y, z);
                    let blend = |plane: &[u8; 64]| {
                        let sum: u32 = taps.iter().map(|&(i, w)| *plane.get(i).unwrap_or(&0) as u32 * w).sum();
                        (sum + 8) >> 4
                    };
                    let w0 = blend(&self.planes[0]);
                    let mut w = [w0; 4];
                    if let Some(c) = self.ccs {
                        w[c] = blend(&self.planes[1]);
                    }
                    let p = if self.partitions > 1 {
                        compute_texel_partition(self.seed, x, y, z, self.partitions, small) as usize
                    } else {
                        0
                    };
                    f(t, p.min(3), w);
                    t += 1;
                }
            }
        }
    }
}

/// The grid weights texel (x, y, z) blends, with their shares out of 16:
/// bilinear in 2D, and in 3D the four corners of the simplex (one of six
/// tetrahedra of the grid cell) the texel falls in.  Zero shares may name
/// grid points past the edge; callers skip them.
fn taps(fp: Footprint, grid: [u32; 3], x: u32, y: u32, z: u32) -> [(usize, u32); 4] {
    // Positions on the grid in sixteenths.
    let pos = |t: u32, b: u32, g: u32| (((1024 + b / 2) / (b - 1)) * t * (g - 1) + 32) >> 6;
    let (gs, gt) = (pos(x, fp.w, grid[0]), pos(y, fp.h, grid[1]));
    let (n, nm) = (grid[0], grid[0] * grid[1]);
    if fp.d == 1 {
        let (fs, ft) = (gs & 15, gt & 15);
        let base = (gs >> 4) + (gt >> 4) * n;
        let w11 = (fs * ft + 8) >> 4;
        let at = |i: u32| i as usize;
        return [
            (at(base), 16 + w11 - fs - ft),
            (at(base + 1), fs - w11),
            (at(base + n), ft - w11),
            (at(base + n + 1), w11),
        ];
    }
    let gp = pos(z, fp.d, grid[2]);
    let (fs, ft, fp_) = (gs & 15, gt & 15, gp & 15);
    let base = (gs >> 4) + (gt >> 4) * n + (gp >> 4) * nm;
    // Walk from the cell's low corner to its high corner along the axes in
    // order of decreasing fraction.
    let (s1, s2, w) = match ((fs > ft) as u8) << 2 | ((ft > fp_) as u8) << 1 | (fs > fp_) as u8 {
        7 => (1, n, [16 - fs, fs - ft, ft - fp_, fp_]),
        3 => (n, 1, [16 - ft, ft - fs, fs - fp_, fp_]),
        5 => (1, nm, [16 - fs, fs - fp_, fp_ - ft, ft]),
        4 => (nm, 1, [16 - fp_, fp_ - fs, fs - ft, ft]),
        2 => (n, nm, [16 - ft, ft - fp_, fp_ - fs, fs]),
        _ => (nm, n, [16 - fp_, fp_ - ft, ft - fs, fs]),
    };
    let i = [base, base + s1, base + s1 + s2, base + 1 + n + nm];
    [(i[0] as usize, w[0]), (i[1] as usize, w[1]), (i[2] as usize, w[2]), (i[3] as usize, w[3])]
}

//endregion

/*┌─────────────────╮
| Unpacking       |
└─────────────────╯*/
//region Unpacking

/// A block mode: the weight grid's size, whether there are two planes of
/// weights, and their BISE range (0..=11).
struct Mode {
    grid: [u32; 3],
    dual: bool,
    range: u32,
}

/// Unpacks a block, or returns None for a malformed one (reserved block
/// modes, a weight grid larger than the block, too many or too few bits),
/// which decodes to the error colour.
fn unpack(block: &[u8; 16], fp: Footprint) -> Option<Block> {
    let bits = Bits128::from_le_bytes(block);
    let mode = bits.get_bits(0, 11);
    if mode & 0x1FF == 0x1FC {
        return void_extent(&bits, mode, fp.d > 1);
    }

    // 1. The weight grid.
    let m = if fp.d > 1 { mode_3d(mode)? } else { mode_2d(mode)? };
    if m.grid[0] > fp.w || m.grid[1] > fp.h || m.grid[2] > fp.d {
        return None;
    }
    let per_plane = (m.grid[0] * m.grid[1] * m.grid[2]) as usize;
    let count = per_plane * if m.dual { 2 } else { 1 };
    if count > 64 {
        return None;
    }
    let weight_bits = ise_sequence_bits(count as i32, m.range);
    if !(24..=96).contains(&weight_bits) {
        return None;
    }
    let below_weights = 128 - weight_bits as u32;

    // 2. Partitions and colour endpoint modes.  When partitions' modes
    //    differ, their high bits sit just below the weights.
    let partitions = bits.get_bits(11, 2) + 1;
    if m.dual && partitions == 4 {
        return None;
    }
    let mut cems = [0u32; 4];
    let mut extra = 0;
    let seed;
    if partitions == 1 {
        cems[0] = bits.get_bits(13, 4);
        seed = 0;
    } else {
        seed = bits.get_bits(13, 10);
        let low = bits.get_bits(23, 6);
        if low & 3 == 0 {
            cems[..partitions as usize].fill(low >> 2);
        } else {
            extra = 3 * partitions - 4;
            let encoded = low | (bits.get_bits(below_weights - extra, extra) << 6);
            let class = (encoded & 3) - 1;
            for (i, cem) in cems.iter_mut().enumerate().take(partitions as usize) {
                let c = (encoded >> (2 + i)) & 1;
                let m = (encoded >> (2 + partitions as usize + 2 * i)) & 3;
                *cem = ((class + c) << 2) | m;
            }
        }
    }
    let ccs = m.dual.then(|| bits.get_bits(below_weights - extra - 2, 2) as usize);

    // 3. Endpoint values: as many as the modes need, at the finest BISE
    //    range that fits in the bits left.
    let values: u32 = cems[..partitions as usize].iter().map(|&c| 2 + 2 * (c >> 2)).sum();
    if values > 18 {
        return None;
    }
    let config_end = if partitions == 1 { 17 } else { 29 };
    let color_bits = below_weights as i32 - extra as i32 - config_end as i32 - if m.dual { 2 } else { 0 };
    let range = (4..=20u32).rev().find(|&r| ise_sequence_bits(values as i32, r) <= color_bits)?;
    let tables = dequant_tables();
    let mut symbols = [0u8; 18];
    decode_bise(range, &mut symbols[..values as usize], &bits, config_end);
    let ep = &tables.endpoints[(range - 4) as usize];
    let v = symbols.map(|s| ep[s as usize] as i32);

    // 4. Weights, stored from the top bit of the block down, planes
    //    interleaved.
    let mut symbols = [0u8; 64];
    decode_bise(m.range, &mut symbols[..count], &bits.reversed(), 0);
    let wq = &tables.weights[m.range as usize];
    let mut planes = [[0u8; 64]; 2];
    for (i, &s) in symbols[..count].iter().enumerate() {
        let (plane, at) = if m.dual { (i & 1, i >> 1) } else { (0, i) };
        planes[plane][at] = *wq.get(s as usize).unwrap_or(&0);
    }

    // 5. Each partition's endpoints.
    let mut endpoints = [[[0i32; 4]; 2]; 4];
    let mut hdr = [[false; 4]; 4];
    let mut at = 0;
    for p in 0..partitions as usize {
        let n = (2 + 2 * (cems[p] >> 2)) as usize;
        (endpoints[p], hdr[p]) = cem_endpoints(cems[p], &v[at..at + n]);
        at += n;
    }
    Some(Block::Weighted(Weighted { grid: m.grid, planes, ccs, partitions, seed, endpoints, hdr }))
}

/// A void-extent block: a constant colour over the block, with the
/// coordinates of a region where it holds, which decoders only check.  HDR
/// colours that are infinite or not a number make the block malformed.
fn void_extent(bits: &Bits128, mode: u32, three_d: bool) -> Option<Block> {
    let color = [64, 80, 96, 112].map(|at| bits.get_bits(at, 16) as u16);
    let hdr = mode & 0x200 != 0;
    if hdr && color.iter().any(|&c| is_half_inf_or_nan(c)) {
        return None;
    }
    let ok = if three_d {
        let c = [10, 19, 28, 37, 46, 55].map(|at| bits.get_bits(at, 9));
        c.iter().all(|&v| v == 0x1FF) || (c[0] < c[1] && c[2] < c[3] && c[4] < c[5])
    } else {
        let c = [12, 25, 38, 51].map(|at| bits.get_bits(at, 13));
        bits.get_bits(10, 2) == 3 && (c.iter().all(|&v| v == 0x1FFF) || (c[0] < c[1] && c[2] < c[3]))
    };
    ok.then_some(Block::Solid { hdr, color })
}

/// Reads a 2D block mode (the specification's "2D block mode layout").
fn mode_2d(m: u32) -> Option<Mode> {
    let (mut dual, mut h) = ((m >> 10) & 1, (m >> 9) & 1);
    let a = (m >> 5) & 3;
    let (q, grid);
    if m & 3 != 0 {
        q = ((m >> 4) & 1) | ((m & 3) << 1);
        let b = (m >> 7) & 3;
        grid = match (m >> 2) & 3 {
            0 => [b + 4, a + 2],
            1 => [b + 8, a + 2],
            2 => [a + 2, b + 8],
            _ if m & 0x100 != 0 => [(b & 1) + 2, a + 2],
            _ => [a + 2, (b & 1) + 6],
        };
    } else {
        if (m >> 2) & 3 == 0 {
            return None;
        }
        q = ((m >> 4) & 1) | (((m >> 2) & 3) << 1);
        let b = (m >> 9) & 3;
        grid = match (m >> 7) & 3 {
            0 => [12, a + 2],
            1 => [a + 2, 12],
            2 => {
                (dual, h) = (0, 0);
                [a + 6, b + 6]
            }
            _ => match a {
                0 => [6, 10],
                1 => [10, 6],
                _ => return None,
            },
        };
    }
    Some(Mode { grid: [grid[0], grid[1], 1], dual: dual == 1, range: q - 2 + 6 * h })
}

/// Reads a 3D block mode (the specification's "3D block mode layout").
fn mode_3d(m: u32) -> Option<Mode> {
    let (mut dual, mut h) = ((m >> 10) & 1, (m >> 9) & 1);
    let a = (m >> 5) & 3;
    let (q, grid);
    if m & 3 != 0 {
        q = ((m >> 4) & 1) | ((m & 3) << 1);
        grid = [a + 2, ((m >> 7) & 3) + 2, ((m >> 2) & 3) + 2];
    } else {
        if (m >> 2) & 3 == 0 {
            return None;
        }
        q = ((m >> 4) & 1) | (((m >> 2) & 3) << 1);
        let b = (m >> 9) & 3;
        if (m >> 7) & 3 != 3 {
            (dual, h) = (0, 0);
        }
        grid = match (m >> 7) & 3 {
            0 => [6, b + 2, a + 2],
            1 => [a + 2, 6, b + 2],
            2 => [a + 2, b + 2, 6],
            _ => match a {
                0 => [6, 2, 2],
                1 => [2, 6, 2],
                2 => [2, 2, 6],
                _ => return None,
            },
        };
    }
    Some(Mode { grid, dual: dual == 1, range: q - 2 + 6 * h })
}

//endregion

/*┌─────────────────╮
| Endpoints       |
└─────────────────╯*/
//region Endpoints

/// Decodes one partition's colour endpoint mode from its dequantised
/// values (0..255): the low and high RGBA endpoints, and which components
/// are HDR.  LDR results are 0..255; HDR ones are 12-bit logarithmic values
/// (0x780 is 1.0) for the 16-bit expansion that `decode_hdr` does.
fn cem_endpoints(cem: u32, v: &[i32]) -> ([[i32; 4]; 2], [bool; 4]) {
    let ldr = [false; 4];
    let (e, hdr) = match cem {
        0 => ([[v[0], v[0], v[0], 255], [v[1], v[1], v[1], 255]], ldr),
        1 => {
            let l0 = (v[0] >> 2) | (v[1] & 0xC0);
            let l1 = (l0 + (v[1] & 0x3F)).min(255);
            ([[l0, l0, l0, 255], [l1, l1, l1, 255]], ldr)
        }
        2 => {
            let (y0, y1) = if v[1] >= v[0] { (v[0] << 4, v[1] << 4) } else { ((v[1] << 4) + 8, (v[0] << 4) - 8) };
            ([[y0, y0, y0, 0x780], [y1, y1, y1, 0x780]], [true; 4])
        }
        3 => {
            let (y0, d) = if v[0] & 0x80 != 0 {
                (((v[1] & 0xE0) << 4) | ((v[0] & 0x7F) << 2), (v[1] & 0x1F) << 2)
            } else {
                (((v[1] & 0xF0) << 4) | ((v[0] & 0x7F) << 1), (v[1] & 0x0F) << 1)
            };
            let y1 = (y0 + d).min(0xFFF);
            ([[y0, y0, y0, 0x780], [y1, y1, y1, 0x780]], [true; 4])
        }
        4 => ([[v[0], v[0], v[0], v[2]], [v[1], v[1], v[1], v[3]]], ldr),
        5 => {
            let (b0, d0) = bit_transfer_signed(v[0], v[1]);
            let (b2, d2) = bit_transfer_signed(v[2], v[3]);
            ([[b0, b0, b0, b2], [b0 + d0, b0 + d0, b0 + d0, b2 + d2]], ldr)
        }
        6 => {
            let s = |c: i32| (c * v[3]) >> 8;
            ([[s(v[0]), s(v[1]), s(v[2]), 255], [v[0], v[1], v[2], 255]], ldr)
        }
        7 => hdr_rgb_scale(v),
        8 | 12 => {
            let a = if cem == 12 { [v[6], v[7]] } else { [255, 255] };
            let lo = [v[0], v[2], v[4], a[0]];
            let hi = [v[1], v[3], v[5], a[1]];
            if hi[0] + hi[1] + hi[2] >= lo[0] + lo[1] + lo[2] {
                ([lo, hi], ldr)
            } else {
                ([blue_contract(hi), blue_contract(lo)], ldr)
            }
        }
        9 | 13 => {
            let mut lo = [0; 4];
            let mut d = [0; 4];
            for c in 0..if cem == 13 { 4 } else { 3 } {
                (lo[c], d[c]) = bit_transfer_signed(v[2 * c], v[2 * c + 1]);
            }
            if cem == 9 {
                (lo[3], d[3]) = (255, 0);
            }
            let hi = [lo[0] + d[0], lo[1] + d[1], lo[2] + d[2], lo[3] + d[3]];
            if d[0] + d[1] + d[2] >= 0 { ([lo, hi], ldr) } else { ([blue_contract(hi), blue_contract(lo)], ldr) }
        }
        10 => {
            let s = |c: i32| (c * v[3]) >> 8;
            ([[s(v[0]), s(v[1]), s(v[2]), v[4]], [v[0], v[1], v[2], v[5]]], ldr)
        }
        11 => hdr_rgb(v, None),
        14 => {
            let (mut e, _) = hdr_rgb(v, None);
            (e[0][3], e[1][3]) = (v[6], v[7]);
            (e, [true, true, true, false])
        }
        _ => hdr_rgb(v, Some((v[6], v[7]))),
    };
    // Every mode keeps its results in range, but a clamp keeps the 16-bit
    // expansion safe whatever the values.
    let clamp = |e: [i32; 4]| {
        let mut o = e;
        for c in 0..4 {
            o[c] = e[c].clamp(0, if hdr[c] { 0xFFF } else { 255 });
        }
        o
    };
    ([clamp(e[0]), clamp(e[1])], hdr)
}

/// Splits a base and an offset that share bits: the offset's low bit is
/// the base's top bit, and the offset is a signed 6-bit value.  Returns the
/// base and the offset.
fn bit_transfer_signed(base: i32, offset: i32) -> (i32, i32) {
    let b = (base >> 1) | (offset & 0x80);
    let mut d = (offset >> 1) & 0x3F;
    if d & 0x20 != 0 {
        d -= 0x40;
    }
    (b, d)
}

/// Moves red and green halfway to blue, undoing the encoder's blue
/// contraction.
fn blue_contract(c: [i32; 4]) -> [i32; 4] {
    [(c[0] + c[2]) >> 1, (c[1] + c[2]) >> 1, c[2], c[3]]
}

/// HDR RGB, base and scale (mode 7): a high endpoint and a scale below it,
/// with the bits spread over the values by a submode.
fn hdr_rgb_scale(v: &[i32]) -> ([[i32; 4]; 2], [bool; 4]) {
    let modeval = ((v[0] & 0xC0) >> 6) | ((v[1] & 0x80) >> 5) | ((v[2] & 0x80) >> 4);
    let (major, mode) = if modeval & 0xC != 0xC {
        (modeval >> 2, modeval & 3)
    } else if modeval != 0xF {
        (modeval & 3, 4)
    } else {
        (0, 5)
    };
    let x = [
        (v[1] >> 6) & 1,
        (v[1] >> 5) & 1,
        (v[2] >> 6) & 1,
        (v[2] >> 5) & 1,
        (v[3] >> 7) & 1,
        (v[3] >> 6) & 1,
        (v[3] >> 5) & 1,
    ];
    // Red, green, blue, scale.
    let mut c = [v[0] & 0x3F, v[1] & 0x1F, v[2] & 0x1F, v[3] & 0x1F];
    // (submodes using it, which value, which bit of x, where it goes)
    const BITS: [(i32, usize, usize, i32); 17] = [
        (0x30, 1, 0, 6),
        (0x3A, 1, 1, 5),
        (0x30, 2, 2, 6),
        (0x3A, 2, 3, 5),
        (0x3D, 3, 6, 5),
        (0x2D, 3, 5, 6),
        (0x04, 3, 4, 7),
        (0x3B, 0, 4, 6),
        (0x04, 0, 3, 6),
        (0x10, 0, 5, 7),
        (0x0F, 0, 2, 7),
        (0x05, 0, 1, 8),
        (0x0A, 0, 0, 8),
        (0x05, 0, 0, 9),
        (0x02, 0, 6, 9),
        (0x01, 0, 3, 10),
        (0x02, 0, 5, 10),
    ];
    for (mask, to, bit, shift) in BITS {
        if (1 << mode) & mask != 0 {
            c[to] |= x[bit] << shift;
        }
    }
    let shift = [1, 1, 2, 3, 4, 5][mode as usize];
    let [mut r, mut g, mut b, s] = c.map(|v| v << shift);
    if mode != 5 {
        (g, b) = (r - g, r - b);
    }
    match major {
        1 => std::mem::swap(&mut r, &mut g),
        2 => std::mem::swap(&mut r, &mut b),
        _ => {}
    }
    let hi = [r, g, b, 0x780];
    let lo = [r - s, g - s, b - s, 0x780];
    ([lo, hi], [true; 4])
}

/// HDR RGB, direct (mode 11, and the RGB of modes 14 and 15): a high
/// endpoint and per-channel differences, with a submode spreading the
/// bits.  `alpha` holds mode 15's two HDR alpha values.
fn hdr_rgb(v: &[i32], alpha: Option<(i32, i32)>) -> ([[i32; 4]; 2], [bool; 4]) {
    let (lo_a, hi_a) = match alpha {
        Some(a) => hdr_alpha(a.0, a.1),
        None => (0x780, 0x780),
    };
    let major = ((v[4] & 0x80) >> 7) | ((v[5] & 0x80) >> 6);
    if major == 3 {
        let lo = [v[0] << 4, v[2] << 4, (v[4] & 0x7F) << 5, lo_a];
        let hi = [v[1] << 4, v[3] << 4, (v[5] & 0x7F) << 5, hi_a];
        return ([lo, hi], [true; 4]);
    }
    let mode = (((v[1] & 0x80) >> 7) | ((v[2] & 0x80) >> 6) | ((v[3] & 0x80) >> 5)) as usize;
    let d_bits = [7, 6, 7, 6, 5, 6, 5, 6][mode];
    let sign_extend = |x: i32| {
        let shift = 32 - d_bits;
        (x << shift) >> shift
    };
    let x = [(v[2] >> 6) & 1, (v[3] >> 6) & 1, (v[4] >> 6) & 1, (v[5] >> 6) & 1, (v[4] >> 5) & 1, (v[5] >> 5) & 1];
    // a, b0, b1, c.
    let mut c = [v[0] | ((v[1] & 0x40) << 2), v[2] & 0x3F, v[3] & 0x3F, v[1] & 0x3F];
    // (submodes using it, which value, which bit of x, where it goes)
    const BITS: [(i32, usize, usize, i32); 13] = [
        (0xA4, 0, 0, 9),
        (0x08, 0, 2, 9),
        (0x50, 0, 4, 9),
        (0x50, 0, 5, 10),
        (0xA0, 0, 1, 10),
        (0xC0, 0, 2, 11),
        (0x04, 3, 1, 6),
        (0xE8, 3, 3, 6),
        (0x20, 3, 2, 7),
        (0x5B, 1, 0, 6),
        (0x5B, 2, 1, 6),
        (0x12, 1, 2, 7),
        (0x12, 2, 3, 7),
    ];
    for (mask, to, bit, shift) in BITS {
        if (1 << mode) & mask != 0 {
            c[to] |= x[bit] << shift;
        }
    }
    let shift = (mode as i32 >> 1) ^ 3;
    let [a, b0, b1, cc] = c.map(|v| v << shift);
    let (d0, d1) = (sign_extend(v[4] & 0x7F) << shift, sign_extend(v[5] & 0x7F) << shift);
    let mut hi = [a, a - b0, a - b1, hi_a];
    let mut lo = [a - cc, a - b0 - cc - d0, a - b1 - cc - d1, lo_a];
    match major {
        1 => {
            hi.swap(0, 1);
            lo.swap(0, 1);
        }
        2 => {
            hi.swap(0, 2);
            lo.swap(0, 2);
        }
        _ => {}
    }
    ([lo, hi], [true; 4])
}

/// Mode 15's HDR alpha: a base and a difference sharing bits by a submode.
fn hdr_alpha(v6: i32, v7: i32) -> (i32, i32) {
    let mode = ((v6 >> 7) & 1) | ((v7 >> 6) & 2);
    let (mut a, mut b) = (v6 & 0x7F, v7 & 0x7F);
    if mode == 3 {
        return (a << 5, b << 5);
    }
    a |= (b << (mode + 1)) & 0x780;
    b &= 0x3F >> mode;
    b ^= 0x20 >> mode;
    b -= 0x20 >> mode;
    a <<= 4 - mode;
    b <<= 4 - mode;
    (a, (a + b).clamp(0, 0xFFF))
}

//endregion

#[cfg(test)]
#[path = "astc3d_tests.rs"]
mod tests;
