//! Draws one page of a document to a PNG, the way thumbnails do, and says
//! how long it took: `doc2png <file> <out.png> [size] [page name]`.
use slerp_documents::{Picture, load_page, load_path, sniff};
use slerp_scene::Limits;
use slerp_scene::textures::Pixels;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (input, output) = (Path::new(&args[1]), &args[2]);
    let size: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(256);
    let head = std::fs::read(input).map(|b| b[..b.len().min(1024)].to_vec()).unwrap_or_default();
    let Some(format) = sniff(input, &head) else {
        eprintln!("not a document");
        std::process::exit(1);
    };
    let limits = Limits { max_texture_size: (size > 0).then_some(size), ..Limits::thumbnail() };
    let started = std::time::Instant::now();
    let picture = match args.get(4) {
        Some(name) => load_page(format, input, &limits, name),
        None => load_path(format, input, &limits),
    };
    let elapsed = started.elapsed();
    match picture {
        Ok(Picture::Image(t)) => {
            let Pixels::Rgba8(px) = &t.levels[0].surfaces[0] else { return };
            let file = std::fs::File::create(output).unwrap();
            let mut enc = png::Encoder::new(file, t.width(), t.height());
            enc.set_color(png::ColorType::Rgba);
            enc.write_header().unwrap().write_image_data(px).unwrap();
            println!("{format:?} {}x{} in {:.0} ms", t.width(), t.height(), elapsed.as_secs_f64() * 1000.0);
            for (k, v) in &t.source.header {
                println!("  {k}: {v}");
            }
        }
        Ok(Picture::Metafile { bytes, facts }) => {
            println!(
                "{format:?} a {}-byte metafile preview in {:.0} ms {facts:?}",
                bytes.len(),
                elapsed.as_secs_f64() * 1000.0
            )
        }
        Err(e) => {
            println!("{format:?} error after {:.0} ms: {e}", elapsed.as_secs_f64() * 1000.0);
            std::process::exit(2);
        }
    }
}
