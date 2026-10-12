//! Roblox's smooth terrain: the voxel grid in `Terrain.SmoothGrid`, and a
//! surface drawn through it.
//!
//! The grid's layout was reverse-engineered by others: zeux's "Voxel
//! terrain: storage" (2017) describes the run records, and chteau's
//! rbx-native (MIT) the whole blob, which this reader follows:
//!
//! - Byte 0 is the version (1), byte 1 the log2 of a chunk's edge (5, so
//!   32 voxels of 4 studs).  An empty grid is just those two bytes.
//! - Chunks follow until the end.  Each starts with its position in chunks
//!   relative to the previous chunk's, three 32-bit integers with their
//!   bytes interleaved by significance (the top bytes of X, Y and Z first).
//! - Then run records fill the chunk's 32³ voxels, X fastest, then Z, then
//!   Y.  A record's lead byte holds the material (0 Air, 1 Water, then the
//!   21 solid materials) in its low six bits; bit 6 says an occupancy byte
//!   follows (otherwise Air is empty and anything else full); bit 7 says a
//!   count byte follows, the run being count + 1 voxels.  A count of 0 is a
//!   one-voxel run with one more byte, the water sharing that voxel.
//!
//! Roblox's own mesher isn't public.  The surface here is Surface Nets: it
//! passes where a voxel's fill crosses one half between voxel centres, and
//! each surface vertex sits at the average crossing in its cell, which
//! rounds terrain the way Roblox's does.  Large terrains are drawn from a
//! coarser grid, each coarse voxel the average of a block of real ones.

use crate::Budget;
use crate::palette;
use glam::Vec3;
use slerp_scene::LoadError;
use std::collections::HashMap;

const CHUNK: i32 = 32;
const CELLS: usize = (CHUNK * CHUNK * CHUNK) as usize;
/// Studs along a voxel's edge.
pub(crate) const VOXEL: f32 = 4.0;
/// Roblox's terrain reaches ±32,000 studs; chunks beyond are damage.
const FARTHEST: i32 = 32_000 / (CHUNK * 4) + 1;

/// One voxel: a material slot and how full of it the voxel is (the stored
/// byte; a solid voxel is `(occupancy + 1) / 256` full, Air always 0).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Cell {
    pub slot: u8,
    pub occupancy: u8,
}

impl Cell {
    const AIR: Cell = Cell { slot: 0, occupancy: 0 };

    /// How full of solid the voxel is, 0 to 1.
    fn solid(self) -> f32 {
        if self.slot >= 2 { (self.occupancy as f32 + 1.0) / 256.0 } else { 0.0 }
    }

    fn water(self) -> bool {
        self.slot == 1
    }
}

/// A chunk of voxels; most chunks of a terrain are all rock or all air.
#[derive(Clone)]
enum Chunk {
    Uniform(Cell),
    Mixed(Box<[Cell]>),
}

impl Chunk {
    fn get(&self, i: usize) -> Cell {
        match self {
            Chunk::Uniform(c) => *c,
            Chunk::Mixed(cells) => cells[i],
        }
    }
}

/// A decoded grid: chunks by position in chunks, each voxel `scale` real
/// voxels across.
pub(crate) struct Grid {
    chunks: HashMap<[i32; 3], Chunk>,
    scale: i32,
}

/// What the terrain holds, for the inspector.
#[derive(Default, Debug)]
pub(crate) struct Facts {
    pub chunks: usize,
    /// Solid voxels by material slot (counting a partly full voxel by its
    /// fill), from the full-resolution grid.
    pub materials: [f64; 23],
    pub water: u64,
}

