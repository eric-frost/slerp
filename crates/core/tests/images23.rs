//! Phase 23's pictures against the test corpus in
//! `local/assets/images23-extra/` (see its SOURCES.md): GIMP's own renders
//! of the XCF files, the OpenRaster spec composites, Photoshop composites,
//! the reference decodes of QOI, SGI, XPM, XBM and ICNS files, and damaged
//! files of every kind, which must fail or load without hanging.  Each test
//! is skipped when the corpus hasn't been fetched.

use slerp_core::scene::{Asset, Image8, Limits};
use slerp_core::textures::Pixels;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn corpus() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/images23-extra");
    dir.is_dir().then_some(dir)
}

fn png(path: &Path) -> Option<Image8> {
    let mut dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).ok()?));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().ok()?;
    let mut buf = vec![0; r.output_buffer_size()?];
    let f = r.next_frame(&mut buf).ok()?;
    let ch = r.output_color_type().0.samples();
    let pixels = (0..(f.width * f.height) as usize)
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
    Some(Image8 { width: f.width, height: f.height, pixels })
}

/// What the viewer shows first, as 8-bit RGBA.
fn shown(path: &Path) -> Result<Image8, String> {
    match slerp_core::load(path, &Limits::viewer()).map_err(|e| e.to_string())? {
        Asset::Canvas(c) => {
            let visible: Vec<bool> = c.layers().iter().map(|l| l.visible).collect();
            Ok(c.compose(0, &visible, 1.0))
        }
        Asset::Image(t) => match t.base() {
            Pixels::Rgba8(v) => Ok(Image8 { width: t.width(), height: t.height(), pixels: v.clone() }),
            _ => Err("a float picture".into()),
        },
        Asset::Gallery(g) if !g.is_empty() => match g[0].texture.base() {
            Pixels::Rgba8(v) => {
                Ok(Image8 { width: g[0].texture.width(), height: g[0].texture.height(), pixels: v.clone() })
            }
            _ => Err("a float picture".into()),
        },
        _ => Err("not a picture".into()),
    }
}

/// Whether two pictures agree: under 1 % of pixels off by more than
/// `tolerance` (premultiplied), transparent pixels by alpha alone.
fn agree(a: &Image8, b: &Image8, tolerance: i32) -> bool {
    if (a.width, a.height) != (b.width, b.height) {
        return false;
    }
    let off = a
        .pixels
        .chunks_exact(4)
        .zip(b.pixels.chunks_exact(4))
        .filter(|(p, q)| {
            let pm = |x: &[u8], c: usize| x[c] as i32 * x[3] as i32 / 255;
            (0..3).map(|c| (pm(p, c) - pm(q, c)).abs()).max().unwrap_or(0).max((p[3] as i32 - q[3] as i32).abs())
                > tolerance
        })
        .count();
    off * 100 < (a.width * a.height) as usize
}

fn files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|r| r.flatten().map(|e| e.path()).filter(|p| p.to_string_lossy().ends_with(ext)).collect())
        .unwrap_or_default();
    out.sort();
    out
}

/// GIMP 3.0.8's own flattened pictures of 200-odd generated XCF files: every
/// mode, precision, group, mask and offset case matches except the ones
/// GIMP can't reproduce itself (random dissolve, the show-mask view) and
/// the references made after converting precision.
#[test]
fn xcf_matches_gimp() {
    let Some(c) = corpus() else { return };
    let dir = c.join("xcf/generated");
    let (mut good, mut bad) = (0, Vec::new());
    for x in files(&dir, ".xcf") {
        let Some(reference) = png(&x.with_extension("ref.png")) else { continue };
        match shown(&x) {
            Ok(img) if agree(&img, &reference, 8) => good += 1,
            _ => bad.push(x.file_name().unwrap().to_string_lossy().to_string()),
        }
    }
    let expected = [
        "mask-show.xcf",
        "mode-dissolve.xcf",
        "mode-lch-chroma.xcf",
        "prec-float-linear-hdr.xcf",
        "prec-half-linear-hdr.xcf",
        "prec-u8-linear.xcf",
    ];
    let surprising: Vec<&String> = bad.iter().filter(|b| !expected.contains(&b.as_str())).collect();
    assert!(good >= 190 && surprising.is_empty(), "{good} match; unexpected: {surprising:?}");
}

/// OpenRaster files show their merged picture, and the spec's composites
/// agree with it.
#[test]
fn openraster_matches_the_spec() {
    let Some(c) = corpus() else { return };
    let (mut good, mut bad) = (0, Vec::new());
    for o in files(&c.join("ora/generated"), ".ora") {
        let name = o.file_name().unwrap().to_string_lossy().to_string();
        let Some(reference) = png(&o.with_extension("ref.png")) else { continue };
        // Hand-made damaged files whose stored picture differs on purpose.
        if name.starts_with("bad-") || name.starts_with("merged-differs") {
            let _ = shown(&o);
            continue;
        }
        match shown(&o) {
            Ok(img) if agree(&img, &reference, 8) => good += 1,
            _ => bad.push(name),
        }
    }
    assert!(good >= 30 && bad.len() <= 2, "{good} match; differ: {bad:?}");
}

