//! MetaImage and NRRD files written by `tools/make-mha-nrrd.py` (SimpleITK and
//! pynrrd), read back and compared with what those libraries saw.

use slerp_volumes::*;
use std::path::{Path, PathBuf};

fn folder(name: &str) -> Option<PathBuf> {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/volumes").join(name);
    if d.join("reference.json").exists() {
        Some(d)
    } else {
        eprintln!("{name} samples missing: run crates/volumes/tools/make-mha-nrrd.py");
        None
    }
}

fn open_file(p: &Path, expect: VolumeFormat) -> Box<Volume> {
    let mut head = vec![0u8; 4096];
    let n = std::io::Read::read(&mut std::fs::File::open(p).unwrap(), &mut head).unwrap();
    let f = sniff(p, &head[..n]).unwrap_or_else(|| panic!("{} isn't recognised", p.display()));
    assert_eq!(f, expect, "{}", p.display());

    open(f, p, None, &OpenOptions::thumbnail())
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
        .content
        .volume()
        .expect("a volume")
}

fn view(kind: PlaneKind, slice: usize) -> SliceView {
    SliceView { kind, slice, t: 0, neurological: false }
}

fn check_reference(dir: &Path, expect: impl Fn(&Path) -> VolumeFormat) -> usize {
    let reference: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("reference.json")).unwrap()).unwrap();
    let mut checked = 0;
    for (name, r) in reference.as_object().unwrap() {
        let p = dir.join(name);
        let v = open_file(&p, expect(&p));
        let shape: Vec<usize> = r["shape"].as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as usize).collect();
        let mine: Vec<usize> = v.shape();
        // Colour axes are extra in mine; compare the rest by size.
        let spatial: Vec<usize> = v.axes.iter().filter(|a| a.role != Role::Color).map(|a| a.len).collect();
        assert!(
            mine == shape || spatial == shape || {
                let mut s = shape.clone();
                s.sort();
                let mut m = mine.clone();
                m.sort();
                s == m
            },
            "{name}: {mine:?} vs {shape:?}"
        );
        if let Some(samples) = r.get("samples") {
            for s in samples.as_array().unwrap() {
                let idx: Vec<usize> = s[0].as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as usize).collect();
                let want = s[1].as_f64().unwrap();
                let got = v.value_at(&idx).unwrap_or_else(|e| panic!("{name} {idx:?}: {e}"));
                assert!((got - want).abs() <= want.abs() * 1e-5 + 1e-4, "{name} at {idx:?}: {got} vs {want}");
                checked += 1;
            }
        }
        if let Some(codes) = r.get("axcodes") {
            let got: String = v.axes[..3].iter().map(|a| a.dir.map_or('?', |d| d.letter())).collect();
            assert_eq!(got, codes.as_str().unwrap(), "{name}");
            for (i, z) in r["zooms"].as_array().unwrap().iter().enumerate() {
                assert!(
                    (v.axes[i].spacing - z.as_f64().unwrap()).abs() < 1e-3,
                    "{name} axis {i}: {} vs {z}",
                    v.axes[i].spacing
                );
            }
        }
        middle_picture(&v, Some(256)).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    checked
}

#[test]
fn metaimage_files_match_simpleitk() {
    let Some(d) = folder("mha-generated") else { return };
    let n = check_reference(&d, |_| VolumeFormat::MetaImage);
    assert!(n > 100, "{n}");
}

#[test]
fn nrrd_files_match_pynrrd_in_every_encoding() {
    let Some(d) = folder("nrrd-generated") else { return };
    let n = check_reference(&d, |_| VolumeFormat::Nrrd);
    assert!(n > 100, "{n}");
}

