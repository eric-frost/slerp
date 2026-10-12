//! Compares what Slerp composes with reference pictures: for each file
//! given, `<name>.ref.png` (or `<name>.png`) beside it.  Prints the mean and
//! largest difference of premultiplied 8-bit values and the share of pixels
//! off by more than 8.  `cargo run -p slerp-paint --example paint_compare -- *.xcf`
use slerp_paint::{PaintFormat, load, sniff};
use slerp_scene::{Asset, Limits};
use std::path::Path;

fn reference(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let s = path.to_string_lossy();
    let stem = s.strip_suffix(path.extension()?.to_str()?)?.trim_end_matches('.').to_string();
    let mut candidates = vec![format!("{stem}.ref.png"), format!("{stem}.png")];
    // ag-psd keeps its expected picture as canvas.png beside src.psd.
    if let Some(dir) = path.parent() {
        candidates.push(dir.join("canvas.png").to_string_lossy().into_owned());
    }
    // SLERP_REFS=root=refs: references for root/x/y.psd at refs/x/y.psd.ref.png.
    if let Ok(map) = std::env::var("SLERP_REFS")
        && let Some((root, refs)) = map.split_once('=')
        && let Ok(rel) = std::fs::canonicalize(path).unwrap_or_default().strip_prefix(std::fs::canonicalize(root).ok()?)
    {
        candidates.insert(0, Path::new(refs).join(format!("{}.ref.png", rel.display())).to_string_lossy().into_owned());
    }
    let file = candidates.iter().find(|c| Path::new(c).exists())?;
    let mut dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(file).ok()?));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().ok()?;
    let mut buf = vec![0; r.output_buffer_size()?];
    let f = r.next_frame(&mut buf).ok()?;
    let (ct, _) = r.output_color_type();
    let n = (f.width * f.height) as usize;
    let ch = ct.samples();
    let px = (0..n)
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
    let mut total = (0usize, 0usize);
    for arg in std::env::args().skip(1) {
        let path = Path::new(&arg);
        let head = std::fs::read(path).map(|b| b[..b.len().min(4096)].to_vec()).unwrap_or_default();
        let Some(format) = sniff(path, &head) else { continue };
        let name = path.file_name().unwrap().to_string_lossy();
        let start = std::time::Instant::now();
        let img = match load(format, path, &Limits::viewer()) {
            Ok(Asset::Canvas(c)) => {
                let shown: Vec<bool> = c.layers().iter().map(|l| l.visible).collect();
                c.compose(0, &shown, 1.0)
            }
            Ok(Asset::Image(t)) => {
                let px = match t.base() {
                    slerp_scene::textures::Pixels::Rgba8(v) => v.clone(),
                    slerp_scene::textures::Pixels::Rgba32F(v) => v
                        .iter()
                        .enumerate()
                        .map(|(i, &x)| {
                            let x = if i % 4 == 3 { x } else { slerp_paint::compose::srgb_encode(x) };
                            (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
                        })
                        .collect(),
                    _ => continue,
                };
                slerp_scene::Image8 { width: t.width(), height: t.height(), pixels: px }
            }
            Ok(_) => continue,
            Err(e) => {
                println!("{name}\tERROR\t{e}");
                continue;
            }
        };
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        // SLERP_COMPARE_OUT=dir writes what was composed, for looking at.
        if let Ok(dir) = std::env::var("SLERP_COMPARE_OUT") {
            let out = Path::new(&dir).join(format!("{name}.png"));
            if let Ok(f) = std::fs::File::create(&out) {
                let mut enc = png::Encoder::new(std::io::BufWriter::new(f), img.width, img.height);
                enc.set_color(png::ColorType::Rgba);
                if let Ok(mut w) = enc.write_header() {
                    let _ = w.write_image_data(&img.pixels);
                }
            }
        }
        let _ = format == PaintFormat::Kra;
        let Some((rw, rh, rpx)) = reference(path) else {
            println!("{name}\tno reference\t{}x{}\t{ms:.0} ms", img.width, img.height);
            continue;
        };
        if (rw, rh) != (img.width, img.height) {
            println!("{name}\tSIZE\t{}x{} vs reference {rw}x{rh}", img.width, img.height);
            total.1 += 1;
            continue;
        }
        let (mut sum, mut max, mut off) = (0f64, 0i32, 0usize);
        for (a, b) in img.pixels.chunks_exact(4).zip(rpx.chunks_exact(4)) {
            let mut worst = 0;
            for c in 0..4 {
                let (x, y) = if c == 3 {
                    (a[3] as i32, b[3] as i32)
                } else {
                    (a[c] as i32 * a[3] as i32 / 255, b[c] as i32 * b[3] as i32 / 255)
                };
                let d = (x - y).abs();
                sum += d as f64;
                worst = worst.max(d);
            }
            max = max.max(worst);
            if worst > 8 {
                off += 1;
            }
        }
        let n = (rw * rh) as usize;
        let mean = sum / (n * 4) as f64;
        let share = off as f64 * 100.0 / n as f64;
        let verdict = if share < 1.0 && mean < 1.0 { "ok" } else { "DIFF" };
        if verdict == "ok" {
            total.0 += 1;
        } else {
            total.1 += 1;
        }
        println!("{name}\t{verdict}\tmean {mean:.2}\tmax {max}\toff {share:.1}%\t{ms:.0} ms");
    }
    println!("# {} match, {} differ", total.0, total.1);
}
