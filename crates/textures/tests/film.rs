//! DPX, Cineon, PAM and farbfeld against files written by OpenImageIO (DPX),
//! by hand (Cineon, PAM, farbfeld) and read back by independent readers
//! (`data/make-film.py`).

use slerp_textures::{DecodeOptions, Format, Pixels, Texture, decode_as, decode_file};
use std::path::{Path, PathBuf};

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/film")
}

fn read(name: &str) -> Vec<u8> {
    std::fs::read(dir().join(name)).unwrap()
}

fn floats(name: &str) -> Vec<f32> {
    read(name).chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

fn rgba8(t: &Texture) -> &[u8] {
    let Pixels::Rgba8(v) = t.base() else { panic!("not 8-bit pixels") };
    v
}

fn srgb(linear: f64) -> f64 {
    let l = linear.clamp(0.0, 1.0);
    if l <= 0.0031308 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 }
}

/// The Cineon film curve as the tests compute it, independently of the
/// decoder: printing density to linear light, white at code 685, black at
/// 95, with a soft shoulder above 0.9.
fn log_to_linear(code10: f64) -> f64 {
    let step = 0.002 / 0.6;
    let floor = 10f64.powf((95.0 - 685.0) * step);
    let lin = (10f64.powf((code10 - 685.0) * step) - floor) / (1.0 - floor);
    if lin <= 0.9 { lin.max(0.0) } else { 0.9 + 0.1 * (1.0 - (-(lin - 0.9) / 0.1).exp()) }
}

fn manifest() -> Vec<(String, usize, usize, usize, String, String)> {
    String::from_utf8(read("manifest.txt"))
        .unwrap()
        .lines()
        .map(|l| {
            let f: Vec<&str> = l.splitn(6, ' ').collect();
            (
                f[0].to_string(),
                f[1].parse().unwrap(),
                f[2].parse().unwrap(),
                f[3].parse().unwrap(),
                f[4].to_string(),
                f.get(5).unwrap_or(&"").to_string(),
            )
        })
        .collect()
}

#[test]
fn every_dpx_and_cineon_case_decodes_to_its_codes() {
    let mut count = 0;
    for (name, w, h, c, transfer, note) in manifest() {
        let file = if name.starts_with("cin_") { format!("{name}.cin") } else { format!("{name}.dpx") };
        let bytes = read(&file);
        let tex = decode_as(
            if name.starts_with("cin_") { Format::Cineon } else { Format::Dpx },
            &bytes,
            &DecodeOptions::full(),
        )
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        let expect = floats(&format!("{name}.f32"));
        // An aspect note says the pixels are wider than tall; the picture
        // comes out that much wider.
        let aspect: f64 = note.strip_prefix("aspect ").map_or(1.0, |a| a.parse().unwrap());
        assert_eq!((tex.width() as usize, tex.height() as usize), ((w as f64 * aspect) as usize, h), "{name}: size");
        if aspect != 1.0 {
            // A flat comparison is enough: the stretch keeps the colours.
            let px = rgba8(&tex);
            assert!(px.chunks_exact(4).any(|p| p[0] != p[1]), "{name}");
            count += 1;
            continue;
        }
        for y in 0..h {
            for x in 0..w {
                let e = &expect[(y * w + x) * c..(y * w + x) * c + c];
                let rgb = |ch: usize| e[ch.min(c.min(3) - 1)] as f64;
                match transfer.as_str() {
                    "linear" => {
                        let Pixels::Rgba32F(v) = tex.base() else { panic!("{name}: not float") };
                        for ch in 0..3 {
                            let got = v[(y * w + x) * 4 + ch] as f64;
                            assert!((got - rgb(ch)).abs() < 1e-5, "{name} ({x},{y}) {ch}: {got} vs {}", rgb(ch));
                        }
                    }
                    _ => {
                        let px = &rgba8(&tex)[(y * w + x) * 4..(y * w + x) * 4 + 4];
                        for (ch, &got) in px.iter().take(3).enumerate() {
                            let want = if transfer == "log" {
                                // Cineon files in these tests store 10-bit
                                // codes scaled to their own depth.
                                srgb(log_to_linear(rgb(ch) * 1023.0)) * 255.0
                            } else {
                                rgb(ch) * 255.0
                            };
                            assert!((got as f64 - want).abs() <= 1.01, "{name} ({x},{y}) {ch}: {got} vs {want:.2}");
                        }
                        if c == 4 {
                            assert!((px[3] as f64 - e[3] as f64 * 255.0).abs() <= 0.51, "{name} alpha ({x},{y})");
                        } else {
                            assert_eq!(px[3], 255, "{name}");
                        }
                    }
                }
            }
        }
        count += 1;
    }
    assert!(count >= 30, "{count}");
}

