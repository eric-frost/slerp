//! Draws SVG files at the size of the PNG beside each (`name.png`, as the
//! resvg test suite keeps its references) and compares the two: share of
//! pixels off by more than 8 levels.  `compare files...`
use slerp_scene::Limits;
use slerp_scene::textures::Pixels;
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
    let (mut good, mut bad, mut errors) = (0, 0, 0);
    let mut worst_list = Vec::new();
    for arg in std::env::args().skip(1) {
        let path = Path::new(&arg);
        let Some((rw, rh, rpx)) = png(&path.with_extension("png")) else { continue };
        let limits = Limits { max_texture_size: Some(rw.max(rh)), ..Limits::thumbnail() };
        let t0 = std::time::Instant::now();
        let tex = match slerp_svg::picture(path, &limits) {
            Ok(t) => t,
            Err(e) => {
                errors += 1;
                println!("{arg}\tERROR\t{e}");
                continue;
            }
        };
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        let Pixels::Rgba8(px) = tex.base() else { continue };
        if (tex.width(), tex.height()) != (rw, rh) {
            bad += 1;
            println!("{arg}\tSIZE\t{}x{} vs {rw}x{rh}", tex.width(), tex.height());
            continue;
        }
        let mut off = 0;
        for (a, b) in px.chunks_exact(4).zip(rpx.chunks_exact(4)) {
            let pm = |p: &[u8], c: usize| p[c] as i32 * p[3] as i32 / 255;
            let d = (0..3).map(|c| (pm(a, c) - pm(b, c)).abs()).max().unwrap().max((a[3] as i32 - b[3] as i32).abs());
            if d > 8 {
                off += 1;
            }
        }
        let share = off as f64 * 100.0 / (rw * rh) as f64;
        if share < 1.0 {
            good += 1;
        } else {
            bad += 1;
            worst_list.push((share, arg.clone()));
        }
        if std::env::var_os("VERBOSE").is_some() {
            println!("{arg}\t{share:.2}%\t{ms:.0} ms");
        }
    }
    worst_list.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (s, a) in worst_list.iter().take(40) {
        println!("DIFF {s:.1}%\t{a}");
    }
    println!("# {good} match, {bad} differ, {errors} errors");
}
