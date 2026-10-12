//! HDF5, MATLAB v7.3 and NetCDF files written by `tools/make-hdf5.py` (h5py
//! and netCDF4), compared with what those libraries saw.

use slerp_volumes::*;
use std::path::{Path, PathBuf};

fn assets(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/volumes").join(name)
}

fn generated() -> Option<PathBuf> {
    let d = assets("hdf5-generated");
    if d.join("reference.json").exists() {
        Some(d)
    } else {
        eprintln!("HDF5 samples missing: run crates/volumes/tools/make-hdf5.py");
        None
    }
}

fn open_part(p: &Path, part: Option<&str>, opts: &OpenOptions) -> Result<Opened, slerp_scene::LoadError> {
    let mut head = vec![0u8; 4096];
    let n = std::io::Read::read(&mut std::fs::File::open(p).unwrap(), &mut head).unwrap();
    let f = sniff(p, &head[..n]).unwrap_or_else(|| panic!("{} isn't recognised", p.display()));
    open(f, p, part, opts)
}

fn volume(p: &Path, part: Option<&str>) -> Box<Volume> {
    open_part(p, part, &OpenOptions::thumbnail())
        .unwrap_or_else(|e| panic!("{} {part:?}: {e}", p.display()))
        .content
        .volume()
        .expect("a volume")
}

fn check(v: &Volume, name: &str, r: &serde_json::Value) -> usize {
    let mut n = 0;
    let shape: Vec<usize> = r["shape"].as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as usize).collect();
    assert_eq!(v.shape(), shape, "{name}");
    for s in r["samples"].as_array().unwrap() {
        let idx: Vec<usize> = s[0].as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as usize).collect();
        let want = s[1].as_f64().unwrap();
        let got = v.value_at(&idx).unwrap_or_else(|e| panic!("{name} {idx:?}: {e}"));
        assert!((got - want).abs() <= want.abs() * 1e-6 + 1e-6, "{name} at {idx:?}: {got} vs {want}");
        n += 1;
    }
    n
}

#[test]
fn every_dataset_matches_h5py() {
    let Some(d) = generated() else { return };
    let reference: serde_json::Value =
        serde_json::from_slice(&std::fs::read(d.join("reference.json")).unwrap()).unwrap();
    let mut checked = 0;
    for (file, sets) in reference.as_object().unwrap() {
        // NetCDF files are checked in their own test.
        if file.ends_with(".nc") || file == "latest.h5" {
            continue;
        }
        let p = d.join(file);
        for (name, r) in sets.as_object().unwrap() {
            if r["samples"].as_array().is_none_or(|s| s.is_empty()) {
                continue;
            }
            // "extensible" datasets use an index kind Slerp doesn't read.
            let res = open_part(&p, Some(name), &OpenOptions::thumbnail());
            if name == "extensible" {
                let e = res.err().expect("extensible array index is refused");
                assert!(e.to_string().contains("extensible"), "{e}");
                continue;
            }
            let v = res.unwrap_or_else(|e| panic!("{file} {name}: {e}")).content.volume().unwrap();
            checked += check(&v, &format!("{file}:{name}"), r);
        }
    }
    assert!(checked > 80, "{checked}");
}

#[test]
fn every_dataset_loads_into_memory_too() {
    let Some(d) = generated() else { return };
    for (file, name) in [
        ("basic.h5", "volume_chunked_shuffle_gzip"),
        ("basic.h5", "volume_gzip"),
        ("latest110.h5", "fixed_array_paged"),
        ("latest110.h5", "btree2"),
    ] {
        let Content::Volume(v) = open_part(&d.join(file), Some(name), &OpenOptions::viewer(1 << 30)).unwrap().content
        else {
            panic!()
        };
        assert!(v.source.as_slice().is_some(), "{file} {name}");
        let m = v.default_map().unwrap();
        for kind in PlaneKind::ALL {
            let c = v.slice_count(&m, kind);
            v.extract(&m, &SliceView { kind, slice: c / 2, t: 0, neurological: false }, ChannelSel::All, None).unwrap();
        }
        // Planes read from the file and from memory agree.
        let f = volume(&d.join(file), Some(name));
        assert_eq!(f.value_at(&[3, 5, 7]).unwrap(), v.value_at(&[3, 5, 7]).unwrap());
    }
}

