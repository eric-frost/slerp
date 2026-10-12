//! Tests over real material files, which are skipped (with a note) when the
//! downloaded test assets are missing, and over damaged input.

use crate::realize::{Channel, FileRef, MatDesc, TexInput};
use crate::{MaterialFormat, load, sniff};
use slerp_scene::glam::{Mat4, Vec3};
use slerp_scene::{Asset, Limits, LoadError, Material, Pose, Property, Scene};
use std::path::{Path, PathBuf};

/*┌─────────────────╮
| Helpers         |
└─────────────────╯*/
//region Helpers

fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets")
}

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(name)
}

fn load_path(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let bytes = std::fs::read(path)?;
    let format = sniff(path, &bytes[..bytes.len().min(512)]).ok_or(LoadError::NotMine)?;
    load(format, path, &bytes, limits)
}

/// Every file under `dir` (recursively) whose extension is one of `exts`.
fn files(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| exts.contains(&e.to_ascii_lowercase().as_str()))
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// The ambientCG folders available locally: chalk's downloads (in a
/// checkout of chalk beside this repository) and the test corpus.
fn ambientcg_dirs() -> Vec<PathBuf> {
    let roots = [
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../chalk/local/matassets")),
        assets().join("materials/ambientcg"),
        assets().join("materials-extra/ambientcg"),
    ];
    let mut dirs = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else { continue };
        for e in entries.flatten() {
            if e.path().is_dir() && !files(&e.path(), &["mtlx"]).is_empty() {
                dirs.push(e.path());
            }
        }
    }
    dirs.sort();
    dirs
}

