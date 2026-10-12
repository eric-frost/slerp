//! Development helper: prints what a file holds and writes its middle slice.
//!
//!     cargo run --example vol -- file [out.png] [part]

use slerp_volumes::*;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: vol <file> [out.png] [part]");
        return;
    };
    let path = Path::new(path);
    let mut head = vec![0u8; 4096];
    let n = std::io::Read::read(&mut std::fs::File::open(path).unwrap(), &mut head).unwrap();
    head.truncate(n);
    let Some(f) = sniff(path, &head) else {
        eprintln!("not recognised");
        return;
    };
    if f == VolumeFormat::Hdf5 && std::env::var("H5_LIST").is_ok() {
        let l = slerp_volumes::hdf5::list(path).unwrap();
        for d in &l.all {
            println!("dataset {} {:?} {:?} {}", d.path, d.shape, d.elem, d.is_picture());
        }
        for p in &l.problems {
            println!("problem: {p}");
        }
        return;
    }
    let t = std::time::Instant::now();
    let opened = open(
        f,
        path,
        args.get(2).map(String::as_str),
        &OpenOptions { thumb_size: Some(256), ..OpenOptions::thumbnail() },
    );
    let opened = match opened {
        Ok(o) => o,
        Err(e) => {
            eprintln!("open failed: {e}");
            return;
        }
    };
    println!("{f:?} opened in {:?}; parts {:?}; part {:?}", t.elapsed(), opened.parts, opened.part);
    let v = match opened.content {
        Content::Volume(v) => v,
        Content::Picture(p) => {
            println!(
                "picture {}x{} source {}x{}",
                p.texture.width(),
                p.texture.height(),
                p.texture.source.width,
                p.texture.source.height
            );
            for (k, val) in &p.facts {
                println!("  {k}: {val}");
            }
            if let (Some(out), slerp_scene::textures::Pixels::Rgba8(px)) = (args.get(1), p.texture.base()) {
                let file = std::fs::File::create(out).unwrap();
                let mut enc = png::Encoder::new(file, p.texture.width(), p.texture.height());
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                enc.write_header().unwrap().write_image_data(px).unwrap();
            }
            return;
        }
    };
    println!("{v:?} no_picture={:?}", v.no_picture);
    for a in &v.axes {
        println!(
            "  axis {:?} len {} stride {} spacing {} dir {:?} {}",
            a.role, a.len, a.stride, a.spacing, a.dir, a.name
        );
    }
    for (k, val) in &v.facts {
        println!("  {k}: {val}");
    }
    for w in &v.warnings {
        println!("  warning: {w}");
    }
    let t = std::time::Instant::now();
    match middle_picture(&v, Some(1024)) {
        Ok(tex) => {
            println!("picture {}x{} in {:?}", tex.width(), tex.height(), t.elapsed());
            if let Some(out) = args.get(1)
                && let slerp_scene::textures::Pixels::Rgba8(px) = tex.base()
            {
                let file = std::fs::File::create(out).unwrap();
                let mut enc = png::Encoder::new(file, tex.width(), tex.height());
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                enc.write_header().unwrap().write_image_data(px).unwrap();
            }
        }
        Err(e) => println!("no picture: {e}"),
    }
    if let Ok(s) = v.stats() {
        println!("stats: min {} max {} mean {:.4} p1 {} p99 {} nan {}", s.min, s.max, s.mean, s.p1, s.p99, s.nan);
    }
}

#[allow(dead_code)]
fn unused() {}
