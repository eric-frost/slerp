//! Tests against LDtk's own sample projects (`deepnight/ldtk`'s
//! `app/extraFiles/samples`, MIT, in `local/assets/editors-extra/ldtk/`;
//! skipped when they aren't there) and against small hand-written files.
//! `SLERP_TEST_OUT=<dir>` saves every composite for a look.

use super::{Pick, load, parts};
use crate::{SpriteFormat, sniff};
use slerp_scene::{Canvas, Image8, Limits};
use std::path::{Path, PathBuf};

fn samples() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/editors-extra/ldtk");
    if p.exists() {
        Some(p)
    } else {
        eprintln!("skipping: {} is missing", p.display());
        None
    }
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

fn compose_all(c: &dyn Canvas) -> Image8 {
    let shown: Vec<bool> = c.layers().iter().map(|l| l.visible).collect();
    c.compose(0, &shown, 1.0)
}

/// How many pixels are opaque and not the level background: a rough
/// measure that tiles were drawn.
fn covered(img: &Image8) -> usize {
    img.pixels.chunks_exact(4).filter(|p| p[3] > 0).count()
}

#[test]
fn every_sample_loads_whole_and_by_level() {
    let Some(dir) = samples() else { return };
    let mut projects: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "ldtk"))
        .collect();
    projects.sort();
    assert!(projects.len() >= 14, "{} projects", projects.len());
    for path in projects {
        let bytes = std::fs::read(&path).unwrap();
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        assert_eq!(sniff(&path, &bytes[..bytes.len().min(4096)]), Some(SpriteFormat::Ldtk), "{name}");
        let start = std::time::Instant::now();
        let world = load(&path, &bytes, &Limits::viewer(), Pick::World).unwrap_or_else(|e| panic!("{name}: {e}"));
        let img = compose_all(&world);
        let took = start.elapsed();
        let thumb = std::time::Instant::now();
        let small = load(&path, &bytes, &Limits::thumbnail(), Pick::World).unwrap();
        let _ = crate::thumbnail(&small, 256);
        eprintln!(
            "{name}: {} x {}, viewer {:.1} ms, thumbnail {:.1} ms, warnings {:?}",
            img.width,
            img.height,
            took.as_secs_f64() * 1000.0,
            thumb.elapsed().as_secs_f64() * 1000.0,
            world.info().warnings
        );
        save(&format!("ldtk-{name}"), &img);
        assert!(covered(&img) > 0, "{name} drew nothing");
        // Only LDtk's built-in icons may go missing: every sample's
        // pictures sit beside it.
        for w in &world.info().warnings {
            assert!(w.contains("built-in icon"), "{name}: {w}");
        }
        for level in parts(&bytes).unwrap() {
            let one = load(&path, &bytes, &Limits::viewer(), Pick::Level(&level)).unwrap();
            assert_eq!(one.info().format, "LDtk level");
            let img = compose_all(&one);
            assert!(covered(&img) > 0, "{name} {level} drew nothing");
            save(&format!("ldtk-{name}-{level}"), &img);
        }
    }
}

#[test]
fn worlds_place_their_levels() {
    let Some(dir) = samples() else { return };
    // A free world: levels at their own coordinates, the first one at the
    // origin and another 352 pixels above it.
    let path = dir.join("Typical_2D_platformer_example.ldtk");
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(parts(&bytes).unwrap(), ["Your_typical_2D_platformer", "Top", "Bottom", "World_Level_3"]);
    let world = load(&path, &bytes, &Limits::viewer(), Pick::World).unwrap();
    assert_eq!(world.bounds[..2], [0, -352]);
    assert_eq!(world.size(), (1152, 944));
    // A row of levels wraps into a block: four 256-pixel levels make two
    // rows of two.
    let path = dir.join("AutoLayers_6_OptionalRules.ldtk");
    let bytes = std::fs::read(&path).unwrap();
    let world = load(&path, &bytes, &Limits::viewer(), Pick::World).unwrap();
    assert_eq!(world.size(), (256 * 2 + 32, 256 * 2 + 32));
    // A project with one level has no parts: it is that level.
    let path = dir.join("Entities.ldtk");
    let bytes = std::fs::read(&path).unwrap();
    assert!(parts(&bytes).unwrap().is_empty());
    let one = load(&path, &bytes, &Limits::viewer(), Pick::World).unwrap();
    assert_eq!(one.info().format, "LDtk level");
    assert_eq!(one.size(), (816, 464));
}

