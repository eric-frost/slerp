use super::*;
use exr::image::{AnyChannel, AnyChannels, Blocks, Encoding, FlatSamples, Image, Layer as ExrLayer};
use exr::math::Vec2;
use exr::meta::attribute::{LineOrder, Text};
use exr::meta::header::{ImageAttributes, LayerAttributes};
use exr::prelude::WritableImage;
use std::io::Cursor;

/// An image of named float channels, written with `compression`.
fn write(w: usize, h: usize, channels: &[(&str, Vec<f32>)], compression: Compression, tiles: bool) -> Vec<u8> {
    let list = channels.iter().map(|(n, v)| AnyChannel::new(*n, FlatSamples::F32(v.clone()))).collect();
    let blocks = if tiles { Blocks::Tiles(Vec2(16, 16)) } else { Blocks::ScanLines };
    let encoding = Encoding { compression, blocks, line_order: LineOrder::Increasing };
    let layer = ExrLayer::new((w, h), LayerAttributes::default(), encoding, AnyChannels::sort(list));
    let mut out = Cursor::new(Vec::new());
    Image::from_layer(layer).write().to_buffered(&mut out).unwrap();
    out.into_inner()
}

fn ramp(w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> Vec<f32> {
    (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| f(x, y)).collect()
}

fn rgba(w: usize, h: usize, compression: Compression, tiles: bool) -> Vec<u8> {
    let r = ramp(w, h, |x, _| x as f32 / w as f32);
    let g = ramp(w, h, |_, y| y as f32 * 2.0);
    let b = ramp(w, h, |x, y| ((x + y) % 3) as f32);
    let a = ramp(w, h, |x, _| if x < w / 2 { 1.0 } else { 0.25 });
    write(w, h, &[("R", r), ("G", g), ("B", b), ("A", a)], compression, tiles)
}

fn pixel(p: &Picture, x: usize, y: usize) -> [f32; 4] {
    let l = &p.levels[0];
    let i = (y * l.width as usize + x) * 4;
    [l.rgba[i], l.rgba[i + 1], l.rgba[i + 2], l.rgba[i + 3]]
}

#[test]
fn every_writable_compression_round_trips() {
    for c in [
        Compression::Uncompressed,
        Compression::RLE,
        Compression::ZIP1,
        Compression::ZIP16,
        Compression::PIZ,
        Compression::PXR24,
        Compression::B44,
    ] {
        for tiles in [false, true] {
            let bytes = rgba(37, 21, c, tiles);
            let exr = open(Cursor::new(&bytes)).unwrap();
            assert_eq!(exr.parts.len(), 1);
            assert_eq!(exr.parts[0].layers[0].channel_text(), "RGBA");
            assert_eq!(exr.default_choice(), Some((0, 0)));
            let p = decode(Cursor::new(&bytes), 0, 0, &Options::default()).unwrap();
            assert_eq!((p.levels[0].width, p.levels[0].height), (37, 21), "{c:?}");
            assert!(p.has_alpha && p.meaning == Meaning::Colour);
            let px = pixel(&p, 20, 10);
            // PXR24 keeps 24 bits of a float, and B44 is lossy for halfs
            // only, which these aren't.
            let tol = 1e-3;
            assert!((px[0] - 20.0 / 37.0).abs() < tol, "{c:?} {tiles} {px:?}");
            assert!((px[1] - 20.0).abs() < tol * 20.0, "{c:?} {tiles} {px:?}");
            assert!((px[2] - 0.0).abs() < tol, "{c:?} {tiles} {px:?}");
            assert!((px[3] - 0.25).abs() < tol, "{c:?} {tiles} {px:?}");
        }
    }
}

#[test]
fn layers_group_by_prefix_and_data_shows_grey() {
    let (w, h) = (8, 4);
    let bytes = write(
        w,
        h,
        &[
            ("diffuse.R", ramp(w, h, |_, _| 1.0)),
            ("diffuse.G", ramp(w, h, |_, _| 0.5)),
            ("diffuse.B", ramp(w, h, |_, _| 0.25)),
            ("Z", ramp(w, h, |x, _| x as f32 * 10.0)),
            ("motion.u", ramp(w, h, |_, _| 3.0)),
            ("motion.v", ramp(w, h, |_, _| -3.0)),
        ],
        Compression::ZIP16,
        false,
    );
    let exr = open(Cursor::new(&bytes)).unwrap();
    let labels: Vec<String> = exr.choices().into_iter().map(|c| c.2).collect();
    assert_eq!(labels, ["Z", "diffuse (RGB)", "motion (uv)"]);
    // The named colour layer wins over the unnamed data layer.
    let (p, l) = exr.default_choice().unwrap();
    assert_eq!(exr.parts[p].layers[l].name, "diffuse");
    let z = decode(Cursor::new(&bytes), 0, 0, &Options::default()).unwrap();
    assert_eq!(z.meaning, Meaning::Data);
    assert!(!z.has_alpha);
    assert_eq!(pixel(&z, 3, 1), [30.0, 30.0, 30.0, 1.0]);
    let m = decode(Cursor::new(&bytes), 0, 2, &Options::default()).unwrap();
    assert_eq!(pixel(&m, 0, 0), [3.0, -3.0, 0.0, 1.0]);
    let d = decode(Cursor::new(&bytes), 0, 1, &Options::default()).unwrap();
    assert_eq!(pixel(&d, 7, 3), [1.0, 0.5, 0.25, 1.0]);
}

#[test]
fn luminance_alone_is_grey_and_with_chroma_is_colour() {
    let (w, h) = (4, 4);
    let bytes = write(w, h, &[("Y", ramp(w, h, |_, _| 0.5))], Compression::ZIP1, false);
    let p = decode_default(&bytes, None, MAX_PIXELS).unwrap();
    assert_eq!(p.meaning, Meaning::Luminance);
    assert_eq!(pixel(&p, 1, 1), [0.5, 0.5, 0.5, 1.0]);
    // Full-resolution chroma: R = (RY + 1) Y, B = (BY + 1) Y.
    let bytes = write(
        w,
        h,
        &[("Y", ramp(w, h, |_, _| 0.5)), ("RY", ramp(w, h, |_, _| 1.0)), ("BY", ramp(w, h, |_, _| -0.5))],
        Compression::ZIP1,
        false,
    );
    let p = decode_default(&bytes, None, MAX_PIXELS).unwrap();
    assert_eq!(p.meaning, Meaning::LuminanceChroma);
    let [r, g, b, _] = pixel(&p, 2, 2);
    assert!((r - 1.0).abs() < 1e-5 && (b - 0.25).abs() < 1e-5);
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    assert!((y - 0.5).abs() < 1e-4, "{r} {g} {b}");
}

#[test]
fn rec709_weights_come_out_of_the_chromaticities() {
    let m = [[0.64 / 0.33, 0.3 / 0.6, 0.15 / 0.06], [1.0, 1.0, 1.0], [0.03 / 0.33, 0.1 / 0.6, 0.79 / 0.06]];
    let w = [0.3127 / 0.329, 1.0, (1.0 - 0.3127 - 0.329) / 0.329];
    let s = solve3(m, w).unwrap();
    assert!((s[0] - 0.2126).abs() < 1e-3 && (s[1] - 0.7152).abs() < 1e-3 && (s[2] - 0.0722).abs() < 1e-3, "{s:?}");
}

#[test]
fn multi_part_files_list_every_part() {
    let (w, h) = (6, 5);
    let make = |name: &str, v: f32| {
        let list = ["R", "G", "B"].iter().map(|c| AnyChannel::new(*c, FlatSamples::F32(vec![v; w * h]))).collect();
        ExrLayer::new(
            (w, h),
            LayerAttributes::named(Text::from(name)),
            Encoding::FAST_LOSSLESS,
            AnyChannels::sort(list),
        )
    };
    let image = Image::from_layers(
        ImageAttributes::new(IntegerBounds::new((0, 0), (w, h))),
        vec![make("left", 0.25), make("right", 0.75)],
    );
    let mut out = Cursor::new(Vec::new());
    image.write().to_buffered(&mut out).unwrap();
    let bytes = out.into_inner();
    let exr = open(Cursor::new(&bytes)).unwrap();
    let labels: Vec<String> = exr.choices().into_iter().map(|c| c.2).collect();
    assert_eq!(labels, ["left: RGB", "right: RGB"]);
    let p = decode(Cursor::new(&bytes), 1, 0, &Options::default()).unwrap();
    assert_eq!(pixel(&p, 5, 4), [0.75, 0.75, 0.75, 1.0]);
    assert_eq!(exr.parts[1].name.as_deref(), Some("right"));
}

#[test]
fn shrinking_keeps_the_shape() {
    let bytes = rgba(300, 100, Compression::ZIP16, false);
    let p = decode(Cursor::new(&bytes), 0, 0, &Options { max_side: Some(64), ..Options::default() }).unwrap();
    assert_eq!((p.levels[0].width, p.levels[0].height), (64, 21));
    let p = decode(Cursor::new(&bytes), 0, 0, &Options { max_pixels: 1000, ..Options::default() });
    assert!(p.unwrap_err().contains("too large"));
}

#[test]
fn display_window_frames_the_data() {
    // Data at (2, 1) inside a 10 × 6 display window.
    let (w, h) = (4, 3);
    let list = ["R", "G", "B"].iter().map(|c| AnyChannel::new(*c, FlatSamples::F32(vec![1.0; w * h]))).collect();
    let attrs = LayerAttributes { layer_position: Vec2(2, 1), ..LayerAttributes::default() };
    let layer = ExrLayer::new((w, h), attrs, Encoding::UNCOMPRESSED, AnyChannels::sort(list));
    let image = Image::new(ImageAttributes::new(IntegerBounds::new((0, 0), (10, 6))), layer);
    let mut out = Cursor::new(Vec::new());
    image.write().to_buffered(&mut out).unwrap();
    let p = decode_default(out.get_ref(), None, MAX_PIXELS).unwrap();
    assert_eq!((p.levels[0].width, p.levels[0].height), (10, 6));
    assert_eq!(pixel(&p, 0, 0), [0.0; 4]);
    assert_eq!(pixel(&p, 2, 1), [1.0, 1.0, 1.0, 1.0]);
    assert_eq!(pixel(&p, 5, 3), [1.0, 1.0, 1.0, 1.0]);
    assert_eq!(pixel(&p, 6, 3), [0.0; 4]);
    assert_eq!(p.notes.len(), 1);
}

/// Damaged files give errors or partial pictures, never panics or hangs.
#[test]
fn damaged_files_never_panic() {
    let good = rgba(40, 30, Compression::PIZ, false);
    for cut in [0, 4, 8, 20, 100, good.len() / 2, good.len() - 1] {
        let _ = open(Cursor::new(&good[..cut]));
        let _ = decode(Cursor::new(&good[..cut]), 0, 0, &Options::default());
    }
    let mut seed = 0x1234_5678u32;
    for _ in 0..300 {
        let mut bad = good.clone();
        for _ in 0..4 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let i = seed as usize % bad.len();
            bad[i] = (seed >> 24) as u8;
        }
        let _ = decode(Cursor::new(&bad), 0, 0, &Options { max_pixels: 1 << 20, ..Options::default() });
    }
    // A data window of 2^30 × 2^30 pixels is refused before allocating.
    let mut huge = rgba(4, 4, Compression::Uncompressed, false);
    let at = huge.windows(10).position(|w| w == b"dataWindow").unwrap();
    // name, type "box2i", size, then xMin yMin xMax yMax.
    let values = at + b"dataWindow\0box2i\0".len() + 4;
    huge[values + 8..values + 12].copy_from_slice(&(1i32 << 30).to_le_bytes());
    huge[values + 12..values + 16].copy_from_slice(&(1i32 << 30).to_le_bytes());
    assert!(decode(Cursor::new(&huge), 0, 0, &Options::default()).is_err());
}