/// Photoshop files from Pillow's and psd-tools' tests against ImageMagick's
/// composites (32-bit files are left out: ImageMagick doesn't encode their
/// linear light for display).
#[test]
fn photoshop_composites() {
    let Some(c) = corpus() else { return };
    let mut good = 0;
    let mut bad = Vec::new();
    for (src, refs) in [("psd/pillow", "psd/refs/pillow"), ("psd/psd-tools", "psd/refs/psd-tools")] {
        for p in files(&c.join(src), ".psd").into_iter().chain(files(&c.join(src), ".psb")) {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            let Some(reference) = png(&c.join(refs).join(format!("{name}.ref.png"))) else { continue };
            match shown(&p) {
                Ok(img) if agree(&img, &reference, 8) => good += 1,
                Err(e) if e == "a float picture" => {}
                Ok(_) | Err(_) => bad.push(name),
            }
        }
    }
    // The rest: vector masks and shapes Photoshop rasterises differently,
    // documents shown scaled (above 16 million pixels), and stand-in
    // composites ImageMagick shows where Slerp shows the embedded preview.
    assert!(good >= 100 && bad.len() <= 12, "{good} match; differ: {bad:?}");
}

/// QOI, SGI, XPM, XBM and ICNS against other decoders' results.
#[test]
fn small_formats_match_other_decoders() {
    let Some(c) = corpus() else { return };
    for (fmt, min) in [("qoi", 50), ("sgi", 75), ("xpm", 80), ("xbm", 115), ("icns", 60)] {
        let refs = c.join(fmt).join("refs");
        let mut good = 0;
        let mut bad = Vec::new();
        for src in std::fs::read_dir(c.join(fmt)).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
            let src_name = src.file_name().unwrap().to_string_lossy().to_string();
            if src_name == "refs" {
                continue;
            }
            for f in std::fs::read_dir(&src).unwrap().flatten().map(|e| e.path()) {
                let key = format!("{src_name}-{}", f.file_name().unwrap().to_string_lossy());
                let Some(reference) =
                    ["ref.png", "pil.ref.png", "qt.ref.png"].iter().find_map(|e| png(&refs.join(format!("{key}.{e}"))))
                else {
                    continue;
                };
                match shown(&f) {
                    Ok(img) if agree(&img, &reference, 2) => good += 1,
                    _ => bad.push(key),
                }
            }
        }
        assert!(good >= min, "{fmt}: {good} match; differ: {bad:?}");
    }
}

/// Every damaged, truncated or hostile file in the corpus fails or loads,
/// quickly, and never takes the process down.
#[test]
fn damaged_files_are_harmless() {
    let Some(c) = corpus() else { return };
    let dirs = [
        "qoi/generated-bad",
        "sgi/generated-bad",
        "sgi/pillow-malformed",
        "sgi/oiio-malformed",
        "xpm/generated-bad",
        "xpm/libxpm-malformed",
        "xbm/generated-bad",
        "icns/generated-bad",
        "icns/pillow-malformed",
        "exr/generated/malformed",
        "exr/openexr-images/Damaged",
        "svg/malformed",
    ];
    for d in dirs {
        for f in
            std::fs::read_dir(c.join(d)).map(|r| r.flatten().map(|e| e.path()).collect::<Vec<_>>()).unwrap_or_default()
        {
            let start = Instant::now();
            let _ = slerp_core::load(&f, &Limits::thumbnail());
            let _ = slerp_core::load(&f, &Limits::viewer());
            assert!(start.elapsed() < Duration::from_secs(30), "{}", f.display());
        }
    }
    for f in files(&c.join("ora/generated"), ".ora").into_iter().chain(files(&c.join("psd/pillow"), ".psd")) {
        let start = Instant::now();
        let _ = shown(&f);
        assert!(start.elapsed() < Duration::from_secs(30), "{}", f.display());
    }
}

/// Every format is recognised by content and mapped to its row of the
/// format table.
#[test]
fn sniffing_and_formats() {
    let Some(c) = corpus() else { return };
    let cases = [
        ("psd/pillow/hopper.psd", "psd"),
        ("psd/psd-tools/32bit.psb", "psd"),
        ("xcf/generated/mode-normal.xcf", "xcf"),
        ("xcf/generated/compressed-gz.xcf.gz", "xcf"),
        ("ora/mypaint/smallimage.ora", "openraster"),
        ("exr/generated/im-rgb-zip.exr", "openexr"),
        ("sgi/pillow/hopper.bw", "sgi"),
        ("svg/svgz", "svg"),
    ];
    for (rel, id) in cases {
        let mut p = c.join(rel);
        if p.is_dir() {
            let Some(first) = std::fs::read_dir(&p).ok().and_then(|mut r| r.next()).and_then(|e| e.ok()) else {
                continue;
            };
            p = first.path();
        }
        if !p.exists() {
            continue;
        }
        let kind = slerp_core::sniff_file(&p).unwrap().unwrap_or_else(|| panic!("{rel} not recognised"));
        assert_eq!(kind.format(&p).map(|f| f.id), Some(id), "{rel}");
    }
}