#[test]
fn types_and_refusals() {
    let Some(d) = generated() else { return };
    let p = d.join("basic.h5");
    let parts = parts_path(VolumeFormat::Hdf5, &p);
    // Pictures come first, then the other numeric arrays; strings and compound records are not listed.
    assert!(parts[0] != "vector");
    for want in ["image", "big_endian", "half", "flags", "rgb", "counts", "vector", "compact", "results/run1/map"] {
        assert!(parts.iter().any(|n| n == want), "{want} in {parts:?}");
    }
    for refused in ["names", "compound", "scalar"] {
        assert!(!parts.iter().any(|n| n == refused), "{refused} in {parts:?}");
    }
    let v = volume(&p, Some("rgb"));
    assert!(v.axes.iter().any(|a| a.role == Role::Color));
    let v = volume(&p, Some("vector"));
    assert!(v.no_picture.is_some());
    v.stats().unwrap();
    let v = volume(&p, Some("flags"));
    assert_eq!(v.dtype, DType::Bool);
    let v = volume(&p, Some("half"));
    assert_eq!(v.dtype, DType::F16);
    let v = volume(&p, Some("big_endian"));
    assert_eq!(v.endian, Endian::Big);
    // The newest layout is refused with a reason, not misread.
    let l = hdf5::list(&d.join("latest.h5")).unwrap();
    assert!(l.problems.iter().any(|p| p.contains("newer HDF5")), "{:?}", l.problems);
}

#[test]
fn matlab_arrays_are_rows_columns_slices() {
    let Some(d) = generated() else { return };
    let p = d.join("matlab73.mat");
    let reference: serde_json::Value =
        serde_json::from_slice(&std::fs::read(d.join("reference.json")).unwrap()).unwrap();
    let v = volume(&p, Some("scan"));
    assert!(v.format.starts_with("MATLAB"));
    assert_eq!(v.shape(), vec![24, 36, 10]);
    let roles: Vec<Role> = v.axes.iter().map(|a| a.role).collect();
    assert_eq!(roles, vec![Role::Y, Role::X, Role::Z]);
    check(&v, "scan", &reference["matlab73.mat"]["scan"]);
    let img = volume(&p, Some("image"));
    check(&img, "image", &reference["matlab73.mat"]["image"]);
    middle_picture(&v, Some(128)).unwrap();
}

#[test]
fn netcdf_in_every_flavour() {
    let Some(h) = generated() else { return };
    let d = assets("netcdf-generated");
    if !d.exists() {
        return;
    }
    let reference: serde_json::Value =
        serde_json::from_slice(&std::fs::read(h.join("reference.json")).unwrap()).unwrap();
    for file in ["temp_classic.nc", "temp_64bit.nc", "temp_cdf5.nc", "temp_nc4.nc"] {
        let p = d.join(file);
        let v = volume(&p, Some("tas"));
        check(&v, file, &reference[file]["tas"]);
        assert_eq!(v.axes.iter().map(|a| a.role).collect::<Vec<_>>().len(), 3, "{file}");
        // The grid's latitudes count upwards, so rows are drawn flipped.
        let y = v.axes.iter().find(|a| a.role == Role::Y).unwrap();
        assert!(y.reverse, "{file}: latitudes ascend");
        assert_eq!(v.fill, Some(-999.0), "{file}");
        // Masked cells (the first rows, first columns) are not numbers.
        assert!(v.value_at(&[1, 0, 0]).unwrap().is_nan(), "{file}");
        let st = v.stats().unwrap();
        assert!(st.nan > 0, "{file}");
        assert!(middle_picture(&v, Some(256)).is_ok(), "{file}");
    }
    // Packed values are unpacked with scale_factor and add_offset.
    let v = volume(&d.join("temp_classic.nc"), Some("scaled"));
    assert!((v.slope - 0.01).abs() < 1e-12 && (v.inter - 20.0).abs() < 1e-12);
    // The time dimension is a time axis.
    let v = volume(&d.join("temp_classic.nc"), Some("tas"));
    assert_eq!(v.axes[0].role, Role::T);
    assert_eq!(parts_path(VolumeFormat::NetCdf, &d.join("temp_classic.nc")), vec!["tas", "scaled"]);
}

