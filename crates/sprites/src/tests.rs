//! Tests against the corpus in `local/assets/` (skipped when it isn't
//! there) and against hand-built files whose composites are worked out by
//! hand.  `SLERP_TEST_OUT=<dir>` saves every composite for a look.

use crate::{SpriteFormat, load, sniff, thumbnail};
use slerp_scene::{Asset, Canvas, Image8, Limits};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets")
}

fn asset(relative: &str) -> Option<PathBuf> {
    let p = assets().join(relative);
    if p.exists() {
        Some(p)
    } else {
        eprintln!("skipping: {} is missing", p.display());
        None
    }
}

fn open(path: &Path, limits: &Limits) -> Arc<dyn Canvas> {
    let bytes = std::fs::read(path).unwrap();
    let format =
        sniff(path, &bytes[..bytes.len().min(4096)]).unwrap_or_else(|| panic!("{} not recognised", path.display()));
    match load(format, path, &bytes, limits) {
        Ok(Asset::Canvas(c)) => c,
        Ok(_) => panic!("not a canvas"),
        Err(e) => panic!("{}: {e}", path.display()),
    }
}

fn shown(c: &dyn Canvas) -> Vec<bool> {
    c.layers().iter().map(|l| l.visible).collect()
}

fn decode_png(path: &Path) -> Image8 {
    let tex =
        slerp_scene::textures::decode(&std::fs::read(path).unwrap(), &slerp_scene::textures::DecodeOptions::full())
            .unwrap();
    crate::pixels::to_rgba8(&tex)
}

fn save(name: &str, img: &Image8) {
    let Some(dir) = std::env::var_os("SLERP_TEST_OUT") else { return };
    let path = Path::new(&dir).join(format!("{name}.png"));
    let file = std::fs::File::create(&path).unwrap();
    let mut enc = png::Encoder::new(file, img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&img.pixels).unwrap();
}

/// Counts pixels that differ by more than `tolerance` in any channel,
/// counting any two fully transparent pixels as equal.
fn differences(a: &Image8, b: &Image8, tolerance: u8) -> usize {
    assert_eq!((a.width, a.height), (b.width, b.height));
    a.pixels
        .chunks_exact(4)
        .zip(b.pixels.chunks_exact(4))
        .filter(|(p, q)| {
            if p[3] == 0 && q[3] == 0 {
                return false;
            }
            p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > tolerance)
        })
        .count()
}

/*┌─────────────────╮
| Aseprite        |
└─────────────────╯*/
//region Aseprite

