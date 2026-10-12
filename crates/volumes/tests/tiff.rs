//! TIFF files written by `tools/make-tiff.py` (tifffile) and the OME-TIFF
//! samples, compared with the values the writers saw; and the whole-slide
//! samples from openslide-testdata.

use slerp_volumes::*;
use std::path::{Path, PathBuf};

fn assets(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/volumes").join(name)
}

fn generated() -> Option<PathBuf> {
    let d = assets("tiff-generated");
    if d.join("reference.json").exists() {
        Some(d)
    } else {
        eprintln!("TIFF samples missing: run crates/volumes/tools/make-tiff.py");
        None
    }
}

fn open_any(p: &Path, opts: &OpenOptions) -> Result<Opened, slerp_scene::LoadError> {
    let mut head = vec![0u8; 4096];
    let n = std::io::Read::read(&mut std::fs::File::open(p).unwrap(), &mut head).unwrap();
    let f = sniff(p, &head[..n]).unwrap_or_else(|| panic!("{} isn't recognised", p.display()));
    assert_eq!(f, VolumeFormat::Tiff, "{}", p.display());
    open(f, p, None, opts)
}

fn volume_of(p: &Path) -> Box<Volume> {
    open_any(p, &OpenOptions::thumbnail())
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
        .content
        .volume()
        .unwrap_or_else(|| panic!("{} isn't a volume", p.display()))
}

/// The index into `v` for a position given along tifffile's axes.
fn index_for(v: &Volume, axes: &str, idx: &[usize]) -> Vec<usize> {
    v.axes
        .iter()
        .map(|a| {
            let letter = match a.role {
                Role::Z => 'Z',
                Role::T => 'T',
                Role::Channel if axes.contains('C') => 'C',
                Role::Channel | Role::Color => 'S',
                Role::Y => 'Y',
                Role::X => 'X',
                _ => panic!("unexpected axis {:?}", a.role),
            };
            axes.find(letter).map_or(0, |k| idx[k])
        })
        .collect()
}

#[test]
fn every_page_layout_matches_tifffile() {
    let Some(d) = generated() else { return };
    let reference: serde_json::Value =
        serde_json::from_slice(&std::fs::read(d.join("reference.json")).unwrap()).unwrap();
    let mut checked = 0;
    for (name, r) in reference.as_object().unwrap() {
        let Some(samples) = r.get("samples") else { continue };
        if name.starts_with("big_") || name == "pyramid_generic.tif" {
            continue;
        }
        let v = volume_of(&d.join(name));
        let axes = r["axes"].as_str().unwrap();
        let lossy = r["lossy"].as_bool().unwrap_or(false);
        for s in samples.as_array().unwrap() {
            let idx: Vec<usize> = s[0].as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as usize).collect();
            let want = s[1].as_f64().unwrap();
            let mine = index_for(&v, axes, &idx);
            let got = v.value_at(&mine).unwrap_or_else(|e| panic!("{name} {idx:?}: {e}"));
            let tol = if lossy { 12.0 } else { want.abs() * 1e-6 };
            assert!((got - want).abs() <= tol, "{name} at {idx:?}: {got} vs {want}");
            checked += 1;
        }
        // And a thumbnail, whatever the type.
        middle_picture(&v, Some(128)).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    assert!(checked > 100, "{checked}");
}

