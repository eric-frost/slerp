//! The reader against real archives under `local/assets/`, skipped when a
//! file hasn't been downloaded.

use slerp_archive::{Archive, ArchiveKind, DataKind, EntryKind};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn asset(relative: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../local/assets").join(relative);
    if p.exists() {
        Some(p)
    } else {
        eprintln!("skipping: {} is missing", p.display());
        None
    }
}

/// Lists an archive and reads every entry, checking that nothing fails
/// (except entries the archive itself marks locked).  Returns the entry
/// counts by kind.
fn read_everything(path: &Path) -> (Archive, BTreeMap<&'static str, usize>) {
    let started = Instant::now();
    let a = Archive::open(path).unwrap();
    let listed = started.elapsed();
    let mut kinds: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut total = 0u64;
    for e in a.entries() {
        *kinds.entry(e.kind.label()).or_default() += 1;
        if e.locked || e.size > 64 << 20 {
            continue;
        }
        let bytes = a.read(e).unwrap_or_else(|err| panic!("{}: {err}", e.path));
        assert_eq!(bytes.len() as u64, e.size, "{}", e.path);
        total += bytes.len() as u64;
    }
    eprintln!(
        "{}: {} ({} entries) listed in {:.1} ms, all read ({} MB) in {:.0} ms; {kinds:?}",
        path.file_name().unwrap().to_string_lossy(),
        a.label(),
        a.entries().len(),
        listed.as_secs_f64() * 1000.0,
        total >> 20,
        started.elapsed().as_secs_f64() * 1000.0,
    );
    (a, kinds)
}

#[test]
fn freedoom_wads() {
    for name in ["doom/freedoom/freedoom1.wad", "doom/freedoom/freedoom2.wad"] {
        let Some(path) = asset(name) else { continue };
        let (a, kinds) = read_everything(&path);
        assert_eq!(a.kind(), ArchiveKind::DoomWad);
        assert!(kinds["sprite"] > 400 && kinds["flat"] > 100 && kinds["map"] > 100, "{kinds:?}");
        assert!(a.groups().iter().any(|g| g.path == "Sprites"));
        assert!(a.groups().iter().filter(|g| g.kind == slerp_archive::GroupKind::Map).count() >= 30);
        assert!(a.entries().iter().any(|e| e.data == DataKind::Playpal));
        assert!(a.entries().iter().any(|e| e.data == DataKind::DmxSound));
        assert!(a.entries().iter().any(|e| matches!(e.data, DataKind::Mus | DataKind::Midi)));
        assert!(a.entries().iter().any(|e| e.data == DataKind::TextureDefs));
        // Few lumps are left unrecognised.
        let unknown = a.entries().iter().filter(|e| e.kind == EntryKind::Other && e.size > 0).count();
        assert!(unknown < 12, "{unknown} unrecognised lumps");
    }
}

#[test]
fn commercial_iwads() {
    for name in ["DOOM.WAD", "DOOM2.WAD", "TNT.WAD", "PLUTONIA.WAD"] {
        let Some(path) = asset(&format!("doom-commercial/{name}")) else { continue };
        read_everything(&path);
    }
}

#[test]
fn quake_paks_and_wads() {
    for name in ["quake/librequake/id1/pak0.pak", "quake/librequake/id1/pak1.pak"] {
        let Some(path) = asset(name) else { continue };
        let (a, kinds) = read_everything(&path);
        assert_eq!(a.kind(), ArchiveKind::Pak);
        assert!(kinds.contains_key("map") || kinds.contains_key("picture"), "{kinds:?}");
    }
    for name in ["quake/librequake-texwads/lq_dev.wad", "halflife/valve/halflife.wad", "halflife/hlwad.wad"] {
        let Some(path) = asset(name) else { continue };
        let (a, kinds) = read_everything(&path);
        assert!(matches!(a.kind(), ArchiveKind::Wad2 | ArchiveKind::Wad3));
        assert!(kinds["texture"] > 0, "{kinds:?}");
    }
}

#[test]
fn big_pk3_lists_quickly() {
    let Some(path) = asset("doom-extra/elementalism/Elementalism_Phase1_RC2_v1.2.pk3") else { return };
    let started = Instant::now();
    let a = Archive::open(&path).unwrap();
    let listed = started.elapsed();
    eprintln!("Elementalism: {} entries listed in {:.1} ms", a.entries().len(), listed.as_secs_f64() * 1000.0);
    assert!(a.entries().len() > 1000);
    assert!(listed.as_millis() < 500, "{listed:?}");
    // Read a few entries of each kind.
    let mut seen = std::collections::HashSet::new();
    for e in a.entries() {
        if e.size < 8 << 20 && seen.insert(e.kind) {
            assert_eq!(a.read(e).unwrap().len() as u64, e.size, "{}", e.path);
        }
    }
}

#[test]
fn nonbsd_pk3s() {
    for name in ["quake/assimp-nonbsd/PK3/SGDTT3.pk3", "quake/assimp-nonbsd/PK3/invalid_meshverts_size.pk3"] {
        let Some(path) = asset(name) else { continue };
        read_everything(&path);
    }
}

