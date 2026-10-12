//! The files other programs read without running Slerp, made from
//! [`FORMATS`](crate::FORMATS).  `slerp formats --write` writes them into the
//! source tree, and this crate's tests fail when a checked-in copy differs
//! from what [`all`] makes, so the files are always current and packaging
//! never runs the generator.

use crate::{Category, FORMATS, Format, Mac, Qt, Uti};
use std::fmt::Write;

/// A generated file: its path from the repository's root, and its text.
pub struct File {
    pub path: &'static str,
    pub text: String,
}

/// Every generated file.
pub fn all() -> Vec<File> {
    vec![
        File { path: "data/slerp.thumbnailer", text: system_thumbnailer() },
        File { path: "data/slerp.desktop", text: desktop() },
        File { path: "data/macos/Info.plist", text: info_plist() },
        File { path: "data/macos/Thumbnails-Info.plist", text: thumbnails_plist() },
        File { path: "qt/slerp.json", text: qt_json() },
        File { path: "qt/formats_table.inc", text: qt_table() },
    ]
}

/// Writes every generated file under `root` (the repository) whose text
/// changed, and returns their paths.
pub fn write_all(root: &std::path::Path) -> std::io::Result<Vec<&'static str>> {
    let mut written = Vec::new();
    for file in all() {
        let path = root.join(file.path);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(file.text.as_str()) {
            std::fs::write(&path, &file.text)?;
            written.push(file.path);
        }
    }
    Ok(written)
}

/// The note at the top of each generated file.
const GENERATED: &str =
    "Generated from crates/formats/src/table.rs by `slerp formats --write`; edit the table, not this file.";

/*┌─────────────────╮
| Freedesktop     |
└─────────────────╯*/
//region Freedesktop

/// The system-wide thumbnailer: the formats whose thumbnails are on by
/// default.
pub fn system_thumbnailer() -> String {
    thumbnailer(&crate::default_thumbnail_types(), "slerp", &format!("# {GENERATED}\n"))
}

/// A freedesktop `.thumbnailer` file running `program` for `types`, with
/// `header` (comment lines) before it.  File managers run
/// `<program> thumbnail <size> <input> <output>`.
pub fn thumbnailer(types: &[&str], program: &str, header: &str) -> String {
    let mut out = String::from(header);
    out.push_str("[Thumbnailer Entry]\n");
    let _ = writeln!(out, "TryExec={program}");
    let _ = writeln!(out, "Exec={program} thumbnail %s %i %o");
    out.push_str("MimeType=");
    for t in types {
        out.push_str(t);
        out.push(';');
    }
    out.push('\n');
    out
}

/// Words that help find the viewer in application launchers.
const KEYWORDS: &str = "glTF;GLB;FBX;OBJ;KTX2;DDS;HDR;Quake;BSP;MaterialX;USD;3D;model;texture;MetaHuman;DNA;Gaussian splat;3DGS;SPZ;Aseprite;sprite;Tiled;tilemap;LUT;cube;3MF;3DS;DirectX;VRML;X3D;OFF;point cloud;lidar;LAS;LAZ;E57;PCD;Doom;GZDoom;PK3;Godot;Unity;scene;prefab;Blender;blend;MAP;TrenchBroom;Hammer;VMF;level;Procreate;Affinity;Sketch;Figma;Clip Studio;Paint.NET;InDesign;Adobe XD;FreeCAD;palette;swatches;colour;color;ASE;ACO;GIMP palette;G-code;gcode;bgcode;3D printing;toolpath;CNC;GPS;GPX;TCX;FIT;KML;KMZ;GeoJSON;track;route;DICOM;medical;CT;MRI;X-ray;ultrasound;DXF;AutoCAD;CAD;Rhino;3dm;Atari ST;DEGAS;NEOchrome;Commodore 64;C64;Koala;Art Studio;Doodle;ZX Spectrum;Amstrad CPC;MSX;Apple II;Atari 8-bit;BBC Micro;ANSI art;SAUCE;BBS;PICO-8;TIC-80;Amiga;IFF;ILBM;XNA;MonoGame;XNB;Blizzard;BLP;PowerVR;PVR;PKM;ETC;Build engine;Duke Nukem 3D;ART tiles;sprite;atlas;libGDX;TexturePacker;WMF;EMF;EMZ;WMZ;metafile;PICT;CorelDRAW;CDR;Visio;VSDX;FITS;XISF;astronomy;DPX;Cineon;film scan;JPEG XR;JXR;HDR screenshot;PAM;farbfeld;NumPy;NIfTI;NRRD;MetaImage;MRI;CT;volume;OME-TIFF;microscopy;slide;SVS;NDPI;HDF5;NetCDF;MATLAB;array;GIF;APNG;WebP;animated image;ICO;CUR;icon;cursor;PBM;PGM;PPM;PNM;Netpbm;TIFF;JPEG 2000;JP2;JPEG XL;JXL;AVIF;HEIC;HEIF;photo;";

