//! Turns a build's blocks into a scene: a grid of 32-block chunks holding
//! global block ids, meshed chunk by chunk in parallel.
//!
//! Full cubes are greedy-meshed: for each direction and slice, the faces
//! that show (their neighbour is air, a partial shape, or see-through
//! unlike them) are merged into the largest rectangles of one appearance,
//! so a flat wall of one block is two triangles per chunk side.  Other
//! shapes (slabs, stairs, fences, panes, plants, doors) are a few boxes or
//! crossed quads per block, with the faces against solid neighbours left
//! out.  Every face's texture coordinates follow the block grid, so a
//! merged rectangle repeats its pattern or texture once per block.
//!
//! Builds larger than the cell budget (millions of blocks in a thumbnail)
//! are thinned: each grid cell stands for k x k x k blocks and keeps the
//! most visible of them, and the scene is scaled back up by k.

use crate::blocks::{BlockState, Connect, Dir, Layer, Look, Shape};
use crate::colors::{self, Pattern};
use crate::layouts::Build;
use crate::patterns;
use crate::textures::Pack;
use rayon::prelude::*;
use slerp_scene::glam::{Vec2, Vec3};
use slerp_scene::{
    AlphaMode, Filter, Info, Limits, LoadError, Material, Mesh, Node, Primitive, Sampler, Scene, TexRef, Transform,
    Wrap,
};
use std::collections::HashMap;
use std::sync::Arc;

const CHUNK: i32 = 32;
const CHUNK_CELLS: usize = (CHUNK * CHUNK * CHUNK) as usize;
/// The padded copy of a chunk the mesher reads: one cell more on each side.
const PAD: usize = CHUNK as usize + 2;

/// Grid cells kept for a thumbnail and for the viewer; bigger builds are
/// thinned to fit.
const THUMBNAIL_CELLS: u64 = 8 << 20;
const VIEWER_CELLS: u64 = 256 << 20;

/*┌─────────────────╮
| Grid            |
└─────────────────╯*/
//region Grid

/// Block ids by position, in chunks; id 0 is air.
struct Grid {
    chunks: HashMap<[i32; 3], Box<[u16]>>,
    /// Each cell's weight (`Look::weight`) when thinned, to keep the most
    /// visible block of those sharing a cell.
    thinned: bool,
}

impl Grid {
    fn index(x: i32, y: i32, z: i32) -> usize {
        ((y & 31) << 10 | (z & 31) << 5 | (x & 31)) as usize
    }

    fn get(&self, x: i32, y: i32, z: i32) -> u16 {
        self.chunks.get(&[x >> 5, y >> 5, z >> 5]).map_or(0, |c| c[Grid::index(x, y, z)])
    }
}

/// Writes cells into the grid, remembering the last chunk, since cells
/// arrive in rows.
struct Writer<'g> {
    grid: &'g mut Grid,
    key: [i32; 3],
    chunk: Option<Box<[u16]>>,
}

impl<'g> Writer<'g> {
    fn new(grid: &'g mut Grid) -> Self {
        Writer { grid, key: [i32::MIN; 3], chunk: None }
    }

    fn set(&mut self, x: i32, y: i32, z: i32, id: u16, looks: &[Look]) {
        let key = [x >> 5, y >> 5, z >> 5];
        if key != self.key {
            self.flush();
            self.chunk =
                Some(self.grid.chunks.remove(&key).unwrap_or_else(|| vec![0u16; CHUNK_CELLS].into_boxed_slice()));
            self.key = key;
        }
        let Some(chunk) = self.chunk.as_mut() else { return };
        let cell = &mut chunk[Grid::index(x, y, z)];
        if !self.grid.thinned || looks[id as usize].weight() >= looks[*cell as usize].weight() {
            *cell = id;
        }
    }

    fn flush(&mut self) {
        if let Some(c) = self.chunk.take() {
            self.grid.chunks.insert(self.key, c);
        }
    }
}

impl Drop for Writer<'_> {
    fn drop(&mut self) {
        self.flush();
    }
}

//endregion

/*┌─────────────────╮
| Appearances     |
└─────────────────╯*/
//region Appearances

/// What a face looks like: one material.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Appearance {
    rgb: [u8; 3],
    image: Image,
    layer: Layer,
    /// Opacity in hundredths.
    alpha: u8,
    /// Glow in tenths.
    glow: u8,
    double_sided: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Image {
    Pattern(Pattern),
    /// A texture from the user's game or resource pack, by model name.
    Pack(String),
}

