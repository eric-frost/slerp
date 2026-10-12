//! The thumbnail rule (`slerp_core::saved_picture`): a file that carries a
//! picture its program saved gets that picture as its thumbnail, and only
//! files without one are drawn.  One test per format that was rendered
//! before the rule moved into core (3MF, slicer plate pictures, Rhino, FBX,
//! DXF, VRM), tests of the quality check (poor pictures give way to the
//! render) and the fallback, and a walk over the corpus checking that
//! every file known to carry a picture uses it.
//! Uses `local/assets/`; missing samples are skipped with a note.
//! `SLERP_TEST_OUT=<dir>` saves the thumbnails.

use slerp_core::{Kind, Prepared, carried_picture, prepare_thumbnail, saved_picture, sniff_file, thumbnail_limits};
use slerp_scene::Image8;
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

fn save(name: &str, img: &Image8) {
    let Some(dir) = std::env::var_os("SLERP_TEST_OUT") else { return };
    let file = std::fs::File::create(Path::new(&dir).join(format!("{name}.png"))).unwrap();
    let mut enc = png::Encoder::new(file, img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&img.pixels).unwrap();
}

/// Checks that `rel` sniffs as `kind`, carries a saved picture at least
/// `min` pixels on its longer side, and that its thumbnail is exactly that
/// picture, made without the GPU.
fn uses_saved_picture(rel: &str, kind: Kind, min: u32) {
    let Some(path) = asset(rel) else { return };
    assert_eq!(sniff_file(&path).unwrap(), Some(kind), "{rel}");
    let size = 256;
    let saved = saved_picture(kind, &path, size).unwrap_or_else(|| panic!("{rel}: no picture"));
    let (w, h) = saved.size();
    assert!(w.max(h) >= min, "{rel}");
    let Prepared::Done(thumb) = prepare_thumbnail(&path, size).unwrap() else { panic!("{rel} was rendered") };
    let Prepared::Done(expected) = saved.thumbnail(size) else { unreachable!() };
    assert_eq!((thumb.width, thumb.height), (expected.width, expected.height), "{rel}");
    assert!(thumb.pixels == expected.pixels, "{rel}: the thumbnail isn't the saved picture");
    assert!(thumb.pixels.chunks_exact(4).any(|p| p[3] > 0 && p[..3] != [0, 0, 0]), "{rel} is blank");
    save(&rel.replace('/', "_"), &thumb);
}

#[test]
fn three_mf_packages_use_their_thumbnail() {
    let f = Kind::Model(slerp_models::ModelFormat::ThreeMf);
    uses_saved_picture("models-extra/3mf-samples/production/box_prod.3mf", f, 128);
    uses_saved_picture("models-extra/3mf-samples/material/multiprop-metallic.3mf", f, 128);
    uses_saved_picture("saved-extra/orca-flowrate-test-pass1.3mf", f, 128);
}

#[test]
fn slicer_projects_use_their_plate_picture() {
    let f = Kind::Model(slerp_models::ModelFormat::ThreeMf);
    uses_saved_picture("saved-extra/orca-pa_pattern.3mf", f, 512);
    uses_saved_picture("saved-extra/orca-auto_pa_line_dual.3mf", f, 512);
    // A project whose relationship names a plate picture it doesn't hold
    // is drawn.
    if let Some(p) = asset("saved-extra/orca-OrcaBadge.3mf") {
        assert!(saved_picture(f, &p, 256).is_none());
        assert!(matches!(prepare_thumbnail(&p, 256).unwrap(), Prepared::Render(_)));
    }
}

#[test]
fn rhino_files_use_their_preview_even_with_meshes() {
    uses_saved_picture("cad-extra/3dm/mx-testing/MeshSelfIntersect/Shoe2-C.3dm", Kind::Cad(slerp_cad::CadFormat::Rhino), 512);
}

/// A saved picture worse than Slerp's render gives way to it: ezdxf's
/// 64 x 32 smudge of a drawing, and Rhino's capture of a small object in
/// an empty viewport.
#[test]
fn poor_pictures_give_way_to_the_render() {
    for (rel, f) in [
        ("cad-extra/dxf/ezdxf/examples_dxf/columns_R2007.dxf", slerp_cad::CadFormat::Dxf),
        ("cad-extra/3dm/diffcheck/logotests.3dm", slerp_cad::CadFormat::Rhino),
    ] {
        let Some(p) = asset(rel) else { continue };
        let kind = Kind::Cad(f);
        assert!(carried_picture(kind, &p, &thumbnail_limits(256)).is_some(), "{rel} carries no picture");
        for size in [128, 256, 512] {
            assert!(saved_picture(kind, &p, size).is_none(), "{rel} at {size}");
        }
        match prepare_thumbnail(&p, 256).unwrap() {
            Prepared::Done(img) => {
                assert!(img.width > 64, "{rel}: the saved picture was used");
                save(&rel.replace('/', "_"), &img);
            }
            Prepared::Render(_) => {}
        }
    }
}

