//! Retro pictures and game images through core: recognised by extension plus
//! size or by magic, the files that share their extensions with other
//! formats left alone, every sample in `local/assets/retro-extra` loaded and
//! thumbnailed inside the time budget, and damaged copies never panicking.

use slerp_core::{Kind, Prepared, load, prepare_thumbnail, sniff_file};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn samples() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/retro-extra");
    dir.exists().then_some(dir)
}

/// A scratch folder for synthetic files.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("slerp-retro-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn kind_of(path: &Path) -> Option<Kind> {
    sniff_file(path).unwrap()
}

#[test]
fn synthetic_files_are_recognised() {
    let dir = scratch("sniff");
    let write = |name: &str, bytes: &[u8]| {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    };
    // Koala: exact size and extension.
    let koala = write("a.kla", &vec![0u8; 10003]);
    assert!(matches!(kind_of(&koala), Some(Kind::Retro(_))));
    assert!(!Kind::Retro(slerp_retro::RetroFormat::Spectrum).usually_3d());
    // The same extension at another size is nothing.
    assert_eq!(kind_of(&write("b.kla", &vec![0u8; 9999])), None);
    // Windows screensavers keep their extension.
    let mut exe = b"MZ".to_vec();
    exe.resize(6912, 0);
    assert_eq!(kind_of(&write("saver.scr", &exe)), None);
    // A Spectrum dump is read by name when asked.
    assert!(matches!(kind_of(&write("spec.scr", &vec![0u8; 6912])), Some(Kind::Retro(_))));
    // Text files with escapes, PKCS#8 keys, GTK Blueprint sources and JSON
    // stay what they were.
    assert_eq!(kind_of(&write("notes.txt", b"hello \x1b[31mred\n")), None);
    assert_eq!(kind_of(&write("key.p8", b"-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n")), None);
    assert_eq!(kind_of(&write("ui.blp", b"using Gtk 4.0;\ntemplate $W : Gtk.Window {}\n")), None);
    assert_eq!(kind_of(&write("data.json", br#"{"frames": 3}"#)), None);
    assert_eq!(kind_of(&write("plain.diz", b"just text\r\n")), None);
    // PICO-8 text carts and TIC-80 chunk lists need their content.
    let cart = write("g.p8", b"pico-8 cartridge // http://www.pico-8.com\nversion 41\n__lua__\n");
    assert!(matches!(kind_of(&cart), Some(Kind::Retro(_))));
    // ANSI art: by name, and by a SAUCE record under any name.
    assert!(matches!(kind_of(&write("a.ans", b"\x1b[1;33mhello")), Some(Kind::Retro(_))));
    let mut sauce = b"\x1b[31mhello\x1a".to_vec();
    sauce.extend(b"SAUCE00");
    sauce.extend(vec![b' '; 35 + 20 + 20]);
    sauce.extend(b"20200101");
    sauce.extend(0u32.to_le_bytes());
    sauce.extend([1, 1]);
    sauce.extend(80u16.to_le_bytes());
    sauce.extend(25u16.to_le_bytes());
    sauce.extend([0u8; 4 + 1 + 1 + 22]);
    assert!(matches!(kind_of(&write("weird.bin", &sauce)), Some(Kind::Retro(_))));
    // Build tile sets and Art Studio share `.art`.
    let mut art = vec![1u8, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    art.extend([1, 0, 1, 0, 0, 0, 0, 0, 9]);
    assert!(matches!(kind_of(&write("TILES000.ART", &art)), Some(Kind::GameImage(_))));
    let mut studio = vec![0u8, 0x20];
    studio.resize(9009, 0);
    assert!(matches!(kind_of(&write("pic.art", &studio)), Some(Kind::Retro(_))));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every file under the sample folders that Slerp recognises.
fn recognised(dir: &Path, out: &mut Vec<(PathBuf, Kind)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            recognised(&p, out);
        } else if let Ok(Some(k)) = sniff_file(&p) {
            if matches!(k, Kind::Retro(_) | Kind::GameImage(_)) || is_atlas(&p, k) || is_quake_sprite(k) {
                out.push((p, k));
            }
        }
    }
}

fn is_atlas(p: &Path, k: Kind) -> bool {
    matches!(k, Kind::Sprite(slerp_sprites::SpriteFormat::Atlas)) && p.extension().is_some()
}

fn is_quake_sprite(k: Kind) -> bool {
    matches!(k, Kind::Quake(slerp_quake::QuakeFormat::Sprite | slerp_quake::QuakeFormat::Sprite2))
}

#[test]
fn every_sample_loads_and_thumbnails_quickly() {
    let Some(dir) = samples() else {
        eprintln!("skipping: local/assets/retro-extra is missing");
        return;
    };
    let mut files = Vec::new();
    recognised(&dir, &mut files);
    assert!(files.len() > 200, "only {} samples recognised", files.len());
    let (mut ok, mut failed, mut slowest) = (0, Vec::new(), Duration::ZERO);
    for (path, _) in &files {
        let start = Instant::now();
        match prepare_thumbnail(path, 256) {
            Ok(Prepared::Done(img)) => {
                assert!(img.width > 0 && img.height > 0 && img.width <= 256 && img.height <= 256, "{}", path.display());
                ok += 1;
            }
            Ok(Prepared::Render(_)) => panic!("{} asked for the GPU", path.display()),
            Err(e) => failed.push(format!("{}: {e}", path.file_name().unwrap().to_string_lossy())),
        }
        slowest = slowest.max(start.elapsed());
        // The whole file, as the viewer opens it, must not panic either.
        let _ = load(path, &slerp_core::scene::Limits::viewer());
    }
    eprintln!("{ok} thumbnails, {} failures, slowest {slowest:?}", failed.len());
    for f in &failed {
        eprintln!("  {f}");
    }
    // Unsupported variants (EAC and PVRTC II textures, float PVR, cube
    // maps, content that isn't an image, files that carry no picture) fail
    // with an error, never with a panic.
    assert!(failed.len() * 10 < files.len(), "{} of {} failed", failed.len(), files.len());
    assert!(slowest < Duration::from_secs(2), "slowest thumbnail took {slowest:?}");
}

#[test]
fn damaged_samples_never_panic() {
    let Some(dir) = samples() else {
        eprintln!("skipping: local/assets/retro-extra is missing");
        return;
    };
    let mut files = Vec::new();
    recognised(&dir, &mut files);
    let tmp = scratch("fuzz");
    let mut seed = 0x9e3779b9u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let limits = slerp_core::thumbnail_limits(256);
    // `SLERP_FUZZ_ROUNDS` runs more damaged copies of each sample.
    let rounds: usize = std::env::var("SLERP_FUZZ_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(6);
    let mut tried = 0;
    for (path, kind) in files.iter().filter(|(p, _)| std::fs::metadata(p).map_or(false, |m| m.len() < 400_000)) {
        let original = std::fs::read(path).unwrap();
        let name = path.file_name().unwrap();
        for round in 0..rounds {
            let mut bytes = original.clone();
            match round % 6 {
                // Cut off at a random point.
                0 | 1 => bytes.truncate(next() as usize % bytes.len().max(1)),
                // A few bytes replaced, anywhere.
                2..=4 => {
                    for _ in 0..1 + next() % 12 {
                        let at = next() as usize % bytes.len().max(1);
                        if let Some(b) = bytes.get_mut(at) {
                            *b = next() as u8;
                        }
                    }
                }
                // The header's numbers set to extremes.
                _ => {
                    for at in (0..bytes.len().min(160)).step_by(4) {
                        if next() % 5 == 0 {
                            for b in bytes.iter_mut().skip(at).take(4) {
                                *b = 0xff;
                            }
                        }
                    }
                }
            }
            let copy = tmp.join(name);
            std::fs::write(&copy, &bytes).unwrap();
            let start = Instant::now();
            // Through the loader of the original kind, whatever the damaged
            // bytes now look like.
            let _ = slerp_core::load_kind(*kind, &copy, &limits);
            let _ = prepare_thumbnail(&copy, 256);
            assert!(start.elapsed() < Duration::from_secs(5), "{} took {:?}", copy.display(), start.elapsed());
            tried += 1;
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(tried > 100 * rounds, "only {tried} damaged files tried");
}
