//! Decodes game images and writes them as PNG, for looking at by eye:
//! `cargo run --example dump -- out_dir file...`.  Not part of the library.

use slerp_scene::textures::Pixels;
use slerp_scene::{Asset, Limits, Texture};
use std::path::Path;

fn write(out: &Path, name: &str, t: &Texture) {
    let Pixels::Rgba8(px) = t.base() else { return };
    let f = std::fs::File::create(out.join(format!("{name}.png"))).unwrap();
    let mut enc = png::Encoder::new(f, t.width(), t.height());
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(px).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = Path::new(&args[0]);
    std::fs::create_dir_all(out).unwrap();
    for file in &args[1..] {
        let path = Path::new(file);
        let bytes = std::fs::read(path).unwrap_or_default();
        let Some(format) = slerp_gameimages::sniff(path, &bytes[..bytes.len().min(4096)]) else {
            println!("{file}: not recognised");
            continue;
        };
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let start = std::time::Instant::now();
        let mut limits = Limits::viewer();
        if let Some(n) = std::env::var("THUMB").ok().and_then(|v| v.parse().ok()) {
            limits = Limits { max_texture_size: Some(n), ..Limits::thumbnail() };
        }
        match slerp_gameimages::load(format, path, &bytes, &limits) {
            Ok(Asset::Image(t)) => {
                write(out, &name, &t);
                println!(
                    "{file}: {} {}x{} in {:?}: {}",
                    t.source.container,
                    t.width(),
                    t.height(),
                    start.elapsed(),
                    t.source.format
                );
            }
            Ok(Asset::Gallery(items)) => {
                for (i, item) in items.iter().take(40).enumerate() {
                    write(out, &format!("{name}-{i:03}"), &item.texture);
                }
                println!(
                    "{file}: gallery of {} in {:?}: {}",
                    items.len(),
                    start.elapsed(),
                    items[0].texture.source.format
                );
            }
            Ok(_) => println!("{file}: not an image"),
            Err(e) => println!("{file}: ERROR {e}"),
        }
    }
}
