use super::automap::{FLOOR_STEP, WALL};
use super::*;
use crate::testutil::{self, asset};
use slerp_scene::textures::Pixels;

/// Builds a WAD from (name, data) lumps.
fn build(magic: &[u8; 4], lumps: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = magic.to_vec();
    out.extend_from_slice(&(lumps.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    let mut dir = Vec::new();
    for (name, data) in lumps {
        dir.extend_from_slice(&(out.len() as u32).to_le_bytes());
        dir.extend_from_slice(&(data.len() as u32).to_le_bytes());
        let mut n = [0u8; 8];
        n[..name.len()].copy_from_slice(name.as_bytes());
        dir.extend_from_slice(&n);
        out.extend_from_slice(data);
    }
    let at = out.len() as u32;
    out[8..12].copy_from_slice(&at.to_le_bytes());
    out.extend_from_slice(&dir);
    out
}

/// A square room with a raised step: four one-sided walls and one
/// two-sided line between sectors of different floor heights.
fn room() -> Vec<u8> {
    let le = |v: &[i16]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let vertexes = le(&[0, 0, 256, 0, 256, 256, 0, 256, 128, 0, 128, 256]);
    let line = |v1: i16, v2: i16, front: i16, back: i16| le(&[v1, v2, 0, 0, 0, front, back]);
    let mut linedefs = Vec::new();
    linedefs.extend(line(0, 4, 0, -1));
    linedefs.extend(line(4, 1, 1, -1));
    linedefs.extend(line(1, 2, 1, -1));
    linedefs.extend(line(2, 5, 1, -1));
    linedefs.extend(line(5, 3, 0, -1));
    linedefs.extend(line(3, 0, 0, -1));
    linedefs.extend(line(4, 5, 0, 1));
    let side = |sector: i16| {
        let mut s = vec![0u8; 28];
        s.extend_from_slice(&sector.to_le_bytes());
        s
    };
    let sector = |floor: i16| {
        let mut s = le(&[floor, 128]);
        s.resize(26, 0);
        s
    };
    build(
        b"PWAD",
        &[
            ("MAP01", vec![]),
            ("THINGS", vec![]),
            ("LINEDEFS", linedefs),
            ("SIDEDEFS", [side(0), side(1)].concat()),
            ("VERTEXES", vertexes),
            ("SECTORS", [sector(0), sector(24)].concat()),
        ],
    )
}

/// A 256-unit square room of two sectors split north-south, lines wound
/// so each sector is on its sides' right: sector 0 (west, floor 0) and
/// sector 1 (east, a 24-unit step up), walls textured WALL, the step's
/// face STEP, player 1 facing north in sector 0 and an imp in sector 1.
fn room3d() -> Vec<u8> {
    let le = |v: &[i16]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let vertexes = le(&[0, 0, 256, 0, 256, 256, 0, 256, 128, 0, 128, 256]);
    let line = |v1: i16, v2: i16, flags: i16, front: i16, back: i16| le(&[v1, v2, flags, 0, 0, front, back]);
    let mut linedefs = Vec::new();
    for (k, (v1, v2)) in [(0, 3), (3, 5), (4, 0), (5, 2), (2, 1), (1, 4)].into_iter().enumerate() {
        linedefs.extend(line(v1, v2, 1, k as i16, -1));
    }
    linedefs.extend(line(5, 4, 4, 6, 7));
    let side = |upper: &str, lower: &str, middle: &str, sector: i16| {
        let mut s = le(&[0, 0]);
        for name in [upper, lower, middle] {
            let mut n = [0u8; 8];
            n[..name.len()].copy_from_slice(name.as_bytes());
            s.extend_from_slice(&n);
        }
        s.extend_from_slice(&sector.to_le_bytes());
        s
    };
    let mut sidedefs = Vec::new();
    for k in 0..6 {
        sidedefs.extend(side("-", "-", "WALL", if k < 3 { 0 } else { 1 }));
    }
    sidedefs.extend(side("-", "STEP", "-", 0));
    sidedefs.extend(side("-", "-", "-", 1));
    let sector = |floor: i16| {
        let mut s = le(&[floor, 128]);
        s.extend_from_slice(b"FLOOR\0\0\0CEIL\0\0\0\0");
        s.extend_from_slice(&le(&[160, 0, 0]));
        s
    };
    let things = le(&[64, 128, 90, 1, 7, 192, 128, 180, 3001, 7]);
    build(
        b"PWAD",
        &[
            ("MAP01", vec![]),
            ("THINGS", things),
            ("LINEDEFS", linedefs),
            ("SIDEDEFS", sidedefs),
            ("VERTEXES", vertexes),
            ("SECTORS", [sector(0), sector(24)].concat()),
        ],
    )
}

fn colours(t: &Texture) -> Vec<[u8; 3]> {
    let Pixels::Rgba8(p) = t.base() else { panic!() };
    let mut c: Vec<[u8; 3]> = p.chunks_exact(4).map(|p| [p[0], p[1], p[2]]).collect();
    c.sort_unstable();
    c.dedup();
    c
}

#[test]
fn pwad_without_palette_draws_its_map_first() {
    let g = load(&room(), &Limits::viewer()).unwrap();
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].name, "MAP01");
    let t = &g[0].texture;
    assert_eq!(t.width().max(t.height()), AUTOMAP_SIZE);
    let c = colours(t);
    let pal =
        |i: u8| [DOOM_PALETTE[i as usize * 3], DOOM_PALETTE[i as usize * 3 + 1], DOOM_PALETTE[i as usize * 3 + 2]];
    assert!(c.contains(&[0, 0, 0]), "black background");
    assert!(c.contains(&pal(WALL)), "red walls");
    assert!(c.contains(&pal(FLOOR_STEP)), "brown step");
    let thumb = load(&room(), &Limits::thumbnail()).unwrap();
    assert_eq!(thumb[0].texture.width(), 512);
}

#[test]
fn udmf_maps() {
    let text = br#"namespace = "zdoom";
        // a triangle
        vertex { x = 0.0; y = 0.0; }
        vertex { x = 64.0; y = 0.0; }
        vertex { x = 0.0; y = 64.0; }
        sector { heightfloor = 0; heightceiling = 128; }
        sidedef { sector = 0; }
        linedef { v1 = 0; v2 = 1; sidefront = 0; }
        linedef { v1 = 1; v2 = 2; sidefront = 0; dontdraw = true; }
        /* block comment */
        linedef { v1 = 2; v2 = 0; sidefront = 0; }"#;
    let wad = build(b"PWAD", &[("E1M1", vec![]), ("TEXTMAP", text.to_vec()), ("ENDMAP", vec![])]);
    let g = load(&wad, &Limits::viewer()).unwrap();
    assert_eq!(g[0].name, "E1M1");
    assert!(g[0].texture.description_lines() == 2);
}

trait Lines {
    fn description_lines(&self) -> usize;
}

impl Lines for Arc<Texture> {
    fn description_lines(&self) -> usize {
        self.source.format.split_whitespace().nth(2).and_then(|n| n.parse().ok()).unwrap_or(0)
    }
}

#[test]
fn patches_decode_with_gaps() {
    // A 2x3 patch: column 0 has rows 0-1, column 1 has row 2 only.
    let mut p = Vec::new();
    p.extend_from_slice(&[2, 0, 3, 0, 0, 0, 0, 0]);
    p.extend_from_slice(&16u32.to_le_bytes());
    p.extend_from_slice(&23u32.to_le_bytes());
    p.extend_from_slice(&[0, 2, 0, 5, 6, 0, 255]);
    p.extend_from_slice(&[2, 1, 0, 7, 0, 255]);
    let (w, h, rgba) = patch(&p, &DOOM_PALETTE[..]).unwrap();
    assert_eq!((w, h), (2, 3));
    let alpha: Vec<u8> = rgba.chunks_exact(4).map(|p| p[3]).collect();
    assert_eq!(alpha, [255, 0, 255, 0, 0, 255]);
    for cut in 0..p.len() - 1 {
        assert!(patch(&p[..cut], &DOOM_PALETTE[..]).is_err() || cut >= 16);
    }
}

#[test]
fn corrupt_wads_never_panic() {
    let wad = room();
    for (i, m) in testutil::mutations(&wad).iter().enumerate() {
        let r = std::panic::catch_unwind(|| load(m, &Limits::viewer()).map(|g| g.len()));
        assert!(r.is_ok(), "mutation {i} panicked");
    }
    assert!(load(b"PWAD\xff\xff\xff\x7f\x0c\0\0\0", &Limits::viewer()).is_err());
    assert!(load(&build(b"IWAD", &[("DEMO1", vec![1, 2, 3])]), &Limits::viewer()).is_err());
}

