use super::*;
use crate::testutil::asset;
use slerp_scene::textures::Pixels;

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

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("slerp-vmf-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A box solid in VMF text, every side with `material`, the top one with
/// `top` and an optional displacement.
fn solid(id: u32, lo: [i32; 3], hi: [i32; 3], material: &str, top: &str, disp: &str) -> String {
    let side = |plane: String, m: &str, u: &str, v: &str, extra: &str| {
        format!(
            "\t\tside\n\t\t{{\n\t\t\t\"plane\" \"{plane}\"\n\t\t\t\"material\" \"{m}\"\n\t\t\t\"uaxis\" \"[{u} 0] 0.25\"\n\t\t\t\"vaxis\" \"[{v} 0] 0.25\"\n{extra}\t\t}}\n"
        )
    };
    let (a, b) = (lo, hi);
    let p = |x: [i32; 3], y: [i32; 3], z: [i32; 3]| {
        format!("({} {} {}) ({} {} {}) ({} {} {})", x[0], x[1], x[2], y[0], y[1], y[2], z[0], z[1], z[2])
    };
    let mut s = format!("\tsolid\n\t{{\n\t\t\"id\" \"{id}\"\n");
    // Top (+Z) first, wound as Hammer writes it.
    s += &side(p([a[0], b[1], b[2]], [b[0], b[1], b[2]], [b[0], a[1], b[2]]), top, "1 0 0", "0 -1 0", disp);
    s += &side(p([a[0], a[1], a[2]], [b[0], a[1], a[2]], [b[0], b[1], a[2]]), material, "1 0 0", "0 -1 0", "");
    s += &side(p([a[0], b[1], b[2]], [a[0], a[1], b[2]], [a[0], a[1], a[2]]), material, "0 1 0", "0 0 -1", "");
    s += &side(p([b[0], b[1], a[2]], [b[0], a[1], a[2]], [b[0], a[1], b[2]]), material, "0 1 0", "0 0 -1", "");
    s += &side(p([b[0], b[1], b[2]], [a[0], b[1], b[2]], [a[0], b[1], a[2]]), material, "1 0 0", "0 0 -1", "");
    s += &side(p([b[0], a[1], a[2]], [a[0], a[1], a[2]], [a[0], a[1], b[2]]), material, "1 0 0", "0 0 -1", "");
    s + "\t}\n"
}

/// A power-2 displacement with a bump in the middle and a blend towards
/// one corner.
fn dispinfo(start: [i32; 3]) -> String {
    let rows = |f: &dyn Fn(usize, usize) -> String| {
        (0..5)
            .map(|r| format!("\t\t\t\t\t\"row{r}\" \"{}\"\n", (0..5).map(|c| f(r, c)).collect::<Vec<_>>().join(" ")))
            .collect::<String>()
    };
    format!(
        "\t\t\tdispinfo\n\t\t\t{{\n\t\t\t\t\"power\" \"2\"\n\t\t\t\t\"startposition\" \"[{} {} {}]\"\n\t\t\t\t\"elevation\" \"0\"\n\t\t\t\tnormals\n\t\t\t\t{{\n{}\t\t\t\t}}\n\t\t\t\tdistances\n\t\t\t\t{{\n{}\t\t\t\t}}\n\t\t\t\toffsets\n\t\t\t\t{{\n{}\t\t\t\t}}\n\t\t\t\talphas\n\t\t\t\t{{\n{}\t\t\t\t}}\n\t\t\t}}\n",
        start[0],
        start[1],
        start[2],
        rows(&|_, _| "0 0 1".into()),
        rows(&|r, c| if r == 2 && c == 2 { "64".into() } else { "0".into() }),
        rows(&|_, _| "0 0 0".into()),
        rows(&|r, c| ((r + c) * 255 / 8).to_string()),
    )
}

/// A VMF with a room, a displaced floor with a blended material, a sky
/// ceiling, a nodraw brush, a func_detail, a func_door, a hidden solid,
/// a prop and a player start.
fn sample() -> String {
    let mut world = String::from(
        "versioninfo\n{\n\t\"editorversion\" \"400\"\n}\nvisgroups\n{\n}\nworld\n{\n\t\"id\" \"1\"\n\t\"classname\" \"worldspawn\"\n\t\"skyname\" \"sky_test\"\n",
    );
    world += &solid(2, [-256, -256, -16], [256, 256, 0], "dev/wall", "nature/blend", &dispinfo([-256, -256, 0]));
    world += &solid(3, [-256, -256, 256], [256, 256, 272], "tools/toolsskybox", "tools/toolsskybox", "");
    world += &solid(4, [-256, -256, 0], [-240, 256, 256], "dev/wall", "dev/wall", "");
    world += &solid(5, [0, 0, 0], [8, 8, 8], "tools/toolsnodraw", "tools/toolsnodraw", "");
    world += "\thidden\n\t{\n";
    world += &solid(6, [100, 100, 0], [116, 116, 16], "dev/wall", "dev/wall", "");
    world += "\t}\n}\n";
    world += "entity\n{\n\t\"id\" \"10\"\n\t\"classname\" \"func_detail\"\n";
    world += &solid(11, [-100, -100, 0], [-90, -90, 32], "dev/wall", "dev/wall", "");
    world += "}\nentity\n{\n\t\"id\" \"12\"\n\t\"classname\" \"func_door\"\n\t\"targetname\" \"door1\"\n";
    world += &solid(13, [200, -20, 0], [210, 20, 96], "glass/window", "glass/window", "");
    world += "}\nentity\n{\n\t\"id\" \"14\"\n\t\"classname\" \"prop_static\"\n\t\"model\" \"models/props_c17/oildrum001.mdl\"\n\t\"origin\" \"10 20 0\"\n}\n";
    world += "entity\n{\n\t\"id\" \"15\"\n\t\"classname\" \"info_player_start\"\n\t\"angles\" \"0 90 0\"\n\t\"origin\" \"0 0 8\"\n}\n";
    world += "entity\n{\n\t\"id\" \"16\"\n\t\"classname\" \"trigger_multiple\"\n";
    world += &solid(17, [0, 0, 0], [8, 8, 8], "tools/toolstrigger", "tools/toolstrigger", "");
    world += "}\ncameras\n{\n\t\"activecamera\" \"-1\"\n}\ncordons\n{\n\t\"active\" \"0\"\n}\n";
    world
}

/// Writes the sample's materials: VMTs and small DXT1 or BGR888 VTFs.
fn write_materials(root: &Path) -> Vec<(String, Vec<u8>)> {
    let bgr = |c: [u8; 3]| crate::vtf::tests::build(4, 3, 4, 4, 0, &[c.repeat(16)]);
    let files: Vec<(String, Vec<u8>)> = vec![
        ("materials/dev/wall.vmt".into(), b"LightmappedGeneric { $basetexture dev/wall_t }".to_vec()),
        (
            "materials/dev/wall_t.vtf".into(),
            crate::vtf::tests::build(5, 13, 4, 4, 0, &[vec![0xff, 0xff, 0, 0, 0, 0, 0, 0]]),
        ),
        (
            "materials/nature/blend.vmt".into(),
            b"WorldVertexTransition { $basetexture nature/dirt $basetexture2 nature/grass }".to_vec(),
        ),
        ("materials/nature/dirt.vtf".into(), bgr([90, 60, 30])),
        ("materials/nature/grass.vtf".into(), bgr([40, 140, 30])),
        ("materials/skybox/sky_testrt.vmt".into(), b"UnlitGeneric { $basetexture skybox/rt }".to_vec()),
        ("materials/skybox/rt.vtf".into(), bgr([200, 100, 100])),
    ];
    for (rel, data) in &files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }
    files
}

