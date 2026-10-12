//! Decodes OpenEXR files and compares them with float references written
//! by the OpenEXR library: `<refs>/<name>.ref.pfm.gz` (RGB, the data window
//! of level 0) and `.ref.alpha.pfm.gz`.  Prints the share of samples off by
//! more than 0.1 % (relative), and times.
//! `cargo run -p slerp-openexr --example exrcheck -- <refs dir> files...`
use std::io::Read;
use std::path::Path;

fn pfm(path: &Path) -> Option<(usize, usize, usize, Vec<f32>)> {
    let gz = std::fs::read(path).ok()?;
    let mut d = Vec::new();
    // gzip: skip the 10-byte header (the writer sets no optional fields).
    let raw = miniz_oxide::inflate::decompress_to_vec(&gz[10..gz.len() - 8]).ok()?;
    d.extend(raw);
    let mut fields = Vec::new();
    let mut at = 0;
    while fields.len() < 4 {
        let start = at;
        while !d[at].is_ascii_whitespace() {
            at += 1;
        }
        fields.push(String::from_utf8_lossy(&d[start..at]).into_owned());
        at += 1;
        while fields.len() < 4 && d[at].is_ascii_whitespace() {
            at += 1;
        }
    }
    let ch = if fields[0] == "PF" { 3 } else { 1 };
    let (w, h): (usize, usize) = (fields[1].parse().ok()?, fields[2].parse().ok()?);
    let le = fields[3].starts_with('-');
    let mut px = vec![0f32; w * h * ch];
    for y in 0..h {
        for x in 0..w * ch {
            let i = at + ((h - 1 - y) * w * ch + x) * 4;
            let b = [d[i], d[i + 1], d[i + 2], d[i + 3]];
            px[y * w * ch + x] = if le { f32::from_le_bytes(b) } else { f32::from_be_bytes(b) };
        }
    }
    let _ = std::io::empty().read(&mut []);
    Some((w, h, ch, px))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let refs = args.next().expect("refs dir");
    let (mut good, mut bad) = (0, 0);
    for file in args {
        let path = Path::new(&file);
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let start = std::time::Instant::now();
        let opts = slerp_openexr::Options { place: false, to_srgb: false, ..Default::default() };
        let result = std::fs::File::open(path).map_err(|e| e.to_string()).and_then(|f| {
            let exr = slerp_openexr::open(std::io::BufReader::new(f))?;
            let (p, l) = exr.default_choice().ok_or("no layer")?;
            slerp_openexr::decode(std::io::BufReader::new(std::fs::File::open(path).unwrap()), p, l, &opts)
        });
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        let pic = match result {
            Ok(p) => p,
            Err(e) => {
                println!("{name}\tERROR\t{e}\t{ms:.1} ms");
                continue;
            }
        };
        let stem = name.trim_end_matches(".exr");
        let Some((w, h, ch, rgb)) = pfm(&Path::new(&refs).join(format!("{stem}.ref.pfm.gz"))) else {
            println!("{name}\tno reference\t{}x{}\t{ms:.1} ms", pic.levels[0].width, pic.levels[0].height);
            continue;
        };
        let l = &pic.levels[0];
        if (w, h) != (l.width as usize, l.height as usize) {
            println!("{name}\tSIZE\t{}x{} vs {w}x{h}", l.width, l.height);
            bad += 1;
            continue;
        }
        let alpha = pfm(&Path::new(&refs).join(format!("{stem}.ref.alpha.pfm.gz")));
        let (mut off, mut n, mut worst) = (0usize, 0usize, 0f32);
        for i in 0..w * h {
            for c in 0..4 {
                let want = if c < 3 {
                    rgb[i * ch + c.min(ch - 1)]
                } else {
                    match &alpha {
                        Some(a) => a.3[i * a.2],
                        None => continue,
                    }
                };
                let have = l.rgba[i * 4 + c];
                n += 1;
                if want.is_nan() && have.is_nan() || want == have {
                    continue;
                }
                let e = (have - want).abs() / want.abs().max(1.0);
                if e.is_nan() || e > 1e-3 {
                    off += 1;
                }
                if e.is_finite() {
                    worst = worst.max(e);
                } else {
                    worst = f32::INFINITY;
                }
            }
        }
        let share = off as f64 * 100.0 / n.max(1) as f64;
        let verdict = if off == 0 { "ok" } else { "DIFF" };
        if off == 0 {
            good += 1
        } else {
            bad += 1
        }
        println!("{name}\t{verdict}\toff {share:.2}%\tworst {worst:.4}\t{ms:.1} ms");
    }
    println!("# {good} match, {bad} differ");
}
