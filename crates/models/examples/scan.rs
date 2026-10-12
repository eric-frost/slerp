//! Loads every model file under the given folders and prints what each
//! holds, how long it took and what went wrong, for checking the loaders
//! against a corpus.
//!
//! `cargo run --release -p slerp-models --example scan -- [--thumb] [--quiet] <path>...`

use slerp_models::{load, sniff};
use slerp_scene::{Asset, Limits};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn walk(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(path).into_iter().flatten().flatten().map(|e| e.path()).collect();
        entries.sort();
        for e in entries {
            if e.file_name().is_some_and(|n| n != ".git") {
                walk(&e, out);
            }
        }
    } else {
        out.push(path.to_path_buf());
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let thumb = args.iter().any(|a| a == "--thumb");
    let quiet = args.iter().any(|a| a == "--quiet");
    let limits = if thumb { Limits::thumbnail() } else { Limits::viewer() };
    let mut files = Vec::new();
    for a in args.iter().filter(|a| !a.starts_with("--")) {
        walk(Path::new(a), &mut files);
    }
    let (mut ok, mut failed, mut total_ms) = (0, 0, 0.0);
    for f in files {
        let Ok(bytes) = std::fs::read(&f) else { continue };
        let Some(format) = sniff(&f, &bytes[..bytes.len().min(4096)]) else { continue };
        let start = Instant::now();
        let result = load(format, &f, &bytes, &limits);
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        total_ms += ms;
        match result {
            Ok(Asset::Scene(s)) => {
                ok += 1;
                let anim_channels: usize = s.animations.iter().map(|a| a.channels.len()).sum();
                let morphs: usize = s.meshes.iter().map(|m| m.primitives.first().map_or(0, |p| p.targets.len())).sum();
                let joints: usize = s.skins.iter().map(|k| k.joints.len()).sum();
                println!(
                    "OK   {:8.1} ms  {:<40} {:<24} nodes {:4} meshes {:3} tris {:8} verts {:8} mats {:3} tex {:3} skins {} joints {} morphs {} clips {} ({} ch) warn {}",
                    ms,
                    f.display()
                        .to_string()
                        .chars()
                        .rev()
                        .take(40)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect::<String>(),
                    s.info.format,
                    s.nodes.len(),
                    s.meshes.len(),
                    s.triangle_count(),
                    s.vertex_count(),
                    s.materials.len(),
                    s.textures.len(),
                    s.skins.len(),
                    joints,
                    morphs,
                    s.animations.len(),
                    anim_channels,
                    s.info.warnings.len()
                );
                if !quiet {
                    for w in s.info.warnings.iter().take(4) {
                        println!("         warning: {w}");
                    }
                }
            }
            Ok(_) => println!("??   {}", f.display()),
            Err(e) => {
                failed += 1;
                println!("FAIL {:8.1} ms  {}  {e}", ms, f.display());
            }
        }
    }
    println!("{ok} loaded, {failed} failed, {total_ms:.0} ms total");
}