#[test]
fn freedoom() {
    for (file, first_map) in [("freedoom1.wad", "E1M1"), ("freedoom2.wad", "MAP01")] {
        let rel = format!("doom/freedoom/{file}");
        let Some(p) = asset(&rel) else { return };
        let bytes = std::fs::read(&p).unwrap();
        let t = std::time::Instant::now();
        let g = load(&bytes, &Limits::thumbnail()).unwrap();
        let thumb_time = t.elapsed();
        let t = std::time::Instant::now();
        let full = load(&bytes, &Limits::viewer()).unwrap();
        eprintln!("{file}: {} entries, thumbnail {thumb_time:?}, viewer {:?}", full.len(), t.elapsed());
        assert_eq!(g[0].name, "TITLEPIC");
        assert_eq!((g[0].texture.width(), g[0].texture.height()), (320, 200));
        assert_eq!(g[1].name, first_map);
        assert!(g.len() > 20);
        if file == "freedoom2.wad" {
            let Pixels::Rgba8(px) = full[0].texture.base() else { panic!() };
            testutil::write_png(Path::new("/tmp/slerp-quake/freedoom2-titlepic.png"), 320, 200, px);
            let m = &full[1].texture;
            let Pixels::Rgba8(px) = m.base() else { panic!() };
            testutil::write_png(Path::new("/tmp/slerp-quake/freedoom2-map01.png"), m.width(), m.height(), px);
        }
    }
}

use super::archive::{self, Namespace, Source};
use std::path::Path;

/*┌─────────────────╮
| 3D levels       |
└─────────────────╯*/
//region 3D levels

use super::graphics::Graphics;
use super::level::{self, EYE_HEIGHT, to_world};
use super::polygon;
use super::resources::{ResFile, Resources};
use slerp_scene::glam::Vec3;

fn read_asset(rel: &str) -> Option<Vec<u8>> {
    asset(rel).map(|p| std::fs::read(p).unwrap())
}

/// Builds map `name` of `wad` over `iwad`, returning the scene, the build
/// statistics and the time taken (parsing the map included).
fn build_timed(wad: &Wad, iwad: Option<&Wad>, at: usize, name: &str) -> (Scene, level::Stats, std::time::Duration) {
    let t = std::time::Instant::now();
    let map = wad.map(at).unwrap();
    let files: Vec<ResFile> = iwad.into_iter().chain([wad]).map(ResFile::Wad).collect();
    let res = Resources::new(files);
    let mut gfx = Graphics::new(&res);
    let (scene, stats) = level::build(&map, name, &mut gfx, &Limits::viewer(), &level::Extras::default()).unwrap();
    (scene, stats, t.elapsed())
}

/// Per sector, the area of its subsectors (from the map's nodes) against
/// the area of its triangles: (sectors that differ by more than 1%, total
/// subsector area, total triangle area over the same sectors).
fn bsp_coverage(map: &map::MapData) -> (usize, f64, f64) {
    let Some(bsp) = &map.bsp else { return (0, 0.0, 0.0) };
    let mut from_bsp = vec![0.0f64; map.sectors.len()];
    for (s, poly) in polygon::subsector_polygons(map, bsp) {
        let mut a = 0.0;
        for i in 0..poly.len() {
            let (p, q) = (poly[i], poly[(i + 1) % poly.len()]);
            a += p[0] * q[1] - q[0] * p[1];
        }
        from_bsp[s] += a * 0.5;
    }
    let areas = polygon::sector_areas(map);
    let (mut bad, mut total_bsp, mut total_tri) = (0, 0.0, 0.0);
    for (s, area) in areas.iter().enumerate() {
        let tri = area.triangle_area();
        total_bsp += from_bsp[s];
        total_tri += tri;
        if (tri - from_bsp[s]).abs() > 0.01 * from_bsp[s].max(tri) + 1.0 {
            bad += 1;
        }
    }
    (bad, total_bsp, total_tri)
}

/// Every map of Freedoom's two IWADs, of the 2xreject PWAD (over Freedoom:
/// Phase 2) and of Doom's commercial IWADs where they are present builds
/// without errors or floods of warnings, quickly,
/// with its triangles covering the area its loops enclose and the area
/// its nodes' subsectors cover.  Run with `--nocapture` for the table.
#[test]
fn every_map_builds() {
    let Some(fd2) = read_asset("doom/freedoom/freedoom2.wad") else { return };
    let iwad2 = Wad::parse(&fd2).unwrap();
    // The commercial IWADs too, where a developer has copied their own
    // (not redistributable, so never in the test corpus).
    let sets: [(&str, bool); 7] = [
        ("doom/freedoom/freedoom1.wad", false),
        ("doom/freedoom/freedoom2.wad", false),
        ("doom/2xreject/2xreject.wad", true),
        ("doom-commercial/DOOM.WAD", false),
        ("doom-commercial/DOOM2.WAD", false),
        ("doom-commercial/TNT.WAD", false),
        ("doom-commercial/PLUTONIA.WAD", false),
    ];
    let mut slowest = (String::new(), std::time::Duration::ZERO);
    for (rel, over_iwad) in sets {
        let Some(bytes) = read_asset(rel) else { continue };
        let wad = Wad::parse(&bytes).unwrap();
        let maps = wad.maps();
        assert!(!maps.is_empty(), "{rel}");
        for (at, name) in maps {
            let (scene, stats, time) = build_timed(&wad, over_iwad.then_some(&iwad2), at, name);
            let map = wad.map(at).unwrap();
            let (bad, bsp_area, tri_area) = bsp_coverage(&map);
            eprintln!(
                "{rel:32} {name:6} {:>6.1} ms {:>7} triangles {:>4} sectors, {} not closed ({} from nodes), loops {:.4}, nodes {:.4} ({bad} sectors differ), {} warnings {:?}",
                time.as_secs_f64() * 1000.0,
                scene.triangle_count(),
                map.sectors.len(),
                stats.broken_sectors,
                stats.bsp_sectors,
                stats.triangle_area / stats.loop_area.max(1.0),
                tri_area / bsp_area.max(1.0),
                scene.info.warnings.len(),
                scene.info.warnings,
            );
            assert!(scene.info.warnings.len() <= 2, "{rel} {name}: {:?}", scene.info.warnings);
            assert!(
                !scene.info.warnings.iter().any(|w| w.contains("missing")),
                "{rel} {name}: {:?}",
                scene.info.warnings
            );
            assert!((stats.triangle_area - stats.loop_area).abs() <= 0.001 * stats.loop_area, "{rel} {name}");
            assert!((tri_area - bsp_area).abs() <= 0.01 * bsp_area, "{rel} {name}: {tri_area} vs {bsp_area}");
            assert!(scene.spawn.is_some(), "{rel} {name}");
            assert!(scene.markers.len() > 10, "{rel} {name}");
            if time > slowest.1 {
                slowest = (format!("{rel} {name}"), time);
            }
        }
    }
    eprintln!("slowest: {} in {:?}", slowest.0, slowest.1);
    // Debug builds here are optimised; a big map takes tens of
    // milliseconds in release.
    assert!(slowest.1.as_secs_f64() < 1.0, "{slowest:?}");
}

/// Player 1 starts at eye height above the floor of the sector it stands
/// in, facing its angle: Freedoom's MAP01 on the ground, MAP19 on a floor
/// 656 units up and facing west.
#[test]
fn spawn_at_eye_height() {
    let Some(bytes) = read_asset("doom/freedoom/freedoom2.wad") else { return };
    let wad = Wad::parse(&bytes).unwrap();
    for (name, (x, y, floor, angle)) in
        [("MAP01", (-192.0, -192.0, 0.0, 0.0)), ("MAP19", (608.0, -736.0, 656.0, 180.0))]
    {
        let scene = build_level(&wad, None, name, &Limits::viewer()).unwrap();
        let spawn = scene.spawn.unwrap();
        let want = to_world(x, y, floor + EYE_HEIGHT);
        assert!((spawn.position - want).length() < 1e-4, "{name}: {} vs {want}", spawn.position);
        assert!((spawn.yaw - (angle as f32 - 90.0).to_radians()).abs() < 1e-5, "{name}");
    }
    // The player is 56 units tall: about 1.7 m.
    let height = to_world(0.0, 0.0, 56.0).y;
    assert!((1.65..1.75).contains(&height), "{height}");
    assert_eq!(to_world(1.0, 2.0, 0.0), Vec3::new(0.0254, 0.0, -0.0508));
}

/// A PWAD that relies on the IWAD's textures and flats finds them in an
/// IWAD in a sibling folder, records it, and says where they came from.
#[test]
fn pwad_borrows_iwad_textures() {
    let (Some(pwad), Some(_)) = (asset("doom/2xreject/2xreject.wad"), asset("doom/freedoom/freedoom2.wad")) else {
        return;
    };
    let bytes = std::fs::read(&pwad).unwrap();
    assert_eq!(parts(&bytes), ["MAP01", "MAP02"]);
    let (scene, neighbours) =
        slerp_scene::collect_neighbours(|| load_part(&pwad, &bytes, &Limits::viewer(), "map02").unwrap());
    // Doom II where it is installed (Steam, GZDoom's folders), else
    // Freedoom: Phase 2 in the sibling folder.
    let from = scene.info.details.iter().find(|(k, _)| k == "Textures from").map(|(_, v)| v.clone()).unwrap();
    let lower = from.to_ascii_lowercase();
    assert!(lower.ends_with("doom2.wad") || lower.ends_with("freedoom2.wad"), "{from}");
    eprintln!("2xreject's textures from {from}");
    assert!(neighbours.iter().any(|p| p.to_string_lossy().eq_ignore_ascii_case(&from)), "{neighbours:?}");
    assert!(scene.info.warnings.is_empty(), "{:?}", scene.info.warnings);
    // Without neighbours, the PWAD's own textures only: one summary
    // warning, not one per surface.
    let alone = Limits { neighbours: false, ..Limits::viewer() };
    let scene = load_part(&pwad, &bytes, &alone, "MAP01").unwrap();
    let missing: Vec<&String> = scene.info.warnings.iter().filter(|w| w.contains("missing")).collect();
    assert_eq!(missing.len(), 1, "{:?}", scene.info.warnings);
    assert!(scene.triangle_count() > 1000);
}

