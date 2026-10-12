use super::*;
use crate::miptex::tests::build;
use crate::testutil::{asset, external};
use crate::wad::tests::build_wad;
use slerp_scene::Aabb;

fn describe(s: &Scene) -> String {
    format!(
        "{} '{}': {} nodes, {} tris, {} materials, {} textures, {} markers, spawn {:?}, warnings {:?}, details {:?}",
        s.info.format,
        s.name,
        s.nodes.len(),
        s.triangle_count(),
        s.materials.len(),
        s.textures.len(),
        s.markers.len(),
        s.spawn.map(|p| p.position),
        s.info.warnings,
        s.info.details
    )
}

fn load_file(p: &Path, limits: &Limits) -> Scene {
    let bytes = std::fs::read(p).unwrap();
    let (r, neighbours) = slerp_scene::collect_neighbours(|| load(p, &bytes, limits));
    let mut s = r.unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    s.info.neighbours = neighbours;
    s
}

/// A scratch folder for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("slerp-editor-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The bounds of everything drawn except skies.
fn drawn_bounds(s: &Scene) -> Aabb {
    let mut b = Aabb::EMPTY;
    for m in &s.meshes {
        for p in &m.primitives {
            if p.material.and_then(|i| s.materials.get(i)).is_some_and(|m| m.effect.is_sky()) {
                continue;
            }
            for &v in &p.positions {
                b.add(v);
            }
        }
    }
    b
}

/// A box room from `lo` to `hi` (inside) with walls `t` thick, as brush
/// text with the given texture per face.
fn room(lo: [i32; 3], hi: [i32; 3], t: i32, tex: &str) -> String {
    let mut out = String::new();
    let b = |a: [i32; 3], c: [i32; 3], out: &mut String| {
        let f = |p: [[i32; 3]; 3]| {
            format!(
                "( {} {} {} ) ( {} {} {} ) ( {} {} {} ) {tex} 0 0 0 1 1\n",
                p[0][0], p[0][1], p[0][2], p[1][0], p[1][1], p[1][2], p[2][0], p[2][1], p[2][2]
            )
        };
        out.push_str("{\n");
        out.push_str(&f([[a[0], a[1], a[2]], [a[0], a[1] + 1, a[2]], [a[0], a[1], a[2] + 1]]));
        out.push_str(&f([[a[0], a[1], a[2]], [a[0], a[1], a[2] + 1], [a[0] + 1, a[1], a[2]]]));
        out.push_str(&f([[a[0], a[1], a[2]], [a[0] + 1, a[1], a[2]], [a[0], a[1] + 1, a[2]]]));
        out.push_str(&f([[c[0], c[1], c[2]], [c[0], c[1] + 1, c[2]], [c[0] + 1, c[1], c[2]]]));
        out.push_str(&f([[c[0], c[1], c[2]], [c[0] + 1, c[1], c[2]], [c[0], c[1], c[2] + 1]]));
        out.push_str(&f([[c[0], c[1], c[2]], [c[0], c[1], c[2] + 1], [c[0], c[1] + 1, c[2]]]));
        out.push_str("}\n");
    };
    let (l, h) = (lo, hi);
    b([l[0] - t, l[1] - t, l[2] - t], [h[0] + t, h[1] + t, l[2]], &mut out);
    b([l[0] - t, l[1] - t, h[2]], [h[0] + t, h[1] + t, h[2] + t], &mut out);
    b([l[0] - t, l[1] - t, l[2]], [l[0], h[1] + t, h[2]], &mut out);
    b([h[0], l[1] - t, l[2]], [h[0] + t, h[1] + t, h[2]], &mut out);
    b([l[0], l[1] - t, l[2]], [h[0], l[1], h[2]], &mut out);
    b([l[0], h[1], l[2]], [h[0], h[1] + t, h[2]], &mut out);
    out
}

/*┌─────────────────╮
| Synthetic maps  |
└─────────────────╯*/
//region Synthetic maps

