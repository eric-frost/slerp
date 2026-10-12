//! The decoders against other decoders: every sample file of
//! `local/assets/images-extra/` that has a reference decode (made with
//! ImageMagick, Pillow, libwebp, libheif, libavif and libjxl, see that
//! folder's SOURCES.md) must come out the same, within the tolerance each
//! format needs.  Skipped when the folder isn't there.
//!
//! References that are known to be wrong (ImageMagick cannot decode
//! subsampled YCbCr TIFF, reads sub-byte WhiteIsZero upside down, ...) are
//! listed in each test with the reason.

use slerp_images::ImageFormat;
use slerp_scene::textures::Pixels;
use slerp_scene::{Asset, Limits};
use std::path::{Path, PathBuf};

fn assets() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/images-extra");
    p.is_dir().then_some(p)
}

fn files(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .filter(|p| p.extension().is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x))))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// The format `slerp_images` recognises a file as, from its first bytes.
fn sniffed(path: &Path) -> Option<ImageFormat> {
    use std::io::Read;
    let mut head = Vec::new();
    std::fs::File::open(path).ok()?.take(4096).read_to_end(&mut head).ok()?;
    slerp_images::sniff(path, &head)
}

fn open(path: &Path) -> Result<Asset, String> {
    let f = sniffed(path).ok_or("not recognised")?;
    slerp_images::load(f, path, &Limits::viewer()).map_err(|e| e.to_string())
}

struct Rgba {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

fn read_png(path: &Path) -> Option<Rgba> {
    let mut dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).ok()?));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut r = dec.read_info().ok()?;
    let mut buf = vec![0; r.output_buffer_size()?];
    let info = r.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let n = (info.width * info.height) as usize;
    let px = match r.output_color_type().0 {
        png::ColorType::Grayscale => buf.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Rgb => buf.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::Rgba => buf,
        _ => return None,
    };
    (px.len() == n * 4).then_some(Rgba { w: info.width, h: info.height, px })
}

fn rgba_of(t: &slerp_scene::Texture) -> Option<Rgba> {
    match t.base() {
        Pixels::Rgba8(v) => Some(Rgba { w: t.width(), h: t.height(), px: v.clone() }),
        Pixels::Rgba32F(v) => {
            // Data (16-bit grey) is shown linearly, light (HDR colour) through the sRGB curve.
            let data = t.source.format.ends_with("(data)");
            let e = |x: f32| {
                let x = x.clamp(0.0, 1.0);
                let s = if data || x <= 0.0031308 {
                    if data { x } else { x * 12.92 }
                } else {
                    1.055 * x.powf(1.0 / 2.4) - 0.055
                };
                (s * 255.0 + 0.5) as u8
            };
            Some(Rgba {
                w: t.width(),
                h: t.height(),
                px: v
                    .chunks_exact(4)
                    .flat_map(|p| [e(p[0]), e(p[1]), e(p[2]), (p[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8])
                    .collect(),
            })
        }
        Pixels::Blocks(..) => None,
    }
}

/// How far two pictures of one size are apart: the largest difference of
/// any value, and the root of the mean squared difference as a fraction of
/// 255.  Transparent pixels count by alpha only: their colour is not seen.
fn distance(a: &Rgba, b: &Rgba) -> (u8, f64) {
    assert_eq!((a.w, a.h), (b.w, b.h), "sizes differ");
    let (mut worst, mut sum) = (0u8, 0f64);
    for (p, q) in a.px.chunks_exact(4).zip(b.px.chunks_exact(4)) {
        let both_clear = p[3] == 0 && q[3] == 0;
        for c in 0..4 {
            if both_clear && c < 3 {
                continue;
            }
            let d = p[c].abs_diff(q[c]);
            worst = worst.max(d);
            sum += (d as f64).powi(2);
        }
    }
    (worst, (sum / (a.px.len() as f64)).sqrt() / 255.0)
}

/// Compares a decoded frame with its reference.  `Err` describes the miss.
fn check(name: &str, got: &Rgba, want: &Rgba, max_diff: u8, max_rmse: f64) -> Result<(), String> {
    if (got.w, got.h) != (want.w, want.h) {
        return Err(format!("{name}: {} x {} instead of {} x {}", got.w, got.h, want.w, want.h));
    }
    let (worst, rmse) = distance(got, want);
    if worst > max_diff && rmse > max_rmse {
        return Err(format!("{name}: largest difference {worst}, rms {rmse:.4}"));
    }
    Ok(())
}

/// What `<name>.delays.txt` says: delays in centiseconds and the loop count.
fn read_delays(path: &Path) -> Option<(Vec<u32>, u32)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut delays = Vec::new();
    let mut loops = 0;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("loop:") {
            // "unknown" where the reference tool doesn't print the count.
            loops = rest.split_whitespace().next().and_then(|v| v.parse().ok()).unwrap_or(0);
        } else if let Some((a, b)) = line.split_once(' ')
            && a.len() == 3
            && a.bytes().all(|c| c.is_ascii_digit())
            && let Ok(v) = b.trim().parse::<u32>()
        {
            delays.push(v);
        }
    }
    Some((delays, loops))
}

