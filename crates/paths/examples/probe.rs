//! Reads files the way thumbnails and the viewer do and reports what came
//! out, with timings: `cargo run --release -p slerp-paths --example probe
//! -- <files...>`.  Drawn thumbnails (tracks, flat CNC jobs) and embedded
//! pictures are written to `/tmp/slerp-paths-probe/`.

use slerp_paths::{PathData, Thumb};
use std::path::Path;
use std::time::Instant;

fn main() {
    let out = Path::new("/tmp/slerp-paths-probe");
    let _ = std::fs::create_dir_all(out);
    let size: u32 = std::env::var("SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(256);
    for arg in std::env::args().skip(1) {
        let path = Path::new(&arg);
        let mut head = Vec::new();
        use std::io::Read;
        if let Ok(f) = std::fs::File::open(path) {
            let _ = f.take(4096).read_to_end(&mut head);
        }
        let Some(format) = slerp_paths::sniff(path, &head) else {
            println!("{arg}: not recognised");
            continue;
        };
        let start = Instant::now();
        let saved = slerp_paths::saved_picture(format, path).ok().flatten();
        let thumb = slerp_paths::thumbnail(format, path, size);
        let thumb_ms = start.elapsed().as_secs_f64() * 1000.0;
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if let Some(t) = &saved {
            let img = slerp_paths_probe_image(t);
            save(&out.join(format!("{name}.embedded.png")), &img);
            println!("{arg}: embedded picture {}x{} ({})", t.width(), t.height(), t.source.format);
        }
        let what = match &thumb {
            Ok(Thumb::Drawn(img)) => {
                save(&out.join(format!("{name}.png")), img);
                format!("drawn {}x{}", img.width, img.height)
            }
            Ok(Thumb::Scene(s)) => format!(
                "scene: {} triangles, {} primitives",
                s.triangle_count(),
                s.meshes.iter().map(|m| m.primitives.len()).sum::<usize>()
            ),
            Err(e) => format!("error: {e}"),
        };
        let start = Instant::now();
        let opened = slerp_paths::open(format, path, &slerp_paths_limits());
        let open_ms = start.elapsed().as_secs_f64() * 1000.0;
        println!("{arg}: {} | thumbnail {thumb_ms:.0} ms: {what}", format.name());
        match opened {
            Ok(o) => {
                println!(
                    "  open {open_ms:.0} ms: {} nodes, {} triangles",
                    o.scene.nodes.len(),
                    o.scene.triangle_count()
                );
                println!("  format: {} | generator: {:?}", o.scene.info.format, o.scene.info.generator);
                for (k, v) in &o.scene.info.details {
                    println!("    {k}: {v}");
                }
                for w in &o.scene.info.warnings {
                    println!("    warning: {w}");
                }
                match &o.data {
                    PathData::Toolpath(tp) => {
                        let feats: Vec<String> = tp
                            .features()
                            .iter()
                            .map(|f| format!("{} {:.1} m", f.name(), tp.feature_length[f.index()] / 1000.0))
                            .collect();
                        println!("    features: {}", feats.join(", "));
                    }
                    PathData::Track(t) => {
                        println!("    lines {} waypoints {}", t.set.lines.len(), t.set.waypoints.len())
                    }
                }
            }
            Err(e) => println!("  open error: {e}"),
        }
    }
}

fn slerp_paths_limits() -> slerp_scene::Limits {
    slerp_scene::Limits::viewer()
}

fn slerp_paths_probe_image(t: &slerp_scene::textures::Texture) -> slerp_scene::Image8 {
    slerp_scene::textures::thumbnail(t, t.width().max(t.height()))
}

fn save(path: &Path, img: &slerp_scene::Image8) {
    let file = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&img.pixels).unwrap();
}