#[test]
fn ome_axes_channels_and_calibration() {
    let Some(d) = generated() else { return };
    let v = volume_of(&d.join("ome_tczyx.ome.tif"));
    let roles: Vec<Role> = v.axes.iter().map(|a| a.role).collect();
    assert_eq!(roles, vec![Role::T, Role::Channel, Role::Z, Role::Y, Role::X]);
    assert_eq!(v.shape(), vec![3, 2, 4, 40, 50]);
    assert_eq!(v.channels.len(), 2);
    assert_eq!(v.channels[0].name, "DAPI");
    assert_eq!(v.channels[0].color, [0, 0, 255]);
    assert_eq!(v.channels[1].color, [0, 255, 0]);
    let z = v.axes.iter().find(|a| a.role == Role::Z).unwrap();
    assert!((z.spacing - 1.5).abs() < 1e-9);
    let x = v.axes.iter().find(|a| a.role == Role::X).unwrap();
    assert!((x.spacing - 0.25).abs() < 1e-9);
    // The map finds the channel axis, and a plane carries both channels.
    let m = v.default_map().unwrap();
    assert_eq!(v.time_count(&m), 3);
    let s = v
        .extract(&m, &SliceView { kind: PlaneKind::Axial, slice: 1, t: 2, neurological: false }, ChannelSel::All, None)
        .unwrap();
    assert_eq!(s.samples, 2);
    let one = v
        .extract(
            &m,
            &SliceView { kind: PlaneKind::Axial, slice: 1, t: 2, neurological: false },
            ChannelSel::One(1),
            None,
        )
        .unwrap();
    assert_eq!(one.data[0], s.data[1]);
    // The picture composites the channels in their colours: DAPI blue, GFP green.
    let tex = middle_picture(&v, None).unwrap();
    assert_eq!(tex.width(), 50);
    let v = volume_of(&d.join("ome_rgb.ome.tif"));
    assert!(v.axes.iter().any(|a| a.role == Role::Color));
    assert_eq!(v.axes.iter().filter(|a| a.role == Role::Z).count(), 1);
}

#[test]
fn ome_samples_from_the_ome_consortium() {
    let dir = assets("ome");
    if !dir.join("multi-channel-4D-series.ome.tif").exists() {
        return;
    }
    let cases = [
        ("single-channel.ome.tif", vec![167, 439]),
        ("multi-channel.ome.tif", vec![3, 167, 439]),
        ("z-series.ome.tif", vec![5, 167, 439]),
        ("time-series.ome.tif", vec![7, 167, 439]),
        ("multi-channel-z-series.ome.tif", vec![3, 5, 167, 439]),
        ("multi-channel-time-series.ome.tif", vec![7, 3, 167, 439]),
        ("4D-series.ome.tif", vec![7, 5, 167, 439]),
        ("multi-channel-4D-series.ome.tif", vec![7, 3, 5, 167, 439]),
        ("multi-channel-4D-series.ome.btf", vec![7, 3, 5, 167, 439]),
        ("single-channel.ome.tf8", vec![167, 439]),
    ];
    for (name, shape) in cases {
        let v = volume_of(&dir.join(name));
        assert_eq!(v.shape(), shape, "{name}");
        assert!(middle_picture(&v, Some(256)).is_ok(), "{name}");
    }
    // The 4D series' own XML lists the planes in XYZCT order: z varies fastest.
    let v = volume_of(&dir.join("multi-channel-4D-series.ome.tif"));
    assert_eq!(
        v.axes.iter().map(|a| a.role).collect::<Vec<_>>(),
        vec![Role::T, Role::Channel, Role::Z, Role::Y, Role::X]
    );
    // The XML names the file, and a renamed copy still finds its planes by the
    // UUID, which the XML repeats.
    let copy_dir = std::env::temp_dir().join(format!("slerp-ome-rename-{}", std::process::id()));
    std::fs::create_dir_all(&copy_dir).unwrap();
    for (from, to, shape) in [
        ("multi-channel-z-series.ome.tif", "holiday photos.tif", vec![3, 5, 167, 439]),
        ("single-channel.ome.tf8", "x.tf8", vec![167, 439]),
    ] {
        std::fs::copy(dir.join(from), copy_dir.join(to)).unwrap();
        assert_eq!(volume_of(&copy_dir.join(to)).shape(), shape, "{to}");
    }
    std::fs::remove_dir_all(&copy_dir).ok();
    // A file whose other channels are in companion files shows the ones it has.
    if dir.join("tubhiswt_C0.ome.tif").exists() {
        let v = volume_of(&dir.join("tubhiswt_C0.ome.tif"));
        assert!(!v.warnings.is_empty());
        assert!(v.axes.iter().all(|a| a.role != Role::Channel || a.len == 1));
    }
}