#[test]
fn reads_a_hammer_map() {
    let dir = scratch("sample");
    write_materials(&dir);
    let text = sample();
    let path = dir.join("mapsrc/test.vmf");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &text).unwrap();
    assert!(sniff(text.as_bytes()));
    assert_eq!(crate::sniff(&path, text.as_bytes()), Some(crate::QuakeFormat::Vmf));

    let s = load_file(&path, &Limits::viewer());
    eprintln!("{}", describe(&s));
    assert_eq!(s.info.format, "Source map (Hammer VMF)");
    let names: Vec<(&str, bool)> = s.nodes.iter().map(|n| (n.name.as_str(), n.visible)).collect();
    assert_eq!(names, [("world", true), ("hidden in Hammer", false), ("func_door door1", true)]);
    // Only glass/window's material is missing (no file at all).
    assert_eq!(s.info.warnings, ["1 materials not found: glass/window"]);
    let mat = |name: &str| s.materials.iter().find(|m| m.name == name).unwrap_or_else(|| panic!("{name}"));
    assert!(mat("dev/wall").base_color_texture.is_some());
    assert_eq!(mat("sky").effect, Effect::PanoramaSky);
    assert_eq!(mat("nature/blend (blend)").alpha_mode, AlphaMode::Blend);
    // The world: the displacement (16 cells, 2 triangles each) and its
    // blend layer, the wall brush's six faces, the func_detail's six, the
    // sky brush's six, and nothing of the nodraw brush.
    let world = &s.meshes[s.nodes[0].mesh.unwrap()];
    let count = |name: &str| -> usize {
        world
            .primitives
            .iter()
            .filter(|p| p.material.is_some_and(|m| s.materials[m].name == name))
            .map(|p| p.triangle_count())
            .sum()
    };
    assert_eq!(count("nature/blend"), 32);
    assert!(count("nature/blend (blend)") > 0 && count("nature/blend (blend)") <= 32);
    assert_eq!(count("dev/wall"), 24, "the displacement brush's other sides are hidden");
    assert_eq!(count("sky"), 12);
    // The bump in the middle of the floor: 64 units up.
    let blend =
        world.primitives.iter().find(|p| p.material.is_some_and(|m| s.materials[m].name == "nature/blend")).unwrap();
    let top = blend.positions.iter().map(|p| p.y).fold(f32::MIN, f32::max);
    assert!((top - 64.0 * crate::UNIT).abs() < 1e-5, "{top}");
    // Markers: the prop by its model's name and the start; the trigger's
    // brush is not drawn.
    let labels: Vec<&str> = s.markers.iter().map(|m| m.label.as_str()).collect();
    assert_eq!(labels, ["oildrum001", "info_player_start"]);
    let spawn = s.spawn.unwrap();
    assert!((spawn.position.y - (8.0 + 64.0) * crate::UNIT).abs() < 1e-5);
    assert!(s.info.neighbours.iter().any(|p| p.ends_with("nature/grass.vtf")), "{:?}", s.info.neighbours);
    let _ = std::fs::remove_dir_all(dir);
}