/// The viewer's desktop entry, which puts it in "Open with" for every
/// format that asks for it.
pub fn desktop() -> String {
    let mut out = format!("# {GENERATED}\n");
    out.push_str(
        "[Desktop Entry]
Type=Application
Name=Slerp
GenericName=Game Asset Viewer
Comment=View 3D models, levels, textures and materials
Exec=slerp view %f
Icon=slerp
StartupWMClass=slerp
Terminal=false
# Below every other application that opens the same types, so the ordinary
# image formats in the list (GIF, TIFF, AVIF, ...) put Slerp in \"Open with\"
# and never make it the default.  KDE reads this; the default is 1.
InitialPreference=0
Categories=Graphics;3DGraphics;Viewer;
",
    );
    let _ = writeln!(out, "Keywords={KEYWORDS}");
    out.push_str("MimeType=");
    for t in crate::open_with_types() {
        out.push_str(t);
        out.push(';');
    }
    out.push('\n');
    out
}

//endregion

/*┌─────────────────╮
| macOS           |
└─────────────────╯*/
//region macOS

/// The types a UTI of `format` conforms to when the table names none.
fn conforms(format: &Format, uti: &Uti) -> &'static [&'static str] {
    if !uti.conforms.is_empty() {
        return uti.conforms;
    }
    match format.category {
        Category::Model | Category::PointCloud | Category::Scene | Category::Level => {
            &["public.3d-content", "public.data"]
        }
        Category::Texture
        | Category::Image
        | Category::Retro
        | Category::Sprite
        | Category::Drawing
        | Category::Medical => &["public.image", "public.data"],
        _ => &["public.data", "public.content"],
    }
}

/// Imported declarations, each once (Quake's and Doom's WADs share one).
fn imports() -> Vec<(&'static Format, &'static Uti)> {
    let mut out: Vec<(&'static Format, &'static Uti)> = Vec::new();
    for f in FORMATS {
        for u in f.imported_utis() {
            if !out.iter().any(|(_, seen)| seen.id == u.id) {
                out.push((f, u));
            }
        }
    }
    out
}

/// The extensions a declaration claims, and its MIME type.
fn uti_tags(format: &'static Format, uti: &'static Uti) -> (Vec<&'static str>, Option<&'static str>) {
    let exts = if uti.exts.is_empty() { format.simple_extensions() } else { uti.exts.to_vec() };
    // The MIME type whose extensions these are, or the format's first.
    let mime = format
        .mime
        .iter()
        .find(|m| m.exts.iter().any(|e| exts.contains(e)))
        .map(|m| m.name)
        .unwrap_or(format.main_mime());
    (exts, Some(mime))
}

