//! Tests against real files in `local/assets/editors-extra/` (skipped when
//! they aren't there; sources and licences in each folder's SOURCES.md)
//! and against damaged copies of them.  `SLERP_TEST_OUT=<dir>` saves every
//! preview for a look.

use super::*;
use slerp_scene::textures::Pixels;
use std::path::PathBuf;

fn folder(name: &str) -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/editors-extra").join(name);
    if p.exists() {
        Some(p)
    } else {
        eprintln!("skipping: {} is missing", p.display());
        None
    }
}

fn files(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()).is_some_and(|e| exts.contains(&e.to_ascii_lowercase().as_str()))
        })
        .collect();
    out.sort();
    out
}

fn save(name: &str, t: &Texture) {
    let Some(dir) = std::env::var_os("SLERP_TEST_OUT") else { return };
    let Pixels::Rgba8(px) = t.base() else { return };
    let file = std::fs::File::create(Path::new(&dir).join(format!("{name}.png"))).unwrap();
    let mut enc = png::Encoder::new(file, t.width(), t.height());
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(px).unwrap();
}

/// Loads every file of a folder, checking the format is recognised and
/// the picture has the expected size.  Returns how many had previews.
fn check(folder_name: &str, exts: &[&str], format: PreviewFormat, expect: &[(&str, (u32, u32))]) -> usize {
    let Some(dir) = folder(folder_name) else { return 0 };
    let mut found = 0;
    for path in files(&dir, exts) {
        let head = std::fs::read(&path).unwrap().into_iter().take(4096).collect::<Vec<u8>>();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(sniff(&path, &head), Some(format), "{name}");
        let start = std::time::Instant::now();
        let result = load_path(format, &path, &Limits::viewer());
        let took = start.elapsed().as_secs_f64() * 1000.0;
        match result {
            Ok(Asset::Image(t)) => {
                eprintln!("{name}: {} x {} in {took:.1} ms", t.width(), t.height());
                save(&format!("preview-{name}"), &t);
                if let Some((_, size)) = expect.iter().find(|(n, _)| *n == name) {
                    assert_eq!((t.width(), t.height()), *size, "{name}");
                }
                found += 1;
            }
            Ok(_) => panic!("{name}: not a picture"),
            Err(e) => {
                eprintln!("{name}: {e} in {took:.1} ms");
                assert!(matches!(e, LoadError::Unsupported(_)), "{name}: {e}");
            }
        }
    }
    found
}

#[test]
fn max_thumbnails() {
    let found = check(
        "max",
        &["max"],
        PreviewFormat::Max,
        &[
            ("xna-shipgame-energy.max", (128, 65)),
            ("xna-avatar-rig-max2010.max", (128, 79)),
            ("mcg-bend-test.max", (512, 258)),
            ("babylon-AnimatedSpotlights.max", (512, 469)),
            ("usd-camera_animated_source.max", (512, 300)),
        ],
    );
    if folder("max").is_some() {
        assert!(found >= 13, "{found}");
    }
}

#[test]
fn sketchup_thumbnails() {
    let found = check(
        "skp",
        &["skp"],
        PreviewFormat::SketchUp,
        &[
            ("testup-skp-v6.skp", (256, 256)),
            ("devtools-materials-cube-su7.skp", (128, 97)),
            ("testup-2021.skp", (256, 256)),
            ("openskp-coedge-2026.skp", (400, 232)),
            ("openskp-style_watermark-2025.skp", (256, 256)),
        ],
    );
    if folder("skp").is_some() {
        assert_eq!(found, 15);
    }
}

#[test]
fn cinema4d_previews() {
    let found = check(
        "c4d",
        &["c4d"],
        PreviewFormat::Cinema4d,
        &[("Pokeball.c4d", (500, 270)), ("maxon_smc_bridge_2026_2.c4d", (500, 300)), ("py-aep_c4d.c4d", (500, 258))],
    );
    // R8 to R12 files have no preview.
    if folder("c4d").is_some() {
        assert_eq!(found, 9);
    }
}