/// The same map in a game folder whose materials are in a VPK pack, with
/// Half-Life 2's pack beside it.
#[test]
fn materials_from_vpk_packs() {
    let dir = scratch("vpk");
    let staging = dir.join("staging");
    let files = write_materials(&staging);
    let game = dir.join("Game/mygame");
    std::fs::create_dir_all(game.join("mapsrc")).unwrap();
    std::fs::write(game.join("gameinfo.txt"), "GameInfo { }").unwrap();
    let (mine, shared): (Vec<_>, Vec<_>) = files.iter().partition(|(rel, _)| !rel.starts_with("materials/dev/"));
    fn as_refs<'a>(v: &[&'a (String, Vec<u8>)]) -> Vec<(&'a str, &'a [u8])> {
        v.iter().map(|(r, d)| (r.as_str(), d.as_slice())).collect()
    }
    crate::vpk::tests::write(&game, 2, &[], &as_refs(&mine));
    crate::vpk::tests::write(&dir.join("Game/hl2"), 1, &as_refs(&shared), &[]);
    let path = game.join("mapsrc/test.vmf");
    std::fs::write(&path, sample()).unwrap();
    let s = load_file(&path, &Limits::thumbnail());
    eprintln!("{}", describe(&s));
    assert_eq!(s.info.warnings, ["1 materials not found: glass/window"]);
    let from = s.info.details.iter().find(|(k, _)| k == "Materials from").map(|(_, v)| v.as_str()).unwrap_or("");
    assert!(from.contains("pak01_dir.vpk"), "{from}");
    assert!(s.info.neighbours.iter().any(|p| p.ends_with("Game/hl2/pak01_dir.vpk")), "{:?}", s.info.neighbours);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn damaged_vmfs_never_panic() {
    let text = sample();
    let bytes = text.as_bytes();
    let mut cases = crate::testutil::mutations(bytes);
    for cut in (0..bytes.len()).step_by(97) {
        cases.push(bytes[..cut].to_vec());
    }
    cases.push(text.replace("\"power\" \"2\"", "\"power\" \"30\"").into_bytes());
    cases.push(text.replace("[-256 -256 0]", "[1e308 nan 0]").into_bytes());
    cases.push(text.replace("(-256 256 0)", "(-256 256 1e300)").into_bytes());
    cases.push(text.replace("0.25", "0").into_bytes());
    cases.push("world { solid { side { \"plane\" \"(0 0 0) (0 0 0) (0 0 0)\" } } }".repeat(50).into_bytes());
    for (i, case) in cases.iter().enumerate() {
        let r = std::panic::catch_unwind(|| load(Path::new("/nonexistent/x.vmf"), case, &Limits::thumbnail()));
        assert!(r.is_ok(), "case {i} panicked");
    }
}

