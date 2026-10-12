//! Decodes retro pictures and writes them as PNG, for looking at by eye:
//! `cargo run --example dump -- out_dir file...`.  Not part of the library.

use slerp_scene::textures::Pixels;
use slerp_scene::{Asset, Limits};
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = Path::new(&args[0]);
    std::fs::create_dir_all(out).unwrap();
    for file in &args[1..] {
        let path = Path::new(file);
        let head = std::fs::read(path).unwrap_or_default();
        let head = &head[..head.len().min(4096)];
        let format = slerp_retro::sniff(path, head).or_else(|| slerp_retro::sniff_by_content(path, head));
        let Some(format) = format else {
            println!("{file}: not recognised");
            continue;
        };
        let bytes = std::fs::read(path).unwrap();
        let start = std::time::Instant::now();
        match slerp_retro::load(format, path, &bytes, &Limits::viewer()) {
            Ok(Asset::Image(t)) => {
                let Pixels::Rgba8(px) = t.base() else { continue };
                let name = path.file_name().unwrap().to_string_lossy();
                let dest = out.join(format!("{name}.png"));
                let f = std::fs::File::create(&dest).unwrap();
                let mut enc = png::Encoder::new(f, t.width(), t.height());
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                enc.write_header().unwrap().write_image_data(px).unwrap();
                println!(
                    "{file}: {} {}x{} in {:?}: {}",
                    t.source.container,
                    t.width(),
                    t.height(),
                    start.elapsed(),
                    t.source.format
                );
            }
            Ok(_) => println!("{file}: not an image"),
            Err(e) => println!("{file}: ERROR {e}"),
        }
    }
}
