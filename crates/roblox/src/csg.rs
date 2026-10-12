//! The geometry Roblox keeps inside a file for unions and mesh parts: a
//! union's triangles (`MeshData`, "CSGMDL") and the convex hulls of a union
//! or mesh part's collision shape (`PhysicalConfigData`/`PhysicsData`,
//! "CSGPHS").  Layouts follow krakow10's rbx_mesh (MIT OR Apache-2.0),
//! which reverse-engineered them; every count is checked against the bytes
//! present before anything is allocated.

use glam::Vec3;

/// Triangles in the part's own space.
#[derive(Clone, Debug, Default)]
pub(crate) struct Mesh {
    pub positions: Vec<Vec3>,
    pub indices: Vec<u32>,
    /// sRGB colours per vertex, when stored: a union's triangles keep the
    /// colours of the parts they came from.
    pub colors: Vec<[u8; 4]>,
    /// The convex pieces of collision hulls, as ranges of `indices`.
    pub pieces: Vec<[usize; 2]>,
}

impl Mesh {
    /// A coarser copy of a hull mesh for thumbnails, where a mesh part's
    /// detail is smaller than a pixel: each convex piece's corners are
    /// merged on a 3 by 3 by 3 grid over the piece (Rossignac and Borrel's
    /// vertex clustering), which keeps its outline in at most a few dozen
    /// triangles.  Meshes without pieces come back unchanged.
    pub fn coarse(&self) -> Mesh {
        if self.pieces.is_empty() {
            return self.clone();
        }
        let mut out = Mesh::default();
        for &[from, to] in &self.pieces {
            let Some(tris) = self.indices.get(from..to) else { continue };
            let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
            for &i in tris {
                let p = self.positions[i as usize];
                lo = lo.min(p);
                hi = hi.max(p);
            }
            let span = (hi - lo).max(Vec3::splat(1e-6));
            let cell = |p: Vec3| {
                let c = ((p - lo) / span * 3.0).floor().clamp(Vec3::ZERO, Vec3::splat(2.0));
                (c.x + c.y * 3.0 + c.z * 9.0) as usize
            };
            let mut sums = [(Vec3::ZERO, 0u32); 27];
            for &i in tris {
                let p = self.positions[i as usize];
                let s = &mut sums[cell(p)];
                s.0 += p;
                s.1 += 1;
            }
            let base = out.positions.len() as u32;
            let mut slot = [u32::MAX; 27];
            for (k, (sum, n)) in sums.iter().enumerate() {
                if *n > 0 {
                    slot[k] = out.positions.len() as u32;
                    out.positions.push(*sum / *n as f32);
                }
            }
            let first = out.indices.len();
            let mut seen = std::collections::HashSet::new();
            for t in tris.chunks_exact(3) {
                let c = [t[0], t[1], t[2]].map(|i| cell(self.positions[i as usize]));
                if c[0] == c[1] || c[1] == c[2] || c[0] == c[2] {
                    continue;
                }
                let mut key = c;
                key.sort_unstable();
                if seen.insert(key) {
                    out.indices.extend(c.map(|k| slot[k]));
                }
            }
            face_out(&mut out, base as usize, first);
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.indices.len() < 3
    }

    /// Drops triangles that point past the vertices or aren't finite.
    fn checked(mut self) -> Option<Mesh> {
        let n = self.positions.len() as u32;
        if !self.positions.iter().all(|p| p.is_finite()) {
            return None;
        }
        if self.colors.len() != self.positions.len() {
            self.colors.clear();
        }
        self.indices.truncate(self.indices.len() / 3 * 3);
        if self.indices.iter().any(|&i| i >= n) {
            self.pieces.clear();
            let kept: Vec<u32> =
                self.indices.chunks_exact(3).filter(|t| t.iter().all(|&i| i < n)).flatten().copied().collect();
            self.indices = kept;
        }
        (!self.is_empty()).then_some(self)
    }
}

/// What a physics blob holds.
#[derive(Debug)]
pub(crate) enum Physics {
    /// The part collides as its box.
    Block,
    /// Convex hulls, which together outline the shape.
    Hulls(Mesh),
}

/// Why a blob gave no geometry, for the inspector's counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Missing {
    /// A key into Roblox's servers (`CSGK`), with nothing in the file.
    OnServers,
    /// A layout Slerp doesn't read.
    Unknown,
}