/// The pictures of one object in all 48 axis orders agree with the NIfTI
/// version of the same object, whatever format stored them.
#[test]
fn orientation_agrees_with_nifti_in_all_three_formats() {
    let nifti =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/volumes/nifti-generated/orient/RAS.nii.gz");
    let (Some(m), Some(n)) = (folder("mha-generated"), folder("nrrd-generated")) else { return };
    if !nifti.exists() {
        return;
    }
    let pictures = |v: &Volume| -> Vec<Vec<f32>> {
        let map = v.default_map().unwrap();
        let mut out = Vec::new();
        for kind in PlaneKind::ALL {
            let c = v.slice_count(&map, kind);
            for s in [0, c / 2, c - 1] {
                let sl = v.extract(&map, &view(kind, s), ChannelSel::All, None).unwrap();
                let mut d = vec![sl.width as f32, sl.height as f32];
                d.extend(sl.data);
                out.push(d);
            }
        }
        out
    };
    let r =
        open(VolumeFormat::Nifti, &nifti, None, &OpenOptions::thumbnail()).unwrap().content.volume().expect("a volume");
    let reference = pictures(&r);
    for (dir, fmt, exts) in
        [(&m, VolumeFormat::MetaImage, ["mha", "mhd"].as_slice()), (&n, VolumeFormat::Nrrd, ["nrrd"].as_slice())]
    {
        let mut count = 0;
        for e in std::fs::read_dir(dir.join("orient")).unwrap() {
            let p = e.unwrap().path();
            if !exts.contains(&p.extension().unwrap().to_str().unwrap()) {
                continue;
            }
            let v = open_file(&p, fmt);
            let got = pictures(&v);
            for (i, (g, r)) in got.iter().zip(&reference).enumerate() {
                assert_eq!(g, r, "{} picture {i}", p.display());
            }
            count += 1;
        }
        assert_eq!(count, 48, "{fmt:?}");
    }
}

#[test]
fn colour_and_time_axes() {
    let Some(m) = folder("mha-generated") else { return };
    let Some(n) = folder("nrrd-generated") else { return };
    let v = open_file(&m.join("rgb.mha"), VolumeFormat::MetaImage);
    assert_eq!(v.axes.iter().filter(|a| a.role == Role::Color).count(), 1);
    let t = middle_picture(&v, None).unwrap();
    assert_eq!((t.width(), t.height()), (14, 16));
    let v = open_file(&n.join("rgb.nrrd"), VolumeFormat::Nrrd);
    assert_eq!(v.axes[0].role, Role::Color);
    assert_eq!(middle_picture(&v, None).unwrap().width(), 16);
    let v = open_file(&n.join("dwi.nrrd"), VolumeFormat::Nrrd);
    let map = v.default_map().unwrap();
    assert_eq!(v.time_count(&map), 5);
    let v = open_file(&m.join("series4d.mha"), VolumeFormat::MetaImage);
    let map = v.default_map().unwrap();
    assert_eq!(v.time_count(&map), 5);
}

#[test]
fn detached_headers_name_their_data_file() {
    let Some(n) = folder("nrrd-generated") else { return };
    let o = open(VolumeFormat::Nrrd, &n.join("detached.nhdr"), None, &OpenOptions::thumbnail()).unwrap();
    assert_eq!(o.neighbours.len(), 1);
    assert!(o.neighbours[0].ends_with("detached.raw"));
    let Some(m) = folder("mha-generated") else { return };
    let o = open(VolumeFormat::MetaImage, &m.join("orient/RAS.mhd"), None, &OpenOptions::thumbnail());
    if let Ok(o) = o {
        assert_eq!(o.neighbours.len(), 1);
    }
}

#[test]
fn compressed_volumes_stream_and_load() {
    let Some(m) = folder("mha-generated") else { return };
    let Some(n) = folder("nrrd-generated") else { return };
    for (p, f) in
        [(m.join("big_compressed.mha"), VolumeFormat::MetaImage), (n.join("big_gzip.nrrd"), VolumeFormat::Nrrd)]
    {
        let t = std::time::Instant::now();
        let v = open_file(&p, f);
        middle_picture(&v, Some(512)).unwrap();
        assert!(t.elapsed().as_millis() < 1500, "{:?}", t.elapsed());
        // Into memory, then every plane kind.
        let v = open(f, &p, None, &OpenOptions::viewer(1 << 30)).unwrap().content.volume().expect("a volume");
        assert!(v.source.as_slice().is_some());
        let map = v.default_map().unwrap();
        for kind in PlaneKind::ALL {
            let c = v.slice_count(&map, kind);
            let s = v.extract(&map, &view(kind, c / 2), ChannelSel::All, None).unwrap();
            assert!(s.data.iter().cloned().fold(0.0, f32::max) > 100.0, "{kind:?}");
        }
    }
}