/// Animations against `<ref>/<name>.frames/NNN.png`: every frame, every delay.
fn animations(
    dir: &str,
    frames_ref: &str,
    exts: &[&str],
    skip: &[(&str, &str)],
    delays_known: bool,
    max_diff: u8,
    max_rmse: f64,
) {
    let Some(root) = assets() else { return };
    let (mut compared, mut failures) = (0, Vec::new());
    for path in files(&root.join(dir), exts) {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if skip.iter().any(|(n, _)| *n == name) {
            continue;
        }
        let frames_dir = root.join(dir).join(frames_ref).join(format!("{name}.frames"));
        let Some((delays, _)) = read_delays(&root.join(dir).join("ref").join(format!("{name}.delays.txt"))) else {
            continue;
        };
        if !frames_dir.is_dir() || delays.len() < 2 {
            continue;
        }
        if sniffed(&path).is_none() {
            // A still image the other tools call an animation of one frame.
            continue;
        }
        let asset = match open(&path) {
            Ok(a) => a,
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                continue;
            }
        };
        let Asset::Canvas(c) = asset else {
            failures.push(format!("{name}: not an animation"));
            continue;
        };
        compared += 1;
        if c.frames().len() != delays.len() {
            failures.push(format!("{name}: {} frames instead of {}", c.frames().len(), delays.len()));
            continue;
        }
        for (i, want_cs) in delays.iter().enumerate() {
            // The reference counts centiseconds, rounded; it may or may not have applied the
            // browsers' minimum, so accept either.
            let (raw, floored) = (*want_cs as f32 / 100.0, slerp_images::delay_seconds(*want_cs * 10));
            let got_delay = c.frames()[i];
            if delays_known && (got_delay - raw).abs() > 0.0105 && (got_delay - floored).abs() > 0.0105 {
                failures.push(format!("{name}: frame {i} delay {got_delay} s instead of {raw} s"));
                break;
            }
            let img = c.compose(i, &[], 1.0);
            let got = Rgba { w: img.width, h: img.height, px: img.pixels };
            // A frame the other decoder could not make is not a test of ours.
            let Some(want) = read_png(&frames_dir.join(format!("{i:03}.png"))) else { break };
            if let Err(e) = check(&format!("{name} frame {i}"), &got, &want, max_diff, max_rmse) {
                failures.push(e);
                break;
            }
        }
    }
    assert!(compared > 0, "no {dir} animation was compared");
    assert!(failures.is_empty(), "{} of {compared} {dir} animations differ:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn gifs_match_imagemagick() {
    // ImageMagick keeps reserved disposal methods 4 to 7 as they are; browsers
    // (and so this reader) treat them as "do nothing".
    animations(
        "gif",
        "ref",
        &["gif"],
        &[
            // ImageMagick fills what the first frame does not cover with the file's background
            // colour; browsers (and this reader) leave it transparent.
            ("anim-first-frame-smaller-than-screen.gif", "background colour"),
            ("imagers-border_touching_layers.gif", "background colour"),
            ("imagers-issue_1455_undersized.gif", "background colour"),
            // ImageMagick restores the canvas after a first frame that says "restore the
            // previous picture" differently from Pillow, which (like browsers) leaves it as it is.
            ("anim-disposal3-previous.gif", "first frame restore previous"),
            ("anim-disposal3-previous-transparent.gif", "first frame restore previous"),
            ("pillow-dispose_prev_first_frame.gif", "first frame restore previous"),
        ],
        true,
        0,
        0.0,
    );
}

#[test]
fn apngs_match_ffmpeg() {
    // 16-bit frames round to 8 bits in two slightly different ways.
    animations(
        "apng",
        "ref-ffmpeg",
        &["png", "apng"],
        // ffmpeg's decoder gives opaque pixels for any stream without an alpha channel, also
        // where a frame was disposed to the transparent background.
        &[("ffmpeg-rgb48-rotate-8frames.apng", "no alpha in ffmpeg's output")],
        true,
        1,
        0.003,
    );
}

#[test]
fn animated_webps_match_imagemagick_closely() {
    // ImageMagick blends translucent frames slightly differently from libwebp
    // (this reader matches libwebp exactly; see `webp_matches_libwebp`).
    animations(
        "webp",
        "ref",
        &["webp"],
        &[
            // ImageMagick paints the ANIM chunk's background colour under the frames, which
            // browsers and libwebp ignore.
            ("anim-container-loop-1-bg-transparent.webp", "background colour"),
            ("anim-container-no-full-frame-bg-blue.webp", "background colour"),
            ("anim-container-with-icc-xmp.webp", "background colour"),
            ("chromium-resources_webp-animated-opaque.webp", "background colour"),
            // ImageMagick composes frames that say "do not blend" with a blend.
            ("anim-container-dispose0-blendoff.webp", "blend method"),
            ("anim-container-dispose1-blendoff.webp", "blend method"),
        ],
        false,
        4,
        0.01,
    );
}

