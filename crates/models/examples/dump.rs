//! Prints a loaded scene's nodes, meshes, skins and materials, for
//! debugging a loader against one file.
//!
//! `cargo run -p slerp-models --example dump -- <file>`

use slerp_models::{load, sniff};
use slerp_scene::{Asset, Limits, Pose};

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("file"));
    let bytes = std::fs::read(&path).expect("read");
    let format = sniff(&path, &bytes[..bytes.len().min(4096)]).expect("unknown format");
    let Ok(Asset::Scene(s)) = load(format, &path, &bytes, &Limits::viewer()) else { panic!("load failed") };
    println!("{} generator {:?}", s.info.format, s.info.generator);
    for (k, v) in &s.info.details {
        println!("  {k}: {v}");
    }
    for w in &s.info.warnings {
        println!("  warning: {w}");
    }
    let pose = Pose::rest(&s);
    println!("bounds {:?}", s.bounds(&pose));
    for (i, n) in s.nodes.iter().enumerate() {
        let t = n.transform;
        println!(
            "node {i:3} {:<30} mesh {:?} skin {:?} children {:?} t {:.3?} r {:.3?} s {:.3?} world {:.3?}",
            n.name,
            n.mesh,
            n.skin,
            n.children,
            t.translation,
            t.rotation,
            t.scale,
            pose.world[i].w_axis.truncate()
        );
    }
    for (i, m) in s.meshes.iter().enumerate() {
        for p in &m.primitives {
            let mut b = slerp_scene::Aabb::EMPTY;
            p.positions.iter().for_each(|&v| b.add(v));
            println!(
                "mesh {i} {:<24} {:?} verts {} tris {} mat {:?} uv0 {} uv1 {} col {} tan {} skinned {} targets {} local {:.3?}..{:.3?}",
                m.name,
                p.topology,
                p.positions.len(),
                p.triangle_count(),
                p.material,
                p.uv0.len(),
                p.uv1.len(),
                p.colors.len(),
                p.tangents.len(),
                p.is_skinned(),
                p.targets.len(),
                b.min,
                b.max
            );
        }
    }
    for (i, k) in s.skins.iter().enumerate() {
        println!("skin {i} {} joints {}", k.name, k.joints.len());
    }
    for (i, m) in s.materials.iter().enumerate() {
        println!(
            "material {i} {:<20} base {:.3?} tex {:?} mr {:?} normal {:?} emissive {:.2?} rough {:.2} metal {:.2} alpha {:?}",
            m.name,
            m.base_color,
            m.base_color_texture.map(|t| t.texture),
            m.metallic_roughness_texture.map(|t| t.texture),
            m.normal_texture.map(|t| t.texture),
            m.emissive,
            m.roughness,
            m.metallic,
            m.alpha_mode
        );
    }
    for a in &s.animations {
        println!("clip {:<30} {:.2}s channels {}", a.name, a.duration, a.channels.len());
    }
}