/// A hand-made map: a 256-unit room with a raised step, a door-like upper
/// wall and a masked grate, checked surface by surface.
#[test]
fn walls_follow_doom_alignment() {
    let wad = room3d();
    let parsed = Wad::parse(&wad).unwrap();
    let scene = build_level(&parsed, None, "MAP01", &Limits::viewer()).unwrap();
    // Floors and ceilings of two sectors (one pair of triangles each, at
    // least), and walls: six one-sided, one lower step.
    let floors = scene.nodes.iter().find(|n| n.name == "Floors").unwrap();
    let floor_mesh = &scene.meshes[floors.mesh.unwrap()];
    let area: f32 = floor_mesh
        .primitives
        .iter()
        .flat_map(|p| p.indices.chunks_exact(3).map(move |t| (p, t)))
        .map(|(p, t)| {
            let [a, b, c] = [0, 1, 2].map(|k| p.positions[t[k] as usize]);
            (b - a).cross(c - a).length() * 0.5
        })
        .sum();
    let want = (256.0 * 0.0254f32).powi(2);
    assert!((area - want).abs() < 1e-3 * want, "{area} vs {want}");
    // Every floor triangle faces up.
    for p in &floor_mesh.primitives {
        for t in p.indices.chunks_exact(3) {
            let [a, b, c] = [0, 1, 2].map(|k| p.positions[t[k] as usize]);
            assert!((b - a).cross(c - a).y > 0.0);
        }
    }
    let walls = scene.nodes.iter().find(|n| n.name == "Walls").unwrap();
    let wall_mesh = &scene.meshes[walls.mesh.unwrap()];
    let quads: usize = wall_mesh.primitives.iter().map(|p| p.indices.len() / 6).sum();
    assert_eq!(quads, 7, "six one-sided walls and the step's lower texture");
    // The room's lights: 160 everywhere, and fake contrast on walls.
    let lights: std::collections::BTreeSet<u32> =
        wall_mesh.primitives.iter().flat_map(|p| p.colors.iter().map(|c| (c.x * 1000.0) as u32)).collect();
    assert_eq!(lights.len(), 2, "east-west walls darker than north-south ones: {lights:?}");
    assert_eq!(scene.kind, slerp_scene::SceneKind::Level);
    let spawn = scene.spawn.unwrap();
    assert!((spawn.position - to_world(64.0, 128.0, EYE_HEIGHT)).length() < 1e-5);
    assert!(spawn.yaw.abs() < 1e-6, "facing north is facing -Z");
    let labels: Vec<&str> = scene.markers.iter().map(|m| m.label.as_str()).collect();
    assert_eq!(labels, ["Player 1 start", "Imp"]);
    // The imp stands on the step.
    assert!(scene.markers[1].position.y > scene.markers[0].position.y);
}

/// The walls of a two-sided line: lower and upper textures at the height
/// differences, pegged to the right edges, and a masked middle texture
/// drawn once.
#[test]
fn texture_pegging() {
    use super::map::{LineDef, MapData, MapFormat, Sector, SideDef};
    let sector = |floor: f64, ceiling: f64| Sector {
        floor,
        ceiling,
        floor_flat: "FLAT".into(),
        ceiling_flat: "FLAT".into(),
        light: 255.0,
        ..Default::default()
    };
    let side = |sector: usize| SideDef {
        upper: "UP".into(),
        lower: "LOW".into(),
        middle: "-".into(),
        sector,
        y_offset: 0.0,
        ..Default::default()
    };
    // Two squares side by side: 0 (floor 0, ceiling 128) and 1 (floor 32,
    // ceiling 96), joined by line 0 from (64, 64) to (64, 0).
    let v = vec![[0.0, 0.0], [64.0, 0.0], [64.0, 64.0], [0.0, 64.0], [128.0, 0.0], [128.0, 64.0]];
    let line = |v1: usize, v2: usize, front: usize, back: Option<usize>, flags: u32| LineDef {
        v1,
        v2,
        flags,
        front: Some(front),
        back,
        alpha: 1.0,
        ..Default::default()
    };
    let mut sides = vec![side(0), side(1)];
    sides[1].middle = "GRATE".into();
    sides[0].middle = "GRATE".into();
    for _ in 0..6 {
        sides.push(SideDef { middle: "WALL".into(), ..side(0) });
    }
    for s in &mut sides[5..8] {
        s.sector = 1;
    }
    let map = MapData {
        format: MapFormat::Doom,
        vertices: v,
        vertex_heights: Default::default(),
        lines: vec![
            line(2, 1, 0, Some(1), super::map::ML_TWOSIDED),
            line(0, 1, 2, None, 0),
            line(3, 0, 3, None, 0),
            line(2, 3, 4, None, 0),
            line(1, 4, 5, None, 0),
            line(4, 5, 6, None, 0),
            line(5, 2, 7, None, 0),
        ],
        sides,
        sectors: vec![sector(0.0, 128.0), sector(32.0, 96.0)],
        things: Vec::new(),
        bsp: None,
    };
    let wad = build(b"PWAD", &[]);
    let wad = Wad::parse(&wad).unwrap();
    let res = Resources::new(vec![ResFile::Wad(&wad)]);
    let mut gfx = Graphics::new(&res);
    let (scene, _) = level::build(&map, "MAP01", &mut gfx, &Limits::viewer(), &level::Extras::default()).unwrap();
    // Textures are missing, so every wall wears one grey material; find
    // the quads by height instead.
    let walls = &scene.meshes[scene.nodes.iter().find(|n| n.name == "Walls").unwrap().mesh.unwrap()];
    let mut spans: Vec<(i32, i32, f32, f32)> = Vec::new();
    for p in &walls.primitives {
        for q in p.indices.chunks_exact(6) {
            // Only line 0's walls, at x = 64.
            if q.iter().any(|&i| (p.positions[i as usize].x - 64.0 * level::UNIT).abs() > 1e-4) {
                continue;
            }
            let ys: Vec<f32> = q.iter().map(|&i| p.positions[i as usize].y).collect();
            let lo = ys.iter().copied().fold(f32::MAX, f32::min);
            let hi = ys.iter().copied().fold(f32::MIN, f32::max);
            let unit = level::UNIT * level::STRETCH;
            let at_lo = q.iter().find(|&&i| p.positions[i as usize].y == lo).unwrap();
            let at_hi = q.iter().find(|&&i| p.positions[i as usize].y == hi).unwrap();
            spans.push((
                (lo / unit).round() as i32,
                (hi / unit).round() as i32,
                p.uv0[*at_lo as usize].y,
                p.uv0[*at_hi as usize].y,
            ));
        }
    }
    // Missing textures are 64 texels tall.  The lower wall (0-32) is
    // pegged to the higher floor: its top row at 32.  The upper wall
    // (96-128) hangs from the lower ceiling's top: v 0 at 96 + 64 = 160.
    assert!(spans.contains(&(0, 32, 0.5, 0.0)), "{spans:?}");
    assert!(spans.contains(&(96, 128, 1.0, 0.5)), "{spans:?}");
    // The masked grate, 64 tall from the lower ceiling (96) down to 32,
    // from both sides.
    assert_eq!(spans.iter().filter(|s| (s.0, s.1) == (32, 96)).count(), 2, "{spans:?}");
}

/// The viewer's limits without reading files beside the opened one, so
/// tests of damaged files don't each load an installed IWAD.
fn alone() -> Limits {
    Limits { neighbours: false, ..Limits::viewer() }
}