/*┌─────────────────╮
| Union triangles |
└─────────────────╯*/
//region Union triangles

/// The bytes CSGMDL files are XORed with, by position.
const NOISE: [u8; 31] = [
    86, 46, 110, 88, 49, 32, 48, 4, 52, 105, 12, 119, 12, 1, 94, 0, 26, 96, 55, 105, 29, 82, 43, 7, 79, 36, 89, 101,
    83, 4, 122,
];

fn unmask(data: &[u8]) -> Vec<u8> {
    data.iter().enumerate().map(|(i, b)| b ^ NOISE[i % NOISE.len()]).collect()
}

/// A union's `MeshData`: CSGMDL version 2 or 4 (wholly masked) or 5 (only
/// its signature masked).
pub(crate) fn union_mesh(data: &[u8]) -> Result<Mesh, Missing> {
    if data.starts_with(b"CSGK") {
        return Err(Missing::OnServers);
    }
    let head = unmask(data.get(..10).ok_or(Missing::Unknown)?);
    if &head[..6] != b"CSGMDL" {
        return Err(Missing::Unknown);
    }
    let version = u32::from_le_bytes([head[6], head[7], head[8], head[9]]);
    match version {
        2 | 4 => mesh2(&unmask(data)[10..]).ok_or(Missing::Unknown),
        5 => mesh5(&data[10..]).ok_or(Missing::Unknown),
        _ => Err(Missing::Unknown),
    }
}

/// Versions 2 and 4: a 32-byte hash, then 84-byte vertices and triangles.
fn mesh2(d: &[u8]) -> Option<Mesh> {
    let mut r = Reader { d, at: 32 };
    let count = r.u32()? as usize;
    if r.u32()? != 84 || count > r.left() / 84 {
        return None;
    }
    let mut positions = Vec::with_capacity(count);
    let mut colors = Vec::with_capacity(count);
    for _ in 0..count {
        let v = r.take(84)?;
        positions.push(vec3(&v[0..12]));
        colors.push([v[24], v[25], v[26], v[27]]);
    }
    let indices = r.u32()? as usize;
    if indices > r.left() / 4 {
        return None;
    }
    let indices = (0..indices).map(|_| r.u32()).collect::<Option<Vec<u32>>>()?;
    Mesh { positions, indices, colors, pieces: Vec::new() }.checked()
}

/// Version 5: counted arrays of positions, normals, colours, normal ids,
/// texture coordinates and tangents, then delta-coded indices with range
/// markers, the first range being the full-detail mesh.
fn mesh5(d: &[u8]) -> Option<Mesh> {
    let mut r = Reader { d, at: 0 };
    let count = r.u16()? as usize;
    let positions: Vec<Vec3> = (0..count).map(|_| r.take(12).map(vec3)).collect::<Option<_>>()?;
    let normals = r.u16()? as usize;
    r.u32()?;
    r.take(normals * 6)?;
    let colors = r.u16()? as usize;
    let colors: Vec<[u8; 4]> = r.take(colors * 4)?.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
    let colors = if colors.len() == positions.len() { colors } else { Vec::new() };
    let ids = r.u16()? as usize;
    r.take(ids)?;
    let uvs = r.u16()? as usize;
    r.take(uvs * 8)?;
    let tangents = r.u16()? as usize;
    r.u32()?;
    r.take(tangents * 6)?;
    // The indices: each a step from the last, in one byte (-64 to 63) or
    // three (a 23-bit step).
    let count = r.u32()? as usize;
    let len = r.u32()? as usize;
    let code = r.take(len)?;
    if count > code.len() {
        return None;
    }
    let mut indices = Vec::with_capacity(count);
    let (mut at, mut index) = (0usize, 0u32);
    for _ in 0..count {
        let v0 = *code.get(at)?;
        at += 1;
        if v0 & 0x80 == 0 {
            if v0 & 0x40 == 0 {
                index = index.wrapping_add(v0 as u32);
            } else {
                index = index.wrapping_sub(-((v0 | 0x80) as i8) as u32);
            }
        } else {
            let (v1, v2) = (*code.get(at)?, *code.get(at + 1)?);
            at += 2;
            index = index.wrapping_add(u32::from_le_bytes([v2, v1, v0 & 0x7f, 0]));
        }
        indices.push(index & 0x7f_ffff);
    }
    let markers = r.u8()? as usize;
    let markers: Vec<u32> = (0..markers).map(|_| r.u32()).collect::<Option<_>>()?;
    // The first range is the full mesh; the rest are coarser levels.
    if let [start, end, ..] = markers[..] {
        let (start, end) = (start as usize, end as usize);
        if start <= end && end <= indices.len() {
            indices = indices[start..end].to_vec();
        }
    }
    Mesh { positions, indices, colors, pieces: Vec::new() }.checked()
}