#[test]
fn zbrush_thumbnails() {
    let found = check(
        "zbrush",
        &["ztl", "zpr", "zbr"],
        PreviewFormat::ZBrush,
        &[
            ("CryptidGame_HairlessBigFoot.ztl", (96, 96)),
            ("WaytoMind_Low_Poly_Mandelkern.ztl", (512, 512)),
            ("escapestudios_Cyberarm_HP.ZPR", (512, 512)),
        ],
    );
    if folder("zbrush").is_some() {
        assert_eq!(found, 8);
    }
}

/// Creative apps' files: every sample is recognised, and those with a
/// stored picture show it.  Files without one (Sketch files saved by
/// other tools, FreeCAD files saved with thumbnails off) are
/// `Unsupported`.
#[test]
fn creative_app_previews() {
    let formats: [(&str, &[&str], PreviewFormat, usize); 9] = [
        ("procreate", &["procreate"], PreviewFormat::Procreate, 3),
        ("affinity", &["afphoto", "afdesign", "afpub", "af"], PreviewFormat::Affinity, 16),
        ("sketch", &["sketch"], PreviewFormat::Sketch, 7),
        ("figma", &["fig"], PreviewFormat::Figma, 8),
        ("clip", &["clip"], PreviewFormat::ClipStudio, 7),
        ("pdn", &["pdn"], PreviewFormat::PaintNet, 9),
        ("indd", &["indd"], PreviewFormat::InDesign, 8),
        ("xd", &["xd"], PreviewFormat::AdobeXd, 5),
        ("freecad", &["fcstd"], PreviewFormat::FreeCad, 7),
    ];
    for (folder_name, exts, format, at_least) in formats {
        let Some(dir) = folder(folder_name) else { continue };
        let mut found = 0;
        for path in files(&dir, exts) {
            let head: Vec<u8> = std::fs::read(&path).unwrap().into_iter().take(4096).collect();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if sniff(&path, &head) != Some(format) {
                // Not this format's container: an old SQLite Sketch file, a
                // cloud stub saved as `.xd`, a raw Figma file.
                eprintln!("{name}: not claimed");
                continue;
            }
            let start = std::time::Instant::now();
            let result = load_path(format, &path, &Limits::thumbnail());
            let took = start.elapsed().as_secs_f64() * 1000.0;
            match result {
                Ok(Asset::Image(t)) => {
                    eprintln!("{name}: {} x {} in {took:.1} ms", t.width(), t.height());
                    save(&format!("preview-{name}"), &t);
                    found += 1;
                }
                Ok(_) => panic!("{name}: not a picture"),
                Err(e) => {
                    eprintln!("{name}: {e} in {took:.1} ms");
                    assert!(matches!(e, LoadError::Unsupported(_)), "{name}: {e}");
                }
            }
        }
        assert!(found >= at_least, "{folder_name}: {found} previews");
    }
}

