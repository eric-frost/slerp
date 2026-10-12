//! The manager in a made-up home folder and system: apply, apply again,
//! reset, and the files left exactly as they were.

use super::*;
use std::collections::BTreeMap;

/// A home folder and a system data folder under the temporary folder.
struct Fixture {
    root: PathBuf,
    env: Env,
}

const MIMEAPPS: &str =
    "[Added Associations]\ntext/csv=org.kde.kate.desktop;\n\n[Default Applications]\nmodel/gltf-binary=f3d.desktop;\n";
const DOLPHINRC: &str = "MenuBar=Disabled\n\n[PreviewSettings]\nPlugins=imagethumbnail,jpegthumbnail,glycin,slerp\n\n[VersionControl]\nenabledPlugins=Git\n";

impl Fixture {
    fn new(name: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!("slerp-associate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let sys = root.join("sys");
        let write = |path: PathBuf, text: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        // The system: Slerp installed as setup.yml installs it, glycin's
        // thumbnailer, F3D's desktop entry, and a small MIME database.
        write(sys.join("thumbnailers/slerp.thumbnailer"), &slerp_formats::generate::system_thumbnailer());
        write(
            sys.join("thumbnailers/glycin.thumbnailer"),
            "[Thumbnailer Entry]\nExec=glycin-thumbnailer -i %i -o %o -s %s\nMimeType=image/png;image/x-dds;\n",
        );
        write(sys.join("applications/slerp.desktop"), &slerp_formats::generate::desktop());
        write(
            sys.join("applications/f3d.desktop"),
            "[Desktop Entry]\nType=Application\nName=F3D\nExec=f3d %f\nMimeType=model/gltf-binary;model/stl;\n",
        );
        let types: Vec<&str> = slerp_formats::FORMATS.iter().flat_map(|f| f.listed_mime()).collect();
        write(sys.join("mime/types"), &(types.join("\n") + "\n"));
        write(sys.join("mime/aliases"), "image/vnd.ms-dds image/x-dds\n");
        write(sys.join("mime/subclasses"), "model/x-vrm model/gltf-binary\napplication/x-materialx application/xml\n");
        // KDE's image plugin, which claims DDS.
        let plugin = sys.join("qt/kf6/thumbcreator/imagethumbnail.so");
        std::fs::create_dir_all(plugin.parent().unwrap()).unwrap();
        std::fs::write(plugin, kde_plugin("imagethumbnail", &["image/png", "image/x-dds"])).unwrap();
        // The user's own settings.
        write(root.join("home/.config/mimeapps.list"), MIMEAPPS);
        write(root.join("home/.config/dolphinrc"), DOLPHINRC);
        let home = root.join("home");
        let env = Env {
            config_home: home.join(".config"),
            config_dirs: Vec::new(),
            data_home: home.join(".local/share"),
            data_dirs: vec![sys.clone()],
            cache_home: home.join(".cache"),
            desktops: vec!["kde".into()],
            qt_plugin_dirs: vec![sys.join("qt")],
            program: "/bin/sh".into(),
            gnome: false,
            refresh: false,
            home,
        };
        Fixture { root, env }
    }

    /// Every file under the home folder but the cache, with its text.
    fn snapshot(&self) -> BTreeMap<PathBuf, String> {
        let mut out = BTreeMap::new();
        let mut stack = vec![self.env.home.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.starts_with(&self.env.cache_home) {
                    continue;
                }
                if p.is_dir() {
                    out.insert(p.clone(), "<dir>".into());
                    stack.push(p);
                } else {
                    out.insert(p.clone(), std::fs::read_to_string(&p).unwrap());
                }
            }
        }
        out
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.env.home.join(rel)).unwrap_or_default()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A file shaped like a KDE thumbnail plugin's metadata note.
fn kde_plugin(id: &str, types: &[&str]) -> Vec<u8> {
    let text = |s: &str| {
        let mut v = vec![0x78, s.len() as u8];
        v.extend(s.as_bytes());
        v
    };
    let mut cbor = vec![0xbf, 0x04, 0xa1];
    cbor.extend(text("KPlugin"));
    cbor.push(0xa2);
    cbor.extend(text("Id"));
    cbor.extend(text(id));
    cbor.extend(text("MimeTypes"));
    cbor.push(0x80 + types.len() as u8);
    for t in types {
        cbor.extend(text(t));
    }
    cbor.push(0xff);
    let mut file = b"ELF?".to_vec();
    file.extend(b"qt-project!\0\x01\x06\x0a\x01");
    file.extend(cbor);
    file
}

fn f(id: &str) -> &'static slerp_formats::Format {
    slerp_formats::by_id(id).unwrap()
}

