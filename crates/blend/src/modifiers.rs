//! An object's modifier stack, applied in order to its mesh: Mirror,
//! Array, Subdivision Surface (and Multiresolution's base smoothing),
//! Solidify, Triangulate, Weld, Edge Split and the "Smooth by Angle"
//! geometry nodes asset.  Modifiers switched off for the viewport are
//! skipped; any other kind is listed so the loader can say what's missing.

use crate::file::BlendFile;
use crate::normals;
use crate::poly::{PolyMesh, edge_key};
use crate::subsurf;
use crate::triangulate;
use crate::view::Obj;
use glam::{Mat3, Mat4, Vec2, Vec3};
use std::collections::{HashMap, HashSet};

/// Copies an Array modifier may make.
const MAX_ARRAY_COUNT: usize = 10_000;

/// What happened to a stack.
#[derive(Default)]
pub struct Report {
    /// Modifiers applied, by their names in Blender's interface.
    pub applied: Vec<String>,
    /// Modifiers enabled but not applied.
    pub skipped: Vec<String>,
    pub warnings: Vec<String>,
}

/// Applies the modifiers of object `ob` to `poly`.  Subdivision stops
/// short of making more than `budget` triangles.
pub fn apply(f: &BlendFile, ob: &Obj, poly: &mut PolyMesh, budget: u64, weights: &Weights) -> Report {
    let mut report = Report::default();
    let slots = ob.int(&["totcol"]).unwrap_or(1).max(1) as usize;
    let mut first_subsurf = true;
    // Vertex weights follow the mesh's own vertices, so a deform by them
    // must come before anything that adds or merges vertices.
    let original_vertices = poly.positions.len();
    for md in ob.list("modifiers").into_iter().take(256) {
        // The common part (name, type, mode) is an embedded ModifierData.
        let common = md.sub("modifier").unwrap_or(md);
        let mode = common.int(&["mode"]).unwrap_or(1);
        if mode & 1 == 0 {
            continue;
        }
        let kind = md.type_name();
        let name = ui_name(kind);
        let done = match kind {
            "ArmatureModifierData" => poly.positions.len() == original_vertices && armature(ob, &md, poly, weights),
            "MirrorModifierData" => mirror(f, ob, &md, poly),
            "ArrayModifierData" => array(f, ob, &md, poly, budget, &mut report.warnings),
            "SubsurfModifierData" => {
                // Fields a file predates read as zero, as Blender reads them:
                // smooth boundary corners and the limit surface.  Before 2.8
                // only the first subdivision in a stack used creases.
                let flags = md.int(&["flags"]).unwrap_or(0);
                let use_creases = if f.header.version < 281 { first_subsurf } else { flags & (1 << 4) != 0 };
                first_subsurf = false;
                let opts = subsurf::Options {
                    simple: md.int(&["subdivType"]).unwrap_or(0) == 1,
                    keep_corners: md.int(&["boundary_smooth"]).unwrap_or(0) == 1,
                    use_creases,
                    limit: flags & (1 << 6) == 0,
                };
                let levels = md.int(&["levels"]).unwrap_or(1).clamp(0, 6) as u32;
                subdivide(poly, levels, opts, budget, &name, &mut report.warnings)
            }
            "MultiresModifierData" => {
                // The sculpted detail lives in displacements Slerp doesn't
                // read; the smoothed base mesh is the closer stand-in.
                let levels = md.int(&["lvl"]).unwrap_or(1).clamp(0, 2) as u32;
                let opts = subsurf::Options { simple: false, keep_corners: false, use_creases: true, limit: true };
                report
                    .warnings
                    .push("Multiresolution: only the smoothed base mesh is shown, not the sculpted detail".into());
                subdivide(poly, levels, opts, budget, &name, &mut report.warnings)
            }
            "SolidifyModifierData" => solidify(&md, poly, slots),
            "TriangulateModifierData" => triangulate(&md, poly),
            "WeldModifierData" => {
                let dist = md.float(&["merge_dist"]).unwrap_or(0.001).max(0.0);
                let connected = md.int(&["mode"]).unwrap_or(0) == 1;
                weld(poly, dist, connected);
                true
            }
            "EdgeSplitModifierData" => {
                let flags = md.int(&["flags"]).unwrap_or(1 << 1 | 1 << 2);
                if flags & (1 << 1) != 0 {
                    normals::mark_sharp_by_angle(
                        poly,
                        md.float(&["split_angle"]).unwrap_or(std::f32::consts::FRAC_PI_6),
                    );
                }
                true
            }
            "NodesModifierData" => match smooth_by_angle(&md) {
                Some((angle, ignore_sharp)) => {
                    // The asset keeps marked sharp edges and the outlines of
                    // flat faces sharp (unless told to ignore sharpness),
                    // then shades every face smooth.
                    if ignore_sharp {
                        poly.sharp_edges.clear();
                    } else {
                        mark_flat_outlines(poly);
                    }
                    poly.face_smooth.iter_mut().for_each(|s| *s = true);
                    normals::mark_sharp_by_angle(poly, angle);
                    true
                }
                None => false,
            },
            _ => false,
        };
        if done {
            report.applied.push(name);
        } else {
            report.skipped.push(name);
        }
    }
    report
}