/// Composites against PNGs Aseprite itself exported (asefile's test data,
/// MIT): sprite, frame, reference picture, and how many channel steps of
/// rounding are tolerated.
#[test]
fn aseprite_matches_its_own_exports() {
    let Some(dir) = asset("sprites-extra/asefile") else { return };
    let cases: &[(&str, usize, &str)] = &[
        ("basic-16x16", 0, "basic-16x16"),
        ("big", 0, "big"),
        ("background", 0, "background"),
        ("layers_and_tags", 0, "layers_and_tags_01"),
        ("layers_and_tags", 1, "layers_and_tags_02"),
        ("layers_and_tags", 2, "layers_and_tags_03"),
        ("layers_and_tags", 3, "layers_and_tags_04"),
        ("transparency", 0, "transparency_01"),
        ("transparency", 1, "transparency_02"),
        ("linked_cels", 0, "linked_cels_01"),
        ("linked_cels", 1, "linked_cels_02"),
        ("linked_cels", 2, "linked_cels_03"),
        ("indexed", 0, "indexed_01"),
        ("grayscale", 0, "grayscale"),
        ("tilemap", 0, "tilemap"),
        ("tilemap_indexed", 0, "tilemap_indexed"),
        ("tilemap_grayscale", 0, "tilemap_grayscale"),
        ("blend_normal", 0, "blend_normal"),
        ("blend_multiply", 0, "blend_multiply"),
        ("blend_screen", 0, "blend_screen"),
        ("blend_overlay", 0, "blend_overlay"),
        ("blend_darken", 0, "blend_darken"),
        ("blend_lighten", 0, "blend_lighten"),
        ("blend_colordodge", 0, "blend_colordodge"),
        ("blend_colorburn", 0, "blend_colorburn"),
        ("blend_hardlight", 0, "blend_hardlight"),
        ("blend_softlight", 0, "blend_softlight"),
        ("blend_difference", 0, "blend_difference"),
        ("blend_exclusion", 0, "blend_exclusion"),
        ("blend_hue", 0, "blend_hue"),
        ("blend_saturation", 0, "blend_saturation"),
        ("blend_saturation_bug", 0, "blend_saturation_bug"),
        ("blend_color", 0, "blend_color"),
        ("blend_luminosity", 0, "blend_luminosity"),
        ("blend_addition", 0, "blend_addition"),
        ("blend_subtract", 0, "blend_subtract"),
        ("blend_divide", 0, "blend_divide"),
    ];
    let mut failures = Vec::new();
    for &(file, frame, reference) in cases {
        let sprite = open(&dir.join(format!("{file}.aseprite")), &Limits::viewer());
        let img = sprite.compose(frame, &shown(&*sprite), 1.0);
        save(&format!("ase-{reference}"), &img);
        let mut want = decode_png(&dir.join(format!("{reference}.png")));
        if reference == "background" {
            // Exported without alpha.
            for p in want.pixels.chunks_exact_mut(4) {
                p[3] = 255;
            }
        }
        let bad = differences(&img, &want, 0);
        if bad > 0 {
            failures.push(format!("{reference}: {bad} of {} pixels differ", img.pixels.len() / 4));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn aseprite_tags_frames_and_layers() {
    let Some(p) = asset("aseprite/layers_and_tags.aseprite") else { return };
    let s = open(&p, &Limits::viewer());
    assert_eq!(s.size(), (16, 16));
    assert_eq!(s.frames().len(), 4);
    assert_eq!(s.layers().len(), 6);
    assert_eq!(s.clips().len(), 3);
    assert!(s.frames().iter().all(|&d| d > 0.0));
    // Hiding every layer leaves nothing.
    let none = s.compose(0, &vec![false; s.layers().len()], 1.0);
    assert!(none.pixels.chunks_exact(4).all(|p| p[3] == 0));
    if let Some(p) = asset("aseprite/upstream/tags3x123reps.aseprite") {
        let s = open(&p, &Limits::viewer());
        let clips = s.clips();
        assert_eq!(clips.len(), 3, "{:?}", clips.iter().map(|c| &c.name).collect::<Vec<_>>());
        assert!(clips.iter().any(|c| c.repeat > 0), "{clips:?}");
    }
}

#[test]
fn aseprite_corpus_thumbnails() {
    let Some(dir) = asset("aseprite") else { return };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
    files.extend(std::fs::read_dir(dir.join("upstream")).into_iter().flatten().flatten().map(|e| e.path()));
    files.retain(|p| p.is_file());
    assert!(files.len() >= 8);
    for p in files {
        let s = open(&p, &Limits::thumbnail());
        let t = thumbnail(&*s, 256);
        assert!(t.width <= 256 && t.height <= 256 && t.width > 0, "{}", p.display());
        assert!(t.pixels.chunks_exact(4).any(|q| q[3] > 0), "{} came out empty", p.display());
        save(&format!("thumb-{}", p.file_stem().unwrap().to_string_lossy()), &t);
    }
}

/// Builds Aseprite files chunk by chunk, for fixtures with known answers.
struct AseWriter {
    width: u16,
    height: u16,
    depth: u16,
    frames: Vec<(u16, Vec<Vec<u8>>)>,
}

impl AseWriter {
    fn chunk(kind: u16, body: &[u8]) -> Vec<u8> {
        let mut c = ((body.len() + 6) as u32).to_le_bytes().to_vec();
        c.extend(kind.to_le_bytes());
        c.extend(body);
        c
    }

    fn string(s: &str) -> Vec<u8> {
        let mut v = (s.len() as u16).to_le_bytes().to_vec();
        v.extend(s.as_bytes());
        v
    }

    fn layer(blend: u16, opacity: u8, name: &str, visible: bool) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend((if visible { 1u16 } else { 0 }).to_le_bytes());
        b.extend(0u16.to_le_bytes()); // image layer
        b.extend(0u16.to_le_bytes()); // child level
        b.extend([0u8; 4]);
        b.extend(blend.to_le_bytes());
        b.push(opacity);
        b.extend([0u8; 3]);
        b.extend(Self::string(name));
        Self::chunk(0x2004, &b)
    }

    /// A cel filled with one colour, compressed or raw.
    fn cel(layer: u16, x: i16, y: i16, w: u16, h: u16, rgba: [u8; 4], compressed: bool) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(layer.to_le_bytes());
        b.extend(x.to_le_bytes());
        b.extend(y.to_le_bytes());
        b.push(255);
        b.extend((if compressed { 2u16 } else { 0 }).to_le_bytes());
        b.extend([0u8; 7]);
        b.extend(w.to_le_bytes());
        b.extend(h.to_le_bytes());
        let pixels: Vec<u8> = (0..w as usize * h as usize).flat_map(|_| rgba).collect();
        if compressed {
            b.extend(miniz_oxide::deflate::compress_to_vec_zlib(&pixels, 6));
        } else {
            b.extend(pixels);
        }
        Self::chunk(0x2005, &b)
    }

    fn link(layer: u16, frame: u16) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(layer.to_le_bytes());
        b.extend([0u8; 4]);
        b.push(255);
        b.extend(1u16.to_le_bytes());
        b.extend([0u8; 7]);
        b.extend(frame.to_le_bytes());
        Self::chunk(0x2005, &b)
    }

    fn tags(tags: &[(u16, u16, u8, u16, &str)]) -> Vec<u8> {
        let mut b = (tags.len() as u16).to_le_bytes().to_vec();
        b.extend([0u8; 8]);
        for &(from, to, dir, repeat, name) in tags {
            b.extend(from.to_le_bytes());
            b.extend(to.to_le_bytes());
            b.push(dir);
            b.extend(repeat.to_le_bytes());
            b.extend([0u8; 10]);
            b.extend(Self::string(name));
        }
        Self::chunk(0x2018, &b)
    }

    fn bytes(&self) -> Vec<u8> {
        let mut body = Vec::new();
        for (ms, chunks) in &self.frames {
            let data: Vec<u8> = chunks.concat();
            body.extend(((data.len() + 16) as u32).to_le_bytes());
            body.extend(0xF1FAu16.to_le_bytes());
            body.extend((chunks.len() as u16).to_le_bytes());
            body.extend(ms.to_le_bytes());
            body.extend([0u8; 2]);
            body.extend((chunks.len() as u32).to_le_bytes());
            body.extend(data);
        }
        let mut h = Vec::new();
        h.extend(((body.len() + 128) as u32).to_le_bytes());
        h.extend(0xA5E0u16.to_le_bytes());
        h.extend((self.frames.len() as u16).to_le_bytes());
        h.extend(self.width.to_le_bytes());
        h.extend(self.height.to_le_bytes());
        h.extend(self.depth.to_le_bytes());
        h.extend(1u32.to_le_bytes()); // layer opacity is valid
        h.extend(100u16.to_le_bytes());
        h.resize(128, 0);
        h.extend(body);
        h
    }

    fn load(&self) -> Arc<dyn Canvas> {
        let bytes = self.bytes();
        let path = Path::new("fixture.aseprite");
        assert_eq!(sniff(path, &bytes), Some(SpriteFormat::Aseprite));
        match load(SpriteFormat::Aseprite, path, &bytes, &Limits::viewer()).unwrap() {
            Asset::Canvas(c) => c,
            _ => unreachable!(),
        }
    }
}

