use super::*;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/*┌─────────────────╮
| The table       |
└─────────────────╯*/
//region The table

#[test]
fn ids_are_unique_and_plain() {
    let mut seen = HashSet::new();
    for f in FORMATS {
        assert!(seen.insert(f.id), "{} twice", f.id);
        assert!(!f.id.is_empty() && f.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
        assert!(!f.name.is_empty(), "{} has no name", f.id);
        // A category's id must never be mistaken for a format's.
        assert!(Category::from_id(f.id).is_none(), "{} is also a category", f.id);
    }
}

#[test]
fn every_format_has_a_mime_type_and_each_type_one_format() {
    let mut owner: HashMap<&str, &str> = HashMap::new();
    for f in FORMATS {
        assert!(!f.mime.is_empty(), "{} has no MIME type", f.id);
        assert!(
            matches!(f.mime[0].origin, Origin::Own | Origin::System | Origin::Extends),
            "{}: the main type must be a real one, not an alias",
            f.id
        );
        for m in f.mime {
            assert!(m.name.contains('/') && m.name == m.name.trim(), "{}: {}", f.id, m.name);
            if let Some(other) = owner.insert(m.name, f.id) {
                assert_eq!(other, f.id, "{} is claimed by {other} and {}", m.name, f.id);
            }
        }
    }
}

#[test]
fn extensions_are_lower_case_and_unique_per_format() {
    for f in FORMATS {
        let mut seen = HashSet::new();
        // A type may repeat an extension another type of the same format
        // has (Quake and Half-Life `.mdl`), so count per type.
        for m in f.mime {
            let mut in_type = HashSet::new();
            for e in m.exts {
                assert!(in_type.insert(*e), "{}: {e} twice in {}", f.id, m.name);
                assert!(!e.is_empty() && !e.starts_with('.') && !e.contains('*'), "{}: {e}", f.id);
                assert!(e.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.'), "{}: {e}", f.id);
                seen.insert(*e);
            }
            for p in m.patterns {
                assert!(p.contains('*') || p.contains('.'), "{}: {p} is not a glob", f.id);
            }
        }
        for e in f.by_content {
            assert!(!seen.contains(e), "{}: {e} is both claimed and confirmed by content", f.id);
            assert!(e.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()), "{}: {e}", f.id);
        }
    }
}

#[test]
fn defaults_respect_better_previewers() {
    for f in FORMATS {
        let both = f.others & (KDE | GNOME) == KDE | GNOME;
        if both && f.thumbnails == Thumbs::On {
            assert!(!f.better.is_empty(), "{}: KDE and GNOME preview it; say why Slerp's are on", f.id);
        }
        if f.thumbnails == Thumbs::Never {
            assert_eq!(f.qt, Qt::No, "{}: nothing to draw, so nothing for Qt", f.id);
        }
    }
    // The ordinary pictures stay off.
    for id in ["png", "jpeg", "gif", "webp", "avif", "heif", "jxl", "tiff", "bmp", "ico"] {
        assert_eq!(by_id(id).unwrap().thumbnails, Thumbs::Off, "{id}");
    }
}

#[test]
fn qt_keys_avoid_other_plugins() {
    // Keys Qt's own plugins and KDE's kimageformats read; claiming one
    // would race them for ordinary pictures.
    const TAKEN: &[&str] = &[
        "ani", "avif", "avifs", "bmp", "cur", "dds", "eps", "epsf", "epsi", "exr", "gif", "hdr", "heic", "heif",
        "icns", "ico", "iff", "ilbm", "j2k", "jp2", "jpeg", "jpf", "jpg", "jpx", "jxl", "jxr", "kra", "lbm", "mng",
        "ora", "pbm", "pcx", "pfm", "pgm", "pic", "png", "ppm", "psd", "pxr", "qoi", "ras", "raw", "rgb", "sct", "svg",
        "svgz", "tga", "tif", "tiff", "wbmp", "webp", "xbm", "xcf", "xpm",
    ];
    let mut pairs = HashSet::new();
    for (k, m, _, _) in generate::qt_entries() {
        assert!(!TAKEN.contains(&k), "Qt key {k} belongs to another plugin");
        assert!(pairs.insert((k, m)), "{k} {m} twice");
        assert!(by_mime(m).is_some_and(|f| f.qt != Qt::No));
    }
}

#[test]
fn mac_declarations_are_consistent() {
    let mut ids: HashMap<&str, &str> = HashMap::new();
    for f in FORMATS {
        for u in f.imported_utis() {
            // Slerp imports under the owners' domains; Apple's own `public.`
            // names are the system's, except Godot's public.res.
            assert!(!u.id.starts_with("public.") || u.id == "public.res", "{}: {}", f.id, u.id);
            assert!(!u.id.starts_with("com.apple.") || u.id.starts_with("com.apple.apple2"), "{}: {}", f.id, u.id);
            let exts = if u.exts.is_empty() { f.simple_extensions() } else { u.exts.to_vec() };
            assert!(!exts.is_empty(), "{}: {} claims no extension", f.id, u.id);
            for e in &exts {
                assert!(f.extensions().contains(e), "{}: {} claims {e}, which isn't the format's", f.id, u.id);
            }
            // Two formats may share a declaration only when they are one
            // kind of file on macOS (Quake's and Doom's WADs).
            if let Some(other) = ids.insert(u.id, f.id) {
                assert!(other == f.id || u.id == "com.idsoftware.wad", "{} in {other} and {}", u.id, f.id);
            }
        }
    }
}

#[test]
fn lookups() {
    assert_eq!(by_id("glb").unwrap().main_mime(), "model/gltf-binary");
    assert_eq!(by_mime("model/x-halflife-mdl").unwrap().id, "quake-mdl");
    let ids = |name: &str| by_file_name(name).iter().map(|f| f.id).collect::<Vec<_>>();
    assert_eq!(ids("brain.NII.GZ"), ["nifti"]);
    assert_eq!(ids("x.ome.tif"), ["ome-tiff"]);
    assert_eq!(ids("cells.tif"), ["tiff"]);
    assert_eq!(ids("m31.fits.fz"), ["fits"]);
    assert_eq!(ids("tiles.art"), ["c64-artstudio", "build-art"]);
    assert_eq!(ids("e1m1.map"), ["quake-map"]);
    assert_eq!(ids("rock_wall_disp_1k.png"), ["heightmap"]);
    assert_eq!(ids("Rock.PNG"), ["png-16bit", "png"]);
    assert!(glob_matches("*_disp_*.png", "a_disp_2k.png") && !glob_matches("*_disp_*.png", "a_disp.png"));
    assert!(ids("readme").is_empty() && ids(".glb").is_empty());
    assert!(viewer_extensions().contains(&"nii.gz") && viewer_extensions().contains(&"map"));
    assert!(viewer_steps_to("Brain.nii.GZ") && viewer_steps_to("e1m1.map") && viewer_steps_to("a.b.glb"));
    assert!(!viewer_steps_to("backup.tar.gz") && !viewer_steps_to("doc.dd") && !viewer_steps_to("glb"));
    assert_eq!(Category::from_id("retro"), Some(Category::Retro));
}

//endregion

/*┌─────────────────╮
| Generated files |
└─────────────────╯*/
//region Generated files

#[test]
fn checked_in_files_match_the_table() {
    let mut stale = Vec::new();
    for file in generate::all() {
        let path = repo().join(file.path);
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        if have != file.text {
            stale.push(file.path);
        }
    }
    assert!(stale.is_empty(), "out of date, run `slerp formats --write`: {stale:?}");
}

#[test]
fn generated_lists_cover_the_table() {
    let thumbnailer = generate::system_thumbnailer();
    for f in FORMATS {
        for m in f.mime.iter().filter(|m| m.origin != Origin::Alias) {
            let listed =
                thumbnailer.contains(&format!("={};", m.name)) || thumbnailer.contains(&format!(";{};", m.name));
            assert_eq!(listed, f.thumbnails_by_default(), "{} in the thumbnailer", m.name);
        }
    }
    let qt = generate::qt_json();
    assert!(qt.contains("\"model/gltf-binary\"") && !qt.contains("image/x-dds"));
    // Only own (imported) types reach Quick Look.
    let ql = generate::thumbnails_plist();
    assert!(ql.contains("com.autodesk.fbx") && !ql.contains("org.khronos.glb"));
}

//endregion

/*┌─────────────────╮
| MIME database   |
└─────────────────╯*/
//region MIME database

/// One `<mime-type>` of a shared-mime-info package.
#[derive(Default)]
struct XmlType {
    comment: bool,
    globs: BTreeSet<String>,
    aliases: Vec<String>,
}

fn read_package(path: &std::path::Path) -> HashMap<String, XmlType> {
    let text = std::fs::read_to_string(path).unwrap();
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(&text, options).unwrap();
    let mut out: HashMap<String, XmlType> = HashMap::new();
    for t in doc.root_element().children().filter(|n| n.has_tag_name("mime-type")) {
        let entry = out.entry(t.attribute("type").unwrap().to_owned()).or_default();
        for c in t.children().filter(|c| c.is_element()) {
            match c.tag_name().name() {
                "comment" => entry.comment = true,
                "glob" => {
                    entry.globs.insert(c.attribute("pattern").unwrap().to_ascii_lowercase());
                }
                "alias" => entry.aliases.push(c.attribute("type").unwrap().to_owned()),
                _ => {}
            }
        }
    }
    out
}

fn globs_of(m: &Mime) -> BTreeSet<String> {
    m.exts.iter().map(|e| format!("*.{e}")).chain(m.patterns.iter().map(|p| p.to_string())).collect()
}

/// `data/slerp.xml` stays hand-written; this keeps it and the table
/// naming the same types, globs and aliases.
#[test]
fn slerp_xml_and_the_table_agree() {
    let xml = read_package(&repo().join("data/slerp.xml"));
    let mut named = HashSet::new();
    for f in FORMATS {
        for m in f.mime {
            named.insert(m.name);
            let entry = xml.get(m.name);
            match m.origin {
                Origin::Own => {
                    let e = entry.unwrap_or_else(|| panic!("{} ({}) isn't in slerp.xml", m.name, f.id));
                    assert!(e.comment, "{}: an own type needs a <comment>", m.name);
                    assert_eq!(e.globs, globs_of(m), "{}: slerp.xml's globs differ from the table's", m.name);
                }
                Origin::Extends => {
                    let e = entry.unwrap_or_else(|| panic!("{} ({}) isn't in slerp.xml", m.name, f.id));
                    let table = globs_of(m);
                    assert!(e.globs.is_subset(&table), "{}: slerp.xml adds globs the table lacks", m.name);
                }
                Origin::Alias => {
                    let main = f.mime[0].name;
                    assert!(
                        xml.get(main).is_some_and(|e| e.aliases.iter().any(|a| a == m.name)),
                        "{}: not an alias of {main} in slerp.xml",
                        m.name
                    );
                }
                Origin::System | Origin::Foreign => {
                    assert!(entry.is_none(), "{} is marked as the system's but slerp.xml defines it", m.name);
                }
            }
        }
    }
    for t in xml.keys() {
        assert!(named.contains(t.as_str()), "slerp.xml defines {t}, which no format in the table names");
    }
}

/// The system types the table names exist in shared-mime-info, with the
/// globs the table gives them.  Skipped where shared-mime-info isn't
/// installed (Windows, macOS).
#[test]
fn system_types_exist() {
    let path = std::path::Path::new("/usr/share/mime/packages/freedesktop.org.xml");
    if !path.exists() {
        return;
    }
    let system = read_package(path);
    let aliases: HashSet<&str> = system.values().flat_map(|t| t.aliases.iter().map(String::as_str)).collect();
    for f in FORMATS {
        for m in f.mime.iter().filter(|m| matches!(m.origin, Origin::System | Origin::Extends)) {
            if aliases.contains(m.name) {
                continue;
            }
            // KDE's own package defines a few (image/x-hdr).
            let Some(t) = system.get(m.name) else {
                let others = std::fs::read_dir("/usr/share/mime/packages").unwrap();
                let found = others.filter_map(|e| e.ok()).any(|e| {
                    std::fs::read_to_string(e.path()).is_ok_and(|s| s.contains(&format!("type=\"{}\"", m.name)))
                });
                assert!(found, "{} ({}) is in no installed MIME package", m.name, f.id);
                continue;
            };
            if m.origin == Origin::System {
                for g in globs_of(m) {
                    assert!(t.globs.contains(&g), "{}: shared-mime-info has no {g}", m.name);
                }
            }
        }
    }
}

//endregion