/// A mesh's vertex group weights and names, for the Armature modifier.
pub struct Weights<'w> {
    pub per_vertex: &'w [Vec<(u16, f32)>],
    pub groups: &'w [String],
}

/// Poses the mesh as its armature was posed when the file was saved: each
/// vertex moved by its groups' bones (by name), blended by weight.
fn armature(ob: &Obj, md: &Obj, poly: &mut PolyMesh, weights: &Weights) -> bool {
    let Some(arm) = md.deref("object").filter(|a| a.type_name() == "Object") else { return false };
    if md.int(&["deformflag"]).unwrap_or(1) & 1 == 0 || weights.per_vertex.is_empty() || weights.groups.is_empty() {
        return false;
    }
    let Some(pose) = arm.deref("pose") else { return false };
    // 1. Each bone's move from rest to pose, in the mesh object's space.
    let to_arm = crate::scene::world_of(&arm).inverse() * crate::scene::world_of(ob);
    let from_arm = to_arm.inverse();
    let mut by_name: HashMap<String, Mat4> = HashMap::new();
    for ch in pose.list("chanbase").into_iter().take(10_000) {
        let Some(bone) = ch.deref("bone") else { continue };
        if bone.int(&["flag"]).unwrap_or(0) & (1 << 12) != 0 {
            continue;
        }
        let m = |o: &Obj, n: &str| o.floats::<16>(&[n]).map(|v| Mat4::from_cols_array(&v)).filter(|m| m.is_finite());
        let (Some(posed), Some(rest)) = (m(&ch, "pose_mat"), m(&bone, "arm_mat")) else { continue };
        if rest.determinant().abs() < 1e-12 {
            continue;
        }
        by_name.insert(ch.string("name"), from_arm * posed * rest.inverse() * to_arm);
    }
    let bones: Vec<Option<Mat4>> = weights.groups.iter().map(|g| by_name.get(g).copied()).collect();
    if bones.iter().all(Option::is_none) {
        return false;
    }
    // 2. Every vertex, blended linearly by its weights.
    let mut moved = vec![None; poly.positions.len()];
    for (v, p) in poly.positions.iter_mut().enumerate() {
        let Some(ws) = weights.per_vertex.get(v) else { continue };
        let (mut sum, mut total, mut blend) = (Vec3::ZERO, 0.0, Mat3::ZERO);
        for &(g, w) in ws {
            if let Some(Some(m)) = bones.get(g as usize) {
                sum += m.transform_point3(*p) * w;
                blend += Mat3::from_mat4(*m) * w;
                total += w;
            }
        }
        if total > 0.0 {
            *p = sum / total;
            moved[v] = Some(blend * (1.0 / total));
        }
    }
    // Custom normals turn with their vertices.
    if let Some(nrm) = &mut poly.custom_normals {
        for (c, n) in nrm.iter_mut().enumerate() {
            if let Some(Some(m)) = poly.corner_verts.get(c).map(|&v| moved[v as usize]) {
                *n = (m.inverse().transpose() * *n).normalize_or_zero();
            }
        }
    }
    true
}

