//! Phase 23's stored previews through core's entry points: CAD files
//! (DWG, DWF, SolidWorks, Inventor, Revit, Solid Edge, Fusion), iWork,
//! Paint Shop Pro, Pixelmator Pro and PaintTool SAI documents, Pyxel Edit
//! and Piskel sprites and Unity packages.  Each real sample must sniff as
//! the right kind and format, and make a thumbnail on the CPU or say it has
//! no picture; look-alikes that share the extensions must not be claimed.
//! Uses `local/assets/editors-extra/`; missing folders are skipped with a
//! note.  `SLERP_TEST_OUT=<dir>` saves every thumbnail.

use slerp_core::{Kind, Prepared};
use slerp_previews::PreviewFormat as Pv;
use std::path::{Path, PathBuf};

fn assets(rel: &str) -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/editors-extra").join(rel);
    if p.is_dir() {
        Some(p)
    } else {
        eprintln!("skipped, not downloaded: {rel}");
        None
    }
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file() && p.extension().is_some_and(|e| !matches!(e.to_str(), Some("md" | "txt" | "png" | "JPG")))
        })
        .collect();
    out.sort();
    out
}

fn save(name: &str, img: &slerp_scene::Image8) {
    let Some(dir) = std::env::var_os("SLERP_TEST_OUT") else { return };
    let file = std::fs::File::create(Path::new(&dir).join(format!("{name}.png"))).unwrap();
    let mut enc = png::Encoder::new(file, img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&img.pixels).unwrap();
}

/// What a sample should be: a kind and a table id, or (for files that only
/// share the extension) something else.
struct Check {
    folder: &'static str,
    kind: Kind,
    id: &'static str,
    /// Samples known to hold no picture, by a part of their name.
    without: &'static [&'static str],
    /// Samples that aren't this format at all, with the kind they are.
    others: &'static [(&'static str, Option<Kind>)],
}

/// Sniffs every sample of a folder and makes its thumbnail; returns how
/// many had a picture.
fn run(c: &Check) -> usize {
    let Some(dir) = assets(c.folder) else { return 0 };
    let mut made = 0;
    for path in files(&dir) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let kind = slerp_core::sniff_file(&path).unwrap();
        if let Some((_, other)) = c.others.iter().find(|(part, _)| name.contains(part)) {
            assert_eq!(kind, *other, "{name}");
            continue;
        }
        assert_eq!(kind, Some(c.kind), "{name}");
        assert_eq!(c.kind.format(&path).map(|f| f.id), Some(c.id), "{name}");
        let started = std::time::Instant::now();
        let result = slerp_core::prepare_thumbnail(&path, 256);
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        match result {
            Ok(Prepared::Done(img)) => {
                assert!(!c.without.iter().any(|w| name.contains(w)), "{name}: a picture where none was expected");
                assert!(img.width.max(img.height) <= 256 && img.width.min(img.height) > 0, "{name}");
                eprintln!("{name}: {} x {} in {ms:.1} ms", img.width, img.height);
                save(&format!("{}-{name}", c.id), &img);
                made += 1;
            }
            Ok(Prepared::Render(_)) => panic!("{name}: a scene"),
            Err(e) => {
                eprintln!("{name}: {e} in {ms:.1} ms");
                assert!(c.without.iter().any(|w| name.contains(w)), "{name}: {e}");
            }
        }
        assert!(ms < 1500.0, "{name}: {ms:.0} ms");
    }
    eprintln!("{}: {made} pictures", c.folder);
    made
}

#[test]
fn autocad_drawings() {
    // Two downloads named .dwg are really DXF text, which the CAD crate
    // draws; R9 to R12 drawings and LibreDWG's own have no picture.
    let made = run(&Check {
        folder: "dwg",
        kind: Kind::Preview(Pv::Dwg),
        id: "dwg",
        without: &["2018-Arc", "2018-Text", "r9-", "r10-", "r11-", "OSMAP"],
        others: &[],
    });
    assert!(made >= 12 || made == 0, "{made}");
}

