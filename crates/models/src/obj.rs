//! Wavefront OBJ with its MTL material library.
//!
//! The reader works in two passes.  The first parses every statement into
//! flat arrays (positions, texture coordinates, normals, face corners) and
//! counts triangles, so oversized files are refused before any geometry is
//! built.  The second builds one node per object or group and one primitive
//! per material, deduplicating corners into shared vertices and
//! triangulating polygons (concave ones too).
//!
//! Supported: negative (relative) indices, forward references, polygons of
//! any size, lines and points, smoothing groups when the file has no normals
//! (faces outside any group are flat, as in Blender), vertex colours written
//! as `v x y z r g b` and ZBrush's `#MRGB` blocks, and MTL with the PBR
//! extension.  OBJ has no up axis or units; many files are Z-up, but nothing
//! in the file says so, so coordinates stay as stored.

use crate::common::{
    ChannelSource, FastMap, TextureSet, check_triangles, flipped_uv_transform, linear_color, looks_like_text,
    newell_normal, parse_f32, parse_i64, srgb_color, triangulate, trim,
};
use glam::{Mat3, Vec2, Vec3, Vec4};
use slerp_scene::{AlphaMode, Limits, LoadError, Material, Mesh, Primitive, Sampler, Scene, TexRef, Topology, Wrap};
use std::collections::HashMap;
use std::path::Path;

/// A text file, which a Windows COFF object file (also `.obj`) is not.
pub fn looks_like_obj(head: &[u8]) -> bool {
    crate::common::is_utf16(head) || looks_like_text(&head[..head.len().min(4096)])
}

/// An absent index in a face corner.
const NONE: u32 = u32::MAX;
/// An index that can never be valid: zero, or relative and before the start.
const INVALID: u32 = u32::MAX - 1;
/// Normal keys at or above this aren't indices into the file's normals.
const FLAT: u32 = 0x8000_0000;
const SMOOTH: u32 = 0xC000_0000;

/*┌─────────────────╮
| Parsing         |
└─────────────────╯*/
//region Parsing

/// A face, line or point statement: a run of corners.
#[derive(Clone, Copy)]
struct Element {
    start: u32,
    len: u32,
    /// Dense smoothing group id, 0 for none (flat).
    smooth: u32,
}

/// Consecutive elements sharing a node, material and kind.
struct Run {
    node: u32,
    material: u32,
    topology: Topology,
    elements: std::ops::Range<usize>,
}

#[derive(Default)]
struct Parsed {
    positions: Vec<Vec3>,
    /// Same length as `positions` once any vertex has a colour.
    colors: Vec<Vec4>,
    uvs: Vec<Vec2>,
    normals: Vec<Vec3>,
    /// (position, uv, normal) per corner, 0-based, `NONE` when absent.
    corners: Vec<[u32; 3]>,
    elements: Vec<Element>,
    runs: Vec<Run>,
    node_names: Vec<String>,
    material_names: Vec<String>,
    mtllibs: Vec<String>,
    generator: Option<String>,
    triangles: u64,
}

/// A statement that changes what later elements belong to.
enum Event {
    Object(String),
    Group(String),
    Material(String),
    Library(String),
    Smooth(i64),
}

/// One slice of the file, parsed on its own thread.  Indices can't all be
/// resolved yet: negative ones count back from a position that depends on
/// the chunks before, so they wait in `relative`.
#[derive(Default)]
struct Chunk {
    positions: Vec<Vec3>,
    colors: Vec<Vec4>,
    mrgb: Vec<Vec4>,
    uvs: Vec<Vec2>,
    normals: Vec<Vec3>,
    corners: Vec<[u32; 3]>,
    /// (corner, component, index relative to this chunk's first entry).
    relative: Vec<(u32, u8, i64)>,
    /// (first corner, corner count, kind).
    elements: Vec<(u32, u32, Topology)>,
    /// State changes, each with the number of elements before it.
    events: Vec<(usize, Event)>,
    generator: Option<String>,
    triangles: u64,
}

/// Iterates the file's statements, joining lines continued with a trailing
/// backslash.
fn statements(bytes: &[u8]) -> Box<dyn Iterator<Item = std::borrow::Cow<'_, [u8]>> + '_> {
    let lines = bytes.split(|&b| b == b'\n');
    if !has_continuations(bytes) {
        return Box::new(lines.map(std::borrow::Cow::Borrowed));
    }
    let mut pending: Vec<u8> = Vec::new();
    let mut lines = lines.peekable();
    Box::new(std::iter::from_fn(move || {
        loop {
            let line = lines.next()?;
            let t = line.strip_suffix(b"\r").unwrap_or(line);
            if let Some(cont) = t.strip_suffix(b"\\") {
                pending.extend_from_slice(cont);
                pending.push(b' ');
                if lines.peek().is_some() {
                    continue;
                }
            } else {
                pending.extend_from_slice(t);
            }
            return Some(std::borrow::Cow::Owned(std::mem::take(&mut pending)));
        }
    }))
}

fn has_continuations(bytes: &[u8]) -> bool {
    crate::common::find(bytes, b"\\\n").is_some() || crate::common::find(bytes, b"\\\r\n").is_some()
}

/// Returns the next whitespace-separated token at or after `*pos`.
#[inline]
fn next_token<'a>(line: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    let mut i = *pos;
    while i < line.len() && (line[i] == b' ' || line[i] == b'\t' || line[i] == b'\r') {
        i += 1;
    }
    let start = i;
    while i < line.len() && !(line[i] == b' ' || line[i] == b'\t' || line[i] == b'\r') {
        i += 1;
    }
    *pos = i;
    (i > start).then(|| &line[start..i])
}

/// Files smaller than this are parsed on one thread.
const PARALLEL_BYTES: usize = 4 << 20;