/// The app's Info.plist (`@VERSION@` is filled in by `tools/macos-app.sh`).
pub fn info_plist() -> String {
    let mut own: Vec<&str> = Vec::new();
    let mut system: Vec<&str> = Vec::new();
    for f in FORMATS.iter().filter(|f| f.offered_on(crate::MACOS)) {
        match f.mac {
            Mac::Import(list) => {
                for u in list {
                    if !own.contains(&u.id) {
                        own.push(u.id);
                    }
                }
            }
            Mac::System(ids) => {
                for id in ids {
                    if !system.contains(id) {
                        system.push(id);
                    }
                }
            }
            Mac::None => {}
        }
    }
    let mut out = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleDisplayName</key>
	<string>Slerp</string>
	<key>CFBundleExecutable</key>
	<string>Slerp</string>
	<key>CFBundleIconFile</key>
	<string>Slerp</string>
	<key>CFBundleIdentifier</key>
	<string>com.slerp3d.Slerp</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>Slerp</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>@VERSION@</string>
	<key>CFBundleVersion</key>
	<string>@VERSION@</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.graphics-design</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSSupportsAutomaticGraphicsSwitching</key>
	<true/>
	<!--
"#,
    );
    let _ = writeln!(out, "\t  {GENERATED}");
    out.push_str(
        r#"
	  File types.  macOS knows a file's type by its extension alone (there
	  is no magic-number matching), and already declares glTF/GLB, OBJ, STL,
	  PLY, COLLADA, USD, KTX, ASTC, DDS, PVR, Radiance HDR, DICOM, NIfTI,
	  PICT, GeoJSON and the ordinary picture formats.  Those keep the
	  system's identifiers and get Slerp in Open With (rank Alternate;
	  LaunchServices still makes Slerp the default where other apps claim
	  only a parent type, as for OBJ and STL), but Quick Look reserves their
	  thumbnails for Apple's own thumbnailers, so they aren't in the
	  extension's list.  The formats macOS doesn't know are imported, not
	  exported, since Slerp owns none of them: an app that defines one
	  (Godot exports public.res for .res and .tres) takes precedence, and
	  the identifiers follow the formats' owners so such apps and Slerp
	  agree.  Slerp is their default viewer (rank Default).  The MIME types
	  match data/slerp.xml.
	-->
	<key>CFBundleDocumentTypes</key>
	<array>
		<dict>
			<key>CFBundleTypeName</key>
			<string>Game or 3D asset</string>
			<key>CFBundleTypeRole</key>
			<string>Viewer</string>
			<key>LSHandlerRank</key>
			<string>Default</string>
			<key>LSItemContentTypes</key>
			<array>
"#,
    );
    for id in &own {
        let _ = writeln!(out, "\t\t\t\t<string>{id}</string>");
    }
    out.push_str(
        r#"			</array>
		</dict>
		<dict>
			<key>CFBundleTypeName</key>
			<string>Model, texture or image</string>
			<key>CFBundleTypeRole</key>
			<string>Viewer</string>
			<key>LSHandlerRank</key>
			<string>Alternate</string>
			<key>LSItemContentTypes</key>
			<array>
"#,
    );
    for id in &system {
        let _ = writeln!(out, "\t\t\t\t<string>{id}</string>");
    }
    out.push_str(
        r#"			</array>
		</dict>
	</array>
	<key>UTImportedTypeDeclarations</key>
	<array>
"#,
    );
    for (f, u) in imports() {
        let (exts, mime) = uti_tags(f, u);
        let description = if u.description.is_empty() { f.name } else { u.description };
        out.push_str("\t\t<dict>\n");
        let _ = writeln!(out, "\t\t\t<key>UTTypeIdentifier</key>\n\t\t\t<string>{}</string>", u.id);
        let _ = writeln!(out, "\t\t\t<key>UTTypeDescription</key>\n\t\t\t<string>{}</string>", xml_escape(description));
        out.push_str("\t\t\t<key>UTTypeConformsTo</key>\n\t\t\t<array>\n");
        for c in conforms(f, u) {
            let _ = writeln!(out, "\t\t\t\t<string>{c}</string>");
        }
        out.push_str("\t\t\t</array>\n\t\t\t<key>UTTypeTagSpecification</key>\n\t\t\t<dict>\n");
        out.push_str("\t\t\t\t<key>public.filename-extension</key>\n\t\t\t\t<array>\n");
        for e in exts {
            let _ = writeln!(out, "\t\t\t\t\t<string>{e}</string>");
        }
        out.push_str("\t\t\t\t</array>\n");
        if let Some(mime) = mime {
            let _ = writeln!(out, "\t\t\t\t<key>public.mime-type</key>\n\t\t\t\t<string>{mime}</string>");
        }
        out.push_str("\t\t\t</dict>\n\t\t</dict>\n");
    }
    out.push_str("\t</array>\n</dict>\n</plist>\n");
    out
}