/// Xfig drawings share `.fig` with Figma and are left alone.
#[test]
fn coreldraw_previews() {
    let Some(dir) = folder("coreldraw") else { return };
    let expect = [
        // RIFF versions 7 to 13 and Presentation Exchange: a 96x96 DIB.
        ("lo-bug54586-66714-Rysunek1.CDR", (96, 96)),
        ("lo-bug53278-76820-Example_28.cdr", (96, 96)),
        ("lo-bug101466-127576-testfile.cdr", (96, 96)),
        ("lo-fdo63782-1.cmx", (96, 96)),
        // One 8-bit file stores 128x128.
        ("lo-bug71183-114901-changzheng_red_flag_vector.cdr", (128, 128)),
        // X4 to X6: a 24-bit BMP, 256 pixels or fitted to the page.
        ("lo-bug130914-158156-Frenchcurve.cdr", (256, 256)),
        ("lo-bug62539-76771-Example2.cdr", (180, 181)),
        ("lo-bug55581-68082-NewSampleTNR_LOGO11kl.cdr", (256, 256)),
        // X7 and later: a PNG.
        ("lo-bug101466-127643-Prova.cdr", (256, 256)),
    ];
    let mut found = 0;
    for path in files(&dir, &["cdr", "cmx"]) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let head = std::fs::read(&path).unwrap().into_iter().take(4096).collect::<Vec<u8>>();
        // One attachment starts in the middle of a file: nothing to recognise.
        if name == "lo-bug55522-67958-staefa.cdr" {
            assert_eq!(sniff(&path, &head), None);
            continue;
        }
        assert_eq!(sniff(&path, &head), Some(PreviewFormat::CorelDraw), "{name}");
        let started = std::time::Instant::now();
        let Ok(Asset::Image(t)) = load_path(PreviewFormat::CorelDraw, &path, &Limits::viewer()) else {
            panic!("{name}: no picture")
        };
        eprintln!("{name}: {} x {} in {:.1} ms", t.width(), t.height(), started.elapsed().as_secs_f64() * 1000.0);
        save(&format!("preview-{name}"), &t);
        if let Some((_, size)) = expect.iter().find(|(n, _)| *n == name) {
            assert_eq!((t.width(), t.height()), *size, "{name}");
        }
        found += 1;
    }
    assert_eq!(found, 40, "{found}");
}

#[test]
fn visio_previews_are_metafiles_or_absent() {
    let Some(dir) = folder("visio") else { return };
    let (mut metafiles, mut none) = (0, 0);
    for path in files(&dir, &["vsd", "vsdx", "vsdm"]) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let head = std::fs::read(&path).unwrap().into_iter().take(4096).collect::<Vec<u8>>();
        // Fuzzer files damaged beyond recognition may not be taken at all.
        if sniff(&path, &head) != Some(PreviewFormat::Visio) {
            eprintln!("{name}: not taken");
            continue;
        }
        match read_stored_path(PreviewFormat::Visio, &path, &Limits::viewer()) {
            Ok(Stored::Metafile(bytes)) => {
                // An EMF (`01 00 00 00 .. " EMF"`) or a WMF.
                let emf = bytes.get(40..44) == Some(b" EMF");
                let wmf = bytes.get(..4) == Some(&[1, 0, 9, 0]) || bytes.get(..4) == Some(&[0xD7, 0xCD, 0xC6, 0x9A]);
                assert!(emf || wmf, "{name}: {:02x?}", &bytes[..bytes.len().min(8)]);
                metafiles += 1;
            }
            Ok(Stored::Image(_)) => {}
            Err(e) => {
                eprintln!("{name}: {e}");
                assert!(matches!(e, LoadError::Unsupported(_) | LoadError::Invalid(_)), "{name}: {e}");
                none += 1;
            }
        }
    }
    eprintln!("{metafiles} metafile thumbnails, {none} without");
    assert!(metafiles >= 40, "{metafiles}");
}

#[test]
fn xfig_drawings_are_not_claimed() {
    assert_eq!(sniff(Path::new("a.fig"), b"#FIG 3.2  Produced by xfig version 3.2.5\nLandscape\n"), None);
    let Some(dir) = folder("figma/xfig") else { return };
    for path in files(&dir, &["fig"]) {
        let head: Vec<u8> = std::fs::read(&path).unwrap().into_iter().take(4096).collect();
        assert_eq!(sniff(&path, &head), None, "{}", path.display());
    }
}

#[test]
fn maya_files_are_not_claimed() {
    let Some(dir) = folder("maya") else { return };
    for path in files(&dir, &["ma", "mb"]) {
        let head: Vec<u8> = std::fs::read(&path).unwrap().into_iter().take(4096).collect();
        assert_eq!(sniff(&path, &head), None, "{}", path.display());
    }
}