/// Parses the file in chunks on several threads, then stitches the chunks
/// together in order.
fn parse(bytes: &[u8], limits: &Limits) -> Result<Parsed, LoadError> {
    // 1. Split at line ends.  Continued lines could straddle a split, so
    //    files that use them stay whole.
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 8);
    let parts = if bytes.len() < PARALLEL_BYTES || has_continuations(bytes) { 1 } else { threads };
    let mut bounds = vec![0];
    for i in 1..parts {
        let mut at = (bytes.len() / parts * i).max(*bounds.last().unwrap_or(&0));
        while at < bytes.len() && bytes[at] != b'\n' {
            at += 1;
        }
        bounds.push((at + 1).min(bytes.len()));
    }
    bounds.push(bytes.len());
    // 2. Parse the chunks.
    let slices: Vec<&[u8]> = bounds.windows(2).map(|w| &bytes[w[0]..w[1]]).collect();
    let chunks: Vec<Result<Chunk, LoadError>> = if slices.len() == 1 {
        vec![parse_chunk(slices[0], true, limits)]
    } else {
        std::thread::scope(|s| {
            let workers: Vec<_> = slices
                .iter()
                .enumerate()
                .map(|(i, &slice)| s.spawn(move || parse_chunk(slice, i == 0, limits)))
                .collect();
            workers
                .into_iter()
                .map(|w| w.join().unwrap_or_else(|_| Err(LoadError::invalid("OBJ parser failed"))))
                .collect()
        })
    };
    let chunks = chunks.into_iter().collect::<Result<Vec<_>, _>>()?;
    // 3. Stitch them together.
    let p = merge(chunks);
    check_triangles(p.triangles, limits)?;
    Ok(p)
}

/// Turns an OBJ index into a 0-based one.  Negative indices count back from
/// the end of the list so far, which within a chunk is only known relative
/// to the chunk's start: those return `Err` with that relative index.
#[inline]
fn chunk_index(token: &[u8], count: usize) -> Result<u32, i64> {
    if token.is_empty() {
        return Ok(NONE);
    }
    match parse_i64(token) {
        Some(i) if i > 0 => Ok((i - 1).min(INVALID as i64 - 1) as u32),
        Some(i) if i < 0 => Err(count as i64 + i),
        _ => Ok(INVALID),
    }
}

fn parse_chunk(bytes: &[u8], first: bool, limits: &Limits) -> Result<Chunk, LoadError> {
    let mut c = Chunk::default();
    let mut line_number = 0usize;
    for line in statements(bytes) {
        let line: &[u8] = &line;
        line_number += 1;
        let mut pos = 0;
        let Some(keyword) = next_token(line, &mut pos) else { continue };
        let rest = || String::from_utf8_lossy(trim(&line[pos..])).into_owned();
        match keyword {
            b"v" => {
                let mut v = [0f32; 7];
                let mut n = 0;
                while let Some(t) = next_token(line, &mut pos) {
                    if n < 7 {
                        v[n] = parse_f32(t).unwrap_or(0.0);
                    }
                    n += 1;
                }
                c.positions.push(Vec3::new(v[0], v[1], v[2]));
                // "x y z r g b" (MeshLab, ZBrush, many scanners) and
                // "x y z r g b a"; "x y z w" is a rational weight.
                if n >= 6 {
                    if c.colors.len() + 1 < c.positions.len() {
                        c.colors.resize(c.positions.len() - 1, Vec4::ONE);
                    }
                    c.colors.push(Vec4::new(v[3], v[4], v[5], if n >= 7 { v[6] } else { 1.0 }));
                } else if !c.colors.is_empty() {
                    c.colors.push(Vec4::ONE);
                }
            }
            b"vt" => {
                let u = next_token(line, &mut pos).and_then(parse_f32).unwrap_or(0.0);
                let v = next_token(line, &mut pos).and_then(parse_f32).unwrap_or(0.0);
                c.uvs.push(Vec2::new(u, 1.0 - v));
            }
            b"vn" => {
                let mut v = [0f32; 3];
                for x in &mut v {
                    *x = next_token(line, &mut pos).and_then(parse_f32).unwrap_or(0.0);
                }
                c.normals.push(Vec3::from(v));
            }
            b"f" | b"fo" | b"l" | b"p" => {
                let topology = match keyword {
                    b"l" => Topology::Lines,
                    b"p" => Topology::Points,
                    _ => Topology::Triangles,
                };
                let start = c.corners.len();
                let relative_start = c.relative.len();
                while let Some(t) = next_token(line, &mut pos) {
                    let mut parts = t.splitn(3, |&b| b == b'/');
                    let counts = [c.positions.len(), c.uvs.len(), c.normals.len()];
                    let mut corner = [NONE; 3];
                    for (k, slot) in corner.iter_mut().enumerate() {
                        let Some(token) = parts.next() else { break };
                        *slot = match chunk_index(token, counts[k]) {
                            Ok(i) => i,
                            Err(relative) => {
                                c.relative.push((c.corners.len() as u32, k as u8, relative));
                                INVALID
                            }
                        };
                    }
                    c.corners.push(corner);
                }
                let len = c.corners.len() - start;
                let min = match topology {
                    Topology::Triangles => 3,
                    Topology::Lines => 2,
                    Topology::Points => 1,
                };
                if len < min {
                    c.corners.truncate(start);
                    c.relative.truncate(relative_start);
                    continue;
                }
                if topology == Topology::Triangles {
                    c.triangles += len as u64 - 2;
                    if c.triangles > limits.max_triangles {
                        check_triangles(c.triangles, limits)?;
                    }
                }
                c.elements.push((start as u32, len as u32, topology));
            }
            b"o" => c.events.push((c.elements.len(), Event::Object(rest()))),
            b"g" => c.events.push((c.elements.len(), Event::Group(rest()))),
            b"usemtl" => c.events.push((c.elements.len(), Event::Material(rest()))),
            b"mtllib" => c.events.push((c.elements.len(), Event::Library(rest()))),
            b"s" => {
                let group = next_token(line, &mut pos).and_then(parse_i64).unwrap_or(0);
                c.events.push((c.elements.len(), Event::Smooth(group)));
            }
            b"#MRGB" => {
                // ZBrush polypaint: 8 hex digits per vertex, mask then RGB.
                if let Some(t) = next_token(line, &mut pos) {
                    for x in t.chunks_exact(8) {
                        let hex = |s: &[u8]| {
                            std::str::from_utf8(s).ok().and_then(|s| u8::from_str_radix(s, 16).ok()).unwrap_or(255)
                        };
                        c.mrgb.push(Vec4::new(
                            hex(&x[2..4]) as f32 / 255.0,
                            hex(&x[4..6]) as f32 / 255.0,
                            hex(&x[6..8]) as f32 / 255.0,
                            1.0,
                        ));
                    }
                }
            }
            _ if first && keyword.first() == Some(&b'#') && line_number <= 10 && c.generator.is_none() => {
                let text = String::from_utf8_lossy(trim(&line[1..])).trim().to_string();
                let lower = text.to_ascii_lowercase();
                let known = [
                    "blender",
                    "export",
                    "created",
                    "generated",
                    "3ds",
                    "maya",
                    "zbrush",
                    "meshlab",
                    "cinema",
                    "houdini",
                    "sketchup",
                    "file",
                ];
                if known.iter().any(|k| lower.contains(k)) && text.len() <= 160 {
                    c.generator = Some(text);
                }
            }
            _ => {}
        }
    }
    Ok(c)
}

