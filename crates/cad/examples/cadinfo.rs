//! Loads CAD files and reports what was read and how long it took; with
//! `--out DIR` also writes each flat drawing's thumbnail there as a PNG
//! (`--dark` for dark paper, `--size N` for its size).
//!
//! `cargo run --release -p slerp-cad --example cadinfo -- [--out DIR] FILE...`

use slerp_cad::{CadFormat, Opened, Paint};
use slerp_scene::Limits;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn main() {
    let mut out: Option<PathBuf> = None;
    let mut size = 256u32;
    let mut dark = false;
    let mut big = false;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().map(PathBuf::from),
            "--size" => size = args.next().and_then(|s| s.parse().ok()).unwrap_or(256),
            "--dark" => dark = true,
            "--big" => big = true,
            _ => files.push(PathBuf::from(a)),
        }
    }
    let limits = Limits::thumbnail();
    for f in files {
        let head = std::fs::read(&f).map(|d| d[..d.len().min(4096)].to_vec()).unwrap_or_default();
        let Some(format) = slerp_cad::sniff(&f, &head) else {
            println!("{}: not a CAD file", f.display());
            continue;
        };
        if std::env::var_os("RHINO_SUMMARY").is_some()
            && let Ok(data) = std::fs::read(&f)
        {
            println!("{}: {}", f.display(), slerp_cad::rhino_summary(&data, &limits));
        }
        let start = Instant::now();
        match slerp_cad::open(format, &f, &limits) {
            Ok(Opened::Flat(d)) => {
                let t = start.elapsed();
                let dr = &d.drawing;
                println!(
                    "{}: {} flat, {} polys {} arcs {} fills {} dots, {} pts, {} layers, {} outliers, {:.1} ms",
                    f.display(),
                    dr.info.format,
                    dr.polys.len(),
                    dr.arcs.len(),
                    dr.fills.len(),
                    dr.dots.len(),
                    dr.pts.len(),
                    dr.layers.len(),
                    dr.outliers,
                    t.as_secs_f64() * 1000.0
                );
                for w in &dr.info.warnings {
                    println!("    warning: {w}");
                }
                if big {
                    let mut items: Vec<_> =
                        dr.polys.iter().map(|p| ((p.max[0] - p.min[0]).hypot(p.max[1] - p.min[1]), p)).collect();
                    items.sort_by(|a, b| b.0.total_cmp(&a.0));
                    for (size, p) in items.iter().take(5) {
                        println!(
                            "    big poly {size:.1} on {} ({} pts) from {:?} to {:?}",
                            dr.layers[p.layer as usize].name, p.len, p.min, p.max
                        );
                    }
                    if let Some(b) = &dr.bounds {
                        let mut n = 0;
                        for p in &dr.polys {
                            let c = [(p.min[0] + p.max[0]) / 2.0, (p.min[1] + p.max[1]) / 2.0];
                            if (c[0] < b.min.x - 5.0 * b.size().x
                                || c[0] > b.max.x + 5.0 * b.size().x
                                || c[1] < b.min.y - 5.0 * b.size().y
                                || c[1] > b.max.y + 5.0 * b.size().y)
                                && n < 6
                            {
                                n += 1;
                                println!("    outlier poly {:?}..{:?} ({} pts)", p.min, p.max, p.len);
                            }
                        }
                    }
                    for f in dr.fills.iter().take(3) {
                        println!("    fill {:?} {:?} layer {}", f.min, f.max, dr.layers[f.layer as usize].name);
                    }
                }
                if let Some(b) = &dr.full_bounds {
                    println!("    extents {} {} {} {}", b.min.x, b.min.y, b.max.x, b.max.y);
                }
                if let Some(dir) = &out {
                    let paint = if dark { Paint::DARK } else { Paint::LIGHT };
                    let (w, h) = slerp_cad::picture_size(dr.bounds.as_ref(), size);
                    let t = Instant::now();
                    let paint = Paint { line_px: 1.3 + size as f64 / 512.0, ..paint };
                    let img = dr.render_fit(w, h, &dr.saved_visibility(), &paint);
                    println!("    render {}x{} in {:.1} ms", w, h, t.elapsed().as_secs_f64() * 1000.0);
                    save(dir, &f, &img);
                }
            }
            Ok(Opened::Model(s)) => {
                println!(
                    "{}: {} 3D scene, {} meshes {} triangles, {:.1} ms",
                    f.display(),
                    s.info.format,
                    s.meshes.len(),
                    s.triangle_count(),
                    start.elapsed().as_secs_f64() * 1000.0
                );
                for (k, v) in &s.info.details {
                    println!("    {k}: {v}");
                }
                for w in &s.info.warnings {
                    println!("    warning: {w}");
                }
            }
            Ok(Opened::Picture(t)) => {
                println!(
                    "{}: {} stored preview {}x{}, {:.1} ms",
                    f.display(),
                    format.name(),
                    t.width(),
                    t.height(),
                    start.elapsed().as_secs_f64() * 1000.0
                );
                if let Some(dir) = &out {
                    let img = slerp_core_like_thumbnail(&t);
                    save(dir, &f, &img);
                }
            }
            Err(e) => println!("{}: {:?} error: {e}", f.display(), format as CadFormat),
        }
    }
}

/// The texture as an 8-bit picture, for saving.
fn slerp_core_like_thumbnail(t: &slerp_scene::Texture) -> slerp_scene::Image8 {
    slerp_scene::textures::thumbnail(t, 512)
}

fn save(dir: &Path, file: &Path, img: &slerp_scene::Image8) {
    std::fs::create_dir_all(dir).ok();
    let name = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let path = dir.join(format!("{name}.png"));
    let mut enc = png::Encoder::new(std::fs::File::create(&path).unwrap(), img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&img.pixels).unwrap();
}