#[test]
fn other_ole_files_are_not_claimed() {
    let head = [ole::MAGIC, &[0u8; 64][..]].concat();
    assert_eq!(sniff(Path::new("report.doc"), &head), None);
    assert_eq!(sniff(Path::new("scene.max"), &head), Some(PreviewFormat::Max));
}

/// Truncated and corrupted copies of one file of each kind: errors, never
/// panics, hangs or huge allocations.
#[test]
fn damaged_files_fail_cleanly() {
    let picks = [
        ("max", "xna-shipgame-energy.max", PreviewFormat::Max),
        ("max", "babylon-SimpleTriangle.max", PreviewFormat::Max),
        ("skp", "testup-skp-v6.skp", PreviewFormat::SketchUp),
        ("skp", "testup-2021.skp", PreviewFormat::SketchUp),
        ("c4d", "Pokeball.c4d", PreviewFormat::Cinema4d),
        ("zbrush", "CryptidGame_HairlessBigFoot.ztl", PreviewFormat::ZBrush),
        ("zbrush", "WaytoMind_Low_Poly_Mandelkern.ztl", PreviewFormat::ZBrush),
        ("procreate", "silicate-Mask_Test_File.procreate", PreviewFormat::Procreate),
        ("affinity", "senos-sun-yellow.afphoto", PreviewFormat::Affinity),
        ("affinity", "massini-AtomicLogo.af", PreviewFormat::Affinity),
        ("figma", "xf2020-App_Quantidade.fig", PreviewFormat::Figma),
        ("figma", "makefiles-logo-kiwi.fig", PreviewFormat::Figma),
        ("sketch", "sketch-constructor-myFile.sketch", PreviewFormat::Sketch),
        ("clip", "clipparse-emptyimage.clip", PreviewFormat::ClipStudio),
        ("clip", "cliptopsd-test_export_all_features.clip", PreviewFormat::ClipStudio),
        ("pdn", "pypdn-Untitled.pdn", PreviewFormat::PaintNet),
        ("indd", "exiftool-InDesign.indd", PreviewFormat::InDesign),
        ("xd", "simplex-Simplex_Game_Engine_Logo.xd", PreviewFormat::AdobeXd),
        ("freecad", "f3d-bracket_1x.FCStd", PreviewFormat::FreeCad),
    ];
    let limits = Limits::thumbnail();
    for (dir, name, format) in picks {
        let Some(dir) = folder(dir) else { continue };
        let Ok(bytes) = std::fs::read(dir.join(name)) else {
            eprintln!("skipping: {name} is missing");
            continue;
        };
        // 1. Truncated at many points.
        for cut in (0..bytes.len()).step_by((bytes.len() / 61).max(1)) {
            let _ = load_source(format, &mut Source::memory(bytes[..cut].to_vec()), &limits);
        }
        // 2. Bytes overwritten, mostly near the start and the end of the
        //    file, where the headers, tables, zip directories and pictures
        //    are.
        let mut seed = 0x2545_F491u32;
        for round in 0..300 {
            let mut copy = bytes.clone();
            for _ in 0..1 + round % 8 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let near = copy.len().min(16384);
                let i = match seed % 3 {
                    0 => seed as usize % copy.len(),
                    1 => seed as usize % near,
                    _ => copy.len() - 1 - seed as usize % near,
                };
                copy[i] = (seed >> 8) as u8;
            }
            let _ = load_source(format, &mut Source::memory(copy), &limits);
        }
    }
}

#[test]
fn reads_only_the_start_of_big_files() {
    // A ZPR project of 31 MB: the thumbnail comes from its first
    // kilobytes, so it takes milliseconds.
    let Some(dir) = folder("zbrush") else { return };
    let path = dir.join("escapestudios_Cyberarm_HP.ZPR");
    let start = std::time::Instant::now();
    load_path(PreviewFormat::ZBrush, &path, &Limits::thumbnail()).unwrap();
    assert!(start.elapsed().as_millis() < 500, "{:?}", start.elapsed());
}

