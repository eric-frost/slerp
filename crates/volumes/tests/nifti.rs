//! NIfTI and Analyze files written by `tools/make-nifti.py` (nibabel), plus
//! nibabel's own test files, read back and compared with what nibabel saw.

use slerp_volumes::*;
use std::path::{Path, PathBuf};

fn generated() -> Option<PathBuf> {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/volumes/nifti-generated");
    if d.join("reference.json").exists() {
        Some(d)
    } else {
        eprintln!("NIfTI samples missing: run crates/volumes/tools/make-nifti.py");
        None
    }
}

fn open_with(p: &Path, opts: &OpenOptions) -> Result<Box<Volume>, slerp_scene::LoadError> {
    let mut head = vec![0u8; 4096];
    let n = std::io::Read::read(&mut std::fs::File::open(p).unwrap(), &mut head).unwrap();
    let f = sniff(p, &head[..n]).unwrap_or_else(|| panic!("{} isn't recognised", p.display()));
    assert_eq!(f, VolumeFormat::Nifti, "{}", p.display());
    let v = open(f, p, None, opts)?.content.volume().expect("a volume");
    Ok(v)
}

fn open_file(p: &Path) -> Box<Volume> {
    open_with(p, &OpenOptions::thumbnail()).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn view(kind: PlaneKind, slice: usize) -> SliceView {
    SliceView { kind, slice, t: 0, neurological: false }
}

#[test]
fn every_file_matches_nibabel() {
    let Some(d) = generated() else { return };
    let reference: serde_json::Value =
        serde_json::from_slice(&std::fs::read(d.join("reference.json")).unwrap()).unwrap();
    let mut checked = 0;
    for (name, r) in reference.as_object().unwrap() {
        let Some(samples) = r.get("samples") else { continue };
        let v = open_file(&d.join(name));
        let shape: Vec<usize> = r["shape"].as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as usize).collect();
        assert_eq!(v.shape()[..shape.len()], shape[..], "{name}");
        for s in samples.as_array().unwrap() {
            let idx: Vec<usize> = s[0].as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as usize).collect();
            let want = s[1].as_f64().unwrap();
            let got = v.value_at(&idx).unwrap();
            assert!((got - want).abs() <= want.abs() * 1e-5 + 1e-4, "{name} at {idx:?}: {got} vs {want}");
            checked += 1;
        }
        // Analyze headers carry no orientation (nibabel assumes LAS), so the
        // axes stay in array order.
        if shape.len() >= 3 && !name.starts_with("analyze") {
            let codes: String = v.axes[..3].iter().map(|a| a.dir.map_or('?', |d| d.letter())).collect();
            assert_eq!(codes, r["axcodes"].as_str().unwrap(), "{name}");
            let zooms = r["zooms"].as_array().unwrap();
            for (i, a) in v.axes[..3].iter().enumerate() {
                let z = zooms[i].as_f64().unwrap();
                assert!((a.spacing - z).abs() < 1e-3, "{name} axis {i}: {} vs {z}", a.spacing);
            }
        }
    }
    assert!(checked > 150, "{checked}");
}

/// The same object stored in all 48 axis orders and directions gives the same
/// axial, coronal and sagittal pictures, so orientation handling is right for
/// every layout a scanner or converter can write.
#[test]
fn all_forty_eight_storage_orders_show_the_same_pictures() {
    let Some(d) = generated() else { return };
    let dir = d.join("orient");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    assert_eq!(files.len(), 48);
    let pictures = |p: &Path| -> Vec<Vec<f32>> {
        let v = open_file(p);
        let m = v.default_map().unwrap();
        let mut out = Vec::new();
        for kind in PlaneKind::ALL {
            // Every slice of every plane, sampled at three places.
            let n = v.slice_count(&m, kind);
            for s in [0, n / 2, n - 1] {
                let sl = v.extract(&m, &view(kind, s), ChannelSel::All, None).unwrap();
                let mut d = vec![sl.width as f32, sl.height as f32];
                d.extend(sl.data);
                out.push(d);
            }
        }
        out
    };
    let reference = pictures(&dir.join("RAS.nii.gz"));
    for p in &files {
        let got = pictures(p);
        for (i, (g, r)) in got.iter().zip(&reference).enumerate() {
            assert_eq!(g, r, "{} picture {i}", p.display());
        }
    }
}

#[test]
fn the_radiological_picture_puts_the_front_up_and_the_right_on_the_left() {
    let Some(d) = generated() else { return };
    let v = open_file(&d.join("orient/RAS.nii.gz"));
    let m = v.default_map().unwrap();
    let s = v.extract(&m, &view(PlaneKind::Axial, 11), ChannelSel::All, None).unwrap();
    assert_eq!(s.edges, [Some('R'), Some('L'), Some('A'), Some('P')]);
    // The bright blob sits at x=15 (right), y=18 (front), z=11 (top): in the
    // picture it is left of centre (right of the patient) and in the upper half.
    let (mut bx, mut by, mut bv) = (0, 0, f32::MIN);
    for y in 0..s.height {
        for x in 0..s.width {
            let val = s.value(x, y, 0).unwrap();
            if val > bv {
                (bx, by, bv) = (x, y, val);
            }
        }
    }
    assert!(bx < s.width / 2, "blob at column {bx} of {}", s.width);
    assert!(by < s.height / 2, "blob at row {by} of {}", s.height);
    let n = SliceView { neurological: true, ..view(PlaneKind::Axial, 11) };
    let s2 = v.extract(&m, &n, ChannelSel::All, None).unwrap();
    assert_eq!(s2.edges[0], Some('L'));
    assert_eq!(s2.value(s.width - 1 - bx, by, 0), Some(bv));
}

