//! Opens a JPEG, PNG or WebP the way the viewer does (texture decode, then
//! orientation and colour) and writes the result: `photo <file> <out.png>`,
//! printing what was done to it.

use slerp_scene::textures::{DecodeOptions, Format, Pixels};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(file), Some(out)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: photo <file> <out.png>");
        std::process::exit(2);
    };
    let path = std::path::Path::new(file);
    let format = match path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Format::Jpeg,
        "png" => Format::Png,
        _ => Format::Webp,
    };
    let tex = slerp_scene::textures::decode_file(path, &DecodeOptions::full()).expect("decode");
    let tex = slerp_images::photo::refine(path, format, tex);
    println!("{}x{} {} {}", tex.width(), tex.height(), tex.source.container, tex.source.format);
    for (k, v) in &tex.source.header {
        println!("  {k}: {v}");
    }
    let Pixels::Rgba8(px) = tex.base() else { return };
    let file = std::fs::File::create(out).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), tex.width(), tex.height());
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(px).unwrap();
}