#[test]
fn quake_map_with_wad_tools_sky_and_liquid() {
    let dir = scratch("quake");
    let wad = build_wad(
        b"WAD2",
        &[
            ("wall", b'D', build("wall", 64, 32, |x, _| (x % 16) as u8 + 16, None)),
            ("sky4", b'D', build("sky4", 256, 128, |_, _| 40, None)),
            ("*water1", b'D', build("*water1", 64, 64, |_, _| 50, None)),
        ],
    );
    std::fs::write(dir.join("test.wad"), wad).unwrap();
    let mut text = String::from(
        "// Game: Quake\n// Format: Standard\n{\n\"classname\" \"worldspawn\"\n\"wad\" \"C:\\quake\\id1\\test.wad\"\n\"_wateralpha\" \"0.4\"\n",
    );
    text += &room([-128, -128, 0], [128, 128, 128], 16, "wall");
    // A sky brush above, a water pool, and a clip brush.
    text += &room([-64, -64, 200], [64, 64, 210], 8, "sky4").lines().take(8).collect::<Vec<_>>().join("\n");
    text += "\n";
    text += &room([-32, -32, 0], [32, 32, 16], 4, "*water1").lines().take(8).collect::<Vec<_>>().join("\n");
    text += "\n";
    text += &room([0, 0, 0], [8, 8, 8], 2, "clip").lines().take(8).collect::<Vec<_>>().join("\n");
    text += "\n}\n{\n\"classname\" \"func_door\"\n\"targetname\" \"d1\"\n";
    text += &room([90, -10, 0], [100, 10, 64], 2, "wall").lines().take(8).collect::<Vec<_>>().join("\n");
    text += "\n}\n{\n\"classname\" \"trigger_once\"\n";
    text += &room([0, 0, 0], [8, 8, 8], 2, "wall").lines().take(8).collect::<Vec<_>>().join("\n");
    text += "\n}\n{\n\"classname\" \"info_player_start\"\n\"origin\" \"0 0 24\"\n\"angle\" \"90\"\n}\n{\n\"classname\" \"light\"\n\"origin\" \"0 0 100\"\n}\n";
    let path = dir.join("test.map");
    std::fs::write(&path, &text).unwrap();
    assert_eq!(crate::sniff(&path, text.as_bytes()), Some(crate::QuakeFormat::Map));

    let s = load_file(&path, &Limits::viewer());
    eprintln!("{}", describe(&s));
    assert_eq!(s.info.format, "Quake map (standard)");
    assert!(s.info.warnings.is_empty(), "{:?}", s.info.warnings);
    assert_eq!(s.kind, SceneKind::Level);
    let names: Vec<(&str, bool)> = s.nodes.iter().map(|n| (n.name.as_str(), n.visible)).collect();
    assert_eq!(
        names,
        [("world", true), ("outside the level", false), ("func_door d1", true)],
        "the trigger is left out"
    );
    // Six room brushes of six faces, a sky brush, a water brush, no clip;
    // the room is sealed, so the faces towards the void are hidden.
    let tris =
        |n: usize| -> usize { s.meshes[s.nodes[n].mesh.unwrap()].primitives.iter().map(|p| p.triangle_count()).sum() };
    assert_eq!(tris(0) + tris(1), (6 * 6 + 6 + 6) * 2);
    assert!(tris(1) >= 18 * 2, "the room's outer faces: {}", tris(1));
    let world = &s.meshes[s.nodes[0].mesh.unwrap()];
    let mat = |name: &str| s.materials.iter().find(|m| m.name == name).unwrap_or_else(|| panic!("{name}"));
    assert_eq!(mat("sky4").effect, Effect::Sky);
    let water = mat("*water1");
    assert_eq!((water.effect, water.alpha_mode, water.base_color.w), (Effect::Warp, AlphaMode::Blend, 0.4));
    assert!(!mat("wall").unlit);
    // Wall texture coordinates: 64 texels wide.
    let p = world.primitives.iter().find(|p| p.material.is_some_and(|m| s.materials[m].name == "wall")).unwrap();
    let span =
        p.uv0.iter().map(|uv| uv.x).fold(f32::MIN, f32::max) - p.uv0.iter().map(|uv| uv.x).fold(f32::MAX, f32::min);
    assert!((span - 288.0 / 64.0).abs() < 1e-4, "{span}");
    assert!(s.spawn.is_some());
    assert_eq!(s.markers.len(), 2);
    assert!(s.info.neighbours.iter().any(|n| n.ends_with("test.wad")), "{:?}", s.info.neighbours);

    // Without the WAD, every texture is a checker, named in one warning.
    std::fs::remove_file(dir.join("test.wad")).unwrap();
    let s = load_file(&path, &Limits::thumbnail());
    assert_eq!(s.info.warnings.len(), 2, "{:?}", s.info.warnings);
    assert!(s.info.warnings[1].starts_with("3 textures not found"), "{:?}", s.info.warnings);
    assert_eq!(s.materials.iter().filter(|m| m.effect == Effect::PanoramaSky).count(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn half_life_valve_map_with_wad3_and_layers() {
    let dir = scratch("hl");
    let pal = [200u8; 768];
    let wad = build_wad(
        b"WAD3",
        &[
            ("hlwall", 0x43, build("hlwall", 32, 32, |_, _| 1, Some(&pal))),
            ("{grate", 0x43, build("{grate", 32, 32, |x, _| if x < 16 { 1 } else { 255 }, Some(&pal))),
            ("!water", 0x43, build("!water", 32, 32, |_, _| 1, Some(&pal))),
        ],
    );
    std::fs::create_dir_all(dir.join("valve")).unwrap();
    std::fs::create_dir_all(dir.join("valve/maps")).unwrap();
    std::fs::write(dir.join("valve/custom.wad"), wad).unwrap();
    let face = |p: [[i32; 3]; 3], tex: &str, u: [i32; 3], v: [i32; 3]| {
        format!(
            "( {} {} {} ) ( {} {} {} ) ( {} {} {} ) {tex} [ {} {} {} 0 ] [ {} {} {} 0 ] 0 1 1\n",
            p[0][0],
            p[0][1],
            p[0][2],
            p[1][0],
            p[1][1],
            p[1][2],
            p[2][0],
            p[2][1],
            p[2][2],
            u[0],
            u[1],
            u[2],
            v[0],
            v[1],
            v[2]
        )
    };
    let cube = |lo: i32, hi: i32, tex: &str| {
        let mut s = String::from("{\n");
        s += &face([[lo, lo, lo], [lo, lo + 1, lo], [lo, lo, lo + 1]], tex, [0, -1, 0], [0, 0, -1]);
        s += &face([[lo, lo, lo], [lo, lo, lo + 1], [lo + 1, lo, lo]], tex, [1, 0, 0], [0, 0, -1]);
        s += &face([[lo, lo, lo], [lo + 1, lo, lo], [lo, lo + 1, lo]], tex, [-1, 0, 0], [0, -1, 0]);
        s += &face([[hi, hi, hi], [hi, hi + 1, hi], [hi + 1, hi, hi]], tex, [1, 0, 0], [0, -1, 0]);
        s += &face([[hi, hi, hi], [hi + 1, hi, hi], [hi, hi, hi + 1]], tex, [-1, 0, 0], [0, 0, -1]);
        s += &face([[hi, hi, hi], [hi, hi, hi + 1], [hi, hi + 1, hi]], tex, [0, 1, 0], [0, 0, -1]);
        s + "}\n"
    };
    let mut text = String::from(
        "// Game: Half-Life\n// Format: Valve\n{\n\"mapversion\" \"220\"\n\"classname\" \"worldspawn\"\n\"wad\" \"\\half-life\\valve\\custom.wad\"\n",
    );
    text += &cube(0, 64, "hlwall");
    text += &cube(100, 132, "sky");
    text += "}\n{\n\"classname\" \"func_group\"\n\"_tb_type\" \"_tb_layer\"\n\"_tb_name\" \"Details\"\n\"_tb_id\" \"2\"\n\"_tb_layer_hidden\" \"1\"\n";
    text += &cube(200, 232, "{grate");
    text += "}\n{\n\"classname\" \"func_water\"\n\"_tb_layer\" \"2\"\n";
    text += &cube(300, 332, "!water");
    text += "}\n{\n\"classname\" \"info_player_start\"\n\"origin\" \"32 32 100\"\n\"angles\" \"0 180 0\"\n}\n{\n\"classname\" \"light\"\n\"_tb_layer\" \"2\"\n\"origin\" \"1 1 1\"\n}\n";
    let path = dir.join("valve/maps/test.map");
    std::fs::write(&path, &text).unwrap();
    let s = load_file(&path, &Limits::viewer());
    eprintln!("{}", describe(&s));
    assert_eq!(s.info.format, "Half-Life map (Valve 220)");
    assert!(s.info.warnings.is_empty(), "{:?}", s.info.warnings);
    let names: Vec<(&str, bool)> = s.nodes.iter().map(|n| (n.name.as_str(), n.visible)).collect();
    assert_eq!(names, [("world", true), ("layer Details", false), ("func_water #2", false)]);
    let mat = |name: &str| s.materials.iter().find(|m| m.name == name).unwrap_or_else(|| panic!("{name}"));
    assert_eq!(mat("{grate").alpha_mode, AlphaMode::Mask(0.5));
    assert_eq!(mat("!water").effect, Effect::Warp);
    assert_eq!(mat("sky").effect, Effect::PanoramaSky, "Half-Life skies are sky boxes");
    // The light in the hidden layer has no marker; the start faces west.
    assert_eq!(s.markers.len(), 1);
    let spawn = s.spawn.unwrap();
    assert!((spawn.yaw - 90f32.to_radians()).abs() < 1e-5);
    assert!((spawn.position.y - (100.0 + 28.0) * crate::UNIT).abs() < 1e-5);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn quake3_map_with_patches_and_brush_primitives() {
    let bp = |a: &str, tex: &str| format!("{a} ( ( 0.0078125 0 0 ) ( 0 0.0078125 0 ) ) {tex} 0 0 0\n");
    let mut text = String::from("// entity 0\n{\n\"classname\" \"worldspawn\"\n// brush 0\n{\nbrushDef\n{\n");
    text += &bp("( 0 0 0 ) ( 0 1 0 ) ( 0 0 1 )", "base/wall");
    text += &bp("( 0 0 0 ) ( 0 0 1 ) ( 1 0 0 )", "base/wall");
    text += &bp("( 0 0 0 ) ( 1 0 0 ) ( 0 1 0 )", "common/caulk");
    text += &bp("( 64 64 64 ) ( 64 65 64 ) ( 65 64 64 )", "base/floor");
    text += &bp("( 64 64 64 ) ( 65 64 64 ) ( 64 64 65 )", "base/wall");
    text += &bp("( 64 64 64 ) ( 64 64 65 ) ( 64 65 64 )", "base/wall");
    text += "}\n}\n{\npatchDef2\n{\nbase/curve\n( 3 3 0 0 0 )\n(\n( ( 0 0 64 0 0 ) ( 0 32 96 0 0.5 ) ( 0 64 64 0 1 ) )\n( ( 32 0 64 0.5 0 ) ( 32 32 96 0.5 0.5 ) ( 32 64 64 0.5 1 ) )\n( ( 64 0 64 1 0 ) ( 64 32 96 1 0.5 ) ( 64 64 64 1 1 ) )\n)\n}\n}\n}\n{\n\"classname\" \"info_player_deathmatch\"\n\"origin\" \"32 32 100\"\n}\n";
    let path = Path::new("/nonexistent/q3.map");
    let s = load(path, text.as_bytes(), &Limits::viewer()).unwrap();
    eprintln!("{}", describe(&s));
    assert_eq!(s.info.format, "Quake 3 map (brush primitives)");
    // Five brush faces (caulk hidden) and a 5x5-vertex patch.
    assert_eq!(s.triangle_count(), 5 * 2 + 4 * 4 * 2);
    let curve = s.materials.iter().find(|m| m.name == "base/curve").unwrap();
    assert!(curve.double_sided);
    assert!(s.spawn.is_some());
}

#[test]
fn damaged_maps_never_panic() {
    let mut text = String::from("{\n\"classname\" \"worldspawn\"\n");
    text += &room([-64, -64, 0], [64, 64, 64], 8, "wall");
    text += "}\n{\n\"classname\" \"info_player_start\"\n\"origin\" \"0 0 24\"\n}\n";
    let bytes = text.as_bytes();
    let mut cases = crate::testutil::mutations(bytes);
    for cut in (0..bytes.len()).step_by(7) {
        cases.push(bytes[..cut].to_vec());
    }
    // Absurd numbers: a plane far outside the world, a huge brush.
    cases.push(text.replace("( -72 -72 -8 )", "( -72 -72 -8e300 )").into_bytes());
    cases.push(text.replace("-72", "-4000000").into_bytes());
    for (i, case) in cases.iter().enumerate() {
        let r = std::panic::catch_unwind(|| load(Path::new("/nonexistent/x.map"), case, &Limits::thumbnail()));
        assert!(r.is_ok(), "case {i} panicked");
    }
}

//endregion

/*┌─────────────────╮
| Real maps       |
└─────────────────╯*/
//region Real maps

/// Every downloaded map loads, with its load time.
#[test]
fn real_maps_load() {
    let mut paths: Vec<PathBuf> = Vec::new();
    for rel in [
        "quake/avix_qcastle/avix_qcastle.map",
        "quake/signati/signati.map",
        "quake/librequake-mapsrc/misc/start.map",
        "quake/librequake-mapsrc/dm/lqdm1.map",
        "quake/librequake-mapsrc/e1/lq_e1m1.map",
        "quake/librequake-mapsrc/brushmodels/b_batt0.map",
        "quake/ericw-testmaps/hl_basic.map",
        "quake/ericw-testmaps/hl_currents.map",
        "quake/ericw-testmaps/hl_liquids.map",
        "quake/ericw-testmaps/q1_mountain.map",
        "quake/ericw-testmaps/qbspfeatures.map",
        "quake/ericw-testmaps/q2_lightmap_custom_scale.map",
        "quake/deepmind-lab-maps/big_screen.map",
        "quake/deepmind-lab-maps/nav_maze_static_01.map",
        "quake/deepmind-lab-maps/em_watermaze.map",
        "halflife/tb_hl.map",
    ] {
        paths.extend(asset(rel));
    }
    for p in [crate::testutil::chalk!("local/calib/hall.map"), crate::testutil::chalk!("local/near-halls/debug/t.map")]
    {
        paths.extend(external(p));
    }
    for p in paths {
        for (label, limits) in [("thumbnail", Limits::thumbnail()), ("viewer", Limits::viewer())] {
            let t = std::time::Instant::now();
            let s = load_file(&p, &limits);
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            eprintln!("{label} {ms:7.1} ms  {}: {}", p.display(), describe(&s));
            assert!(s.triangle_count() > 0);
        }
    }
}

//endregion

/*┌─────────────────╮
| Against BSPs    |
└─────────────────╯*/
//region Against BSPs

/// The summed area (square metres) of everything drawn except skies and
/// liquids, which compilers treat differently from solid faces.
fn solid_area(s: &Scene, visible_only: bool) -> f64 {
    let mut area = 0.0;
    for n in &s.nodes {
        let (Some(mi), true) = (n.mesh, n.visible || !visible_only) else { continue };
        for p in &s.meshes[mi].primitives {
            if p.material.and_then(|i| s.materials.get(i)).is_some_and(|m| m.effect != Effect::None) {
                continue;
            }
            for t in p.indices.chunks_exact(3) {
                let [a, b, c] = [0, 1, 2].map(|k| p.positions[t[k] as usize].as_dvec3());
                area += (b - a).cross(c - a).length() * 0.5;
            }
        }
    }
    area
}

fn detail(s: &Scene, name: &str) -> usize {
    s.info.details.iter().find(|(k, _)| k == name).and_then(|(_, v)| v.parse().ok()).unwrap_or(0)
}

/// A map source against the BSP compiled from it: the compiler removes
/// faces nobody can see (outside the level, inside other brushes) and
/// splits the rest, so the BSP's drawn bounds lie within the map's, its
/// drawn area is at most the map's, and its face count is of the same
/// order.  LibreQuake's WADs are found through the relative paths its maps
/// give (`../../../../texture-wads/`), laid out here as in its repository.
#[test]
fn maps_agree_with_their_compiled_bsps() {
    let pairs = [
        ("quake/librequake-mapsrc/misc/start.map", "quake/librequake/id1/pak0/maps/start.bsp"),
        ("quake/librequake-mapsrc/e1/lq_e1m1.map", "quake/librequake/id1/pak0/maps/lq_e1m1.bsp"),
        ("quake/librequake-mapsrc/e0/lq_e0m1.map", "quake/librequake/id1/pak0/maps/lq_e0m1.bsp"),
        ("quake/librequake-mapsrc/brushmodels/b_batt0.map", "quake/librequake/id1/pak0/maps/b_batt0.bsp"),
        ("quake/librequake-mapsrc/dm/lqdm1.map", "quake/librequake/id1/maps/lqdm1.bsp"),
        ("quake/librequake-mapsrc/dm/lqdm5.map", "quake/librequake/id1/maps/lqdm5.bsp"),
        ("quake/librequake-mapsrc/dm/lqdm9.map", "quake/librequake/id1/maps/lqdm9.bsp"),
        ("quake/signati/signati.map", "quake/signati/signati.bsp"),
        ("quake/avix_qcastle/avix_qcastle.map", "quake/avix_qcastle/avix_qcastle.bsp"),
        ("halflife/tb_hl.map", "halflife/tb_hl.bsp"),
    ];
    let wads = asset("quake/librequake-texwads");
    let dir = scratch("lq");
    if let Some(w) = &wads {
        std::os::unix::fs::symlink(w, dir.join("texture-wads")).unwrap();
    }
    let mut compared = 0;
    for (map_rel, bsp_rel) in pairs {
        let (Some(map_path), Some(bsp_path)) = (asset(map_rel), asset(bsp_rel)) else { continue };
        // LibreQuake's sources go where their WAD paths expect them.
        let map_path = if map_rel.contains("librequake-mapsrc") && wads.is_some() {
            let sub = map_rel.trim_start_matches("quake/librequake-mapsrc/");
            let at = dir.join("lq1/maps/src").join(sub);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            let _ = std::os::unix::fs::symlink(&map_path, &at);
            at
        } else {
            map_path
        };
        let map = load_file(&map_path, &Limits::viewer());
        let bsp = crate::bsp::load(&bsp_path, &std::fs::read(&bsp_path).unwrap(), &Limits::viewer()).unwrap();
        let (mb, bb) = (drawn_bounds(&map), drawn_bounds(&bsp));
        let (ma, ba, shown) = (solid_area(&map, false), solid_area(&bsp, false), solid_area(&map, true));
        let (mf, bf) = (detail(&map, "Faces"), detail(&bsp, "Faces"));
        let found = map.info.details.iter().find(|(k, _)| k == "Textures").map(|(_, v)| v.clone()).unwrap_or_default();
        eprintln!(
            "{map_rel}: textures {found}; map {:?}..{:?} {ma:.0} m2 ({shown:.0} m2 shown) {mf} faces; bsp {:?}..{:?} {ba:.0} m2 {bf} faces",
            mb.min, mb.max, bb.min, bb.max
        );
        // The BSP's bounds are inside the map's (to a centimetre).
        assert!(
            (bb.min - mb.min).min_element() > -0.01 && (mb.max - bb.max).min_element() > -0.01,
            "{map_rel}: bounds"
        );
        assert!(ba <= ma * 1.01, "{map_rel}: area {ba} > {ma}");
        assert!(ba > ma * 0.15, "{map_rel}: area {ba} much less than {ma}");
        assert!(bf as f64 > mf as f64 * 0.2 && (bf as f64) < mf as f64 * 5.0, "{map_rel}: faces {bf} against {mf}");
        if map_rel.contains("librequake-mapsrc") && wads.is_some() {
            // All but a stray texture or two come from LibreQuake's WADs.
            let (total, got) = found.split_once(" (").map_or((0, 0), |(t, f)| {
                (t.parse::<usize>().unwrap_or(0), f.trim_end_matches(" found)").parse::<usize>().unwrap_or(0))
            });
            assert!(got as f64 >= total as f64 * 0.95, "{map_rel}: {found} {:?}", map.info.warnings);
        }
        compared += 1;
    }
    eprintln!("compared {compared} maps");
    let _ = std::fs::remove_dir_all(dir);
}

//endregion

/// Draws a few maps from above with the software rasteriser into
/// `/tmp/slerp-editor/`, for checking textures and coordinates by eye.
#[test]
#[ignore]
fn debug_pictures() {
    for rel in [
        "quake/avix_qcastle/avix_qcastle.map",
        "quake/signati/signati.map",
        "quake/ericw-testmaps/hl_currents.map",
        "quake/ericw-testmaps/qbspfeatures.map",
        "quake/deepmind-lab-maps/big_screen.map",
    ] {
        let Some(p) = asset(rel) else { continue };
        let mut s = load_file(&p, &Limits::thumbnail());
        for m in &mut s.meshes {
            m.primitives.retain(|p| !p.material.is_some_and(|i| s.materials[i].effect.is_sky()));
        }
        let size = 768;
        let px = crate::testutil::render(&s, Vec3::new(-0.6, -1.0, -0.8), size, true);
        let name = p.file_stem().unwrap().to_string_lossy().to_string();
        crate::testutil::write_png(Path::new(&format!("/tmp/slerp-editor/{name}.png")), size, size, &px);
    }
}