/// The Quick Look extension's Info.plist: every type Slerp imports and can
/// thumbnail.  The extension declines the ones the user switched off, so the
/// list doesn't depend on the user's choices.
pub fn thumbnails_plist() -> String {
    let mut out = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!--
"#,
    );
    let _ = writeln!(out, "  {GENERATED}");
    out.push_str(
        r#"
  Slerp's Quick Look thumbnail extension, bundled as
  Slerp.app/Contents/PlugIns/SlerpThumbnails.appex by
  tools/macos-app.sh.  QLSupportedContentTypes lists only the types the
  app's Info.plist imports: Quick Look ignores an extension's claim on a
  type macOS itself declares.  The extension reads the user's choices
  (file-types.jsonc in ~/Library/Application Support/Slerp) and declines
  the types switched off there.
-->
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleDisplayName</key>
	<string>Slerp Thumbnails</string>
	<key>CFBundleExecutable</key>
	<string>SlerpThumbnails</string>
	<key>CFBundleIdentifier</key>
	<string>com.slerp3d.Slerp.Thumbnails</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>SlerpThumbnails</string>
	<key>CFBundlePackageType</key>
	<string>XPC!</string>
	<key>CFBundleShortVersionString</key>
	<string>@VERSION@</string>
	<key>CFBundleSupportedPlatforms</key>
	<array>
		<string>MacOSX</string>
	</array>
	<key>CFBundleVersion</key>
	<string>@VERSION@</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>NSExtension</key>
	<dict>
		<key>NSExtensionAttributes</key>
		<dict>
			<key>QLSupportedContentTypes</key>
			<array>
"#,
    );
    let mut seen: Vec<&str> = Vec::new();
    for f in FORMATS.iter().filter(|f| f.can_thumbnail()) {
        for u in f.imported_utis() {
            if !seen.contains(&u.id) {
                seen.push(u.id);
                let _ = writeln!(out, "\t\t\t\t<string>{}</string>", u.id);
            }
        }
    }
    out.push_str(
        r#"			</array>
		</dict>
		<key>NSExtensionPointIdentifier</key>
		<string>com.apple.quicklook.thumbnail</string>
		<key>NSExtensionPrincipalClass</key>
		<string>SlerpThumbnailProvider</string>
	</dict>
</dict>
</plist>
"#,
    );
    out
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

//endregion

/*┌─────────────────╮
| Qt              |
└─────────────────╯*/
//region Qt

/// The Qt plugin's format keys with their MIME types and shapes, in the
/// order Qt's metadata lists them.
pub fn qt_entries() -> Vec<(&'static str, &'static str, Qt, &'static str)> {
    FORMATS.iter().flat_map(|f| f.qt_pairs().into_iter().map(move |(k, m)| (k, m, f.qt, f.id))).collect()
}

/// `qt/slerp.json`, the plugin's metadata, which Qt reads without loading
/// the plugin and pairs by position.
pub fn qt_json() -> String {
    let entries = qt_entries();
    let mut out = String::from("{\n \"Keys\": [\n");
    let n = entries.len();
    for (i, (k, _, _, _)) in entries.iter().enumerate() {
        let _ = writeln!(out, "  \"{k}\"{}", if i + 1 < n { "," } else { "" });
    }
    out.push_str(" ],\n \"MimeTypes\": [\n");
    for (i, (_, m, _, _)) in entries.iter().enumerate() {
        let _ = writeln!(out, "  \"{m}\"{}", if i + 1 < n { "," } else { "" });
    }
    out.push_str(" ]\n}\n");
    out
}

/// `qt/formats_table.inc`, the same list for `formats.cpp`.
pub fn qt_table() -> String {
    let mut out = format!(
        "// {GENERATED}\n// The rows of kFormats in formats.cpp, in the order of slerp.json: key, MIME type, shape, format id.\n"
    );
    for (k, m, shape, id) in qt_entries() {
        let shape = if shape == Qt::Turntable { "Turntable" } else { "Still" };
        let _ = writeln!(out, "{{\"{k}\", \"{m}\", Shape::{shape}, \"{id}\"}},");
    }
    out
}

//endregion