/// Two opaque layers, the top one with each blend mode at 100 % and 50 %
/// opacity, against values worked out from the PDF blend formulas.
#[test]
fn aseprite_blend_modes_by_hand() {
    let bottom = [200u8, 100, 50, 255];
    let top = [100u8, 200, 250, 255];
    // (mode, result at full opacity)
    let cases: &[(u16, [u8; 3])] = &[
        (0, [100, 200, 250]),
        (1, [78, 78, 49]),     // multiply: b*s
        (2, [222, 222, 251]),  // screen: b + s - b*s
        (4, [100, 100, 50]),   // darken
        (5, [200, 200, 250]),  // lighten
        (10, [100, 100, 200]), // difference
        (11, [144, 144, 202]), // exclusion: b + s - 2bs
        (16, [255, 255, 255]), // addition, clipped
        (17, [100, 0, 0]),     // subtract, clipped
        (18, [255, 128, 51]),  // divide: b / s, capped
        (3, [188, 157, 98]),   // overlay: hard light with the roles swapped
        (8, [157, 188, 247]),  // hard light: multiply or screen by 2s
    ];
    for &(mode, want) in cases {
        for (opacity, compressed) in [(255u8, true), (128, false)] {
            let w = AseWriter {
                width: 4,
                height: 3,
                depth: 32,
                frames: vec![(
                    100,
                    vec![
                        AseWriter::layer(0, 255, "bottom", true),
                        AseWriter::layer(mode, opacity, "top", true),
                        AseWriter::cel(0, 0, 0, 4, 3, bottom, compressed),
                        AseWriter::cel(1, 1, 1, 2, 1, top, !compressed),
                    ],
                )],
            };
            let s = w.load();
            let img = s.compose(0, &shown(&*s), 1.0);
            let at = |x: usize, y: usize| &img.pixels[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
            // Outside the top cel: the bottom layer alone.
            assert_eq!(at(0, 0), &bottom, "mode {mode}");
            assert_eq!(at(3, 2), &bottom, "mode {mode}");
            let got = at(1, 1);
            if opacity == 255 {
                // Aseprite rounds products before summing them, the
                // formulas after: a step apart at most.
                for c in 0..3 {
                    assert!(got[c].abs_diff(want[c]) <= 1, "mode {mode}: {got:?} vs {want:?}");
                }
            } else {
                // Half opacity lands halfway, within a step of rounding.
                for c in 0..3 {
                    let mid = (bottom[c] as i32 + want[c] as i32) / 2;
                    assert!((got[c] as i32 - mid).abs() <= 1, "mode {mode} at 50 %: {got:?} vs {mid}");
                }
            }
            assert_eq!(got[3], 255);
            assert_eq!(at(2, 1), got, "mode {mode}");
        }
    }
}

#[test]
fn aseprite_links_tags_and_hidden_layers() {
    let red = [255, 0, 0, 255];
    let blue = [0, 0, 255, 255];
    let w = AseWriter {
        width: 2,
        height: 2,
        depth: 32,
        frames: vec![
            (
                50,
                vec![
                    AseWriter::layer(0, 255, "a", true),
                    AseWriter::layer(0, 255, "hidden", false),
                    AseWriter::cel(0, 0, 0, 2, 2, red, true),
                    AseWriter::cel(1, 0, 0, 2, 2, blue, true),
                    AseWriter::tags(&[(0, 2, 2, 0, "bounce"), (1, 9, 1, 3, "past the end"), (2, 1, 0, 0, "backwards")]),
                ],
            ),
            (70, vec![AseWriter::link(0, 0)]),
            (0, vec![AseWriter::cel(0, 1, 1, 1, 1, blue, false)]),
        ],
    };
    let s = w.load();
    assert_eq!(s.frames(), &[0.05, 0.07, 0.1]);
    assert_eq!(s.layers().iter().map(|l| l.visible).collect::<Vec<_>>(), [true, false]);
    // Frame 1 links to frame 0's red cel; frame 2 has one blue pixel.
    let f1 = s.compose(1, &shown(&*s), 1.0);
    assert!(f1.pixels.chunks_exact(4).all(|p| p == red));
    let f2 = s.compose(2, &shown(&*s), 1.0);
    assert_eq!(&f2.pixels[12..16], &blue);
    assert_eq!(f2.pixels[3], 0);
    // Showing the hidden layer covers everything in blue.
    let both = s.compose(0, &[true, true], 1.0);
    assert!(both.pixels.chunks_exact(4).all(|p| p == blue));
    // Tags: kept, the one running past the end cut short, the one ending
    // before it starts dropped.
    let clips = s.clips();
    assert_eq!(clips.len(), 2);
    assert_eq!((clips[0].from, clips[0].to, clips[0].direction), (0, 2, slerp_scene::PlayDirection::PingPong));
    assert_eq!((clips[1].to, clips[1].repeat), (2, 3));
}

/// Truncated and corrupted copies of every corpus sprite and fixture
/// load or fail, but never panic or hang.
#[test]
fn aseprite_survives_damage() {
    let mut samples: Vec<Vec<u8>> = Vec::new();
    if let Some(dir) = asset("aseprite") {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            if e.path().is_file() {
                samples.push(std::fs::read(e.path()).unwrap());
            }
        }
    }
    if let Some(p) = asset("sprites-extra/asefile/tilemap.aseprite") {
        samples.push(std::fs::read(p).unwrap());
    }
    let started = std::time::Instant::now();
    let mut seed = 0x1234_5678u32;
    let mut rand = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for bytes in &samples {
        let try_one = |b: &[u8]| {
            if let Ok(Asset::Canvas(c)) = load(SpriteFormat::Aseprite, Path::new("x.aseprite"), b, &Limits::thumbnail())
            {
                let n = c.frames().len();
                for f in [n / 2, n.saturating_sub(1)] {
                    let _ = c.compose(f, &shown(&*c), 64.0 / c.size().0.max(c.size().1).max(1) as f32);
                }
                let _ = thumbnail(&*c, 64);
            }
        };
        for cut in (0..bytes.len()).step_by((bytes.len() / 40).max(1)) {
            try_one(&bytes[..cut]);
        }
        for _ in 0..60 {
            let mut b = bytes.clone();
            for _ in 0..1 + rand() % 8 {
                let i = rand() as usize % b.len();
                b[i] = rand() as u8;
            }
            try_one(&b);
        }
    }
    // A header claiming a 65535 x 65535 canvas composes at a reduced scale
    // rather than allocating 17 GB.
    // Its one cel claims 65535 x 65535 pixels but holds a few bytes, and is
    // left out rather than padded.
    let mut huge = AseWriter {
        width: 65535,
        height: 65535,
        depth: 32,
        frames: vec![(100, vec![AseWriter::layer(0, 255, "x", true)])],
    };
    huge.frames[0].1.push(AseWriter::cel(0, 0, 0, 4, 4, [1, 2, 3, 255], true));
    let cel = huge.frames[0].1.last_mut().unwrap();
    cel[6 + 16..6 + 20].copy_from_slice(&[255, 255, 255, 255]);
    let s = huge.load();
    let t = thumbnail(&*s, 128);
    assert_eq!((t.width, t.height), (128, 128));
    assert!(started.elapsed().as_secs() < 120, "took {:?}", started.elapsed());
}