//endregion

/*┌─────────────────╮
| Collision hulls |
└─────────────────╯*/
//region Collision hulls

/// How many triangles and convex pieces a physics blob holds, read from
/// its counts without decoding it (version 8 inflates only its first
/// bytes), so a place can choose how much hull detail it can afford.
pub(crate) fn physics_size(data: &[u8]) -> (usize, usize) {
    if !data.starts_with(b"CSGPHS") || data.len() < 10 {
        return (0, 0);
    }
    let version = u32::from_le_bytes([data[6], data[7], data[8], data[9]]);
    let mut r = Reader { d: data, at: 10 };
    let skip = match version {
        3 | 5 => 0,
        6 => 40,
        7 => 41,
        8 => {
            let mut head = [0u8; 12];
            let Some(Ok(mut decoder)) = data.get(12..).map(zstd::stream::read::Decoder::new) else { return (0, 0) };
            if std::io::Read::read_exact(&mut decoder, &mut head).is_err() {
                return (0, 0);
            }
            let n = |k: usize| u32::from_le_bytes([head[k], head[k + 1], head[k + 2], head[k + 3]]) as usize;
            return (n(8).min(4_000_000), n(0).min(1_000_000));
        }
        _ => return (0, 0),
    };
    let (mut tris, mut pieces) = (0usize, 0usize);
    if r.take(skip).is_none() {
        return (0, 0);
    }
    while r.left() > 0 {
        let Some(_) = r.take(40) else { break };
        let Some(floats) = r.u32() else { break };
        if r.u32().is_none() || r.take((floats as usize).saturating_mul(4)).is_none() {
            break;
        }
        let Some(indices) = r.u32() else { break };
        if r.take((indices as usize).saturating_mul(4)).is_none() {
            break;
        }
        tris += indices as usize / 3;
        pieces += 1;
    }
    (tris, pieces)
}

/// How much of the collision hulls a place draws: all of them, coarse
/// copies, each convex piece as its box, or only each part's box.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Detail {
    Full,
    Coarse,
    PieceBoxes,
    PartBoxes,
}

impl Mesh {
    /// Each convex piece as its box (twelve triangles), in part space.
    pub fn piece_boxes(&self) -> Mesh {
        let mut out = Mesh::default();
        for &[from, to] in &self.pieces {
            let Some(tris) = self.indices.get(from..to) else { continue };
            let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
            for &i in tris {
                let p = self.positions[i as usize];
                lo = lo.min(p);
                hi = hi.max(p);
            }
            if !lo.is_finite() {
                continue;
            }
            let base = out.positions.len() as u32;
            for k in 0..8 {
                out.positions.push(Vec3::new(
                    if k & 1 == 0 { lo.x } else { hi.x },
                    if k & 2 == 0 { lo.y } else { hi.y },
                    if k & 4 == 0 { lo.z } else { hi.z },
                ));
            }
            let first = out.indices.len();
            for q in [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]] {
                out.indices.extend([q[0], q[1], q[2], q[0], q[2], q[3]].map(|v| base + v));
            }
            face_out(&mut out, base as usize, first);
        }
        out
    }
}