#[test]
fn fbx_files_use_their_header_thumbnail() {
    let f = Kind::Model(slerp_models::ModelFormat::Fbx);
    uses_saved_picture("fbx/ufbx/motionbuilder_thumbnail_7700_binary.fbx", f, 128);
    uses_saved_picture("fbx/ufbx/motionbuilder_thumbnail_7700_ascii.fbx", f, 128);
    // An all-white thumbnail isn't a picture of anything: rendered.
    if let Some(p) = asset("fbx/assimp/boxWithCompressedCTypeArray.FBX") {
        assert!(saved_picture(f, &p, 256).is_none());
        assert!(matches!(prepare_thumbnail(&p, 256).unwrap(), Prepared::Render(_)));
    }
}

#[test]
fn dxf_drawings_use_autocads_thumbnail() {
    let f = Kind::Cad(slerp_cad::CadFormat::Dxf);
    uses_saved_picture("cad-extra/dxf/clover/reinforced_plate_base.dxf", f, 512);
    // libredwg writes DWG data where the bitmap belongs: drawn instead.
    if let Some(p) = asset("cad-extra/dxf/libredwg/sample_2000.dxf") {
        assert!(saved_picture(f, &p, 256).is_none());
        assert!(prepare_thumbnail(&p, 256).is_ok());
    }
}

#[test]
fn vrm_avatars_use_their_portrait() {
    uses_saved_picture("vrm/AvatarSample_A.vrm", Kind::Gltf, 256);
    uses_saved_picture("vrm/Seed-san.vrm", Kind::Gltf, 256);
    // A VRM without one, and plain glTF, are rendered.
    for rel in ["vrm/EmissionMigration_v0.106.0.vrm"] {
        if let Some(p) = asset(rel) {
            assert!(saved_picture(Kind::Gltf, &p, 256).is_none(), "{rel}");
        }
    }
}