#[test]
fn dwf_files() {
    let made = run(&Check {
        folder: "dwf",
        kind: Kind::Preview(Pv::Dwf),
        id: "dwf",
        without: &["02085SA", "048_134", "05110CP", "1005174394358", "711765"],
        others: &[],
    });
    assert!(made >= 8 || made == 0, "{made}");
    // A drawing with three sheets offers each as a part.
    let Some(dir) = assets("dwf") else { return };
    let path = dir.join("aspose-for_layers_test.dwf");
    let parts = slerp_core::parts(&path);
    assert_eq!(parts, ["Drawing1-Model", "Drawing1-Layout1", "Drawing1-Layout2"]);
    for p in &parts {
        let asset = slerp_core::load_part(&path, &slerp_scene::Limits::viewer(), p).unwrap();
        assert!(matches!(asset, slerp_scene::Asset::Image(_)), "{p}");
    }
}

#[test]
fn mechanical_cad_files() {
    let checks = [
        Check {
            folder: "solidworks",
            kind: Kind::Preview(Pv::SolidWorks),
            id: "solidworks",
            without: &["SAMTEC-JETSON"],
            others: &[],
        },
        Check {
            folder: "inventor",
            kind: Kind::Preview(Pv::Inventor),
            id: "inventor",
            without: &["RESC1005X35N"],
            others: &[],
        },
        Check { folder: "revit", kind: Kind::Preview(Pv::Revit), id: "revit", without: &[], others: &[] },
        Check { folder: "solidedge", kind: Kind::Preview(Pv::SolidEdge), id: "solid-edge", without: &[], others: &[] },
        Check {
            folder: "fusion360",
            kind: Kind::Preview(Pv::Fusion),
            id: "fusion360",
            without: &["Bottom_Case_75HE"],
            // Bazis-Mebelshchik furniture designs share .f3d.
            others: &[("bazis-", None)],
        },
    ];
    for c in &checks {
        run(c);
    }
}

#[test]
fn creative_app_documents() {
    let checks = [
        Check {
            folder: "iwork",
            kind: Kind::Preview(Pv::Iwork),
            id: "iwork",
            without: &["Keynote_1", "Numbers_2", "issue-18", "PwdProtected", "Keynote2018"],
            others: &[],
        },
        Check {
            folder: "psp",
            kind: Kind::Preview(Pv::PaintShopPro),
            id: "paintshop-pro",
            without: &[],
            // A mask download that is really a BMP.
            others: &[(".msk", Some(Kind::Texture(slerp_textures::Format::Bmp)))],
        },
        Check {
            folder: "pixelmator",
            kind: Kind::Preview(Pv::Pixelmator),
            id: "pixelmator-pro",
            without: &[],
            // Classic Pixelmator's documents aren't claimed.
            others: &[(".pxm", None)],
        },
        Check { folder: "sai", kind: Kind::Preview(Pv::Sai), id: "painttool-sai", without: &[], others: &[] },
    ];
    for c in &checks {
        run(c);
    }
}

#[test]
fn pixel_art_and_unity_packages() {
    let checks = [
        Check {
            folder: "pyxel",
            kind: Kind::Sprite(slerp_sprites::SpriteFormat::Pyxel),
            id: "pyxel-edit",
            without: &[],
            others: &[],
        },
        Check {
            folder: "piskel",
            kind: Kind::Sprite(slerp_sprites::SpriteFormat::Piskel),
            id: "piskel",
            without: &[],
            others: &[],
        },
        Check {
            folder: "unitypackage",
            kind: Kind::Archive(slerp_core::archive::ArchiveKind::UnityPackage),
            id: "unity-package",
            // Code-only packages have neither previews nor pictures.
            without: &["EasyButtons", "UniTask", "NativeFilePicker"],
            others: &[],
        },
    ];
    for c in &checks {
        run(c);
    }
}

/// Files that only share an extension stay unclaimed: assembler source,
/// parity files, TLS keys, a makefile named .psp.
#[test]
fn lookalikes_are_left_alone() {
    let Some(dir) = assets("negatives") else { return };
    for path in files(&dir) {
        let kind = slerp_core::sniff_file(&path).unwrap();
        assert!(
            !matches!(kind, Some(Kind::Preview(_) | Kind::Archive(_) | Kind::Sprite(_))),
            "{}: {kind:?}",
            path.display()
        );
    }
}