/// Mutated levels never panic, and truncated map lumps fail cleanly or
/// build what's left.
#[test]
fn corrupt_levels_never_panic() {
    let wad = room3d();
    for (i, m) in testutil::mutations(&wad).iter().enumerate() {
        let r = std::panic::catch_unwind(|| {
            let _ = parts(m);
            load_part(Path::new("/nonexistent/x.wad"), m, &alone(), "MAP01").map(|s| s.triangle_count())
        });
        assert!(r.is_ok(), "mutation {i} panicked");
    }
    let Some(bytes) = read_asset("doom/freedoom/freedoom2.wad") else { return };
    let real = Wad::parse(&bytes).unwrap();
    let (at, _) = real.maps()[0];
    // Rebuild MAP01 with each of its lumps cut short in turn.
    let lumps: Vec<(String, Vec<u8>)> =
        real.lumps[at..at + 11].iter().map(|l| (l.name.clone(), l.data.to_vec())).collect();
    for cut in 1..lumps.len() {
        for keep in [0.0, 0.37, 0.5, 0.93] {
            let mut ls = lumps.clone();
            let n = (ls[cut].1.len() as f64 * keep) as usize;
            ls[cut].1.truncate(n);
            let refs: Vec<(&str, Vec<u8>)> = ls.iter().map(|(n, d)| (n.as_str(), d.clone())).collect();
            let w = build(b"PWAD", &refs);
            let r = std::panic::catch_unwind(|| {
                load_part(Path::new("/nonexistent/x.wad"), &w, &alone(), "MAP01").map(|s| s.triangle_count())
            });
            assert!(r.is_ok(), "{} cut to {keep} panicked", lumps[cut].0);
        }
    }
    // Random vertex positions and sidedef sector numbers: tangled lines
    // still triangulate without panicking.
    let mut seed = 12345u64;
    let mut rand = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..20 {
        let mut ls = lumps.clone();
        let verts = ls.iter_mut().find(|(n, _)| n == "VERTEXES").unwrap();
        for _ in 0..40 {
            let at = (rand() as usize % (verts.1.len() / 2)) * 2;
            verts.1[at..at + 2].copy_from_slice(&((rand() % 4096) as i16 - 2048).to_le_bytes());
        }
        let refs: Vec<(&str, Vec<u8>)> = ls.iter().map(|(n, d)| (n.as_str(), d.clone())).collect();
        let w = build(b"PWAD", &refs);
        let r = std::panic::catch_unwind(|| {
            load_part(Path::new("/nonexistent/x.wad"), &w, &alone(), "MAP01").map(|s| s.triangle_count())
        });
        assert!(r.is_ok());
    }
}

#[test]
fn sky_names() {
    assert_eq!(level::sky_name("E3M2"), "SKY3");
    assert_eq!(level::sky_name("MAP11"), "SKY1");
    assert_eq!(level::sky_name("map12"), "SKY2");
    assert_eq!(level::sky_name("MAP30"), "SKY3");
    assert!(level::light_factor(255.0, 0) > 0.99);
    assert!(level::light_factor(0.0, 0) > 0.0 && level::light_factor(0.0, 0) < 0.02);
    assert!(level::light_factor(128.0, -1) < level::light_factor(128.0, 0));
    assert!(level::light_factor(128.0, 1) > level::light_factor(128.0, 0));
}

/// Sky ceilings wear the map's sky as a panorama: Freedoom's 256-wide
/// SKY1 repeats four times a turn, with its row 100 (below the fade added
/// on top) on the horizon.
#[test]
fn sky_is_a_panorama() {
    let Some(bytes) = read_asset("doom/freedoom/freedoom1.wad") else { return };
    let wad = Wad::parse(&bytes).unwrap();
    let (at, name) = wad.maps()[0];
    let (scene, _, _) = build_timed(&wad, None, at, name);
    let sky = scene.materials.iter().find(|m| m.effect == slerp_scene::Effect::PanoramaSky).expect("a sky");
    assert!(sky.effect.is_sky() && sky.unlit);
    let r = sky.base_color_texture.expect("the sky's picture");
    let t = &scene.textures[r.texture];
    let (w, h) = (t.levels[0].width as f32, t.levels[0].height as f32);
    let tr = r.transform.expect("placed on the cylinder");
    assert!((tr.scale.x * w - 1024.0).abs() < 1e-3, "{} columns a turn", tr.scale.x * w);
    let horizon = tr.offset.y * h;
    assert!((horizon - (100.0 + 48.0)).abs() < 1e-3, "horizon at texel row {horizon}");
    assert_eq!(r.sampler.wrap_v, slerp_scene::Wrap::ClampToEdge);
}

/// Things show as sprites that face the camera: a "Things" node of
/// alpha-masked billboards, one quad per thing, standing on the floor.
#[test]
fn things_are_sprites() {
    let Some(bytes) = read_asset("doom/freedoom/freedoom1.wad") else { return };
    let wad = Wad::parse(&bytes).unwrap();
    let (at, name) = wad.maps()[0];
    let (scene, _, _) = build_timed(&wad, None, at, name);
    let node = scene.nodes.iter().find(|n| n.name == "Things").expect("a Things node");
    let mesh = &scene.meshes[node.mesh.unwrap()];
    let quads: usize = mesh.primitives.iter().map(|p| p.indices.len() / 6).sum();
    assert!(quads > 20, "{quads} sprites");
    for p in &mesh.primitives {
        let m = &scene.materials[p.material.unwrap()];
        assert_eq!(m.effect, slerp_scene::Effect::Billboard);
        assert!(matches!(m.alpha_mode, slerp_scene::AlphaMode::Mask(_)) && m.base_color_texture.is_some());
        assert_eq!((p.uv1.len(), p.colors.len()), (p.positions.len(), p.positions.len()));
        // Corners lie around the anchor, within a few metres.
        assert!(p.uv1.iter().all(|o| o.y.abs() < 5.0 && o.x.abs() < 5.0));
        assert!(p.uv1.iter().any(|o| o.y > 0.0));
    }
    // Monsters and pillars stand a metre or more tall (pools of blood
    // don't).
    let tallest = mesh.primitives.iter().flat_map(|p| &p.uv1).map(|o| o.y).fold(0.0f32, f32::max);
    assert!(tallest > 1.0, "tallest sprite {tallest} m");
}

//endregion

#[test]
#[ignore]
fn debug_sector_areas() {
    let file = std::env::var("DOOM_FILE").unwrap_or("doom/freedoom/freedoom1.wad".into());
    let name = std::env::var("DOOM_MAP").unwrap_or("E1M4".into());
    let bytes = read_asset(&file).unwrap();
    let wad = Wad::parse(&bytes).unwrap();
    let (at, _) = wad.maps().into_iter().find(|(_, n)| *n == name).unwrap();
    let map = wad.map(at).unwrap();
    let areas = polygon::sector_areas(&map);
    let bsp = map.bsp.as_ref().unwrap();
    let mut from_bsp = vec![0.0f64; map.sectors.len()];
    for (s, poly) in polygon::subsector_polygons(&map, bsp) {
        let mut a = 0.0;
        for i in 0..poly.len() {
            let (p, q) = (poly[i], poly[(i + 1) % poly.len()]);
            a += p[0] * q[1] - q[0] * p[1];
        }
        from_bsp[s] += a * 0.5;
    }
    for (s, a) in areas.iter().enumerate() {
        let t = a.triangle_area();
        if (t - a.loop_area).abs() > 1.0 || (t - from_bsp[s]).abs() > 0.01 * t + 1.0 {
            eprintln!(
                "sector {s}: loops {} triangles {t} bsp {} broken {} points {}",
                a.loop_area,
                from_bsp[s],
                a.broken,
                a.points.len()
            );
            for l in &map.lines {
                if map.sector_of(l.front) == Some(s) || map.sector_of(l.back) == Some(s) {
                    let (p, q) = map.ends(l).unwrap();
                    eprintln!(
                        "   {:?} -> {:?} front {:?} back {:?}",
                        p,
                        q,
                        map.sector_of(l.front),
                        map.sector_of(l.back)
                    );
                }
            }
        }
    }
}

