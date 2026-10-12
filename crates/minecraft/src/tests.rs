//! Whole-file tests: schematics of every layout, written here and
//! downloaded, damaged files, thinning, and textures from a resource pack.

use crate::nbt::write::{gzip, root};
use crate::nbt::{Compound, Tag};
use crate::*;
use slerp_scene::glam::Vec3;
use std::path::PathBuf;

/*┌─────────────────╮
| Writing files   |
└─────────────────╯*/
//region Writing files

fn c<'a>(entries: Vec<(&'a str, Tag<'a>)>) -> Compound<'a> {
    Compound(entries.into_iter().map(|(k, v)| (k.into(), v)).collect())
}

fn s(v: &str) -> Tag<'_> {
    Tag::String(v.into())
}

fn varints(values: &[u32]) -> Vec<u8> {
    let mut out = Vec::new();
    for &v in values {
        let mut v = v;
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                break;
            }
            out.push(b | 0x80);
        }
    }
    out
}

/// A Sponge schematic of `version` holding `cells` (x fastest, then z,
/// then y) over `palette`.
fn sponge(version: i32, size: [i16; 3], palette: &[&str], cells: &[u32]) -> Vec<u8> {
    let bytes: &'static [u8] = Box::leak(varints(cells).into_boxed_slice());
    let pal = || {
        Tag::Compound(Compound(
            palette
                .iter()
                .enumerate()
                .map(|(i, p)| (std::borrow::Cow::Owned(p.to_string()), Tag::Int(i as i32)))
                .collect(),
        ))
    };
    let mut e: Vec<(&str, Tag<'static>)> = vec![
        ("Version", Tag::Int(version)),
        ("DataVersion", Tag::Int(3465)),
        ("Width", Tag::Short(size[0])),
        ("Height", Tag::Short(size[1])),
        ("Length", Tag::Short(size[2])),
    ];
    let raw = if version == 3 {
        e.push(("Blocks", Tag::Compound(c(vec![("Palette", pal()), ("Data", Tag::ByteArray(bytes))]))));
        root("", &c(vec![("Schematic", Tag::Compound(c(e)))]))
    } else {
        e.push(("PaletteMax", Tag::Int(palette.len() as i32)));
        e.push(("Palette", pal()));
        e.push(("BlockData", Tag::ByteArray(bytes)));
        e.push(("Entities", Tag::List(vec![Tag::Compound(c(vec![("Id", s("minecraft:pig"))]))])));
        root("Schematic", &c(e))
    };
    gzip(&raw)
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("slerp-minecraft-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn load_bytes(name: &str, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    let dir = temp(&name.replace('.', "-"));
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    let format = sniff(&path, &bytes[..bytes.len().min(4096)]).ok_or(LoadError::NotMine)?;
    let r = load(format, &path, bytes, limits, &TextureSource::Off);
    let _ = std::fs::remove_dir_all(dir);
    r
}

fn detail<'a>(s: &'a Scene, key: &str) -> Option<&'a str> {
    s.info.details.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn bounds(s: &Scene) -> (Vec3, Vec3) {
    let t = s.nodes[0].transform;
    let mut lo = Vec3::MAX;
    let mut hi = Vec3::MIN;
    for p in s.meshes.iter().flat_map(|m| &m.primitives).flat_map(|p| &p.positions) {
        let w = t.translation + *p * t.scale;
        lo = lo.min(w);
        hi = hi.max(w);
    }
    (lo, hi)
}

//endregion

/*┌─────────────────╮
| Layouts         |
└─────────────────╯*/
//region Layouts

#[test]
fn sponge_versions_and_greedy_meshing() {
    // A 3 x 2 x 1 row of stone with a glass block on top of the middle.
    let palette = ["minecraft:air", "minecraft:stone", "minecraft:glass"];
    let cells = [1, 1, 1, 0, 2, 0];
    for version in [1, 2, 3] {
        let bytes = sponge(version, [3, 2, 1], &palette, &cells);
        let s = load_bytes("row.schem", &bytes, &Limits::viewer()).unwrap();
        assert_eq!(s.info.format, format!("Sponge schematic version {version}"));
        // The stone row is one box of 6 quads and the glass a cube with
        // its bottom hidden.
        let tris: usize = s.triangle_count();
        // Stone shows through glass, so the row's top is whole.
        assert_eq!(tris, 2 * 6 + 2 * 5, "version {version}");
        assert_eq!(detail(&s, "Blocks"), Some("4 of 2 kinds"));
        assert_eq!(detail(&s, "3"), Some("stone  (75.0 %)"));
        let (lo, hi) = bounds(&s);
        assert!(
            lo.abs_diff_eq(Vec3::new(-1.5, 0.0, -0.5), 1e-5) && hi.abs_diff_eq(Vec3::new(1.5, 2.0, 0.5), 1e-5),
            "{lo} {hi}"
        );
    }
}

