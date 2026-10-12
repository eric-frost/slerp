//! Creative apps' files and colour palettes through core's entry points:
//! each sample is sniffed as the right kind past every other loader (zips,
//! headerless colour tables), and its thumbnail is made on the CPU.  Uses
//! `local/assets/editors-extra/`; samples that aren't there are skipped
//! with a note.

use slerp_core::palette::PaletteFormat;
use slerp_core::{Kind, Prepared};
use slerp_previews::PreviewFormat;
use std::path::{Path, PathBuf};

fn folder(rel: &str) -> Option<PathBuf> {
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
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e != "md"))
        .collect();
    out.sort();
    out
}

/// Every file of a folder that core takes for `kind` gets a thumbnail no
/// larger than asked for, or (files saved without a preview or without
/// colours) an `Unsupported` error.  Other files there (an Aseprite
/// sprite among the `.ase` palettes) must not be taken for any palette or
/// preview.  Returns how many made a thumbnail.
fn thumbnails(rel: &str, kind: Kind) -> usize {
    let Some(dir) = folder(rel) else { return 0 };
    let mut made = 0;
    for path in files(&dir) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let found = slerp_core::sniff_file(&path).unwrap();
        if found != Some(kind) {
            assert!(!matches!(found, Some(Kind::Palette(_) | Kind::Preview(_))), "{rel}/{name}: {found:?}");
            eprintln!("{rel}/{name}: not taken ({found:?})");
            continue;
        }
        assert!(!kind.usually_3d());
        let start = std::time::Instant::now();
        match slerp_core::prepare_thumbnail(&path, 128) {
            Ok(Prepared::Done(img)) => {
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                eprintln!("{rel}/{name}: {} x {} in {ms:.1} ms", img.width, img.height);
                assert!(img.width.max(img.height) <= 128 && img.width.min(img.height) > 0, "{name}");
                made += 1;
            }
            Ok(Prepared::Render(_)) => panic!("{name}: a scene"),
            Err(e @ slerp_core::ThumbError::Load(slerp_scene::LoadError::Unsupported(_))) => {
                eprintln!("{rel}/{name}: {e}")
            }
            Err(e) => panic!("{rel}/{name}: {e}"),
        }
    }
    made
}

#[test]
fn creative_app_previews_are_thumbnails() {
    for (rel, format) in [
        ("procreate", PreviewFormat::Procreate),
        ("affinity", PreviewFormat::Affinity),
        ("sketch", PreviewFormat::Sketch),
        ("figma", PreviewFormat::Figma),
        ("clip", PreviewFormat::ClipStudio),
        ("pdn", PreviewFormat::PaintNet),
        ("indd", PreviewFormat::InDesign),
        ("xd", PreviewFormat::AdobeXd),
        ("freecad", PreviewFormat::FreeCad),
    ] {
        if folder(rel).is_some() {
            assert!(thumbnails(rel, Kind::Preview(format)) > 0, "{rel}");
        }
    }
}

#[test]
fn palettes_are_thumbnails() {
    for (rel, format) in [
        ("palettes/ase", PaletteFormat::Ase),
        ("palettes/aco", PaletteFormat::Aco),
        ("palettes/act", PaletteFormat::Act),
        ("palettes/gpl", PaletteFormat::Gpl),
        ("palettes/kpl", PaletteFormat::Kpl),
        ("palettes/pal-jasc", PaletteFormat::JascPal),
        ("palettes/pal-riff", PaletteFormat::RiffPal),
        ("palettes/hex", PaletteFormat::Hex),
        ("palettes/swatches", PaletteFormat::ProcreateSwatches),
    ] {
        if folder(rel).is_some() {
            assert!(thumbnails(rel, Kind::Palette(format)) > 0, "{rel}");
        }
    }
    // The viewer's list: names and values.
    if let Some(dir) = folder("palettes/ase")
        && let Some(path) = files(&dir).first()
    {
        let p = slerp_core::load_palette(path).unwrap();
        assert!(!p.is_empty());
    }
}

/// Files sharing palettes' and Figma's extensions aren't taken for them.
#[test]
fn look_alikes_are_left_alone() {
    for rel in ["palettes/pal-other", "palettes/hex-other", "palettes/xfig", "figma/xfig"] {
        let Some(dir) = folder(rel) else { continue };
        for path in files(&dir) {
            let kind = slerp_core::sniff_file(&path).unwrap();
            assert!(!matches!(kind, Some(Kind::Palette(_) | Kind::Preview(_))), "{}: {kind:?}", path.display());
        }
    }
}