#[test]
fn imagej_hyperstacks_keep_pages_back_to_back() {
    let Some(d) = generated() else { return };
    let v = volume_of(&d.join("imagej_hyperstack.tif"));
    assert_eq!(v.shape(), vec![3, 4, 2, 40, 50]);
    let z = v.axes.iter().find(|a| a.role == Role::Z).unwrap();
    assert!((z.spacing - 0.5).abs() < 1e-9);
    // 4 pixels per micrometre.
    let x = v.axes.iter().find(|a| a.role == Role::X).unwrap();
    assert!((x.spacing - 0.25).abs() < 1e-6, "{}", x.spacing);
}

#[test]
fn pages_of_different_sizes_are_parts() {
    let Some(d) = generated() else { return };
    let p = d.join("pages_mixed.tif");
    let o = open_any(&p, &OpenOptions::thumbnail()).unwrap();
    assert_eq!(o.parts, vec!["Page 1 (80 x 100)", "Page 2 (40 x 50)", "Page 3 (30 x 30)"]);
    let o = open(VolumeFormat::Tiff, &p, Some("Page 3 (30 x 30)"), &OpenOptions::thumbnail()).unwrap();
    let v = o.content.volume().unwrap();
    assert_eq!(v.shape(), vec![30, 30, 3]);
}

#[test]
fn a_generic_pyramid_is_a_slide() {
    let Some(d) = generated() else { return };
    let p = d.join("pyramid_generic.tif");
    let parts = parts_path(VolumeFormat::Tiff, &p);
    assert_eq!(parts.len(), 4, "{parts:?}");
    assert!(parts[0].starts_with("Level 0 (1600 x 1200"));
    // A thumbnail comes from a small level, not the full-size one.
    let opts = OpenOptions { thumb_size: Some(200), ..OpenOptions::thumbnail() };
    let o = open(VolumeFormat::Tiff, &p, None, &opts).unwrap();
    assert!(o.part.as_deref().unwrap().starts_with("Level 3 (200 x 150"), "{:?}", o.part);
    let Content::Picture(pic) = o.content else { panic!("a picture") };
    assert!(pic.texture.width() <= 400);
    // Asking for the base level decodes it all.
    let o = open(VolumeFormat::Tiff, &p, Some(&parts[0]), &OpenOptions::viewer(1 << 30)).unwrap();
    let Content::Picture(pic) = o.content else { panic!("a picture") };
    assert_eq!((pic.texture.width(), pic.texture.height()), (1600, 1200));
}

#[test]
fn palette_and_inverted_grey_follow_the_file() {
    let Some(d) = generated() else { return };
    let v = volume_of(&d.join("palette.tif"));
    assert_eq!(v.axes.iter().filter(|a| a.role == Role::Color).count(), 1);
    let w = volume_of(&d.join("miniswhite_u8.tif"));
    assert!(w.invert);
    let t = middle_picture(&w, None).unwrap();
    assert!(t.width() > 0);
}

fn slide(name: &str) -> Option<PathBuf> {
    let p = assets("slides").join(name);
    p.exists().then_some(p)
}