/// Marks every edge of a flat-shaded face sharp.
fn mark_flat_outlines(poly: &mut PolyMesh) {
    for f in 0..poly.face_count() {
        if poly.face_smooth.get(f).copied().unwrap_or(true) {
            continue;
        }
        let r = poly.face(f);
        for c in r.clone() {
            let next = if c + 1 == r.end { r.start } else { c + 1 };
            poly.sharp_edges.insert(edge_key(poly.corner_verts[c], poly.corner_verts[next]));
        }
    }
}

/// The name Blender's interface gives a modifier structure.
fn ui_name(kind: &str) -> String {
    let base = kind.strip_suffix("ModifierData").unwrap_or(kind);
    match base {
        "Subsurf" => "Subdivision Surface",
        "Multires" => "Multiresolution",
        "Nodes" => "Geometry Nodes",
        "EdgeSplit" => "Edge Split",
        "WeightedNormal" => "Weighted Normal",
        "NormalEdit" => "Normal Edit",
        "DataTransfer" => "Data Transfer",
        "ParticleSystem" => "Particle System",
        "SimpleDeform" => "Simple Deform",
        "CorrectiveSmooth" => "Corrective Smooth",
        "LaplacianSmooth" => "Laplacian Smooth",
        "MeshSequenceCache" => "Mesh Sequence Cache",
        "UVProject" => "UV Project",
        "UVWarp" => "UV Warp",
        other => other,
    }
    .to_string()
}

/*┌─────────────────╮
| Merging         |
└─────────────────╯*/
//region Merging

/// Merges vertices: `target[v]` is the vertex `v` becomes.  Faces lose
/// repeated corners; faces left with fewer than three, and faces repeating
/// an earlier face's vertices, go; unused vertices go.
pub fn merge(poly: &mut PolyMesh, target: &[u32]) {
    // 1. Faces through the map.
    let mut keep = vec![true; poly.face_count()];
    let mut seen_faces: HashSet<Vec<u32>> = HashSet::new();
    let mut new_corners: Vec<u32> = Vec::with_capacity(poly.corner_verts.len());
    let mut corner_src: Vec<usize> = Vec::with_capacity(poly.corner_verts.len());
    let mut starts = vec![0u32];
    let mut materials = Vec::new();
    let mut smooth = Vec::new();
    for f in 0..poly.face_count() {
        let r = poly.face(f);
        let mut ring: Vec<(u32, usize)> = Vec::with_capacity(r.len());
        for c in r.clone() {
            let v = target[poly.corner_verts[c] as usize];
            if ring.last().is_none_or(|l| l.0 != v) {
                ring.push((v, c));
            }
        }
        while ring.len() > 1 && ring.first().map(|x| x.0) == ring.last().map(|x| x.0) {
            ring.pop();
        }
        let mut sorted: Vec<u32> = ring.iter().map(|x| x.0).collect();
        sorted.sort_unstable();
        let distinct = sorted.windows(2).all(|w| w[0] != w[1]);
        if ring.len() < 3 || !distinct || !seen_faces.insert(sorted) {
            keep[f] = false;
            continue;
        }
        for (v, c) in ring {
            new_corners.push(v);
            corner_src.push(c);
        }
        starts.push(new_corners.len() as u32);
        materials.push(poly.face_material.get(f).copied().unwrap_or(0));
        smooth.push(poly.face_smooth.get(f).copied().unwrap_or(true));
    }
    for l in &mut poly.uvs {
        l.values = corner_src.iter().map(|&c| l.values[c]).collect();
    }
    for l in &mut poly.colors {
        l.values = corner_src.iter().map(|&c| l.values[c]).collect();
    }
    if let Some(n) = &mut poly.custom_normals {
        *n = corner_src.iter().map(|&c| n[c]).collect();
    }
    // 2. Drop unused vertices.
    let mut used = vec![u32::MAX; poly.positions.len()];
    let mut positions = Vec::new();
    for v in &mut new_corners {
        if used[*v as usize] == u32::MAX {
            used[*v as usize] = positions.len() as u32;
            positions.push(poly.positions[*v as usize]);
        }
        *v = used[*v as usize];
    }
    let remap = |a: u32| used.get(target[a as usize] as usize).copied().filter(|&x| x != u32::MAX);
    poly.sharp_edges = poly
        .sharp_edges
        .iter()
        .filter_map(|&(a, b)| Some(edge_key(remap(a)?, remap(b)?)))
        .filter(|(a, b)| a != b)
        .collect();
    poly.creases = poly
        .creases
        .iter()
        .filter_map(|(&(a, b), &w)| Some((edge_key(remap(a)?, remap(b)?), w)))
        .filter(|((a, b), _)| a != b)
        .collect();
    poly.positions = positions;
    poly.corner_verts = new_corners;
    poly.face_starts = starts;
    poly.face_material = materials;
    poly.face_smooth = smooth;
}