/// A physics blob: `CSGPHS` versions 3 and 5 to 8, or the plain block.
/// Version 8 is Zstandard-compressed and may hold no more than `max_bytes`
/// once inflated.
pub(crate) fn physics(data: &[u8], max_bytes: usize) -> Result<Physics, Missing> {
    if data.starts_with(b"CSGK") {
        return Err(Missing::OnServers);
    }
    if !data.starts_with(b"CSGPHS") || data.len() < 10 {
        return Err(Missing::Unknown);
    }
    let version = u32::from_le_bytes([data[6], data[7], data[8], data[9]]);
    if version == 0 && data[10..].starts_with(b"BLOCK") {
        return Ok(Physics::Block);
    }
    if version == 8 {
        return v8::read(data, max_bytes).map(Physics::Hulls).ok_or(Missing::Unknown);
    }
    let mut r = Reader { d: data, at: 10 };
    let single = match version {
        3 | 5 => false,
        6 => {
            r.take(40).ok_or(Missing::Unknown)?;
            false
        }
        7 => {
            // A geometry kind (0 to 3 or 6), then the physical properties.
            if !matches!(r.u8(), Some(0..=3 | 6)) {
                return Err(Missing::Unknown);
            }
            r.take(40).ok_or(Missing::Unknown)?;
            false
        }
        _ => return Err(Missing::Unknown),
    };
    let mut out = Mesh::default();
    while r.left() > 0 {
        hull(&mut r, &mut out).ok_or(Missing::Unknown)?;
        if single {
            break;
        }
    }
    out.checked().map(Physics::Hulls).ok_or(Missing::Unknown)
}

/// One hull: a fixed 40-byte prefix, the position floats and the indices.
fn hull(r: &mut Reader, out: &mut Mesh) -> Option<()> {
    let prefix = r.take(40)?;
    if prefix[0] != 16 || prefix[20] != 16 {
        return None;
    }
    let floats = r.u32()? as usize;
    if r.u32()? != 4 || !floats.is_multiple_of(3) || floats > r.left() / 4 {
        return None;
    }
    let base = out.positions.len() as u32;
    for _ in 0..floats / 3 {
        out.positions.push(vec3(r.take(12)?));
    }
    let indices = r.u32()? as usize;
    if indices > r.left() / 4 {
        return None;
    }
    let n = (floats / 3) as u32;
    let first = out.indices.len();
    for _ in 0..indices {
        let i = r.u32()?;
        // An index past this hull's vertices is damage; clamp it to the
        // hull rather than letting it reach another hull's.
        out.indices.push(base + i.min(n.saturating_sub(1)));
    }
    out.indices.truncate(first + (out.indices.len() - first) / 3 * 3);
    face_out(out, base as usize, first);
    Some(())
}

/// Turns each triangle of a convex hull (vertices from `base`, triangles
/// from index `first`) to face away from the hull's centre: the formats
/// don't promise a winding, and back faces aren't drawn.
fn face_out(m: &mut Mesh, base: usize, first: usize) {
    m.pieces.push([first, m.indices.len()]);
    let verts = &m.positions[base.min(m.positions.len())..];
    if verts.is_empty() {
        return;
    }
    let centre = verts.iter().copied().sum::<Vec3>() / verts.len() as f32;
    for t in m.indices[first..].chunks_exact_mut(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| m.positions.get(i as usize).copied().unwrap_or_default());
        if (b - a).cross(c - a).dot(a - centre) < 0.0 {
            t.swap(1, 2);
        }
    }
}

//endregion

/*┌─────────────────╮
| Version 8       |
└─────────────────╯*/
//region Version 8

/// `CSGPHS` version 8: after a geometry kind and a zero byte, a Zstandard
/// stream holding counts, a box, hulls stored plainly, and hulls stored
/// as Edgebreaker connectivity (a "CLERS" symbol per triangle) with their
/// positions.  The Edgebreaker decoder follows rbx_mesh's, made iterative
/// (its recursion and loops are unbounded on damaged input) with every
/// index checked and a step budget.
mod v8 {
    use super::{Mesh, Reader, face_out, vec3};
    use glam::Vec3;
    use std::io::Read;