#[test]
fn sniffs_vmfs() {
    assert!(sniff(b"versioninfo\n{\n\t\"editorversion\" \"400\""));
    assert!(sniff(b"\xef\xbb\xbf// saved\r\n\"world\"\r\n{\r\n"));
    assert!(sniff(b"world {"));
    assert!(!sniff(b"worldspawn {"));
    assert!(!sniff(b"versioninfo = 3"));
    assert!(!sniff(b"{\"version\":3}"));
}

/// Every downloaded VMF loads, with its load time.
#[test]
fn real_vmfs_load() {
    let mut paths = Vec::new();
    for rel in [
        "source/hl2sb-mapsrc/sb_wasteland.vmf",
        "source/hl2sb-mapsrc/sb_field.vmf",
        "source/vmfinstanceinserter/instancetest_box.vmf",
        "source/vmf-tool-examples/test_disp.vmf",
        "source/vmf-tool-examples/hemisphere.vmf",
        "source/vmf-tool-examples/power2_obj.vmf",
        "source/vmf-tool-examples/test2.vmf",
        "source/valve/sdk_pl_goldrush.vmf",
    ] {
        paths.extend(asset(rel));
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

/// Writes `/tmp/slerp-editor/vmfdemo/`: Hammer's sb_wasteland map with
/// stand-in materials (VMTs naming VTFs made from LibreQuake's BSD-3
/// textures, its terrain a WorldVertexTransition blend of dirt and grass),
/// for screenshots of the whole material path.
#[test]
#[ignore]
fn make_demo() {
    let (Some(map), Some(wads)) = (asset("source/hl2sb-mapsrc/sb_wasteland.vmf"), asset("quake/librequake-texwads"))
    else {
        return;
    };
    let out = Path::new("/tmp/slerp-editor/vmfdemo");
    let _ = std::fs::remove_dir_all(out);
    std::fs::create_dir_all(out.join("mapsrc")).unwrap();
    std::fs::copy(&map, out.join("mapsrc/sb_wasteland.vmf")).unwrap();
    let vtf_of = |wad: &str, name: &str| -> Vec<u8> {
        let bytes = std::fs::read(wads.join(wad)).unwrap();
        let w = crate::wad::Wad::parse(&bytes).unwrap();
        let e = w.entries.iter().find(|e| e.name.eq_ignore_ascii_case(name)).unwrap();
        let d = w.decode(e, crate::pixels::QUAKE_PALETTE, None).unwrap().unwrap();
        let Pixels::Rgba8(px) = d.base.base() else { panic!() };
        let bgr: Vec<u8> = px.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]).collect();
        crate::vtf::tests::build(4, 3, d.base.width() as u16, d.base.height() as u16, 0, &[bgr])
    };
    let write = |rel: &str, data: &[u8]| {
        let p = out.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    };
    write("materials/demo/dirt.vtf", &vtf_of("lq_terra.wad", "med_bigdirt"));
    write("materials/demo/grass.vtf", &vtf_of("lq_terra.wad", "grass1"));
    write("materials/demo/gravel.vtf", &vtf_of("lq_terra.wad", "gravel1"));
    write("materials/demo/brick.vtf", &vtf_of("lq_medieval.wad", "brick1"));
    write(
        "materials/nature/dirtfloor008a.vmt",
        b"WorldVertexTransition { $basetexture demo/dirt $basetexture2 demo/grass }",
    );
    write("materials/nature/dirtfloor003a.vmt", b"LightmappedGeneric { $basetexture demo/gravel }");
    write("materials/nature/dirtfloor003b.vmt", b"LightmappedGeneric { $basetexture demo/gravel }");
    write("materials/brick/brickwall003a.vmt", b"LightmappedGeneric { $basetexture demo/brick }");

    // A hill of four power-4 displacements blending from dirt to grass.
    let n = 17;
    let rows = |f: &dyn Fn(usize, usize) -> String| {
        (0..n)
            .map(|r| format!("\"row{r}\" \"{}\"\n", (0..n).map(|c| f(r, c)).collect::<Vec<_>>().join(" ")))
            .collect::<String>()
    };
    let mut world = String::from(
        "versioninfo\n{\n\"editorversion\" \"400\"\n}\nworld\n{\n\"classname\" \"worldspawn\"\n\"skyname\" \"sky_day01_01\"\n",
    );
    for (k, (x0, y0)) in [(-1024, -1024), (0, -1024), (-1024, 0), (0, 0)].into_iter().enumerate() {
        let height = |r: usize, c: usize| {
            let (x, y) = (x0 as f64 + c as f64 * 64.0, y0 as f64 + r as f64 * 64.0);
            let d = (x * x + y * y).sqrt();
            (300.0 * (-d * d / 400_000.0).exp() + 40.0 * (x / 150.0).sin() * (y / 190.0).cos()).max(0.0)
        };
        let disp = format!(
            "dispinfo\n{{\n\"power\" \"4\"\n\"startposition\" \"[{x0} {y0} 0]\"\n\"elevation\" \"0\"\nnormals\n{{\n{}}}\ndistances\n{{\n{}}}\noffsets\n{{\n{}}}\nalphas\n{{\n{}}}\n}}\n",
            rows(&|_, _| "0 0 1".into()),
            rows(&|r, c| format!("{:.2}", height(r, c))),
            rows(&|_, _| "0 0 0".into()),
            rows(&|r, c| format!("{:.0}", (height(r, c) / 300.0 * 255.0).min(255.0))),
        );
        world += &solid(
            100 + k as u32,
            [x0, y0, -16],
            [x0 + 1024, y0 + 1024, 0],
            "tools/toolsnodraw",
            "nature/dirtfloor008a",
            &disp,
        );
    }
    world +=
        "}\nentity\n{\n\"classname\" \"info_player_start\"\n\"origin\" \"-900 -900 200\"\n\"angles\" \"0 45 0\"\n}\n";
    write("mapsrc/hills.vmf", world.as_bytes());
}