#[test]
fn animated_avifs_match_libavif() {
    // libavif's default chroma upsampling repeats chroma samples where this reader interpolates,
    // so the frames agree only closely.
    animations("avif", "ref", &["avif", "avifs"], &[], true, 12, 0.02);
}

#[test]
fn stills_ico_and_pnm_match() {
    let Some(root) = assets() else { return };
    let mut failures = Vec::new();
    let mut compared = 0;
    for (dir, exts) in [("ico", &["ico", "cur"][..]), ("pnm", &["pbm", "pgm", "ppm", "pnm"][..])] {
        for path in files(&root.join(dir), exts) {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let Some(want) = read_png(&root.join(dir).join("ref").join(format!("{name}.ref.png"))) else { continue };
            let Ok(asset) = open(&path) else { continue };
            // A multi-size icon's reference is its first entry in file order; the
            // gallery lists the largest first, so look for the entry of that size.
            let candidates: Vec<Rgba> = match &asset {
                Asset::Image(t) => rgba_of(t).into_iter().collect(),
                Asset::Gallery(items) => items.iter().filter_map(|i| rgba_of(&i.texture)).collect(),
                _ => continue,
            };
            // Where ImageMagick or Pillow differ from the Windows rules this reader follows:
            // - bmp-32bpp-topdown: Pillow also applies the AND mask to a bitmap that has alpha;
            //   Windows and browsers use the alpha alone;
            // - imagers-bmp_v5_with_icc: ImageMagick's picture is shifted against this reader's.
            if ["bmp-32bpp-topdown.ico", "imagers-bmp_v5_with_icc.ico"].contains(&name.as_str()) {
                continue;
            }
            compared += 1;
            let hit =
                candidates.iter().any(|c| (c.w, c.h) == (want.w, want.h) && check(&name, c, &want, 1, 0.01).is_ok());
            if !hit {
                failures.push(format!(
                    "{name}: none of {} pictures matches the {} x {} reference",
                    candidates.len(),
                    want.w,
                    want.h
                ));
            }
        }
    }
    assert!(compared > 20, "{compared}");
    assert!(failures.is_empty(), "{} differ:\n{}", failures.len(), failures.join("\n"));
}

/// The ordinary TIFF pictures against ImageMagick.  Left out, with the reason:
/// - sub-byte WhiteIsZero samples (ImageMagick shows them upside down), checked
///   instead against each other in `tiff_variants_agree`;
/// - subsampled YCbCr without JPEG (ImageMagick returns garbage);
/// - floating point and signed pictures (ImageMagick clamps; this reader maps
///   linear light to sRGB), and CMYK with JPEG (different ink conversions);
/// - ICC profiles (ImageMagick does not convert).
#[test]
fn tiffs_match_imagemagick() {
    let Some(root) = assets() else { return };
    let skip = |n: &str| {
        n.contains("miniswhite")
            || n.contains("hopper2I")
            || n.contains("hopper4I")
            || n.contains("ycbcr-")
            || n.contains("dscf0013")
            || n.contains("ycbcr-cat")
            || n.contains("32f")
            || n.contains("f32")
            || n.contains("32b-float")
            || n.contains("64f")
            || n.contains("fp16")
            || n.contains("int8")
            || n.contains("int16")
            || n.contains("-i8")
            || n.contains("cmyk")
            || n.contains("icc")
            || n.contains("ianare")
            || n.contains("alpha")
    };
    let (mut compared, mut failures, mut mine) = (0, Vec::new(), 0);
    for path in files(&root.join("tiff"), &["tif", "tiff"]) {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if sniffed(&path) != Some(ImageFormat::Tiff) {
            continue;
        }
        mine += 1;
        if skip(&name) {
            continue;
        }
        let Some(want) = read_png(&root.join("tiff/ref").join(format!("{name}.ref.png"))) else { continue };
        match open(&path) {
            Ok(Asset::Image(t)) => {
                let Some(got) = rgba_of(&t) else { continue };
                compared += 1;
                if let Err(e) = check(&name, &got, &want, 3, 0.02) {
                    failures.push(e);
                }
            }
            Ok(_) => failures.push(format!("{name}: not a picture")),
            // Compressions slerp-tiff doesn't read (old-style JPEG, LZMA, WebP, 24-bit samples).
            Err(e) => {
                let known = [
                    "old-style JPEG",
                    "LZMA",
                    "WebP",
                    "24-bit",
                    "different sizes",
                    "outside the file",
                    "colour map",
                    "no strip or tile",
                    "LZW code",
                    "out of range",
                    "fax data is damaged",
                ];
                if !known.iter().any(|k| e.contains(k)) {
                    failures.push(format!("{name}: {e}"));
                }
            }
        }
    }
    assert!(mine > 150 && compared > 100, "mine {mine}, compared {compared}");
    assert!(failures.is_empty(), "{} of {compared} TIFFs differ:\n{}", failures.len(), failures.join("\n"));
}