#[test]
fn nibabels_own_files_open() {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/volumes/nifti");
    if !d.join("anatomical.nii").exists() {
        return;
    }
    for name in [
        "anatomical.nii",
        "functional.nii",
        "example4d.nii.gz",
        "example_nifti2.nii.gz",
        "standard.nii.gz",
        "reoriented_anat_moved.nii",
    ] {
        let v = open_file(&d.join(name));
        let t = middle_picture(&v, Some(512)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(t.width() > 0, "{name}");
    }
    // anatomical.nii is stored LAS with 2 mm voxels.
    let v = open_file(&d.join("anatomical.nii"));
    assert_eq!(v.shape(), vec![33, 41, 25]);
    assert_eq!(v.axes[0].dir, Some(Dir::L));
    let f = open_file(&d.join("functional.nii"));
    assert_eq!(f.shape(), vec![17, 21, 3, 20]);
    let m = f.default_map().unwrap();
    assert_eq!(f.time_count(&m), 20);
    // Scaled values: slope 0.0754, intercept 3100.76.
    assert!((f.slope - 0.07540696859359741).abs() < 1e-9);
}

#[test]
fn colour_complex_and_scaled_data() {
    let Some(d) = generated() else { return };
    let v = open_file(&d.join("rgb24.nii"));
    let m = v.default_map().unwrap();
    let s = v.extract(&m, &view(PlaneKind::Axial, 2), ChannelSel::All, None).unwrap();
    assert_eq!(s.samples, 3);
    // The voxel at x=3, y=5, z=2 is (48, 90, 80).  The axes point right and
    // front, so the radiological picture mirrors both: column 12, row 8.
    let at = (8 * s.width + 12) * 3;
    assert_eq!(&s.data[at..at + 3], &[48.0, 90.0, 80.0]);
    let c = open_file(&d.join("dtype_complex64.nii"));
    assert_eq!(c.dtype, DType::C64);
    assert!(middle_picture(&c, None).is_ok());
}

#[test]
fn pairs_open_from_either_file_and_analyze_is_found_by_its_header() {
    let Some(d) = generated() else { return };
    for stem in ["pair1", "pair2", "analyze", "gzpair"] {
        let a = open_file(&d.join(format!("{stem}.hdr")));
        assert_eq!(a.shape(), vec![24, 20, 12], "{stem}");
        let img = if stem == "gzpair" { d.join("gzpair.img.gz") } else { d.join(format!("{stem}.img")) };
        if stem != "gzpair" {
            let b = open_file(&img);
            assert_eq!(b.value_at(&[3, 4, 5]).unwrap(), a.value_at(&[3, 4, 5]).unwrap(), "{stem}");
        }
        assert!(middle_picture(&a, None).is_ok(), "{stem}");
    }
    // An .img with no header beside it is not claimed.
    let tmp = std::env::temp_dir().join(format!("slerp-nifti-img-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let p = tmp.join("disk.img");
    std::fs::write(&p, vec![7u8; 5000]).unwrap();
    assert_eq!(sniff(&p, &[7u8; 4096]), None);
    // An Analyze header that is really something else (wrong datatype size).
    let hdr = std::fs::read(d.join("analyze.hdr")).unwrap();
    let mut bad = hdr.clone();
    bad[72] = 7;
    std::fs::write(tmp.join("bad.hdr"), &bad).unwrap();
    std::fs::write(tmp.join("bad.img"), vec![0u8; 100]).unwrap();
    assert_eq!(sniff(&tmp.join("bad.hdr"), &bad), None);
    assert_eq!(sniff(&tmp.join("bad.img"), &[0u8; 100]), None);
    // A Radiance HDR picture named .hdr is never a volume.
    assert_eq!(sniff(&tmp.join("sky.hdr"), b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 4 +X 4\n"), None);
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn big_volumes_stay_on_disk_and_thumbnails_are_quick() {
    let Some(d) = generated() else { return };
    let p = d.join("big_512x512x500.nii");
    if !p.exists() {
        return;
    }
    // Over the memory limit: the volume stays in its file.
    let v = open_with(&p, &OpenOptions::viewer(64 << 20)).unwrap();
    assert!(v.source.as_slice().is_none());
    let m = v.default_map().unwrap();
    for kind in PlaneKind::ALL {
        let n = v.slice_count(&m, kind);
        let s = v.extract(&m, &view(kind, n / 2), ChannelSel::All, None).unwrap();
        assert!(s.data.iter().cloned().fold(0.0, f32::max) > 100.0, "{kind:?}");
    }
    // Under it: copied into memory, and the same planes come out.
    let small = d.join("mid_256x256x250.nii.gz");
    let mem = open_with(&small, &OpenOptions::viewer(1 << 30)).unwrap();
    assert!(mem.source.as_slice().is_some());
    let t = std::time::Instant::now();
    let tex = middle_picture(&open_file(&p), Some(512)).unwrap();
    assert!(t.elapsed().as_millis() < 1500, "{:?}", t.elapsed());
    assert!(tex.width() >= 256);
    // A gzip volume's thumbnail inflates only to its middle slice.
    let t = std::time::Instant::now();
    middle_picture(&open_file(&small), Some(512)).unwrap();
    assert!(t.elapsed().as_millis() < 1500, "{:?}", t.elapsed());
}

#[test]
fn a_thumbnail_of_a_deep_stream_stops_at_its_reach() {
    let Some(d) = generated() else { return };
    let p = d.join("mid_256x256x250.nii.gz");
    if !p.exists() {
        return;
    }
    // 256 x 256 x 250 int16 is 32 MB; a reach of 4 MB can't get to the middle.
    let opts = OpenOptions { memory_limit: 0, reach: 4 << 20, thumb_size: None, neighbours: true };
    let v = open_with(&p, &opts).unwrap();
    let tex = middle_picture(&v, Some(256)).unwrap();
    assert!(tex.width() > 0);
}

#[test]
fn damaged_nifti_files_never_panic() {
    let Some(d) = generated() else { return };
    let tmp = std::env::temp_dir().join(format!("slerp-nifti-damage-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let mut seed = 777u64;
    let mut rnd = move || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as usize
    };
    for name in ["dtype_int16.nii", "series4d.nii.gz", "nifti2.nii.gz", "rgb24.nii", "dtype_be_f4.nii", "five_d.nii"] {
        let bytes = std::fs::read(d.join(name)).unwrap();
        for round in 0..80 {
            let mut b = bytes.clone();
            match round % 4 {
                0 => b.truncate(rnd() % b.len()),
                1 => {
                    for _ in 0..6 {
                        let i = rnd() % b.len().min(540);
                        b[i] = rnd() as u8;
                    }
                }
                2 => {
                    let i = rnd() % b.len();
                    b[i] ^= 0xff;
                }
                _ => {
                    // Huge declared dimensions.
                    if b.len() > 60 && !name.ends_with(".gz") {
                        for k in 0..4 {
                            b[42 + 2 * k..44 + 2 * k].copy_from_slice(&0x7fffi16.to_le_bytes());
                        }
                        b[40] = 4;
                    }
                }
            }
            let ext = if name.ends_with(".gz") { "x.nii.gz" } else { "x.nii" };
            let p = tmp.join(ext);
            std::fs::write(&p, &b).unwrap();
            if let Some(f) = sniff(&p, &b[..b.len().min(4096)])
                && let Ok(o) = open(f, &p, None, &OpenOptions::thumbnail())
            {
                let v = o.content.volume().expect("a volume");
                let _ = middle_picture(&v, Some(128));
                let _ = v.stats();
            }
        }
    }
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn a_gzip_bomb_in_place_of_a_volume_is_stopped() {
    let tmp = std::env::temp_dir().join(format!("slerp-nifti-bomb-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    // A valid 4 x 4 x 4 int16 header followed by 200 MB of zeros, gzipped.
    let mut raw = vec![0u8; 352];
    raw[0..4].copy_from_slice(&348i32.to_le_bytes());
    raw[40..42].copy_from_slice(&3i16.to_le_bytes());
    for (i, d) in [4i16, 4, 4].iter().enumerate() {
        raw[42 + 2 * i..44 + 2 * i].copy_from_slice(&d.to_le_bytes());
    }
    raw[70..72].copy_from_slice(&4i16.to_le_bytes());
    raw[72..74].copy_from_slice(&16i16.to_le_bytes());
    raw[108..112].copy_from_slice(&352f32.to_le_bytes());
    raw[344..348].copy_from_slice(b"n+1\0");
    raw.extend(vec![0u8; 200 << 20]);
    let mut gz = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
    gz.extend(miniz_oxide::deflate::compress_to_vec(&raw, 1));
    gz.extend([0u8; 8]);
    let p = tmp.join("bomb.nii.gz");
    std::fs::write(&p, &gz).unwrap();
    let t = std::time::Instant::now();
    let o = open(VolumeFormat::Nifti, &p, None, &OpenOptions::viewer(1 << 30));
    // The header declares 128 bytes of data, so only that much is ever inflated.
    if let Ok(o) = o {
        let v = o.content.volume().expect("a volume");
        assert!(v.bytes() <= 4 * 4 * 4 * 2);
    }
    assert!(t.elapsed().as_secs() < 5);
    std::fs::remove_dir_all(&tmp).ok();
}