/// Real files from xarray's example data: NCEP reanalysis packed as shorts
/// (CDF-1), ERA-Interim with four dimensions (CDF-2), an ocean mask and a 3D
/// microscope image saved as NetCDF-4.
#[test]
fn real_netcdf_files_open() {
    let d = assets("netcdf");
    if !d.join("air_temperature.nc").exists() {
        return;
    }
    let v = volume(&d.join("air_temperature.nc"), None);
    assert_eq!(v.shape(), vec![2920, 25, 53]);
    assert_eq!(v.axes[0].role, Role::T);
    // The values are packed: stored shorts times scale_factor plus add_offset.
    assert!(v.slope != 1.0 || v.inter != 0.0);
    let t = v.value_at(&[0, 10, 10]).unwrap();
    assert!((180.0..330.0).contains(&t), "{t} K");
    assert!(middle_picture(&v, Some(256)).is_ok());
    let v = volume(&d.join("eraint_uvz.nc"), Some("u"));
    assert_eq!(v.shape(), vec![2, 3, 241, 480]);
    assert_eq!(v.axes.iter().map(|a| a.role).collect::<Vec<_>>(), vec![Role::T, Role::Z, Role::Y, Role::X]);
    let m = v.default_map().unwrap();
    assert_eq!((v.time_count(&m), v.slice_count(&m, PlaneKind::Axial)), (2, 3));
    // NetCDF-4: the coordinate variables aren't listed as arrays.
    let parts = parts_path(VolumeFormat::Hdf5, &d.join("basin_mask.nc"));
    assert_eq!(parts[0], "basin");
    let v = volume(&d.join("cells3d.nc"), Some("images"));
    assert_eq!(v.shape(), vec![60, 2, 256, 256]);
    assert_eq!(v.dtype, DType::U16);
    let st = v.stats().unwrap();
    assert!(st.max > 1000.0);
    // Everything in the folder opens in a moment.
    for e in std::fs::read_dir(&d).unwrap().flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "nc") {
            let t = std::time::Instant::now();
            if let Ok(o) = open_part(&p, None, &OpenOptions::thumbnail())
                && let Some(v) = o.content.volume()
            {
                let _ = middle_picture(&v, Some(256));
            }
            assert!(t.elapsed().as_secs() < 3, "{}", p.display());
        }
    }
}

#[test]
fn damaged_files_never_panic() {
    let Some(d) = generated() else { return };
    let tmp = std::env::temp_dir().join(format!("slerp-h5-damage-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let mut seed = 31337u64;
    let mut rnd = move || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as usize
    };
    let files = [
        d.join("basic.h5"),
        d.join("latest110.h5"),
        d.join("matlab73.mat"),
        assets("netcdf-generated").join("temp_nc4.nc"),
        assets("netcdf-generated").join("temp_classic.nc"),
        assets("netcdf-generated").join("temp_cdf5.nc"),
    ];
    for src in files.iter().filter(|p| p.exists()) {
        let bytes = std::fs::read(src).unwrap();
        let ext = src.extension().unwrap().to_str().unwrap().to_string();
        for round in 0..150 {
            let mut b = bytes.clone();
            match round % 5 {
                0 => b.truncate(rnd() % b.len()),
                1 => {
                    for _ in 0..8 {
                        let i = rnd() % b.len().min(2048);
                        b[i] = rnd() as u8;
                    }
                }
                2 => {
                    for _ in 0..4 {
                        let i = rnd() % b.len();
                        b[i] = rnd() as u8;
                    }
                }
                3 => {
                    let i = rnd() % b.len();
                    b[i] ^= 0xff;
                }
                _ => {
                    // Large numbers near the front, where sizes and addresses live.
                    for _ in 0..3 {
                        let i = rnd() % b.len().min(1500);
                        b[i] = 0xff;
                        if i + 1 < b.len() {
                            b[i + 1] = 0x7f;
                        }
                    }
                }
            }
            let p = tmp.join(format!("x.{ext}"));
            std::fs::write(&p, &b).unwrap();
            let t = std::time::Instant::now();
            if let Some(f) = sniff(&p, &b[..b.len().min(4096)]) {
                let _ = parts_path(f, &p);
                if let Ok(o) = open(f, &p, None, &OpenOptions::thumbnail())
                    && let Some(v) = o.content.volume()
                {
                    let _ = middle_picture(&v, Some(128));
                    let _ = v.stats();
                }
            }
            assert!(t.elapsed().as_secs() < 5, "{} round {round}: {:?}", src.display(), t.elapsed());
        }
    }
    std::fs::remove_dir_all(&tmp).ok();
}

/// Found by fuzzing (a damaged copy of `netcdf-generated/temp_nc4.nc`, kept
/// in `tests/fuzz/`): a chunk index placing a chunk far outside the
/// dataset overflowed the grid position it is looked up by.
#[test]
fn chunk_outside_the_dataset_is_ignored() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz/hdf5-chunk-outside-grid.nc");
    let head = std::fs::read(&p).unwrap();
    let f = sniff(&p, &head).unwrap();
    let limits = slerp_scene::Limits { max_texture_size: Some(256), ..slerp_scene::Limits::thumbnail() };
    let _ = load_path(f, &p, &limits);
}