//endregion

/*┌─────────────────╮
| Tiled           |
└─────────────────╯*/
//region Tiled

fn tiled_maps() -> Vec<PathBuf> {
    let Some(dir) = asset("tiled/tiled-examples") else { return Vec::new() };
    let mut out = Vec::new();
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(d).unwrap().flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|e| e == "tmx" || e == "tmj") {
                out.push(p);
            }
        }
    }
    walk(&dir, &mut out);
    if let Some(extra) = asset("sprites-extra/tiled") {
        walk(&extra, &mut out);
    }
    out
}

#[test]
fn tiled_examples_compose_with_their_tilesets() {
    let maps = tiled_maps();
    if maps.is_empty() {
        return;
    }
    for p in &maps {
        let started = std::time::Instant::now();
        let map = open(p, &Limits::thumbnail());
        let loaded = started.elapsed();
        let t = thumbnail(&*map, 256);
        let name = p.file_stem().unwrap().to_string_lossy().into_owned();
        eprintln!(
            "{name}: {:?}, load {:.1} ms, thumbnail {:.1} ms, warnings {:?}",
            map.size(),
            loaded.as_secs_f64() * 1e3,
            (started.elapsed() - loaded).as_secs_f64() * 1e3,
            map.info().warnings
        );
        assert!(map.info().warnings.is_empty(), "{name}: {:?}", map.info().warnings);
        assert!(
            t.pixels.chunks_exact(4).filter(|q| q[3] > 0).count() > (t.width * t.height / 50) as usize,
            "{name} is empty"
        );
        save(&format!("tiled-{name}"), &t);
    }
}

#[test]
fn tiled_orientations_have_tileds_sizes() {
    let Some(dir) = asset("tiled/tiled-examples") else { return };
    // Map sizes in pixels as Tiled's renderers give them (plus room for
    // tiles taller than the grid, which these maps don't have).
    for (file, want) in [
        // 25 + 25 half tiles each way, plus 32 pixels for 64-pixel-tall
        // tiles on a 32-pixel grid and 16 for the tileset's offset.
        ("isometric_grass_and_water.tmx", (1600, 848)),
        ("desert.tmx", (1280, 1280)),
        // 20 rows of 14-pixel hexagons with 6-pixel sides: 20 * 14 + 7
        // wide, 20 * 9 + 3 tall, plus 18-pixel tiles' overhang and the
        // tileset's 1-pixel offset.
        ("hexagonal-mini.tmx", (291, 190)),
        // Staggered along x: 20 * 45 + 15 wide, 20 * 60 + 30 tall.
        ("test_hexagonal_tile_60x60x30.tmx", (915, 1230)),
        ("isometric_staggered_grass_and_water.tmx", (0, 0)),
    ] {
        let map = open(&dir.join(file), &Limits::viewer());
        eprintln!("{file}: {:?} {:?}", map.size(), map.info().details);
        if want != (0, 0) {
            assert_eq!(map.size(), want, "{file}");
        }
    }
}