/// The official luminance-chroma sample matches the same picture stored as
/// RGB, within what 2 × 2 chroma subsampling loses.  Skipped without the
/// test corpus (`tools/fetch-test-assets.ts`).
#[test]
fn chroma_subsampled_matches_rgb() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../local/assets/images23-extra/exr/openexr-images/Chromaticities");
    let (Ok(yc), Ok(rgb)) = (std::fs::read(dir.join("Rec709_YC.exr")), std::fs::read(dir.join("Rec709.exr"))) else {
        return;
    };
    let opts = Options { place: false, ..Options::default() };
    let a = decode(Cursor::new(&yc), 0, 0, &opts).unwrap();
    let b = decode(Cursor::new(&rgb), 0, 0, &opts).unwrap();
    assert_eq!(a.meaning, Meaning::LuminanceChroma);
    let (la, lb) = (&a.levels[0], &b.levels[0]);
    assert_eq!((la.width, la.height), (lb.width, lb.height));
    let mut err = 0f64;
    for (x, y) in la.rgba.iter().zip(&lb.rgba) {
        err += ((x - y).abs() / y.abs().max(0.05)) as f64;
    }
    let mean = err / la.rgba.len() as f64;
    assert!(mean < 0.05, "mean relative difference {mean}");
}

/// A gzipped PFM reference: width, height, channels, rows from the top.
fn pfm(path: &std::path::Path) -> Option<(usize, usize, usize, Vec<f32>)> {
    let gz = std::fs::read(path).ok()?;
    let d = miniz_oxide::inflate::decompress_to_vec(gz.get(10..gz.len().checked_sub(8)?)?).ok()?;
    let text = String::from_utf8_lossy(&d[..d.len().min(64)]).into_owned();
    let mut words = text.split_ascii_whitespace();
    let ch = if words.next()? == "PF" { 3 } else { 1 };
    let (w, h): (usize, usize) = (words.next()?.parse().ok()?, words.next()?.parse().ok()?);
    let scale = words.next()?;
    let at = d.len() - w * h * ch * 4;
    let le = scale.starts_with('-');
    let mut px = vec![0f32; w * h * ch];
    for y in 0..h {
        for x in 0..w * ch {
            let i = at + ((h - 1 - y) * w * ch + x) * 4;
            let b = [d[i], d[i + 1], d[i + 2], d[i + 3]];
            px[y * w * ch + x] = if le { f32::from_le_bytes(b) } else { f32::from_be_bytes(b) };
        }
    }
    Some((w, h, ch, px))
}

