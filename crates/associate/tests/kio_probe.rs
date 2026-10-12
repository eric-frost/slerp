//! Dolphin's view of the choices, through KIO itself: the KIO probe
//! (`tools/kio-probe`, built into `local/kio-probe`) asks for previews the
//! way Dolphin does, with a temporary config and data folder, after the
//! manager has applied a choice there.  A format switched off stops getting
//! Slerp's thumbnails, the others keep them, and switching it on again
//! brings them back.  Skipped where KDE's KIO, the probe, an installed
//! Slerp or the test files are missing.

#![cfg(not(any(windows, target_os = "macos")))]

use slerp_associate::Record;
use slerp_associate::freedesktop::{self, Env};
use slerp_core::choices::Choices;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Runs the probe on `files` with `home`'s folders, and returns its lines.
fn probe(home: &Path, files: &[&Path]) -> Vec<String> {
    let out = Command::new(root().join("local/kio-probe"))
        .args(files)
        .env("QT_QPA_PLATFORM", "offscreen")
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .output()
        .expect("kio-probe runs");
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect()
}

/// Whether the probe's line for `file` says it got a preview.
fn previewed(lines: &[String], file: &Path) -> bool {
    let name = file.to_string_lossy();
    lines.iter().any(|l| l.starts_with("OK") && l.ends_with(name.as_ref()))
}

#[test]
fn a_format_switched_off_loses_slerps_thumbnails_in_kde_and_gets_them_back() {
    let glb = root().join("local/assets/gltf/khronos/Duck/glTF-Binary/Duck.glb");
    let ktx2 = root().join("local/assets/textures-extra/basisu-2.50/etc1s.ktx2");
    let installed = Path::new("/usr/local/share/thumbnailers/slerp.thumbnailer").exists();
    if !root().join("local/kio-probe").exists() || !glb.exists() || !ktx2.exists() || !installed {
        eprintln!("needs local/kio-probe, an installed Slerp and the test assets; skipped");
        return;
    }
    // 1. A home folder with only Dolphin's plugin list, Slerp in it.
    let home = std::env::temp_dir().join(format!("slerp-kio-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".config")).unwrap();
    std::fs::write(home.join(".config/dolphinrc"), "[PreviewSettings]\nPlugins=imagethumbnail,jpegthumbnail,slerp\n")
        .unwrap();
    let mut env = Env::from_system();
    env.home = home.clone();
    env.config_home = home.join(".config");
    env.data_home = home.join(".local/share");
    env.cache_home = home.join(".cache");
    env.desktops = vec!["kde".into()];
    env.gnome = false;
    env.refresh = false;
    let files = [glb.as_path(), ktx2.as_path()];
    let before = probe(&home, &files);
    assert!(previewed(&before, &glb) && previewed(&before, &ktx2), "{before:?}");

    // 2. glTF binary off: KIO finds no thumbnailer for it, KTX2 keeps Slerp's.
    let mut choices = Choices::default();
    choices.set_thumbnails(slerp_formats::by_id("glb").unwrap(), false);
    let mut record = Record::default();
    freedesktop::apply(&env, &mut choices, &mut record, false);
    let off = probe(&home, &files);
    assert!(!previewed(&off, &glb) && previewed(&off, &ktx2), "{off:?}");

    // 3. On again.
    choices.set_thumbnails(slerp_formats::by_id("glb").unwrap(), true);
    freedesktop::apply(&env, &mut choices, &mut record, false);
    let on = probe(&home, &files);
    assert!(previewed(&on, &glb) && previewed(&on, &ktx2), "{on:?}");

    // 4. Reset leaves the home folder as it was.
    freedesktop::reset(&env, &mut record, false);
    assert!(!home.join(".local").exists());
    assert_eq!(
        std::fs::read_to_string(home.join(".config/dolphinrc")).unwrap(),
        "[PreviewSettings]\nPlugins=imagethumbnail,jpegthumbnail,slerp\n"
    );
    eprintln!("before: {before:?}\noff: {off:?}\non: {on:?}");
    let _ = std::fs::remove_dir_all(&home);
}