#[test]
fn header_fields_are_listed() {
    let tex = decode_as(Format::Dpx, &read("dpx_10_fields.dpx"), &DecodeOptions::full()).unwrap();
    let find = |k: &str| tex.source.header.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    assert_eq!(find("Frame position"), Some("7"));
    assert_eq!(find("Sequence length"), Some("100"));
    assert_eq!(find("Film frame rate"), Some("24"));
    assert_eq!(find("Slate"), Some("Slate here"));
    assert_eq!(find("Time code"), Some("01:02:03:04"));
    assert!(find("Creator").is_some_and(|c| c.starts_with("OpenImageIO")), "{:?}", tex.source.header);
    assert!(find("Image file name").is_some_and(|c| c.ends_with("dpx_10_fields.dpx")));
    assert_eq!(find("Frame ID"), Some("FID 1"));
    assert_eq!(find("Bit depth"), Some("10"));
    assert_eq!(tex.source.container, "DPX");
}

#[test]
fn undersized_and_reduced_decodes() {
    // A thumbnail-sized decode averages boxes in linear light.
    let bytes = read("dpx_16_be.dpx");
    let full = decode_as(Format::Dpx, &bytes, &DecodeOptions::full()).unwrap();
    let small = decode_as(Format::Dpx, &bytes, &DecodeOptions::preview(8)).unwrap();
    // 32x20 shrunk by 4 (32/8).
    assert_eq!((small.width(), small.height()), (8, 5));
    let (f, s) = (rgba8(&full), rgba8(&small));
    let lin = |v: u8| srgb_to_linear(v as f64 / 255.0);
    for c in 0..3 {
        let mut sum = 0.0;
        for dy in 0..4 {
            for dx in 0..4 {
                sum += lin(f[((8 + dy) * 32 + 4 + dx) * 4 + c]);
            }
        }
        let want = srgb(sum / 16.0) * 255.0;
        let got = s[(2 * 8 + 1) * 4 + c] as f64;
        assert!((got - want).abs() <= 1.5, "{c}: {got} vs {want}");
    }
}

fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

#[test]
fn files_from_other_programs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/science-extra");
    let mut checked = 0;
    for sub in ["dpx", "cineon", "dpx/unclear/nuke-extracted"] {
        let Ok(entries) = std::fs::read_dir(root.join(sub)) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
            if ext != "dpx" && ext != "cin" {
                continue;
            }
            let tex =
                decode_file(&path, &DecodeOptions::preview(64)).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let thumb = slerp_textures::thumbnail(&tex, 64);
            assert!(thumb.pixels.iter().step_by(4).any(|&v| v > 20), "{} is black", path.display());
            checked += 1;
        }
    }
    eprintln!("checked {checked} downloaded DPX and Cineon files");
}

/*┌─────────────────╮
| PAM and farbfeld|
└─────────────────╯*/

fn to8(v: u16) -> u8 {
    ((v as u32 * 255 + 32767) / 65535) as u8
}

#[test]
fn pam_variants() {
    let rgba = read("rgba8.raw");
    let wide: Vec<u16> = read("rgba16.raw").chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
    let n = 32 * 20;
    let decode = |name: &str| {
        decode_as(Format::Pam, &read(name), &DecodeOptions::full()).unwrap_or_else(|e| panic!("{name}: {e}"))
    };
    let t = decode("pam_rgba8.pam");
    assert_eq!(rgba8(&t), &rgba[..]);
    assert!(t.has_alpha);
    let t = decode("pam_rgb16.pam");
    for i in 0..n {
        for c in 0..3 {
            assert_eq!(rgba8(&t)[i * 4 + c], to8(wide[i * 4 + c]), "pixel {i}");
        }
        assert_eq!(rgba8(&t)[i * 4 + 3], 255);
    }
    let t = decode("pam_gray8.pam");
    for i in 0..n {
        assert_eq!(&rgba8(&t)[i * 4..i * 4 + 4], &[rgba[i * 4], rgba[i * 4], rgba[i * 4], 255]);
    }
    let t = decode("pam_graya8.pam");
    for i in 0..n {
        assert_eq!(&rgba8(&t)[i * 4..i * 4 + 4], &[rgba[i * 4], rgba[i * 4], rgba[i * 4], rgba[i * 4 + 1]]);
    }
    let t = decode("pam_bw.pam");
    for i in 0..n {
        let v = if rgba[i * 4] > 127 { 255 } else { 0 };
        assert_eq!(&rgba8(&t)[i * 4..i * 4 + 3], &[v, v, v]);
    }
    let t = decode("pam_gray16.pam");
    assert!(t.source.format.ends_with("(data)"), "{}", t.source.format);
    let Pixels::Rgba32F(v) = t.base() else { panic!() };
    for i in 0..n {
        assert!((v[i * 4] - wide[i * 4] as f32 / 65535.0).abs() < 1e-6);
    }
}

#[test]
fn farbfeld_matches() {
    let wide: Vec<u16> = read("rgba16.raw").chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
    let t = decode_as(Format::Farbfeld, &read("farbfeld.ff"), &DecodeOptions::full()).unwrap();
    for (i, v) in rgba8(&t).iter().enumerate() {
        assert_eq!(*v, to8(wide[i]), "sample {i}");
    }
    assert_eq!(t.source.container, "farbfeld");
}