/// Follows merge targets to their end, so chains collapse to one vertex.
fn resolve(target: &mut [u32]) {
    for v in 0..target.len() {
        let mut t = target[v];
        let mut steps = 0;
        while target[t as usize] != t && steps < 64 {
            t = target[t as usize];
            steps += 1;
        }
        target[v] = t;
    }
}

//endregion

/*┌─────────────────╮
| Modifiers       |
└─────────────────╯*/
//region Modifiers

fn mirror(_f: &BlendFile, ob: &Obj, md: &Obj, poly: &mut PolyMesh) -> bool {
    let flag = md.int(&["flag"]).unwrap_or(0);
    let mut axes: Vec<usize> = (0..3).filter(|a| flag & (1 << (3 + a)) != 0).collect();
    if axes.is_empty() {
        axes.push(md.int(&["axis"]).unwrap_or(0).clamp(0, 2) as usize);
    }
    let tolerance = md.float(&["tolerance"]).unwrap_or(0.001).max(0.0);
    let merge_on = flag & (1 << 7) == 0;
    let uv_offset = md.floats::<2>(&["uv_offset"]).map(Vec2::from).unwrap_or(Vec2::ZERO);
    let uv_copy = md.floats::<2>(&["uv_offset_copy"]).map(Vec2::from).unwrap_or(Vec2::ZERO);
    // Mirroring about another object: into its space and back.
    let space = md
        .deref("mirror_ob")
        .filter(|m| m.type_name() == "Object")
        .map(|m| crate::scene::world_of(&m).inverse() * crate::scene::world_of(ob));
    for axis in axes {
        let mut s = Mat4::IDENTITY;
        s.col_mut(axis)[axis] = -1.0;
        let mtx = match space {
            Some(t) => t.inverse() * s * t,
            None => s,
        };
        let normal_mtx = Mat3::from_mat4(mtx).inverse().transpose();
        let n = poly.positions.len();
        let mut copy = poly.clone();
        for p in &mut copy.positions {
            *p = mtx.transform_point3(*p);
        }
        if let Some(nrm) = &mut copy.custom_normals {
            for v in nrm.iter_mut() {
                *v = (normal_mtx * *v).normalize_or_zero();
            }
        }
        for l in &mut copy.uvs {
            for uv in &mut l.values {
                if flag & (1 << 1) != 0 {
                    uv.x = 1.0 - uv.x + uv_offset.x;
                }
                if flag & (1 << 2) != 0 {
                    uv.y = 1.0 - uv.y + uv_offset.y;
                }
                *uv += uv_copy;
            }
        }
        copy.flip_faces(false);
        // Vertices that land within the tolerance of their own mirror image
        // meet at the midpoint and merge.
        let mut target: Vec<u32> = (0..2 * n as u32).collect();
        if merge_on {
            for i in 0..n {
                let (a, b) = (poly.positions[i], copy.positions[i]);
                if a.distance_squared(b) < tolerance * tolerance {
                    let mid = (a + b) * 0.5;
                    poly.positions[i] = mid;
                    copy.positions[i] = mid;
                    target[n + i] = i as u32;
                }
            }
        }
        poly.append(&copy);
        if merge_on {
            merge(poly, &target);
        }
    }
    true
}