/// Decodes a `SmoothGrid` blob.  An empty grid gives `None`.
pub(crate) fn decode(bytes: &[u8], budget: &mut Budget, facts: &mut Facts) -> Result<Option<Grid>, LoadError> {
    let bad = |m: &str| LoadError::invalid(format!("terrain: {m}"));
    let (&version, &log2) =
        (bytes.first().ok_or_else(|| bad("empty"))?, bytes.get(1).ok_or_else(|| bad("no chunk size"))?);
    if version != 1 {
        return Err(LoadError::Unsupported(format!("terrain version {version}")));
    }
    if log2 != 5 {
        return Err(LoadError::Unsupported(format!("terrain chunks of 2^{log2} voxels")));
    }
    let mut at = 2usize;
    let mut key = [0i32; 3];
    let mut chunks = HashMap::new();
    while at < bytes.len() {
        // 1. The chunk's position, relative to the last.
        let delta = bytes.get(at..at + 12).ok_or_else(|| bad("a chunk position is cut short"))?;
        at += 12;
        let mut d = [0u32; 3];
        for round in 0..4 {
            for (axis, v) in d.iter_mut().enumerate() {
                *v = (*v << 8) | delta[round * 3 + axis] as u32;
            }
        }
        for axis in 0..3 {
            key[axis] = key[axis].wrapping_add(d[axis] as i32);
        }
        if key.iter().any(|v| v.abs() > FARTHEST) {
            return Err(bad("a chunk lies beyond the terrain's extent"));
        }
        // 2. Its runs.
        let mut cells: Vec<Cell> = Vec::new();
        let mut first: Option<Cell> = None;
        let mut uniform = true;
        let mut filled = 0usize;
        while filled < CELLS {
            let lead = *bytes.get(at).ok_or_else(|| bad("a chunk is cut short"))?;
            at += 1;
            let slot = lead & 0x3f;
            if slot >= 23 {
                return Err(bad("an unknown material"));
            }
            let mut occupancy = if slot == 0 { 0 } else { 255 };
            if lead & 0x40 != 0 {
                occupancy = *bytes.get(at).ok_or_else(|| bad("a chunk is cut short"))?;
                at += 1;
            }
            let mut run = 1usize;
            if lead & 0x80 != 0 {
                let count = *bytes.get(at).ok_or_else(|| bad("a chunk is cut short"))?;
                at += 1;
                if count == 0 {
                    // The water sharing a partly full voxel.
                    bytes.get(at).ok_or_else(|| bad("a chunk is cut short"))?;
                    at += 1;
                } else {
                    run = count as usize + 1;
                }
            }
            if filled + run > CELLS {
                return Err(bad("a run overflows its chunk"));
            }
            let cell = if slot == 0 { Cell::AIR } else { Cell { slot, occupancy } };
            match first {
                None => first = Some(cell),
                Some(f) if f != cell && uniform => {
                    uniform = false;
                    budget.take_terrain((CELLS * 2) as u64)?;
                    cells.reserve_exact(CELLS);
                    cells.resize(filled, f);
                }
                _ => {}
            }
            if !uniform {
                cells.resize(filled + run, cell);
            }
            if cell.slot >= 2 {
                facts.materials[cell.slot as usize] += run as f64 * cell.solid() as f64;
            } else if cell.water() {
                facts.water += run as u64;
            }
            filled += run;
        }
        let chunk =
            if uniform { Chunk::Uniform(first.unwrap_or(Cell::AIR)) } else { Chunk::Mixed(cells.into_boxed_slice()) };
        if chunks.insert(key, chunk).is_some() {
            return Err(bad("a chunk is stored twice"));
        }
    }
    facts.chunks = chunks.len();
    Ok((!chunks.is_empty()).then_some(Grid { chunks, scale: 1 }))
}

impl Grid {
    /// Chunks holding a surface: mixed ones, and uniform ones beside a
    /// chunk (or nothing) of another kind.
    fn mixed_count(&self) -> usize {
        self.chunks.values().filter(|c| matches!(c, Chunk::Mixed(_))).count()
    }