#[test]
fn apply_twice_then_reset_leaves_every_file_as_it_was() {
    let fx = Fixture::new("roundtrip");
    let before = fx.snapshot();
    let mut choices = Choices::default();
    choices.set_thumbnails(f("glb"), false);
    choices.set_thumbnails(f("gif"), true);
    choices.set_open_by_default(f("glb"), true);
    choices.set_open_by_default(f("visio"), true);
    choices.set_replace_kde_previews(f("dds"), true);
    let mut record = Record::default();

    // 1. Apply.
    let report = apply(&fx.env, &mut choices, &mut record, false);
    assert!(!report.choices_changed, "{report:?}");
    let thumbnailer = fx.read(".local/share/thumbnailers/slerp.thumbnailer");
    assert!(thumbnailer.contains(";image/gif;") && !thumbnailer.contains("model/gltf-binary"), "{thumbnailer}");
    assert!(thumbnailer.contains("Exec=/bin/sh thumbnail %s %i %o"));
    let dolphin = fx.read(".config/dolphinrc");
    assert!(dolphin.contains("Plugins=jpegthumbnail,glycin,slerp\n"), "{dolphin}");
    assert!(report.notes.iter().any(|n| n.contains("imagethumbnail is off") && n.contains("image/png")), "{report:?}");
    let mimeapps = fx.read(".config/mimeapps.list");
    assert!(mimeapps.contains("model/gltf-binary=slerp.desktop;\n"), "{mimeapps}");
    // Visio isn't in slerp.desktop, so it is also associated.
    assert!(
        mimeapps.contains("[Added Associations]\ntext/csv=org.kde.kate.desktop;\napplication/vnd.visio=slerp.desktop;")
    );
    assert!(mimeapps.contains("application/vnd.visio=slerp.desktop;\n"));

    // 2. Applying again changes nothing.
    let snap = fx.snapshot();
    let again = apply(&fx.env, &mut choices, &mut record, false);
    assert_eq!(fx.snapshot(), snap, "{again:?}");
    assert!(!again.changes.iter().any(|c| c.starts_with("write") || c.contains("mimeapps") || c.contains("dolphinrc")));

    // 3. A choice switched back restores just its entry.
    choices.set_open_by_default(f("visio"), false);
    apply(&fx.env, &mut choices, &mut record, false);
    let mimeapps = fx.read(".config/mimeapps.list");
    assert!(!mimeapps.contains("visio") && mimeapps.contains("model/gltf-binary=slerp.desktop;"));

    // 4. Reset: byte for byte, the thumbnailer folder gone again.
    let report = reset(&fx.env, &mut record, false);
    assert!(!report.changes.is_empty());
    assert_eq!(fx.snapshot(), before);
    assert!(record.is_empty(), "{record:?}");
}

#[test]
fn the_defaults_change_only_slerps_own_file() {
    let fx = Fixture::new("defaults");
    let before = fx.snapshot();
    let mut record = Record::default();
    apply(&fx.env, &mut Choices::default(), &mut record, false);
    let after = fx.snapshot();
    let changed: Vec<&PathBuf> = after.keys().filter(|p| before.get(*p) != after.get(*p)).collect();
    // Slerp is already in Dolphin's list here, and nothing asks for a
    // default app: only the per-user thumbnailer (and its folders) appear.
    let own = fx.env.data_home.join("thumbnailers");
    assert!(changed.iter().all(|p| p.starts_with(&own) || own.starts_with(p)), "{changed:?}");
    let own = fx.read(".local/share/thumbnailers/slerp.thumbnailer");
    let system = slerp_formats::generate::system_thumbnailer();
    assert_eq!(own.lines().last(), system.lines().last(), "the same MIME types as the system-wide file");
    reset(&fx.env, &mut record, false);
    assert_eq!(fx.snapshot(), before);
}

#[test]
fn dry_runs_change_nothing() {
    let fx = Fixture::new("dry");
    let before = fx.snapshot();
    let mut choices = Choices::default();
    choices.set_open_by_default(f("glb"), true);
    let mut record = Record::default();
    let report = apply(&fx.env, &mut choices, &mut record, true);
    assert!(report.changes.iter().any(|c| c.contains("mimeapps")), "{report:?}");
    assert_eq!(fx.snapshot(), before);
    assert_eq!(record, Record::default());
}

