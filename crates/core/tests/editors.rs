//! Editor files through core's entry points: LDtk levels as file parts,
//! Blockbench models as scenes to render, and the preview pictures of
//! 3ds Max, SketchUp, Cinema 4D and ZBrush files as finished thumbnails.
//! Uses `local/assets/editors-extra/`; samples that aren't there are
//! skipped with a note.

use slerp_core::{Kind, Prepared};
use slerp_scene::{Asset, Limits};
use std::path::{Path, PathBuf};

fn asset(rel: &str) -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/editors-extra").join(rel);
    if p.is_file() {
        Some(p)
    } else {
        eprintln!("skipped, not downloaded: {rel}");
        None
    }
}

#[test]
fn ldtk_levels_are_parts() {
    let Some(path) = asset("ldtk/WorldMap_Free_layout.ldtk") else { return };
    let parts = slerp_core::parts(&path);
    assert_eq!(parts.len(), 12);
    assert_eq!(parts[0], "Entrance");
    // The whole file is the world; a part is one level.
    let Asset::Canvas(world) = slerp_core::load(&path, &Limits::viewer()).unwrap() else { panic!("not a canvas") };
    assert_eq!(world.info().format, "LDtk world");
    let Asset::Canvas(level) = slerp_core::load_part(&path, &Limits::viewer(), "Top").unwrap() else {
        panic!("not a canvas")
    };
    assert_eq!(level.info().format, "LDtk level");
    assert_eq!(level.size(), (512, 256));
    assert!(slerp_core::load_part(&path, &Limits::viewer(), "Nowhere").is_err());
    // Thumbnails show the world, on the CPU.
    let Prepared::Done(img) = slerp_core::prepare_thumbnail(&path, 256).unwrap() else { panic!("not a picture") };
    assert_eq!(img.width.max(img.height), 256);
}

#[test]
fn blockbench_models_render() {
    let Some(path) = asset("blockbench/apollo-parrot-optifine-boxuv-v4.5.bbmodel") else { return };
    let kind = slerp_core::sniff_file(&path).unwrap();
    assert!(matches!(kind, Some(Kind::Model(slerp_models::ModelFormat::Blockbench))));
    assert!(kind.unwrap().usually_3d());
    let Prepared::Render(scene) = slerp_core::prepare_thumbnail(&path, 256).unwrap() else { panic!("not a scene") };
    assert!(scene.triangle_count() > 0);
    assert!(slerp_core::parts(&path).is_empty());
}

#[test]
fn previews_are_thumbnails_without_a_gpu() {
    // 3ds Max and SketchUp files open as scenes in the viewer, but their
    // thumbnails are still the pictures they carry.
    for (rel, kind) in [
        ("max/babylon-RiggedElf.max", Kind::Max),
        ("skp/grevit-Example.skp", Kind::SketchUp),
        ("c4d/py-aep_c4d.c4d", Kind::Preview(slerp_previews::PreviewFormat::Cinema4d)),
        ("zbrush/UVU_Thalassotitan.ztl", Kind::Preview(slerp_previews::PreviewFormat::ZBrush)),
    ] {
        let Some(path) = asset(rel) else { continue };
        let found = slerp_core::sniff_file(&path).unwrap();
        assert_eq!(found, Some(kind), "{rel}");
        assert!(!found.unwrap().usually_3d());
        let start = std::time::Instant::now();
        let Prepared::Done(img) = slerp_core::prepare_thumbnail(&path, 128).unwrap() else { panic!("{rel}") };
        eprintln!("{rel}: {} x {} in {:.1} ms", img.width, img.height, start.elapsed().as_secs_f64() * 1000.0);
        assert!(img.width.max(img.height) <= 128 && img.width.min(img.height) > 0, "{rel}");
    }
    // A Cinema 4D file from before R13 has no preview: no thumbnail.
    if let Some(path) = asset("c4d/maxon_cpp_stairs.c4d") {
        assert!(slerp_core::prepare_thumbnail(&path, 128).is_err());
    }
}