/// Joins the chunks in file order: concatenates the vertex data, resolves
/// relative indices, and replays the state changes to assign every element
/// its node, material and smoothing group.
fn merge(chunks: Vec<Chunk>) -> Parsed {
    let mut p = Parsed { node_names: vec!["default".into()], ..Parsed::default() };
    let mut node_keys: HashMap<(String, String), u32> = HashMap::new();
    node_keys.insert((String::new(), String::new()), 0);
    let (mut object, mut group) = (String::new(), String::new());
    let (mut node, mut material, mut smooth) = (0u32, NONE, 0u32);
    let mut smooth_ids: FastMap<i64, u32> = FastMap::default();
    let any_colors = chunks.iter().any(|c| !c.colors.is_empty());
    let mut mrgb = Vec::new();
    p.generator = chunks.first().and_then(|c| c.generator.clone());
    for c in chunks {
        let base = [p.positions.len() as i64, p.uvs.len() as i64, p.normals.len() as i64];
        let corner_base = p.corners.len() as u32;
        // 1. Vertex data.
        if any_colors {
            let mut colors = c.colors;
            colors.resize(c.positions.len(), Vec4::ONE);
            p.colors.extend(colors);
        }
        p.positions.extend(c.positions);
        p.uvs.extend(c.uvs);
        p.normals.extend(c.normals);
        mrgb.extend(c.mrgb);
        p.triangles += c.triangles;
        // 2. Corners, with relative indices made absolute.
        p.corners.extend(c.corners);
        for (corner, k, relative) in c.relative {
            let absolute = base[k as usize] + relative;
            if let Some(slot) = p.corners.get_mut((corner_base + corner) as usize) {
                slot[k as usize] = if (0..INVALID as i64).contains(&absolute) { absolute as u32 } else { INVALID };
            }
        }
        // 3. Elements, with the state changes between them.
        let mut events = c.events.into_iter().peekable();
        for (index, &(start, len, topology)) in
            c.elements.iter().enumerate().chain(std::iter::once((c.elements.len(), &(0, 0, Topology::Points))))
        {
            while let Some((_, event)) = events.next_if(|(at, _)| *at <= index) {
                match event {
                    Event::Object(name) => {
                        object = name;
                        group.clear();
                    }
                    Event::Group(name) => group = name,
                    Event::Material(name) => {
                        material = match p.material_names.iter().position(|n| *n == name) {
                            Some(i) => i as u32,
                            None => {
                                p.material_names.push(name);
                                (p.material_names.len() - 1) as u32
                            }
                        };
                        continue;
                    }
                    Event::Library(name) => {
                        if !name.is_empty() && !p.mtllibs.contains(&name) {
                            p.mtllibs.push(name);
                        }
                        continue;
                    }
                    Event::Smooth(g) => {
                        smooth = match g {
                            0 => 0,
                            g => {
                                let next = smooth_ids.len() as u32 + 1;
                                *smooth_ids.entry(g).or_insert(next.min(SMOOTH - FLAT - 1))
                            }
                        };
                        continue;
                    }
                }
                // Objects and groups pick the node.
                let next = p.node_names.len() as u32;
                node = *node_keys.entry((object.clone(), group.clone())).or_insert(next);
                if node == next {
                    let label = if !group.is_empty() && group != "default" { &group } else { &object };
                    p.node_names.push(if label.is_empty() { "default".into() } else { label.clone() });
                }
            }
            if index == c.elements.len() {
                break;
            }
            let element = Element { start: corner_base + start, len, smooth };
            let at = p.elements.len();
            p.elements.push(element);
            match p.runs.last_mut() {
                Some(r) if r.node == node && r.material == material && r.topology == topology => {
                    r.elements.end = at + 1
                }
                _ => p.runs.push(Run { node, material, topology, elements: at..at + 1 }),
            }
        }
    }
    if p.colors.is_empty() && mrgb.len() == p.positions.len() && !mrgb.is_empty() {
        p.colors = mrgb;
    }
    // Colours may be 0-255 rather than 0-1; either way they are sRGB.
    let max = p.colors.iter().map(|c| c.truncate().max_element()).fold(0.0f32, f32::max);
    let scale = if max > 1.0 { 1.0 / 255.0 } else { 1.0 };
    for c in &mut p.colors {
        *c =
            srgb_color(c.x * scale, c.y * scale, c.z * scale, if scale < 1.0 && c.w > 1.0 { c.w * scale } else { c.w });
    }
    p
}

//endregion

/*┌─────────────────╮
| Building        |
└─────────────────╯*/
//region Building