/// The same two-sector room in Hexen's binary format and in UDMF builds
/// the same level as in Doom's format.
#[test]
fn hexen_and_udmf_levels() {
    let doom_bytes = room3d();
    let doom_wad = Wad::parse(&doom_bytes).unwrap();
    let doom = build_level(&doom_wad, None, "MAP01", &Limits::viewer()).unwrap();

    // Hexen: 16-byte linedefs (special byte and five arguments where Doom
    // keeps special and tag), 20-byte things with a height, and BEHAVIOR.
    let lump = |n: &str| doom_wad.lumps.iter().find(|l| l.name == n).unwrap().data.to_vec();
    let mut linedefs = Vec::new();
    for l in lump("LINEDEFS").chunks_exact(14) {
        linedefs.extend_from_slice(&l[0..6]);
        linedefs.extend_from_slice(&[0; 6]);
        linedefs.extend_from_slice(&l[10..14]);
    }
    let mut things = Vec::new();
    for t in lump("THINGS").chunks_exact(10) {
        things.extend_from_slice(&[0, 0]);
        things.extend_from_slice(&t[0..4]);
        things.extend_from_slice(&[0, 0]);
        things.extend_from_slice(&t[4..10]);
        things.extend_from_slice(&[0; 6]);
    }
    let hexen = build(
        b"PWAD",
        &[
            ("MAP01", vec![]),
            ("THINGS", things),
            ("LINEDEFS", linedefs),
            ("SIDEDEFS", lump("SIDEDEFS")),
            ("VERTEXES", lump("VERTEXES")),
            ("SECTORS", lump("SECTORS")),
            ("BEHAVIOR", vec![0; 16]),
        ],
    );
    let hexen = build_level(&Wad::parse(&hexen).unwrap(), None, "MAP01", &Limits::viewer()).unwrap();
    assert_eq!(hexen.info.format, "Doom map (Hexen format)");

    // UDMF, written out by hand.
    let text = br#"namespace = "zdoom";
        vertex { x = 0; y = 0; } vertex { x = 256; y = 0; } vertex { x = 256; y = 256; }
        vertex { x = 0; y = 256; } vertex { x = 128; y = 0; } vertex { x = 128; y = 256; }
        sector { heightfloor = 0; heightceiling = 128; texturefloor = "FLOOR"; textureceiling = "CEIL"; lightlevel = 160; }
        sector { heightfloor = 24; heightceiling = 128; texturefloor = "FLOOR"; textureceiling = "CEIL"; lightlevel = 160; }
        sidedef { sector = 0; texturemiddle = "WALL"; } sidedef { sector = 0; texturemiddle = "WALL"; }
        sidedef { sector = 0; texturemiddle = "WALL"; } sidedef { sector = 1; texturemiddle = "WALL"; }
        sidedef { sector = 1; texturemiddle = "WALL"; } sidedef { sector = 1; texturemiddle = "WALL"; }
        sidedef { sector = 0; texturebottom = "STEP"; } sidedef { sector = 1; }
        linedef { v1 = 0; v2 = 3; sidefront = 0; blocking = true; }
        linedef { v1 = 3; v2 = 5; sidefront = 1; }
        linedef { v1 = 4; v2 = 0; sidefront = 2; }
        linedef { v1 = 5; v2 = 2; sidefront = 3; }
        linedef { v1 = 2; v2 = 1; sidefront = 4; }
        linedef { v1 = 1; v2 = 4; sidefront = 5; }
        linedef { v1 = 5; v2 = 4; sidefront = 6; sideback = 7; twosided = true; }
        thing { x = 64; y = 128; angle = 90; type = 1; skill1 = true; single = true; }
        thing { x = 192; y = 128; angle = 180; type = 3001; skill1 = true; single = true; }"#;
    let udmf = build(b"PWAD", &[("MAP01", vec![]), ("TEXTMAP", text.to_vec()), ("ENDMAP", vec![])]);
    let udmf = build_level(&Wad::parse(&udmf).unwrap(), None, "MAP01", &Limits::viewer()).unwrap();
    assert_eq!(udmf.info.format, "Doom map (UDMF, namespace zdoom)");

    for (name, level) in [("Hexen", &hexen), ("UDMF", &udmf)] {
        assert_eq!(level.triangle_count(), doom.triangle_count(), "{name}");
        assert_eq!(level.vertex_count(), doom.vertex_count(), "{name}");
        assert!((level.spawn.unwrap().position - doom.spawn.unwrap().position).length() < 1e-6, "{name}");
        assert_eq!(level.markers.len(), 2, "{name}");
        let b = level.bounds(&slerp_scene::Pose::rest(level));
        let d = doom.bounds(&slerp_scene::Pose::rest(&doom));
        assert!((b.min - d.min).length() < 1e-5 && (b.max - d.max).length() < 1e-5, "{name}");
    }
}

/// WADs built to exhaust memory or time stay bounded: 300 textures of
/// 4096x4096, each stacking 100 copies of a 4096x4096 patch (19 GB and
/// 480 billion texel copies if taken literally), and a sector with 3000
/// holes.
#[test]
fn hostile_levels_stay_bounded() {
    // 1. A patch that decodes to 4096x4096 from 21 KB: every column points
    //    at one shared run of posts.
    let side = 4096usize;
    let mut patch = Vec::new();
    patch.extend_from_slice(&(side as u16).to_le_bytes());
    patch.extend_from_slice(&(side as u16).to_le_bytes());
    patch.extend_from_slice(&[0; 4]);
    let column_at = (8 + side * 4) as u32;
    for _ in 0..side {
        patch.extend_from_slice(&column_at.to_le_bytes());
    }
    // Posts of 128 rows; past row 254 each top is relative to the last.
    for k in 0..32 {
        patch.extend_from_slice(&[if k < 2 { k as u8 * 128 } else { 128 }, 128, 0]);
        patch.extend_from_slice(&[7; 128]);
        patch.push(0);
    }
    patch.push(255);
    let mut pnames = 1u32.to_le_bytes().to_vec();
    pnames.extend_from_slice(b"BIG\0\0\0\0\0");
    let count = 300usize;
    let mut texture1 = (count as u32).to_le_bytes().to_vec();
    let record = 22 + 100 * 10;
    for i in 0..count {
        texture1.extend_from_slice(&((4 + count * 4 + i * record) as u32).to_le_bytes());
    }
    for i in 0..count {
        let mut name = [0u8; 8];
        name[..format!("T{i}").len()].copy_from_slice(format!("T{i}").as_bytes());
        texture1.extend_from_slice(&name);
        texture1.extend_from_slice(&[0; 4]);
        texture1.extend_from_slice(&(side as u16).to_le_bytes());
        texture1.extend_from_slice(&(side as u16).to_le_bytes());
        texture1.extend_from_slice(&[0; 4]);
        texture1.extend_from_slice(&100u16.to_le_bytes());
        for _ in 0..100 {
            texture1.extend_from_slice(&[0; 4]);
            texture1.extend_from_slice(&0u16.to_le_bytes());
            texture1.extend_from_slice(&[1, 0, 0, 0]);
        }
    }
    // A room of 300 walls, each wearing its own texture.
    let le = |v: &[i16]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let mut vertexes = Vec::new();
    for i in 0..count {
        let a = i as f64 / count as f64 * std::f64::consts::TAU;
        vertexes.extend(le(&[(a.cos() * 2000.0) as i16, (a.sin() * 2000.0) as i16]));
    }
    let mut linedefs = Vec::new();
    let mut sidedefs = Vec::new();
    for i in 0..count {
        linedefs.extend(le(&[((i + 1) % count) as i16, i as i16, 1, 0, 0, i as i16, -1]));
        let mut s = le(&[0, 0]);
        s.extend_from_slice(&[b'-', 0, 0, 0, 0, 0, 0, 0, b'-', 0, 0, 0, 0, 0, 0, 0]);
        let mut name = [0u8; 8];
        name[..format!("T{i}").len()].copy_from_slice(format!("T{i}").as_bytes());
        s.extend_from_slice(&name);
        s.extend(le(&[0]));
        sidedefs.extend(s);
    }
    let mut sector = le(&[0, 128]);
    sector.extend_from_slice(b"FLAT\0\0\0\0FLAT\0\0\0\0");
    sector.extend(le(&[160, 0, 0]));
    let wad = build(
        b"PWAD",
        &[
            ("PNAMES", pnames),
            ("TEXTURE1", texture1),
            ("P_START", vec![]),
            ("BIG", patch),
            ("P_END", vec![]),
            ("MAP01", vec![]),
            ("THINGS", vec![]),
            ("LINEDEFS", linedefs),
            ("SIDEDEFS", sidedefs),
            ("VERTEXES", vertexes),
            ("SECTORS", sector),
        ],
    );
    let t = std::time::Instant::now();
    let scene = build_level(&Wad::parse(&wad).unwrap(), None, "MAP01", &Limits::viewer()).unwrap();
    let texels: usize = scene.textures.iter().map(|t| t.width() as usize * t.height() as usize).sum();
    eprintln!(
        "huge textures: {:?}, {} textures, {texels} texels, {:?}",
        t.elapsed(),
        scene.textures.len(),
        scene.info.warnings
    );
    assert!(texels <= 64 << 20);
    assert!(scene.info.warnings.iter().any(|w| w.contains("memory limits")));
    assert!(t.elapsed().as_secs_f64() < 20.0);

    // 2. One sector with square pillars of another sector: 900 holes
    //    (3,604 points) are triangulated exactly; 3,025 (12,104 points) are
    //    past the cap ear clipping is allowed, so the sector is left to the
    //    BSP at once rather than clipped for seconds.
    for n in [30usize, 55] {
        let mut verts: Vec<i16> = vec![0, 0, 6000, 0, 6000, 6000, 0, 6000];
        let mut lines: Vec<[i16; 7]> =
            vec![[0, 3, 1, 0, 0, 0, -1], [3, 2, 1, 0, 0, 0, -1], [2, 1, 1, 0, 0, 0, -1], [1, 0, 1, 0, 0, 0, -1]];
        for gx in 0..n {
            for gy in 0..n {
                let (x, y) = (50 + gx as i16 * 108, 50 + gy as i16 * 108);
                let b = (verts.len() / 2) as i16;
                verts.extend_from_slice(&[x, y, x + 50, y, x + 50, y + 50, x, y + 50]);
                for k in 0..4 {
                    lines.push([b + k, b + (k + 1) % 4, 4, 0, 0, 2, 1]);
                }
            }
        }
        let mut sides = Vec::new();
        for sector in [0i16, 0, 1] {
            let mut s = le(&[0, 0]);
            s.extend_from_slice(&[b'-', 0, 0, 0, 0, 0, 0, 0, b'-', 0, 0, 0, 0, 0, 0, 0, b'-', 0, 0, 0, 0, 0, 0, 0]);
            s.extend(le(&[sector]));
            sides.extend(s);
        }
        let mut sectors = Vec::new();
        for floor in [0i16, 32] {
            let mut s = le(&[floor, 128]);
            s.extend_from_slice(b"FLAT\0\0\0\0FLAT\0\0\0\0");
            s.extend(le(&[160, 0, 0]));
            sectors.extend(s);
        }
        let wad = build(
            b"PWAD",
            &[
                ("MAP01", vec![]),
                ("THINGS", vec![]),
                ("LINEDEFS", lines.iter().flat_map(|l| le(l)).collect()),
                ("SIDEDEFS", sides),
                ("VERTEXES", le(&verts)),
                ("SECTORS", sectors),
            ],
        );
        let t = std::time::Instant::now();
        let wad = Wad::parse(&wad).unwrap();
        let map = wad.map(wad.maps()[0].0).unwrap();
        let areas = polygon::sector_areas(&map);
        let want = 6000.0 * 6000.0 - (n * n) as f64 * 2500.0;
        eprintln!("{} holes: {:?}, area {} of {want}", n * n, t.elapsed(), areas[0].triangle_area());
        if n * n * 4 + 4 <= 4096 {
            assert!((areas[0].triangle_area() - want).abs() < 1e-3 * want);
        } else {
            assert!(areas[0].broken && areas[0].triangles.is_empty());
        }
        assert!(t.elapsed().as_secs_f64() < 5.0);
    }
}