/// The appearances of every block id's faces, deduplicated.
struct Appearances {
    list: Vec<Appearance>,
    index: HashMap<Appearance, u32>,
    /// Per block id: the top, side and bottom faces' appearance, plus 1
    /// (0 for none).
    faces: Vec<[u32; 3]>,
    /// Per block id, for flowers: the stem's appearance, plus 1.
    stems: Vec<u32>,
}

impl Appearances {
    fn add(&mut self, a: Appearance) -> u32 {
        if let Some(&i) = self.index.get(&a) {
            return i + 1;
        }
        self.list.push(a.clone());
        self.index.insert(a, self.list.len() as u32 - 1);
        self.list.len() as u32
    }

    fn new(states: &[BlockState], looks: &[Look], pack: Option<&Pack>) -> Appearances {
        let mut out = Appearances { list: Vec::new(), index: HashMap::new(), faces: Vec::new(), stems: Vec::new() };
        for (state, look) in states.iter().zip(looks) {
            if look.shape == Shape::Air {
                out.faces.push([0; 3]);
                out.stems.push(0);
                continue;
            }
            let double_sided = matches!(look.shape, Shape::Cross) || look.layer != Layer::Opaque;
            let base = |rgb: [u8; 3], image: Image| Appearance {
                rgb,
                image,
                layer: look.layer,
                alpha: (look.alpha * 100.0).round() as u8,
                glow: (look.glow * 10.0).round() as u8,
                double_sided,
            };
            let packed = pack
                .and_then(|p| p.faces(state))
                .filter(|f| pack.is_some_and(|p| p.texture(&f.side.0).is_some() && p.texture(&f.top.0).is_some()));
            let textured = packed.is_some();
            let faces = match packed {
                Some(f) => [f.top, f.side, f.bottom].map(|(tex, tint)| {
                    let mut a = base(tint.unwrap_or([255; 3]), Image::Pack(tex));
                    // Textures carry their own holes: leaves and plants cut
                    // out, plain glass too.
                    if a.layer == Layer::Translucent && state.name == "glass" || state.name.ends_with("_leaves") {
                        a.layer = Layer::Cutout;
                        a.alpha = 100;
                        a.double_sided = true;
                    }
                    out.add(a)
                }),
                None => {
                    let c = look.colors;
                    [c.top, c.side, c.bottom].map(|(rgb, p)| out.add(base(rgb, Image::Pattern(p))))
                }
            };
            out.faces.push(faces);
            // A flower's stem is green below its blossom.
            let stem = if look.shape == Shape::Cross && colors::is_flower(&state.name) && !textured {
                out.add(base([70, 120, 40], Image::Pattern(Pattern::Plant)))
            } else {
                0
            };
            out.stems.push(stem);
        }
        out
    }
}

//endregion

/*┌─────────────────╮
| Meshing         |
└─────────────────╯*/
//region Meshing

/// Geometry of one appearance.
#[derive(Default)]
struct Part {
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    uvs: Vec<Vec2>,
    indices: Vec<u32>,
}

