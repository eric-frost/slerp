//! Loads glTF, GLB or VRM files and writes them again as GLB, for checking
//! the writer against the Khronos validator without building the rest of
//! Slerp: `cargo run --example to_glb -- OUT_DIR FILE... [--node=N]
//! [--mesh=N] [--anim=N] [--skeleton] [--centre] [--png]`.

use slerp_gltf::{GlbOptions, Placement, Selection, write_glb};
use slerp_scene::{Asset, Limits};
use std::path::Path;
use std::time::Instant;

fn main() {
    let mut options = GlbOptions::default();
    let mut what = Selection::Scene;
    let mut skeleton = false;
    let mut files = Vec::new();
    for arg in std::env::args().skip(1) {
        let number = |a: &str| a.split_once('=').and_then(|(_, n)| n.parse().ok()).unwrap_or(0);
        match arg.as_str() {
            "--centre" => options.placement = Placement::Centred,
            "--png" => options.original_images = false,
            "--skeleton" => skeleton = true,
            a if a.starts_with("--node=") => what = Selection::Node(number(a)),
            a if a.starts_with("--mesh=") => what = Selection::Mesh(number(a)),
            a if a.starts_with("--anim=") => what = Selection::Animation { index: number(a), skeleton_only: false },
            _ => files.push(arg),
        }
    }
    if let Selection::Animation { skeleton_only, .. } = &mut what {
        *skeleton_only = skeleton;
    }
    let Some((out_dir, files)) = files.split_first() else {
        eprintln!("usage: to_glb OUT_DIR FILE...");
        std::process::exit(2);
    };
    std::fs::create_dir_all(out_dir).ok();
    for file in files {
        let path = Path::new(file);
        let Ok(bytes) = std::fs::read(path) else {
            println!("{file}: can't read");
            continue;
        };
        let start = Instant::now();
        let scene = match slerp_gltf::load(path, &bytes, &Limits::viewer()) {
            Ok(Asset::Scene(s)) => s,
            Ok(_) => continue,
            Err(e) => {
                println!("{file}: load failed: {e}");
                continue;
            }
        };
        let loaded = start.elapsed();
        let start = Instant::now();
        match write_glb(&scene, &what, &options) {
            Ok(glb) => {
                let written = start.elapsed();
                let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let parent = path.parent().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().into_owned());
                let name = format!("{}-{stem}.glb", parent.unwrap_or_default());
                let out = Path::new(out_dir).join(name.replace(' ', "_"));
                std::fs::write(&out, &glb.bytes).ok();
                println!(
                    "{file}: loaded {:.0} ms, written {:.0} ms, {} KB -> {}",
                    loaded.as_secs_f64() * 1000.0,
                    written.as_secs_f64() * 1000.0,
                    glb.bytes.len() >> 10,
                    out.display()
                );
                for w in &glb.warnings {
                    println!("  warning: {w}");
                }
            }
            Err(e) => println!("{file}: write failed: {e}"),
        }
    }
}