/// Godot packs of every format version, real exports (`archives-extra`).
#[test]
fn godot_packs() {
    use slerp_archive::ArchiveError;
    let Some(dir) = asset("archives-extra/pck") else { return };
    let mut seen = 0;
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let opened = Archive::open(&path);
        if name.contains("encrypted-dir") {
            // The directory is encrypted: refused with a message.
            let err = opened.unwrap_err();
            assert!(matches!(err, ArchiveError::Encrypted(_)), "{name}: {err}");
            assert!(err.to_string().contains("encrypted"));
            continue;
        }
        let a = opened.unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(a.kind(), ArchiveKind::Pck, "{name}");
        assert!(a.entries().len() >= 15, "{name}: {} entries", a.entries().len());
        let locked = a.entries().iter().filter(|e| e.locked).count();
        if name.contains("encrypted-files") {
            assert_eq!(locked, 9, "{name}");
        } else {
            assert_eq!(locked, 0, "{name}");
        }
        for e in a.entries().iter().filter(|e| !e.locked) {
            let bytes = a.read(e).unwrap_or_else(|err| panic!("{name}: {}: {err}", e.path));
            assert_eq!(bytes.len() as u64, e.size, "{name}: {}", e.path);
        }
        // Every project has its settings and icon at the top level.
        assert!(a.entries().iter().any(|e| e.path == "project.binary" || e.path == "project.godot"), "{name}");
        assert!(a.entries().iter().any(|e| e.path.starts_with("icon.")), "{name}");
        eprintln!("{name}: {} ({} entries)", a.label(), a.entries().len());
        seen += 1;
    }
    assert!(seen >= 8, "{seen} packs");
}

/// Valve packs: multi-part, single-file, preloaded, Source 2, and the ones
/// whose archive parts are missing (listed, but their files fail to read).
#[test]
fn valve_packs() {
    let Some(dir) = asset("archives-extra/vpk") else { return };
    let mut opened = 0;
    for sub in ["valvepython", "plumber", "sourcepak", "source2", "sourcepak-game/portal", "sourcepak-game/tf2"] {
        let folder = dir.join(sub);
        let Ok(read) = std::fs::read_dir(&folder) else { continue };
        for entry in read.flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            // Archive parts have no directory of their own.
            let is_directory = name.ends_with("_dir.vpk")
                || !name.rsplit_once('_').is_some_and(|(_, n)| n.starts_with(char::is_numeric));
            if !is_directory || !name.ends_with(".vpk") {
                continue;
            }
            let a = match Archive::open(&path) {
                Ok(a) => a,
                // Respawn's variant has another version number.
                Err(slerp_archive::ArchiveError::Unsupported(m)) if name.contains("revpk") => {
                    eprintln!("{name}: {m}");
                    continue;
                }
                Err(e) => panic!("{name}: {e}"),
            };
            assert_eq!(a.kind(), ArchiveKind::Vpk);
            let (mut ok, mut failed) = (0, 0);
            for e in a.entries() {
                match a.read(e) {
                    Ok(b) => {
                        assert_eq!(b.len() as u64, e.size, "{name}: {}", e.path);
                        ok += 1;
                    }
                    Err(_) => failed += 1,
                }
            }
            eprintln!("{sub}/{name}: {} - {} entries, {ok} read, {failed} unreadable", a.label(), a.entries().len());
            // Packs whose parts are on disk read completely.
            if sub == "valvepython" || sub == "plumber" || sub == "source2" {
                assert_eq!(failed, 0, "{sub}/{name}");
                assert!(ok > 0);
            }
            opened += 1;
        }
    }
    assert!(opened >= 6, "{opened} packs");
}

#[test]
fn build_group_files() {
    for (name, min) in [
        ("archives-extra/grp/duke3d-shareware/DUKE3D.GRP", 200),
        ("archives-extra/grp/kenbuild/STUFF.DAT", 7),
        ("archives-extra/grp/synthetic/small.grp", 3),
    ] {
        let Some(path) = asset(name) else { continue };
        let (a, kinds) = read_everything(&path);
        assert_eq!(a.kind(), ArchiveKind::Grp);
        assert!(a.entries().len() >= min, "{name}: {kinds:?}");
    }
    if let Some(path) = asset("archives-extra/grp/synthetic/empty.grp") {
        assert!(Archive::open(&path).unwrap().entries().is_empty());
    }
}

/// Unity packages from GitHub releases: every asset listed under its
/// project path and readable, and the preview pictures found quickly.
#[test]
fn unity_packages() {
    let Some(dir) = asset("editors-extra/unitypackage") else { return };
    let expect = [
        ("univrm-UniVRM-0.131.3_011b.unitypackage", 36),
        ("uraymarching-uRaymarching-v2.3.0.unitypackage", 29),
        ("udesktopduplication-uDesktopDuplication@v1.8.0.unitypackage", 11),
        ("ushadertemplate-uShaderTemplate@v1.0.1.unitypackage", 2),
        ("unitask-UniTask.2.5.11.unitypackage", 0),
    ];
    let mut n = 0;
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "unitypackage") {
            continue;
        }
        let head: Vec<u8> = std::fs::read(&path).unwrap().into_iter().take(4096).collect();
        assert_eq!(slerp_archive::sniff(&head), Some(ArchiveKind::UnityPackage), "{}", path.display());
        let (a, _) = read_everything(&path);
        assert!(!a.entries().is_empty());
        assert!(a.entries().iter().all(|e| e.path.starts_with("Assets/") || e.path.starts_with("Packages/")));
        let started = Instant::now();
        let pictures = slerp_archive::unity::pictures(&path, 200, std::time::Duration::from_secs(10)).unwrap();
        let previews = pictures.iter().filter(|p| p.stored_preview).count();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        eprintln!("{name}: {previews} previews of {} pictures in {:?}", pictures.len(), started.elapsed());
        if let Some((_, count)) = expect.iter().find(|e| e.0 == name) {
            assert_eq!(previews, *count, "{name}");
        }
        let started = Instant::now();
        let nine = slerp_archive::unity::pictures(&path, 9, std::time::Duration::from_millis(250)).unwrap();
        assert!(nine.len() <= 9 && started.elapsed().as_millis() < 400, "{name}: {:?}", started.elapsed());
        n += 1;
    }
    assert!(n >= 7, "{n}");
}