impl Part {
    /// A quad given its corners counter-clockwise seen from outside.
    fn quad(&mut self, corners: [Vec3; 4], normal: Vec3, dir: Option<Dir>) {
        let base = self.positions.len() as u32;
        for c in corners {
            self.positions.push(c);
            self.normals.push(normal);
            self.uvs.push(match dir {
                Some(d) => uv(d, c),
                None => Vec2::ZERO,
            });
        }
        self.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// Texture coordinates from a position on a face, so textures run along
/// the block grid with their tops up and are seen unmirrored from outside.
fn uv(d: Dir, p: Vec3) -> Vec2 {
    match d {
        Dir::East => Vec2::new(-p.z, -p.y),
        Dir::West => Vec2::new(p.z, -p.y),
        Dir::South => Vec2::new(p.x, -p.y),
        Dir::North => Vec2::new(-p.x, -p.y),
        Dir::Up => Vec2::new(p.x, p.z),
        Dir::Down => Vec2::new(p.x, -p.z),
    }
}

fn normal(d: Dir) -> Vec3 {
    match d {
        Dir::East => Vec3::X,
        Dir::West => Vec3::NEG_X,
        Dir::Up => Vec3::Y,
        Dir::Down => Vec3::NEG_Y,
        Dir::South => Vec3::Z,
        Dir::North => Vec3::NEG_Z,
    }
}

/// The face rectangle of direction `d` on plane `plane` (along d's axis)
/// from (u0, v0) to (u1, v1) on the other two axes, taken in cyclic order
/// after d's axis, counter-clockwise seen from outside.
fn face(d: Dir, plane: f32, u0: f32, v0: f32, u1: f32, v1: f32) -> [Vec3; 4] {
    let (a, positive) = d.axis();
    let (u, v) = ((a + 1) % 3, (a + 2) % 3);
    let corner = |cu: f32, cv: f32| {
        let mut p = Vec3::ZERO;
        p[a] = plane;
        p[u] = cu;
        p[v] = cv;
        p
    };
    let c = [corner(u0, v0), corner(u1, v0), corner(u1, v1), corner(u0, v1)];
    if positive { c } else { [c[0], c[3], c[2], c[1]] }
}

/// Which face colours a direction takes: 0 top, 1 side, 2 bottom.
fn face_kind(d: Dir) -> usize {
    match d {
        Dir::Up => 0,
        Dir::Down => 2,
        _ => 1,
    }
}

/// A chunk with a border of its neighbours' cells, for reading neighbours
/// without looking chunks up.
struct Padded {
    cells: Vec<u16>,
}

impl Padded {
    fn new(grid: &Grid, key: [i32; 3]) -> Padded {
        let mut cells = vec![0u16; PAD * PAD * PAD];
        let o = [key[0] * CHUNK, key[1] * CHUNK, key[2] * CHUNK];
        let own = &grid.chunks[&key];
        for y in -1..=CHUNK {
            for z in -1..=CHUNK {
                for x in -1..=CHUNK {
                    let inside = (0..CHUNK).contains(&x) && (0..CHUNK).contains(&y) && (0..CHUNK).contains(&z);
                    let v = if inside { own[Grid::index(x, y, z)] } else { grid.get(o[0] + x, o[1] + y, o[2] + z) };
                    cells[Padded::at(x, y, z)] = v;
                }
            }
        }
        Padded { cells }
    }

    fn at(x: i32, y: i32, z: i32) -> usize {
        ((y + 1) as usize * PAD + (z + 1) as usize) * PAD + (x + 1) as usize
    }

    fn get(&self, x: i32, y: i32, z: i32) -> u16 {
        self.cells[Padded::at(x, y, z)]
    }
}

/// The geometry of one chunk, by appearance.
fn mesh_chunk(grid: &Grid, key: [i32; 3], looks: &[Look], apps: &Appearances) -> HashMap<u32, Part> {
    let pad = Padded::new(grid, key);
    let origin = Vec3::new((key[0] * CHUNK) as f32, (key[1] * CHUNK) as f32, (key[2] * CHUNK) as f32);
    let mut parts: HashMap<u32, Part> = HashMap::new();
    let n = CHUNK as usize;

    // 1. Full cubes and liquids, greedy-meshed slice by slice.
    let mut mask = vec![0u32; n * n];
    for d in Dir::ALL {
        let (a, positive) = d.axis();
        let (ua, va) = ((a + 1) % 3, (a + 2) % 3);
        let step = if positive { 1 } else { -1 };
        let kind = face_kind(d);
        for slice in 0..CHUNK {
            for j in 0..CHUNK {
                for i in 0..CHUNK {
                    let mut p = [0i32; 3];
                    p[a] = slice;
                    p[ua] = i;
                    p[va] = j;
                    let id = pad.get(p[0], p[1], p[2]) as usize;
                    let mut m = 0;
                    if id != 0 && matches!(looks[id].shape, Shape::Cube | Shape::Liquid) {
                        p[a] += step;
                        let next = pad.get(p[0], p[1], p[2]) as usize;
                        if next == 0 || !looks[id].hidden_by(&looks[next]) {
                            m = apps.faces[id][kind];
                        }
                    }
                    mask[i as usize + j as usize * n] = m;
                }
            }
            let plane = origin[a] + slice as f32 + if positive { 1.0 } else { 0.0 };
            for j in 0..n {
                let mut i = 0;
                while i < n {
                    let m = mask[i + j * n];
                    if m == 0 {
                        i += 1;
                        continue;
                    }
                    let mut w = 1;
                    while i + w < n && mask[i + w + j * n] == m {
                        w += 1;
                    }
                    let mut h = 1;
                    'grow: while j + h < n {
                        for k in 0..w {
                            if mask[i + k + (j + h) * n] != m {
                                break 'grow;
                            }
                        }
                        h += 1;
                    }
                    for jj in 0..h {
                        mask[i + (j + jj) * n..i + w + (j + jj) * n].fill(0);
                    }
                    let (u0, v0) = (origin[ua] + i as f32, origin[va] + j as f32);
                    let corners = face(d, plane, u0, v0, u0 + w as f32, v0 + h as f32);
                    parts.entry(m - 1).or_default().quad(corners, normal(d), Some(d));
                    i += w;
                }
            }
        }
    }

    // 2. Everything else, block by block.
    for y in 0..CHUNK {
        for z in 0..CHUNK {
            for x in 0..CHUNK {
                let id = pad.get(x, y, z) as usize;
                if id == 0 {
                    continue;
                }
                let look = &looks[id];
                let at = origin + Vec3::new(x as f32, y as f32, z as f32);
                let solid = |dx: i32, dy: i32, dz: i32| looks[pad.get(x + dx, y + dy, z + dz) as usize].solid();
                match &look.shape {
                    Shape::Air | Shape::Cube | Shape::Liquid => {}
                    Shape::Boxes(boxes) => {
                        for b in boxes {
                            add_box(&mut parts, apps.faces[id], at, *b, &solid);
                        }
                    }
                    Shape::Connected(kind, sides) => {
                        let neighbour = |dx: i32, dz: i32| {
                            let other = &looks[pad.get(x + dx, y, z + dz) as usize];
                            other.solid()
                                || matches!(&other.shape, Shape::Connected(k, _) if k == kind)
                                || (*kind == Connect::Fence && other.kind.ends_with("_fence_gate"))
                        };
                        let offsets = [(0, -1), (1, 0), (0, 1), (-1, 0)];
                        let on: [bool; 4] =
                            std::array::from_fn(|s| sides[s].unwrap_or_else(|| neighbour(offsets[s].0, offsets[s].1)));
                        for b in connected(*kind, on) {
                            add_box(&mut parts, apps.faces[id], at, b, &solid);
                        }
                    }
                    Shape::Cross => {
                        let stem = apps.stems[id];
                        let side = apps.faces[id][1];
                        if stem != 0 {
                            cross(&mut parts, stem, at, 0.0, 0.5);
                            cross(&mut parts, side, at, 0.5, 0.85);
                        } else {
                            cross(&mut parts, side, at, 0.0, 1.0);
                        }
                    }
                }
            }
        }
    }
    parts
}

/// The faces of a box (in sixteenths) at `at`, leaving out those on the
/// block's boundary against a solid neighbour.
fn add_box(
    parts: &mut HashMap<u32, Part>,
    faces: [u32; 3],
    at: Vec3,
    b: [u8; 6],
    solid: &dyn Fn(i32, i32, i32) -> bool,
) {
    let lo = Vec3::new(b[0] as f32, b[1] as f32, b[2] as f32) / 16.0;
    let hi = Vec3::new(b[3] as f32, b[4] as f32, b[5] as f32) / 16.0;
    if lo.cmpge(hi).any() {
        return;
    }
    for d in Dir::ALL {
        let (a, positive) = d.axis();
        let plane = if positive { hi[a] } else { lo[a] };
        let on_edge = if positive { plane >= 1.0 } else { plane <= 0.0 };
        if on_edge {
            let mut off = [0i32; 3];
            off[a] = if positive { 1 } else { -1 };
            if solid(off[0], off[1], off[2]) {
                continue;
            }
        }
        let app = faces[face_kind(d)];
        if app == 0 {
            continue;
        }
        let (u, v) = ((a + 1) % 3, (a + 2) % 3);
        let corners = face(d, at[a] + plane, at[u] + lo[u], at[v] + lo[v], at[u] + hi[u], at[v] + hi[v]);
        parts.entry(app - 1).or_default().quad(corners, normal(d), Some(d));
    }
}

/// Two crossed vertical quads from height `y0` to `y1` (fractions of the
/// block), lit as if facing up so both sides look the same.
fn cross(parts: &mut HashMap<u32, Part>, app: u32, at: Vec3, y0: f32, y1: f32) {
    if app == 0 {
        return;
    }
    let part = parts.entry(app - 1).or_default();
    let inset = 0.15;
    for (a, b) in [((inset, inset), (1.0 - inset, 1.0 - inset)), ((inset, 1.0 - inset), (1.0 - inset, inset))] {
        let p = |x: f32, y: f32, z: f32| at + Vec3::new(x, y, z);
        let corners = [p(a.0, y0, a.1), p(b.0, y0, b.1), p(b.0, y1, b.1), p(a.0, y1, a.1)];
        let base = part.positions.len() as u32;
        let uvs =
            [Vec2::new(0.0, 1.0 - y0), Vec2::new(1.0, 1.0 - y0), Vec2::new(1.0, 1.0 - y1), Vec2::new(0.0, 1.0 - y1)];
        for (c, t) in corners.into_iter().zip(uvs) {
            part.positions.push(c);
            part.normals.push(Vec3::Y);
            part.uvs.push(t);
        }
        part.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// The boxes of a fence, wall or pane joined to the sides in `on` (north,
/// east, south, west).
fn connected(kind: Connect, on: [bool; 4]) -> Vec<[u8; 6]> {
    // Arms towards each side, given as boxes for north and turned.
    let turn = |b: [u8; 6], side: usize| -> [u8; 6] {
        let [x0, y0, z0, x1, y1, z1] = b;
        match side {
            0 => b,
            1 => [16 - z1, y0, x0, 16 - z0, y1, x1],
            2 => [x0, y0, 16 - z1, x1, y1, 16 - z0],
            _ => [z0, y0, x0, z1, y1, x1],
        }
    };
    let (post, arms): ([u8; 6], Vec<[u8; 6]>) = match kind {
        Connect::Fence => ([6, 0, 6, 10, 16, 10], vec![[7, 6, 0, 9, 9, 6], [7, 12, 0, 9, 15, 6]]),
        Connect::Wall => ([4, 0, 4, 12, 16, 12], vec![[5, 0, 0, 11, 14, 4]]),
        Connect::Pane => ([7, 0, 7, 9, 16, 9], vec![[7, 0, 0, 9, 16, 7]]),
    };
    let mut out = vec![post];
    for (side, &yes) in on.iter().enumerate() {
        if yes {
            out.extend(arms.iter().map(|&b| turn(b, side)));
        }
    }
    out
}

//endregion

/*┌─────────────────╮
| Scene           |
└─────────────────╯*/
//region Scene

/// Builds the scene of a schematic: its blocks meshed, centred on the
/// ground, one block to a metre.  Counts of each kind of block go to the
/// inspector.
pub fn scene(build: &Build, limits: &Limits, pack: Option<&Pack>, thumbnail: bool) -> Result<Scene, LoadError> {
    // 1. One palette for every region, air first.
    let mut states: Vec<BlockState> = vec![BlockState::air()];
    let mut ids: HashMap<BlockState, u16> = HashMap::new();
    let mut looks: Vec<Look> = vec![Look::air()];
    let mut maps: Vec<Vec<u16>> = Vec::with_capacity(build.regions.len());
    for r in &build.regions {
        let mut map = Vec::with_capacity(r.palette.len());
        for s in &r.palette {
            let look = crate::blocks::look(s);
            let id = if look.shape == Shape::Air {
                0
            } else if let Some(&id) = ids.get(s) {
                id
            } else {
                if states.len() >= u16::MAX as usize {
                    return Err(LoadError::TooLarge("more than 65,535 kinds of block".into()));
                }
                states.push(s.clone());
                looks.push(look);
                ids.insert(s.clone(), states.len() as u16 - 1);
                states.len() as u16 - 1
            };
            map.push(id);
        }
        maps.push(map);
    }

    // 2. Bounds and thinning.
    let mut min = [i64::MAX; 3];
    let mut max = [i64::MIN; 3];
    let mut volume = 0u64;
    for r in &build.regions {
        for a in 0..3 {
            min[a] = min[a].min(r.origin[a]);
            max[a] = max[a].max(r.origin[a] + r.size[a] as i64);
        }
        volume = volume.saturating_add(r.volume());
    }
    if volume == 0 {
        return Err(LoadError::invalid("the schematic holds no blocks"));
    }
    let budget = if thumbnail { THUMBNAIL_CELLS } else { VIEWER_CELLS };
    let mut k = 1u64;
    while volume / (k * k * k) > budget {
        k += 1;
    }
    for a in 0..3 {
        if (max[a] - min[a]) / k as i64 > i32::MAX as i64 / 2 {
            return Err(LoadError::TooLarge("the schematic's regions lie too far apart".into()));
        }
    }

    // 3. Fill the grid and count blocks.
    let mut grid = Grid { chunks: HashMap::new(), thinned: k > 1 };
    let mut counts: Vec<u64> = vec![0; states.len()];
    {
        let mut w = Writer::new(&mut grid);
        for (r, map) in build.regions.iter().zip(&maps) {
            let o = [r.origin[0] - min[0], r.origin[1] - min[1], r.origin[2] - min[2]];
            let k = k as i64;
            r.for_each(|x, y, z, i| {
                let id = map.get(i as usize).copied().unwrap_or(0);
                if id == 0 {
                    return;
                }
                counts[id as usize] += 1;
                let (gx, gy, gz) = (o[0] + x as i64, o[1] + y as i64, o[2] + z as i64);
                if k == 1 {
                    w.set(gx as i32, gy as i32, gz as i32, id, &looks);
                } else {
                    w.set((gx / k) as i32, (gy / k) as i32, (gz / k) as i32, id, &looks);
                }
            });
        }
    }
    let total: u64 = counts.iter().sum();
    if total == 0 {
        return Err(LoadError::invalid("the schematic holds only air"));
    }

    // 4. Mesh the chunks in parallel and gather the parts by appearance.
    let apps = Appearances::new(&states, &looks, pack);
    let keys: Vec<[i32; 3]> = grid.chunks.keys().copied().collect();
    let meshed: Vec<HashMap<u32, Part>> = keys.par_iter().map(|&key| mesh_chunk(&grid, key, &looks, &apps)).collect();
    drop(grid);
    let mut parts: Vec<Part> = (0..apps.list.len()).map(|_| Part::default()).collect();
    let mut triangles = 0u64;
    for chunk in meshed {
        for (app, p) in chunk {
            let into = &mut parts[app as usize];
            let base = into.positions.len() as u32;
            into.positions.extend(p.positions);
            into.normals.extend(p.normals);
            into.uvs.extend(p.uvs);
            into.indices.extend(p.indices.iter().map(|i| i + base));
            triangles += (p.indices.len() / 3) as u64;
        }
        if triangles > limits.max_triangles {
            return Err(LoadError::TooLarge(format!("{triangles} triangles")));
        }
    }

    // 5. One material per appearance, sharing the pattern textures.
    let mut scene = Scene::default();
    let mut images: HashMap<Image, (usize, f32)> = HashMap::new();
    let mut mesh = Mesh { name: "blocks".into(), ..Default::default() };
    for (app, part) in apps.list.iter().zip(parts) {
        if part.indices.is_empty() {
            continue;
        }
        let (texture, mean) = match images.get(&app.image) {
            Some(&v) => v,
            None => {
                let (tex, mean) = match &app.image {
                    Image::Pattern(p) => {
                        let (t, mean) = patterns::texture(*p);
                        (Some(Arc::new(t)), mean)
                    }
                    Image::Pack(name) => (pack.and_then(|p| p.texture(name)), 1.0),
                };
                let v = match tex {
                    Some(t) => {
                        scene.textures.push(t);
                        (scene.textures.len() - 1, mean)
                    }
                    None => (usize::MAX, 1.0),
                };
                images.insert(app.image.clone(), v);
                v
            }
        };
        scene.materials.push(material(app, (texture != usize::MAX).then_some(texture), mean));
        mesh.primitives.push(Primitive {
            positions: part.positions,
            normals: part.normals,
            uv0: part.uvs,
            indices: part.indices,
            material: Some(scene.materials.len() - 1),
            ..Default::default()
        });
    }
    if mesh.primitives.is_empty() {
        return Err(LoadError::invalid("nothing in the schematic is drawn"));
    }

    // 6. Centred on the ground, a metre to a block, scaled back up when
    //    thinned.
    let size = Vec3::new((max[0] - min[0]) as f32, (max[1] - min[1]) as f32, (max[2] - min[2]) as f32);
    let mut node = Node::new(if build.regions.len() == 1 { "blocks".to_string() } else { "regions".to_string() });
    let kf = k as f32;
    node.transform = Transform {
        translation: Vec3::new(-size.x / 2.0, 0.0, -size.z / 2.0),
        scale: Vec3::splat(kf),
        ..Transform::IDENTITY
    };
    scene.meshes.push(mesh);
    node.mesh = Some(0);
    scene.nodes.push(node);
    scene.roots = vec![0];

    // 7. Facts: size, block counts by kind.
    let mut info = Info { format: build.format.clone(), ..Default::default() };
    info.details.extend(build.details.iter().cloned());
    info.details.push(("Size".into(), format!("{} x {} x {} blocks", size.x, size.y, size.z)));
    if k > 1 {
        info.details.push(("Shown".into(), format!("every {k}th block in each direction")));
    }
    let mut by_name: HashMap<&str, u64> = HashMap::new();
    let mut unknown: Vec<&str> = Vec::new();
    for (i, s) in states.iter().enumerate().skip(1) {
        if counts[i] > 0 {
            *by_name.entry(s.name.as_str()).or_default() += counts[i];
            if !looks[i].known && !unknown.contains(&s.name.as_str()) {
                unknown.push(&s.name);
            }
        }
    }
    let mut sorted: Vec<(&str, u64)> = by_name.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    info.details.push(("Blocks".into(), format!("{} of {} kinds", thousands(total), sorted.len())));
    const SHOWN: usize = 40;
    // Counts first: the inspector's first column stays narrow while the
    // names, some long, take the wide one.
    for (name, n) in sorted.iter().take(SHOWN) {
        let share = *n as f64 * 100.0 / total as f64;
        info.details.push((thousands(*n), format!("{name}  ({share:.1} %)")));
    }
    if sorted.len() > SHOWN {
        let rest: u64 = sorted[SHOWN..].iter().map(|s| s.1).sum();
        info.details.push((thousands(rest), format!("{} other kinds", sorted.len() - SHOWN)));
    }
    if !unknown.is_empty() {
        unknown.sort_unstable();
        let shown = unknown.iter().take(12).copied().collect::<Vec<_>>().join(", ");
        let more = if unknown.len() > 12 { format!(" and {} more", unknown.len() - 12) } else { String::new() };
        info.details.push(("Guessed colours".into(), format!("{shown}{more}")));
    }
    info.details.push((
        "Block textures".into(),
        match pack {
            Some(p) => p.describe(),
            None => "built-in colours (no Minecraft jar or resource pack found)".into(),
        },
    ));
    scene.info = info;
    Ok(scene)
}

fn material(app: &Appearance, texture: Option<usize>, mean: f32) -> Material {
    // The colour is divided by the pattern's average brightness, so the
    // block averages to the table's colour.
    let lin = |v: u8| (patterns::srgb_to_linear(v) / mean.max(0.05)).min(1.0);
    let rgb = Vec3::new(lin(app.rgb[0]), lin(app.rgb[1]), lin(app.rgb[2]));
    let glow = app.glow as f32 / 10.0;
    let mut m = Material {
        name: format!("block {:02x}{:02x}{:02x}", app.rgb[0], app.rgb[1], app.rgb[2]),
        base_color: rgb.extend(1.0),
        base_color_texture: texture.map(|t| TexRef {
            sampler: Sampler {
                wrap_u: Wrap::Repeat,
                wrap_v: Wrap::Repeat,
                mag: Filter::Nearest,
                min: Filter::Linear,
                mipmaps: true,
            },
            ..TexRef::new(t)
        }),
        roughness: 0.9,
        specular: 0.25,
        emissive: rgb * glow,
        emissive_texture: if glow > 0.0 { texture.map(TexRef::new) } else { None },
        double_sided: app.double_sided,
        ..Default::default()
    };
    match app.layer {
        Layer::Opaque => {}
        Layer::Cutout => m.alpha_mode = AlphaMode::Mask(0.5),
        Layer::Translucent => {
            m.alpha_mode = AlphaMode::Blend;
            m.base_color.w = app.alpha as f32 / 100.0;
            m.roughness = 0.15;
            m.specular = 0.5;
        }
    }
    m
}

/// A count with thin spaces between thousands: 1 234 567.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faces_wind_outward() {
        for d in Dir::ALL {
            let c = face(d, 1.0, 0.0, 0.0, 1.0, 1.0);
            let n = (c[1] - c[0]).cross(c[2] - c[0]);
            assert!(n.normalize().abs_diff_eq(normal(d), 1e-6), "{d:?}: {n}");
        }
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(thousands(12), "12");
        // A fence joined north and east: post plus two arms each.
        let boxes = connected(Connect::Fence, [true, true, false, false]);
        assert_eq!(boxes.len(), 5);
        assert!(boxes.contains(&[10, 6, 7, 16, 9, 9]), "{boxes:?}");
    }
}
