//! Loads glTF, GLB or VRM files and prints what they hold and how long
//! loading took: `cargo run --release --example load -- [--viewer] FILE...`.

use slerp_scene::{Asset, Limits, Pose};
use std::time::Instant;

fn main() {
    let mut limits = Limits::thumbnail();
    let mut repeat = 1;
    let mut per_node = false;
    let mut files = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--viewer" => limits = Limits::viewer(),
            "--nodes" => per_node = true,
            a if a.starts_with("--repeat=") => repeat = a[9..].parse().unwrap_or(1),
            _ => files.push(arg),
        }
    }
    for file in files {
        let path = std::path::Path::new(&file);
        let bytes = std::fs::read(path).expect("read");
        let mut times = Vec::new();
        let mut result = None;
        for _ in 0..repeat {
            let start = Instant::now();
            let r = slerp_gltf::load(path, &bytes, &limits);
            times.push(start.elapsed().as_secs_f64() * 1000.0);
            result = Some(r);
        }
        let first = times[0];
        times.sort_by(f64::total_cmp);
        println!("== {file}: first {first:.1} ms, best {:.1} ms, median {:.1} ms", times[0], times[times.len() / 2]);
        let scene = match result.unwrap() {
            Ok(Asset::Scene(s)) => s,
            Ok(_) => {
                println!("not a scene");
                continue;
            }
            Err(e) => {
                println!("error: {e}");
                continue;
            }
        };
        let morphs: usize =
            scene.meshes.iter().map(|m| m.primitives.iter().map(|p| p.targets.len()).max().unwrap_or(0)).sum();
        println!(
            "{} | {:?} | nodes {} meshes {} prims {} tris {} verts {} morphs {} materials {} textures {} skins {:?} clips {}",
            scene.info.format,
            scene.info.generator,
            scene.nodes.len(),
            scene.meshes.len(),
            scene.meshes.iter().map(|m| m.primitives.len()).sum::<usize>(),
            scene.triangle_count(),
            scene.vertex_count(),
            morphs,
            scene.materials.len(),
            scene.textures.len(),
            scene.skins.iter().map(|s| s.joints.len()).collect::<Vec<_>>(),
            scene.animations.len(),
        );
        let mut pose = Pose::rest(&scene);
        let b = scene.bounds(&pose);
        println!("rest bounds {:?} .. {:?} size {:?}", b.min, b.max, b.size());
        if per_node {
            let mut scene = scene;
            let visible: Vec<bool> = scene.nodes.iter().map(|n| n.visible).collect();
            for i in 0..scene.nodes.len() {
                if scene.nodes[i].mesh.is_none() {
                    continue;
                }
                scene.nodes.iter_mut().enumerate().for_each(|(k, n)| n.visible = k == i);
                let b = scene.bounds(&pose);
                println!(
                    "  node {i} {:?} skin {:?}: {:?} .. {:?}",
                    scene.nodes[i].name, scene.nodes[i].skin, b.min, b.max
                );
            }
            scene.nodes.iter_mut().zip(visible).for_each(|(n, v)| n.visible = v);
            continue;
        }
        if !scene.animations.is_empty() {
            let a = &scene.animations[0];
            pose.sample(&scene, 0, a.duration * 0.5);
            pose.update_world(&scene);
            let b = scene.bounds(&pose);
            println!(
                "clip '{}' ({:.2} s, {} channels) mid bounds size {:?}",
                a.name,
                a.duration,
                a.channels.len(),
                b.size()
            );
        }
        if let Some(h) = &scene.humanoid {
            println!("humanoid bones {}", h.bones.len());
        }
        if !scene.expressions.is_empty() {
            let names: Vec<String> =
                scene.expressions.iter().map(|e| format!("{}({})", e.name, e.binds.len())).collect();
            println!("expressions {}", names.join(" "));
        }
        for (k, v) in &scene.info.details {
            let v: String = v.chars().take(150).collect();
            println!("  {k}: {v}");
        }
        for w in &scene.info.warnings {
            println!("  warning: {w}");
        }
    }
}