fn array(_f: &BlendFile, ob: &Obj, md: &Obj, poly: &mut PolyMesh, budget: u64, warnings: &mut Vec<String>) -> bool {
    if md.ptr("start_cap") != 0 || md.ptr("end_cap") != 0 {
        warnings.push("Array: start and end caps are not shown".into());
    }
    // 1. The step between copies: constant, relative to the mesh's size,
    //    and through an offset object.
    let offset_type = md.int(&["offset_type"]).unwrap_or(1 << 1);
    let mut step = Mat4::IDENTITY;
    let mut translate = Vec3::ZERO;
    if offset_type & 1 != 0 {
        translate += md.floats::<3>(&["offset"]).map(Vec3::from).unwrap_or(Vec3::ZERO);
    }
    if offset_type & (1 << 1) != 0 && !poly.positions.is_empty() {
        let (lo, hi) = poly
            .positions
            .iter()
            .fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), &p| (lo.min(p), hi.max(p)));
        let scale = md.floats::<3>(&["scale"]).map(Vec3::from).unwrap_or(Vec3::X);
        translate += scale * (hi - lo);
    }
    step *= Mat4::from_translation(translate);
    if offset_type & (1 << 2) != 0
        && let Some(o) = md.deref("offset_ob").filter(|o| o.type_name() == "Object")
    {
        step = step * crate::scene::world_of(ob).inverse() * crate::scene::world_of(&o);
    }
    if !step.is_finite() {
        return false;
    }
    // 2. How many copies.
    let mut count = md.int(&["count"]).unwrap_or(2).max(1) as usize;
    if md.int(&["fit_type"]).unwrap_or(0) == 1 {
        let dist = step.w_axis.truncate().length();
        let length = md.float(&["length"]).unwrap_or(0.0);
        if dist > 1e-6 {
            count = ((length / dist) as usize).saturating_add(1);
        }
    }
    let per_copy = poly.triangle_count().max(1) as u64;
    let max_copies = (budget / per_copy).max(1) as usize;
    if count > max_copies.min(MAX_ARRAY_COUNT) {
        warnings.push(format!("Array: {count} copies cut to {}", max_copies.min(MAX_ARRAY_COUNT)));
        count = max_copies.min(MAX_ARRAY_COUNT);
    }
    // 3. The copies, merged where adjacent ones touch.
    let base = poly.clone();
    let n = base.positions.len();
    let uv_offset = md.floats::<2>(&["uv_offset"]).map(Vec2::from).unwrap_or(Vec2::ZERO);
    let normal_step = Mat3::from_mat4(step).inverse().transpose();
    let mut m = Mat4::IDENTITY;
    let mut nm = Mat3::IDENTITY;
    for k in 1..count {
        m *= step;
        nm *= normal_step;
        let mut copy = base.clone();
        for p in &mut copy.positions {
            *p = m.transform_point3(*p);
        }
        if let Some(nrm) = &mut copy.custom_normals {
            for v in nrm.iter_mut() {
                *v = (nm * *v).normalize_or_zero();
            }
        }
        for l in &mut copy.uvs {
            for uv in &mut l.values {
                *uv += uv_offset * k as f32;
            }
        }
        poly.append(&copy);
    }
    let flags = md.int(&["flags"]).unwrap_or(0);
    if flags & 1 != 0 && count > 1 {
        let dist = md.float(&["merge_dist"]).unwrap_or(0.01).max(0.0);
        let mut target: Vec<u32> = (0..poly.positions.len() as u32).collect();
        let pairs = (1..count).map(|k| (k - 1, k)).chain((flags & 2 != 0 && count > 2).then_some((count - 1, 0)));
        for (a, b) in pairs {
            // Vertices of copy b that touch a vertex of copy a merge into
            // it.
            let grid = Grid::new(&poly.positions[a * n..(a + 1) * n], dist);
            for i in 0..n {
                let p = poly.positions[b * n + i];
                if let Some(j) = grid.nearest(p, dist) {
                    target[b * n + i] = (a * n + j) as u32;
                }
            }
        }
        resolve(&mut target);
        merge(poly, &target);
    }
    true
}

fn subdivide(
    poly: &mut PolyMesh,
    levels: u32,
    opts: subsurf::Options,
    budget: u64,
    name: &str,
    warnings: &mut Vec<String>,
) -> bool {
    // After the first level every face is a quad per original corner, and
    // each level after that multiplies quads by four.
    let corners = poly.corner_verts.len() as u64;
    let mut levels = levels;
    while levels > 0 && 2 * corners * 4u64.saturating_pow(levels - 1) > budget {
        levels -= 1;
    }
    if levels == 0 && corners > 0 && budget < 2 * corners {
        warnings.push(format!("{name}: not applied, the mesh is too large"));
        return false;
    }
    subsurf::subdivide(poly, levels, opts);
    true
}