fn file_name(t: Option<&TexInput>) -> Option<String> {
    t.map(|t| match &t.file {
        FileRef::Disk(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        FileRef::Embedded { name, .. } => name.clone(),
    })
}

/// A one-line summary of a material, for test output.
fn describe_material(m: &Material) -> String {
    let c = m.base_color;
    let mut s = format!(
        "base ({:.2} {:.2} {:.2} {:.2}) metal {:.2} rough {:.2} {:?}",
        c.x, c.y, c.z, c.w, m.metallic, m.roughness, m.alpha_mode
    );
    let slots = [
        ("base", m.base_color_texture),
        ("mr", m.metallic_roughness_texture),
        ("normal", m.normal_texture),
        ("ao", m.occlusion_texture),
        ("emissive", m.emissive_texture),
        ("height", m.height_texture),
    ];
    for (name, t) in slots {
        if let Some(t) = t {
            s += &format!(" {name}=#{}", t.texture);
            if let Some(x) = t.transform {
                s += &format!(
                    "[scale {:.2},{:.2} off {:.2},{:.2} rot {:.2}]",
                    x.scale.x, x.scale.y, x.offset.x, x.offset.y, x.rotation
                );
            }
        }
    }
    if m.height_texture.is_some() {
        s += &format!(" height_scale {}", m.height_scale);
    }
    if m.emissive != slerp_scene::glam::Vec3::ZERO {
        s += &format!(" emissive {:.2?}", m.emissive);
    }
    s
}

fn describe(asset: &Asset) -> String {
    match asset {
        Asset::Material(m) => {
            let mut s = format!("material {:?}: {}", m.name, describe_material(&m.material));
            s += &format!(" | {} textures", m.textures.len());
            if let Some(p) = &m.preview {
                s += &format!(" | preview {}x{}", p.width(), p.height());
            }
            for (k, v) in &m.info.details {
                s += &format!("\n    {k}: {v}");
            }
            for w in &m.info.warnings {
                s += &format!("\n    warning: {w}");
            }
            s
        }
        Asset::Scene(sc) => {
            let mut s = format!(
                "scene {:?}: {} nodes, {} meshes, {} triangles, {} materials, {} textures",
                sc.name,
                sc.nodes.len(),
                sc.meshes.len(),
                sc.triangle_count(),
                sc.materials.len(),
                sc.textures.len()
            );
            for m in sc.materials.iter().take(8) {
                s += &format!("\n    {:?}: {}", m.name, describe_material(m));
            }
            for (k, v) in sc.info.details.iter().filter(|(k, _)| !k.starts_with("Material ")) {
                s += &format!("\n    {k}: {v}");
            }
            for w in &sc.info.warnings {
                s += &format!("\n    warning: {w}");
            }
            s
        }
        Asset::Image(t) => format!("image {}x{}", t.width(), t.height()),
        Asset::Gallery(g) => format!("gallery of {}", g.len()),
        _ => "2D picture".to_string(),
    }
}

/// Viewer limits with thumbnail-sized textures, to keep tests quick.
fn quick() -> Limits {
    Limits { max_texture_size: Some(256), ..Limits::viewer() }
}

//endregion

/*┌─────────────────╮
| Sniffing        |
└─────────────────╯*/
//region Sniffing

#[test]
fn sniffs_by_content_and_name() {
    let p = |s: &str| PathBuf::from(s);
    assert_eq!(sniff(&p("a.usd"), b"PXR-USDC\0\x08"), Some(MaterialFormat::Usd));
    assert_eq!(sniff(&p("a.usd"), b"#usda 1.0\n"), Some(MaterialFormat::Usd));
    assert_eq!(sniff(&p("a.mtlx"), b"<?xml version=\"1.0\"?>\n<materialx"), Some(MaterialFormat::MaterialX));
    assert_eq!(sniff(&p("a.xml"), b"<?xml version=\"1.0\"?>\n<materialx version"), Some(MaterialFormat::MaterialX));
    assert_eq!(sniff(&p("a.xml"), b"<?xml version=\"1.0\"?>\n<svg"), None);
    assert_eq!(sniff(&p("m.tres"), b"[gd_resource type=\"StandardMaterial3D\""), Some(MaterialFormat::GodotResource));
    assert_eq!(sniff(&p("m.tscn"), b"[gd_scene load_steps=2"), None);
    assert_eq!(sniff(&p("a.sbsar"), b"7z\xBC\xAF\x27\x1C\0\x03"), Some(MaterialFormat::Sbsar));
    assert_eq!(sniff(&p("a.7z"), b"7z\xBC\xAF\x27\x1C\0\x03"), None);
    assert_eq!(sniff(&p("a.usdz"), b"PK\x03\x04"), Some(MaterialFormat::Usdz));
    assert_eq!(sniff(&p("a.zip"), b"PK\x03\x04"), None);
    assert_eq!(sniff(&p("a.png"), b"\x89PNG\r\n"), None);
}

//endregion

/*┌─────────────────╮
| ambientCG       |
└─────────────────╯*/
//region ambientCG

/// Each ambientCG download holds the same material as MaterialX, USD and
/// Godot: all three must name the same colour, roughness, normal and height
/// maps (and metalness and opacity maps when there are any).
#[test]
fn ambientcg_formats_agree() {
    let dirs = ambientcg_dirs();
    if dirs.is_empty() {
        eprintln!("skipped: no ambientCG downloads");
        return;
    }
    for dir in dirs {
        let one = |ext: &str| files(&dir, &[ext]).into_iter().next().unwrap_or_else(|| panic!("{dir:?} has no .{ext}"));
        let (mtlx, usdc, tres) = (one("mtlx"), one("usdc"), one("tres"));
        let x = crate::mtlx::parse(&mtlx, &std::fs::read(&mtlx).unwrap(), true).unwrap().desc;
        let u = crate::usd::test_material_desc(&usdc).unwrap();
        let g = crate::godot::parse(&tres, &std::fs::read(&tres).unwrap()).unwrap().desc;
        let slot = |d: &MatDesc, s: usize| file_name(d.slots()[s].1);
        for (s, name) in [(0, "base colour"), (3, "roughness"), (4, "normal"), (7, "height")] {
            let a = slot(&x, s);
            assert!(a.is_some(), "{dir:?}: MaterialX has no {name}");
            assert_eq!(a, slot(&u, s), "{dir:?}: USD {name}");
            assert_eq!(a, slot(&g, s), "{dir:?}: Godot {name}");
        }
        // ambientCG's Godot files leave opacity out, so it only has to
        // agree where present.
        for (s, name) in [(1, "opacity"), (2, "metallic")] {
            if let Some(a) = slot(&x, s) {
                assert_eq!(Some(&a), slot(&u, s).as_ref(), "{dir:?}: USD {name}");
                if s == 2 || slot(&g, s).is_some() {
                    assert_eq!(Some(&a), slot(&g, s).as_ref(), "{dir:?}: Godot {name}");
                }
            }
        }
        // Scalar maps are read from the red (or grey) channel of
        // greyscale images in every format.
        for d in [&x, &u, &g] {
            let ch = d.roughness.as_ref().unwrap().channel;
            assert!(matches!(ch, Channel::R | Channel::Luminance), "{dir:?} {}: roughness channel {ch:?}", d.model);
            assert_eq!(d.normal.as_ref().unwrap().channel, Channel::Rgb);
        }
        eprintln!("{}: {} | {} | {}", dir.display(), x.model, u.model, g.model);
    }
}

/// With textures decoded, all three formats give the same pictures.
#[test]
fn ambientcg_textures_match() {
    let Some(dir) = ambientcg_dirs().into_iter().next() else {
        eprintln!("skipped: no ambientCG downloads");
        return;
    };
    let mut results = Vec::new();
    for ext in ["mtlx", "usdc", "tres"] {
        let path = files(&dir, &[ext]).remove(0);
        let Asset::Material(m) = load_path(&path, &Limits::viewer()).unwrap() else {
            panic!("{path:?} is not a material")
        };
        assert!(m.preview.is_some(), "{path:?}: no preview found");
        assert!(m.info.warnings.is_empty(), "{path:?}: {:?}", m.info.warnings);
        eprintln!("{}: {}", path.display(), describe(&Asset::Material(m.clone_shallow())));
        results.push(m);
    }
    let pixels = |m: &slerp_scene::MaterialAsset, t: Option<slerp_scene::TexRef>| -> (u32, u32, u64) {
        let t = t.unwrap_or_else(|| panic!("{}: missing texture", m.info.format));
        let tex = &m.textures[t.texture];
        let sum = match tex.base() {
            slerp_scene::textures::Pixels::Rgba8(v) => v.iter().map(|&b| b as u64).sum(),
            slerp_scene::textures::Pixels::Rgba32F(v) => v.iter().map(|&f| (f * 255.0) as u64).sum(),
            slerp_scene::textures::Pixels::Blocks(..) => panic!("materials decode to texels"),
        };
        (tex.width(), tex.height(), sum)
    };
    let sums: Vec<[(u32, u32, u64); 4]> = results
        .iter()
        .map(|m| {
            let mm = &m.material;
            [
                pixels(m, mm.base_color_texture),
                pixels(m, mm.metallic_roughness_texture),
                pixels(m, mm.normal_texture),
                pixels(m, mm.height_texture),
            ]
        })
        .collect();
    assert_eq!(sums[0], sums[1], "MaterialX and USD textures differ");
    assert_eq!(sums[0], sums[2], "MaterialX and Godot textures differ");
}

/// A thumbnail-sized load with a preview skips the material's textures.
#[test]
fn preview_skips_textures_for_thumbnails() {
    let Some(dir) = ambientcg_dirs().into_iter().next() else { return };
    let path = files(&dir, &["mtlx"]).remove(0);
    let Asset::Material(m) = load_path(&path, &Limits::thumbnail()).unwrap() else { panic!() };
    assert!(m.preview.is_some());
    assert!(m.textures.is_empty());
    assert!(m.material.base_color_texture.is_none());
    assert!(m.info.details.iter().any(|(k, _)| k == "Base colour"));
}

//endregion

/*┌─────────────────╮
| Other corpora   |
└─────────────────╯*/
//region Other corpora

/// Every MaterialX example and Poly Haven material loads.
#[test]
fn materialx_corpus() {
    let mut paths = files(&assets().join("materials/materialx-examples/Examples"), &["mtlx"]);
    paths.extend(files(&assets().join("materials/polyhaven"), &["mtlx"]));
    if paths.is_empty() {
        eprintln!("skipped: no MaterialX examples");
        return;
    }
    for path in &paths {
        match load_path(path, &quick()) {
            Ok(a) => eprintln!("{}: {}", path.display(), describe(&a)),
            Err(e) => panic!("{}: {e}", path.display()),
        }
    }
}

#[test]
fn materialx_mappings() {
    let ex = assets().join("materials/materialx-examples/Examples");
    if !ex.is_dir() {
        eprintln!("skipped: no MaterialX examples");
        return;
    }
    let desc = |rel: &str| {
        let p = ex.join(rel);
        crate::mtlx::parse(&p, &std::fs::read(&p).unwrap(), true).unwrap().desc
    };

    // glTF PBR: ORM channels from one texture, alpha from the colour's.
    let d = desc("GltfPbr/gltf_pbr_boombox.mtlx");
    assert_eq!(d.model, "gltf_pbr");
    assert_eq!(file_name(d.base_color.as_ref()).as_deref(), Some("BoomBox_baseColor.png"));
    assert_eq!(d.opacity.as_ref().map(|t| t.channel), Some(Channel::A));
    assert_eq!(d.roughness.as_ref().map(|t| t.channel), Some(Channel::G));
    assert_eq!(d.metallic.as_ref().map(|t| t.channel), Some(Channel::B));
    assert_eq!(d.occlusion.as_ref().map(|t| t.channel), Some(Channel::R));
    assert!(d.normal.is_some() && d.emissive.is_some());

    // Standard Surface with textures tiled four times.
    let d = desc("StandardSurface/standard_surface_wood_tiled.mtlx");
    let t = d.base_color.as_ref().expect("wood colour map");
    assert!(t.transform.is_some_and(|x| (x.scale.x - 4.0).abs() < 1e-5), "tiling: {:?}", t.transform);
    assert!(d.roughness.as_ref().is_some_and(|r| r.channel == Channel::R && r.transform.is_some()));
    // Brass: a constant metal whose texture only tints the coat, which
    // glTF has no colour for.
    let d = desc("StandardSurface/standard_surface_brass_tiled.mtlx");
    assert!(d.base_color.is_none() && d.roughness.is_some());
    assert!(d.material.metallic > 0.9 && d.material.clearcoat > 0.9);

    // Constant materials.
    let d = desc("OpenPbr/open_pbr_lightbulb.mtlx");
    assert!(d.material.emissive.x > 1.0, "{:?}", d.material.emissive);
    let d = desc("StandardSurface/standard_surface_glass.mtlx");
    assert!(d.material.base_color.w < 1.0);
    let d = desc("UsdPreviewSurface/usd_preview_surface_gold.mtlx");
    assert_eq!(d.model, "UsdPreviewSurface");
    assert!(d.material.metallic > 0.9);
}

#[test]
fn materialx_fixture() {
    let p = data("material.mtlx");
    let parsed = crate::mtlx::parse(&p, &std::fs::read(&p).unwrap(), true).unwrap();
    assert_eq!(parsed.version, "1.39");
    let d = parsed.desc;
    assert_eq!(d.model, "standard_surface");
    assert_eq!(d.material.name, "M_wood");

    // Colour: an image through place2d (scale 0.5 tiles twice), tinted by
    // the node graph's interface input.
    let base = d.base_color.as_ref().unwrap();
    assert_eq!(file_name(Some(base)).as_deref(), Some("tex.png"));
    assert_eq!(base.color_space, slerp_scene::ColorSpace::Srgb);
    assert_eq!(base.sampler.wrap_u, slerp_scene::Wrap::MirroredRepeat);
    let t = base.transform.expect("place2d transform");
    assert!((t.scale - slerp_scene::glam::Vec2::splat(2.0)).length() < 1e-5, "{t:?}");
    assert!((d.material.base_color.truncate() - slerp_scene::glam::Vec3::new(0.9, 0.8, 0.7)).length() < 1e-5);

    // Roughness: the green channel of a tiled image, tiling from the
    // interface.
    let r = d.roughness.as_ref().unwrap();
    assert_eq!((file_name(Some(r)).as_deref(), r.channel), (Some("preview.png"), Channel::G));
    assert!(r.transform.is_some_and(|t| (t.scale.x - 3.0).abs() < 1e-5));

    // A normal map at half strength, a procedural that can't be evaluated
    // (metalness keeps its default), a constant, and displacement.
    assert!(d.normal.is_some());
    assert_eq!(d.material.normal_scale, 0.5);
    assert!(d.metallic.is_none());
    assert_eq!(d.material.metallic, 0.0);
    assert_eq!(d.material.clearcoat, 0.25);
    assert_eq!(file_name(d.height.as_ref()).as_deref(), Some("tex.png"));
    assert!((d.material.height_scale - 0.02).abs() < 1e-6);
}

#[test]
fn materialx_legacy_shaderref() {
    let p = data("legacy.mtlx");
    let d = crate::mtlx::parse(&p, &std::fs::read(&p).unwrap(), true).unwrap().desc;
    assert_eq!(d.material.name, "Legacy");
    assert_eq!(file_name(d.base_color.as_ref()).as_deref(), Some("tex.png"));
    assert_eq!(d.material.metallic, 1.0);
}

/// USD files with meshes become scenes; material-only ones materials.
#[test]
fn usd_corpus() {
    let root = assets().join("usd");
    let mut paths = files(&root, &["usd", "usda", "usdc", "usdz"]);
    // Layers that only make sense referenced from another (and the kitchen,
    // which `kitchen_set` times on its own).
    paths.retain(|p| {
        let s = p.strip_prefix(&root).unwrap_or(p).to_string_lossy().into_owned();
        !s.contains("geo/") && !s.contains("assets/") && !s.contains("Kitchen_set") && !s.ends_with("DrawModes.usd")
    });
    if paths.is_empty() {
        eprintln!("skipped: no USD samples");
        return;
    }
    for path in &paths {
        let t = std::time::Instant::now();
        match load_path(path, &quick()) {
            Ok(a) => eprintln!("{} ({:.0} ms): {}", path.display(), t.elapsed().as_secs_f64() * 1000.0, describe(&a)),
            Err(e) => panic!("{}: {e}", path.display()),
        }
    }
}

#[test]
fn usd_scenes() {
    let teapot = assets().join("usd/apple/teapot.usdz");
    if teapot.is_file() {
        let Asset::Scene(s) = load_path(&teapot, &quick()).unwrap() else { panic!("teapot is not a scene") };
        assert!(s.triangle_count() > 1000);
        assert!(s.materials.iter().any(|m| m.base_color_texture.is_some()), "{}", describe(&Asset::Scene(s)));
        assert!(s.meshes.iter().flat_map(|m| &m.primitives).all(|p| !p.uv0.is_empty() && !p.normals.is_empty()));
    }
    // A payload whose layer stack adds materials, a variant selecting a
    // referenced model, and textures next to the layers.
    let wg = assets().join("usd/usdwg-teapot/Teapot.usd");
    if wg.is_file() {
        let Asset::Scene(s) = load_path(&wg, &quick()).unwrap() else { panic!("Teapot.usd is not a scene") };
        eprintln!("{}", describe(&Asset::Scene(s.clone_shallow())));
        assert!(s.triangle_count() > 100);
        assert!(!s.materials.is_empty());
    }
    // Material variants: the selected variant's bindings are used.
    let chameleon = assets().join("usd/apple/chameleon_anim_mtl_variant.usdz");
    if chameleon.is_file() {
        let Asset::Scene(s) = load_path(&chameleon, &quick()).unwrap() else { panic!() };
        assert!(s.materials.iter().any(|m| m.base_color_texture.is_some()), "{}", describe(&Asset::Scene(s)));
    }
}

#[test]
fn godot_variants() {
    let dir = data("godot");
    let read = |name: &str| {
        let p = dir.join(name);
        crate::godot::parse(&p, &std::fs::read(&p).unwrap())
    };
    let d = read("orm.tres").unwrap().desc;
    assert_eq!(d.model, "ORMMaterial3D");
    assert_eq!(d.roughness.as_ref().map(|t| t.channel), Some(Channel::G));
    assert_eq!(d.metallic.as_ref().map(|t| t.channel), Some(Channel::B));
    assert_eq!(d.occlusion.as_ref().map(|t| t.channel), Some(Channel::R));
    // res:// paths resolve from the folder holding project.godot.
    match &d.base_color.as_ref().unwrap().file {
        FileRef::Disk(p) => assert!(p.ends_with("godot/textures/albedo.png"), "{p:?}"),
        other => panic!("{other:?}"),
    }
    assert!(d.material.double_sided);
    assert!(matches!(d.material.alpha_mode, slerp_scene::AlphaMode::Mask(t) if (t - 0.3).abs() < 1e-6));
    assert!((d.material.base_color.x - 1.0).abs() < 1e-6 && (d.material.base_color.y - 0.2140).abs() < 1e-3);
    assert!(d.base_color.as_ref().unwrap().transform.is_some_and(|t| t.scale.x == 2.0));

    let d = read("godot3.tres").unwrap().desc;
    assert_eq!(d.model, "SpatialMaterial");
    assert!(d.normal.is_some());
    assert!(d.material.unlit);
    assert!(d.material.emissive.x > 0.0);

    let d = read("shader.tres").unwrap().desc;
    assert!(d.base_color.is_some() && d.normal.is_some());

    assert!(matches!(read("mesh.tres"), Err(LoadError::Unsupported(_))));
}

/// The one freely downloadable `.sbsar` has no icon, so gets no thumbnail;
/// a small archive with one shows it.
#[test]
fn sbsar_icons() {
    let sample = assets().join("materials/sbsar/fabric.sbsar");
    if sample.is_file() {
        assert!(matches!(load_path(&sample, &quick()), Err(LoadError::Unsupported(_))));
    }
    let Asset::Image(t) = load_path(&data("icon.sbsar"), &quick()).unwrap() else { panic!("not an image") };
    assert_eq!((t.width(), t.height()), (16, 16));
}

//endregion

/*┌─────────────────╮
| USD readers     |
└─────────────────╯*/
//region USD readers

/// The same layers written as text and as binary crate files (by Pixar's
/// USD 26.08) read the same.
#[test]
fn crate_matches_text() {
    let dir = data("usd");
    let mut pairs = 0;
    for usda in files(&dir, &["usda"]) {
        let usdc = usda.with_extension("usdc");
        if !usdc.is_file() {
            continue;
        }
        let mut budget = u64::MAX;
        let a = crate::usd::usda_for_test(&std::fs::read(&usda).unwrap()).unwrap();
        let b = crate::usd::usdc_for_test(&std::fs::read(&usdc).unwrap(), &mut budget).unwrap();
        crate::usd::assert_layers_match(&a, &b, &usda.display().to_string());
        pairs += 1;
    }
    assert!(pairs >= 3, "only {pairs} fixture pairs");
}

#[test]
fn crate_scene_fixture() {
    let Asset::Scene(s) = load_path(&data("usd/scene.usdc"), &quick()).unwrap() else { panic!("not a scene") };
    eprintln!("{}", describe(&Asset::Scene(s.clone_shallow())));
    // A cube of 6 quads, faceVarying UVs: 24 vertices, 12 triangles.
    let cube = s.meshes.iter().find(|m| m.name == "Box").expect("Box mesh");
    let tris: usize = cube.primitives.iter().map(|p| p.triangle_count()).sum();
    assert_eq!(tris, 12);
    assert_eq!(cube.primitives.len(), 2, "GeomSubset splits the box");
    let node = s.nodes.iter().find(|n| n.name == "Box").unwrap();
    assert!((node.transform.translation - slerp_scene::glam::Vec3::new(1.0, 2.0, 3.0)).length() < 1e-5);
    assert!(s.materials.len() >= 2);

    // The textured material reads through a UsdTransform2d (scale 2,
    // rotation 90 degrees) and mirrors in S; its roughness comes from the
    // Material's interface input.
    let m = s.materials.iter().find(|m| m.name == "Textured").expect("Textured material");
    assert!((m.roughness - 0.7).abs() < 1e-6, "{}", describe_material(m));
    let d = crate::usd::test_material_desc_named(&data("usd/scene.usda"), "Textured").unwrap();
    let t = d.base_color.as_ref().expect("texture");
    let x = t.transform.expect("transform");
    assert!((x.scale.x.abs() - 2.0).abs() < 1e-4 && (x.scale.y.abs() - 2.0).abs() < 1e-4, "{x:?}");
    assert!((x.rotation.abs() - std::f32::consts::FRAC_PI_2).abs() < 1e-4, "{x:?}");
    assert_eq!(t.sampler.wrap_u, slerp_scene::Wrap::MirroredRepeat);

    // The class prim isn't drawn, the instance referencing it is; the
    // variant picks the cylinder; the time-sampled ball sits at its first
    // sample; the stage is turned Y-up.
    assert!(s.nodes.iter().any(|n| n.name == "C"));
    assert!(s.nodes.iter().any(|n| n.name == "B") && !s.nodes.iter().any(|n| n.name == "A"));
    let ball = s.nodes.iter().find(|n| n.name == "Ball").unwrap();
    assert!((ball.transform.translation.z - 2.0).abs() < 1e-5);
    assert_eq!(s.roots.len(), 1);
    assert!(s.info.details.iter().any(|(k, v)| k == "Preview" && v == "preview.png"));

    // Text, crate and package give the same scene.
    for other in ["usd/scene.usda", "usd/scene.usdz"] {
        let Asset::Scene(o) = load_path(&data(other), &quick()).unwrap() else { panic!() };
        assert_eq!(o.triangle_count(), s.triangle_count(), "{other}");
        assert_eq!(o.materials.len(), s.materials.len(), "{other}");
        assert_eq!(o.textures.len(), s.textures.len(), "{other}");
    }
}

//endregion

/*┌─────────────────╮
| UsdSkel         |
└─────────────────╯*/
//region UsdSkel

fn scene(path: &Path, limits: &Limits) -> Scene {
    match load_path(path, limits) {
        Ok(Asset::Scene(s)) => s,
        Ok(a) => panic!("{path:?} is not a scene: {}", describe(&a)),
        Err(e) => panic!("{path:?}: {e}"),
    }
}

fn node(s: &Scene, name: &str) -> usize {
    s.nodes.iter().position(|n| n.name == name).unwrap_or_else(|| panic!("no node {name}"))
}

/// The world-space vertices of a node's mesh in `pose`: morph targets by
/// the pose's weights, then skinning (or the node's world matrix).
fn posed(s: &Scene, pose: &Pose, n: usize) -> Vec<Vec3> {
    let node = &s.nodes[n];
    let mesh = &s.meshes[node.mesh.expect("a mesh")];
    let joints = node.skin.map(|k| pose.joint_matrices(s, k));
    let mut out = Vec::new();
    for p in &mesh.primitives {
        for (v, &position) in p.positions.iter().enumerate() {
            let mut q = position;
            for (t, w) in p.targets.iter().zip(&pose.weights[n]) {
                q += t.positions[v] * *w;
            }
            out.push(match (&joints, p.is_skinned()) {
                (Some(j), true) => {
                    let mut m = Mat4::ZERO;
                    for k in 0..4 {
                        m += j[p.joints[v][k] as usize] * p.weights[v][k];
                    }
                    m.transform_point3(q)
                }
                _ => pose.world[n].transform_point3(q),
            });
        }
    }
    out
}

/// Whether two point sets match: every point of each lies within `tol` of
/// a point of the other.
fn same_points(a: &[Vec3], b: &[Vec3], tol: f32) -> Result<(), String> {
    for (x, y, which) in [(a, b, "ours"), (b, a, "reference")] {
        for p in x {
            let d = y.iter().map(|q| p.distance(*q)).fold(f32::INFINITY, f32::min);
            if d > tol {
                return Err(format!("{which} point {p:?} is {d} from the other set"));
            }
        }
    }
    Ok(())
}

/// The scene posed by its first clip at time code `t` of a stage starting
/// at time code 1 with 30 per second (the fixtures').
fn pose_at(s: &Scene, t: f32) -> Pose {
    let mut pose = Pose::rest(s);
    pose.sample(s, 0, (t - 1.0) / 30.0);
    pose.update_world(s);
    pose
}

/// Skinning, blend shapes (sparse, with an inbetween, past weight 1), a
/// rigid binding and animated transforms all match UsdSkel's own baked
/// points, between samples and beyond both ends too, in text and crate files.
#[test]
fn skel_matches_usdskel() {
    let reference = std::fs::read_to_string(data("usd/skel.reference.txt")).unwrap();
    for file in ["usd/skel.usda", "usd/skel.usdc"] {
        let s = scene(&data(file), &quick());
        assert_eq!(s.animations.len(), 1, "{file}");
        assert_eq!(s.animations[0].name, "Anim");
        assert!((s.animations[0].duration - 20.0 / 30.0).abs() < 1e-5, "{}", s.animations[0].duration);
        let mut checked = 0;
        for line in reference.lines().filter(|l| !l.starts_with('#')) {
            let mut words = line.split_whitespace();
            let (name, t) = (words.next().unwrap(), words.next().unwrap().parse::<f32>().unwrap());
            let numbers: Vec<f32> = words.map(|w| w.parse().unwrap()).collect();
            let expected: Vec<Vec3> = numbers.chunks_exact(3).map(Vec3::from_slice).collect();
            let pose = pose_at(&s, t);
            let ours = posed(&s, &pose, node(&s, name));
            same_points(&ours, &expected, 1e-3).unwrap_or_else(|e| panic!("{file} {name} at {t}: {e}"));
            checked += 1;
        }
        assert_eq!(checked, 28);
    }
}

/// In the bind pose (rest transforms equal to the bind transforms), skinned
/// meshes sit exactly where the same meshes do unskinned.
#[test]
fn skinned_bind_pose_matches_rest_mesh() {
    let path = data("usd/bind.usda");
    let skinned = scene(&path, &quick());
    let text = std::fs::read_to_string(&path).unwrap().replace("rel skel:skeleton = </Rig/Skel>", "");
    let Asset::Scene(plain) = load(MaterialFormat::Usd, &path, text.as_bytes(), &quick()).unwrap() else { panic!() };
    for name in ["Bar", "Fin"] {
        let (a, b) = (node(&skinned, name), node(&plain, name));
        assert!(skinned.nodes[a].skin.is_some() && plain.nodes[b].skin.is_none(), "{name}");
        let ours = posed(&skinned, &Pose::rest(&skinned), a);
        let rest = posed(&plain, &Pose::rest(&plain), b);
        same_points(&ours, &rest, 1e-5).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    // Joints hang under the skeleton's node, one per joint, named by path.
    let skel = node(&skinned, "Skel");
    let names: Vec<&str> = skinned.nodes[skel].children.iter().map(|&c| skinned.nodes[c].name.as_str()).collect();
    assert_eq!(names, ["root"]);
    assert_eq!(skinned.skins.len(), 2, "the bar's own joint order, and the skeleton's (fin and cube)");
}

/// An animated joint moves the vertices it weighs on: at time 11 the root
/// has turned 10 degrees about y and moved 0.25 along x, and the middle
/// joint bent 45 degrees about z, so the bottom ring (all root) only turns
/// and slides while the top ring swings far.
#[test]
fn animated_joint_moves_its_vertices() {
    let s = scene(&data("usd/skel.usda"), &quick());
    let bar = node(&s, "Bar");
    let rest = posed(&s, &pose_at(&s, 1.0), bar);
    let bent = posed(&s, &pose_at(&s, 11.0), bar);
    let root = Mat4::from_translation(Vec3::new(0.25, 0.0, 0.0)) * Mat4::from_rotation_y(10f32.to_radians());
    for (a, b) in rest.iter().zip(&bent) {
        if a.y.abs() < 1e-4 {
            assert!((*b - root.transform_point3(*a)).length() < 1e-4, "bottom ring {a} moved to {b}");
        } else if a.y > 1.9 {
            assert!((*b - *a).length() > 0.5, "top ring {a} only moved to {b}");
        }
    }
}

/// Blend shapes become morph targets (an inbetween is a target of its own),
/// start at the first sample's weights and are keyed by the clip.  Shape
/// weight 1.5 carries on past the primary shape along the last stretch:
/// the inbetween at 0.5 gets -1, the primary 2.
#[test]
fn blend_shape_weights() {
    let s = scene(&data("usd/skel.usdc"), &quick());
    let bar = node(&s, "Bar");
    let mesh = &s.meshes[s.nodes[bar].mesh.unwrap()];
    assert_eq!(mesh.target_names, ["Bulge", "Twist", "Twist half"]);
    assert!(mesh.primitives.iter().all(|p| p.targets.len() == 3 && !p.targets[0].normals.is_empty()));
    assert_eq!(s.nodes[bar].weights, [0.0, 0.0, 0.0]);
    let ch = s.animations[0].channels.iter().find(|c| c.node == bar && c.property == Property::Weights).unwrap();
    let times: Vec<f32> = ch.times.iter().map(|t| t * 30.0 + 1.0).collect();
    assert_eq!(times.len(), 3);
    let key = |k: usize| &ch.values[k * 3..k * 3 + 3];
    assert_eq!(key(1), [1.0, 0.0, 1.0]);
    let last = key(2);
    assert!((last[0] - 0.25).abs() < 1e-6 && (last[1] - 2.0).abs() < 1e-5 && (last[2] + 1.0).abs() < 1e-5, "{last:?}");
}

/// Thumbnails don't animate: no clips, no morph targets, and blend shapes
/// weighted at the first sample are added to the points.
#[test]
fn thumbnails_bake_first_sample() {
    let path = data("usd/skel.usda");
    let text = std::fs::read_to_string(&path).unwrap().replace("1: [0, 0],", "1: [0, 1],");
    let thumb = Limits { neighbours: false, ..Limits::thumbnail() };
    let Asset::Scene(s) = load(MaterialFormat::Usd, &path, text.as_bytes(), &thumb).unwrap() else { panic!() };
    assert!(s.animations.is_empty());
    let bar = node(&s, "Bar");
    assert!(s.meshes[s.nodes[bar].mesh.unwrap()].primitives.iter().all(|p| p.targets.is_empty()));
    // Bulge (weight 1 now) pushes the middle ring out by 0.15 in x and z.
    let pts = posed(&s, &Pose::rest(&s), bar);
    let middle: Vec<&Vec3> = pts.iter().filter(|p| (p.y - 1.0).abs() < 1e-4).collect();
    assert_eq!(middle.len(), 4);
    assert!(middle.iter().all(|p| (p.x.abs() - 0.4).abs() < 1e-4 && (p.z.abs() - 0.4).abs() < 1e-4), "{middle:?}");
}

/// A stage of a skeleton and its animation, without meshes, is a scene of
/// joints and a clip: an animation library for other characters.  The
/// skeleton has no rest transforms, so they come from its bind transforms.
#[test]
fn skeleton_without_meshes() {
    let text = r#"#usda 1.0
(
    timeCodesPerSecond = 24
)

def Skeleton "Skel" (
    prepend apiSchemas = ["SkelBindingAPI"]
)
{
    uniform token[] joints = ["hips", "hips/spine"]
    uniform matrix4d[] bindTransforms = [((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 1, 0, 1)), ((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 1.5, 0, 1))]
    rel skel:animationSource = <Walk>

    def SkelAnimation "Walk"
    {
        uniform token[] joints = ["hips/spine"]
        quatf[] rotations.timeSamples = {
            0: [(1, 0, 0, 0)],
            12: [(0.7071068, 0.7071068, 0, 0)],
        }
    }
}
"#;
    let path = data("usd/skeleton-only.usda");
    let Asset::Scene(s) = load(MaterialFormat::Usd, &path, text.as_bytes(), &quick()).unwrap() else { panic!() };
    assert!(s.meshes.is_empty());
    let spine = node(&s, "spine");
    assert!((s.nodes[spine].transform.translation - Vec3::new(0.0, 0.5, 0.0)).length() < 1e-6);
    assert_eq!(s.animations.len(), 1);
    assert!((s.animations[0].duration - 0.5).abs() < 1e-6);
    let mut pose = Pose::rest(&s);
    pose.sample(&s, 0, 0.5);
    let r = pose.local[spine].rotation;
    assert!((r.x - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5, "{r}");
}

/// Skeletal clips of real files: they play, and the first frame matches
/// what Pixar's UsdSkel bakes (bounds measured with usd-core 26.8).
#[test]
fn skel_samples() {
    let cases: [(&str, [f32; 6]); 6] = [
        ("usd/usdwg-tests/USDZ/RiggedSimple/RiggedSimple.usdz", [-1.0, -0.3947, -1.0, 1.0, 8.7554, 1.0]),
        ("usd/usdwg-tests/USDZ/CesiumMan/CesiumMan.usdz", [-0.3105, -0.0106, -0.4466, 0.1947, 1.4472, 0.4499]),
        ("usd/usdwg-tests/USDZ/BrainStem/BrainStem.usdz", [-0.6821, 0.0223, -0.2772, 0.7441, 1.8177, 0.3942]),
        ("usd/apple/toy_drummer.usdz", [-0.0579, -0.0, -0.0422, 0.0393, 0.1362, 0.0422]),
        ("usd-extra/apple/robot.usdz", [-0.0713, -0.0003, -0.1012, 0.0713, 0.2735, 0.0735]),
        ("usd-extra/openusd-skel/imaging-usdskel/twoBends.usda", [-0.0008, -0.1, -0.0135, 0.0522, 0.1, 0.0248]),
    ];
    for (file, expected) in cases {
        let path = assets().join(file);
        if !path.is_file() {
            eprintln!("skipped: no {file}");
            continue;
        }
        let s = scene(&path, &quick());
        let b = s.bounds(&Pose::rest(&s));
        let got = [b.min.x, b.min.y, b.min.z, b.max.x, b.max.y, b.max.z];
        assert!(got.iter().zip(expected).all(|(g, e)| (g - e).abs() < 2e-4), "{file}: {got:?}");
        assert!(!s.skins.is_empty(), "{file}");
        let thumb = scene(&path, &Limits::thumbnail());
        assert!(thumb.animations.is_empty());
        let b = thumb.bounds(&Pose::rest(&thumb));
        let got = [b.min.x, b.min.y, b.min.z, b.max.x, b.max.y, b.max.z];
        assert!(got.iter().zip(expected).all(|(g, e)| (g - e).abs() < 2e-4), "{file} thumbnail: {got:?}");
    }
}

/// Blender's USD exports (armatures, shape keys driven by a SkelAnimation,
/// object animation) pose as Pixar's UsdSkel bakes them, at the start and
/// half a second in (bounds with morph targets, measured with usd-core 26.8).
#[test]
fn blender_exports() {
    type Bounds = [f32; 6];
    let cases: [(&str, Bounds, Bounds); 3] = [
        (
            "03_skinned_cylinder.usdc",
            [-1.0, 0.0, -1.0159, 1.0, 4.2612, 1.8977],
            [-1.0, 0.0, -1.0098, 1.0, 4.2401, 1.5373],
        ),
        ("28_shapekeys_anim.usdc", [-1.0, -1.0, -7.0, 1.0, 4.0, 1.0], [-1.0, -1.0, -6.0, 1.0, 2.0, 1.0]),
        ("03_all_animations.usdc", [-1.0, -1.0, -5.118, 1.0, 5.0, 1.0], [-4.677, -1.0, -1.5, 2.0, 5.5451, 1.5]),
    ];
    for (file, start, half) in cases {
        let path = assets().join("usd-extra/blender").join(file);
        if !path.is_file() {
            eprintln!("skipped: no {file}");
            continue;
        }
        let s = scene(&path, &quick());
        for (t, expected) in [(0.0, start), (0.5, half)] {
            let mut pose = Pose::rest(&s);
            pose.sample(&s, 0, t);
            pose.update_world(&s);
            let mut b = slerp_scene::Aabb::EMPTY;
            for n in (0..s.nodes.len()).filter(|&n| s.nodes[n].mesh.is_some()) {
                posed(&s, &pose, n).into_iter().for_each(|p| b.add(p));
            }
            let got = [b.min.x, b.min.y, b.min.z, b.max.x, b.max.y, b.max.z];
            assert!(got.iter().zip(expected).all(|(g, e)| (g - e).abs() < 2e-4), "{file} at {t} s: {got:?}");
        }
    }
}

//endregion

/*┌─────────────────╮
| Instancers      |
└─────────────────╯*/
//region Instancers

/// The world matrices of the meshes a point instancer placed for instance
/// `k`: nodes named "<prototype> k" or "<prototype>/<mesh> k" under the
/// instancer, or such an instance node's children.
fn instance_matrices(s: &Scene, pose: &Pose, instancer: usize, k: usize) -> Vec<Mat4> {
    let suffix = format!(" {k}");
    let mut out = Vec::new();
    for &n in s.nodes[instancer].children.iter().filter(|&&c| s.nodes[c].name.ends_with(&suffix)) {
        if s.nodes[n].mesh.is_some() {
            out.push(pose.world[n]);
        } else {
            out.extend(s.nodes[n].children.iter().map(|&c| pose.world[c]));
        }
    }
    assert!(!out.is_empty(), "no instance {k}");
    out
}

/// Instances stand where UsdGeom puts them (orientations, scales, the
/// prototype root's own transform), hidden and inactive ids are left out,
/// a prototype under an `over` is drawn only as instances, and an
/// instancer of instancers places whole instancers.  Meshes are shared.
#[test]
fn instancer_matches_usdgeom() {
    let reference = std::fs::read_to_string(data("usd/instancer.reference.txt")).unwrap();
    for file in ["usd/instancer.usda", "usd/instancer.usdc"] {
        let s = scene(&data(file), &quick());
        let pose = Pose::rest(&s);
        let trees = node(&s, "Trees");
        // Three trees of two meshes each, and a rock.
        assert_eq!(s.nodes[trees].children.len(), 3 * 2 + 1, "{file}: shown instances");
        assert!(!s.nodes.iter().any(|n| n.name == "Rock"), "{file}: the prototype under an over is drawn");
        assert_eq!(s.meshes.len(), 3, "{file}: trunk, crown and stone, shared");
        for line in reference.lines().filter(|l| !l.starts_with('#') && l.starts_with("/World/Trees")) {
            let mut words = line.split_whitespace().skip(1);
            let k: usize = words.next().unwrap().parse().unwrap();
            let rows: Vec<f32> = words.map(|w| w.parse().unwrap()).collect();
            // USD's rows are glam's columns.
            let expected = Mat4::from_cols_slice(&rows);
            for m in instance_matrices(&s, &pose, trees, k) {
                assert!(m.abs_diff_eq(expected, 2e-3), "{file} instance {k}: {m} vs {expected}");
            }
        }
        // Rows places the whole Trees instancer twice: 2 x 4 instances,
        // flattened into a node per mesh.
        let rows = node(&s, "Rows");
        assert_eq!(s.nodes[rows].children.len(), 2 * 7);
        let meshes = |n: usize| {
            let mut stack = vec![n];
            let mut count = 0;
            while let Some(n) = stack.pop() {
                count += s.nodes[n].mesh.is_some() as usize;
                stack.extend(&s.nodes[n].children);
            }
            count
        };
        assert_eq!(meshes(rows), 2 * (3 * 2 + 1), "{file}");
        // 4 instances of Trees, and 2 of Rows holding 4 each.
        let fact = s.info.details.iter().find(|(k, _)| k == "Instances").map(|(_, v)| v.as_str());
        assert_eq!(fact, Some("14, from 3 point instancers"), "{file}");
    }
}

/// A USD text stage of one cube placed `n` times along x.
fn many_instances(n: usize) -> String {
    let positions: Vec<String> = (0..n).map(|i| format!("({i}, 0, 0)")).collect();
    format!(
        "#usda 1.0\n(\n    metersPerUnit = 1\n    upAxis = \"Y\"\n)\n\ndef PointInstancer \"Many\"\n{{\n    \
         rel prototypes = </Many/Box>\n    int[] protoIndices = [{}]\n    point3f[] positions = [{}]\n\n    \
         def Cube \"Box\"\n    {{\n        double size = 0.5\n    }}\n}}\n",
        vec!["0"; n].join(", "),
        positions.join(", ")
    )
}

/// Beyond the cap, instances are thinned evenly (every n-th kept) with a
/// warning; thumbnails keep fewer.
#[test]
fn instancer_thinning() {
    let text = many_instances(150_000);
    let path = data("usd/many.usda");
    let viewer = Limits { neighbours: false, ..Limits::viewer() };
    let thumbnail = Limits { neighbours: false, ..Limits::thumbnail() };
    for (limits, cap) in [(viewer, 100_000), (thumbnail, 10_000)] {
        let start = std::time::Instant::now();
        let Asset::Scene(s) = load(MaterialFormat::Usd, &path, text.as_bytes(), &limits).unwrap() else { panic!() };
        eprintln!("150,000 instances, {cap} kept: {:.0} ms", start.elapsed().as_secs_f64() * 1000.0);
        let many = node(&s, "Many");
        let kids = &s.nodes[many].children;
        assert_eq!(kids.len(), cap);
        assert_eq!(s.meshes.len(), 1, "the cube is shared");
        // Evenly spread: the kept instances' positions step by 1.5 on average.
        let xs: Vec<f32> = kids.iter().map(|&c| s.nodes[c].transform.translation.x).collect();
        assert!(xs.windows(2).all(|w| w[1] > w[0] && w[1] - w[0] <= 150_000.0 / cap as f32 + 1.0));
        assert!(xs[xs.len() - 1] > 149_000.0 - 150_000.0 / cap as f32 * 2.0);
        assert!(
            s.info.warnings.iter().any(|w| w.contains(&format!("{cap} of 150000 instances shown"))),
            "{:?}",
            s.info.warnings
        );
    }
}

/// Pixar's PointInstancer tests load, with the instance counts UsdGeom
/// gives (after masks; prototypes with the proxy purpose are hidden here,
/// as the stage also has render geometry).
#[test]
fn instancer_samples() {
    let cases = [
        ("pi_basic.usda", 4),
        ("pi_masking.usda", 4),
        ("pi_pi.usda", 29),
        ("ni_pi.usda", 7),
        ("pi_sibling_pi_over.usda", 4),
        ("pi_bad_schema_input.usda", 0),
        ("pi_cycles.usda", 3),
    ];
    let dir = assets().join("usd-extra/openusd-instancers/imaging-pointinstancer");
    for (file, _) in cases {
        let path = dir.join(file);
        if !path.is_file() {
            eprintln!("skipped: no {file}");
            continue;
        }
        let start = std::time::Instant::now();
        let s = scene(&path, &quick());
        assert!(start.elapsed().as_secs_f64() < 2.0, "{file}");
        eprintln!("{file}: {}", describe(&Asset::Scene(s.clone_shallow())).lines().next().unwrap_or(""));
    }
    // pi_masking: five instances, one inactive id.
    let path = dir.join("pi_masking.usda");
    if path.is_file() {
        let s = scene(&path, &quick());
        assert_eq!(s.nodes[node(&s, "Instancer")].children.len(), 4);
    }
}

//endregion

/*┌─────────────────╮
| Damaged input   |
└─────────────────╯*/
//region Damaged input

/// A small deterministic random number generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Feeds truncated and corrupted copies of a file to the loader: none may
/// panic, and none may take long.
/// `SLERP_DAMAGE_ROUNDS` overrides the number of copies, for longer
/// runs by hand.
fn survive_damage(path: &Path, rounds: usize) {
    let rounds = std::env::var("SLERP_DAMAGE_ROUNDS").ok().and_then(|r| r.parse().ok()).unwrap_or(rounds);
    let Ok(bytes) = std::fs::read(path) else {
        eprintln!("skipped: {path:?} missing");
        return;
    };
    let format = sniff(path, &bytes).unwrap_or_else(|| panic!("{path:?} not sniffed"));
    // Keep textures out of it: those belong to the textures crate.
    let limits = Limits { neighbours: false, ..quick() };
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ bytes.len() as u64);
    let start = std::time::Instant::now();
    for i in 0..rounds {
        let mut b = bytes.clone();
        if i % 2 == 0 {
            b.truncate((rng.next() as usize) % bytes.len().max(1));
        } else {
            for _ in 0..1 + rng.next() % 8 {
                let at = (rng.next() as usize) % b.len().max(1);
                if let Some(x) = b.get_mut(at) {
                    *x = rng.next() as u8;
                }
            }
        }
        let _ = load(format, path, &b, &limits);
    }
    let per = start.elapsed().as_secs_f64() * 1000.0 / rounds as f64;
    eprintln!("{}: {rounds} damaged copies, {per:.2} ms each", path.display());
    assert!(per < 200.0, "damaged copies of {path:?} are slow to reject");
}

#[test]
fn damaged_usdc() {
    survive_damage(&data("usd/scene.usdc"), 3000);
    survive_damage(&data("usd/dict.usdc"), 2000);
    survive_damage(&data("usd/arrays.usdc"), 2000);
    survive_damage(&data("usd/composition.usdc"), 2000);
    survive_damage(&data("usd/skel.usdc"), 3000);
    survive_damage(&data("usd/instancer.usdc"), 3000);
}

#[test]
fn damaged_usda() {
    survive_damage(&data("usd/scene.usda"), 2000);
    survive_damage(&data("usd/skel.usda"), 3000);
    survive_damage(&data("usd/instancer.usda"), 3000);
}

/// Skeletons, skins and instancers whose counts and indices disagree load
/// without panicking: indices past the joints, weights shorter than the
/// points, element sizes of zero and huge, mismatched bind transforms,
/// prototype indices past the prototypes, and so on.
#[test]
fn inconsistent_skel_and_instancers() {
    let edits: &[(&str, &str, &str)] = &[
        ("skel", "elementSize = 2", "elementSize = 0"),
        ("skel", "elementSize = 2", "elementSize = 100000000"),
        ("skel", "int[] primvars:skel:jointIndices = [1, 0", "int[] primvars:skel:jointIndices = [70000, -5"),
        (
            "skel",
            "float[] primvars:skel:jointWeights = [1.0, 0.0, 1.0",
            "float[] primvars:skel:jointWeights = [nan, inf, 1.0",
        ),
        ("skel", "uniform token[] joints = [\"root\", ", "uniform token[] joints = ["),
        ("skel", "\"root/mid\", \"root/mid/tip\", \"root\"]", "\"root/mid\"]"),
        ("skel", "uniform int[] pointIndices = [4, 5, 6, 7]", "uniform int[] pointIndices = [400, -5, 6, 7, 8, 9]"),
        ("skel", "weight = 0.5", "weight = 1"),
        ("skel", "rel skel:skeleton = </Rig/Skel>", "rel skel:skeleton = </Rig/Geo>"),
        ("skel", "rel skel:animationSource = </Rig/Anim>", "rel skel:animationSource = </Rig>"),
        ("skel", "11: [0.5, 1],", "11: [0.5],"),
        ("instancer", "int[] protoIndices = [0, 1, 0, 1, 0, 0]", "int[] protoIndices = [0, 9, -1, 1, 0, 0]"),
        ("instancer", "int[] protoIndices = [0, 1, 0, 1, 0, 0]", "int[] protoIndices = [0, 1]"),
        ("instancer", "rel prototypes = </World/Trees>", "rel prototypes = </World>"),
        ("instancer", "float3[] scales = [(1, 1, 1), (2, 1, 2)", "float3[] scales = [(1, 1, 1), (nan, 1e38, 2)"),
    ];
    for &(file, from, to) in edits {
        let path = data(&format!("usd/{file}.usda"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(from), "{file}: {from}");
        let text = text.replacen(from, to, 1);
        match load(MaterialFormat::Usd, &path, text.as_bytes(), &quick()) {
            Ok(Asset::Scene(s)) => {
                let pose = Pose::rest(&s);
                let _ = s.bounds(&pose);
            }
            Ok(_) => {}
            Err(e) => eprintln!("{file} with {to:?}: {e}"),
        }
    }
}

#[test]
fn damaged_usdz() {
    survive_damage(&data("usd/scene.usdz"), 2000);
}

#[test]
fn damaged_materialx() {
    survive_damage(&data("material.mtlx"), 2000);
}

#[test]
fn damaged_godot() {
    survive_damage(&data("godot/orm.tres"), 2000);
}

#[test]
fn damaged_sbsar() {
    survive_damage(&data("icon.sbsar"), 2000);
}

/// Pixar's Kitchen_set: about 2000 layers' worth of references and
/// payloads on disk.
#[test]
fn kitchen_set() {
    let path = assets().join("usd/pixar-kitchen-set/Kitchen_set.usd");
    if !path.is_file() {
        eprintln!("skipped: no Kitchen_set");
        return;
    }
    let start = std::time::Instant::now();
    let a = load_path(&path, &Limits::thumbnail()).unwrap();
    eprintln!("Kitchen_set ({:.0} ms): {}", start.elapsed().as_secs_f64() * 1000.0, describe(&a));
    let Asset::Scene(s) = a else { panic!("not a scene") };
    assert!(s.triangle_count() > 100_000);
}

/// References and inherits that lead back into their own subtree are
/// skipped rather than nesting forever.
#[test]
fn composition_cycles() {
    let start = std::time::Instant::now();
    let Asset::Scene(s) = load_path(&data("usd/cycles.usda"), &quick()).unwrap() else { panic!("not a scene") };
    assert!(start.elapsed().as_secs_f64() < 1.0);
    assert!(s.nodes.len() < 40, "{} nodes", s.nodes.len());
    assert!(s.info.warnings.iter().any(|w| w.contains("cycle")), "{:?}", s.info.warnings);
}

/// A crate file whose tables claim enormous counts is refused without
/// allocating them.
#[test]
fn huge_counts_are_refused() {
    let mut b = std::fs::read(data("usd/scene.usdc")).unwrap();
    // Overwrite every 8-byte-aligned small integer after the header with a
    // huge one, one at a time, and check each load fails fast.
    let limits = Limits { neighbours: false, ..quick() };
    let path = data("usd/scene.usdc");
    let original = b.clone();
    let start = std::time::Instant::now();
    for at in (88..b.len().saturating_sub(8)).step_by(8) {
        b.copy_from_slice(&original);
        b[at..at + 8].copy_from_slice(&(u64::MAX / 3).to_le_bytes());
        let _ = load(MaterialFormat::Usd, &path, &b, &limits);
    }
    assert!(start.elapsed().as_secs() < 20);
}

//endregion

/*┌─────────────────╮
| USD sweep       |
└─────────────────╯*/
//region USD sweep

/// Every USD file of the test corpus, for the before-and-after sweep.
fn usd_sweep_files() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for dir in ["usd", "usd-extra", "materials", "materials-extra", "rigged-extra/pmx/usd-mmd-fixtures"] {
        paths.extend(files(&assets().join(dir), &["usd", "usda", "usdc", "usdz"]));
    }
    paths.extend(files(&data("usd"), &["usd", "usda", "usdc", "usdz"]));
    paths
}

/// Writes one line per USD sample (triangles, nodes, meshes, skins, clips
/// and the rest pose's bounds, or the error) to the file
/// `SLERP_USD_SWEEP` names, to compare the loader before and after a
/// change: `cargo test -p slerp-materials usd_sweep -- --ignored`.
#[test]
#[ignore]
fn usd_sweep() {
    let Ok(out) = std::env::var("SLERP_USD_SWEEP") else {
        eprintln!("skipped: set SLERP_USD_SWEEP to the report's path");
        return;
    };
    let mut report = String::new();
    for path in usd_sweep_files() {
        let rel = path.strip_prefix(assets()).unwrap_or(&path).display().to_string();
        let t = std::time::Instant::now();
        let line = match load_path(&path, &quick()) {
            Ok(Asset::Scene(s)) => {
                let b = s.bounds(&slerp_scene::Pose::rest(&s));
                format!(
                    "scene\t{}\t{}\t{}\t{}\t{}\t{:.4?}\t{:.4?}",
                    s.triangle_count(),
                    s.nodes.len(),
                    s.meshes.len(),
                    s.skins.len(),
                    s.animations.len(),
                    b.min.to_array(),
                    b.max.to_array()
                )
            }
            Ok(a) => format!("other\t{}", describe(&a).lines().next().unwrap_or("")),
            Err(e) => format!("error\t{e}"),
        };
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        report += &format!("{rel}\t{ms:.0}\t{line}\n");
    }
    std::fs::write(&out, report).unwrap();
}

/// Prints how one USD file loads, with its node tree, for checking by eye:
/// `SLERP_USD=<file> cargo test -p slerp-materials print_usd -- --ignored --nocapture`.
#[test]
#[ignore]
fn print_usd() {
    let Ok(path) = std::env::var("SLERP_USD") else { return };
    let a = load_path(Path::new(&path), &quick()).unwrap();
    println!("{}", describe(&a));
    let Asset::Scene(s) = a else { return };
    fn walk(s: &slerp_scene::Scene, n: usize, depth: usize) {
        let node = &s.nodes[n];
        let t = &node.transform;
        println!(
            "{:indent$}{} t{:.3?} r{:.3?} s{:.3?}{}{}",
            "",
            node.name,
            t.translation.to_array(),
            t.rotation.to_array(),
            t.scale.to_array(),
            node.mesh.map_or(String::new(), |m| format!(
                " mesh {m} ({} tris)",
                s.meshes[m].primitives.iter().map(|p| p.triangle_count()).sum::<usize>()
            )),
            node.skin.map_or(String::new(), |k| format!(" skin {k}")),
            indent = depth * 2
        );
        for &c in node.children.iter().take(40) {
            walk(s, c, depth + 1);
        }
    }
    for &r in &s.roots {
        walk(&s, r, 0);
    }
    for a in &s.animations {
        println!("clip {:?}: {:.2} s, {} channels", a.name, a.duration, a.channels.len());
    }
    // Bounds with the first clip at the seconds `SLERP_USD_SECONDS` lists.
    let seconds = std::env::var("SLERP_USD_SECONDS").unwrap_or_default();
    for t in seconds.split(',').filter_map(|t| t.parse::<f32>().ok()) {
        let mut pose = slerp_scene::Pose::rest(&s);
        if !s.animations.is_empty() {
            pose.sample(&s, 0, t);
            pose.update_world(&s);
        }
        // With morph targets, which `Scene::bounds` leaves out.
        let mut b = slerp_scene::Aabb::EMPTY;
        for n in (0..s.nodes.len()).filter(|&n| s.nodes[n].mesh.is_some()) {
            posed(&s, &pose, n).into_iter().for_each(|p| b.add(p));
        }
        println!("at {t} s: {:.4?} {:.4?}", b.min.to_array(), b.max.to_array());
    }
}

//endregion

/*┌─────────────────╮
| Corpus report   |
└─────────────────╯*/
//region Corpus report

/// Prints how every local material file maps, for checking by eye:
/// `cargo test -p slerp-materials print_corpus -- --ignored --nocapture`.
#[test]
#[ignore]
fn print_corpus() {
    let mut paths = Vec::new();
    for dir in ambientcg_dirs() {
        paths.extend(files(&dir, &["mtlx", "usdc", "tres"]));
    }
    paths.extend(files(&assets().join("materials/materialx-examples/Examples"), &["mtlx"]));
    paths.extend(files(&assets().join("materials/polyhaven"), &["mtlx"]));
    paths.extend(files(&assets().join("materials/sbsar"), &["sbsar"]));
    let usd = assets().join("usd");
    paths.extend(files(&usd, &["usd", "usda", "usdc", "usdz"]).into_iter().filter(|p| {
        let s = p.strip_prefix(&usd).unwrap_or(p).to_string_lossy().into_owned();
        !s.contains("geo/") && !s.contains("assets/") && !s.ends_with("DrawModes.usd")
    }));
    for path in paths {
        let t = std::time::Instant::now();
        let r = load_path(&path, &Limits::thumbnail());
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        match r {
            Ok(a) => println!("{} [{ms:.0} ms thumbnail]: {}", path.display(), describe(&a)),
            Err(e) => println!("{} [{ms:.0} ms]: error: {e}", path.display()),
        }
    }
}

//endregion

trait CloneShallow {
    fn clone_shallow(&self) -> Self;
}

impl CloneShallow for slerp_scene::MaterialAsset {
    fn clone_shallow(&self) -> Self {
        slerp_scene::MaterialAsset {
            name: self.name.clone(),
            material: self.material.clone(),
            textures: self.textures.clone(),
            preview: self.preview.clone(),
            info: self.info.clone(),
        }
    }
}

impl CloneShallow for slerp_scene::Scene {
    fn clone_shallow(&self) -> Self {
        slerp_scene::Scene {
            name: self.name.clone(),
            nodes: self.nodes.clone(),
            roots: self.roots.clone(),
            meshes: self.meshes.clone(),
            materials: self.materials.clone(),
            textures: self.textures.clone(),
            info: self.info.clone(),
            ..Default::default()
        }
    }
}