#[test]
fn a_default_changed_elsewhere_is_given_up() {
    let fx = Fixture::new("override");
    let mut choices = Choices::default();
    choices.set_open_by_default(f("glb"), true);
    let mut record = Record::default();
    apply(&fx.env, &mut choices, &mut record, false);
    // The user picks F3D again in KDE's settings.
    let path = fx.env.config_home.join("mimeapps.list");
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("model/gltf-binary=slerp.desktop;", "model/gltf-binary=f3d.desktop;");
    std::fs::write(&path, &text).unwrap();
    let report = apply(&fx.env, &mut choices, &mut record, false);
    assert!(report.choices_changed && !choices.open_by_default(f("glb")), "{report:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
}

#[test]
fn slerp_taken_out_of_dolphin_stays_out_until_the_choices_change() {
    let fx = Fixture::new("declined");
    let path = fx.env.config_home.join("dolphinrc");
    std::fs::write(&path, "[PreviewSettings]\nPlugins=imagethumbnail\n").unwrap();
    let mut choices = Choices::default();
    let mut record = Record::default();
    apply(&fx.env, &mut choices, &mut record, false);
    assert!(fx.read(".config/dolphinrc").contains("Plugins=imagethumbnail,slerp"));
    // The user unticks Slerp in Dolphin's settings; applying again (after an
    // update) leaves it out.
    std::fs::write(&path, "[PreviewSettings]\nPlugins=imagethumbnail\n").unwrap();
    apply(&fx.env, &mut choices, &mut record, false);
    apply(&fx.env, &mut choices, &mut record, false);
    assert_eq!(fx.read(".config/dolphinrc"), "[PreviewSettings]\nPlugins=imagethumbnail\n");
    // Changing Slerp's formats asks for it again.
    choices.set_thumbnails(f("gif"), true);
    apply(&fx.env, &mut choices, &mut record, false);
    assert!(fx.read(".config/dolphinrc").contains("Plugins=imagethumbnail,slerp"));
}

#[test]
fn gnome_gets_files_that_claim_switched_off_types_first() {
    let mut fx = Fixture::new("gnome");
    fx.env.gnome = true;
    fx.env.desktops = vec!["gnome".into()];
    let before = fx.snapshot();
    let mut choices = Choices::default();
    choices.set_thumbnails(f("dds"), false);
    choices.set_thumbnails(f("glb"), false);
    let mut record = Record::default();
    apply(&fx.env, &mut choices, &mut record, false);
    // GNOME's rule: every file counts, first claim wins, user's folder first.
    let all = apps::thumbnailers(&fx.env);
    let who = |mime: &str| apps::gnome_choice(&all, mime).map(|t| t.id.clone());
    assert_eq!(who("image/x-dds").as_deref(), Some("slerp-handoff-glycin"));
    assert_eq!(apps::gnome_choice(&all, "image/x-dds").unwrap().exec, "glycin-thumbnailer -i %i -o %o -s %s");
    assert_eq!(who("model/gltf-binary").as_deref(), Some("slerp-off"));
    assert_eq!(who("image/ktx2").as_deref(), Some("slerp"));
    // KDE's rule (one file per name) still sees only Slerp's own list.
    let kde = apps::by_name_first(&all);
    let slerp = kde.iter().find(|t| t.id == "slerp").unwrap();
    assert!(slerp.path.starts_with(&fx.env.data_home) && !slerp.mime_types.iter().any(|m| m == "image/x-dds"));
    reset(&fx.env, &mut record, false);
    assert_eq!(fx.snapshot(), before);
}

#[test]
fn status_names_rivals_and_handlers() {
    let fx = Fixture::new("status");
    let mut choices = Choices::default();
    let list = status(&fx.env, &choices);
    let dds = list.iter().find(|s| s.format.id == "dds").unwrap();
    assert_eq!(dds.kde_rivals, ["imagethumbnail"]);
    assert_eq!(dds.thumbnails_by.as_deref(), Some("KDE's imagethumbnail"));
    assert!(dds.notes.iter().any(|n| n.contains("KDE's own plugins win")));
    let glb = list.iter().find(|s| s.format.id == "glb").unwrap();
    assert_eq!((glb.thumbnails_by.as_deref(), glb.opens_with.as_deref()), (Some("Slerp"), Some("F3D")));
    let mtlx = list.iter().find(|s| s.format.id == "materialx").unwrap();
    assert_eq!(mtlx.opens_with.as_deref(), Some("Slerp"));
    choices.set_open_by_default(f("glb"), true);
    let mut record = Record::default();
    apply(&fx.env, &mut choices, &mut record, false);
    let glb = status(&fx.env, &choices).into_iter().find(|s| s.format.id == "glb").unwrap();
    assert_eq!(glb.opens_with.as_deref(), Some("Slerp"));
}