/// The same layer written in every encoding Tiled offers composes to the
/// same picture.
#[test]
fn tiled_encodings_agree() {
    let Some(dir) = asset("tiled/tiled-examples") else { return };
    let original = std::fs::read_to_string(dir.join("desert.tmx")).unwrap();
    let base = open(&dir.join("desert.tmx"), &Limits::viewer());
    let want = base.compose(0, &shown(&*base), 0.5);
    // The ids, from the zlib layer.
    let start = original.find("<data").unwrap();
    let text_start = original[start..].find('>').unwrap() + start + 1;
    let text_end = original[text_start..].find("</data>").unwrap() + text_start;
    let b64 = original[text_start..text_end].trim();
    let packed = crate::tiled::data_for_tests::base64(b64);
    let raw = crate::tiled::data_for_tests::inflate(&packed);
    let ids: Vec<u32> = raw.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    let enc = |b: &[u8]| {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(b)
    };
    let mut gz = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
    gz.extend(miniz_oxide::deflate::compress_to_vec(&raw, 6));
    gz.extend([0u8; 8]);
    let csv = ids.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",");
    let xml_tiles: String = ids.iter().map(|v| format!("<tile gid=\"{v}\"/>")).collect();
    let variants = [
        ("csv", format!("<data encoding=\"csv\">\n{csv}\n</data>")),
        ("base64", format!("<data encoding=\"base64\">{}</data>", enc(&raw))),
        ("gzip", format!("<data encoding=\"base64\" compression=\"gzip\">{}</data>", enc(&gz))),
        (
            "zstd",
            format!(
                "<data encoding=\"base64\" compression=\"zstd\">{}</data>",
                enc(&zstd::bulk::compress(&raw, 3).unwrap())
            ),
        ),
        ("xml", format!("<data>{xml_tiles}</data>")),
    ];
    let tmp = std::env::temp_dir().join(format!("slerp-tiled-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    for (name, data) in variants {
        let text = format!("{}{}{}", &original[..start], data, &original[text_end + "</data>".len()..])
            .replace("source=\"desert.tsx\"", &format!("source=\"{}\"", dir.join("desert.tsx").display()));
        let path = tmp.join(format!("desert-{name}.tmx"));
        std::fs::write(&path, text).unwrap();
        let map = open(&path, &Limits::viewer());
        let got = map.compose(0, &shown(&*map), 0.5);
        assert_eq!(differences(&got, &want, 0), 0, "{name}");
        assert!(
            map.info().details.iter().any(|(k, v)| k == "Layer data" && v.contains(name.trim_end_matches("64"))),
            "{name}"
        );
    }
    // And as JSON, with the ids as an array and as zlib base64.
    for (name, data) in [
        ("array", format!("\"data\": [{csv}]")),
        ("zlib", format!("\"encoding\": \"base64\", \"compression\": \"zlib\", \"data\": \"{b64}\"")),
    ] {
        let json = format!(
            r#"{{"type": "map", "orientation": "orthogonal", "width": 40, "height": 40, "tilewidth": 32, "tileheight": 32,
               "tilesets": [{{"firstgid": 1, "source": "{}"}}],
               "layers": [{{"type": "tilelayer", "name": "Ground", "width": 40, "height": 40, "opacity": 1, "visible": true, {data}}}]}}"#,
            dir.join("desert.tsx").display()
        );
        let path = tmp.join(format!("desert-{name}.tmj"));
        std::fs::write(&path, json).unwrap();
        let map = open(&path, &Limits::viewer());
        assert_eq!(map.size(), base.size());
        let got = map.compose(0, &shown(&*map), 0.5);
        assert_eq!(differences(&got, &want, 0), 0, "JSON {name}");
    }
    std::fs::remove_dir_all(&tmp).ok();
}

/// A 2 x 2 tileset picture whose four pixels tell flips apart, drawn as
/// one-pixel tiles: flipped tiles show the picture's pixels moved.
#[test]
fn tiled_flips_and_missing_pictures() {
    let tmp = std::env::temp_dir().join(format!("slerp-tiled-flip-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let (r, g, b, w) = ([255u8, 0, 0, 255], [0u8, 255, 0, 255], [0u8, 0, 255, 255], [255u8, 255, 255, 255]);
    let mut png_bytes = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png_bytes, 2, 2);
        enc.set_color(png::ColorType::Rgba);
        enc.write_header().unwrap().write_image_data(&[r, g, b, w].concat()).unwrap();
    }
    std::fs::write(tmp.join("quad.png"), &png_bytes).unwrap();
    // One 2 x 2 tile, placed four times: plain, flipped horizontally,
    // vertically, and diagonally.
    let gids = [1u32, 1 | 0x8000_0000, 1 | 0x4000_0000, 1 | 0x2000_0000];
    let tmx = format!(
        r#"<?xml version="1.0"?>
<map version="1.10" orientation="orthogonal" width="4" height="1" tilewidth="2" tileheight="2">
 <tileset firstgid="1" name="q" tilewidth="2" tileheight="2" tilecount="1" columns="1"><image source="quad.png" width="2" height="2"/></tileset>
 <layer name="t" width="4" height="1"><data encoding="csv">{}</data></layer>
 <objectgroup name="o"><object id="1" x="0" y="0" width="2" height="2"/><object id="2" x="1" y="1"><point/></object></objectgroup>
</map>"#,
        gids.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",")
    );
    let path = tmp.join("flip.tmx");
    std::fs::write(&path, &tmx).unwrap();
    let map = open(&path, &Limits::viewer());
    assert_eq!(map.size(), (8, 2));
    let img = map.compose(0, &[true, false], 1.0);
    let px = |x: usize, y: usize| -> [u8; 4] { img.pixels[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4].try_into().unwrap() };
    // Plain: r g / b w.
    assert_eq!([px(0, 0), px(1, 0), px(0, 1), px(1, 1)], [r, g, b, w]);
    // Horizontal: g r / w b.
    assert_eq!([px(2, 0), px(3, 0), px(2, 1), px(3, 1)], [g, r, w, b]);
    // Vertical: b w / r g.
    assert_eq!([px(4, 0), px(5, 0), px(4, 1), px(5, 1)], [b, w, r, g]);
    // Diagonal (transposed): r b / g w.
    assert_eq!([px(6, 0), px(7, 0), px(6, 1), px(7, 1)], [r, b, g, w]);
    // The object layer draws over the first tile.
    let with_objects = map.compose(0, &[true, true], 1.0);
    assert_ne!(with_objects.pixels[..8], img.pixels[..8]);

    // An image layer, offset by one pixel and repeated across: columns 1
    // and 2 show the picture's two columns, and so on along the row.
    let img_map = r#"<map orientation="orthogonal" width="3" height="1" tilewidth="2" tileheight="2">
 <imagelayer name="i" offsetx="1" offsety="0" repeatx="1"><image source="quad.png" width="2" height="2"/></imagelayer>
