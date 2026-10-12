//! Loaders list the files they read beside the main one in
//! `Info::neighbours`, which the viewer watches to reload when one changes.
//! Uses the test corpus in `local/assets/`; samples that aren't there are
//! skipped with a note.

use slerp_scene::{Asset, Limits};
use std::path::{Path, PathBuf};

fn asset(rel: &str) -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets").join(rel);
    if p.is_file() {
        Some(p)
    } else {
        eprintln!("skipped, not downloaded: {rel}");
        None
    }
}

/// The neighbours `rel` reports, as file names.
fn neighbours(rel: &str) -> Option<Vec<String>> {
    let path = asset(rel)?;
    let asset = slerp_core::load(&path, &Limits::viewer()).unwrap_or_else(|e| panic!("{rel}: {e}"));
    let info = match &asset {
        Asset::Scene(s) => &s.info,
        Asset::Material(m) => &m.info,
        Asset::Canvas(c) => c.info(),
        _ => panic!("{rel}: unexpected asset"),
    };
    for n in &info.neighbours {
        assert!(n.is_absolute() || n.starts_with(path.parent().unwrap()), "{rel}: {}", n.display());
    }
    Some(info.neighbours.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect())
}

fn has(list: &[String], name: &str) -> bool {
    list.iter().any(|n| n.eq_ignore_ascii_case(name))
}

#[test]
fn gltf_lists_its_buffers_and_images() {
    let Some(n) = neighbours("gltf/polyhaven/Lantern_01/Lantern_01_1k.gltf") else { return };
    assert!(has(&n, "Lantern_01.bin"), "{n:?}");
    assert!(n.iter().filter(|f| f.ends_with(".jpg")).count() >= 3, "{n:?}");
}

#[test]
fn obj_lists_its_material_library_and_textures() {
    let Some(n) = neighbours("obj/assimp/spider.obj") else { return };
    assert!(has(&n, "spider.mtl"), "{n:?}");
    assert!(has(&n, "SpiderTex.jpg"), "{n:?}");
}

#[test]
fn quake_maps_list_their_lightmaps() {
    let Some(n) = neighbours("quake/avix_qcastle/avix_qcastle.bsp") else { return };
    assert!(has(&n, "avix_qcastle.lit"), "{n:?}");
}

#[test]
fn tiled_maps_list_their_tilesets() {
    let Some(n) = neighbours("tiled/tiled-examples/desert.tmx") else { return };
    assert!(has(&n, "desert.tsx") || n.iter().any(|f| f.ends_with(".png")), "{n:?}");
}

#[test]
fn materials_list_their_textures() {
    let Some(n) = neighbours("materials/ambientcg/Metal049A/Metal049A_1K-JPG.mtlx") else { return };
    assert!(n.iter().filter(|f| f.ends_with(".jpg")).count() >= 3, "{n:?}");
}