/// `load_part` for every map of both IWADs and 2xreject (over Freedoom
/// found in the sibling folder): the median of 5 runs, WAD directory and
/// IWAD search included.  Run with `cargo test --release -- --ignored
/// --nocapture bench_levels`.
#[test]
#[ignore]
fn bench_levels() {
    for rel in ["doom/freedoom/freedoom1.wad", "doom/freedoom/freedoom2.wad", "doom/2xreject/2xreject.wad"] {
        let Some(path) = asset(rel) else { continue };
        let bytes = std::fs::read(&path).unwrap();
        let mut times = Vec::new();
        for name in parts(&bytes) {
            let mut runs: Vec<f64> = (0..5)
                .map(|_| {
                    let t = std::time::Instant::now();
                    load_part(&path, &bytes, &Limits::viewer(), &name).unwrap();
                    t.elapsed().as_secs_f64() * 1000.0
                })
                .collect();
            runs.sort_by(f64::total_cmp);
            times.push((runs[2], name));
        }
        times.sort_by(|a, b| a.0.total_cmp(&b.0));
        let median = times[times.len() / 2].0;
        let (slowest, name) = times.last().unwrap();
        eprintln!("{rel:32} {} maps, median {median:.1} ms, slowest {name} {slowest:.1} ms", times.len());
    }
}

/*┌─────────────────╮
| GZDoom archives |
└─────────────────╯*/
//region GZDoom archives

/// Every map of the Elementalism megawad (a 585 MB PK3 of UDMF maps with
/// slopes, 3D floors and fog, over Doom II where it is installed, else
/// Freedoom: Phase 2) builds without panics or floods of warnings.  Run with `--nocapture`
/// for the table; with `--release` the times are the real ones.
#[test]
fn elementalism_maps_build() {
    let Some(path) = asset("doom-extra/elementalism/Elementalism_Phase1_RC2_v1.2.pk3") else { return };
    let t = std::time::Instant::now();
    let names = archive_parts(Source::open(&path).unwrap());
    eprintln!("parts in {:?}: {names:?}", t.elapsed());
    assert!(names.len() >= 17, "{names:?}");
    assert_eq!(&names[..3], ["HUB", "TITLEMAP", "E1"], "MAPINFO's order");
    let mut slowest = (String::new(), std::time::Duration::ZERO);
    for name in &names {
        let t = std::time::Instant::now();
        let scene = load_archive_part(&path, Source::open(&path).unwrap(), &Limits::viewer(), name)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let time = t.elapsed();
        let detail = |k: &str| scene.info.details.iter().find(|(n, _)| n == k).map_or("0", |(_, v)| v.as_str());
        eprintln!(
            "{name:9} {:>7.0} ms {:>8} triangles {:>5} materials, slopes {}, 3D floors {}, fogged {}, warnings {:?}",
            time.as_secs_f64() * 1000.0,
            scene.triangle_count(),
            scene.materials.len(),
            detail("Sloped planes"),
            detail("3D floors"),
            detail("Sectors with coloured fog"),
            scene.info.warnings
        );
        assert!(scene.info.warnings.len() <= 2, "{name}: {:?}", scene.info.warnings);
        assert!(scene.spawn.is_some() || name == "TITLEMAP", "{name}");
        let from = scene.info.details.iter().find(|(k, _)| k == "Textures from").map(|(_, v)| v.to_ascii_lowercase());
        assert!(from.as_deref().is_some_and(|f| f.ends_with("doom2.wad")), "{name}: {from:?}");
        if name == "E1" {
            eprintln!("Elementalism's textures from {}", from.unwrap_or_default());
        }
        if time > slowest.1 {
            slowest = (name.clone(), time);
        }
    }
    eprintln!("slowest: {} in {:?}", slowest.0, slowest.1);
}

//endregion

/// Prints facts about one map of a WAD or PK3, for debugging:
/// `DOOM_FILE=... DOOM_MAP=E1 cargo test -p slerp-quake debug_level -- --ignored --nocapture`.
#[test]
#[ignore]
fn debug_level() {
    let file = std::env::var("DOOM_FILE").unwrap_or("doom-extra/elementalism/Elementalism_Phase1_RC2_v1.2.pk3".into());
    let name = std::env::var("DOOM_MAP").unwrap_or("E1".into());
    let path = asset(&file).unwrap();
    let scene = match path.extension().and_then(|e| e.to_str()) {
        Some("wad") => load_part(&path, &std::fs::read(&path).unwrap(), &Limits::viewer(), &name).unwrap(),
        _ => load_archive_part(&path, Source::open(&path).unwrap(), &Limits::viewer(), &name).unwrap(),
    };
    for m in &scene.materials {
        if m.effect.is_sky() || std::env::var("ALL").is_ok() {
            eprintln!("{} {:?} {:?}", m.name, m.base_color_texture, m.fog);
        }
        if m.effect.is_sky()
            && let Some(t) = m.base_color_texture
        {
            let tex = &scene.textures[t.texture];
            if let Pixels::Rgba8(px) = tex.base() {
                testutil::write_png(Path::new("/tmp/slerp-quake/sky.png"), tex.width(), tex.height(), px);
            }
        }
    }
    eprintln!("{:#?}", scene.info.details);
    // RAY="x y eyez angle pitch ndcx ndcy" names what the camera sees at a
    // screen point (vertical field of view 73.74 degrees, square picture).
    if let Ok(ray) = std::env::var("RAY") {
        let v: Vec<f32> = ray.split_whitespace().filter_map(|t| t.parse().ok()).collect();
        let origin = to_world(v[0] as f64, v[1] as f64, v[2] as f64);
        let (yaw, pitch) = ((v[3] - 90.0).to_radians(), (-v[4]).to_radians());
        let fwd = Vec3::new(-yaw.sin() * pitch.cos(), pitch.sin(), -yaw.cos() * pitch.cos());
        let right = Vec3::new(yaw.cos(), 0.0, -yaw.sin());
        let up = right.cross(fwd);
        let dir = (fwd + right * v[5] * 0.75 + up * v[6] * 0.75).normalize();
        let mut best: Option<(f32, String, String, Vec3)> = None;
        for node in &scene.nodes {
            let Some(mi) = node.mesh else { continue };
            for p in &scene.meshes[mi].primitives {
                for t in p.indices.chunks_exact(3) {
                    let [a, b, c] = [0, 1, 2].map(|k| p.positions[t[k] as usize]);
                    let (e1, e2) = (b - a, c - a);
                    let h = dir.cross(e2);
                    let det = e1.dot(h);
                    if det.abs() < 1e-9 {
                        continue;
                    }
                    let f = 1.0 / det;
                    let s0 = origin - a;
                    let u = f * s0.dot(h);
                    let q = s0.cross(e1);
                    let w = f * dir.dot(q);
                    let d = f * e2.dot(q);
                    if u < 0.0 || w < 0.0 || u + w > 1.0 || d <= 1e-4 {
                        continue;
                    }
                    if best.as_ref().is_none_or(|b| d < b.0) {
                        let m = p.material.map(|m| scene.materials[m].name.clone()).unwrap_or_default();
                        best = Some((d, node.name.clone(), m, origin + dir * d));
                    }
                }
            }
        }
        if let Some((d, node, m, at)) = best {
            let doom = (at.x / level::UNIT, -at.z / level::UNIT, at.y / (level::UNIT * level::STRETCH));
            eprintln!("ray hits {node} / {m} at {d:.1} m, Doom {doom:?}");
        }
    }
}

/*┌─────────────────╮
| Archives, slopes|
| and 3D floors   |
└─────────────────╯*/
//region Archives, slopes and 3D floors

use super::archive::Archive;

/// A 64x64 picture of one colour, as PNG bytes.
fn png(rgb: [u8; 3]) -> Vec<u8> {
    testutil::png_bytes(64, 64, &[rgb[0], rgb[1], rgb[2], 255].repeat(64 * 64))
}