#[test]
fn damaged_files_never_panic() {
    let (Some(m), Some(n)) = (folder("mha-generated"), folder("nrrd-generated")) else { return };
    let tmp = std::env::temp_dir().join(format!("slerp-mha-damage-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let mut seed = 4242u64;
    let mut rnd = move || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as usize
    };
    let files = [
        m.join("dtype_int16_z.mha"),
        m.join("dtype_float32.mha"),
        n.join("dtype_int16_gzip.nrrd"),
        n.join("orient/LPS.nrrd"),
        n.join("hex.nrrd"),
        n.join("dwi.nrrd"),
    ];
    for src in files.iter().filter(|p| p.exists()) {
        let bytes = std::fs::read(src).unwrap();
        let ext = src.extension().unwrap().to_str().unwrap().to_string();
        for round in 0..80 {
            let mut b = bytes.clone();
            match round % 4 {
                0 => b.truncate(rnd() % b.len()),
                1 => {
                    for _ in 0..6 {
                        let i = rnd() % b.len().min(700);
                        b[i] = rnd() as u8;
                    }
                }
                2 => {
                    let i = rnd() % b.len();
                    b[i] ^= 0xff;
                }
                _ => {
                    // Absurd sizes in the text header.
                    let s = String::from_utf8_lossy(&b[..b.len().min(900)])
                        .replace("sizes:", "sizes: 99999 99999 ")
                        .replace("DimSize =", "DimSize = 999999 999999 ");
                    let mut nb = s.into_bytes();
                    nb.extend_from_slice(&b[b.len().min(900)..]);
                    b = nb;
                }
            }
            let p = tmp.join(format!("x.{ext}"));
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
fn a_bzip2_bomb_is_stopped_at_the_declared_size() {
    // 3 x 3 x 1 uchar declared, 100 MB of zeros compressed with bzip2.
    let tmp = std::env::temp_dir().join(format!("slerp-bz-bomb-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let raw = tmp.join("zeros.bin");
    std::fs::write(&raw, vec![0u8; 100 << 20]).unwrap();
    if std::process::Command::new("bzip2").arg("-kf").arg(&raw).status().map(|s| s.success()).unwrap_or(false) {
        let mut b = b"NRRD0004\ntype: uchar\ndimension: 3\nsizes: 3 3 1\nencoding: bzip2\n\n".to_vec();
        b.extend(std::fs::read(tmp.join("zeros.bin.bz2")).unwrap());
        let p = tmp.join("bomb.nrrd");
        std::fs::write(&p, &b).unwrap();
        let t = std::time::Instant::now();
        let v = open(VolumeFormat::Nrrd, &p, None, &OpenOptions::viewer(1 << 30))
            .unwrap()
            .content
            .volume()
            .expect("a volume");
        assert_eq!(v.bytes(), 9);
        assert!(t.elapsed().as_secs() < 5);
    }
    std::fs::remove_dir_all(&tmp).ok();
}

/// Files written by 3D Slicer itself: a diffusion set whose first axis is the
/// list of gradients, and a vector field.
#[test]
fn slicers_own_nrrd_files_open() {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/volumes/nrrd");
    if !d.join("multishell-DWI-451dir.nhdr").exists() {
        return;
    }
    let v = open_file(&d.join("multishell-DWI-451dir.nhdr"), VolumeFormat::Nrrd);
    assert_eq!(v.shape(), vec![451, 7, 7, 2]);
    assert_eq!(v.axes[0].role, Role::T);
    let dirs: Vec<_> = v.axes[1..].iter().map(|a| a.dir).collect();
    assert_eq!(dirs, vec![Some(Dir::L), Some(Dir::P), Some(Dir::S)]);
    let map = v.default_map().unwrap();
    assert_eq!(v.time_count(&map), 451);
    assert!((v.axes[1].spacing - 2.49).abs() < 0.01);
    middle_picture(&v, None).unwrap();
    let f = open_file(&d.join("deform2x2x2_ras.nrrd"), VolumeFormat::Nrrd);
    assert_eq!(f.axes[0].role, Role::Color);
    assert_eq!(f.axes[1].dir, Some(Dir::L));
    assert!(!f.warnings.is_empty());
}