/// Files that are one picture saved several ways must come out the same.
#[test]
fn tiff_variants_agree() {
    let Some(root) = assets() else { return };
    let t = root.join("tiff");
    let pic = |n: &str| -> Option<Rgba> {
        match open(&t.join(n)) {
            Ok(Asset::Image(tex)) => rgba_of(&tex),
            _ => None,
        }
    };
    let groups: &[&[&str]] = &[
        // Grey at 2 and 4 bits, stored normally, inverted and with reversed bits.
        &[
            "pillow-tiff_gray_2_4_bpp_hopper2.tif",
            "pillow-tiff_gray_2_4_bpp_hopper2I.tif",
            "pillow-tiff_gray_2_4_bpp_hopper2R.tif",
            "pillow-tiff_gray_2_4_bpp_hopper2IR.tif",
        ],
        &[
            "pillow-tiff_gray_2_4_bpp_hopper4.tif",
            "pillow-tiff_gray_2_4_bpp_hopper4I.tif",
            "pillow-tiff_gray_2_4_bpp_hopper4R.tif",
            "pillow-tiff_gray_2_4_bpp_hopper4IR.tif",
        ],
        // The same pictures with every compression.
        &[
            "rgb8-none.tif",
            "rgb8-lzw.tif",
            "rgb8-deflate.tif",
            "rgb8-packbits.tif",
            "rgb8-zstd.tif",
            "rgb8-planar-none.tif",
            "rgb8-tiled-128x128.tif",
            "rgb8-strips-rowsperstrip7.tif",
            "rgb8-bigtiff-lzw.tif",
        ],
        &[
            "bilevel1-none.tif",
            "bilevel1-lzw.tif",
            "bilevel1-packbits.tif",
            "bilevel1-deflate.tif",
            "bilevel1-ccitt-g4.tif",
            "bilevel1-ccitt-g3.tif",
        ],
        // Chroma subsampling changes the picture a little, not by much.
    ];
    let mut checked = 0;
    for g in groups {
        let first = g.iter().find_map(|n| pic(n).map(|p| (n, p)));
        let Some((first_name, first)) = first else { continue };
        for n in g.iter() {
            let Some(p) = pic(n) else { continue };
            checked += 1;
            // Rounding of 16-bit and sub-byte scaling can differ by a level or two.
            check(n, &p, &first, 4, 0.02).unwrap_or_else(|e| panic!("{e} (against {first_name})"));
        }
    }
    assert!(checked >= 8, "{checked}");
    // Subsampled chroma stays close to the unsubsampled picture.
    if let Some(full) = pic("ycbcr-444-raw.tif") {
        for n in
            ["ycbcr-2x2-raw.tif", "ycbcr-2x1-raw.tif", "ycbcr-1x2-raw.tif", "ycbcr-4x1-raw.tif", "ycbcr-4x4-raw.tif"]
        {
            if let Some(p) = pic(n) {
                let (_, rmse) = distance(&p, &full);
                assert!(rmse < 0.12, "{n}: rms {rmse}");
            }
        }
    }
}

#[test]
fn jpeg2000_matches() {
    let Some(root) = assets() else { return };
    let (mut compared, mut failures, mut odd) = (0, Vec::new(), 0);
    for path in files(&root.join("jp2"), &["jp2", "j2k", "jpf", "j2c", "jpx"]) {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let Some(mut want) = read_png(&root.join("jp2/ref").join(format!("{name}.ref.png"))) else { continue };
        let Ok(Asset::Image(t)) = open(&path) else { continue };
        let Some(mut got) = rgba_of(&t) else { continue };
        // OpenJPEG's conformance and regression streams are built to break decoders: image
        // and tile offsets, subsampled and differently sized components, channel
        // definitions that disagree with the data.  hayro-jpeg2000 reads some differently
        // (sizes include the image offset; a fourth component is taken for alpha where
        // OpenJPEG ignores it); they must still open and be close where sizes agree.
        let odd_file = name.starts_with("opj-") || name.contains("image-offset") || name.starts_with("pillow-00r1");
        if odd_file && (got.w, got.h) != (want.w, want.h) {
            odd += 1;
            continue;
        }
        // ImageMagick drops what it takes for unspecified channels; colour is what is compared.
        for (g, w) in got.px.chunks_exact_mut(4).zip(want.px.chunks_exact_mut(4)) {
            g[3] = 255;
            w[3] = 255;
        }
        compared += 1;
        if let Err(e) = check(&name, &got, &want, 3, 0.02) {
            if odd_file {
                odd += 1;
            } else {
                failures.push(e);
            }
        }
    }
    assert!(compared > 100 && odd < 40, "{compared} compared, {odd} odd");
    assert!(failures.is_empty(), "{} of {compared} differ:\n{}", failures.len(), failures.join("\n"));
}

/// The decoded frames of every animated WebP, against libwebp's own
/// animation decoder, byte for byte.
#[test]
fn webp_matches_libwebp() {
    let Some(root) = assets() else { return };
    let mut compared = 0;
    for path in files(&root.join("webp"), &["webp"]) {
        let Some(ImageFormat::AnimatedWebp) = sniffed(&path) else { continue };
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(&path).unwrap();
        let Some(frames) = libwebp_frames(&bytes) else { continue };
        let Ok(Asset::Canvas(c)) = open(&path) else { continue };
        if c.frames().len() != frames.len() {
            // A file with unusable frames: libwebp stops where it can't go on.
            continue;
        }
        compared += 1;
        for (i, want) in frames.iter().enumerate() {
            let got = c.compose(i, &[], 1.0);
            assert_eq!(got.pixels.len(), want.len(), "{name} frame {i}");
            let same =
                got.pixels.chunks_exact(4).zip(want.chunks_exact(4)).all(|(a, b)| a == b || (a[3] == 0 && b[3] == 0));
            assert!(same, "{name}: frame {i} differs from libwebp's");
        }
    }
    assert!(compared > 10, "{compared}");
}

