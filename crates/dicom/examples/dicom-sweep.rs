//! Opens DICOM files and folders of them, decodes each file's middle frame
//! and prints one line per file: time, size, encoding, or the error.  With
//! `--png <dir>`, also writes each picture there as PNG (thumbnail size with
//! `--size N`).
//!
//! `cargo run --release -p slerp-dicom --example dicom-sweep -- [--png dir] [--size N] <files or folders>`

use slerp_dicom::series::is_dicom_file;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut entries: Vec<PathBuf> =
            std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).collect();
        entries.sort();
        for e in entries {
            walk(&e, out);
        }
    } else if is_dicom_file(p) {
        out.push(p.to_path_buf());
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut png: Option<PathBuf> = None;
    let mut size: Option<u32> = None;
    while let Some(i) = args.iter().position(|a| a == "--png" || a == "--size") {
        let v = args.remove(i + 1);
        if args.remove(i) == "--png" {
            png = Some(PathBuf::from(v));
        } else {
            size = v.parse().ok();
        }
    }
    let mut files = Vec::new();
    for a in &args {
        walk(Path::new(a), &mut files);
    }
    let (mut ok, mut failed) = (0, 0);
    for f in &files {
        let t = Instant::now();
        let result = slerp_dicom::picture_file(f, size, true);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        match result {
            Ok(tex) => {
                ok += 1;
                println!(
                    "OK   {ms:7.1} ms {:>5}x{:<5} {} | {}",
                    tex.width(),
                    tex.height(),
                    f.display(),
                    tex.source.format
                );
                if let Some(dir) = &png {
                    let name = f.to_string_lossy().replace('/', "_");
                    let file = std::fs::File::create(dir.join(format!("{name}.png"))).expect("create png");
                    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), tex.width(), tex.height());
                    enc.set_color(png::ColorType::Rgba);
                    enc.set_depth(png::BitDepth::Eight);
                    let slerp_scene::textures::Pixels::Rgba8(px) = tex.base() else { continue };
                    enc.write_header().and_then(|mut w| w.write_image_data(px)).expect("write png");
                }
            }
            Err(e) => {
                failed += 1;
                println!("FAIL {ms:7.1} ms {} | {e}", f.display());
            }
        }
    }
    println!("{ok} pictures, {failed} failures");
}