#[test]
fn litematica_regions_sit_where_they_say() {
    // Two regions: one with a negative size (running back from its
    // position), one 10 blocks along x.
    let pack = |values: &[u32], bits: u32| -> &'static [u8] {
        let words = (values.len() as u64 * bits as u64).div_ceil(64) as usize;
        let mut longs = vec![0u64; words];
        for (i, &v) in values.iter().enumerate() {
            let start = i as u64 * bits as u64;
            let (w, o) = ((start >> 6) as usize, (start & 63) as u32);
            longs[w] |= (v as u64) << o;
            if o + bits > 64 {
                longs[w + 1] |= (v as u64) >> (64 - o);
            }
        }
        Box::leak(longs.iter().flat_map(|l| l.to_be_bytes()).collect::<Vec<u8>>().into_boxed_slice())
    };
    let xyz =
        |x: i32, y: i32, z: i32| Tag::Compound(c(vec![("x", Tag::Int(x)), ("y", Tag::Int(y)), ("z", Tag::Int(z))]));
    let state = |n: &'static str| Tag::Compound(c(vec![("Name", s(n))]));
    let region = |pos: Tag<'static>, size: Tag<'static>, palette: Vec<Tag<'static>>, longs: &'static [u8]| {
        Tag::Compound(c(vec![
            ("Position", pos),
            ("Size", size),
            ("BlockStatePalette", Tag::List(palette)),
            ("BlockStates", Tag::LongArray(longs)),
            ("TileEntities", Tag::List(Vec::new())),
        ]))
    };
    // Region a: 2 x 1 x 1 of oak planks at x 0..2 (size -2 from x = 1).
    let a = region(
        xyz(1, 0, 0),
        xyz(-2, 1, 1),
        vec![state("minecraft:air"), state("minecraft:oak_planks")],
        pack(&[1, 1], 2),
    );
    // Region b: one gold block at x = 10, a palette of five so 3 bits.
    let five = ["air", "stone", "dirt", "sand", "gold_block"]
        .map(|n| state(Box::leak(format!("minecraft:{n}").into_boxed_str())));
    let b = region(xyz(10, 0, 0), xyz(1, 1, 1), five.to_vec(), pack(&[4], 3));
    let meta = c(vec![("Name", s("Two")), ("Author", s("Slerp")), ("RegionCount", Tag::Int(2))]);
    let file = c(vec![
        ("MinecraftDataVersion", Tag::Int(3953)),
        ("Version", Tag::Int(6)),
        ("Metadata", Tag::Compound(meta)),
        ("Regions", Tag::Compound(c(vec![("planks", a), ("gold", b)]))),
    ]);
    let bytes = gzip(&root("", &file));
    assert!(stored_preview(MinecraftFormat::Litematica, &bytes).is_none(), "this one has no picture");
    let s = load_bytes("two.litematic", &bytes, &Limits::viewer()).unwrap();
    assert_eq!(detail(&s, "Regions"), Some("planks, gold"));
    assert_eq!(detail(&s, "Name"), Some("Two"));
    assert_eq!(detail(&s, "Size"), Some("11 x 1 x 1 blocks"));
    assert_eq!(detail(&s, "Blocks"), Some("3 of 2 kinds"));
    let (lo, hi) = bounds(&s);
    assert!((hi.x - lo.x - 11.0).abs() < 1e-5, "{lo} {hi}");
    assert!(detail(&s, "Game version").unwrap().contains("1.21"));
}