#[test]
fn layers_toggle() {
    let Some(dir) = samples() else { return };
    let path = dir.join("Typical_2D_platformer_example.ldtk");
    let bytes = std::fs::read(&path).unwrap();
    let level = load(&path, &bytes, &Limits::viewer(), Pick::Level("Your_typical_2D_platformer")).unwrap();
    let names: Vec<&str> = level.layers().iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["Background", "Bg_textures", "Collisions", "Wall_shadows", "Entities"]);
    let all = compose_all(&level);
    // Without the background, the sky between tiles is transparent.
    let without_bg = level.compose(0, &[false, true, true, true, true], 1.0);
    assert!(covered(&without_bg) < covered(&all));
    // Nothing shown, nothing drawn.
    let none = level.compose(0, &[false; 5], 1.0);
    assert_eq!(covered(&none), 0);
}

#[test]
fn separate_level_files() {
    let Some(dir) = samples() else { return };
    let path = dir.join("SeparateLevelFiles.ldtk");
    let bytes = std::fs::read(&path).unwrap();
    let world = load(&path, &bytes, &Limits::viewer(), Pick::World).unwrap();
    assert_eq!(world.info().neighbours.len(), 3, "{:?}", world.info().neighbours);
    assert!(world.levels.iter().all(|l| !l.level.layers.is_empty()));
    // A level file opened on its own finds its project above it.
    let level = dir.join("SeparateLevelFiles/World_Level_1.ldtkl");
    let lb = std::fs::read(&level).unwrap();
    assert_eq!(sniff(&level, &lb), Some(SpriteFormat::Ldtk));
    let one = load(&level, &lb, &Limits::viewer(), Pick::World).unwrap();
    assert_eq!(one.info().format, "LDtk level");
    assert!(one.info().details.iter().any(|(k, v)| k == "Level" && v == "World_Level_1"));
    assert!(one.info().neighbours.contains(&path));
    // Without neighbours it still draws what it carries.
    let alone = Limits { neighbours: false, ..Limits::thumbnail() };
    let one = load(&level, &lb, &alone, Pick::World).unwrap();
    assert!(covered(&compose_all(&one)) > 0);
    let world = load(&path, &bytes, &alone, Pick::World).unwrap();
    assert!(world.info().warnings.iter().any(|w| w.contains("separate level files")));
}

#[test]
fn missing_tilesets_draw_cells() {
    let Some(dir) = samples() else { return };
    let path = dir.join("Typical_TopDown_example.ldtk");
    let bytes = std::fs::read(&path).unwrap();
    let alone = Limits { neighbours: false, ..Limits::thumbnail() };
    let world = load(&path, &bytes, &alone, Pick::World).unwrap();
    assert!(world.info().warnings.iter().any(|w| w.contains("coloured cells")));
    let with = load(&path, &bytes, &Limits::viewer(), Pick::World).unwrap();
    let (a, b) = (compose_all(&world), compose_all(&with));
    assert_eq!((a.width, a.height), (b.width, b.height));
    assert!(a.pixels != b.pixels);
    save("ldtk-topdown-no-tilesets", &a);
}

/// A small hand-written project: one level with an IntGrid layer, a tile
/// layer whose picture is missing and an entity of each shape.
const SMALL: &str = r##"{
  "__header__": { "fileType": "LDtk Project JSON", "appVersion": "1.5.3" },
  "jsonVersion": "1.5.3", "worldLayout": "Free", "defaultGridSize": 8,
  "defs": {
    "layers": [
      { "uid": 3, "identifier": "Things", "type": "Entities" },
      { "uid": 2, "identifier": "Tiles", "type": "Tiles" },
      { "uid": 1, "identifier": "Walls", "type": "IntGrid",
        "intGridValues": [ { "value": 1, "color": "#FF0000" }, { "value": 2, "color": "#00FF00" } ] }
    ],
    "entities": [
      { "uid": 10, "renderMode": "Rectangle", "color": "#0000FF", "fillOpacity": 1, "lineOpacity": 0 },
      { "uid": 11, "renderMode": "Ellipse", "color": "#FFFF00", "fillOpacity": 1, "lineOpacity": 0 }
    ],
    "tilesets": [ { "uid": 20, "relPath": "nowhere.png", "tileGridSize": 8 } ]
  },
  "levels": [ {
    "identifier": "L", "iid": "a", "worldX": 0, "worldY": 0, "pxWid": 32, "pxHei": 16, "__bgColor": "#000000",
    "layerInstances": [
      { "__type": "Entities", "layerDefUid": 3, "__gridSize": 8, "__opacity": 1, "visible": true,
        "entityInstances": [
          { "defUid": 10, "px": [16, 0], "width": 8, "height": 8, "__pivot": [0, 0] },
          { "defUid": 11, "px": [28, 12], "width": 8, "height": 8, "__pivot": [0.5, 0.5], "__smartColor": "#FFFFFF" }
        ] },
      { "__type": "Tiles", "layerDefUid": 2, "__gridSize": 8, "__opacity": 1, "visible": true, "__tilesetDefUid": 20,
        "gridTiles": [ { "px": [8, 8], "src": [0, 0], "f": 0, "t": 0 } ] },
      { "__type": "IntGrid", "layerDefUid": 1, "__gridSize": 8, "__cWid": 4, "__cHei": 2, "__opacity": 1,
        "visible": true, "intGridCsv": [1, 0, 0, 0, 2, 0, 0, 0] }
    ]
  } ]
}"##;

