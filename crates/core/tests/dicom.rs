//! DICOM through core: recognised by content with or without the preamble
//! and whatever the file is called, thumbnails of images and DICOMDIRs, and
//! no other format's files taken for DICOM.

use slerp_core::{Kind, Prepared, prepare_thumbnail, sniff_file};
use std::path::{Path, PathBuf};

fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets")
}

#[test]
fn recognised_by_content() {
    let dir = assets().join("dicom/pydicom-test-files");
    if !dir.exists() {
        eprintln!("skipping: {} is missing", dir.display());
        return;
    }
    // Part 10, a bare data set, an extension-less DICOMDIR member and the
    // DICOMDIR itself.
    for f in [
        "CT_small.dcm",
        "ExplVR_LitEndNoMeta.dcm",
        "ExplVR_BigEndNoMeta.dcm",
        "dicomdirtests/77654033/CT2/17106",
        "dicomdirtests/DICOMDIR",
    ] {
        assert_eq!(sniff_file(&dir.join(f)).unwrap(), Some(Kind::Dicom), "{f}");
        assert!(!Kind::Dicom.usually_3d());
    }
    // The same bytes under another name.
    let tmp = std::env::temp_dir().join(format!("slerp-dicom-sniff-{}", std::process::id()));
    std::fs::copy(dir.join("CT_small.dcm"), &tmp).unwrap();
    assert_eq!(sniff_file(&tmp).unwrap(), Some(Kind::Dicom));
    std::fs::remove_file(&tmp).unwrap();
}

#[test]
fn thumbnails() {
    let base = assets().join("dicom");
    for f in [
        "pydicom-test-files/CT_small.dcm",
        "pydicom-test-files/dicomdirtests/DICOMDIR",
        "pydicom-data/US1_J2KR.dcm",
        "pydicom-data/color3d_jpeg_baseline.dcm",
        "pydicom-data/RG1_J2KR.dcm",
        // Stores a CT lung window for an X-ray, which would paint it white.
        "tcia-lidc-idri-0001/dx/00000001.dcm",
    ] {
        let path = base.join(f);
        if !path.exists() {
            eprintln!("skipping: {f}");
            continue;
        }
        let t = std::time::Instant::now();
        let Prepared::Done(img) = prepare_thumbnail(&path, 256).unwrap_or_else(|e| panic!("{f}: {e}")) else {
            panic!("{f}: a DICOM thumbnail needs no GPU")
        };
        eprintln!("{f}: {}x{} in {:?}", img.width, img.height, t.elapsed());
        assert!(img.width <= 256 && img.height <= 256 && img.width.max(img.height) >= 64, "{f}: {img:?}");
        // Opaque, and not one flat colour.
        assert!(img.pixels.chunks_exact(4).all(|p| p[3] == 255), "{f}");
        let first = &img.pixels[..3];
        assert!(img.pixels.chunks_exact(4).any(|p| &p[..3] != first), "{f}: flat");
    }
}

/// Files of every other format in the corpus never sniff as DICOM.
#[test]
fn nothing_else_is_dicom() {
    let root = assets();
    let Ok(categories) = std::fs::read_dir(&root) else { return };
    let mut checked = 0;
    for cat in categories.flatten() {
        let name = cat.file_name().to_string_lossy().into_owned();
        if name.starts_with("dicom") || name.starts_with('.') {
            continue;
        }
        let mut stack = vec![cat.path()];
        let mut in_category = 0;
        while let Some(p) = stack.pop() {
            if in_category > 400 {
                break;
            }
            if p.is_dir() {
                stack.extend(std::fs::read_dir(&p).into_iter().flatten().flatten().map(|e| e.path()));
            } else {
                in_category += 1;
                checked += 1;
                assert_ne!(sniff_file(&p).ok().flatten(), Some(Kind::Dicom), "{}", p.display());
            }
        }
    }
    eprintln!("{checked} files checked");
}
