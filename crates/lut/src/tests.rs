//! Tables with known answers (identities, swaps, inversions, the
//! OpenColorIO test files worked through by hand), the corpus in
//! `local/assets/luts/`, and damaged files.

use super::*;
use std::fmt::Write;
use std::path::PathBuf;

fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/luts")
}

/// A `.cube` file whose entries are `f(r, g, b)` on an `n`-point lattice.
fn cube_text(n: usize, header: &str, f: impl Fn(f32, f32, f32) -> [f32; 3]) -> String {
    let mut s = format!("# made by a test\nTITLE \"test\"\n{header}LUT_3D_SIZE {n}\n\n");
    let k = (n - 1) as f32;
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let v = f(r as f32 / k, g as f32 / k, b as f32 / k);
                let _ = writeln!(s, "{:.6} {:.6} {:.6}", v[0], v[1], v[2]);
            }
        }
    }
    s
}

fn grade(lut: &Lut, rgb: [u8; 3]) -> [u8; 3] {
    let mut px = [rgb[0], rgb[1], rgb[2], 77];
    lut.apply(&mut px);
    assert_eq!(px[3], 77, "alpha must be left alone");
    [px[0], px[1], px[2]]
}

#[test]
fn identity_tables_round_trip_exactly() {
    for n in [2, 17, 33, 65] {
        let lut = parse(LutFormat::Cube, cube_text(n, "", |r, g, b| [r, g, b]).as_bytes()).unwrap();
        assert_eq!(lut.lattice_size(), Some(n));
        for v in 0..=255u8 {
            assert_eq!(grade(&lut, [v, v, v]), [v, v, v], "size {n}");
        }
        let mut pixels: Vec<u8> = Vec::new();
        for r in (0..256u32).step_by(5) {
            for g in (0..256u32).step_by(3) {
                for b in (0..256u32).step_by(7) {
                    pixels.extend([r as u8, g as u8, b as u8, 255]);
                }
            }
        }
        let before = pixels.clone();
        lut.apply(&mut pixels);
        assert!(before == pixels, "size {n} changed colours");
    }
    // A 1D identity, and a .3dl identity with a 10-bit shaper and 12-bit
    // output.
    let mut s = String::from("LUT_1D_SIZE 1024\n");
    for i in 0..1024 {
        let v = i as f32 / 1023.0;
        let _ = writeln!(s, "{v} {v} {v}");
    }
    let lut = parse(LutFormat::Cube, s.as_bytes()).unwrap();
    assert_eq!(lut.curve_size(), Some(1024));
    for v in 0..=255u8 {
        assert_eq!(grade(&lut, [v, 255 - v, v / 2]), [v, 255 - v, v / 2]);
    }
    let n = 17usize;
    // The shaper's points aren't quite even (1023 doesn't divide by 16),
    // so the identity's outputs follow them.
    let shaper: Vec<usize> = (0..n).map(|i| (i * 1023 + 8) / (n - 1)).collect();
    let mut s = String::from("3DMESH\nMesh 4 12\n");
    s += &shaper.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" ");
    s.push('\n');
    for r in 0..n {
        for g in 0..n {
            for b in 0..n {
                let q = |i: usize| (shaper[i] as f64 * 4095.0 / 1023.0).round() as u32;
                let _ = writeln!(s, "{} {} {}", q(r), q(g), q(b));
            }
        }
    }
    let lut = parse(LutFormat::ThreeDl, s.as_bytes()).unwrap();
    for v in 0..=255u8 {
        assert_eq!(grade(&lut, [v, v / 3, 255 - v]), [v, v / 3, 255 - v]);
    }
}

#[test]
fn known_transforms() {
    // Swapping red and blue is linear, so any lattice gives it exactly.
    let swap = parse(LutFormat::Cube, cube_text(5, "", |r, g, b| [b, g, r]).as_bytes()).unwrap();
    assert_eq!(grade(&swap, [10, 20, 30]), [30, 20, 10]);
    assert_eq!(grade(&swap, [255, 0, 128]), [128, 0, 255]);
    // Inverting with a two-entry 1D table.
    let invert = parse(LutFormat::Cube, b"LUT_1D_SIZE 2\n1 1 1\n0 0 0\n").unwrap();
    assert_eq!(grade(&invert, [10, 20, 30]), [245, 235, 225]);
    // Out-of-range outputs are clipped, NaN-free.
    let loud = parse(LutFormat::Cube, cube_text(2, "", |r, g, b| [r * 4.0, g - 2.0, b]).as_bytes()).unwrap();
    assert_eq!(grade(&loud, [200, 200, 0]), [255, 0, 0]);
    // A domain from 0 to 2: input 1.0 (white) is the middle of the table.
    let half =
        parse(LutFormat::Cube, cube_text(3, "DOMAIN_MIN 0 0 0\nDOMAIN_MAX 2 2 2\n", |r, g, b| [r, g, b]).as_bytes())
            .unwrap();
    assert_eq!(grade(&half, [255, 0, 255]), [128, 0, 128]);
}

