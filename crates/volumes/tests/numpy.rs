//! NumPy files written by `tools/make-numpy.py`, read back and compared with
//! the values NumPy saw.  Skipped (with a note) when the folder is missing.

use slerp_volumes::*;
use std::path::{Path, PathBuf};

fn dir() -> Option<PathBuf> {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/volumes/numpy");
    if d.join("reference.json").exists() {
        Some(d)
    } else {
        eprintln!("NumPy samples missing: run crates/volumes/tools/make-numpy.py");
        None
    }
}

fn open_file(p: &Path) -> Result<Box<Volume>, slerp_scene::LoadError> {
    let mut head = vec![0u8; 4096];
    let n = std::io::Read::read(&mut std::fs::File::open(p).unwrap(), &mut head).unwrap();
    let f = sniff(p, &head[..n]).expect("recognised");
    let v = open(f, p, None, &OpenOptions::thumbnail())?.content.volume().expect("a volume");
    Ok(v)
}

#[test]
fn every_array_matches_numpy() {
    let Some(d) = dir() else { return };
    let reference: serde_json::Value =
        serde_json::from_slice(&std::fs::read(d.join("reference.json")).unwrap()).unwrap();
    let mut checked = 0;
    for (name, r) in reference.as_object().unwrap() {
        // Refused and empty arrays have no values to compare.
        if r.get("samples").is_none() {
            continue;
        }
        let v = open_file(&d.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        let shape: Vec<usize> = r["shape"].as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as usize).collect();
        if shape.is_empty() {
            continue;
        }
        assert_eq!(v.shape(), shape, "{name}");
        if let Some(samples) = r.get("samples") {
            for s in samples.as_array().unwrap() {
                let (flat, want) = (s[0].as_u64().unwrap() as usize, s[1].as_f64().unwrap());
                // Unravel the C-order flat index over the logical shape.
                let mut idx = vec![0usize; shape.len()];
                let mut rest = flat;
                for k in (0..shape.len()).rev() {
                    idx[k] = rest % shape[k];
                    rest /= shape[k];
                }
                let got = v.value_at(&idx).unwrap();
                assert!((got - want).abs() <= want.abs() * 1e-6 + 1e-9, "{name} at {idx:?}: {got} vs {want}");
                checked += 1;
            }
        }
        if let (Some(min), Some(max)) = (r.get("min"), r.get("max")) {
            let st = v.stats().unwrap();
            let (min, max) = (min.as_f64().unwrap(), max.as_f64().unwrap());
            // Statistics sample large arrays; these are small enough to read whole.
            assert_eq!(st.sampled, st.total, "{name}");
            assert!((st.min - min).abs() <= min.abs() * 1e-5 + 1e-5, "{name}: min {} vs {min}", st.min);
            assert!((st.max - max).abs() <= max.abs() * 1e-5 + 1e-5, "{name}: max {} vs {max}", st.max);
            let mean = r["mean"].as_f64().unwrap();
            assert!((st.mean - mean).abs() <= mean.abs() * 1e-3 + 1e-3, "{name}: mean {} vs {mean}", st.mean);
        }
    }
    assert!(checked > 50, "{checked}");
}

#[test]
fn shapes_choose_pictures_stacks_or_statistics() {
    let Some(d) = dir() else { return };
    let thumb = |n: &str| middle_picture(&open_file(&d.join(n)).unwrap(), Some(512));
    for n in [
        "float32_2d.npy",
        "uint8_2d.npy",
        "rgb_uint8.npy",
        "rgba_float32.npy",
        "int16_3d.npy",
        "float32_4d_tzyx.npy",
        "rgb_stack_4d.npy",
        "bool_mask.npy",
        "complex64.npy",
        "v2.npy",
        "v3.npy",
    ] {
        let t = thumb(n).unwrap_or_else(|e| panic!("{n}: {e}"));
        assert!(t.width() > 0 && t.height() > 0, "{n}");
    }
    for n in ["vector_1d.npy", "scalar.npy", "five_d.npy"] {
        let v = open_file(&d.join(n)).unwrap();
        assert!(v.no_picture.is_some(), "{n}");
        assert!(thumb(n).is_err(), "{n}");
        v.stats().unwrap();
    }
    for n in ["structured.npy", "object.npy", "strings.npy", "datetime.npy"] {
        let e = open_file(&d.join(n)).err().unwrap_or_else(|| panic!("{n} should be refused"));
        assert!(!e.to_string().is_empty(), "{n}");
    }
    // The stack's middle slice is the picture, and RGB stays RGB.
    let v = open_file(&d.join("rgb_stack_4d.npy")).unwrap();
    assert_eq!(v.axes.iter().filter(|a| a.role == Role::Color).count(), 1);
}