/// The corpus's files against the OpenEXR library's decodes of them
/// (`local/assets/images23-extra/exr/refs`): every compression, sample type,
/// window and level layout.  Skipped without the corpus.
#[test]
fn corpus_matches_openexr() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/images23-extra/exr");
    let mut checked = 0;
    for (files, refs) in [
        ("generated", "refs/generated"),
        ("openexr-images/ScanLines", "refs/openexr-images/ScanLines"),
        ("openexr-images/TestImages", "refs/openexr-images/TestImages"),
        ("openexr-images/MultiResolution", "refs/openexr-images/MultiResolution"),
        ("openexr-images/DisplayWindow", "refs/openexr-images/DisplayWindow"),
        ("openexr-images/Chromaticities", "refs/openexr-images/Chromaticities"),
    ] {
        let Ok(dir) = std::fs::read_dir(root.join(files)) else { continue };
        for f in dir.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "exr")) {
            let stem = f.file_stem().unwrap().to_string_lossy().to_string();
            let Some((w, h, ch, rgb)) = pfm(&root.join(refs).join(format!("{stem}.ref.pfm.gz"))) else { continue };
            let bytes = std::fs::read(&f).unwrap();
            let exr = open(Cursor::new(&bytes)).unwrap();
            let (p, l) = exr.default_choice().unwrap();
            let pic = decode(Cursor::new(&bytes), p, l, &Options { place: false, to_srgb: false, ..Options::default() }).unwrap();
            let lv = &pic.levels[0];
            assert_eq!((lv.width as usize, lv.height as usize), (w, h), "{stem}");
            let off = (0..w * h * 3)
                .filter(|&i| {
                    let (want, have) = (rgb[(i / 3) * ch + (i % 3).min(ch - 1)], lv.rgba[(i / 3) * 4 + i % 3]);
                    !(want == have
                        || want.is_nan() && have.is_nan()
                        || (want - have).abs() <= 1e-3 * want.abs().max(1.0))
                })
                .count();
            assert_eq!(off, 0, "{stem}: {off} samples differ");
            checked += 1;
        }
    }
    assert!(checked == 0 || checked > 90, "{checked} files checked");
}