/// The zip reader: stored and deflated entries, folders left out, data
/// before the archive, and damaged archives that fail without panicking.
#[test]
fn pk3_archive_reader() {
    let wall = png([200, 0, 0]);
    let text = b"map MAP01 \"Test\" { sky1 = \"SKY1\" }".to_vec();
    let z = testutil::zip(&[
        ("MAPINFO.txt", &text, false),
        ("textures/walls/Brick.png", &wall, true),
        ("flats/", b"", false),
        ("flats/floor.raw.png", &wall, false),
    ]);
    let a = Archive::open(Source::Memory(&z)).unwrap();
    let paths: Vec<&str> = a.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["MAPINFO.txt", "textures/walls/Brick.png", "flats/floor.raw.png"]);
    assert_eq!(a.read(0, 1 << 20).unwrap(), text);
    assert_eq!(a.read(1, 1 << 20).unwrap(), wall);
    assert!(matches!(a.read(1, 10), Err(LoadError::TooLarge(_))));
    assert_eq!(a.read_many(&[2, 0], 1 << 20), [Some(wall.clone()), Some(text.clone())]);
    assert_eq!(archive::classify("textures/walls/Brick.png"), (Namespace::Textures, "BRICK".into()));
    assert_eq!(archive::classify("flats/floor.raw.png"), (Namespace::Flats, "FLOOR.RA".into()));
    assert_eq!(archive::classify("MAPINFO.txt"), (Namespace::Global, "MAPINFO".into()));
    assert_eq!(archive::classify("models/x/skin.png").0, Namespace::Other);
    assert!(archive_is_doom(paths.iter().copied()));
    assert!(!archive_is_doom(["3D/3dmodel.model", "[Content_Types].xml"]));

    // Not a zip: not ours.
    assert!(matches!(Archive::open(Source::Memory(b"MZ\0\0 not a zip")), Err(LoadError::NotMine)));

    // Damage never panics; truncation fails cleanly.
    for (i, m) in testutil::mutations(&z).iter().enumerate() {
        let r = std::panic::catch_unwind(|| {
            if let Ok(a) = Archive::open(Source::Memory(m)) {
                for k in 0..a.entries.len() {
                    let _ = a.read(k, 1 << 20);
                }
            }
        });
        assert!(r.is_ok(), "mutation {i} panicked");
    }
    for cut in 0..z.len() {
        let r = std::panic::catch_unwind(|| Archive::open(Source::Memory(&z[..cut])).map(|a| a.entries.len()));
        assert!(r.is_ok(), "cut {cut} panicked");
    }
}

/// PK7 (7z) archives read the same way, each solid block decoded once.
#[test]
fn pk7_archive_reader() {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter};
    let wall = png([0, 200, 0]);
    let mut w = ArchiveWriter::new(std::io::Cursor::new(Vec::new())).unwrap();
    let entries = vec![ArchiveEntry::new_file("TEXTURES"), ArchiveEntry::new_file("patches/wallp.png")];
    let readers: Vec<sevenz_rust2::SourceReader<std::io::Cursor<Vec<u8>>>> = vec![
        std::io::Cursor::new(b"Texture \"W\", 64, 64 { Patch \"WALLP\", 0, 0 }".to_vec()).into(),
        std::io::Cursor::new(wall.clone()).into(),
    ];
    w.push_archive_entries(entries, readers).unwrap();
    let bytes = w.finish().unwrap().into_inner();
    assert!(is_7z(&bytes));
    let a = Archive::open(Source::Memory(&bytes)).unwrap();
    assert!(a.is_solid());
    let got = a.read_many(&[1, 0], 1 << 20);
    assert_eq!(got[0].as_deref(), Some(&wall[..]));
    assert!(got[1].as_ref().is_some_and(|t| t.starts_with(b"Texture")));
    for cut in (0..bytes.len()).step_by(7) {
        let r = std::panic::catch_unwind(|| {
            if let Ok(a) = Archive::open(Source::Memory(&bytes[..cut])) {
                let _ = a.read_many(&[0, 1], 1 << 20);
            }
        });
        assert!(r.is_ok(), "cut {cut} panicked");
    }
}

/// A PK3 holding one UDMF map with GZDoom's features, its textures, flats
/// and MAPINFO: room A (sector 0) with fog, a 3D floor and a panned,
/// rotated, scaled flat; room B (sector 1) east of it with a floor sloping
/// up from 32 at x = 256 to 96 at x = 512; and a control sector far away.
fn gzdoom_pk3() -> Vec<u8> {
    let textmap = br#"namespace = "zdoom";
        vertex { x = 0; y = 0; } vertex { x = 256; y = 0; } vertex { x = 256; y = 256; } vertex { x = 0; y = 256; }
        vertex { x = 512; y = 0; } vertex { x = 512; y = 256; }
        vertex { x = 1000; y = 0; } vertex { x = 1064; y = 0; } vertex { x = 1064; y = 64; } vertex { x = 1000; y = 64; }
        sector { heightfloor = 0; heightceiling = 256; texturefloor = "FLOOR"; textureceiling = "F_SKY1"; lightlevel = 160;
                 id = 5; fadecolor = 0x204060; fogdensity = 100; xpanningfloor = 16; rotationfloor = 90; xscalefloor = 2; }
        sector { heightfloor = 32; heightceiling = 256; texturefloor = "FLOOR"; textureceiling = "FLOOR"; lightlevel = 200;
                 floorplane_a = -0.25; floorplane_b = 0; floorplane_c = 1; floorplane_d = 32; lightcolor = 0xff8000; }
        sector { heightfloor = 64; heightceiling = 96; texturefloor = "FLOOR"; textureceiling = "FLOOR"; lightlevel = 100; }
        sidedef { sector = 0; texturemiddle = "BRICK"; } sidedef { sector = 0; texturemiddle = "BRICK"; }
        sidedef { sector = 0; texturebottom = "BRICK"; } sidedef { sector = 1; }
        sidedef { sector = 0; texturemiddle = "BRICK"; }
        sidedef { sector = 1; texturemiddle = "BRICK"; scalex_mid = 2.0; } sidedef { sector = 1; texturemiddle = "BRICK"; }
        sidedef { sector = 1; texturemiddle = "BRICK"; }
        sidedef { sector = 2; texturemiddle = "BRICK"; } sidedef { sector = 2; texturemiddle = "BRICK"; }
        sidedef { sector = 2; texturemiddle = "BRICK"; } sidedef { sector = 2; texturemiddle = "BRICK"; }
        linedef { v1 = 0; v2 = 3; sidefront = 0; }
        linedef { v1 = 3; v2 = 2; sidefront = 1; }
        linedef { v1 = 2; v2 = 1; sidefront = 2; sideback = 3; twosided = true; }
        linedef { v1 = 1; v2 = 0; sidefront = 4; }
        linedef { v1 = 2; v2 = 5; sidefront = 5; }
        linedef { v1 = 5; v2 = 4; sidefront = 6; }
        linedef { v1 = 4; v2 = 1; sidefront = 7; }
        linedef { v1 = 6; v2 = 9; sidefront = 8; special = 160; arg0 = 5; arg1 = 1; arg3 = 255; }
        linedef { v1 = 9; v2 = 8; sidefront = 9; }
        linedef { v1 = 8; v2 = 7; sidefront = 10; }
        linedef { v1 = 7; v2 = 6; sidefront = 11; }
        thing { x = 64; y = 128; angle = 0; type = 1; skill2 = true; skill3 = true; single = true; }"#;
    let wad = build(b"PWAD", &[("MAP01", vec![]), ("TEXTMAP", textmap.to_vec()), ("ENDMAP", vec![])]);
    let textures = b"Texture \"BRICK\", 64, 64 { XScale 2.0 YScale 2.0 Patch \"BRICKP\", 0, 0 }".to_vec();
    let mapinfo = b"map MAP01 \"Test Rooms\" { lightmode = 0 }".to_vec();
    let (brick, floor) = (png([180, 60, 40]), png([90, 90, 90]));
    testutil::zip(&[
        ("TEXTURES.txt", &textures, false),
        ("ZMAPINFO.txt", &mapinfo, true),
        ("patches/brickp.png", &brick, true),
        ("flats/floor.png", &floor, false),
        ("maps/MAP01.wad", &wad, true),
    ])
}

/// Every vertex of the primitives of node `name` using material `m` (all
/// materials when `None`).
fn node_vertices(scene: &Scene, name: &str) -> Vec<(Vec3, slerp_scene::glam::Vec2, Option<usize>)> {
    let Some(node) = scene.nodes.iter().find(|n| n.name == name) else { return Vec::new() };
    let mesh = &scene.meshes[node.mesh.unwrap()];
    mesh.primitives
        .iter()
        .flat_map(|p| p.positions.iter().zip(&p.uv0).map(move |(&q, &uv)| (q, uv, p.material)))
        .collect()
}