#[test]
fn opencolorio_test_files() {
    let dir = assets().join("ocio");
    if !dir.exists() {
        eprintln!("skipping: {} is missing", dir.display());
        return;
    }
    // DOMAIN_MIN 0 1 0, DOMAIN_MAX 2 2 1, every entry twice its lattice
    // position: the output is (r, 2 (g - 1), 2 b).
    let iridas = parse(LutFormat::Cube, &std::fs::read(dir.join("iridas_3d.cube")).unwrap()).unwrap();
    let out = iridas.apply_rgb([1.0, 1.5, 0.25]);
    for (o, w) in out.iter().zip([1.0, 1.0, 0.5]) {
        assert!((o - w).abs() < 1e-5, "{out:?}");
    }
    // Resolve's 1D shaper (input -1 to 3) before a 3D table (input 0 to
    // 4): 1.0 shapes to (2.15, 2.15, 2.2), lattice point (1.075, 1.075,
    // 1.1), which blends four corners of the cell from (1, 1, 1) to
    // (2, 2, 2) as worked out in the comments of this test's source.
    let resolve = parse(LutFormat::Cube, &std::fs::read(dir.join("resolve_1d3d.cube")).unwrap()).unwrap();
    assert_eq!((resolve.curve_size(), resolve.lattice_size()), (Some(6), Some(3)));
    let out = resolve.apply_rgb([1.0, 1.0, 1.0]);
    for (o, w) in out.iter().zip([0.4625, 0.4625, 0.45]) {
        assert!((o - w).abs() < 1e-5, "{out:?}");
    }
    // Lustre's 33-point mesh: blue changes fastest in the file, so its
    // second row is red 0, green 0, blue 1.
    let lustre = parse(LutFormat::ThreeDl, &std::fs::read(dir.join("lustre_33x33x33.3dl")).unwrap()).unwrap();
    let l = lustre.lattice.as_ref().unwrap();
    assert_eq!(l.size, 33);
    assert_eq!(l.entries[33 * 33], [0.0, 30.0 / 4095.0, 71.0 / 4095.0]);
    assert_eq!(l.entries[0], [2.0 / 4095.0, 4.0 / 4095.0, 4.0 / 4095.0]);
    // Autodesk's own sample: a 17-point shaper, 0 to 1023 in steps of 64
    // with the last point at 1023, so it isn't evenly spaced.
    let discreet = parse(LutFormat::ThreeDl, &std::fs::read(dir.join("discreet-3d-lut.3dl")).unwrap()).unwrap();
    assert_eq!(discreet.lattice_size(), Some(17));
    assert!(discreet.lattice.as_ref().unwrap().axis.is_some());
    assert_eq!(discreet.lattice.as_ref().unwrap().entries[0], [17.0 / 4095.0; 3]);
}

#[test]
fn corpus_thumbnails() {
    let dir = assets();
    if !dir.exists() {
        eprintln!("skipping: {} is missing", dir.display());
        return;
    }
    let mut files = Vec::new();
    for sub in ["", "ocio", "pronom"] {
        for e in std::fs::read_dir(dir.join(sub)).into_iter().flatten().flatten() {
            if e.path().is_file() {
                files.push(e.path());
            }
        }
    }
    assert!(files.len() >= 9, "{files:?}");
    let photo = reference_photo();
    for p in files {
        let bytes = std::fs::read(&p).unwrap();
        let format =
            sniff(&p, &bytes[..bytes.len().min(4096)]).unwrap_or_else(|| panic!("{} not recognised", p.display()));
        let started = std::time::Instant::now();
        let Asset::Lut(lut) = load(format, &p, &bytes, &Limits::thumbnail()).unwrap() else { panic!() };
        let t = thumbnail(&*lut, 256);
        eprintln!("{}: {:?} {:.1} ms", p.display(), lut.info().details, started.elapsed().as_secs_f64() * 1e3);
        assert_eq!((t.width, t.height), (256, 171));
        // The top left corner is the photo untouched.
        let small = photos::photo_at(256);
        assert_eq!(&t.pixels[..4 * 8], &small.pixels[..4 * 8]);
        assert!(photo.width == 512);
    }
}