fn solidify(md: &Obj, poly: &mut PolyMesh, slots: usize) -> bool {
    let thickness = md.float(&["offset"]).unwrap_or(0.01);
    let fac = md.float(&["offset_fac"]).unwrap_or(-1.0);
    let flag = md.int(&["flag"]).unwrap_or(1);
    let rim = flag & 1 != 0;
    let flip = flag & (1 << 5) != 0;
    let shell = !(rim && flag & (1 << 6) != 0);
    let max_slot = slots.saturating_sub(1) as i64;
    let mat_ofs = md.int(&["mat_ofs"]).unwrap_or(0);
    let mat_ofs_rim = md.int(&["mat_ofs_rim"]).unwrap_or(0);
    let slot = |m: u16, ofs: i64| (m as i64 + ofs).clamp(0, max_slot.max(m as i64)) as u16;
    // 1. Offsets as Blender's simple mode computes them.
    let ofs_orig = -(((-fac + 1.0) * 0.5) * thickness);
    let ofs_new = thickness + ofs_orig;
    let swap = (ofs_new >= ofs_orig) == flip;
    let (orig_ofs, copy_ofs) = if swap { (ofs_orig, ofs_new) } else { (ofs_new, ofs_orig) };
    // 2. Smooth vertex normals, angle weighted over every face.
    let face_normals = poly.face_normals();
    let mut vn = vec![Vec3::ZERO; poly.positions.len()];
    for f in 0..poly.face_count() {
        let r = poly.face(f);
        for c in r.clone() {
            let prev = if c == r.start { r.end - 1 } else { c - 1 };
            let next = if c + 1 == r.end { r.start } else { c + 1 };
            let p = poly.positions[poly.corner_verts[c] as usize];
            let a = (poly.positions[poly.corner_verts[prev] as usize] - p).normalize_or_zero();
            let b = (poly.positions[poly.corner_verts[next] as usize] - p).normalize_or_zero();
            vn[poly.corner_verts[c] as usize] += face_normals[f] * a.dot(b).clamp(-1.0, 1.0).acos();
        }
    }
    for v in &mut vn {
        *v = v.normalize_or_zero();
    }
    // 3. Boundary edges, for the rim, in their faces' direction.
    // Each edge's vertices in face order, its face and its two corners.
    type Use = (u32, u32, usize, usize, usize);
    let mut uses: HashMap<(u32, u32), Use> = HashMap::new();
    let mut count: HashMap<(u32, u32), u32> = HashMap::new();
    for f in 0..poly.face_count() {
        let r = poly.face(f);
        for c in r.clone() {
            let next = if c + 1 == r.end { r.start } else { c + 1 };
            let (a, b) = (poly.corner_verts[c], poly.corner_verts[next]);
            *count.entry(edge_key(a, b)).or_default() += 1;
            uses.insert(edge_key(a, b), (a, b, f, c, next));
        }
    }
    // 4. The shell: a flipped copy, both sides moved along the normals.
    let n = poly.positions.len();
    let mut copy = poly.clone();
    for (i, p) in poly.positions.iter_mut().enumerate() {
        *p += vn[i] * orig_ofs;
    }
    for (i, p) in copy.positions.iter_mut().enumerate() {
        *p += vn[i] * copy_ofs;
    }
    for m in &mut copy.face_material {
        *m = slot(*m, mat_ofs);
    }
    copy.flip_faces(true);
    let base_faces = poly.face_count();
    let rim_edges: Vec<(u32, u32, usize, usize, usize)> =
        count.iter().filter(|&(_, &c)| c == 1).filter_map(|(k, _)| uses.get(k).copied()).collect();
    if shell {
        poly.append(&copy);
    } else {
        // Without the shell, keep the copy's vertices for the rim only.
        poly.positions.extend_from_slice(&copy.positions);
    }
    // 5. Rim quads joining each boundary edge to its copy.
    if rim {
        let n32 = n as u32;
        for (a, b, f, ca, cb) in rim_edges {
            let start = poly.corner_verts.len();
            poly.corner_verts.extend([b, a, a + n32, b + n32]);
            poly.face_starts.push(poly.corner_verts.len() as u32);
            poly.face_material.push(slot(poly.face_material.get(f).copied().unwrap_or(0), mat_ofs_rim));
            poly.face_smooth.push(poly.face_smooth.get(f).copied().unwrap_or(true));
            for l in &mut poly.uvs {
                let (ua, ub) = (l.values[ca], l.values[cb]);
                l.values.extend([ub, ua, ua, ub]);
            }
            for l in &mut poly.colors {
                let (ua, ub) = (l.values[ca], l.values[cb]);
                l.values.extend([ub, ua, ua, ub]);
            }
            if let Some(nrm) = &mut poly.custom_normals {
                nrm.resize(start + 4, Vec3::ZERO);
            }
        }
    }
    let _ = base_faces;
    // Rim faces get computed normals; any custom normals elsewhere stay.
    if let Some(nrm) = &mut poly.custom_normals
        && nrm.contains(&Vec3::ZERO)
    {
        poly.custom_normals = None;
    }
    true
}

