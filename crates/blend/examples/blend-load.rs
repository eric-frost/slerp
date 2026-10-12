//! Loads `.blend` files as the viewer would and prints what they hold and
//! how long each took: `blend-load [--thumbnail] FILE...`.

use slerp_scene::Limits;
use std::time::Instant;

fn main() {
    let mut limits = Limits::viewer();
    let mut nodes = false;
    for arg in std::env::args().skip(1) {
        if arg == "--thumbnail" {
            limits = Limits::thumbnail();
            continue;
        }
        if arg == "--nodes" {
            nodes = true;
            continue;
        }
        let path = std::path::Path::new(&arg);
        let t = Instant::now();
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                println!("{arg}: {e}");
                continue;
            }
        };
        let preview = slerp_blend::preview(&bytes).ok().flatten().map(|p| (p.width(), p.height()));
        let t_preview = t.elapsed();
        match slerp_blend::load(path, &bytes, &limits) {
            Ok(s) => {
                let visible_tris: usize = visible_triangles(&s);
                println!(
                    "{arg}: {} | {} nodes, {} meshes, {} tris ({} visible), {} materials, {} textures, {} markers | preview {:?} in {:.1} ms | load {:.1} ms",
                    s.info.format,
                    s.nodes.len(),
                    s.meshes.len(),
                    s.triangle_count(),
                    visible_tris,
                    s.materials.len(),
                    s.textures.len(),
                    s.markers.len(),
                    preview,
                    t_preview.as_secs_f64() * 1000.0,
                    t.elapsed().as_secs_f64() * 1000.0
                );
                for (k, v) in &s.info.details {
                    println!("    {k}: {v}");
                }
                for w in &s.info.warnings {
                    println!("    warning: {w}");
                }
                if nodes {
                    for (i, n) in s.nodes.iter().enumerate() {
                        let t = n.transform.translation;
                        println!(
                            "    node {i} {:?} visible {} mesh {:?} children {:?} at ({:.3}, {:.3}, {:.3})",
                            n.name, n.visible, n.mesh, n.children, t.x, t.y, t.z
                        );
                    }
                    for (i, m) in s.materials.iter().enumerate() {
                        println!(
                            "    material {i} {:?} base {:?} tex {:?} mr {:?} ({}, {}) normal {:?} emissive {:?} alpha {:?}",
                            m.name,
                            m.base_color,
                            m.base_color_texture.map(|t| (t.texture, t.uv_set)),
                            m.metallic_roughness_texture.map(|t| t.texture),
                            m.metallic,
                            m.roughness,
                            m.normal_texture.map(|t| t.texture),
                            m.emissive,
                            m.alpha_mode
                        );
                    }
                }
            }
            Err(e) => println!("{arg}: error {e} | preview {preview:?}"),
        }
    }
}

/// Triangles drawn: visible nodes' meshes, counted per node.
fn visible_triangles(s: &slerp_scene::Scene) -> usize {
    let mut total = 0;
    let mut stack: Vec<(usize, bool)> = s.roots.iter().map(|&r| (r, true)).collect();
    while let Some((i, vis)) = stack.pop() {
        let n = &s.nodes[i];
        let vis = vis && n.visible;
        if vis && let Some(m) = n.mesh {
            total += s.meshes[m].primitives.iter().map(|p| p.triangle_count()).sum::<usize>();
        }
        stack.extend(n.children.iter().map(|&c| (c, vis)));
    }
    total
}