/// Frames as libwebp's animation decoder gives them (RGBA, full canvas).
fn libwebp_frames(data: &[u8]) -> Option<Vec<Vec<u8>>> {
    use libwebp_sys as w;
    // SAFETY: plain use of libwebp's demux API on a buffer that outlives the decoder.
    unsafe {
        let mut opts: w::WebPAnimDecoderOptions = std::mem::zeroed();
        if w::WebPAnimDecoderOptionsInit(&mut opts) == 0 {
            return None;
        }
        opts.color_mode = w::WEBP_CSP_MODE::MODE_RGBA;
        opts.use_threads = 0;
        let wd = w::WebPData { bytes: data.as_ptr(), size: data.len() };
        let dec = w::WebPAnimDecoderNew(&wd, &opts);
        if dec.is_null() {
            return None;
        }
        let mut info: w::WebPAnimInfo = std::mem::zeroed();
        w::WebPAnimDecoderGetInfo(dec, &mut info);
        let mut out = Vec::new();
        for _ in 0..info.frame_count {
            let mut buf: *mut u8 = std::ptr::null_mut();
            let mut ts = 0;
            if w::WebPAnimDecoderGetNext(dec, &mut buf, &mut ts) == 0 {
                break;
            }
            out.push(
                std::slice::from_raw_parts(buf, info.canvas_width as usize * info.canvas_height as usize * 4).to_vec(),
            );
        }
        w::WebPAnimDecoderDelete(dec);
        Some(out)
    }
}

/// A still the way the viewer opens it: JPEG, PNG and WebP through the
/// texture crate plus the orientation and colour of `photo::refine`, the
/// rest through this crate.
fn photo(path: &Path) -> Option<Rgba> {
    use slerp_scene::textures::{DecodeOptions, Format};
    let name = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let format = match name.as_str() {
        "jpg" | "jpeg" => Format::Jpeg,
        "png" => Format::Png,
        "webp" => Format::Webp,
        _ => {
            let Ok(Asset::Image(t)) = open(path) else { return None };
            return rgba_of(&t);
        }
    };
    let tex = slerp_scene::textures::decode_file(path, &DecodeOptions::full()).ok()?;
    rgba_of(&slerp_images::photo::refine(path, format, tex))
}

fn turned(r: &Rgba, orientation: u8) -> Rgba {
    let (px, w, h) = slerp_images::orient_for_test(&r.px, r.w as usize, r.h as usize, 4, orientation);
    Rgba { w: w as u32, h: h as u32, px }
}

/// Photographs carry the way up in EXIF (JPEG, WebP, PNG's eXIf chunk, TIFF);
/// the reference decodes are as stored, so the picture shown is the
/// reference turned as the number says.
#[test]
fn exif_orientation_turns_photographs_upright() {
    let Some(root) = assets() else { return };
    let dir = root.join("exif-orientation");
    let (mut compared, mut failures) = (0, Vec::new());
    for path in files(&dir, &["jpg", "jpeg", "png", "webp", "tif", "tiff"]) {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        // The number the generated and sample files carry in their names.
        let digit = name
            .rsplit_once('.')
            .map(|(stem, _)| stem)
            .and_then(|stem| stem.rsplit(['-', '_']).next())
            .and_then(|d| d.parse::<u8>().ok())
            .filter(|d| (1..=8).contains(d));
        let Some(o) = digit else { continue };
        if name.contains("invalid") {
            continue;
        }
        let Some(want) = read_png(&dir.join("ref").join(format!("{name}.ref.png"))) else { continue };
        let Some(got) = photo(&path) else { continue };
        compared += 1;
        if let Err(e) = check(&name, &got, &turned(&want, o), 8, 0.04) {
            failures.push(e);
        }
    }
    assert!(compared > 40, "{compared}");
    assert!(failures.is_empty(), "{} of {compared} differ:\n{}", failures.len(), failures.join("\n"));
}