#[test]
fn gzdoom_features_in_a_pk3() {
    let pk3 = gzdoom_pk3();
    let names = archive_parts(Source::Memory(&pk3));
    assert_eq!(names, ["MAP01"]);
    let alone = Limits { neighbours: false, ..Limits::viewer() };
    let scene = load_archive_part(Path::new("/nonexistent/test.pk3"), Source::Memory(&pk3), &alone, "MAP01").unwrap();
    assert!(scene.info.warnings.is_empty(), "{:?}", scene.info.warnings);
    let detail = |k: &str| scene.info.details.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    assert_eq!(detail("Title").as_deref(), Some("Test Rooms"));
    assert_eq!(detail("Sloped planes").as_deref(), Some("1"));
    assert_eq!(detail("3D floors").as_deref(), Some("1"));
    let z = |units: f64| to_world(0.0, 0.0, units).y;

    // 1. TEXTURES: BRICK is 64 texels at XScale 2, so 32 units wide.  Room
    //    A's north wall (256 units) repeats it 8 times; room B's (256 to
    //    512, side scale 2) 16 times.
    let brick = scene.materials.iter().position(|m| m.name == "BRICK").expect("BRICK");
    let tex = &scene.textures[scene.materials[brick].base_color_texture.unwrap().texture];
    assert_eq!((tex.width(), tex.height()), (64, 64));
    let is_brick = |m: &Option<usize>| m.is_some_and(|m| scene.materials[m].name == "BRICK");
    let walls = node_vertices(&scene, "Walls");
    let span = |pred: &dyn Fn(Vec3) -> bool| {
        let us: Vec<f32> = walls.iter().filter(|(p, _, m)| pred(*p) && is_brick(m)).map(|(_, uv, _)| uv.x).collect();
        us.iter().copied().fold(f32::MIN, f32::max) - us.iter().copied().fold(f32::MAX, f32::min)
    };
    let north = -256.0 * level::UNIT;
    let a_span = span(&|p| (p.z - north).abs() < 1e-4 && p.x < 256.0 * level::UNIT - 1e-4);
    let b_span = span(&|p| (p.z - north).abs() < 1e-4 && p.x > 256.0 * level::UNIT + 1e-4);
    assert!((a_span - 8.0).abs() < 1e-3, "{a_span}");
    assert!((b_span - 16.0).abs() < 1e-3, "{b_span}");

    // 2. The slope: room B's floor runs from 32 to 96, and its north wall
    //    is cut to it at each end.
    let floors = node_vertices(&scene, "Floors");
    let b_floor: Vec<Vec3> = floors.iter().map(|v| v.0).filter(|p| p.x > 256.0 * level::UNIT + 1e-4).collect();
    let at_east =
        b_floor.iter().filter(|p| (p.x - 512.0 * level::UNIT).abs() < 1e-4).map(|p| p.y).fold(f32::MIN, f32::max);
    assert!((at_east - z(96.0)).abs() < 1e-4, "{at_east}");
    let wall_low = |x: f64| {
        walls
            .iter()
            .filter(|(p, _, _)| (p.z - north).abs() < 1e-4 && (p.x - x as f32 * level::UNIT).abs() < 1e-4)
            .map(|v| v.0.y)
            .fold(f32::MAX, f32::min)
    };
    // Room A's north wall reaches the floor (0) at x = 256; room B's
    // starts at its floor's 32 there and ends at 96.
    assert!(wall_low(256.0).abs() < 1e-4);
    let at_256 = |y: f32| {
        walls.iter().any(|(p, _, _)| {
            (p.z - north).abs() < 1e-4 && (p.x - 256.0 * level::UNIT).abs() < 1e-4 && (p.y - y).abs() < 1e-4
        })
    };
    assert!(at_256(z(32.0)), "room B's wall cut to its floor");
    assert!((wall_low(512.0) - z(96.0)).abs() < 1e-4);
    // The step between the rooms: a lower wall from 0 to 32 at x = 256,
    // facing west into room A.
    let node = scene.nodes.iter().find(|n| n.name == "Walls").unwrap();
    let step = scene.meshes[node.mesh.unwrap()].primitives.iter().any(|p| {
        p.indices.chunks_exact(3).any(|t| {
            let v = [t[0], t[1], t[2]].map(|i| p.positions[i as usize]);
            let n = p.normals[t[0] as usize];
            v.iter().all(|q| (q.x - 256.0 * level::UNIT).abs() < 1e-4 && q.y < z(32.0) + 1e-4)
                && v.iter().any(|q| (q.y - z(32.0)).abs() < 1e-4)
                && n.x < -0.99
        })
    });
    assert!(step, "the step's lower wall");

    // 3. The 3D floor: a slab from 64 to 96 over room A, top facing up,
    //    with sides; its control sector, out in the void, is hidden.
    let slab = node_vertices(&scene, "3D floors");
    assert!(slab.iter().any(|v| (v.0.y - z(96.0)).abs() < 1e-4) && slab.iter().any(|v| (v.0.y - z(64.0)).abs() < 1e-4));
    assert!(slab.iter().all(|v| v.0.x <= 256.0 * level::UNIT + 1e-4), "over room A only");
    assert!(!walls.iter().any(|v| (v.0.y - z(96.0)).abs() < 1e-4 && v.0.x > 900.0 * level::UNIT));
    let control = scene.nodes.iter().find(|n| n.name == "Unseen areas").expect("unseen node");
    assert!(!control.visible);

    // 4. Fog and light: room A's surfaces fade into its fog (density 100
    //    halves to 50 in GZDoom's units), room B's take its orange light.
    let fogged =
        scene.materials.iter().filter_map(|m| m.fog).find(|f| f.density > 0.0 && f.color.z > f.color.x).expect("fog");
    let want = 50.0 / 64000.0 / 0.0254;
    assert!((fogged.density - want).abs() < 1e-6, "{} vs {want}", fogged.density);
    let colors: Vec<Vec4> =
        scene.meshes.iter().flat_map(|m| &m.primitives).flat_map(|p| p.colors.iter().copied()).collect();
    assert!(colors.iter().any(|c| c.x > c.y * 2.0 && c.z < 0.01), "orange light");
    assert!(scene.materials.iter().filter(|m| !m.effect.is_sky()).all(|m| m.fog.is_some()), "Doom shading");
}

use slerp_scene::glam::Vec4;

/// GZDoom's flat placement: x/64 and -y/64 rotated, scaled to the
/// texture, panned, then scaled by the plane's scale.
#[test]
fn flat_transforms() {
    use super::map::FlatTransform;
    let t = FlatTransform { x_pan: 16.0, y_pan: 0.0, x_scale: 2.0, y_scale: 1.0, rotation: 90.0 };
    let uv = level::flat_uv([0.0, 0.0], &t, (64.0, 64.0));
    assert!((uv.x - 0.5).abs() < 1e-6 && uv.y.abs() < 1e-6, "{uv}");
    // Rotated a quarter turn: moving east moves v.
    let uv = level::flat_uv([64.0, 0.0], &t, (64.0, 64.0));
    assert!((uv.x - 0.5).abs() < 1e-5 && (uv.y + 1.0).abs() < 1e-5, "{uv}");
    let plain = FlatTransform::default();
    let uv = level::flat_uv([32.0, 64.0], &plain, (64.0, 64.0));
    assert_eq!(uv, slerp_scene::glam::Vec2::new(0.5, -1.0));
}

/// PK3s are recognised by their content: a zip holding Doom maps or
/// lumps is Doom's, whatever its name, and other zips (a 3MF model) are
/// not; without the file to look into, the extension decides.
#[test]
fn pk3s_sniff_by_content() {
    let dir = std::env::temp_dir().join(format!("slerp-pk3-sniff-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pk3 = gzdoom_pk3();
    let named_zip = dir.join("mod.zip");
    std::fs::write(&named_zip, &pk3).unwrap();
    assert_eq!(crate::sniff(&named_zip, &pk3[..256]), Some(crate::QuakeFormat::DoomArchive));
    let model = testutil::zip(&[("3D/3dmodel.model", b"<model/>", false), ("[Content_Types].xml", b"<x/>", false)]);
    let named_pk3 = dir.join("not-doom.pk3");
    std::fs::write(&named_pk3, &model).unwrap();
    assert_eq!(crate::sniff(&named_pk3, &model[..64]), None, "a zip without Doom content");
    assert_eq!(crate::sniff(Path::new("/nonexistent/x.pk3"), &pk3[..64]), Some(crate::QuakeFormat::DoomArchive));
    assert_eq!(crate::sniff(Path::new("/nonexistent/x.zip"), &pk3[..64]), None);
    assert_eq!(crate::sniff(Path::new("a.sbsar"), b"7z\xBC\xAF\x27\x1C\0\x03"), None, "Substance's");
    // Through the path-reading entry points, as core calls them.
    assert_eq!(crate::parts_path(crate::QuakeFormat::DoomArchive, &named_zip), ["MAP01"]);
    let alone = Limits { neighbours: false, ..Limits::viewer() };
    let level = crate::load_part_path(crate::QuakeFormat::DoomArchive, &named_zip, &alone, "MAP01").unwrap();
    assert!(matches!(level, slerp_scene::Asset::Scene(_)));
    let gallery = crate::load_path(crate::QuakeFormat::DoomArchive, &named_zip, &Limits::thumbnail()).unwrap();
    assert!(matches!(gallery, slerp_scene::Asset::Gallery(ref g) if g[0].name == "MAP01"));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Damaged GZDoom archives never panic, whichever lump the damage hits.
#[test]
fn corrupt_pk3s_never_panic() {
    let pk3 = gzdoom_pk3();
    for (i, m) in testutil::mutations(&pk3).iter().enumerate() {
        let r = std::panic::catch_unwind(|| {
            let _ = archive_parts(Source::Memory(m));
            let _ = load_archive(Source::Memory(m), &Limits::viewer());
            let alone = Limits { neighbours: false, ..Limits::viewer() };
            load_archive_part(Path::new("/nonexistent/x.pk3"), Source::Memory(m), &alone, "MAP01")
                .map(|s| s.triangle_count())
        });
        assert!(r.is_ok(), "mutation {i} panicked");
    }
}

//endregion