#[test]
fn structures_and_their_sniffing() {
    let pos = |x: i32, y: i32, z: i32| Tag::List(vec![Tag::Int(x), Tag::Int(y), Tag::Int(z)]);
    let block = |state: i32, p: Tag<'static>| Tag::Compound(c(vec![("state", Tag::Int(state)), ("pos", p)]));
    let palette = Tag::List(vec![
        Tag::Compound(c(vec![
            ("Name", s("minecraft:oak_stairs")),
            ("Properties", Tag::Compound(c(vec![("facing", s("east")), ("half", s("bottom"))]))),
        ])),
        Tag::Compound(c(vec![("Name", s("minecraft:poppy"))])),
    ]);
    let file = c(vec![
        ("DataVersion", Tag::Int(3465)),
        ("size", pos(2, 1, 1)),
        ("palette", palette),
        ("blocks", Tag::List(vec![block(0, pos(0, 0, 0)), block(1, pos(1, 0, 0)), block(1, pos(9, 9, 9))])),
        ("entities", Tag::List(Vec::new())),
    ]);
    let bytes = gzip(&root("", &file));
    let sc = load_bytes("house.nbt", &bytes, &Limits::viewer()).unwrap();
    assert_eq!(sc.info.format, "Minecraft structure");
    // Stairs: two boxes; the poppy: a green stem and a red blossom, two
    // crossed quads each.  The block outside the size is left out.
    assert_eq!(detail(&sc, "Blocks"), Some("2 of 2 kinds"));
    assert!(sc.materials.iter().any(|m| m.alpha_mode == slerp_scene::AlphaMode::Mask(0.5) && m.double_sided));
    // Other NBT files called .nbt aren't claimed.
    let other = gzip(&root("", &c(vec![("Data", Tag::Compound(c(vec![("LevelName", s("World"))])))])));
    assert!(matches!(load_bytes("level.nbt", &other, &Limits::viewer()), Err(LoadError::NotMine)));
    let dir = temp("sniff");
    for (name, bytes, want) in [
        ("a.nbt", bytes.clone(), Some(MinecraftFormat::Structure)),
        ("b.nbt", other.clone(), None),
        ("c.schem", b"PK\x03\x04 not nbt".to_vec(), None),
        ("d.schem", gzip(b"plain text"), None),
        ("e.txt", bytes.clone(), None),
    ] {
        let p = dir.join(name);
        std::fs::write(&p, &bytes).unwrap();
        assert_eq!(sniff(&p, &bytes[..bytes.len().min(4096)]), want, "{name}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn mcedit_numbers_become_blocks() {
    // 2 x 1 x 1: red wool (35:14) and an upside-down oak stair facing
    // north (53:3|4).
    let file = c(vec![
        ("Width", Tag::Short(2)),
        ("Height", Tag::Short(1)),
        ("Length", Tag::Short(1)),
        ("Materials", s("Alpha")),
        ("Blocks", Tag::ByteArray(&[35, 53])),
        ("Data", Tag::ByteArray(&[14, 7])),
    ]);
    let s = load_bytes("old.schematic", &gzip(&root("Schematic", &file)), &Limits::viewer()).unwrap();
    assert_eq!(s.info.format, "MCEdit schematic");
    let counts: Vec<&str> = s.info.details.iter().filter(|(k, _)| k == "1").map(|(_, v)| v.as_str()).collect();
    assert_eq!(counts, ["oak_stairs  (50.0 %)", "red_wool  (50.0 %)"]);
}

/// A Litematica preview picture: ARGB ints, a square, read as RGBA.
#[test]
fn litematica_preview_pictures() {
    let px: Vec<u8> =
        [0xff10_2030u32, 0x80ff_0000, 0xff00_ff00, 0xff00_00ff].iter().flat_map(|v| v.to_be_bytes()).collect();
    let px: &'static [u8] = Box::leak(px.into_boxed_slice());
    let meta = c(vec![("Name", s("Pic")), ("PreviewImageData", Tag::IntArray(px))]);
    let file = c(vec![
        ("Metadata", Tag::Compound(meta)),
        ("Regions", Tag::Compound(Compound::default())),
        ("Version", Tag::Int(6)),
    ]);
    let t = stored_preview(MinecraftFormat::Litematica, &gzip(&root("", &file))).unwrap();
    assert_eq!((t.width(), t.height()), (2, 2));
    let slerp_scene::textures::Pixels::Rgba8(p) = t.base() else { panic!() };
    assert_eq!(p[..8], [0x10, 0x20, 0x30, 0xff, 0xff, 0, 0, 0x80]);
    // Metadata after megabytes of regions: found in the whole file.
    let big: &'static [u8] = Box::leak(vec![7u8; 3 << 20].into_boxed_slice());
    let meta = c(vec![("PreviewImageData", Tag::IntArray(px))]);
    let file =
        c(vec![("Regions", Tag::Compound(c(vec![("r", Tag::ByteArray(big))]))), ("Metadata", Tag::Compound(meta))]);
    assert_eq!(stored_preview(MinecraftFormat::Litematica, &gzip(&root("", &file))).unwrap().width(), 2);
    // Not square: no picture.
    let three: &'static [u8] = Box::leak(vec![0u8; 12].into_boxed_slice());
    let file = c(vec![("Metadata", Tag::Compound(c(vec![("PreviewImageData", Tag::IntArray(three))])))]);
    assert!(stored_preview(MinecraftFormat::Litematica, &gzip(&root("", &file))).is_none());
    assert!(stored_preview(MinecraftFormat::Sponge, &gzip(&root("", &file))).is_none());
}

//endregion

/*┌─────────────────╮
| Limits          |
└─────────────────╯*/
//region Limits

/// Every cut and many corruptions of each layout never panic.
#[test]
fn damaged_files_fail_cleanly() {
    let mcedit = c(vec![
        ("Width", Tag::Short(2)),
        ("Height", Tag::Short(1)),
        ("Length", Tag::Short(2)),
        ("Blocks", Tag::ByteArray(&[35, 53, 85, 102])),
        ("Data", Tag::ByteArray(&[14, 7, 0, 0])),
        ("AddBlocks", Tag::ByteArray(&[0, 0])),
    ]);
    let pos = |x: i32| Tag::List(vec![Tag::Int(x), Tag::Int(0), Tag::Int(0)]);
    let structure = c(vec![
        ("size", Tag::List(vec![Tag::Int(2), Tag::Int(1), Tag::Int(1)])),
        ("palette", Tag::List(vec![Tag::Compound(c(vec![("Name", s("minecraft:glass_pane"))]))])),
        (
            "blocks",
            Tag::List(vec![
                Tag::Compound(c(vec![("state", Tag::Int(0)), ("pos", pos(0))])),
                Tag::Compound(c(vec![("state", Tag::Int(0)), ("pos", pos(1))])),
            ]),
        ),
    ]);
    let samples = [
        (
            "a.schem",
            sponge(2, [3, 2, 1], &["minecraft:air", "minecraft:stone", "minecraft:oak_fence"], &[1, 2, 1, 0, 2, 0]),
        ),
        (
            "b.schem",
            sponge(3, [2, 2, 2], &["minecraft:glass", "minecraft:oak_stairs[facing=west]"], &[0, 1, 1, 0, 0, 1, 1, 0]),
        ),
        ("c.schematic", gzip(&root("Schematic", &mcedit))),
        ("d.nbt", gzip(&root("", &structure))),
    ];
    let started = std::time::Instant::now();
    for (name, good) in samples {
        // The raw NBT too, so corruptions reach the parser and layouts
        // rather than failing the gzip check.
        let raw = crate::nbt::decompress(&good, 1 << 20).unwrap().into_owned();
        for bytes in [good.clone(), raw] {
            let n = bytes.len();
            for cut in (0..n).step_by((n / 64).max(1)) {
                let _ = load_bytes(name, &bytes[..cut], &Limits::viewer());
            }
            let mut seed = 0x9e37_79b9u32;
            for _ in 0..300 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let mut bad = bytes.clone();
                let at = seed as usize % n;
                bad[at] = (seed >> 8) as u8;
                let _ = load_bytes(name, &bad, &Limits::viewer());
            }
        }
    }
    // A size far larger than the data, and a decompression bomb.
    let lie = sponge(2, [i16::MAX, i16::MAX, i16::MAX], &["minecraft:stone"], &[0; 8]);
    assert!(load_bytes("lie.schem", &lie, &Limits::viewer()).is_err());
    let mut zeros = vec![0u8; 300 << 20];
    zeros[..11].copy_from_slice(&[10, 0, 0, 7, 0, 1, b'x', 0x12, 0xc0, 0, 0]);
    let bomb = gzip(&zeros);
    assert!(matches!(load_bytes("bomb.schem", &bomb, &Limits::thumbnail()), Err(LoadError::TooLarge(_))));
    assert!(started.elapsed().as_secs() < 60, "{:?}", started.elapsed());
}

/// A build of tens of millions of blocks is thinned for thumbnails and
/// meshes in a fraction of a second.
#[test]
fn large_builds_are_thinned_for_thumbnails() {
    let (w, h, l) = (400usize, 128usize, 400usize);
    let mut cells = vec![0u32; w * h * l];
    for y in 0..h {
        for z in 0..l {
            for x in 0..w {
                // Hills of stone under grass, with glass towers.
                let ground = 40 + ((x as f32 * 0.05).sin() * 12.0 + (z as f32 * 0.07).cos() * 10.0) as usize;
                let tower = x % 50 < 6 && z % 50 < 6 && y < 110;
                cells[(y * l + z) * w + x] = if y < ground {
                    1
                } else if y == ground {
                    2
                } else if tower {
                    3
                } else {
                    0
                };
            }
        }
    }
    let bytes = sponge(2, [w as i16, h as i16, l as i16], &["air", "stone", "grass_block", "glass"], &cells);
    let started = std::time::Instant::now();
    let s = load_bytes("big.schem", &bytes, &Limits::thumbnail()).unwrap();
    let took = started.elapsed();
    eprintln!("{} blocks: {} triangles in {took:?}, {:?}", w * h * l, s.triangle_count(), detail(&s, "Shown"));
    assert!(detail(&s, "Shown").is_some(), "20 million cells are thinned");
    assert!(s.nodes[0].transform.scale.x > 1.0);
    let viewer = load_bytes("big.schem", &bytes, &Limits::viewer()).unwrap();
    assert!(detail(&viewer, "Shown").is_none(), "the viewer keeps every block");
    assert!(viewer.triangle_count() > s.triangle_count());
}

//endregion

/*┌─────────────────╮
| Textures        |
└─────────────────╯*/
//region Textures

fn png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut e = png::Encoder::new(&mut out, width, height);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(rgba).unwrap();
    out
}