/// Deduplicates (position, uv, normal key) corners into vertices.  Corners
/// are chained per position index, which is faster than hashing and needs
/// no table larger than the positions.
struct Dedup {
    head: Vec<u32>,
    next: Vec<u32>,
    keys: Vec<(u32, u32)>,
    source: Vec<u32>,
}

impl Dedup {
    fn new(positions: usize) -> Self {
        Dedup { head: vec![NONE; positions], next: Vec::new(), keys: Vec::new(), source: Vec::new() }
    }

    /// Returns the vertex for a corner and whether it is new.
    #[inline]
    fn get(&mut self, v: u32, key: (u32, u32)) -> (u32, bool) {
        let mut i = self.head[v as usize];
        while i != NONE {
            if self.keys[i as usize] == key {
                return (i, false);
            }
            i = self.next[i as usize];
        }
        let index = self.keys.len() as u32;
        self.keys.push(key);
        self.next.push(self.head[v as usize]);
        self.source.push(v);
        self.head[v as usize] = index;
        (index, true)
    }

    /// Makes a vertex no other corner shares, for flat-shaded faces: their
    /// key holds the face number, so a lookup could never succeed.
    #[inline]
    fn fresh(&mut self, v: u32) -> u32 {
        let index = self.keys.len() as u32;
        self.keys.push((NONE, NONE));
        self.next.push(NONE);
        self.source.push(v);
        index
    }

    /// Clears the table for the next primitive, touching only used entries.
    fn reset(&mut self) {
        for &v in &self.source {
            self.head[v as usize] = NONE;
        }
        self.next.clear();
        self.keys.clear();
        self.source.clear();
    }
}

/// Smooth normals per (position, smoothing group), summed from the faces
/// that have no normals of their own, area-weighted.
fn smooth_normals(p: &Parsed) -> FastMap<(u32, u32), Vec3> {
    let mut acc: FastMap<(u32, u32), Vec3> = FastMap::default();
    let mut pts = Vec::new();
    for run in p.runs.iter().filter(|r| r.topology == Topology::Triangles) {
        for e in &p.elements[run.elements.clone()] {
            if e.smooth == 0 {
                continue;
            }
            let corners = &p.corners[e.start as usize..(e.start + e.len) as usize];
            if corners.iter().all(|c| (c[2] as usize) < p.normals.len()) {
                continue;
            }
            pts.clear();
            pts.extend(corners.iter().map(|c| p.positions.get(c[0] as usize).copied().unwrap_or(Vec3::ZERO)));
            let n = newell_normal(&pts);
            for c in corners {
                if (c[0] as usize) < p.positions.len() && (c[2] as usize) >= p.normals.len() {
                    *acc.entry((c[0], e.smooth)).or_insert(Vec3::ZERO) += n;
                }
            }
        }
    }
    acc
}

/// Builds one primitive from the runs that share a node, material and kind.
fn build_primitive(
    p: &Parsed,
    runs: &[&Run],
    dedup: &mut Dedup,
    smooth: &FastMap<(u32, u32), Vec3>,
    skipped: &mut usize,
) -> Primitive {
    let topology = runs[0].topology;
    let mut prim = Primitive { topology, ..Default::default() };
    let mut uvs = Vec::new();
    let mut normals = Vec::new();
    let mut any_uv = false;
    let mut tris = Vec::new();
    let mut face_vertices: Vec<u32> = Vec::new();
    let mut pts = Vec::new();
    let npos = p.positions.len();
    for run in runs {
        for (ei, e) in p.elements[run.elements.clone()].iter().enumerate() {
            let face_index = (run.elements.start + ei) as u32;
            let all = &p.corners[e.start as usize..(e.start + e.len) as usize];
            // Corners naming missing positions are dropped; the face stays
            // if enough remain.
            let valid: Vec<[u32; 3]>;
            let corners = if all.iter().any(|c| c[0] as usize >= npos) {
                *skipped += 1;
                valid = all.iter().copied().filter(|c| (c[0] as usize) < npos).collect();
                let min = match topology {
                    Topology::Triangles => 3,
                    Topology::Lines => 2,
                    Topology::Points => 1,
                };
                if valid.len() < min {
                    continue;
                }
                &valid[..]
            } else {
                all
            };
            let mut face_normal = Vec3::ZERO;
            let order: &[[u32; 3]] = match topology {
                Topology::Triangles => {
                    pts.clear();
                    pts.extend(corners.iter().map(|c| p.positions[c[0] as usize]));
                    face_normal = newell_normal(&pts).normalize_or_zero();
                    tris.clear();
                    triangulate(&pts, &mut tris);
                    &tris
                }
                _ => &[],
            };
            let mut vertex = |c: &[u32; 3], prim: &mut Primitive| -> u32 {
                let has_uv = (c[1] as usize) < p.uvs.len();
                let has_normal = (c[2] as usize) < p.normals.len();
                let nkey = match topology {
                    Topology::Triangles if has_normal => c[2],
                    Topology::Triangles if e.smooth == 0 => FLAT | face_index.min(SMOOTH - FLAT - 1),
                    Topology::Triangles => SMOOTH | e.smooth,
                    _ => 0,
                };
                let (index, new) = if (FLAT..SMOOTH).contains(&nkey) {
                    (dedup.fresh(c[0]), true)
                } else {
                    dedup.get(c[0], (if has_uv { c[1] } else { NONE }, nkey))
                };
                if new {
                    prim.positions.push(p.positions[c[0] as usize]);
                    if !p.colors.is_empty() {
                        prim.colors.push(p.colors[c[0] as usize]);
                    }
                    uvs.push(if has_uv { p.uvs[c[1] as usize] } else { Vec2::ZERO });
                    any_uv |= has_uv;
                    if topology == Topology::Triangles {
                        normals.push(match nkey {
                            k if k < FLAT => p.normals[k as usize],
                            k if k < SMOOTH => face_normal,
                            _ => smooth.get(&(c[0], e.smooth)).map_or(face_normal, |n| n.normalize_or_zero()),
                        });
                    }
                }
                index
            };
            match topology {
                Topology::Triangles => {
                    // One vertex per corner, shared by the polygon's
                    // triangles.
                    face_vertices.clear();
                    for c in corners {
                        let i = vertex(c, &mut prim);
                        face_vertices.push(i);
                    }
                    for tri in order {
                        prim.indices.extend(tri.map(|k| face_vertices[k as usize]));
                    }
                }
                Topology::Lines => {
                    for pair in corners.windows(2) {
                        let a = vertex(&pair[0], &mut prim);
                        let b = vertex(&pair[1], &mut prim);
                        prim.indices.extend([a, b]);
                    }
                }
                Topology::Points => {
                    for c in corners {
                        let i = vertex(c, &mut prim);
                        prim.indices.push(i);
                    }
                }
            }
        }
    }
    dedup.reset();
    if any_uv {
        prim.uv0 = uvs;
    }
    prim.normals = normals;
    prim
}