</map>"#;
    let img_path = tmp.join("image.tmx");
    std::fs::write(&img_path, img_map).unwrap();
    let layered = open(&img_path, &Limits::viewer());
    let shot = layered.compose(0, &[true], 1.0);
    let at = |x: usize, y: usize| -> [u8; 4] { shot.pixels[(y * 6 + x) * 4..(y * 6 + x) * 4 + 4].try_into().unwrap() };
    assert_eq!([at(0, 0), at(1, 0), at(2, 0), at(3, 0)], [g, r, g, r]);
    assert_eq!([at(1, 1), at(2, 1)], [b, w]);

    // Without neighbouring files (GNOME's sandbox), tiles become coloured
    // cells, the same colour for the same tile, and a warning says why.
    let limits = Limits { neighbours: false, ..Limits::thumbnail() };
    let bare = open(&path, &limits);
    assert!(!bare.info().warnings.is_empty());
    let cells = bare.compose(0, &[true, false], 1.0);
    let c = |x: usize| &cells.pixels[x * 4..x * 4 + 4];
    assert_eq!(c(0), c(7));
    assert_eq!(c(0)[3], 255);
    // A picture that isn't there: the same, with a warning naming it.
    std::fs::write(&path, tmx.replace("quad.png", "gone.png")).unwrap();
    let gone = open(&path, &Limits::viewer());
    assert!(gone.info().warnings.iter().any(|w| w.contains("gone.png")), "{:?}", gone.info().warnings);
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn tiled_survives_damage() {
    let maps = tiled_maps();
    let started = std::time::Instant::now();
    let mut seed = 0x9e37_79b9u32;
    let mut rand = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for p in maps.iter().take(12) {
        let bytes = std::fs::read(p).unwrap();
        let format = sniff(p, &bytes).unwrap();
        let try_one = |b: &[u8]| {
            if let Ok(Asset::Canvas(c)) = load(format, p, b, &Limits::thumbnail()) {
                let _ = thumbnail(&*c, 64);
            }
        };
        for cut in (0..bytes.len()).step_by((bytes.len() / 25).max(1)) {
            try_one(&bytes[..cut]);
        }
        for _ in 0..25 {
            let mut b = bytes.clone();
            for _ in 0..1 + rand() % 6 {
                let i = rand() as usize % b.len();
                b[i] = b"0123456789-<>\"=, x"[rand() as usize % 18];
            }
            try_one(&b);
        }
    }
    // Maps that claim far more tiles than they hold are refused quickly.
    let bomb = r#"<map orientation="orthogonal" width="1000000" height="1000000" tilewidth="32" tileheight="32"><layer width="1000000" height="1000000"><data encoding="csv">1</data></layer></map>"#;
    let r = load(SpriteFormat::Tmx, Path::new("bomb.tmx"), bomb.as_bytes(), &Limits::thumbnail());
    if let Ok(Asset::Canvas(c)) = r {
        assert!(!c.info().warnings.is_empty());
        let _ = thumbnail(&*c, 64);
    }
    let json = r#"{"orientation": "isometric", "width": 3, "height": 2, "tilewidth": 64, "tileheight": 32, "layers": [{"type": "group", "layers": [{"type": "tilelayer", "width": 3, "height": 2, "data": [1, 2, 3, 4, 5, 6]}]}], "tilesets": [{"firstgid": 1, "source": "nowhere.tsj"}]}"#;
    let c = match load(SpriteFormat::Tmj, Path::new("/nonexistent/m.tmj"), json.as_bytes(), &Limits::viewer()).unwrap()
    {
        Asset::Canvas(c) => c,
        _ => unreachable!(),
    };
    assert_eq!(c.size(), (5 * 32, 5 * 16));
    assert_eq!(c.layers().len(), 2);
    assert!(c.layers()[1].depth == 1);
    assert!(thumbnail(&*c, 64).pixels.chunks_exact(4).any(|p| p[3] > 0));
    assert!(started.elapsed().as_secs() < 120, "took {:?}", started.elapsed());
}

//endregion

/*┌─────────────────╮
| Piskel, Pyxel   |
└─────────────────╯*/
//region Piskel, Pyxel

/// A PNG of RGBA pixels, for hand-built documents.
fn png_of(img: &Image8) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&img.pixels).unwrap();
    out
}

/// A picture of one colour.
fn filled(w: u32, h: u32, px: [u8; 4]) -> Image8 {
    Image8 { width: w, height: h, pixels: px.repeat((w * h) as usize) }
}

fn px(img: &Image8, x: u32, y: u32) -> [u8; 4] {
    crate::pixels::get(img, x, y)
}

fn load_bytes(name: &str, bytes: &[u8], limits: &Limits) -> Result<Arc<dyn Canvas>, slerp_scene::LoadError> {
    let path = Path::new(name);
    let format = sniff(path, &bytes[..bytes.len().min(4096)]).expect("recognised");
    match load(format, path, bytes, limits)? {
        Asset::Canvas(c) => Ok(c),
        _ => panic!("not a canvas"),
    }
}