#[test]
fn whole_slide_files_give_thumbnails_from_small_levels() {
    let cases: [(&str, &str, usize); 6] = [
        ("CMU-1-Small-Region.svs", "Thumbnail", 4),
        ("JP2K-33003-1.svs", "Thumbnail", 6),
        ("CMU-1.ndpi", "Level 3", 5),
        ("Huron-1.tif", "Level 2", 6),
        ("Leica-Fluorescence-1.scn", "fluorescence level 2", 10),
        ("Philips-4.tiff", "Level 8", 10),
    ];
    for (name, expect, nparts) in cases {
        let Some(p) = slide(name) else { continue };
        let t = std::time::Instant::now();
        let opts = OpenOptions { thumb_size: Some(256), ..OpenOptions::thumbnail() };
        let o = open_any(&p, &opts).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(o.parts.len(), nparts, "{name}: {:?}", o.parts);
        assert!(o.part.as_deref().unwrap().contains(expect), "{name}: {:?}", o.part);
        let Content::Picture(pic) = o.content else { panic!("{name}: a picture") };
        assert!(pic.texture.width().max(pic.texture.height()) >= 256 / 2, "{name}");
        // Never the full-resolution level.
        assert!(
            pic.texture.source.width.max(pic.texture.source.height) < 5000,
            "{name}: {}x{}",
            pic.texture.source.width,
            pic.texture.source.height
        );
        assert!(t.elapsed().as_millis() < 800, "{name}: {:?}", t.elapsed());
    }
}

#[test]
fn slide_parts_include_label_and_macro_and_load_one_by_one() {
    let Some(p) = slide("CMU-1-Small-Region.svs") else { return };
    let parts = parts_path(VolumeFormat::Tiff, &p);
    for want in ["Label", "Macro", "Thumbnail", "Level 0"] {
        let part = parts.iter().find(|n| n.starts_with(want)).unwrap_or_else(|| panic!("{want} in {parts:?}"));
        let o = open(VolumeFormat::Tiff, &p, Some(part), &OpenOptions::viewer(1 << 30)).unwrap();
        let Content::Picture(pic) = o.content else { panic!("a picture") };
        assert!(pic.texture.width() > 100, "{part}");
    }
    // The full-size level of a bigger slide is refused rather than decoded.
    let Some(p) = slide("JP2K-33003-1.svs") else { return };
    let parts = parts_path(VolumeFormat::Tiff, &p);
    let big = parts.iter().find(|n| n.contains("too large")).unwrap();
    assert!(open(VolumeFormat::Tiff, &p, Some(big), &OpenOptions::viewer(1 << 30)).is_err());
    assert!(!parts[0].contains("too large"));
}

#[test]
fn stacks_stream_and_load() {
    let Some(d) = generated() else { return };
    let p = d.join("big_stack_lzw.tif");
    if !p.exists() {
        return;
    }
    let t = std::time::Instant::now();
    let v = volume_of(&p);
    assert_eq!(v.shape(), vec![120, 512, 512]);
    middle_picture(&v, Some(512)).unwrap();
    assert!(t.elapsed().as_millis() < 500, "{:?}", t.elapsed());
    // Into memory: every plane kind works.
    let Content::Volume(m) = open_any(&p, &OpenOptions::viewer(1 << 30)).unwrap().content else { panic!("volume") };
    assert!(m.source.as_slice().is_some());
    let map = m.default_map().unwrap();
    for kind in PlaneKind::ALL {
        let c = m.slice_count(&map, kind);
        m.extract(&map, &SliceView { kind, slice: c / 2, t: 0, neurological: false }, ChannelSel::All, None).unwrap();
    }
    assert_eq!(m.value_at(&[7, 100, 200]).unwrap(), v.value_at(&[7, 100, 200]).unwrap());
}