    /// A grid `factor` times coarser (a power of two up to 32): each voxel
    /// the average fill of a block of these, made of the fullest one's
    /// material, water where most of the block is water.  Each fine chunk
    /// falls inside one coarse chunk, so the work is one pass over the
    /// stored voxels.
    fn coarser(&self, factor: i32) -> Grid {
        #[derive(Clone, Copy, Default)]
        struct Acc {
            fill: f32,
            water: u32,
            best: Cell,
        }
        let side = CHUNK / factor;
        let mut accs: HashMap<[i32; 3], Vec<Acc>> = HashMap::new();
        for (key, chunk) in &self.chunks {
            let coarse = key.map(|c| c.div_euclid(factor));
            let corner = key.map(|c| c.rem_euclid(factor) * side);
            let acc = accs.entry(coarse).or_insert_with(|| vec![Acc::default(); CELLS]);
            let mut add = |x: i32, y: i32, z: i32, c: Cell, n: u32| {
                let i = ((corner[0] + x) + (corner[2] + z) * CHUNK + (corner[1] + y) * CHUNK * CHUNK) as usize;
                let a = &mut acc[i];
                a.fill += c.solid() * n as f32;
                a.water += c.water() as u32 * n;
                if c.solid() > a.best.solid() {
                    a.best = c;
                }
            };
            match chunk {
                Chunk::Uniform(c) => {
                    for y in 0..side {
                        for z in 0..side {
                            for x in 0..side {
                                add(x, y, z, *c, (factor * factor * factor) as u32);
                            }
                        }
                    }
                }
                Chunk::Mixed(cells) => {
                    for y in 0..CHUNK {
                        for z in 0..CHUNK {
                            for x in 0..CHUNK {
                                let c = cells[(x + z * CHUNK + y * CHUNK * CHUNK) as usize];
                                add(x / factor, y / factor, z / factor, c, 1);
                            }
                        }
                    }
                }
            }
        }
        let block = (factor * factor * factor) as f32;
        let chunks = accs
            .into_iter()
            .map(|(key, acc)| {
                let cells: Vec<Cell> = acc
                    .iter()
                    .map(|a| {
                        let fill = a.fill / block;
                        if fill > 0.0 && a.best.slot >= 2 {
                            Cell { slot: a.best.slot, occupancy: ((fill * 256.0) as i32 - 1).clamp(0, 255) as u8 }
                        } else if a.water as f32 > block * 0.5 {
                            Cell { slot: 1, occupancy: 255 }
                        } else {
                            Cell::AIR
                        }
                    })
                    .collect();
                let first = cells[0];
                let chunk = if cells.iter().all(|&c| c == first) {
                    Chunk::Uniform(first)
                } else {
                    Chunk::Mixed(cells.into_boxed_slice())
                };
                (key, chunk)
            })
            .collect();
        Grid { chunks, scale: self.scale * factor }
    }
}

/// Triangles in studs with a material slot per vertex.
#[derive(Default)]
pub(crate) struct Surface {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub slots: Vec<u8>,
    pub indices: Vec<u32>,
}

/// The solid surface and the water's, from a grid made coarse enough that
/// the solid surface keeps to about `triangles` triangles.
pub(crate) fn mesh(grid: Grid, triangles: u64) -> (Surface, Surface, i32) {
    // A mixed chunk has a surface about one chunk face across, two
    // triangles per voxel; each halving of resolution quarters that.
    let estimate = grid.mixed_count().max(1) as u64 * (CHUNK * CHUNK * 2) as u64;
    let mut factor = 1;
    while estimate / (factor * factor) as u64 > triangles && factor < 16 {
        factor *= 2;
    }
    let grid = if factor > 1 { grid.coarser(factor) } else { grid };
    let solid = surface(&grid, |c| c.solid(), |_, _| true);
    // Water is drawn only where it meets air: its faces against solid
    // terrain are hidden anyway and would flicker against it.
    let water = surface(&grid, |c| if c.water() { 1.0 } else { 0.0 }, |a, b| a.slot == 0 || b.slot == 0);
    (solid, water, grid.scale)
}

