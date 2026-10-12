//! Prints what the reader makes of SketchUp files: counts, bounds, timing
//! and warnings, for comparing with OpenSKP.

use slerp_scene::{Limits, Pose};
use std::time::Instant;

fn main() {
    for path in std::env::args().skip(1) {
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) => {
                println!("{path}: {e}");
                continue;
            }
        };
        let name = path.rsplit('/').next().unwrap_or(&path).to_string();
        let t = Instant::now();
        let model = slerp_sketchup::read(&data, &Limits::viewer());
        let read_ms = t.elapsed().as_secs_f64() * 1000.0;
        let model = match model {
            Ok(m) => m,
            Err(e) => {
                println!("{name:45} ERROR {e}");
                continue;
            }
        };
        let (faces, edges, inst) = model.counts();
        let t = Instant::now();
        let scene = slerp_sketchup::load(&data, &Limits::viewer()).expect("loaded once");
        let all_ms = t.elapsed().as_secs_f64() * 1000.0;
        let pose = Pose::rest(&scene);
        let b = visible_bounds(&scene, &pose, false);
        let all = visible_bounds(&scene, &pose, true);
        println!(
            "{name:45} {:12} defs={:4} faces={:6} edges={:6} inst={:5} mats={:3} layers={:3} tris={:8} nodes={:6} meshes={:5} bounds=[{:.3} {:.3} {:.3}]..[{:.3} {:.3} {:.3}] read={read_ms:.1}ms all={all_ms:.1}ms",
            model.version,
            model.definitions.len(),
            faces,
            edges,
            inst,
            model.materials.len(),
            model.layers.len(),
            scene.triangle_count(),
            scene.nodes.len(),
            scene.meshes.len(),
            b.min.x,
            b.min.y,
            b.min.z,
            b.max.x,
            b.max.y,
            b.max.z
        );
        println!(
            "    everything: [{:.3} {:.3} {:.3}]..[{:.3} {:.3} {:.3}]  placed face triangles {}",
            all.min.x,
            all.min.y,
            all.min.z,
            all.max.x,
            all.max.y,
            all.max.z,
            placed(&scene, false)
        );
        for w in &scene.info.warnings {
            println!("    warning: {w}");
        }
        if std::env::var_os("SKP_DETAILS").is_some() {
            for (k, v) in &scene.info.details {
                println!("    {k}: {v}");
            }
            for m in &model.materials {
                println!(
                    "    material {:30} {:?} opacity={} texture={:?} colorized={}",
                    m.name,
                    m.color,
                    m.opacity,
                    m.texture.as_ref().map(|t| (t.bytes.len(), t.width, t.height, t.file_name.clone())),
                    m.colorized
                );
            }
            if std::env::var_os("SKP_FACES").is_some() {
                for (di, d) in model.definitions.iter().chain(std::iter::once(&model.root)).enumerate() {
                    for f in &d.faces {
                        let verts: Vec<_> = f.loops[0]
                            .iter()
                            .map(|c| {
                                let e = d.edges[c.edge as usize];
                                d.vertices[if c.reversed { e.b } else { e.a } as usize]
                            })
                            .collect();
                        println!(
                            "    def {di} face n={:?} front={:?} back={:?} mapping={:?} verts={:?}",
                            f.normal, f.front, f.back, f.mapping, verts
                        );
                    }
                    for i in &d.instances {
                        println!(
                            "    def {di} instance of {} matrix={:?} material={:?}",
                            i.definition, i.matrix, i.material
                        );
                    }
                }
            }
            for l in &model.layers {
                println!("    tag {:30} hidden={}", l.name, l.hidden);
            }
        }
    }
}

/// Bounds of what shows (each node and its ancestors visible), or of
/// everything.
fn visible_bounds(scene: &slerp_scene::Scene, pose: &Pose, everything: bool) -> slerp_scene::Aabb {
    let parents = scene.parents();
    let mut out = slerp_scene::Aabb::EMPTY;
    for (i, node) in scene.nodes.iter().enumerate() {
        let mut shown = everything || node.visible;
        let mut p = parents[i];
        while let Some(q) = p {
            shown &= everything || scene.nodes[q].visible;
            p = parents[q];
        }
        let (Some(m), true) = (node.mesh, shown) else { continue };
        for prim in &scene.meshes[m].primitives {
            if prim.topology != slerp_scene::Topology::Triangles {
                continue;
            }
            for &v in &prim.positions {
                out.add(pose.world[i].transform_point3(v));
            }
        }
    }
    out
}

/// Triangles drawn, counting every placement.
fn placed(scene: &slerp_scene::Scene, _hidden: bool) -> usize {
    scene
        .nodes
        .iter()
        .filter_map(|n| n.mesh)
        .map(|m| scene.meshes[m].primitives.iter().map(|p| p.triangle_count()).sum::<usize>())
        .sum()
}