    pub(super) fn read(data: &[u8], max_bytes: usize) -> Option<Mesh> {
        // 1. The kind, a zero, then the compressed body.
        if !matches!(data.get(10), Some(0..=3 | 6)) || data.get(11) != Some(&0) {
            return None;
        }
        let mut body = Vec::new();
        let decoder = zstd::stream::read::Decoder::new(data.get(12..)?).ok()?;
        decoder.take(max_bytes as u64 + 1).read_to_end(&mut body).ok()?;
        if body.len() > max_bytes {
            return None;
        }
        // 2. Counts and the box.
        let mut r = Reader { d: &body, at: 0 };
        let hulls = r.u32()? as usize;
        let position_count = r.u32()? as usize;
        let face_count = r.u32()? as usize;
        r.take(8)?;
        let raw_len = r.u32()?;
        let clers_bits = r.u32()?;
        let clers_len = r.u32()? as usize;
        r.take(4 + 24)?;
        let mut out = Mesh::default();
        // 3. Plain hulls: their triangle and position ranges, then the
        //    triangles (indices within each hull) and positions.
        if raw_len != 0 {
            let ranges = |r: &mut Reader| -> Option<Vec<u32>> {
                let n = r.u32()? as usize;
                if n > r.left() / 4 {
                    return None;
                }
                (0..n).map(|_| r.u32()).collect()
            };
            let face_ranges = ranges(&mut r)?;
            let faces = face_ranges.last().copied().unwrap_or(0) as usize;
            if faces > r.left() / 4 {
                return None;
            }
            let indices: Vec<u32> = (0..faces).map(|_| r.u32()).collect::<Option<_>>()?;
            let pos_ranges = ranges(&mut r)?;
            let floats = pos_ranges.last().copied().unwrap_or(0) as usize;
            if floats > r.left() / 4 {
                return None;
            }
            let positions: Vec<f32> = (0..floats).map(|_| r.u32().map(f32::from_bits)).collect::<Option<_>>()?;
            add_hulls(&mut out, &face_ranges, &indices, &pos_ranges, &positions)?;
        }
        // 4. Edgebreaker hulls.
        let clers = r.take(clers_len)?;
        if position_count > r.left() / 12 || hulls > face_count.max(1) {
            return None;
        }
        // A closed hull has about twice as many triangles as corners.
        if face_count > 2 * position_count + 4 * hulls || face_count > 4_000_000 {
            return None;
        }
        let positions: Vec<f32> =
            (0..position_count * 3).map(|_| r.u32().map(f32::from_bits)).collect::<Option<_>>()?;
        let mut decoder = Decoder::new(Bits::new(clers, clers_bits)?, face_count);
        let (mut face_ranges, mut pos_ranges) = (vec![0u32], vec![0u32]);
        for _ in 0..hulls {
            decoder.hull()?;
            face_ranges.push(decoder.face * 3);
            pos_ranges.push(decoder.vertices * 3);
        }
        let indices = decoder.indices;
        add_hulls(&mut out, &face_ranges, &indices, &pos_ranges, &positions)?;
        (!out.is_empty()).then_some(out)
    }

    /// Adds hulls whose triangles (`indices`, by hull-local vertex) and
    /// positions (three floats each) are split by the two range lists.
    fn add_hulls(
        out: &mut Mesh,
        face_ranges: &[u32],
        indices: &[u32],
        pos_ranges: &[u32],
        positions: &[f32],
    ) -> Option<()> {
        for (f, p) in face_ranges.windows(2).zip(pos_ranges.windows(2)) {
            let (f0, f1, p0, p1) = (f[0] as usize, f[1] as usize, p[0] as usize, p[1] as usize);
            let tris = indices.get(f0..f1)?;
            let floats = positions.get(p0..p1)?;
            let base = out.positions.len();
            let n = (floats.len() / 3) as u32;
            if n == 0 {
                continue;
            }
            for c in floats.chunks_exact(3) {
                let bytes: Vec<u8> = c.iter().flat_map(|f| f.to_le_bytes()).collect();
                out.positions.push(vec3(&bytes));
            }
            let first = out.indices.len();
            for t in tris.chunks_exact(3) {
                out.indices.extend(t.iter().map(|&i| base as u32 + i.min(n - 1)));
            }
            face_out(out, base, first);
        }
        let _ = Vec3::ZERO;
        Some(())
    }