/// Surface Nets over `density`, keeping the faces of edges `keep` accepts
/// (given the two voxels the edge joins).
fn surface(grid: &Grid, density: impl Fn(Cell) -> f32, keep: impl Fn(Cell, Cell) -> bool) -> Surface {
    let mut out = Surface::default();
    let size = VOXEL * grid.scale as f32;
    let mut keys: Vec<[i32; 3]> = grid.chunks.keys().copied().collect();
    keys.sort_unstable();
    // Chunks next to stored ones may hold the surface's far side.
    let mut all = keys.clone();
    for k in &keys {
        for d in [[-1, 0, 0], [0, -1, 0], [0, 0, -1]] {
            all.push([k[0] + d[0], k[1] + d[1], k[2] + d[2]]);
        }
    }
    all.sort_unstable();
    all.dedup();
    const S: i32 = CHUNK + 2;
    let mut samples = vec![Cell::AIR; (S * S * S) as usize];
    let mut values = vec![0f32; (S * S * S) as usize];
    // The vertex of each cell in the chunk and the layer before it.
    let mut vertex = vec![u32::MAX; (S * S * S) as usize];
    let at = |x: i32, y: i32, z: i32| ((x + 1) + (y + 1) * S + (z + 1) * S * S) as usize;
    for key in all {
        let origin = key.map(|c| c * CHUNK);
        // 1. Densities from one voxel before the chunk to one past it,
        //    read through the 27 chunks around it.  Chunks that are all
        //    one side of the surface, with neighbours alike, are skipped.
        let near: [Option<&Chunk>; 27] = std::array::from_fn(|n| {
            let d = [(n % 3) as i32 - 1, ((n / 3) % 3) as i32 - 1, (n / 9) as i32 - 1];
            grid.chunks.get(&[key[0] + d[0], key[1] + d[1], key[2] + d[2]])
        });
        let side = |c: Option<&Chunk>| match c {
            None => Some(density(Cell::AIR) >= 0.5),
            Some(Chunk::Uniform(cell)) => Some(density(*cell) >= 0.5),
            Some(Chunk::Mixed(_)) => None,
        };
        let first = side(near[13]);
        if first.is_some() && near.iter().all(|&c| side(c) == first) {
            continue;
        }
        let (mut inside, mut outside) = (false, false);
        for z in -1..=CHUNK {
            for y in -1..=CHUNK {
                for x in -1..=CHUNK {
                    let (cx, lx) = split(x);
                    let (cy, ly) = split(y);
                    let (cz, lz) = split(z);
                    let c = near[(cx + cy * 3 + cz * 9) as usize]
                        .map_or(Cell::AIR, |ch| ch.get((lx + lz * CHUNK + ly * CHUNK * CHUNK) as usize));
                    let d = density(c);
                    let i = at(x, y, z);
                    samples[i] = c;
                    values[i] = d;
                    if d >= 0.5 {
                        inside = true;
                    } else {
                        outside = true;
                    }
                }
            }
        }
        if !(inside && outside) {
            continue;
        }
        // 2. A vertex in every cell the surface crosses, for cells whose
        //    first corner is from -1 to 30 (the cells around this chunk's
        //    edges).
        vertex.fill(u32::MAX);
        for z in -1..CHUNK {
            for y in -1..CHUNK {
                for x in -1..CHUNK {
                    let mut corners = [0f32; 8];
                    let mut mask = 0u8;
                    for (k, corner) in corners.iter_mut().enumerate() {
                        let (dx, dy, dz) = ((k & 1) as i32, ((k >> 1) & 1) as i32, ((k >> 2) & 1) as i32);
                        *corner = values[at(x + dx, y + dy, z + dz)];
                        if *corner >= 0.5 {
                            mask |= 1 << k;
                        }
                    }
                    if mask == 0 || mask == 255 {
                        continue;
                    }
                    // The average crossing of the cell's twelve edges.
                    let (mut sum, mut n) = (Vec3::ZERO, 0.0);
                    for (a, b) in EDGES {
                        let (da, db) = (corners[a], corners[b]);
                        if (da >= 0.5) != (db >= 0.5) {
                            let t = ((0.5 - da) / (db - da)).clamp(0.0, 1.0);
                            sum += corner_pos(a).lerp(corner_pos(b), t);
                            n += 1.0;
                        }
                    }
                    let local = sum / n;
                    // Fill falls outward: the normal is minus its gradient.
                    let g = Vec3::new(
                        (corners[1] + corners[3] + corners[5] + corners[7])
                            - (corners[0] + corners[2] + corners[4] + corners[6]),
                        (corners[2] + corners[3] + corners[6] + corners[7])
                            - (corners[0] + corners[1] + corners[4] + corners[5]),
                        (corners[4] + corners[5] + corners[6] + corners[7])
                            - (corners[0] + corners[1] + corners[2] + corners[3]),
                    );
                    // The material: the fullest corner's.
                    let mut best = samples[at(x, y, z)];
                    for k in 0..8 {
                        let c = samples[at(x + (k & 1), y + ((k >> 1) & 1), z + ((k >> 2) & 1))];
                        if density(c) > density(best) || best.slot < 2 && c.slot >= 2 {
                            best = c;
                        }
                    }
                    let p = (Vec3::new((origin[0] + x) as f32, (origin[1] + y) as f32, (origin[2] + z) as f32)
                        + 0.5
                        + local)
                        * size;
                    vertex[at(x, y, z)] = out.positions.len() as u32;
                    out.positions.push(p);
                    out.normals.push((-g).normalize_or(Vec3::Y));
                    out.slots.push(best.slot);
                }
            }
        }
        // 3. A quad across every voxel edge the surface crosses, joining
        //    the four cells around the edge.
        for z in 0..CHUNK {
            for y in 0..CHUNK {
                for x in 0..CHUNK {
                    let a = at(x, y, z);
                    for axis in 0..3 {
                        let step = [(1, 0, 0), (0, 1, 0), (0, 0, 1)][axis];
                        let b = at(x + step.0, y + step.1, z + step.2);
                        let (ia, ib) = (values[a] >= 0.5, values[b] >= 0.5);
                        if ia == ib || !keep(samples[a], samples[b]) {
                            continue;
                        }
                        // The cells sharing the edge, around it in order.
                        let (u, v) = match axis {
                            0 => ((0, 1, 0), (0, 0, 1)),
                            1 => ((0, 0, 1), (1, 0, 0)),
                            _ => ((1, 0, 0), (0, 1, 0)),
                        };
                        let cell = |du: i32, dv: i32| {
                            vertex[at(x - u.0 * du - v.0 * dv, y - u.1 * du - v.1 * dv, z - u.2 * du - v.2 * dv)]
                        };
                        let q = [cell(1, 1), cell(0, 1), cell(0, 0), cell(1, 0)];
                        if q.contains(&u32::MAX) {
                            continue;
                        }
                        // Facing out of the full side.
                        let q = if ia { q } else { [q[3], q[2], q[1], q[0]] };
                        out.indices.extend_from_slice(&[q[0], q[1], q[2], q[0], q[2], q[3]]);
                    }
                }
            }
        }
    }
    out
}

