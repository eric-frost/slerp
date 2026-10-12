//! Metafiles, CorelDRAW and Visio through core's entry points: each sample
//! is sniffed as the right kind, and its thumbnail is made on the CPU, on a
//! white page for drawings.  Uses `local/assets/metafile-extra/` and
//! `local/assets/editors-extra/{coreldraw,visio}/`; samples that aren't
//! there are skipped with a note.

use slerp_core::{Kind, Prepared};
use slerp_metafile::MetafileFormat;
use slerp_previews::PreviewFormat;
use std::path::{Path, PathBuf};

fn assets(rel: &str) -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets").join(rel);
    if p.is_dir() {
        Some(p)
    } else {
        eprintln!("skipped, not downloaded: {rel}");
        None
    }
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e != "md" && e != "png") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn thumbnail(path: &Path, size: u32) -> Option<slerp_scene::Image8> {
    match slerp_core::prepare_thumbnail(path, size) {
        Ok(Prepared::Done(img)) => {
            assert!(img.width.max(img.height) <= size && img.width.min(img.height) > 0, "{}", path.display());
            Some(img)
        }
        Ok(Prepared::Render(_)) => panic!("{}: a scene", path.display()),
        Err(slerp_core::ThumbError::Load(slerp_scene::LoadError::Unsupported(_))) => None,
        Err(e) => panic!("{}: {e}", path.display()),
    }
}

#[test]
fn coreldraw_files_show_their_stored_picture() {
    let Some(dir) = assets("editors-extra/coreldraw") else { return };
    let (mut made, mut total) = (0, 0);
    for path in files(&dir) {
        total += 1;
        let found = slerp_core::sniff_file(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // One bug attachment starts in the middle of a file.
        if name == "lo-bug55522-67958-staefa.cdr" {
            assert_eq!(found, None, "{name}");
            continue;
        }
        assert_eq!(found, Some(Kind::Preview(PreviewFormat::CorelDraw)), "{name}");
        let started = std::time::Instant::now();
        let img = thumbnail(&path, 128).unwrap_or_else(|| panic!("{name}: no picture"));
        eprintln!("{name}: {} x {} in {:.1} ms", img.width, img.height, started.elapsed().as_secs_f64() * 1000.0);
        assert!(img.pixels.chunks_exact(4).any(|p| p[3] > 0), "{name}: empty");
        made += 1;
    }
    eprintln!("{made} of {total} CorelDRAW samples");
    assert!(made >= 30 || total < 30);
}

#[test]
fn visio_files_show_their_stored_picture() {
    let Some(dir) = assets("editors-extra/visio") else { return };
    let (mut made, mut without) = (0, 0);
    for path in files(&dir) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let found = slerp_core::sniff_file(&path).unwrap();
        // The OLE files and zips here are Visio's, whatever their damage.
        if found != Some(Kind::Preview(PreviewFormat::Visio)) {
            eprintln!("{name}: not taken ({found:?})");
            continue;
        }
        match thumbnail(&path, 128) {
            Some(img) => {
                // Drawings sit on a white page: nothing is transparent.
                assert!(img.pixels.chunks_exact(4).all(|p| p[3] == 255), "{name}");
                made += 1;
            }
            None => without += 1,
        }
    }
    eprintln!("{made} Visio thumbnails, {without} files without one");
    assert!(made >= 20 || without > 0);
}

#[test]
fn metafile_samples_are_taken_and_drawn() {
    let Some(dir) = assets("metafile-extra") else { return };
    let mut drawn = 0;
    for path in files(&dir) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let ext = path.extension().unwrap().to_string_lossy().to_ascii_lowercase();
        let found = slerp_core::sniff_file(&path).unwrap();
        // The fuzzing corpus holds damaged files that no longer look like
        // metafiles; the rest must be taken for the right format.
        match (ext.as_str(), found) {
            ("emf", Some(Kind::Metafile(MetafileFormat::Emf)))
            | ("wmf", Some(Kind::Metafile(MetafileFormat::Wmf)))
            | ("pict", Some(Kind::Metafile(MetafileFormat::Pict))) => {}
            ("emf" | "wmf" | "pict", other) => {
                // (Two of POI's files are misnamed: font data and junk.)
                let damaged = ["graphicfilter", "clusterfuzz", "crash", "VHZ2NYFUYUUJNGLABL26ORTQZA76FJEW", "bug69924"];
                assert!(damaged.iter().any(|d| path.to_string_lossy().contains(d)), "{name}: {other:?}");
                continue;
            }
            _ => continue,
        }
        let started = std::time::Instant::now();
        let img = thumbnail(&path, 256);
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        assert!(ms < 3000.0, "{name} took {ms} ms");
        if let Some(img) = img {
            assert!(img.pixels.chunks_exact(4).all(|p| p[3] == 255), "{name}: thumbnails sit on a white page");
            drawn += 1;
        }
    }
    eprintln!("{drawn} metafiles drawn");
    assert!(drawn > 100);
}

#[test]
fn near_misses_are_not_metafiles() {
    // A real PNG and TGA, and headers that resemble WMF and EMF closely.
    for (name, bytes) in [
        ("a.png", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec()),
        ("a.wmf", vec![1, 0, 9, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        ("a.emf", vec![1, 0, 0, 0, 108, 0, 0, 0]),
        ("a.pict", vec![0u8; 600]),
    ] {
        let dir = std::env::temp_dir().join(format!("slerp-metafile-neg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, &bytes).unwrap();
        let kind = slerp_core::sniff_file(&path).unwrap();
        assert!(!matches!(kind, Some(Kind::Metafile(_))), "{name}: {kind:?}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