    /// Reads bits as Roblox writes them: little-endian 32-bit words, each
    /// taken from its top bit down, the last word holding only what is
    /// left of the bit count.
    pub(super) struct Bits<'a> {
        words: std::slice::ChunksExact<'a, u8>,
        cache: u32,
        cached: u32,
        left: u32,
    }

    impl<'a> Bits<'a> {
        pub(super) fn new(bytes: &'a [u8], bits: u32) -> Option<Bits<'a>> {
            if (bytes.len() as u64) * 8 < bits as u64 || !bytes.len().is_multiple_of(4) {
                return None;
            }
            Some(Bits { words: bytes.chunks_exact(4), cache: 0, cached: 0, left: bits })
        }

        pub(super) fn read(&mut self, bits: u32) -> Option<u32> {
            let (mut value, mut have) = (0u32, 0u32);
            if self.cached < bits {
                let draw = self.left.min(32);
                self.left -= draw;
                let word = self.words.next().map_or(0, |w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]));
                (value, have) = (self.cache, self.cached);
                (self.cache, self.cached) = (word, draw);
            }
            let need = bits - have;
            if self.cached < need {
                return None;
            }
            let shift = self.cached - need;
            let popped = self.cache.unbounded_shr(shift);
            self.cache &= 1u32.unbounded_shl(shift).wrapping_sub(1);
            self.cached -= need;
            Some(value.unbounded_shl(need) | popped)
        }
    }

    /// The five Edgebreaker symbols: a new vertex (C), a split (S), the
    /// left or right neighbour (L, R), the end of a branch (E).
    #[derive(Debug, PartialEq)]
    enum Symbol {
        C,
        S,
        L,
        R,
        E,
    }

    /// Edge states besides a twin edge's number.
    const UNINIT: i64 = -3;
    const BOUNDARY: i64 = -1;
    const PROCESSING: i64 = -2;

    fn next(e: u32) -> u32 {
        e / 3 * 3 + (e + 1) % 3
    }

    fn prev(e: u32) -> u32 {
        e / 3 * 3 + (e + 2) % 3
    }

    pub(super) struct Decoder<'a> {
        bits: Bits<'a>,
        /// Per edge (three per triangle): its twin, or a state.
        adjacency: Vec<i64>,
        /// Per edge: the vertex at that corner, numbered within its hull.
        pub(super) indices: Vec<u32>,
        pub(super) face: u32,
        /// Vertices of the hulls decoded so far.
        pub(super) vertices: u32,
        /// Steps left before the decoder gives up on a damaged stream.
        steps: u64,
    }

    impl<'a> Decoder<'a> {
        pub(super) fn new(bits: Bits<'a>, faces: usize) -> Decoder<'a> {
            Decoder {
                bits,
                adjacency: vec![UNINIT; faces * 3],
                indices: vec![0; faces * 3],
                face: 0,
                vertices: 0,
                steps: 64 * (faces as u64 * 3 + 64),
            }
        }

        fn symbol(&mut self) -> Option<Symbol> {
            if self.bits.read(1)? == 0 {
                return Some(Symbol::C);
            }
            Some(match self.bits.read(2)? {
                0 => Symbol::S,
                1 => Symbol::L,
                2 => Symbol::R,
                _ => Symbol::E,
            })
        }

        fn step(&mut self) -> Option<()> {
            self.steps = self.steps.checked_sub(1)?;
            Some(())
        }

        fn adj(&self, e: u32) -> Option<i64> {
            self.adjacency.get(e as usize).copied()
        }

        fn set_adj(&mut self, e: u32, v: i64) -> Option<()> {
            *self.adjacency.get_mut(e as usize)? = v;
            Some(())
        }

        fn index(&self, e: u32) -> Option<u32> {
            self.indices.get(e as usize).copied()
        }

        fn set_index(&mut self, e: u32, v: u32) -> Option<()> {
            *self.indices.get_mut(e as usize)? = v;
            Some(())
        }

        /// A twin edge's number when `v` is one.
        fn twin(v: i64) -> Option<u32> {
            (v >= 0).then_some(v as u32)
        }

        /// Starts a hull with a first triangle and decodes the rest.
        pub(super) fn hull(&mut self) -> Option<()> {
            let edge = self.face.checked_mul(3)?;
            self.set_adj(edge, BOUNDARY)?;
            self.set_adj(edge + 1, UNINIT)?;
            self.set_adj(edge + 2, BOUNDARY)?;
            for k in 0..3 {
                self.set_index(edge + k, k)?;
            }
            self.face += 1;
            let mut count = 3u32;
            self.decode(&mut count, edge + 1)?;
            self.vertices = self.vertices.checked_add(count)?;
            Some(())
        }

        /// The symbols of one hull: each adds a triangle on the gate edge;
        /// a split decodes one branch now and comes back for the other, an
        /// end finishes a branch.
        fn decode(&mut self, count: &mut u32, start: u32) -> Option<()> {
            let mut pending: Vec<u32> = Vec::new();
            let mut cursor = start;
            loop {
                self.step()?;
                let (e0, e1, e2) = (self.face.checked_mul(3)?, self.face * 3 + 1, self.face * 3 + 2);
                self.set_adj(e0, cursor as i64)?;
                self.set_adj(e1, UNINIT)?;
                self.set_adj(e2, UNINIT)?;
                self.face += 1;
                self.set_adj(cursor, e0 as i64)?;
                let (a, b) = (self.index(prev(cursor))?, self.index(next(cursor))?);
                self.set_index(e1, a)?;
                self.set_index(e2, b)?;
                cursor = e1;
                match self.symbol()? {
                    Symbol::C => {
                        self.set_index(e0, *count)?;
                        self.set_adj(next(cursor), BOUNDARY)?;
                        *count = count.checked_add(1)?;
                    }
                    Symbol::S => pending.push(next(cursor)),
                    Symbol::L => {
                        self.set_adj(cursor, PROCESSING)?;
                        cursor = next(cursor);
                    }
                    Symbol::R => {
                        let n = next(cursor);
                        self.set_adj(n, PROCESSING)?;
                        self.zip(n)?;
                    }
                    Symbol::E => {
                        self.set_adj(cursor, PROCESSING)?;
                        let n = next(cursor);
                        self.set_adj(n, PROCESSING)?;
                        self.zip(n)?;
                        match pending.pop() {
                            Some(c) => cursor = c,
                            None => return Some(()),
                        }
                    }
                }
            }
        }

        /// Joins boundary edges left open around `current` as twins,
        /// merging the vertices they share.
        fn zip(&mut self, mut current: u32) -> Option<u32> {
            while self.adj(current)? == PROCESSING {
                self.step()?;
                let mut candidate = next(current);
                while let Some(opposite) = Self::twin(self.adj(candidate)?) {
                    self.step()?;
                    candidate = next(opposite);
                }
                if self.adj(candidate)? != BOUNDARY {
                    break;
                }
                self.set_adj(current, candidate as i64)?;
                self.set_adj(candidate, current as i64)?;
                current = prev(current);
                let mut previous = current;
                let donor = self.index(prev(candidate))?;
                self.set_index(prev(current), donor)?;
                let mut connected = self.adj(current)?;
                while let Some(id) = Self::twin(connected)
                    && candidate != previous
                {
                    self.step()?;
                    previous = prev(id);
                    self.set_index(prev(previous), donor)?;
                    connected = self.adj(previous)?;
                }
                while let Some(linked) = Self::twin(self.adj(current)?)
                    && current != candidate
                {
                    self.step()?;
                    current = prev(linked);
                }
            }
            Some(current)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// rbx_mesh's test words: symbols read in the order Roblox writes
        /// them, the last word cut short.
        #[test]
        fn symbols_in_roblox_bit_order() {
            const BYTES: [u8; 8] =
                [0b0101_1110, 0b0011_0010, 0b0101_1110, 0b0011_0010, 0b0101_1110, 0b0011_0010, 0b0101_1110, 0b11_0010];
            let mut d = Decoder::new(Bits::new(&BYTES, 62).unwrap(), 4);
            let round = [Symbol::C, Symbol::C, Symbol::R, Symbol::C, Symbol::S, Symbol::L, Symbol::E, Symbol::C];
            for s in round.iter().chain(&round) {
                assert_eq!(d.symbol().as_ref(), Some(s));
            }
            for s in &round[2..] {
                assert_eq!(d.symbol().as_ref(), Some(s));
            }
            for s in &round {
                assert_eq!(d.symbol().as_ref(), Some(s));
            }
            assert_eq!(d.symbol(), None);
        }

        #[test]
        fn rbx_mesh_empirical_words() {
            let mut bits = Bits::new(&[183, 219, 55, 0], 22).unwrap();
            assert_eq!(bits.read(1), Some(1));
            let mut d = Decoder::new(bits, 4);
            for s in [Symbol::L, Symbol::E, Symbol::L, Symbol::L, Symbol::R, Symbol::R, Symbol::E] {
                assert_eq!(d.symbol(), Some(s));
            }
            assert_eq!(d.symbol(), None);
        }

        /// Random symbol streams never index out of bounds or loop.
        #[test]
        fn noise_ends() {
            let mut seed = 0x1234_5678_9abc_def1u64;
            for _ in 0..2000 {
                let words: Vec<u8> = (0..64)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        seed as u8
                    })
                    .collect();
                let mut d = Decoder::new(Bits::new(&words, 512).unwrap(), 40);
                for _ in 0..6 {
                    if d.hull().is_none() {
                        break;
                    }
                }
            }
        }
    }
}

