//! Compares slerp-blend's reading of `.blend` files with Blender's own glTF
//! export of them (made by `tools/blender-reference.py`): triangle counts,
//! bounds, how far the surfaces lie from each other and how far normals
//! differ.  `blend-compare FILE.blend REF_BASE [FILE.blend REF_BASE ...]`.

use glam::{Mat4, Vec3};
use slerp_scene::{Asset, Limits, Pose, Scene};
use std::collections::HashMap;
use std::path::Path;

/// World-space vertices with normals of every visible mesh, and the
/// triangle count.
fn world_points(s: &Scene) -> (Vec<(Vec3, Vec3)>, usize) {
    world_points_named(s, None)
}

fn world_points_named(s: &Scene, name: Option<&str>) -> (Vec<(Vec3, Vec3)>, usize) {
    let pose = Pose::rest(s);
    let mut out = Vec::new();
    let mut tris = 0;
    for (i, n) in s.nodes.iter().enumerate() {
        let (Some(m), true) = (n.mesh, n.visible) else { continue };
        if name.is_some_and(|w| n.name != w && s.meshes[m].name != w) {
            continue;
        }
        let w: Mat4 = pose.world[i];
        let nm = glam::Mat3::from_mat4(w).inverse().transpose();
        for p in &s.meshes[m].primitives {
            tris += p.triangle_count();
            for (k, &v) in p.positions.iter().enumerate() {
                let normal = p.normals.get(k).map(|&x| (nm * x).normalize_or_zero()).unwrap_or(Vec3::ZERO);
                out.push((w.transform_point3(v), normal));
            }
        }
    }
    (out, tris)
}

struct Grid {
    cell: f32,
    map: HashMap<(i32, i32, i32), Vec<usize>>,
}

impl Grid {
    fn new(pts: &[(Vec3, Vec3)], cell: f32) -> Grid {
        let mut map: HashMap<(i32, i32, i32), Vec<usize>> = HashMap::new();
        for (i, (p, _)) in pts.iter().enumerate() {
            map.entry(key(*p, cell)).or_default().push(i);
        }
        Grid { cell, map }
    }

    /// Indices of points within the 27 cells around `p`, widening the
    /// search until something turns up (at most a few rings).
    fn near(&self, p: Vec3) -> Vec<usize> {
        let (x, y, z) = key(p, self.cell);
        for r in 1..=4 {
            let mut out = Vec::new();
            for dx in -r..=r {
                for dy in -r..=r {
                    for dz in -r..=r {
                        if let Some(l) = self.map.get(&(x + dx, y + dy, z + dz)) {
                            out.extend_from_slice(l);
                        }
                    }
                }
            }
            if !out.is_empty() {
                return out;
            }
        }
        Vec::new()
    }
}