//endregion

/*┌─────────────────╮
| Materials       |
└─────────────────╯*/
//region Materials

/// A texture statement such as `map_Kd -s 2 2 1 -clamp on wood.png`.
#[derive(Clone, Debug, Default)]
struct MapRef {
    file: String,
    bump_scale: Option<f32>,
    offset: Vec2,
    scale: Option<Vec2>,
    clamp: bool,
}

#[derive(Clone, Debug, Default)]
struct MtlDef {
    kd: Option<Vec3>,
    ks: Option<Vec3>,
    ke: Option<Vec3>,
    ns: Option<f32>,
    d: Option<f32>,
    tr: Option<f32>,
    ni: Option<f32>,
    pr: Option<f32>,
    pm: Option<f32>,
    pc: Option<f32>,
    pcr: Option<f32>,
    illum: Option<i64>,
    map_kd: Option<MapRef>,
    map_ke: Option<MapRef>,
    map_d: Option<MapRef>,
    bump: Option<MapRef>,
    norm: Option<MapRef>,
    map_pr: Option<MapRef>,
    map_pm: Option<MapRef>,
    map_ao: Option<MapRef>,
}

fn parse_map(rest: &[u8]) -> MapRef {
    let mut m = MapRef::default();
    let mut pos = 0;
    loop {
        let save = pos;
        let Some(t) = next_token(rest, &mut pos) else { break };
        if t.first() != Some(&b'-') || t.len() < 2 || parse_f32(t).is_some() {
            pos = save;
            break;
        }
        // Each option takes a fixed or optional number of numeric values.
        let numbers = |max: usize, pos: &mut usize| -> Vec<f32> {
            let mut out = Vec::new();
            while out.len() < max {
                let save = *pos;
                match next_token(rest, pos).and_then(parse_f32) {
                    Some(v) => out.push(v),
                    None => {
                        *pos = save;
                        break;
                    }
                }
            }
            out
        };
        match t {
            b"-bm" => m.bump_scale = numbers(1, &mut pos).first().copied(),
            b"-o" => {
                let v = numbers(3, &mut pos);
                m.offset = Vec2::new(v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0));
            }
            b"-s" => {
                let v = numbers(3, &mut pos);
                m.scale = Some(Vec2::new(v.first().copied().unwrap_or(1.0), v.get(1).copied().unwrap_or(1.0)));
            }
            b"-t" => drop(numbers(3, &mut pos)),
            b"-mm" => drop(numbers(2, &mut pos)),
            b"-boost" | b"-texres" => drop(numbers(1, &mut pos)),
            b"-clamp" => m.clamp = next_token(rest, &mut pos) == Some(b"on"),
            b"-blendu" | b"-blendv" | b"-cc" | b"-imfchan" | b"-type" => drop(next_token(rest, &mut pos)),
            _ => {}
        }
    }
    m.file = String::from_utf8_lossy(trim(&rest[pos.min(rest.len())..])).into_owned();
    m
}

fn parse_mtl(bytes: &[u8], out: &mut HashMap<String, MtlDef>) {
    let mut current: Option<String> = None;
    for line in bytes.split(|&b| b == b'\n') {
        let mut pos = 0;
        let Some(keyword) = next_token(line, &mut pos) else { continue };
        let rest = &line[pos..];
        if keyword == b"newmtl" {
            let name = String::from_utf8_lossy(trim(rest)).into_owned();
            out.entry(name.clone()).or_default();
            current = Some(name);
            continue;
        }
        let Some(m) = current.as_ref().and_then(|n| out.get_mut(n)) else { continue };
        let num = |pos: &mut usize| next_token(line, pos).and_then(parse_f32);
        let color = |pos: &mut usize| -> Option<Vec3> {
            let r = num(pos)?;
            // "Kd 0.5" means grey; "Kd spectral ..." and "Kd xyz ..." aren't read.
            let g = num(pos).unwrap_or(r);
            let b = num(pos).unwrap_or(g);
            Some(Vec3::new(r, g, b))
        };
        match keyword.to_ascii_lowercase().as_slice() {
            b"kd" => m.kd = color(&mut pos),
            b"ks" => m.ks = color(&mut pos),
            b"ke" => m.ke = color(&mut pos),
            b"ns" => m.ns = num(&mut pos),
            b"d" => {
                m.d = next_token(line, &mut pos).and_then(|t| if t == b"-halo" { num(&mut pos) } else { parse_f32(t) })
            }
            b"tr" => m.tr = num(&mut pos),
            b"ni" => m.ni = num(&mut pos),
            b"pr" => m.pr = num(&mut pos),
            b"pm" => m.pm = num(&mut pos),
            b"pc" => m.pc = num(&mut pos),
            b"pcr" => m.pcr = num(&mut pos),
            b"illum" => m.illum = next_token(line, &mut pos).and_then(parse_i64),
            b"map_kd" => m.map_kd = Some(parse_map(rest)),
            b"map_ke" => m.map_ke = Some(parse_map(rest)),
            b"map_d" => m.map_d = Some(parse_map(rest)),
            b"map_bump" | b"bump" => m.bump = Some(parse_map(rest)),
            b"norm" | b"map_kn" | b"map_normal" => m.norm = Some(parse_map(rest)),
            b"map_pr" => m.map_pr = Some(parse_map(rest)),
            b"map_pm" => m.map_pm = Some(parse_map(rest)),
            b"map_ka" | b"map_ao" => m.map_ao = Some(parse_map(rest)),
            _ => {}
        }
    }
}