/// A picture too small for the thumbnail isn't a preview: the file is
/// drawn, and the little picture is shown only when the content can't be.
#[test]
fn tiny_pictures_are_only_a_last_resort() {
    let dir = std::env::temp_dir().join(format!("slerp-saved-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let picture = |side: u32| {
        let mut png = Vec::new();
        let mut enc = png::Encoder::new(&mut png, side, side);
        enc.set_color(png::ColorType::Rgba);
        // A gradient: a flat colour would be a placeholder too.
        let px: Vec<u8> = (0..side * side).flat_map(|i| [200, 30 + (i % side) as u8, 30, 255]).collect();
        enc.write_header().unwrap().write_image_data(&px).unwrap();
        png
    };
    let rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Target="/3D/3dmodel.model" Id="r0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/><Relationship Target="/Metadata/thumbnail.png" Id="r1" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail"/></Relationships>"#;
    let model = br#"<?xml version="1.0"?><model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02"><resources><object id="1" type="model"><mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="10" y="0" z="0"/><vertex x="0" y="10" z="0"/><vertex x="0" y="0" z="10"/></vertices><triangles><triangle v1="0" v2="2" v3="1"/><triangle v1="0" v2="1" v3="3"/><triangle v1="0" v2="3" v3="2"/><triangle v1="1" v2="2" v3="3"/></triangles></mesh></object></resources><build><item objectid="1"/></build></model>"#;
    let kind = Kind::Model(slerp_models::ModelFormat::ThreeMf);
    let write = |name: &str, model: &[u8], side: u32| {
        let png = picture(side);
        let zip = slerp_archive::testutil::zip(&[
            ("_rels/.rels", rels.as_slice()),
            ("3D/3dmodel.model", model),
            ("Metadata/thumbnail.png", png.as_slice()),
        ]);
        let p = dir.join(name);
        std::fs::write(&p, zip).unwrap();
        p
    };
    // 1. 100 pixels: the picture for a 128 px thumbnail, the model for a
    //    256 px one.
    let p = write("big.3mf", model, 100);
    assert!(saved_picture(kind, &p, 128).is_some());
    assert!(matches!(prepare_thumbnail(&p, 128).unwrap(), Prepared::Done(_)));
    assert!(matches!(prepare_thumbnail(&p, 256).unwrap(), Prepared::Render(_)));
    // 2. 32 pixels: the model.
    let p = write("small.3mf", model, 32);
    assert!(saved_picture(kind, &p, 128).is_none());
    assert!(matches!(prepare_thumbnail(&p, 128).unwrap(), Prepared::Render(_)));
    // 3. 32 pixels and a model that can't be read: the picture after all.
    let p = write("broken.3mf", b"<model><resources>", 32);
    match prepare_thumbnail(&p, 128).unwrap() {
        Prepared::Done(img) => assert_eq!(&img.pixels[..3], &[200, 30, 30]),
        Prepared::Render(_) => panic!("a broken model rendered"),
    }
    std::fs::remove_dir_all(&dir).ok();
}

/*┌─────────────────╮
| Corpus          |
└─────────────────╯*/
//region Corpus

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Whether a file is known, from its bytes alone and independently of the
/// readers, to carry a picture that should be its thumbnail: a 3MF with a
/// `Metadata/thumbnail.png` or `plate_1.png` member, a Rhino file with a
/// preview chunk, a DXF with a
/// `THUMBNAILIMAGE` section holding a bitmap (`310` lines starting with a
/// BITMAPINFOHEADER, `28000000`), an FBX whose header names `ImageData`, a
/// VRM naming a thumbnail.  `None` when the file's format isn't one of
/// those checked this way.
fn known_carrier(path: &Path, bytes: &[u8]) -> Option<bool> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "3mf" => {
            let zip = slerp_archive::Archive::from_bytes(bytes.to_vec()).ok()?;
            zip.entries().iter().any(|e| {
                let p = e.path.to_ascii_lowercase();
                (p == "metadata/thumbnail.png" || p == "metadata/plate_1.png") && e.size > 0
            })
        }
        "dxf" => {
            let at = bytes.windows(14).position(|w| w == b"THUMBNAILIMAGE")?;
            find(&bytes[at..bytes.len().min(at + 200)], b"310\n28000000")
                || find(&bytes[at..bytes.len().min(at + 200)], b"310\r\n28000000")
        }
        "fbx" => {
            let head = &bytes[..bytes.len().min(1 << 20)];
            let header_end = head.windows(17).position(|w| w == b"GlobalSettings").unwrap_or(head.len());
            find(&head[..header_end], b"SceneInfo") && find(&head[..header_end], b"ImageData")
        }
        // Rhino's preview chunks (`TCODE_PROPERTIES_PREVIEWIMAGE` and its
        // compressed form) in the properties table near the start.  Files
        // written by rhino3dm and some saved by Rhino itself have none.
        "3dm" => {
            let head = &bytes[..bytes.len().min(4 << 20)];
            find(head, &0x2000_8023u32.to_le_bytes()) || find(head, &0x2000_8025u32.to_le_bytes())
        }
        "vrm" => {
            let json = &bytes[..bytes.len().min(4 << 20)];
            find(json, b"\"thumbnailImage\":") && !find(json, b"\"thumbnailImage\":null")
                || find(json, b"\"VRM\"") && find(json, b"\"texture\":") && !find(json, b"\"texture\":-1")
        }
        _ => return None,
    })
}

/// Every corpus file of a format with a saved-picture reader: those whose
/// bytes show a picture must give one (perhaps a placeholder, too small or
/// flat to use), and every one that gives a real one has it as its
/// thumbnail.
#[test]
fn the_corpus_uses_saved_pictures() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets");
    if !root.is_dir() {
        eprintln!("no corpus; skipped");
        return;
    }
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();
    let wanted = ["3mf", "3dm", "fbx", "dxf", "vrm", "glb", "blend", "gcode", "bgcode", "litematic", "exr", "skp"];
    let mut counts: std::collections::BTreeMap<String, (u32, u32)> = Default::default();
    let mut missing = Vec::new();
    for path in files {
        let Some(ext) = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase) else { continue };
        if !wanted.contains(&ext.as_str()) {
            continue;
        }
        let Ok(Some(kind)) = sniff_file(&path) else { continue };
        let saved = saved_picture(kind, &path, 128);
        let count = counts.entry(ext.clone()).or_default();
        count.0 += 1;
        let bytes = std::fs::read(&path).unwrap_or_default();
        let known = known_carrier(&path, &bytes).unwrap_or(false);
        if known && saved.is_none() && carried_picture(kind, &path, &thumbnail_limits(128)).is_none() {
            missing.push(path.display().to_string());
        }
        let Some(saved) = saved else { continue };
        count.1 += 1;
        let Ok(Prepared::Done(thumb)) = prepare_thumbnail(&path, 128) else {
            panic!("{}: carries a picture but its thumbnail is drawn", path.display())
        };
        let Prepared::Done(expected) = saved.thumbnail(128) else { unreachable!() };
        assert!(thumb.pixels == expected.pixels, "{}: the thumbnail isn't the saved picture", path.display());
    }
    for (ext, (all, with)) in &counts {
        eprintln!("{ext}: {with} of {all} files carry a picture");
    }
    assert!(missing.is_empty(), "files known to carry a picture without one read: {missing:#?}");
}

//endregion
