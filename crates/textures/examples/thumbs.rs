//! Writes a PNG thumbnail of each file named on the command line, next to
//! `OUT_DIR`, and prints how long decoding and thumbnailing took.  With
//! `SLERP_SLICES=n`, volume textures also get `n` evenly spaced slices as
//! `<name>.slice<z>.png`.
//!
//!     cargo run --release --example thumbs -p slerp-textures -- OUT_DIR SIZE FILE...

use slerp_textures::{DecodeOptions, Level, Pixels, Texture, TextureKind, decode_file, thumbnail};
use std::time::Instant;

fn write_png(path: &str, img: &slerp_textures::Image8) {
    let mut enc = png::Encoder::new(std::fs::File::create(path).unwrap(), img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&img.pixels).unwrap();
}

/// Slice `z` of a volume texture's first level, as a 2D texture.
fn slice(tex: &Texture, z: u32) -> Texture {
    let l = &tex.levels[0];
    let texels = (l.width * l.height) as usize * 4;
    let range = z as usize * texels..(z as usize + 1) * texels;
    let surface = match &l.surfaces[0] {
        Pixels::Rgba8(v) => Pixels::Rgba8(v[range].to_vec()),
        Pixels::Rgba32F(v) => Pixels::Rgba32F(v[range].to_vec()),
        Pixels::Blocks(..) => panic!("blocks"),
    };
    let level = Level { width: l.width, height: l.height, depth: 1, surfaces: vec![surface] };
    Texture { kind: TextureKind::D2, levels: vec![level], source: tex.source.clone(), ..tex.clone() }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (out, size) = (&args[0], args[1].parse::<u32>().unwrap());
    for file in &args[2..] {
        let name = std::path::Path::new(file).file_name().unwrap().to_string_lossy().to_string();
        // SLERP_REPEAT=n decodes n more times first, to time a warm process.
        for _ in 0..std::env::var("SLERP_REPEAT").ok().and_then(|v| v.parse().ok()).unwrap_or(0) {
            let _ = decode_file(std::path::Path::new(file), &DecodeOptions::preview(size));
        }
        let t0 = Instant::now();
        match decode_file(std::path::Path::new(file), &DecodeOptions::preview(size)) {
            Ok(tex) => {
                let t1 = Instant::now();
                let img = thumbnail(&tex, size);
                let t2 = Instant::now();
                write_png(&format!("{out}/{name}.png"), &img);
                let slices = std::env::var("SLERP_SLICES").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
                let depth = tex.levels[0].depth;
                if tex.kind == TextureKind::D3 && slices > 0 {
                    for i in 0..slices.min(depth) {
                        let z = i * (depth - 1) / (slices - 1).max(1);
                        write_png(&format!("{out}/{name}.slice{z}.png"), &thumbnail(&slice(&tex, z), size));
                    }
                }
                println!(
                    "{name}: {}x{} ({}), decode {:.1} ms, thumbnail {:.1} ms, {} {}",
                    tex.source.width,
                    tex.source.height,
                    tex.source.format,
                    (t1 - t0).as_secs_f64() * 1e3,
                    (t2 - t1).as_secs_f64() * 1e3,
                    tex.source.container,
                    tex.source.header.len()
                );
            }
            Err(e) => println!("{name}: ERROR {e}"),
        }
    }
}