#[test]
fn splits_and_test_images() {
    let images = test_images();
    assert_eq!(images.len(), 4);
    for i in images {
        assert_eq!((i.image.width, i.image.height), (512, 341), "{}", i.name);
    }
    let a = Image8 { width: 40, height: 20, pixels: vec![0; 40 * 20 * 4] };
    let b = Image8 { width: 40, height: 20, pixels: vec![200; 40 * 20 * 4] };
    let v = split_vertical(&a, &b, 0.5);
    // Columns 0 and 39 are far from the line at x = 20; 19 and 20 are on
    // it, lightened.
    assert_eq!(&v.pixels[..4], &[0, 0, 0, 0]);
    assert_eq!(&v.pixels[39 * 4..40 * 4], &[200, 200, 200, 200]);
    assert!(v.pixels[19 * 4] > 0 && v.pixels[20 * 4] > 200);
    let d = split_diagonal(&a, &b);
    assert_eq!(d.pixels[0], 0);
    assert_eq!(d.pixels[(40 * 20 - 1) * 4], 200);
    // A thumbnail asked bigger than the photo isn't enlarged.
    let id = parse(LutFormat::Cube, cube_text(2, "", |r, g, b| [r, g, b]).as_bytes()).unwrap();
    let t = thumbnail(&id, 1024);
    assert_eq!((t.width, t.height), (512, 341));
}

#[test]
fn recognises_tables_not_chemistry() {
    assert_eq!(sniff(Path::new("a.cube"), b"TITLE \"x\"\nLUT_3D_SIZE 2\n0 0 0\n"), Some(LutFormat::Cube));
    assert_eq!(sniff(Path::new("A.CUBE"), b"# c\nLUT_1D_SIZE 4\n"), Some(LutFormat::Cube));
    // A Gaussian cube file (electron densities) shares the extension.
    let gaussian =
        b" CPMD CUBE FILE.\n OUTER LOOP: X, MIDDLE LOOP: Y, INNER LOOP: Z\n    3    0.000000    0.000000    0.000000\n";
    assert_eq!(sniff(Path::new("density.cube"), gaussian), None);
    assert_eq!(sniff(Path::new("x.3dl"), b"3DMESH\nMesh 4 12\n"), Some(LutFormat::ThreeDl));
    assert_eq!(sniff(Path::new("x.3dl"), b"0 64 128 192\n0 0 0\n"), Some(LutFormat::ThreeDl));
    assert_eq!(sniff(Path::new("x.3dl"), b"hello world\n"), None);
    assert_eq!(sniff(Path::new("x.txt"), b"0 64 128 192\n"), None);
}

#[test]
fn damaged_tables_fail_quickly() {
    let started = std::time::Instant::now();
    // Sizes the file can't hold fail without allocating them.
    for text in [
        "LUT_3D_SIZE 256\n0 0 0\n",
        "LUT_3D_SIZE 257\n",
        "LUT_3D_SIZE 1\n0 0 0\n",
        "LUT_1D_SIZE 99999999\n",
        "LUT_3D_SIZE 2\n0 0 0\n1 1\n",
        "0 0 0\n",
        "LUT_3D_SIZE x\n",
        "DOMAIN_MIN 0 0\nLUT_3D_SIZE 2\n",
    ] {
        assert!(parse(LutFormat::Cube, text.as_bytes()).is_err(), "{text:?}");
    }
    assert!(parse(LutFormat::ThreeDl, b"3DMESH\nMesh 8 12\n0 0 0\n").is_err());
    assert!(parse(LutFormat::ThreeDl, b"0 1 2 3 4 5\nx y z\n").is_err());
    assert!(parse(LutFormat::ThreeDl, b"").is_err());
    // An empty domain falls back to 0..1 with a warning.
    let flat =
        parse(LutFormat::Cube, cube_text(2, "DOMAIN_MIN 1 1 1\nDOMAIN_MAX 1 1 1\n", |r, g, b| [r, g, b]).as_bytes())
            .unwrap();
    assert!(!flat.info.warnings.is_empty());
    // Truncated and mangled copies of the corpus never panic.
    let mut seed = 0x51ce_b00cu32;
    let mut rand = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let mut samples = vec![(LutFormat::Cube, cube_text(9, "", |r, g, b| [g, b, r]).into_bytes())];
    for (rel, f) in [
        ("ocio/resolve_1d3d.cube", LutFormat::Cube),
        ("ocio/discreet-3d-lut.3dl", LutFormat::ThreeDl),
        ("CameraLUT.cube", LutFormat::Cube),
    ] {
        if let Ok(b) = std::fs::read(assets().join(rel)) {
            samples.push((f, b));
        }
    }
    for (format, bytes) in samples {
        for cut in (0..bytes.len()).step_by((bytes.len() / 30).max(1)) {
            if let Ok(l) = parse(format, &bytes[..cut]) {
                let _ = thumbnail(&l, 32);
            }
        }
        for _ in 0..40 {
            let mut b = bytes.clone();
            for _ in 0..1 + rand() % 5 {
                let i = rand() as usize % b.len();
                b[i] = b"0123456789.-eE \n#xLUT_"[rand() as usize % 22];
            }
            if let Ok(l) = parse(format, &b) {
                let _ = thumbnail(&l, 32);
            }
        }
    }
    assert!(started.elapsed().as_secs() < 60, "took {:?}", started.elapsed());
}