/// A few bytes of `power 4` per displacement can't ask for unbounded
/// vertices: they are built up to a budget in proportion to the file.
#[test]
fn displacement_stubs_stay_bounded() {
    let stub =
        "\t\t\tdispinfo\n\t\t\t{\n\t\t\t\t\"power\" \"4\"\n\t\t\t\t\"startposition\" \"[-256 -256 0]\"\n\t\t\t}\n";
    let one = solid(2, [-256, -256, -16], [256, 256, 0], "dev/wall", "dev/wall", stub);
    let text = format!("world\n{{\n\"classname\" \"worldspawn\"\n{}}}\n", one.repeat(3000));
    let s = load(Path::new("/nonexistent/x.vmf"), text.as_bytes(), &Limits::viewer()).unwrap();
    let budget = text.len() / 16;
    // Each built displacement is 289 vertices and 512 triangles; the rest
    // are plain boxes.
    let displaced =
        s.info.details.iter().find(|(k, _)| k == "Displacements").map_or(0, |(_, v)| v.parse::<usize>().unwrap());
    assert!(displaced * 289 <= budget, "{displaced} displacements for a budget of {budget}");
    assert!(displaced > 0);
}

/// Times the stages of loading a large VMF.  Run with `--release --ignored
/// --nocapture stages`.
#[test]
#[ignore]
fn stages() {
    let Some(p) = asset("source/valve/sdk_pl_goldrush.vmf") else { return };
    let bytes = std::fs::read(&p).unwrap();
    let t = std::time::Instant::now();
    let kv = keyvalues::parse(&bytes);
    eprintln!("keyvalues {:?} ({} top)", t.elapsed(), kv.len());
    let t = std::time::Instant::now();
    let v = parse(&bytes);
    eprintln!("parse {:?} ({} entities)", t.elapsed(), v.entities.len());
    let t = std::time::Instant::now();
    let s = load(&p, &bytes, &Limits::thumbnail()).unwrap();
    eprintln!("load {:?} ({} triangles)", t.elapsed(), s.triangle_count());
}