fn triangulate(md: &Obj, poly: &mut PolyMesh) -> bool {
    let min = md.int(&["min_vertices"]).unwrap_or(4).max(4) as usize;
    let quad_method = md.int(&["quad_method"]).unwrap_or(0);
    let normals = poly.face_normals();
    let mut out = poly.empty_like();
    out.positions = poly.positions.clone();
    out.sharp_edges = poly.sharp_edges.clone();
    out.creases = poly.creases.clone();
    let mut tris = Vec::new();
    let mut budget = triangulate::EarBudget::new();
    for f in 0..poly.face_count() {
        let r = poly.face(f);
        let n = r.len();
        tris.clear();
        if n < min {
            tris.push((0..n).collect::<Vec<usize>>());
        } else if n == 4 {
            let p = |k: usize| poly.positions[poly.corner_verts[r.start + k] as usize];
            let d02 = p(0).distance(p(2));
            let d13 = p(1).distance(p(3));
            let split13 = match quad_method {
                1 => false,
                2 => true,
                4 => d13 > d02,
                _ => d13 < d02,
            };
            if split13 {
                tris.extend([vec![0, 1, 3], vec![1, 2, 3]]);
            } else {
                tris.extend([vec![0, 1, 2], vec![0, 2, 3]]);
            }
        } else {
            let mut t = Vec::new();
            triangulate::face_triangles(poly, f, normals[f], &mut t, &mut budget);
            tris.extend(t.into_iter().map(|t| t.to_vec()));
        }
        for t in &tris {
            for &k in t {
                let c = r.start + k;
                out.corner_verts.push(poly.corner_verts[c]);
                for (o, l) in out.uvs.iter_mut().zip(&poly.uvs) {
                    o.values.push(l.values[c]);
                }
                for (o, l) in out.colors.iter_mut().zip(&poly.colors) {
                    o.values.push(l.values[c]);
                }
                if let (Some(o), Some(nrm)) = (&mut out.custom_normals, &poly.custom_normals) {
                    o.push(nrm[c]);
                }
            }
            out.face_starts.push(out.corner_verts.len() as u32);
            out.face_material.push(poly.face_material.get(f).copied().unwrap_or(0));
            out.face_smooth.push(poly.face_smooth.get(f).copied().unwrap_or(true));
        }
    }
    *poly = out;
    true
}