/// The real Piskel files: frames, layers and the time each takes.
#[test]
fn piskel_corpus() {
    let Some(dir) = asset("editors-extra/piskel") else { return };
    let expect = [
        ("notchify-cm_waiting.piskel", 1, 8),
        ("piskelart-chr_capeWoman.piskel", 4, 4),
        ("js13k-hero.piskel", 1, 5),
        ("godot-Icon.piskel", 2, 1),
        ("downwellgba-title-bg.piskel", 8, 1),
    ];
    let mut n = 0;
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "piskel") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let start = std::time::Instant::now();
        let c = open(&path, &Limits::viewer());
        let took = start.elapsed();
        let thumb = thumbnail(&*open(&path, &Limits::thumbnail()), 256);
        eprintln!("{name}: {:?}, {} layers, {} frames, {took:?}", c.size(), c.layers().len(), c.frames().len());
        save(&format!("piskel-{name}"), &thumb);
        assert!(thumb.pixels.chunks_exact(4).any(|p| p[3] > 0), "{name}: empty thumbnail");
        if let Some((_, layers, frames)) = expect.iter().find(|e| e.0 == name) {
            assert_eq!((c.layers().len(), c.frames().len()), (*layers, *frames), "{name}");
        }
        n += 1;
    }
    assert!(n >= 7, "{n}");
}

/// Sheets in both layouts, hidden frames, layer opacity and the old grid
/// model, built by hand.
#[test]
fn piskel_by_hand() {
    use base64::Engine;
    let b64 = |img: &Image8| {
        format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png_of(img)))
    };
    // Two 2x2 frames side by side: red, then green; a blue layer at 50 %
    // over it, chunked as one column with two rows.
    let mut strip = filled(4, 2, [255, 0, 0, 255]);
    for y in 0..2 {
        for x in 2..4 {
            let i = (y * 4 + x) * 4;
            strip.pixels[i..i + 4].copy_from_slice(&[0, 255, 0, 255]);
        }
    }
    let mut column = filled(2, 4, [0, 0, 255, 255]);
    column.pixels[16..].iter_mut().for_each(|v| *v = 0);
    let bottom = serde_json::json!({"name": "Bottom", "frameCount": 2, "base64PNG": b64(&strip)}).to_string();
    let top = serde_json::json!({"name": "Top", "opacity": 0.5, "frameCount": 2,
        "chunks": [{"layout": [[0, 1]], "base64PNG": b64(&column)}]})
    .to_string();
    let doc = serde_json::json!({"modelVersion": 2, "piskel": {"name": "t", "fps": 4, "width": 2, "height": 2,
        "layers": [bottom, top]}});
    let c = load_bytes("a.piskel", doc.to_string().as_bytes(), &Limits::viewer()).unwrap();
    assert_eq!(c.frames(), &[0.25, 0.25]);
    let all = shown(&*c);
    let f0 = c.compose(0, &all, 1.0);
    let p = px(&f0, 0, 0);
    assert!(p[0] > 120 && p[0] < 135 && p[2] > 120 && p[2] < 135 && p[3] == 255, "{p:?}");
    // Frame 1 of the top layer is empty: green shows through.
    assert_eq!(px(&c.compose(1, &all, 1.0), 1, 1), [0, 255, 0, 255]);
    assert_eq!(px(&c.compose(0, &[true, false], 1.0), 0, 0), [255, 0, 0, 255]);
    // A hidden first frame: the thumbnail is the second.
    let mut hidden = doc.clone();
    hidden["piskel"]["hiddenFrames"] = serde_json::json!([0]);
    let c = load_bytes("a.piskel", hidden.to_string().as_bytes(), &Limits::thumbnail()).unwrap();
    assert_eq!(c.frames().len(), 1);
    assert_eq!(px(&c.compose(0, &[true, false], 1.0), 0, 0), [0, 255, 0, 255]);
    // Model version 1: a grid of colour strings, by column.
    let grid = serde_json::json!([["#ff0000", "TRANSPARENT"], ["rgba(0, 0, 255, 0.5)", "#0f0"]]).to_string();
    let layer = serde_json::json!({"name": "L", "frames": [grid]}).to_string();
    let v1 = serde_json::json!({"modelVersion": 1, "piskel": {"width": 2, "height": 2, "layers": [layer]}});
    let c = load_bytes("b.piskel", v1.to_string().as_bytes(), &Limits::viewer()).unwrap();
    let img = c.compose(0, &shown(&*c), 1.0);
    assert_eq!(
        (px(&img, 0, 0), px(&img, 0, 1), px(&img, 1, 0), px(&img, 1, 1)),
        ([255, 0, 0, 255], [0, 0, 0, 0], [0, 0, 255, 128], [0, 255, 0, 255])
    );
    // Version 0: a bare list of grids, known by its name.
    let v0 = format!("[{grid}]");
    assert!(load_bytes("c.piskel", v0.as_bytes(), &Limits::viewer()).is_ok());
    assert!(sniff(Path::new("c.json"), v0.as_bytes()).is_none());
}

/// The real Pyxel Edit documents.  Sunnyland's two layers composed must be
/// its back layer with the front one over it.
#[test]
fn pyxel_corpus() {
    let Some(dir) = asset("editors-extra/pyxel") else { return };
    let mut n = 0;
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "pyxel") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let start = std::time::Instant::now();
        let c = open(&path, &Limits::thumbnail());
        let thumb = thumbnail(&*c, 256);
        eprintln!("{name}: {:?}, {} layers in {:?}", c.size(), c.layers().len(), start.elapsed());
        save(&format!("pyxel-{name}"), &thumb);
        n += 1;
    }
    assert_eq!(n, 5);
    let c = open(&dir.join("spinjam-sunnyland-01.pyxel"), &Limits::viewer());
    assert_eq!(c.size(), (368, 336));
    assert_eq!(c.layers().iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["back", "front"]);
    // The old file without versions or blend modes, three layers.
    let c = open(&dir.join("dvergar-map.pyxel"), &Limits::viewer());
    assert_eq!(c.layers().iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["background", "walls", "objects"]);
}