fn px(img: &Image8, x: u32, y: u32) -> [u8; 4] {
    crate::pixels::get(img, x, y)
}

#[test]
fn hand_written_project() {
    let path = Path::new("/nonexistent/small.ldtk");
    let ldtk = load(path, SMALL.as_bytes(), &Limits::viewer(), Pick::World).unwrap();
    let img = compose_all(&ldtk);
    assert_eq!((img.width, img.height), (32, 16));
    assert_eq!(px(&img, 2, 2), [255, 0, 0, 255], "IntGrid value 1");
    assert_eq!(px(&img, 2, 10), [0, 255, 0, 255], "IntGrid value 2");
    assert_eq!(px(&img, 12, 2), [0, 0, 0, 255], "background");
    assert_eq!(px(&img, 18, 2), [0, 0, 255, 255], "rectangle entity");
    assert_eq!(px(&img, 28, 12), [255, 255, 255, 255], "ellipse, smart colour, centred on its pivot");
    assert_eq!(px(&img, 24, 8), [0, 0, 0, 255], "outside the ellipse");
    let cell = px(&img, 12, 12);
    assert!(cell != [0, 0, 0, 255] && cell[3] == 255, "missing tile drawn as a cell: {cell:?}");
    assert!(ldtk.info().warnings.iter().any(|w| w.contains("nowhere.png")));
    // Scaled down, still the same layout.
    let half = ldtk.compose(0, &[true; 4], 0.5);
    assert_eq!((half.width, half.height), (16, 8));
    assert_eq!(px(&half, 1, 1), [255, 0, 0, 255]);
}

#[test]
fn damaged_files_fail_cleanly() {
    let mut inputs: Vec<(String, Vec<u8>)> = vec![("small".into(), SMALL.as_bytes().to_vec())];
    if let Some(dir) = samples() {
        for name in
            ["Typical_2D_platformer_example.ldtk", "Entities.ldtk", "Test_file_for_API_showing_all_features.ldtk"]
        {
            inputs.push((name.into(), std::fs::read(dir.join(name)).unwrap()));
        }
    }
    let dir = samples().unwrap_or_else(|| PathBuf::from("/nonexistent"));
    let limits = Limits::thumbnail();
    for (name, bytes) in &inputs {
        let path = dir.join(name);
        // 1. Truncated anywhere: JSON errors, never panics.
        for cut in (0..bytes.len()).step_by((bytes.len() / 97).max(1)) {
            let _ = load(&path, &bytes[..cut], &limits, Pick::World);
            let _ = parts(&bytes[..cut]);
        }
        // 2. Numbers changed to wild values, which still parse: nothing
        //    may panic or allocate without bound.
        let text = String::from_utf8_lossy(bytes);
        for (i, wild) in ["-1", "4294967296", "-99999999999", "1e300", "0", "null", "\"x\"", "[]"].iter().enumerate() {
            let mut seed = 0x9E37_79B9u32.wrapping_mul(i as u32 + 1);
            let mut out = String::with_capacity(text.len());
            let mut chars = text.chars().peekable();
            while let Some(c) = chars.next() {
                if c.is_ascii_digit() {
                    while chars.peek().is_some_and(|d| d.is_ascii_digit() || *d == '.') {
                        chars.next();
                    }
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    out.push_str(if seed.is_multiple_of(7) { wild } else { "3" });
                } else {
                    out.push(c);
                }
            }
            if let Ok(c) = load(&path, out.as_bytes(), &limits, Pick::World) {
                let _ = crate::thumbnail(&c, 128);
            }
        }
    }
}