/// Which of three chunks (before, this, after) a coordinate from -1 to 32
/// falls in, and where in it.
fn split(v: i32) -> (i32, i32) {
    if v < 0 {
        (0, v + CHUNK)
    } else if v >= CHUNK {
        (2, v - CHUNK)
    } else {
        (1, v)
    }
}

/// A cell's corners, numbered by their x, y and z bits.
fn corner_pos(k: usize) -> Vec3 {
    Vec3::new((k & 1) as f32, ((k >> 1) & 1) as f32, ((k >> 2) & 1) as f32)
}

const EDGES: [(usize, usize); 12] =
    [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)];

/// The colour of a material slot, from the place's colours.
pub(crate) fn color(colors: &[[u8; 3]; 23], slot: u8) -> [u8; 3] {
    colors.get(slot as usize).copied().unwrap_or(palette::TERRAIN_MATERIALS[8].1)
}

/// The chunk positions of a blob, for tests.
#[cfg(test)]
pub(crate) fn chunk_keys(bytes: &[u8]) -> Vec<[i32; 3]> {
    let mut budget = Budget::new(1 << 20, &slerp_scene::Limits::viewer());
    decode(bytes, &mut budget, &mut Facts::default())
        .ok()
        .flatten()
        .map(|g| g.chunks.keys().copied().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Chooses a test chunk's voxel at x, y and z.
    type Fill = dyn Fn(i32, i32, i32) -> Cell;

    /// A grid of chunks at their keys whose voxels the functions choose,
    /// encoded as Roblox does.
    pub(crate) fn encode(chunks: &[([i32; 3], &Fill)]) -> Vec<u8> {
        let mut out = vec![1u8, 5];
        let mut last = [0i32; 3];
        for (key, fill) in chunks {
            let d: [u32; 3] = std::array::from_fn(|a| key[a].wrapping_sub(last[a]) as u32);
            last = *key;
            for shift in [24, 16, 8, 0] {
                out.extend(d.iter().map(|v| (v >> shift) as u8));
            }
            let mut cells = Vec::with_capacity(CELLS);
            for y in 0..CHUNK {
                for z in 0..CHUNK {
                    for x in 0..CHUNK {
                        cells.push(fill(x, y, z));
                    }
                }
            }
            let mut i = 0;
            while i < cells.len() {
                let c = cells[i];
                let mut run = 1;
                while i + run < cells.len() && run < 256 && cells[i + run] == c {
                    run += 1;
                }
                let full = if c.slot == 0 { 0 } else { 255 };
                let mut lead = c.slot;
                if c.occupancy != full {
                    lead |= 0x40;
                }
                if run > 1 {
                    lead |= 0x80;
                }
                out.push(lead);
                if c.occupancy != full {
                    out.push(c.occupancy);
                }
                if run > 1 {
                    out.push((run - 1) as u8);
                }
                i += run;
            }
        }
        out
    }

    fn budget() -> Budget {
        Budget::new(1 << 20, &slerp_scene::Limits::viewer())
    }

    #[test]
    fn a_slab_meshes_to_a_flat_top() {
        // Grass up to voxel 10, half a voxel more at 10.
        let fill = |_: i32, y: i32, _: i32| match y {
            ..10 => Cell { slot: 2, occupancy: 255 },
            10 => Cell { slot: 2, occupancy: 127 },
            _ => Cell::AIR,
        };
        let bytes = encode(&[([0, 0, 0], &fill)]);
        let mut facts = Facts::default();
        let grid = decode(&bytes, &mut budget(), &mut facts).unwrap().unwrap();
        assert_eq!(facts.chunks, 1);
        assert!(facts.materials[2] > 32.0 * 32.0 * 10.0);
        let (solid, water, scale) = mesh(grid, 1_000_000);
        assert_eq!(scale, 1);
        assert!(water.indices.is_empty());
        assert!(!solid.indices.is_empty());
        // The top sits where fill crosses one half: half-way into voxel 10.
        let top = solid
            .positions
            .iter()
            .filter(|p| p.x > 20.0 && p.x < 100.0 && p.z > 20.0 && p.z < 100.0)
            .map(|p| p.y)
            .fold(f32::MIN, f32::max);
        assert!((top - 10.5 * VOXEL).abs() < 0.6, "top at {top}");
        assert!(solid.slots.iter().all(|&s| s == 2));
        // Upward faces wind anticlockwise seen from above.
        let up =
            solid.indices.chunks_exact(3).find(|t| t.iter().all(|&i| solid.positions[i as usize].y > 40.0)).unwrap();
        let [a, b, c] = [up[0], up[1], up[2]].map(|i| solid.positions[i as usize]);
        assert!((b - a).cross(c - a).y > 0.0);
    }

    #[test]
    fn coarse_grids_keep_the_shape() {
        let fill = |_: i32, y: i32, _: i32| if y < 16 { Cell { slot: 8, occupancy: 255 } } else { Cell::AIR };
        let bytes = encode(&[([0, 0, 0], &fill), ([1, 0, 0], &fill)]);
        let grid = decode(&bytes, &mut budget(), &mut Facts::default()).unwrap().unwrap();
        let (solid, _, scale) = mesh(grid, 100);
        assert!(scale > 1);
        let top = solid.positions.iter().map(|p| p.y).fold(f32::MIN, f32::max);
        assert!((top - 16.0 * VOXEL).abs() <= VOXEL * scale as f32, "top at {top}");
    }

    #[test]
    fn damaged_grids_are_refused() {
        let mut facts = Facts::default();
        assert!(decode(&[], &mut budget(), &mut facts).is_err());
        assert!(decode(&[2, 5], &mut budget(), &mut facts).is_err());
        assert!(decode(&[1, 5], &mut budget(), &mut facts).unwrap().is_none());
        let fill = |_: i32, _: i32, _: i32| Cell { slot: 3, occupancy: 255 };
        let good = encode(&[([0, 0, 0], &fill)]);
        for cut in [3, 14, good.len() - 1] {
            assert!(decode(&good[..cut], &mut budget(), &mut facts).is_err(), "cut at {cut}");
        }
        let far = encode(&[([1000, 0, 0], &fill)]);
        assert!(decode(&far, &mut budget(), &mut facts).is_err());
        let twice = encode(&[([0, 0, 0], &fill), ([0, 0, 0], &fill)]);
        assert!(decode(&twice, &mut budget(), &mut facts).is_err());
        // A run past the chunk's end.
        let mut over = vec![1u8, 5];
        over.extend_from_slice(&[0; 12]);
        over.extend(std::iter::repeat_n([0x83u8, 255], 129).flatten());
        assert!(decode(&over, &mut budget(), &mut facts).is_err());
    }
}