#[test]
fn damaged_tiffs_never_panic() {
    let Some(d) = generated() else { return };
    let tmp = std::env::temp_dir().join(format!("slerp-tiff-damage-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let mut seed = 2025u64;
    let mut rnd = move || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as usize
    };
    let mut files: Vec<PathBuf> = [
        "gray_u16_lzw_pred.tif",
        "tiled_lzw_pred_rgb.tif",
        "ome_tczyx.ome.tif",
        "bigtiff_u16.tif",
        "pyramid_generic.tif",
        "rgb_jpeg_strips.tif",
        "imagej_hyperstack.tif",
        "gray_f32_pred3_deflate.tif",
        "gray_u32_zstd.tif",
        "rgb_u16_planar.tif",
    ]
    .iter()
    .map(|n| d.join(n))
    .collect();
    files.push(assets("ome").join("multi-channel-z-series.ome.tif"));
    files.push(assets("slides").join("CMU-1-Small-Region.svs"));
    for src in files.iter().filter(|p| p.exists()) {
        let bytes = std::fs::read(src).unwrap();
        for round in 0..100 {
            let mut b = bytes.clone();
            match round % 5 {
                0 => b.truncate(rnd() % b.len()),
                1 => {
                    // Damage the header and the first directory.
                    for _ in 0..8 {
                        let i = rnd() % b.len().min(300);
                        b[i] = rnd() as u8;
                    }
                }
                2 => {
                    // The directory at the end of the file, where writers put it.
                    for _ in 0..6 {
                        let n = b.len();
                        let i = n - 1 - rnd() % n.min(2000);
                        b[i] = rnd() as u8;
                    }
                }
                3 => {
                    let i = rnd() % b.len();
                    b[i] ^= 0xff;
                    b.truncate(b.len() - rnd() % 5);
                }
                _ => {
                    // Absurd numbers in the header's first offset and tag counts.
                    let i = 4 + rnd() % 8;
                    b[i] = 0xff;
                    let j = rnd() % b.len().min(600);
                    b[j] = 0xff;
                }
            }
            let p = tmp.join("x.tif");
            std::fs::write(&p, &b).unwrap();
            if let Some(f) = sniff(&p, &b[..b.len().min(4096)]) {
                let opts = OpenOptions { thumb_size: Some(128), ..OpenOptions::thumbnail() };
                if let Ok(o) = open(f, &p, None, &opts)
                    && let Content::Volume(v) = o.content
                {
                    let _ = middle_picture(&v, Some(128));
                    let _ = v.stats();
                }
                let _ = parts_path(f, &p);
            }
        }
    }
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn tiny_files_with_absurd_claims_are_refused() {
    let tmp = std::env::temp_dir().join(format!("slerp-tiff-absurd-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    // A TIFF whose one page claims 2^31 x 2^31 pixels and a billion strips.
    let mut b = b"II*\0\x08\0\0\0".to_vec();
    let entries: [(u16, u16, u32, u32); 8] = [
        (256, 4, 1, 0x7fff_ffff),
        (257, 4, 1, 0x7fff_ffff),
        (258, 3, 1, 16),
        (259, 3, 1, 1),
        (262, 3, 1, 1),
        (273, 4, 1_000_000_000, 200),
        (277, 3, 1, 1),
        (279, 4, 1_000_000_000, 200),
    ];
    b.extend((entries.len() as u16).to_le_bytes());
    for (t, ty, c, v) in entries {
        b.extend(t.to_le_bytes());
        b.extend(ty.to_le_bytes());
        b.extend(c.to_le_bytes());
        b.extend(v.to_le_bytes());
    }
    b.extend(0u32.to_le_bytes());
    b.extend(vec![0u8; 300]);
    let p = tmp.join("absurd.tif");
    std::fs::write(&p, &b).unwrap();
    let t = std::time::Instant::now();
    let r = open_any(&p, &OpenOptions::thumbnail());
    if let Ok(o) = r
        && let Content::Volume(v) = o.content
    {
        assert!(middle_picture(&v, Some(64)).is_err());
    }
    assert!(t.elapsed().as_secs() < 3);
    // A directory loop and a self-referencing chain end.
    let mut c = b"II*\0\x08\0\0\0\x00\x00\x08\0\0\0".to_vec();
    c.extend(vec![0u8; 100]);
    std::fs::write(&p, &c).unwrap();
    let _ = open_any(&p, &OpenOptions::thumbnail());
    std::fs::remove_dir_all(&tmp).ok();
}