//endregion

fn vec3(b: &[u8]) -> Vec3 {
    let f = |i: usize| f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    Vec3::new(f(0), f(4), f(8))
}

struct Reader<'a> {
    d: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn left(&self) -> usize {
        self.d.len().saturating_sub(self.at)
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.d.len())?;
        let s = &self.d[self.at..end];
        self.at = end;
        Some(s)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        let b = self.take(2)?;
        Some(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masked_signatures() {
        // The masks rbx_mesh documents for versions 2, 4 and 5.
        assert_eq!(unmask(b"\x15\x7d\x29\x15\x75\x6c\x32\x04\x34\x69"), b"CSGMDL\x02\0\0\0");
        assert_eq!(unmask(b"\x15\x7d\x29\x15\x75\x6c\x35\x04\x34\x69"), b"CSGMDL\x05\0\0\0");
    }

    #[test]
    fn damaged_blobs_give_nothing() {
        assert_eq!(union_mesh(b"CSGK0123").unwrap_err(), Missing::OnServers);
        assert_eq!(union_mesh(b"").unwrap_err(), Missing::Unknown);
        let mut v5 = unmask(b"CSGMDL\x05\0\0\0")[..10].to_vec();
        v5.extend_from_slice(&[0xff, 0xff]);
        assert!(union_mesh(&v5).is_err());
        assert!(matches!(physics(b"CSGPHS\0\0\0\0BLOCK", 1 << 20), Ok(Physics::Block)));
        assert!(physics(b"CSGPHS\x03\0\0\0\x10", 1 << 20).is_err());
        let mut huge = b"CSGPHS\x03\0\0\0".to_vec();
        huge.extend_from_slice(&[16, 0, 0, 0]);
        huge.extend_from_slice(&[0; 16]);
        huge.extend_from_slice(&[16, 0, 0, 0]);
        huge.extend_from_slice(&[0; 16]);
        huge.extend_from_slice(&0xffff_fff0u32.to_le_bytes());
        huge.extend_from_slice(&4u32.to_le_bytes());
        assert!(physics(&huge, 1 << 20).is_err());
    }
}