/// Headers promising enormous or impossible pictures, tables and chains
/// fail at once instead of allocating or looping.
#[test]
fn hostile_headers_are_bounded() {
    let limits = Limits::viewer();
    let run = |format: PreviewFormat, bytes: Vec<u8>| {
        let start = std::time::Instant::now();
        let result = load_source(format, &mut Source::memory(bytes), &limits);
        assert!(result.is_err(), "{format:?}");
        assert!(start.elapsed().as_millis() < 500, "{format:?}: {:?}", start.elapsed());
    };
    // Affinity: a trailer far past the end, then a preview of 4 GB.
    let mut af = b"\x00\xFFKA\x0b\x00\x00\x00nsrP#Inf".to_vec();
    af.resize(0x40, 0);
    af[0x18..0x20].copy_from_slice(&u64::MAX.to_le_bytes());
    run(PreviewFormat::Affinity, af.clone());
    af[0x18..0x20].copy_from_slice(&0x40u64.to_le_bytes());
    af.extend_from_slice(b"\xFF\xFF\xFF\xFFThmb\x01\x00\x00\x00");
    af.extend_from_slice(&u32::MAX.to_le_bytes());
    af.extend_from_slice(&29u64.to_le_bytes());
    af.extend_from_slice(&u32::MAX.to_le_bytes());
    af.push(1);
    run(PreviewFormat::Affinity, af);
    // Paint.NET: a header of 16 MB in a tiny file.
    run(PreviewFormat::PaintNet, b"PDN3\xFF\xFF\xFF<pdnImage>".to_vec());
    // InDesign: a page count of 4 billion.
    let mut indd = vec![0u8; 8192];
    indd[..16].copy_from_slice(b"\x06\x06\xED\xF5\xD8\x1D\x46\xE5\xBD\x31\xEF\xE7\xFE\x74\xB7\x1D");
    indd[16..24].copy_from_slice(b"DOCUMENT");
    indd[280..284].copy_from_slice(&u32::MAX.to_le_bytes());
    run(PreviewFormat::InDesign, indd);
    // Figma: a thumbnail chunk of 4 GB.
    let mut fig = b"fig-kiwi\x0f\x00\x00\x00".to_vec();
    fig.extend_from_slice(&u32::MAX.to_le_bytes());
    fig.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    run(PreviewFormat::Figma, fig);
    // Clip Studio: a database chunk pointing at an SQLite header whose
    // schema page is its own child.
    let mut clip = b"CSFCHUNK".to_vec();
    clip.extend_from_slice(&0u64.to_be_bytes());
    clip.extend_from_slice(&24u64.to_be_bytes());
    clip.extend_from_slice(b"CHNKSQLi");
    clip.extend_from_slice(&4096u64.to_be_bytes());
    let mut db = vec![0u8; 4096];
    db[..16].copy_from_slice(b"SQLite format 3\0");
    db[16..18].copy_from_slice(&4096u16.to_be_bytes());
    db[100] = 0x05;
    db[103..105].copy_from_slice(&1u16.to_be_bytes());
    db[108..112].copy_from_slice(&1u32.to_be_bytes());
    db[112..114].copy_from_slice(&200u16.to_be_bytes());
    db[200..204].copy_from_slice(&1u32.to_be_bytes());
    clip.extend_from_slice(&db);
    run(PreviewFormat::ClipStudio, clip);
}