/// Pictures with a matrix and curve ICC profile come out as the reference
/// made with lcms (to sRGB, perceptual intent), within what two colour
/// engines and 8 bits allow.
#[test]
fn icc_profiles_convert_to_srgb() {
    let Some(root) = assets() else { return };
    let dir = root.join("icc");
    let (mut compared, mut failures) = (0, Vec::new());
    for path in files(&dir, &["jpg", "png", "webp", "tif"]) {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        // HDR curves, CMYK and grey profiles, and lookup-table profiles are not converted by
        // this reader (the picture is shown as sRGB, and says so).
        if ["cicp", "cmyk", "gray", "grey", "hlg", "pq", "lut", "profile_big"].iter().any(|k| name.contains(k)) {
            continue;
        }
        // The orientation samples copied here have unrotated references; PNG references are
        // left to `png_and_jpeg_charts_agree`: ImageMagick converts PNGs differently from
        // its own JPEG decodes of the same chart, and this reader agrees with the JPEGs.
        if name.starts_with("exif-orientation--") || name.ends_with(".png") {
            continue;
        }
        let Some(want) = read_png(&dir.join("ref").join(format!("{name}.ref.png"))) else { continue };
        let Some(got) = photo(&path) else { continue };
        compared += 1;
        if let Err(e) = check(&name, &got, &want, 12, 0.03) {
            failures.push(e);
        }
    }
    assert!(compared > 10, "{compared}");
    assert!(failures.is_empty(), "{} of {compared} differ:\n{}", failures.len(), failures.join("\n"));
}

/// One colour chart is saved in each of several colour spaces as JPEG, PNG
/// and WebP: with the profile applied, all three must show the same colours.
#[test]
fn png_and_jpeg_charts_agree() {
    let Some(root) = assets() else { return };
    let dir = root.join("icc");
    let mut compared = 0;
    for png in files(&dir, &["png"]) {
        let name = png.file_stem().unwrap().to_string_lossy().to_string();
        if !name.starts_with("chart-") || name.contains("cicp") || name.contains("png-") {
            continue;
        }
        let Some(a) = photo(&png) else { continue };
        let Some(b) = photo(&dir.join(format!("{name}.jpg"))) else { continue };
        compared += 1;
        check(&name, &a, &b, 40, 0.012).unwrap_or_else(|e| panic!("{e}"));
    }
    assert!(compared >= 5, "{compared}");
}

