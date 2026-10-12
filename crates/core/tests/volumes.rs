//! Scientific arrays, medical volumes and microscopy images through core:
//! recognised by content, thumbnailed inside the time budget from the middle
//! slice (or a small pyramid level), their parts listed, lookalikes left
//! alone, and damaged copies never panicking.

use slerp_core::{Kind, Prepared, load_part, parts, prepare_thumbnail, sniff_file, thumbnail_limits};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn assets() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/volumes");
    dir.exists().then_some(dir)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("slerp-volumes-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn thumb(path: &Path) -> (u32, u32, Duration) {
    let t = Instant::now();
    match prepare_thumbnail(path, 256).unwrap_or_else(|e| panic!("{}: {e}", path.display())) {
        Prepared::Done(img) => (img.width, img.height, t.elapsed()),
        Prepared::Render(_) => panic!("{} needs the GPU", path.display()),
    }
}

#[test]
fn every_format_is_recognised_and_thumbnailed_quickly() {
    let Some(d) = assets() else { return };
    let files = [
        "numpy/float32_3d_fortran.npy",
        "numpy/rgb_uint8.npy",
        "numpy/arrays.npz",
        "numpy/arrays_compressed.npz",
        "nifti/anatomical.nii",
        "nifti/example4d.nii.gz",
        "nifti/MNI152_T1_1mm_2009c.nii.gz",
        "nifti-generated/pair1.hdr",
        "nifti-generated/analyze.img",
        "nifti-generated/big_512x512x500.nii",
        "mha-generated/dtype_int16_z.mha",
        "mha-generated/rgb.mha",
        "nrrd-generated/dtype_int16_gzip.nrrd",
        "nrrd/multishell-DWI-451dir.nhdr",
        "tiff-generated/stack_u16_lzw.tif",
        "tiff-generated/ome_tczyx.ome.tif",
        "tiff-generated/imagej_hyperstack.tif",
        "tiff-generated/big_stack_lzw.tif",
        "ome/multi-channel-4D-series.ome.tif",
        "slides/CMU-1-Small-Region.svs",
        "slides/CMU-1.ndpi",
        "slides/Huron-1.tif",
        "slides/Philips-4.tiff",
        "slides/JP2K-33003-1.svs",
        "slides/Leica-Fluorescence-1.scn",
        "hdf5-generated/basic.h5",
        "hdf5-generated/matlab73.mat",
        "netcdf-generated/temp_classic.nc",
        "netcdf-generated/temp_nc4.nc",
        "netcdf/cells3d.nc",
    ];
    let mut done = 0;
    for rel in files {
        let p = d.join(rel);
        if !p.exists() {
            continue;
        }
        assert!(matches!(sniff_file(&p).unwrap(), Some(Kind::Volume(_))), "{rel}: {:?}", sniff_file(&p));
        assert!(!sniff_file(&p).unwrap().unwrap().usually_3d(), "{rel}");
        let (w, h, took) = thumb(&p);
        assert!(w > 0 && h > 0, "{rel}");
        assert!(took < Duration::from_millis(900), "{rel} took {took:?}");
        done += 1;
    }
    assert!(done > 15, "{done}");
}

#[test]
fn parts_come_from_arrays_datasets_and_slide_levels() {
    let Some(d) = assets() else { return };
    let p = d.join("numpy/arrays.npz");
    if p.exists() {
        assert_eq!(parts(&p), vec!["vector", "picture", "volume"]);
        let a = load_part(&p, &thumbnail_limits(256), "volume").unwrap();
        assert!(matches!(a, slerp_core::scene::Asset::Image(_)));
    }
    let p = d.join("hdf5-generated/basic.h5");
    if p.exists() {
        let names = parts(&p);
        assert!(names.contains(&"results/run1/map".to_string()), "{names:?}");
        assert!(load_part(&p, &thumbnail_limits(256), "results/run1/map").is_ok());
        assert!(load_part(&p, &thumbnail_limits(256), "no/such").is_err());
    }
    let p = d.join("slides/CMU-1-Small-Region.svs");
    if p.exists() {
        let names = parts(&p);
        for want in ["Level 0", "Thumbnail", "Label", "Macro"] {
            assert!(names.iter().any(|n| n.starts_with(want)), "{want} in {names:?}");
        }
        assert!(load_part(&p, &thumbnail_limits(256), &names[0]).is_ok());
    }
    // A plain .npy and a TIFF stack have no parts.
    assert!(parts(&d.join("numpy/uint8_2d.npy")).is_empty());
}