/// Loads the files `SLERP_PROBE` lists (separated by `:`), printing what
/// each shows and how long it took, and saving the pictures when
/// `SLERP_TEST_OUT` is set: `SLERP_PROBE=a.dwg:b.key cargo test -p
/// slerp-previews probe -- --ignored --nocapture`.
#[test]
#[ignore]
fn probe() {
    let Some(list) = std::env::var_os("SLERP_PROBE") else { return };
    for path in std::env::split_paths(&list) {
        let head: Vec<u8> = std::fs::read(&path).map(|b| b.into_iter().take(4096).collect()).unwrap_or_default();
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let Some(format) = sniff(&path, &head) else {
            eprintln!("{name}: not recognised");
            continue;
        };
        for (label, limits) in [("thumbnail", Limits::thumbnail()), ("viewer", Limits::viewer())] {
            let start = std::time::Instant::now();
            let result = read_stored_path(format, &path, &limits);
            let took = start.elapsed().as_secs_f64() * 1000.0;
            match result {
                Ok(Stored::Image(t)) => {
                    eprintln!(
                        "{name} ({format:?}, {label}): {} x {} in {took:.1} ms {:?}",
                        t.width(),
                        t.height(),
                        t.source.header
                    );
                    save(&format!("probe-{label}-{name}"), &t);
                }
                Ok(Stored::Metafile(m)) => {
                    eprintln!("{name} ({format:?}, {label}): a {}-byte metafile in {took:.1} ms", m.len())
                }
                Err(e) => eprintln!("{name} ({format:?}, {label}): {e} in {took:.1} ms"),
            }
        }
    }
}

/*┌─────────────────╮
| CAD and paint   |
| apps' previews  |
└─────────────────╯*/
//region CAD and paint apps' previews

#[test]
fn dwg_previews() {
    let found = check(
        "dwg",
        &["dwg"],
        PreviewFormat::Dwg,
        &[
            ("ixmilia-R14.dwg", (512, 512)),
            ("acadsharp-sample_AC1032.dwg", (256, 94)),
            ("libredwg-example_r13.dwg", (216, 140)),
        ],
    );
    if folder("dwg").is_some() {
        assert!(found >= 15, "{found}");
    }
}

#[test]
fn dwf_previews() {
    let found = check("dwf", &["dwf", "dwfx"], PreviewFormat::Dwf, &[("dwftoolkit-Arbor_Press_3D.dwf", (213, 213))]);
    if folder("dwf").is_some() {
        assert!(found >= 8, "{found}");
    }
}