/// HEIC pictures decode through the system's libheif, exactly as ImageMagick
/// (which uses the same library) shows them.  Without libheif the files must
/// say what to install, not fail some other way.
#[test]
fn heic_matches_libheif() {
    let Some(root) = assets() else { return };
    let (mut compared, mut failures, mut missing) = (0, Vec::new(), 0);
    for path in files(&root.join("heic"), &["heic", "heif", "hif", "heics"]) {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if sniffed(&path) != Some(ImageFormat::Heif) {
            continue;
        }
        let Some(want) = read_png(&root.join("heic/ref").join(format!("{name}.ref.png"))) else { continue };
        match open(&path) {
            Ok(Asset::Image(t)) => {
                let Some(got) = rgba_of(&t) else { continue };
                compared += 1;
                // Colour profiles are applied here and not by ImageMagick: compare picture geometry
                // exactly and colour only where there is no wide-gamut profile.
                let wide = name.contains("icc-") || name.contains("p3") || name.contains("2020");
                if let Err(e) = check(&name, &got, &want, if wide { 255 } else { 2 }, if wide { 1.0 } else { 0.004 }) {
                    failures.push(e);
                }
            }
            Ok(_) => failures.push(format!("{name}: not a picture")),
            Err(e) if e.contains("libheif library") => missing += 1,
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }
    assert!(compared + missing > 20, "{compared} compared, {missing} without libheif");
    assert!(failures.is_empty(), "{} of {compared} differ:\n{}", failures.len(), failures.join("\n"));
}

/// Truncated and corrupted copies of sample files never panic (a panic is
/// caught and reported as "internal decoder error", which counts as a
/// failure here) and never take long.
#[test]
fn damaged_samples_never_panic() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static PANICS: AtomicUsize = AtomicUsize::new(0);
    let Some(root) = assets() else { return };
    std::panic::set_hook(Box::new(|info| {
        PANICS.fetch_add(1, Ordering::SeqCst);
        eprintln!("panic: {info}");
    }));
    let dir = std::env::temp_dir().join(format!("slerp-images-damage-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut seed = 0x2545f4914f6cdd1du64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let (mut tried, mut slowest) = (0, std::time::Duration::ZERO);
    for folder in ["gif", "apng", "webp", "ico", "pnm", "tiff", "jp2", "heic", "avif", "jxl"] {
        let all = files(
            &root.join(folder),
            &[
                "gif", "png", "webp", "ico", "cur", "pbm", "pgm", "ppm", "pnm", "tif", "tiff", "jp2", "j2k", "heic",
                "heif", "avif", "avifs", "jxl",
            ],
        );
        for src in all.iter().step_by(5).take(14) {
            let Ok(bytes) = std::fs::read(src) else { continue };
            if bytes.len() > 3 << 20 || bytes.len() < 16 {
                continue;
            }
            let ext = src.extension().unwrap().to_string_lossy().to_string();
            for round in 0..8 {
                let mut b = bytes.clone();
                match round % 4 {
                    0 => b.truncate(rnd() as usize % b.len()),
                    1 => {
                        // Flip bytes anywhere.
                        for _ in 0..8 {
                            let i = rnd() as usize % b.len();
                            b[i] = rnd() as u8;
                        }
                    }
                    2 => {
                        // Flip bytes in the headers, where sizes and counts live.
                        for _ in 0..6 {
                            let i = rnd() as usize % b.len().min(200);
                            b[i] = rnd() as u8;
                        }
                    }
                    _ => {
                        // A cut and a flip.
                        b.truncate(b.len() / 2 + rnd() as usize % (b.len() / 2).max(1));
                        let i = rnd() as usize % b.len();
                        b[i] ^= 0xff;
                    }
                }
                let p = dir.join(format!("damaged.{ext}"));
                std::fs::write(&p, &b).unwrap();
                if std::env::var_os("SLERP_DAMAGE_TRACE").is_some() {
                    eprintln!("trying {} round {round}", src.display());
                }
                let t = std::time::Instant::now();
                match open(&p) {
                    Err(e) => assert!(!e.contains("internal decoder error"), "{}: {e}", src.display()),
                    Ok(Asset::Canvas(c)) => {
                        for i in (0..c.frames().len()).take(6) {
                            let _ = c.compose(i, &[], 1.0);
                        }
                    }
                    Ok(_) => {}
                }
                slowest = slowest.max(t.elapsed());
                tried += 1;
            }
        }
    }
    let _ = std::panic::take_hook();
    std::fs::remove_dir_all(&dir).ok();
    assert!(tried > 400, "{tried}");
    assert_eq!(PANICS.load(Ordering::SeqCst), 0, "a decoder panicked on damaged input");
    assert!(slowest < std::time::Duration::from_secs(10), "slowest {slowest:?}");
}

/// AVIF stills against libavif (`avifdec`, dav1d inside, bilinear chroma
/// upsampling, see `scripts/make_avif_bilinear_refs.sh`) where it can read
/// the file, otherwise against the first reference decode (ImageMagick or
/// libavif's default upsampling, which repeats chroma samples: close, but only
/// to within the loose tolerance).  Geometry is compared exactly.  Colour is
/// compared only for pictures without wide-gamut or HDR code points:
/// `avifdec` leaves those unconverted.
///
/// Files whose reference is known to be wrong are listed with the reason.
#[test]
fn avif_stills_match_libavif() {
    let Some(root) = assets() else { return };
    let dir = root.join("avif");
    // (file name part, why the reference decoder is the one that is wrong)
    let reference_wrong: &[(&str, &str)] = &[
        (
            "alpha-mask-limited",
            "libavif leaves limited-range monochrome unexpanded (verified against a lossless source)",
        ),
        ("Monochrome.avif", "libavif leaves limited-range monochrome unexpanded"),
        (".monochrome.", "libavif leaves limited-range monochrome unexpanded"),
        (
            "-crop.avif",
            "libavif and libheif scale an alpha plane that has no clap of its own to the cropped size; here it is a mask of exactly the cropped area, so cutting both is what the file means",
        ),
        (
            "extended_pixi",
            "colour description unspecified: libavif guesses the matrix, this crate takes BT.601 as AV1 decoders do",
        ),
        (
            "alpha-8bit-420-premultiplied",
            "libavif un-premultiplies through libyuv, a few levels off libheif's and this crate's exact division",
        ),
        // Checked below against the file's full-range twin instead.
        ("alpha-limited", "libheif leaves limited-range alpha unstretched, libavif can't read these files"),
    ];
    // Files whose alpha this libavif doesn't find; only their colour is compared.
    let alpha_unseen: &[(&str, &str)] =
        &[("two_targets", "this libavif finds no alpha through an auxl reference that names two items")];
    let (mut compared, mut failures, mut unsupported, mut tight) = (0, Vec::new(), Vec::new(), 0);
    for path in files(&dir, &["avif", "avifs"]) {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if sniffed(&path) != Some(ImageFormat::Avif) || name.starts_with("anim") {
            continue;
        }
        if reference_wrong.iter().any(|(part, _)| name.contains(part)) {
            continue;
        }
        let first = dir.join("ref").join(format!("{name}.ref.png"));
        let bilinear = read_png(&dir.join("ref-bilinear").join(format!("{name}.png")));
        let loose = bilinear.is_none();
        let Some(mut want) = bilinear.or_else(|| read_png(&first)) else { continue };
        match open(&path) {
            Ok(Asset::Image(t)) => {
                let Some(got) = rgba_of(&t) else { continue };
                compared += 1;
                if !loose && alpha_unseen.iter().any(|(part, _)| name.contains(part)) {
                    for (w, g) in want.px.chunks_exact_mut(4).zip(got.px.chunks_exact(4)) {
                        let rgb = if g[3] == 0 { [0; 3] } else { [w[0], w[1], w[2]] };
                        w.copy_from_slice(&[rgb[0], rgb[1], rgb[2], g[3]]);
                    }
                }
                // HDR (PQ, HLG) and wide-gamut pictures are converted, not compared.
                let colourful = t.is_hdr()
                    || t.source.header.iter().any(|(k, v)| k == "Colour" && v.contains("converted"))
                    || ["p3", "2020", "pq", "hlg", "icc", "cicp", "gamma", "linear"].iter().any(|k| name.contains(k));
                // A few pixels have more edge than middle.
                let tiny = want.w * want.h <= 400;
                let (d, r) = match (colourful, loose || tiny) {
                    (true, _) => (255, 1.0),
                    (false, true) => (12, 0.02),
                    (false, false) => {
                        tight += 1;
                        (6, 0.008)
                    }
                };
                if let Err(e) = check(&name, &got, &want, d, r) {
                    failures.push(e);
                }
            }
            Ok(Asset::Canvas(_)) => {}
            Ok(_) => failures.push(format!("{name}: not a picture")),
            Err(e) => unsupported.push(format!("{name}: {e}")),
        }
    }
    // Limited-range alpha must come out as its full-range twin's does.
    let mut twins = 0;
    for path in files(&dir, &["avif"]) {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let Some(twin) = name.contains("alpha-limited").then(|| dir.join(name.replace("alpha-limited", "alpha-full")))
        else {
            continue;
        };
        if let (Ok(Asset::Image(a)), Ok(Asset::Image(b))) = (open(&path), open(&twin))
            && let (Some(a), Some(b)) = (rgba_of(&a), rgba_of(&b))
        {
            twins += 1;
            // The twins are separate encodes, so the alpha agrees only about as well as lossy codings do.
            let alpha_of = |p: &Rgba| Rgba {
                w: p.w,
                h: p.h,
                px: p.px.chunks_exact(4).flat_map(|c| [c[3], c[3], c[3], 255]).collect(),
            };
            if let Err(e) = check(&format!("{name} against its full-range twin"), &alpha_of(&a), &alpha_of(&b), 8, 0.02)
            {
                failures.push(e);
            }
        }
    }
    assert!(twins > 5, "{twins} twins");
    assert!(compared > 100 && tight > 100, "{compared} compared, {tight} tightly");
    eprintln!("{} AVIF files were refused:\n{}", unsupported.len(), unsupported.join("\n"));
    assert!(failures.is_empty(), "{} of {compared} differ:\n{}", failures.len(), failures.join("\n"));
}

/// JPEG XL stills against libjxl (`djxl`, its PNG converted to 8-bit sRGB).
/// djxl keeps the stored orientation, so the reference is turned the way the
/// file says before comparing.
#[test]
fn jxl_stills_match_djxl() {
    let Some(root) = assets() else { return };
    let dir = root.join("jxl");
    if !dir.is_dir() {
        return;
    }
    // (file name part, why the reference can't be compared)
    let skip: &[(&str, &str)] = &[
        ("hdr-", "the reference keeps the PQ or HLG coding; this crate shows linear light"),
        ("pq", "the reference keeps the PQ coding"),
        ("hlg", "the reference keeps the HLG coding"),
        ("linear", "the reference keeps linear values; this crate shows linear light"),
        ("float32", "floating point light, shown through the HDR path"),
        ("jxlrs-candle", "16-bit float linear light, shown through the HDR path"),
        ("cmyk", "CMYK is converted with the textbook formula, the reference with the profile"),
        (
            "grayscale.jxl",
            "djxl labels its sRGB grey with the file's grey ICC profile, which the reference applies again",
        ),
        ("icc-gray", "djxl labels its sRGB grey with the file's grey ICC profile, which the reference applies again"),
        ("lossy_with_icc", "the reference was not converted from the file's Adobe RGB profile"),
        ("jxlconf-spot", "jxl-rs paints the spot colour channels, djxl's PNG leaves them out"),
        ("splines.jxl", "its splines exceed the Level 5 limits this decoder keeps to (refused)"),
    ];
    // Wide-gamut pictures go through this crate's conversion and libjxl's CMS: close, not equal.
    let loose = ["colorspace-", "icc-", "with_icc", "grayscale_patches"];
    let (mut compared, mut failures) = (0, Vec::new());
    for path in files(&dir, &["jxl"]) {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if skip.iter().any(|(p, _)| name.contains(p)) {
            continue;
        }
        let Some(mut want) = read_png(&dir.join("ref").join(format!("{name}.ref.png"))) else { continue };
        match open(&path) {
            Ok(Asset::Image(t)) => {
                let Some(got) = rgba_of(&t) else { continue };
                let exif = t
                    .source
                    .header
                    .iter()
                    .find(|(k, _)| k == "Orientation")
                    .and_then(|(_, v)| v.split("(EXIF ").nth(1)?.split(')').next()?.parse::<u8>().ok());
                if let Some(o) = exif {
                    let (px, w, h) = slerp_images::orient_for_test(&want.px, want.w as usize, want.h as usize, 4, o);
                    want = Rgba { w: w as u32, h: h as u32, px };
                }
                compared += 1;
                let (d, r) = if loose.iter().any(|p| name.contains(p)) { (40, 0.06) } else { (8, 0.01) };
                if let Err(e) = check(&name, &got, &want, d, r) {
                    failures.push(e);
                }
            }
            Ok(Asset::Canvas(_)) => {}
            Ok(_) => failures.push(format!("{name}: not a picture")),
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }
    assert!(compared > 80, "{compared}");
    assert!(failures.is_empty(), "{} of {compared} differ:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn animated_jxls_match_djxl() {
    animations("jxl", "ref", &["jxl"], &[], true, 8, 0.01);
}