fn tex_ref(textures: &mut TextureSet, map: &MapRef) -> Option<TexRef> {
    let index = textures.file(&map.file)?;
    Some(with_options(TexRef::new(index), map))
}

fn with_options(mut r: TexRef, map: &MapRef) -> TexRef {
    let scale = map.scale.unwrap_or(Vec2::ONE);
    r.transform = flipped_uv_transform(Mat3::from_translation(map.offset) * Mat3::from_scale(scale));
    if map.clamp {
        r.sampler = Sampler { wrap_u: Wrap::ClampToEdge, wrap_v: Wrap::ClampToEdge, ..Sampler::default() };
    }
    r
}

/// Maps an MTL definition onto the metallic-roughness model.
fn convert_material(name: &str, m: &MtlDef, textures: &mut TextureSet, details: &mut Vec<String>) -> Material {
    let mut out = Material { name: name.to_string(), ..Material::default() };
    let pbr = m.pr.is_some() || m.pm.is_some() || m.map_pr.is_some() || m.map_pm.is_some();

    // 1. Colour and opacity.  "Tr" is 1 - "d", though some exporters write
    //    opacity there; a material that would be invisible is taken as
    //    such a mistake and drawn opaque.
    let kd = m.kd.unwrap_or(Vec3::splat(0.8));
    let mut alpha = m.d.or(m.tr.map(|t| 1.0 - t)).unwrap_or(1.0).clamp(0.0, 1.0);
    if alpha < 0.01 {
        alpha = 1.0;
    }
    out.base_color = linear_color(kd.x, kd.y, kd.z, alpha);
    if let Some(map) = &m.map_kd
        && let Some(mut r) = tex_ref(textures, map)
    {
        // Blender, the most common source, ignores Kd under a texture,
        // and exporters often write black or 0.6 grey there.
        out.base_color = Vec4::new(1.0, 1.0, 1.0, alpha);
        if let Some(mask) = m.map_d.as_ref().and_then(|d| textures.file(&d.file))
            && let Some(merged) = textures.merge_alpha(r.texture, mask)
        {
            r.texture = merged;
            out.alpha_mode = AlphaMode::Mask(0.5);
            out.double_sided = true;
        }
        out.base_color_texture = Some(r);
    }
    if out.alpha_mode == AlphaMode::Opaque && alpha < 0.999 {
        out.alpha_mode = AlphaMode::Blend;
    }

    // 2. Roughness and metalness: the PBR extension when present, else
    //    Blender's mapping of the Phong exponent (Ns 0..1000), so files
    //    Blender wrote come back as they were.
    out.roughness = match (m.pr, m.ns) {
        (Some(pr), _) => pr,
        (None, Some(ns)) if !pbr => 1.0 - (ns.clamp(0.0, 1000.0) / 1000.0).sqrt(),
        _ => out.roughness,
    }
    .clamp(0.0, 1.0);
    out.metallic = m.pm.unwrap_or(0.0).clamp(0.0, 1.0);
    if !pbr {
        // Phong's specular colour as the strength of the dielectric
        // reflection: Blender writes 0.5 for its default.
        if let Some(ks) = m.ks {
            out.specular = (ks.max_element() * 2.0).clamp(0.0, 1.0);
        }
    }
    let rough = m.map_pr.as_ref().and_then(|map| textures.file(&map.file).map(|t| (t, map)));
    let metal = m.map_pm.as_ref().and_then(|map| textures.file(&map.file).map(|t| (t, map)));
    let source =
        |t: Option<(usize, &MapRef)>| t.map(|(texture, _)| ChannelSource { texture, channel: 0, invert: false });
    if let Some(packed) = textures.pack_metal_rough(source(rough), source(metal)) {
        if rough.is_some() {
            out.roughness = 1.0;
        }
        if metal.is_some() {
            out.metallic = 1.0;
        }
        let options = rough.or(metal).map(|(_, map)| map.clone()).unwrap_or_default();
        out.metallic_roughness_texture = Some(with_options(TexRef::new(packed), &options));
    }
    if let Some(ni) = m.ni.filter(|&n| n >= 1.0 && n.is_finite()) {
        out.ior = ni;
    }
    out.clearcoat = m.pc.unwrap_or(0.0).clamp(0.0, 1.0);
    out.clearcoat_roughness = m.pcr.unwrap_or(0.0).clamp(0.0, 1.0);

    // 3. Emission.
    if let Some(ke) = m.ke {
        out.emissive = linear_color(ke.x, ke.y, ke.z, 1.0).truncate();
    }
    if let Some(r) = m.map_ke.as_ref().and_then(|map| tex_ref(textures, map)) {
        if out.emissive == Vec3::ZERO {
            out.emissive = Vec3::ONE;
        }
        out.emissive_texture = Some(r);
    }

    // 4. Normal maps: "norm" always is one; "bump" often is one too, as
    //    Blender writes normal maps there, so its picture decides.
    if let Some(map) = &m.norm {
        if let Some(r) = tex_ref(textures, map) {
            out.normal_texture = Some(r);
            out.normal_scale = map.bump_scale.unwrap_or(1.0);
        }
    } else if let Some(map) = &m.bump
        && let Some(r) = tex_ref(textures, map)
    {
        if textures.get(r.texture).is_some_and(crate::common::looks_like_normal_map) {
            out.normal_texture = Some(r);
            out.normal_scale = map.bump_scale.unwrap_or(1.0);
        } else {
            details.push(format!("{name}: height-map bump not drawn"));
        }
    }
    if let Some(r) = m.map_ao.as_ref().and_then(|map| tex_ref(textures, map)) {
        out.occlusion_texture = Some(r);
    }
    out
}