/// A resource pack beside the schematic dresses its blocks: a cube_all
/// stone, a cube_column log, and grass tinted green.
#[test]
fn resource_pack_textures() {
    let dir = temp("pack");
    let red = png(16, 16, &[200, 20, 20, 255].repeat(256));
    let grey = png(16, 32, &[128, 128, 128, 255].repeat(512));
    let files: Vec<(&str, &[u8])> = vec![
        ("pack.mcmeta", b"{\"pack\":{\"pack_format\":15,\"description\":\"test\"}}"),
        ("assets/minecraft/blockstates/stone.json", br#"{"variants":{"":{"model":"minecraft:block/stone"}}}"#),
        ("assets/minecraft/models/block/stone.json", br#"{"parent":"minecraft:block/cube_all","textures":{"all":"minecraft:block/stone"}}"#),
        (
            "assets/minecraft/models/block/cube_all.json",
            br##"{"parent":"block/cube","textures":{"particle":"#all","down":"#all","up":"#all","north":"#all","east":"#all","south":"#all","west":"#all"}}"##,
        ),
        (
            "assets/minecraft/models/block/cube.json",
            br##"{"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"down":{"texture":"#down"},"up":{"texture":"#up"},"north":{"texture":"#north"},"south":{"texture":"#south"},"west":{"texture":"#west"},"east":{"texture":"#east"}}}]}"##,
        ),
        ("assets/minecraft/blockstates/short_grass.json", br#"{"variants":{"":{"model":"block/short_grass"}}}"#),
        ("assets/minecraft/models/block/short_grass.json", br#"{"parent":"block/tinted_cross","textures":{"cross":"block/short_grass"}}"#),
        (
            "assets/minecraft/models/block/tinted_cross.json",
            br##"{"elements":[{"from":[0.8,0,8],"to":[15.2,16,8],"faces":{"north":{"texture":"#cross","tintindex":0},"south":{"texture":"#cross","tintindex":0}}}]}"##,
        ),
        ("assets/minecraft/textures/block/stone.png", &red),
        ("assets/minecraft/textures/block/short_grass.png", &grey),
    ];
    std::fs::write(dir.join("pack.zip"), slerp_archive::testutil::zip(&files)).unwrap();
    let path = dir.join("t.schem");
    let bytes = sponge(2, [2, 1, 1], &["minecraft:stone", "minecraft:short_grass"], &[0, 1]);
    std::fs::write(&path, &bytes).unwrap();
    let s = load(MinecraftFormat::Sponge, &path, &bytes, &Limits::viewer(), &TextureSource::Auto).unwrap();
    assert!(detail(&s, "Block textures").unwrap().ends_with("pack.zip"), "{:?}", detail(&s, "Block textures"));
    // The grass wears its texture, square (the first frame), tinted.
    let grass = s.materials.iter().find(|m| m.double_sided && m.base_color.y > m.base_color.x).expect("tinted grass");
    let tex = &s.textures[grass.base_color_texture.unwrap().texture];
    assert_eq!((tex.width(), tex.height()), (16, 16));
    let stone = s.materials.iter().find(|m| !m.double_sided).unwrap();
    assert_eq!(stone.base_color.truncate(), Vec3::ONE);
    // Off means the colour table even with a pack beside it.
    let plain = load(MinecraftFormat::Sponge, &path, &bytes, &Limits::viewer(), &TextureSource::Off).unwrap();
    assert!(detail(&plain, "Block textures").unwrap().starts_with("built-in"));
    let _ = std::fs::remove_dir_all(dir);
}

//endregion

/*┌─────────────────╮
| Real files      |
└─────────────────╯*/
//region Real files

fn asset(rel: &str) -> Option<PathBuf> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets").join(rel);
    if p.exists() {
        Some(p)
    } else {
        eprintln!("skipping: {} is missing", p.display());
        None
    }
}

/// Schematics from prismarine-schematic and schematic4j's tests (MIT),
/// Sponge 1 to 3, Litematica 5 and 6, MCEdit and Schematica, and the ones
/// `tools/make-minecraft-samples.ts` writes.
#[test]
fn downloaded_schematics() {
    let files = [
        "minecraft-extra/prismarine-schematic/smallhouse1.schem",
        "minecraft-extra/prismarine-schematic/viking-house1.schematic",
        "minecraft-extra/prismarine-schematic/worldedit-v3.schem",
        "minecraft-extra/schematic4j/litematica/v5/island.litematic",
        "minecraft-extra/schematic4j/litematica/v5/mansion.litematic",
        "minecraft-extra/schematic4j/litematica/v5/simple.litematic",
        "minecraft-extra/schematic4j/litematica/v5/tower.litematic",
        "minecraft-extra/schematic4j/litematica/v6/demo.litematic",
        "minecraft-extra/schematic4j/schematica/12727.schematic",
        "minecraft-extra/schematic4j/schematica/9383.schematic",
        "minecraft-extra/schematic4j/sponge/v1/sponge-v1.schem",
        "minecraft-extra/schematic4j/sponge/v2/green-cottage.schem",
        "minecraft-extra/schematic4j/sponge/v2/interieur-exterieur-chunk-project.schem",
        "minecraft-extra/schematic4j/sponge/v2/issue-1.schem",
        "minecraft-extra/schematic4j/sponge/v3/sponge-v3.schem",
        "minecraft-extra/synthetic/castle-town.schem",
        "minecraft-extra/synthetic/regions.litematic",
        "minecraft-extra/synthetic/well.nbt",
        "minecraft-extra/synthetic/classic.schematic",
    ];
    for rel in files {
        let Some(p) = asset(rel) else { continue };
        let bytes = std::fs::read(&p).unwrap();
        let format = sniff(&p, &bytes[..bytes.len().min(4096)]).unwrap_or_else(|| panic!("{rel} not recognised"));
        let s =
            load(format, &p, &bytes, &Limits::viewer(), &TextureSource::Off).unwrap_or_else(|e| panic!("{rel}: {e}"));
        eprintln!(
            "{rel}: {}, {} triangles, {}, guessed {:?}",
            s.info.format,
            s.triangle_count(),
            detail(&s, "Blocks").unwrap_or(""),
            detail(&s, "Guessed colours")
        );
        assert!(s.triangle_count() > 0, "{rel}");
    }
}

//endregion