/// Blend modes, opacity, hidden layers and groups, built by hand.
#[test]
fn pyxel_by_hand() {
    let doc = |layers: serde_json::Value| {
        serde_json::json!({"canvas": {"width": 2, "height": 1, "numLayers": layers.as_object().unwrap().len(),
            "layers": layers}, "version": "0.4.95"})
        .to_string()
    };
    let grey = png_of(&filled(2, 1, [100, 100, 100, 255]));
    let red_half = png_of(&Image8 { width: 2, height: 1, pixels: vec![200, 0, 0, 255, 0, 0, 0, 0] });
    let layers = serde_json::json!({
        "0": {"name": "top", "blendMode": "multiply", "alpha": 255, "hidden": false},
        "1": {"name": "bottom", "blendMode": "normal", "alpha": 255, "hidden": false}});
    let zip = |doc: &str, top: &[u8]| {
        slerp_archive::testutil::zip(&[("docData.json", doc.as_bytes()), ("layer0.png", top), ("layer1.png", &grey)])
    };
    let c = load_bytes("a.pyxel", &zip(&doc(layers.clone()), &red_half), &Limits::viewer()).unwrap();
    let img = c.compose(0, &shown(&*c), 1.0);
    // 100 * 200 / 255 = 78; green and blue multiply to 0.
    assert_eq!(px(&img, 0, 0), [78, 0, 0, 255]);
    assert_eq!(px(&img, 1, 0), [100, 100, 100, 255]);
    // Invert at half opacity; then the top layer hidden.
    let mut l = layers.clone();
    l["0"]["blendMode"] = "invert".into();
    l["0"]["alpha"] = 128.into();
    let c = load_bytes("a.pyxel", &zip(&doc(l.clone()), &red_half), &Limits::viewer()).unwrap();
    let p = px(&c.compose(0, &shown(&*c), 1.0), 0, 0);
    assert!((125..=130).contains(&p[0]) && p[0] == p[1], "{p:?}");
    assert!(c.layers()[1].note.contains("invert"));
    l["0"]["hidden"] = true.into();
    let c = load_bytes("a.pyxel", &zip(&doc(l), &red_half), &Limits::viewer()).unwrap();
    assert!(!c.layers()[1].visible);
    assert_eq!(px(&c.compose(0, &shown(&*c), 1.0), 0, 0), [100, 100, 100, 255]);
    // A hidden group hides the layer inside it.
    let grouped = serde_json::json!({
        "0": {"name": "inside", "parentIndex": 1, "blendMode": "normal", "alpha": 255, "hidden": false},
        "1": {"name": "group", "type": "layer_group", "parentIndex": -1, "hidden": true},
        "2": {"name": "bottom", "parentIndex": -1, "hidden": false}});
    let bytes = slerp_archive::testutil::zip(&[
        ("docData.json", doc(grouped).as_bytes()),
        ("layer0.png", &red_half),
        ("layer2.png", &grey),
    ]);
    let c = load_bytes("g.pyxel", &bytes, &Limits::viewer()).unwrap();
    assert_eq!(c.layers()[2].depth, 1);
    assert!(c.layers()[1].group);
    assert_eq!(px(&c.compose(0, &shown(&*c), 1.0), 0, 0), [100, 100, 100, 255]);
    assert_eq!(px(&c.compose(0, &[true, true, true], 1.0), 0, 0), [200, 0, 0, 255]);
    // Damaged: no document, a huge canvas, a loop of parents.
    assert!(load_bytes("x.pyxel", &slerp_archive::testutil::zip(&[("layer0.png", &grey)]), &Limits::viewer()).is_err());
    let huge = serde_json::json!({"canvas": {"width": 1 << 20, "height": 1 << 20, "layers": {}}}).to_string();
    let bytes = slerp_archive::testutil::zip(&[("docData.json", huge.as_bytes())]);
    assert!(load_bytes("x.pyxel", &bytes, &Limits::viewer()).is_err());
    let looped = serde_json::json!({
        "0": {"name": "a", "parentIndex": 1}, "1": {"name": "b", "parentIndex": 0}});
    let bytes = slerp_archive::testutil::zip(&[("docData.json", doc(looped).as_bytes()), ("layer0.png", &grey)]);
    assert!(load_bytes("x.pyxel", &bytes, &Limits::viewer()).is_ok());
}

/// Truncated and mutated copies of the real files never panic.
#[test]
fn piskel_and_pyxel_survive_damage() {
    for (folder, name) in [
        ("editors-extra/piskel", "piskelart-chr_capeWoman.piskel"),
        ("editors-extra/piskel", "godot-Icon.piskel"),
        ("editors-extra/pyxel", "adtennant-test_v0.4.8.pyxel"),
        ("editors-extra/pyxel", "squareatom-test_sample_1.pyxel"),
    ] {
        let Some(path) = asset(&format!("{folder}/{name}")) else { continue };
        let bytes = std::fs::read(&path).unwrap();
        let limits = Limits::viewer();
        for cut in (0..bytes.len()).step_by((bytes.len() / 97).max(1)) {
            if let Some(f) = sniff(&path, &bytes[..cut]) {
                let _ = load(f, &path, &bytes[..cut], &limits);
            }
        }
        let format = sniff(&path, &bytes).unwrap();
        let mut seed = 0x1234_5678u32;
        for _ in 0..300 {
            let mut copy = bytes.clone();
            for _ in 0..4 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let i = seed as usize % copy.len();
                copy[i] = (seed >> 8) as u8;
            }
            if let Ok(Asset::Canvas(c)) = load(format, &path, &copy, &limits) {
                let _ = thumbnail(&*c, 128);
            }
        }
    }
}

//endregion