//endregion

pub fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    // 1. Parse everything; this refuses oversized files early.
    let text = crate::common::utf8_text(bytes);
    let p = parse(&text, limits)?;
    let mut scene = Scene::default();
    scene.info.format = "Wavefront OBJ".into();
    scene.info.generator = p.generator.clone();
    if p.positions.is_empty() {
        return Err(LoadError::invalid("OBJ has no vertices"));
    }

    // 2. Materials, from every library the file names (or one named like
    //    the file when it names none but uses materials).
    let mut textures = TextureSet::new(path, limits);
    let mut defs: HashMap<String, MtlDef> = HashMap::new();
    let mut libraries = p.mtllibs.clone();
    if libraries.is_empty()
        && !p.material_names.is_empty()
        && let Some(stem) = path.file_stem()
    {
        libraries.push(format!("{}.mtl", stem.to_string_lossy()));
    }
    for lib in &libraries {
        // A name with spaces may be one file or several.
        let found = textures.read_neighbour(lib).map(|f| vec![f]).or_else(|| {
            let parts: Vec<_> = lib.split_whitespace().filter_map(|n| textures.read_neighbour(n)).collect();
            (!parts.is_empty()).then_some(parts)
        });
        match found {
            Some(files) => files.iter().for_each(|(_, b)| parse_mtl(b, &mut defs)),
            None if limits.neighbours && !p.mtllibs.is_empty() => {
                scene.info.warnings.push(format!("material library not found: {lib}"))
            }
            None => {}
        }
    }
    let mut details = Vec::new();
    for name in &p.material_names {
        let material = match defs.get(name) {
            Some(def) => convert_material(name, def, &mut textures, &mut details),
            None => Material { name: name.clone(), ..Material::default() },
        };
        scene.materials.push(material);
    }

    // 3. Geometry: one node per object or group, one primitive per
    //    material and kind.
    let smooth = smooth_normals(&p);
    let mut dedup = Dedup::new(p.positions.len());
    let mut skipped = 0usize;
    let mut by_node: Vec<Vec<&Run>> = (0..p.node_names.len()).map(|_| Vec::new()).collect();
    for run in &p.runs {
        by_node[run.node as usize].push(run);
    }
    for (node_index, runs) in by_node.iter().enumerate() {
        if runs.is_empty() {
            continue;
        }
        let mut keys: Vec<(u32, Topology)> = Vec::new();
        for r in runs {
            if !keys.contains(&(r.material, r.topology)) {
                keys.push((r.material, r.topology));
            }
        }
        let mut mesh = Mesh { name: p.node_names[node_index].clone(), ..Mesh::default() };
        for key in keys {
            let group: Vec<&Run> = runs.iter().copied().filter(|r| (r.material, r.topology) == key).collect();
            let mut prim = build_primitive(&p, &group, &mut dedup, &smooth, &mut skipped);
            if prim.indices.is_empty() {
                continue;
            }
            prim.material = (key.0 != NONE).then_some(key.0 as usize);
            mesh.primitives.push(prim);
        }
        if !mesh.primitives.is_empty() {
            scene.add_mesh_node(mesh);
        }
    }
    if skipped > 0 {
        scene.info.warnings.push(format!("{skipped} faces refer to missing vertices"));
    }
    if scene.meshes.is_empty() {
        // Vertices and nothing that uses them: a point cloud, as scanners
        // write them.
        let points = Primitive {
            topology: Topology::Points,
            indices: (0..p.positions.len() as u32).collect(),
            positions: p.positions.clone(),
            colors: p.colors.clone(),
            ..Primitive::default()
        };
        scene.add_mesh_node(Mesh { name: "points".into(), primitives: vec![points], target_names: Vec::new() });
        scene.info.details.push(("Points".into(), p.positions.len().to_string()));
    }

    // 4. Facts for the inspector.
    scene.info.details.push(("Objects".into(), scene.nodes.len().to_string()));
    scene.info.details.push(("Materials".into(), scene.materials.len().to_string()));
    if !p.colors.is_empty() {
        scene.info.details.push(("Vertex colours".into(), "yes".into()));
    }
    scene.info.details.extend(details.into_iter().map(|d| ("Note".to_string(), d)));
    scene.info.warnings.extend(textures.warnings);
    scene.textures = textures.textures;
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_str(text: &str) -> Scene {
        load(Path::new("/nonexistent/t.obj"), text.as_bytes(), &Limits::viewer()).unwrap()
    }

    #[test]
    fn quads_negative_indices_and_groups() {
        let scene = load_str(
            "# Blender 4.2 OBJ\no A\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf -4 -3 -2 -1\n\
             o B\nv 0 0 1\nv 1 0 1\nv 1 1 1\nf 5 6 7\nusemtl red\nf 5 7 6\n",
        );
        assert_eq!(scene.info.generator.as_deref(), Some("Blender 4.2 OBJ"));
        assert_eq!(scene.nodes.len(), 2);
        assert_eq!(scene.meshes[0].name, "A");
        assert_eq!(scene.meshes[1].primitives.len(), 2);
        assert_eq!(scene.triangle_count(), 4);
        // No normals and no smoothing group: flat, so every face has its own
        // vertices.
        assert_eq!(scene.meshes[1].primitives[0].positions.len(), 3);
        assert_eq!(scene.meshes[0].primitives[0].normals[0], Vec3::Z);
        assert_eq!(scene.materials[0].name, "red");
    }

    #[test]
    fn shares_vertices_and_smooths() {
        let scene = load_str(
            "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\ns 1\nf 1/1 2/2 3/3\nf 1/1 3/3 4/4\n",
        );
        let p = &scene.meshes[0].primitives[0];
        assert_eq!(p.positions.len(), 4);
        assert_eq!(p.uv0[2], Vec2::new(1.0, 0.0));
        assert!((p.normals[0] - Vec3::Z).length() < 1e-6);
    }

    #[test]
    fn concave_polygons_lines_points_and_colours() {
        let scene = load_str(
            "v 0 0 0 1 0 0\nv 2 0 0 0 1 0\nv 2 1 0 0 0 1\nv 1 1 0\nv 1 2 0\nv 0 2 0\nvn 0 0 1\n\
             f 1//1 2//1 3//1 4//1 5//1 6//1\nl 1 2 3\np 4 5\n",
        );
        let prims = &scene.meshes[0].primitives;
        assert_eq!(prims.len(), 3);
        assert_eq!(prims[0].triangle_count(), 4);
        assert_eq!(prims[0].colors[1], Vec4::new(0.0, 1.0, 0.0, 1.0));
        assert_eq!(prims[0].colors[3], Vec4::ONE);
        assert_eq!(prims[1].topology, Topology::Lines);
        assert_eq!(prims[1].indices.len(), 4);
        assert_eq!(prims[2].topology, Topology::Points);
    }

    #[test]
    fn mtl_maps_and_pbr() {
        let mut defs = HashMap::new();
        parse_mtl(
            b"newmtl a\nKd 1 0 0\nKs 0.5 0.5 0.5\nNs 250\nd 0.5\nmap_Kd -s 2 2 1 -clamp on my tex.png\nnewmtl b\nPr 0.3\nPm 1\nKe 1 1 1\nTr 1\n",
            &mut defs,
        );
        let a = &defs["a"];
        assert_eq!(a.map_kd.as_ref().unwrap().file, "my tex.png");
        assert!(a.map_kd.as_ref().unwrap().clamp);
        let limits = Limits { neighbours: false, ..Limits::viewer() };
        let mut textures = TextureSet::new(Path::new("x.obj"), &limits);
        let mut notes = Vec::new();
        let m = convert_material("a", a, &mut textures, &mut notes);
        assert_eq!(m.base_color, Vec4::new(1.0, 0.0, 0.0, 0.5));
        assert_eq!(m.alpha_mode, AlphaMode::Blend);
        assert!((m.roughness - 0.5).abs() < 1e-6);
        assert_eq!(m.specular, 1.0);
        let b = convert_material("b", &defs["b"], &mut textures, &mut notes);
        assert_eq!((b.roughness, b.metallic, b.emissive), (0.3, 1.0, Vec3::ONE));
        // "Tr 1" would make it invisible: taken as an exporter's mistake.
        assert_eq!(b.alpha_mode, AlphaMode::Opaque);
    }

    #[test]
    fn missing_mtl_and_bad_indices_are_warnings() {
        let dir = std::env::temp_dir().join(format!("slerp-obj-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.obj");
        let text = b"mtllib gone.mtl\nusemtl x\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\nf 1 2 9\nf 0 1 2\nf -9 1 2\n";
        let scene = load(&path, text, &Limits::viewer()).unwrap();
        assert_eq!(scene.triangle_count(), 1);
        assert_eq!(scene.info.warnings.len(), 2, "{:?}", scene.info.warnings);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn survives_garbage() {
        let text = b"v 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0.5\nvn\nf 1/1/1 2/2/2 3/3/3\nf 1/9999999999999999999/1 2 3\nf a b c\nf\nl 1\ns banana\ng\no\nusemtl\n#MRGB ff\nf 1//-7 2// 3//\n\\";
        for cut in 0..text.len() {
            let _ = load(Path::new("t.obj"), &text[..cut], &Limits::viewer());
        }
        let scene = load(Path::new("t.obj"), text, &Limits::viewer()).unwrap();
        assert!(scene.triangle_count() >= 2);
        let limits = Limits { max_triangles: 1, ..Limits::viewer() };
        assert!(matches!(load(Path::new("t.obj"), text, &limits), Err(LoadError::TooLarge(_))));
        assert!(load(Path::new("t.obj"), b"\xff\xfe\0garbage", &Limits::viewer()).is_err());
    }

    #[test]
    fn vertices_alone_are_a_point_cloud() {
        let scene = load_str("v 0 0 0 1 0 0\nv 1 0 0 0 1 0\n");
        let p = &scene.meshes[0].primitives[0];
        assert_eq!((p.topology, p.indices.len(), p.colors.len()), (Topology::Points, 2, 2));
    }

    #[test]
    fn parallel_parse_matches_one_chunk() {
        // Relative indices, groups, materials and smoothing all cross the
        // chunk boundaries.
        let mut text = String::new();
        for i in 0..80_000 {
            text += &format!("v {i} 0 0\nv {i} 1 0\nv {i} 0 1\nvt 0 {i}\n");
            if i % 997 == 0 {
                text += &format!("g part{}\nusemtl m{}\ns {}\n", i / 997, i % 3, i % 2);
            }
            text += &format!("f -3/-1 -2/-1 -1/-1\nf {} {} {}\n", i * 3 + 1, i * 3 + 2, i * 3 + 3);
        }
        assert!(text.len() > PARALLEL_BYTES);
        let one = merge(vec![parse_chunk(text.as_bytes(), true, &Limits::viewer()).unwrap()]);
        let many = parse(text.as_bytes(), &Limits::viewer()).unwrap();
        assert_eq!(one.corners, many.corners);
        assert_eq!(one.positions, many.positions);
        let elements = |p: &Parsed| p.elements.iter().map(|e| (e.start, e.len, e.smooth)).collect::<Vec<_>>();
        assert_eq!(elements(&one), elements(&many));
        let runs = |p: &Parsed| p.runs.iter().map(|r| (r.node, r.material, r.elements.clone())).collect::<Vec<_>>();
        assert_eq!(runs(&one), runs(&many));
        assert_eq!((one.node_names, one.material_names), (many.node_names, many.material_names));
        assert_eq!(many.corners[0], [0, 0, NONE]);
    }

    #[test]
    fn continued_lines() {
        let scene = load_str("v 0 0 0\nv 1 0 0\nv 0 1 0\nv 1 1 0\nf 1 2 \\\n 4 3\n");
        assert_eq!(scene.triangle_count(), 2);
    }
}