/// Merges vertices closer than `dist`: all of them, or only along edges
/// (Blender's "Connected" mode).
pub fn weld(poly: &mut PolyMesh, dist: f32, connected: bool) {
    let n = poly.positions.len();
    let mut target: Vec<u32> = (0..n as u32).collect();
    fn find(t: &mut [u32], mut v: u32) -> u32 {
        while t[v as usize] != v {
            t[v as usize] = t[t[v as usize] as usize];
            v = t[v as usize];
        }
        v
    }
    fn union(t: &mut [u32], a: u32, b: u32) {
        let (ra, rb) = (find(t, a), find(t, b));
        // The lower index survives, as in Blender.
        if ra < rb {
            t[rb as usize] = ra;
        } else if rb < ra {
            t[ra as usize] = rb;
        }
    }
    if connected {
        for f in 0..poly.face_count() {
            let r = poly.face(f);
            for c in r.clone() {
                let next = if c + 1 == r.end { r.start } else { c + 1 };
                let (a, b) = (poly.corner_verts[c], poly.corner_verts[next]);
                if poly.positions[a as usize].distance(poly.positions[b as usize]) <= dist {
                    union(&mut target, a, b);
                }
            }
        }
    } else {
        let grid = Grid::new(&poly.positions, dist);
        for i in 0..n {
            for j in grid.within(poly.positions[i], dist) {
                if j != i {
                    union(&mut target, i as u32, j as u32);
                }
            }
        }
    }
    for v in 0..n as u32 {
        let r = find(&mut target, v);
        target[v as usize] = r;
    }
    merge(poly, &target);
}

/// The "Smooth by Angle" geometry nodes asset that Blender 4.1 and later
/// add for Shade Auto Smooth: its angle and whether it ignores marked
/// sharp edges.  None for any other node group.
fn smooth_by_angle(md: &Obj) -> Option<(f32, bool)> {
    let group = md.deref("node_group")?;
    if !group.id_name().starts_with("Smooth by Angle") {
        return None;
    }
    let mut angle = 30f32.to_radians();
    let mut ignore = false;
    let props = md.sub("settings").and_then(|s| s.deref("properties"));
    for p in props.map(|p| p.sub("data").map(|d| d.list("group")).unwrap_or_default()).unwrap_or_default() {
        let name = p.string("name");
        if name.ends_with("_use_attribute") || name.ends_with("_attribute_name") {
            continue;
        }
        let data = p.sub("data");
        match p.int(&["type"]) {
            // IDP_FLOAT keeps its bits in `val`; IDP_DOUBLE in `val` and
            // `val2`.
            Some(2) => {
                if let Some(v) = data.and_then(|d| d.int(&["val"])) {
                    angle = f32::from_bits(v as u32);
                }
            }
            Some(8) => {
                if let Some(d) = data {
                    let lo = d.int(&["val"]).unwrap_or(0) as u32 as u64;
                    let hi = d.int(&["val2"]).unwrap_or(0) as u32 as u64;
                    let bits = if md.f.header.big_endian { lo << 32 | hi } else { hi << 32 | lo };
                    angle = f64::from_bits(bits) as f32;
                }
            }
            // IDP_INT or IDP_BOOLEAN: Ignore Sharpness.
            Some(1) | Some(13) => ignore = data.and_then(|d| d.int(&["val"])).unwrap_or(0) != 0,
            _ => {}
        }
    }
    angle.is_finite().then_some((angle, ignore))
}

//endregion

/// A uniform grid over points for neighbour queries within a distance.
struct Grid {
    cell: f32,
    map: HashMap<(i32, i32, i32), Vec<u32>>,
    points: Vec<Vec3>,
}

impl Grid {
    fn new(points: &[Vec3], dist: f32) -> Grid {
        let cell = dist.max(1e-6);
        let mut map: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::new();
        for (i, p) in points.iter().enumerate() {
            map.entry(Self::key(*p, cell)).or_default().push(i as u32);
        }
        Grid { cell, map, points: points.to_vec() }
    }

    fn key(p: Vec3, cell: f32) -> (i32, i32, i32) {
        let q = (p / cell).floor();
        (q.x as i32, q.y as i32, q.z as i32)
    }

    fn within(&self, p: Vec3, dist: f32) -> Vec<usize> {
        let (x, y, z) = Self::key(p, self.cell);
        let mut out = Vec::new();
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if let Some(list) = self.map.get(&(x + dx, y + dy, z + dz)) {
                        out.extend(list.iter().map(|&i| i as usize).filter(|&i| self.points[i].distance(p) <= dist));
                    }
                }
            }
        }
        out
    }

    fn nearest(&self, p: Vec3, dist: f32) -> Option<usize> {
        self.within(p, dist)
            .into_iter()
            .min_by(|&a, &b| self.points[a].distance_squared(p).total_cmp(&self.points[b].distance_squared(p)))
    }
}
