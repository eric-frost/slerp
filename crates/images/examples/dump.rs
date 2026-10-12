//! Opens a file the way the viewer does and writes what came out as PNG
//! files, for comparing with other decoders: `dump <file> <folder>`.
//! Animations give `<folder>/NNN.png` for every frame (full canvas, 8-bit
//! RGBA) and `delays.txt` (one delay in milliseconds per line, then the
//! number of plays); pictures give `<folder>/000.png`; galleries one file
//! per picture.  HDR pictures are written as 16-bit PNG after clamping to 0
//! to 1 in sRGB.

use slerp_images::ImageFormat;
use slerp_scene::textures::Pixels;
use slerp_scene::{Asset, Limits, Texture};
use std::io::Write;
use std::path::Path;

fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) {
    let file = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(rgba).unwrap();
}

fn texture_rgba(t: &Texture) -> Vec<u8> {
    match t.base() {
        Pixels::Rgba8(v) => v.clone(),
        Pixels::Rgba32F(v) => v
            .chunks_exact(4)
            .flat_map(|p| {
                let e = |x: f32| {
                    let x = x.clamp(0.0, 1.0);
                    let s = if x <= 0.0031308 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 };
                    (s * 255.0 + 0.5) as u8
                };
                [e(p[0]), e(p[1]), e(p[2]), (p[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8]
            })
            .collect(),
        Pixels::Blocks(..) => Vec::new(),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(file), Some(out)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: dump <file> <folder>");
        std::process::exit(2);
    };
    let (file, out) = (Path::new(file), Path::new(out));
    std::fs::create_dir_all(out).unwrap();
    let mut head = vec![0u8; 4096];
    let n = std::io::Read::read(&mut std::fs::File::open(file).unwrap(), &mut head).unwrap();
    head.truncate(n);
    let Some(format) = slerp_images::sniff(file, &head) else {
        eprintln!("not one of this crate's formats");
        std::process::exit(1);
    };
    let started = std::time::Instant::now();
    let asset = match slerp_images::load(format, file, &Limits::viewer()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{}: {e}", format.name());
            std::process::exit(1);
        }
    };
    eprintln!("{} opened in {:.1} ms", format_name(format), started.elapsed().as_secs_f64() * 1000.0);
    match asset {
        Asset::Image(t) => {
            write_png(&out.join("000.png"), t.width(), t.height(), &texture_rgba(&t));
            println!("picture {}x{} {} {}", t.width(), t.height(), t.source.container, t.source.format);
            for (k, v) in &t.source.header {
                println!("  {k}: {v}");
            }
        }
        Asset::Gallery(items) => {
            for (i, item) in items.iter().enumerate() {
                let t = &item.texture;
                write_png(&out.join(format!("{i:03}.png")), t.width(), t.height(), &texture_rgba(t));
                println!("{}: {}x{}", item.name, t.width(), t.height());
            }
        }
        Asset::Canvas(c) => {
            let (w, h) = c.size();
            let mut delays = std::fs::File::create(out.join("delays.txt")).unwrap();
            let t = std::time::Instant::now();
            for (i, d) in c.frames().iter().enumerate() {
                let img = c.compose(i, &[], 1.0);
                write_png(&out.join(format!("{i:03}.png")), img.width, img.height, &img.pixels);
                writeln!(delays, "{}", (d * 1000.0).round() as u32).unwrap();
            }
            writeln!(delays, "plays {}", c.loops().map_or("forever".to_string(), |n| n.to_string())).unwrap();
            println!(
                "animation {w}x{h}, {} frames, decoded in {:.1} ms",
                c.frames().len(),
                t.elapsed().as_secs_f64() * 1000.0
            );
            for (k, v) in &c.info().details {
                println!("  {k}: {v}");
            }
            for w in &c.info().warnings {
                println!("  warning: {w}");
            }
        }
        _ => println!("something else"),
    }
}

fn format_name(f: ImageFormat) -> &'static str {
    f.name()
}