#[test]
fn wide_gamut_primaries_convert_to_srgb() {
    use exr::meta::attribute::Chromaticities;
    let c = |p: [f32; 8]| Chromaticities { red: Vec2(p[0], p[1]), green: Vec2(p[2], p[3]), blue: Vec2(p[4], p[5]), white: Vec2(p[6], p[7]) };
    assert!(to_srgb_matrix(&c([0.64, 0.33, 0.3, 0.6, 0.15, 0.06, 0.3127, 0.329])).is_none());
    // Rec. 2020: its pure red lies outside sRGB (red above 1, green and
    // blue below 0), and white stays white.
    let m = to_srgb_matrix(&c([0.708, 0.292, 0.170, 0.797, 0.131, 0.046, 0.3127, 0.329])).unwrap();
    assert!(m[0][0] > 1.5 && m[1][0] < 0.0);
    for row in m {
        assert!((row.iter().sum::<f32>() - 1.0).abs() < 1e-3);
    }
    // ACES AP0 with its own white converts white to white too.
    let m = to_srgb_matrix(&c([0.7347, 0.2653, 0.0, 1.0, 0.0001, -0.077, 0.32168, 0.33767])).unwrap();
    for row in m {
        assert!((row.iter().sum::<f32>() - 1.0).abs() < 1e-3, "{m:?}");
    }
}

/// Found by fuzzing: the generated DWAA sample with a block whose counters
/// claim about 10^17 AC coefficients, which `exr` allocated before checking
/// (an allocation failure aborts the process).  The block is now skipped.
#[test]
fn dwa_block_claiming_too_much_is_skipped() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../local/assets/images23-extra/exr/generated/im-rgb-dwaa.exr");
    let Ok(mut bytes) = std::fs::read(&path) else { return };
    for (at, value) in [(453, 1), (455, 224), (457, 0), (458, 0), (38785, 50), (48595, 50), (50761, 50)] {
        bytes[at] = value;
    }
    let _ = decode_default(&bytes, Some(256), 1 << 24);
}