#[test]
fn cad_ole_and_solidworks_previews() {
    // Folder, extensions, format, sizes expected, least pictures.
    type Set = (&'static str, &'static [&'static str], PreviewFormat, &'static [(&'static str, (u32, u32))], usize);
    let sets: [Set; 5] = [
        (
            "solidworks",
            &["sldprt", "sldasm", "slddrw"],
            PreviewFormat::SolidWorks,
            &[("pcfan-PC_Fan.SLDASM", (640, 480)), ("cmuphyscomp-link_coupler.SLDPRT", (1188, 818))],
            17,
        ),
        (
            "inventor",
            &["ipt", "iam", "idw", "ipn"],
            PreviewFormat::Inventor,
            &[("respiraworks-enclosure.iam", (512, 512))],
            15,
        ),
        ("revit", &["rvt", "rfa", "rte"], PreviewFormat::Revit, &[("mcd-Device_Tag.rfa", (128, 128))], 9),
        ("solidedge", &["par", "psm", "asm", "dft"], PreviewFormat::SolidEdge, &[("lesanded-h3.par", (243, 178))], 15),
        ("fusion360", &["f3d"], PreviewFormat::Fusion, &[("cr10-rx100m6-bracket.f3d", (1024, 1024))], 10),
    ];
    for (dir, exts, format, expect, least) in sets {
        let Some(path) = folder(dir) else { continue };
        // Bazis-Mebelshchik's .f3d files aren't Fusion's and aren't claimed.
        if format == PreviewFormat::Fusion {
            for p in files(&path, &["f3d"]).iter().filter(|p| p.to_string_lossy().contains("bazis-")) {
                let head: Vec<u8> = std::fs::read(p).unwrap().into_iter().take(4096).collect();
                assert_eq!(sniff(p, &head), None, "{}", p.display());
                std::fs::metadata(p).unwrap();
            }
            let found = files(&path, &["f3d"])
                .into_iter()
                .filter(|p| !p.to_string_lossy().contains("bazis-"))
                .filter(|p| load_path(format, p, &Limits::viewer()).is_ok())
                .count();
            assert!(found >= least, "{dir}: {found}");
            continue;
        }
        let found = check(dir, exts, format, expect);
        assert!(found >= least, "{dir}: {found}");
    }
}

#[test]
fn iwork_previews() {
    let found = check(
        "iwork",
        &["key", "pages", "numbers"],
        PreviewFormat::Iwork,
        &[("tika-testKeynote2013.key", (1028, 772)), ("tika-testPages.pages", (361, 512))],
    );
    if folder("iwork").is_some() {
        assert!(found >= 11, "{found}");
    }
}

#[test]
fn paint_app_previews() {
    let found = check(
        "psp",
        &["psp", "pspimage", "pspframe", "psptube", "tub", "pfr"],
        PreviewFormat::PaintShopPro,
        &[("pdn-Sunset_X6.pspimage", (800, 600)), ("corel-Halloween template 1.pspimage", (1080, 1920))],
    );
    if folder("psp").is_some() {
        assert!(found >= 30, "{found}");
    }
    let found = check("pixelmator", &["pxd"], PreviewFormat::Pixelmator, &[("magpie-Magpie.pxd", (1024, 755))]);
    if folder("pixelmator").is_some() {
        assert_eq!(found, 6);
    }
    let found = check(
        "sai",
        &["sai", "sai2"],
        PreviewFormat::Sai,
        &[("saire-linework.sai", (128, 128)), ("agd-Bright_herfece_1.sai2", (3000, 3000))],
    );
    if folder("sai").is_some() {
        assert!(found >= 7, "{found}");
    }
}

/// SAI 2's merged canvas, decoded from its deltas, against SAI's own JPEG
/// thumbnail of it: the two must show the same picture.
#[test]
fn sai2_canvas_matches_its_thumbnail() {
    let Some(dir) = folder("sai") else { return };
    for name in ["agd-Bright_herfece_1.sai2", "agd-Bright_Charsheet_1.sai2"] {
        let path = dir.join(name);
        let mut small = Limits::thumbnail();
        small.max_texture_size = Some(64);
        let canvas = match crate::sai::sai2_preview(&mut Source::open(&path).unwrap(), &Limits::viewer()) {
            Ok(t) => slerp_scene::textures::thumbnail(&t, 48),
            Err(e) => panic!("{name}: {e}"),
        };
        let thumb = slerp_scene::textures::thumbnail(
            &load_source(PreviewFormat::Sai, &mut Source::open(&path).unwrap(), &small).unwrap(),
            48,
        );
        assert_eq!((canvas.width, canvas.height), (thumb.width, thumb.height), "{name}");
        let diff: u64 = canvas.pixels.iter().zip(&thumb.pixels).map(|(a, b)| a.abs_diff(*b) as u64).sum();
        let mean = diff as f64 / canvas.pixels.len() as f64;
        eprintln!("{name}: mean difference {mean:.2}");
        assert!(mean < 12.0, "{name}: {mean}");
    }
}

/// Real files of every new kind, truncated and with bytes overwritten:
/// errors or pictures, never panics.
#[test]
fn damaged_cad_and_paint_files_fail_cleanly() {
    let picks = [
        ("dwg", "acadsharp-sample_AC1015.dwg", PreviewFormat::Dwg),
        ("dwg", "libredwg-2013-circle.dwg", PreviewFormat::Dwg),
        ("dwf", "aspose-for_layers_test.dwf", PreviewFormat::Dwf),
        ("dwf", "aspose-Tyrannosaurus.dwfx", PreviewFormat::Dwf),
        ("solidworks", "crazyflie-Longstrut.sldprt", PreviewFormat::SolidWorks),
        ("solidworks", "blussyya-C00_cube_10mm.SLDPRT", PreviewFormat::SolidWorks),
        ("inventor", "frc3344-Arm_Final.ipt", PreviewFormat::Inventor),
        ("revit", "mcd-Device_Tag.rfa", PreviewFormat::Revit),
        ("solidedge", "dfab16-A1_EE16B1019_2.par", PreviewFormat::SolidEdge),
        ("fusion360", "cr10-rx100m6-bracket.f3d", PreviewFormat::Fusion),
        ("iwork", "tika-testKeynote2013.key", PreviewFormat::Iwork),
        ("psp", "claunia-example.psp", PreviewFormat::PaintShopPro),
        ("pixelmator", "prufrock-AppIcon.pxd", PreviewFormat::Pixelmator),
        ("sai", "saire-linework.sai", PreviewFormat::Sai),
        ("sai", "agd-Bright_herfece_1.sai2", PreviewFormat::Sai),
    ];
    let limits = Limits::thumbnail();
    for (dir, name, format) in picks {
        let Some(dir) = folder(dir) else { continue };
        let Ok(bytes) = std::fs::read(dir.join(name)) else {
            eprintln!("skipping: {name} is missing");
            continue;
        };
        let start = std::time::Instant::now();
        for cut in (0..bytes.len()).step_by((bytes.len() / 61).max(1)) {
            let _ = read_stored(format, &mut Source::memory(bytes[..cut].to_vec()), &limits);
        }
        let mut seed = 0x2545_F491u32;
        for round in 0..200 {
            let mut copy = bytes.clone();
            for _ in 0..1 + round % 8 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let near = copy.len().min(16384);
                let i = match seed % 3 {
                    0 => seed as usize % copy.len(),
                    1 => seed as usize % near,
                    _ => copy.len() - 1 - seed as usize % near,
                };
                copy[i] = (seed >> 8) as u8;
            }
            let _ = read_stored(format, &mut Source::memory(copy), &limits);
        }
        eprintln!("{name}: damaged copies in {:?}", start.elapsed());
    }
    // A few damaged SAI 2 canvases at full size, where the deltas are read.
    let Some(dir) = folder("sai") else { return };
    let Ok(bytes) = std::fs::read(dir.join("agd-Bright_herfece_1.sai2")) else { return };
    let mut seed = 0x1234_5678u32;
    for _ in 0..6 {
        let mut copy = bytes.clone();
        for _ in 0..20 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let i = 46296 + seed as usize % 2_284_032;
            copy[i] = (seed >> 8) as u8;
        }
        let _ = read_stored(PreviewFormat::Sai, &mut Source::memory(copy), &Limits::viewer());
    }
}

/// Thumbnails read a little of a big file, so they take milliseconds (the
/// bound is loose because tests run side by side).
#[test]
fn big_cad_files_read_only_what_they_need() {
    let picks = [
        ("revit", "packt-FD_METRIC_Architecture_2021.rte", PreviewFormat::Revit),
        ("dwf", "flyfish-autodesk-floor-plans.dwfx", PreviewFormat::Dwf),
        ("sai", "agd-Bright_Charsheet_1.sai2", PreviewFormat::Sai),
        ("inventor", "respiraworks-enclosure.iam", PreviewFormat::Inventor),
    ];
    for (dir, name, format) in picks {
        let Some(dir) = folder(dir) else { continue };
        let path = dir.join(name);
        if !path.exists() {
            continue;
        }
        let start = std::time::Instant::now();
        load_path(format, &path, &Limits::thumbnail()).unwrap();
        let took = start.elapsed();
        eprintln!("{name}: {took:?}");
        assert!(took.as_millis() < 1000, "{name}: {took:?}");
    }
}

//endregion