fn key(p: Vec3, cell: f32) -> (i32, i32, i32) {
    let q = (p / cell).floor();
    (q.x as i32, q.y as i32, q.z as i32)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    for pair in args.chunks(2) {
        let [blend, base] = pair else { break };
        let bytes = std::fs::read(blend).expect("read .blend");
        let ours = match slerp_blend::load(Path::new(blend), &bytes, &Limits::viewer()) {
            Ok(s) => s,
            Err(e) => {
                println!("{blend}: ours failed: {e}");
                continue;
            }
        };
        let glb = format!("{base}.glb");
        let Ok(gbytes) = std::fs::read(&glb) else {
            println!("{blend}: no reference");
            continue;
        };
        let reference = match slerp_gltf::load(Path::new(&glb), &gbytes, &Limits::viewer()) {
            Ok(Asset::Scene(s)) => s,
            _ => {
                println!("{blend}: reference unreadable");
                continue;
            }
        };
        if std::env::var("PER_OBJECT").is_ok() {
            per_object(&ours, base);
        }
        if let Ok(name) = std::env::var("NORMALS_OF") {
            let (a, _) = world_points_named(&ours, Some(&name));
            let (b, _) = world_points_named(&reference, Some(&name));
            let cell = 0.01;
            let gb = Grid::new(&b, cell);
            let mut hist = [0usize; 5];
            for (p, n) in &a {
                let same: Vec<Vec3> =
                    gb.near(*p).iter().filter(|&&i| b[i].0.distance(*p) < 1e-4).map(|&i| b[i].1).collect();
                if same.is_empty() {
                    continue;
                }
                let best = same.iter().map(|m| m.dot(*n).clamp(-1.0, 1.0).acos().to_degrees()).fold(f32::MAX, f32::min);
                let k = if best < 1.0 {
                    0
                } else if best < 5.0 {
                    1
                } else if best < 20.0 {
                    2
                } else if best < 60.0 {
                    3
                } else {
                    4
                };
                hist[k] += 1;
            }
            println!(
                "  normals of {name}: <1 {} <5 {} <20 {} <60 {} more {} ({} vs {} points)",
                hist[0],
                hist[1],
                hist[2],
                hist[3],
                hist[4],
                a.len(),
                b.len()
            );
        }
        let (a, ta) = world_points(&ours);
        let (b, tb) = world_points(&reference);
        if a.is_empty() || b.is_empty() {
            println!("{blend}: tris {ta} vs {tb}, points {} vs {}", a.len(), b.len());
            continue;
        }
        let bounds = |pts: &[(Vec3, Vec3)]| {
            pts.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), (p, _)| (lo.min(*p), hi.max(*p)))
        };
        let (alo, ahi) = bounds(&a);
        let (blo, bhi) = bounds(&b);
        let diag = (bhi - blo).length().max(1e-6);
        let bdiff = (alo - blo).abs().max_element().max((ahi - bhi).abs().max_element()) / diag;
        // Distances from ours to the reference and back, sampled.
        let cell = diag / 200.0;
        let gb = Grid::new(&b, cell);
        let ga = Grid::new(&a, cell);
        let step = (a.len() / 50_000).max(1);
        let (mut sum, mut max, mut n, mut angle_sum, mut bad, mut nn) = (0f32, 0f32, 0, 0f32, 0, 0);
        for (p, normal) in a.iter().step_by(step) {
            let near = gb.near(*p);
            let d = near.iter().map(|&i| b[i].0.distance(*p)).fold(f32::MAX, f32::min);
            let d = if d == f32::MAX { cell * 4.0 } else { d };
            sum += d;
            max = max.max(d);
            n += 1;
            // Normals of reference vertices at the same place.
            let same: Vec<Vec3> =
                near.iter().filter(|&&i| b[i].0.distance(*p) < diag * 1e-4).map(|&i| b[i].1).collect();
            if !same.is_empty() && *normal != Vec3::ZERO {
                let best = same.iter().map(|m| m.dot(*normal).clamp(-1.0, 1.0).acos()).fold(f32::MAX, f32::min);
                angle_sum += best.to_degrees();
                nn += 1;
                if best.to_degrees() > 10.0 {
                    bad += 1;
                }
            }
        }
        let step_b = (b.len() / 50_000).max(1);
        let mut back_max = 0f32;
        for (p, _) in b.iter().step_by(step_b) {
            let d = ga.near(*p).iter().map(|&i| a[i].0.distance(*p)).fold(f32::MAX, f32::min);
            let d = if d == f32::MAX { cell * 4.0 } else { d };
            back_max = back_max.max(d);
        }
        println!(
            "{blend}: tris {ta} vs {tb} | bounds {:.4} | dist mean {:.5} max {:.4} back max {:.4} | normals {:.2} deg mean, {:.1}% over 10 deg ({nn} matched)",
            bdiff,
            sum / n as f32 / diag,
            max / diag,
            back_max / diag,
            if nn > 0 { angle_sum / nn as f32 } else { 0.0 },
            if nn > 0 { 100.0 * bad as f32 / nn as f32 } else { 0.0 },
        );
    }
}

/// Triangles and bounds per object: Blender's (from the reference JSON)
/// against ours (the nodes of that name).  Only differences are printed.
fn per_object(ours: &Scene, base: &str) {
    let Ok(text) = std::fs::read_to_string(format!("{base}.json")) else { return };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    type Entry = (u64, bool, Vec3, Vec3);
    let empty = || (0, false, Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    let mut theirs: std::collections::BTreeMap<String, Entry> = Default::default();
    for o in json["objects"].as_array().into_iter().flatten() {
        let name = o["name"].as_str().unwrap_or("").to_string();
        let shown = o["visible"].as_bool().unwrap_or(false) && o["render"].as_bool().unwrap_or(false);
        let v = |k: &str| {
            let a = o[k].as_array()?;
            let f = |i: usize| a.get(i).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
            // Blender's Z up to Y up.
            Some(Vec3::new(f(0), f(2), -f(1)))
        };
        let e = theirs.entry(name).or_insert_with(empty);
        e.0 += o["triangles"].as_u64().unwrap_or(0);
        e.1 |= shown;
        if let (Some(a), Some(b)) = (v("min"), v("max")) {
            e.2 = e.2.min(a.min(b));
            e.3 = e.3.max(a.max(b));
        }
    }
    let pose = Pose::rest(ours);
    let mut mine: std::collections::BTreeMap<String, Entry> = Default::default();
    for (i, n) in ours.nodes.iter().enumerate() {
        let e = mine.entry(n.name.clone()).or_insert_with(empty);
        e.1 |= n.visible;
        let Some(m) = n.mesh else { continue };
        for p in &ours.meshes[m].primitives {
            e.0 += p.triangle_count() as u64;
            for &v in &p.positions {
                let w = pose.world[i].transform_point3(v);
                e.2 = e.2.min(w);
                e.3 = e.3.max(w);
            }
        }
    }
    let mut shown = 0;
    for (name, t) in &theirs {
        let m = mine.get(name).cloned().unwrap_or_else(empty);
        let size = (t.3 - t.2).length().max(1e-6);
        let off = ((m.2 - t.2).abs().max_element().max((m.3 - t.3).abs().max_element())) / size;
        if t.0 == m.0 && t.1 == m.1 && (off < 0.01 || t.0 == 0) {
            continue;
        }
        shown += 1;
        if shown > 40 {
            continue;
        }
        println!("  {name:28} blender {:7} {:5} | ours {:7} {:5} | bounds off {off:.3}", t.0, t.1, m.0, m.1);
    }
    if shown > 40 {
        println!("  ... {} more", shown - 40);
    }
}