#[test]
fn lookalikes_are_left_alone() {
    let dir = scratch("lookalikes");
    let write = |name: &str, bytes: &[u8]| {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    };
    let kind = |p: &Path| sniff_file(p).unwrap();
    // Names without the content.
    assert_eq!(kind(&write("fake.npy", b"just text, no magic")), None);
    assert_eq!(kind(&write("fake.nii", &[0u8; 800])), None);
    assert_eq!(kind(&write("fake.nrrd", b"NRRD is a word\n")), None);
    assert_eq!(kind(&write("fake.mha", b"NDims DimSize in some text file")), None);
    assert_eq!(kind(&write("fake.h5", &[0u8; 500])), None);
    // Other zips and gzips are not arrays or volumes.
    assert_eq!(kind(&write("plain.zip", b"PK\x03\x04and some more bytes")), None);
    assert_eq!(kind(&write("archive.tar.gz", &[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3, 3, 0])), None);
    // Disk images and raw dumps with the extensions Analyze pairs use.
    assert_eq!(kind(&write("disk.img", &vec![7u8; 5000])), None);
    assert_eq!(kind(&write("dump.raw", &vec![9u8; 5000])), None);
    // A Radiance picture named .hdr is a texture, not a volume.
    let sky = write(
        "sky.hdr",
        b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 2 +X 2\n\x80\x80\x80\x80\x80\x80\x80\x80\x80\x80\x80\x80",
    );
    assert!(!matches!(kind(&sky), Some(Kind::Volume(_))));
    // A header with the Analyze size but nonsense after it.
    let mut h = vec![0u8; 348];
    h[0..4].copy_from_slice(&348i32.to_le_bytes());
    assert_eq!(kind(&write("odd.hdr", &h)), None);
    // Camera raw files are TIFF-shaped; they are not ours.
    let tiff = b"II*\0\x08\0\0\0\0\0\0\0\0\0";
    assert_eq!(kind(&write("shot.dng", tiff)), None);
    assert_eq!(kind(&write("shot.cr2", tiff)), None);
    assert!(matches!(kind(&write("page.tif", tiff)), Some(Kind::Volume(_))));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn damaged_files_fail_cleanly_through_core() {
    let Some(d) = assets() else { return };
    let dir = scratch("damage");
    let mut seed = 5u64;
    let mut rnd = move || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as usize
    };
    for rel in [
        "numpy/float32_3d_fortran.npy",
        "nifti/anatomical.nii",
        "nrrd-generated/dtype_int16_gzip.nrrd",
        "tiff-generated/ome_tczyx.ome.tif",
        "hdf5-generated/basic.h5",
        "netcdf-generated/temp_classic.nc",
        "slides/CMU-1-Small-Region.svs",
    ] {
        let src = d.join(rel);
        if !src.exists() {
            continue;
        }
        let bytes = std::fs::read(&src).unwrap();
        let name = src.file_name().unwrap().to_owned();
        for round in 0..30 {
            let mut b = bytes.clone();
            if round % 2 == 0 {
                b.truncate(rnd() % b.len());
            } else {
                for _ in 0..6 {
                    let i = rnd() % b.len().min(2000);
                    b[i] = rnd() as u8;
                }
            }
            let p = dir.join(&name);
            std::fs::write(&p, &b).unwrap();
            let t = Instant::now();
            if let Ok(Some(Kind::Volume(_))) = sniff_file(&p) {
                let _ = prepare_thumbnail(&p, 128);
                let _ = parts(&p);
            }
            assert!(t.elapsed() < Duration::from_secs(5), "{rel} round {round}");
        }
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_header_without_its_data_file_says_so() {
    let Some(d) = assets() else { return };
    let dir = scratch("alone");
    for (from, to) in [("mha-generated/orient/AIL.mhd", "AIL.mhd"), ("nrrd-generated/detached.nhdr", "detached.nhdr")] {
        let p = dir.join(to);
        if std::fs::copy(d.join(from), &p).is_err() {
            continue;
        }
        let e = prepare_thumbnail(&p, 256).err().expect("no data, no thumbnail").to_string();
        assert!(e.contains("beside this header is missing"), "{to}: {e}");
    }
    std::fs::remove_dir_all(&dir).ok();
}