#[test]
fn archives_list_their_arrays_and_picture_the_first_image() {
    let Some(d) = dir() else { return };
    for name in ["arrays.npz", "arrays_compressed.npz"] {
        let p = d.join(name);
        let f = sniff(&p, &std::fs::read(&p).unwrap()[..64]).unwrap();
        assert_eq!(f, VolumeFormat::Npz);
        let parts = parts_path(f, &p);
        assert_eq!(parts.len(), 3, "{name}");
        let o = open(f, &p, None, &OpenOptions::thumbnail()).unwrap();
        // The first array is a vector, so the second is shown.
        assert_eq!(o.part.as_deref(), Some(parts[1].as_str()), "{name}");
        let v = o.content.volume().expect("a volume");
        assert!(middle_picture(&v, Some(256)).is_ok());
        for part in &parts {
            let o = open(f, &p, Some(part), &OpenOptions::thumbnail()).unwrap();
            let v = o.content.volume().expect("a volume");
            v.stats().unwrap();
        }
    }
    // Deflated members read like stored ones.
    let p = d.join("arrays_compressed.npz");
    let vol = |n| {
        open(VolumeFormat::Npz, &p, Some(n), &OpenOptions::thumbnail()).unwrap().content.volume().expect("a volume")
    };
    let v = vol("volume");
    assert_eq!(v.shape(), vec![24, 40, 32]);
    // The same array saved as a plain .npy in Fortran order.
    let plain = open_file(&d.join("float32_3d_fortran.npy")).unwrap();
    for idx in [[12, 20, 16], [0, 0, 0], [23, 39, 31], [5, 33, 2]] {
        assert_eq!(v.value_at(&idx).unwrap(), plain.value_at(&idx).unwrap(), "{idx:?}");
    }
    // Reading backwards through a deflated member restarts the stream.
    let a = v.value_at(&[20, 5, 5]).unwrap();
    let b = v.value_at(&[2, 5, 5]).unwrap();
    assert_eq!(a, v.value_at(&[20, 5, 5]).unwrap());
    assert!(b.is_finite());
}

#[test]
fn damaged_numpy_files_never_panic() {
    let Some(d) = dir() else { return };
    let tmp = std::env::temp_dir().join(format!("slerp-npy-damage-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let mut seed = 12345u64;
    let mut rnd = move || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as usize
    };
    for name in [
        "float32_3d_fortran.npy",
        "uint16_2d.npy",
        "rgb_stack_4d.npy",
        "arrays.npz",
        "arrays_compressed.npz",
        "int16_3d_bigendian.npy",
    ] {
        let bytes = std::fs::read(d.join(name)).unwrap();
        for round in 0..60 {
            let mut b = bytes.clone();
            match round % 3 {
                0 => b.truncate(rnd() % b.len()),
                1 => {
                    for _ in 0..8 {
                        let i = rnd() % b.len().min(300);
                        b[i] = rnd() as u8;
                    }
                }
                _ => {
                    let i = rnd() % b.len();
                    b[i] ^= 0xff;
                    b.truncate(b.len() - rnd() % 7);
                }
            }
            let p = tmp.join(if name.ends_with("npz") { "x.npz" } else { "x.npy" });
            std::fs::write(&p, &b).unwrap();
            let head = &b[..b.len().min(4096)];
            if let Some(f) = sniff(&p, head)
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
fn a_header_that_claims_terabytes_costs_nothing() {
    let tmp = std::env::temp_dir().join(format!("slerp-npy-huge-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let mut dict = b"{'descr': '<f8', 'fortran_order': False, 'shape': (100000, 100000, 100), }".to_vec();
    while !(10 + dict.len() + 1).is_multiple_of(64) {
        dict.push(b' ');
    }
    dict.push(b'\n');
    let mut b = b"\x93NUMPY\x01\x00".to_vec();
    b.extend((dict.len() as u16).to_le_bytes());
    b.extend(dict);
    b.extend(vec![0u8; 4096]);
    let p = tmp.join("huge.npy");
    std::fs::write(&p, &b).unwrap();
    // The data is cut short, so the array clips to what is there or is refused.
    let r = open(VolumeFormat::Npy, &p, None, &OpenOptions::thumbnail());
    assert!(r.is_err());
    std::fs::remove_dir_all(&tmp).ok();
}
