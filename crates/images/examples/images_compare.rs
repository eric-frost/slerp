//! Compares decoded pictures with reference PNGs kept as
//! `<format>/refs/<source>-<name>.ref.png` beside `<format>/<source>/<name>`
//! (or `.pil.ref.png` / `.qt.ref.png` when ImageMagick couldn't decode the
//! file), as the phase 23 test corpus lays them out.  Galleries (icons) are
//! compared by their first, largest picture.  Pixels with zero alpha compare
//! by alpha alone.  `cargo run -p slerp-images --example images_compare -- files...`
use slerp_scene::textures::Pixels;
use slerp_scene::{Asset, Limits};
use std::path::Path;

fn png(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let mut dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).ok()?));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().ok()?;
    let mut buf = vec![0; r.output_buffer_size()?];
    let f = r.next_frame(&mut buf).ok()?;
    let ch = r.output_color_type().0.samples();
    let px = (0..(f.width * f.height) as usize)
        .flat_map(|i| {
            let s = |c: usize| buf[i * ch + c];
            match ch {
                1 => [s(0), s(0), s(0), 255],
                2 => [s(0), s(0), s(0), s(1)],
                3 => [s(0), s(1), s(2), 255],
                _ => [s(0), s(1), s(2), s(3)],
            }
        })
        .collect();
    Some((f.width, f.height, px))
}

fn main() {
    let (mut good, mut bad) = (0, 0);
    for arg in std::env::args().skip(1) {
        let path = Path::new(&arg);
        let (Some(src), Some(fmt)) = (path.parent(), path.parent().and_then(|p| p.parent())) else { continue };
        let name = path.file_name().unwrap().to_string_lossy();
        let refs = fmt.join("refs");
        let key = format!("{}-{name}", src.file_name().unwrap().to_string_lossy());
        let reference = ["ref.png", "pil.ref.png", "qt.ref.png"]
            .iter()
            .map(|e| refs.join(format!("{key}.{e}")))
            .find(|p| p.exists());
        let head = std::fs::read(path).map(|b| b[..b.len().min(4096)].to_vec()).unwrap_or_default();
        let Some(format) = slerp_images::sniff(path, &head) else {
            println!("{key}\tNOT RECOGNISED");
            continue;
        };
        let t0 = std::time::Instant::now();
        let tex = match slerp_images::load(format, path, &Limits::viewer()) {
            Ok(Asset::Image(t)) => t,
            Ok(Asset::Gallery(g)) if !g.is_empty() => g[0].texture.clone(),
            Ok(_) => continue,
            Err(e) => {
                println!("{key}\tERROR\t{e}\t{}", if reference.is_some() { "(a reference exists)" } else { "" });
                continue;
            }
        };
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        let Pixels::Rgba8(px) = tex.base() else {
            println!("{key}\tfloat picture");
            continue;
        };
        let Some(reference) = reference else {
            println!("{key}\tno reference\t{}x{}", tex.width(), tex.height());
            continue;
        };
        let Some((rw, rh, rpx)) = png(&reference) else { continue };
        if (rw, rh) != (tex.width(), tex.height()) {
            println!("{key}\tSIZE\t{}x{} vs {rw}x{rh}", tex.width(), tex.height());
            bad += 1;
            continue;
        }
        let (mut off, mut worst) = (0, 0);
        for (a, b) in px.chunks_exact(4).zip(rpx.chunks_exact(4)) {
            let d = if a[3] == 0 && b[3] == 0 {
                0
            } else {
                (0..4).map(|c| (a[c] as i32 - b[c] as i32).abs()).max().unwrap_or(0)
            };
            worst = worst.max(d);
            if d > 2 {
                off += 1;
            }
        }
        if off == 0 {
            good += 1
        } else {
            bad += 1
        }
        let share = off as f64 * 100.0 / (rw * rh) as f64;
        println!("{key}\t{}\toff {share:.2}%\tworst {worst}\t{ms:.1} ms", if off == 0 { "ok" } else { "DIFF" });
    }
    println!("# {good} match, {bad} differ");
}