#[test]
fn magic_numbers_pick_the_format() {
    for (name, format) in [
        ("dpx_10_be_a.dpx", Format::Dpx),
        ("dpx_10_le_a.dpx", Format::Dpx),
        ("cin_10_rgb.cin", Format::Cineon),
        ("cin_10_rgb_le.cin", Format::Cineon),
        ("pam_rgba8.pam", Format::Pam),
        ("farbfeld.ff", Format::Farbfeld),
    ] {
        assert_eq!(slerp_textures::sniff(&read(name)[..32]), Some(format), "{name}");
    }
}

/*┌─────────────────╮
| Chroma and luma |
└─────────────────╯*/

/// Packs 10-bit samples three to a big-endian word, method A, as a row.
fn pack_10(samples: &[u32]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in samples.chunks(3) {
        let mut w = 0u32;
        for (k, v) in chunk.iter().enumerate() {
            w |= v << (22 - 10 * k);
        }
        out.extend(w.to_be_bytes());
    }
    out
}

/// A big-endian 10-bit DPX built from `dpx_10_be_a.dpx`'s header with a new
/// descriptor and samples.
fn ycbcr_file(descriptor: u8, rows: &[Vec<u8>]) -> Vec<u8> {
    let base = read("dpx_10_be_a.dpx");
    let offset = u32::from_be_bytes([base[4], base[5], base[6], base[7]]) as usize;
    let mut f = base[..offset].to_vec();
    f[0x320] = descriptor;
    // User-defined transfer, Rec. 709 colorimetry, no end-of-line padding.
    f[0x321] = 0;
    f[0x322] = 4;
    f[0x324 + 8..0x324 + 12].copy_from_slice(&[0; 4]);
    for r in rows {
        f.extend(r);
    }
    f
}

/// The Y, Cb and Cr codes (0..1023, full range, Rec. 709) of an RGB triple
/// of codes.
fn to_ycbcr(r: f64, g: f64, b: f64) -> [f64; 3] {
    let (r, g, b) = (r / 1023.0, g / 1023.0, b / 1023.0);
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let cb = (b - y) / (2.0 * (1.0 - 0.0722)) + 0.5;
    let cr = (r - y) / (2.0 * (1.0 - 0.2126)) + 0.5;
    [y, cb, cr].map(|v| (v * 1023.0).round().clamp(0.0, 1023.0))
}

#[test]
fn chroma_luma_elements_come_out_as_the_colours_they_encode() {
    let (w, h) = (32usize, 20usize);
    let source = floats("dpx_10_be_a.f32");
    let codes = |x: usize, y: usize, c: usize| (source[(y * w + x) * 3 + c] as f64 * 1023.0).round();
    // 4:4:4: Cb Y Cr for each pixel.
    let rows: Vec<Vec<u8>> = (0..h)
        .map(|y| {
            let mut s = Vec::new();
            for x in 0..w {
                let [yy, cb, cr] = to_ycbcr(codes(x, y, 0), codes(x, y, 1), codes(x, y, 2));
                s.extend([cb as u32, yy as u32, cr as u32]);
            }
            pack_10(&s)
        })
        .collect();
    let tex = decode_as(Format::Dpx, &ycbcr_file(102, &rows), &DecodeOptions::full()).unwrap();
    let px = rgba8(&tex);
    let mut worst = 0i32;
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let want = srgb(srgb_to_linear(codes(x, y, c) / 1023.0)) * 255.0;
                worst = worst.max((px[(y * w + x) * 4 + c] as f64 - want).abs().round() as i32);
            }
        }
    }
    assert!(worst <= 4, "4:4:4 worst error {worst}");
    // 4:2:2: Cb Y0 Cr Y1 per pair, chroma averaged over the pair; a smooth
    // picture comes back close.
    let rows: Vec<Vec<u8>> = (0..h)
        .map(|y| {
            let mut s = Vec::new();
            for x in (0..w).step_by(2) {
                let a = to_ycbcr(codes(x, y, 0), codes(x, y, 1), codes(x, y, 2));
                let b = to_ycbcr(codes(x + 1, y, 0), codes(x + 1, y, 1), codes(x + 1, y, 2));
                s.extend([((a[1] + b[1]) / 2.0) as u32, a[0] as u32, ((a[2] + b[2]) / 2.0) as u32, b[0] as u32]);
            }
            pack_10(&s)
        })
        .collect();
    let tex = decode_as(Format::Dpx, &ycbcr_file(100, &rows), &DecodeOptions::full()).unwrap();
    let px = rgba8(&tex);
    let mut total = 0.0;
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let want = srgb(srgb_to_linear(codes(x, y, c) / 1023.0)) * 255.0;
                total += (px[(y * w + x) * 4 + c] as f64 - want).abs();
            }
        }
    }
    let mean = total / (w * h * 3) as f64;
    assert!(mean < 6.0, "4:2:2 mean error {mean}");
}
