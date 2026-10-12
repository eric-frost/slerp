//! Renders metafiles to PNG files for looking at and for comparing with
//! other renderers: `render <size> <out-dir> <file>...`.  Prints how long
//! each took and what it skipped.

use slerp_metafile::{MetafileFormat, load, sniff};
use slerp_scene::Limits;
use std::path::Path;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let size: u32 = args[1].parse().unwrap();
    let out_dir = Path::new(&args[2]);
    std::fs::create_dir_all(out_dir).unwrap();
    for file in &args[3..] {
        let path = Path::new(file);
        let bytes = std::fs::read(path).unwrap();
        let head = &bytes[..bytes.len().min(4096)];
        let Some(format) = sniff(path, head) else {
            println!("{file}: not recognised");
            continue;
        };
        let limits =
            if size == 0 { Limits::viewer() } else { Limits { max_texture_size: Some(size), ..Limits::thumbnail() } };
        let t = Instant::now();
        let result = std::panic::catch_unwind(|| load(format, &bytes, &limits));
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        match result {
            Err(_) => println!("{file}: PANIC"),
            Ok(Err(e)) => println!("{file}: {format:?} error {e} ({ms:.0} ms)"),
            Ok(Ok(tex)) => {
                let level = &tex.levels[0];
                let slerp_scene::textures::Pixels::Rgba8(px) = &level.surfaces[0] else { panic!() };
                // Over white, as thumbnails show it.
                let mut flat = px.clone();
                for p in flat.chunks_exact_mut(4) {
                    let a = p[3] as u32;
                    for c in p.iter_mut().take(3) {
                        *c = ((*c as u32 * a + 255 * (255 - a)) / 255) as u8;
                    }
                    p[3] = 255;
                }
                let name = path.file_name().unwrap().to_string_lossy();
                let out = out_dir.join(format!("{name}.png"));
                let f = std::fs::File::create(&out).unwrap();
                let mut enc = png::Encoder::new(f, level.width, level.height);
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                enc.write_header().unwrap().write_image_data(&flat).unwrap();
                let skipped: Vec<String> = tex
                    .source
                    .header
                    .iter()
                    .filter(|(k, _)| k != "Drawing")
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect();
                println!("{file}: {:?} {}x{} {ms:.0} ms {}", format, level.width, level.height, skipped.join("; "));
            }
        }
        let _ = MetafileFormat::Wmf;
    }
}
