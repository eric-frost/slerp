//! G-code and GPS tracks through core's entry points: each sample is
//! sniffed as the right kind past every other loader (FIT beside FITS
//! images, `.nc` beside NetCDF, XML and JSON beside the model formats),
//! and its thumbnail comes out the way it should: the slicer's own
//! picture, a drawn track, or a toolpath to render.  Uses
//! `local/assets/paths-extra/`; samples that aren't there are skipped with
//! a note.

use slerp_core::paths::PathFormat;
use slerp_core::{Kind, Prepared};
use std::path::{Path, PathBuf};

fn sample(rel: &str) -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/paths-extra").join(rel);
    if p.exists() {
        Some(p)
    } else {
        eprintln!("skipped, not downloaded: {rel}");
        None
    }
}

fn kind_of(path: &Path) -> Option<Kind> {
    slerp_core::sniff_file(path).unwrap()
}

/// What a thumbnail of `rel` is: the finished picture's size, or `None`
/// when the file needs the GPU.
fn thumbnail(rel: &str, format: PathFormat) -> Option<Option<(u32, u32)>> {
    let path = sample(rel)?;
    assert_eq!(kind_of(&path), Some(Kind::Path(format)), "{rel}");
    Some(match slerp_core::prepare_thumbnail(&path, 256).unwrap() {
        Prepared::Done(image) => {
            assert!(image.width <= 256 && image.height <= 256);
            assert!(image.pixels.chunks_exact(4).any(|p| p[3] > 0), "{rel} is blank");
            Some((image.width, image.height))
        }
        Prepared::Render(scene) => {
            assert!(scene.triangle_count() > 0, "{rel} has nothing to draw");
            None
        }
    })
}

#[test]
fn gcode_with_pictures_shows_them() {
    // PrusaSlicer's PNG and QOI pictures, in text and in binary G-code.
    for rel in [
        "gcode-preview/3DBenchy.gcode",
        "gcode-preview/calicat.gcode",
        "gcode-preview/screw.gcode",
        "libbgcode/mini_cube_b_ref.gcode",
    ] {
        let size = thumbnail(rel, PathFormat::Gcode);
        assert!(size.is_none_or(|s| s.is_some()), "{rel} should be a finished picture");
    }
    for rel in ["libbgcode/mini_cube_b.bgcode", "libbgcode/mini_cube_ps2.8.1.bgcode"] {
        assert!(thumbnail(rel, PathFormat::Bgcode).is_none_or(|s| s.is_some()), "{rel}");
    }
}

#[test]
fn gcode_without_pictures_is_rendered_or_drawn() {
    // A vase sliced with an empty thumbnail list: the toolpath in 3D.
    if let Some(done) = thumbnail("gcode-preview/vase.gcode", PathFormat::Gcode) {
        assert!(done.is_none(), "a toolpath is rendered, not drawn");
    }
    // The same print with its pictures cut out.
    if let Some(path) = sample("gcode-preview/screw.gcode") {
        let dir = std::env::temp_dir().join(format!("slerp-core-paths-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = String::from_utf8_lossy(&std::fs::read(path).unwrap()).into_owned();
        let mut keep = true;
        let stripped: String = text
            .lines()
            .filter(|l| {
                if l.starts_with("; thumbnail begin") {
                    keep = false;
                }
                let k = keep;
                if l.starts_with("; thumbnail end") {
                    keep = true;
                }
                k
            })
            .map(|l| format!("{l}\n"))
            .collect();
        let copy = dir.join("screw.gcode");
        std::fs::write(&copy, stripped).unwrap();
        assert_eq!(kind_of(&copy), Some(Kind::Path(PathFormat::Gcode)));
        assert!(matches!(slerp_core::prepare_thumbnail(&copy, 256).unwrap(), Prepared::Render(_)));
        assert!(Kind::Path(PathFormat::Gcode).usually_3d());
        std::fs::remove_dir_all(&dir).unwrap();
    }
    // CNC programs: a mill's plunging cuts are rendered, a flat plotter
    // path is drawn.
    thumbnail("gcode-preview/easel.gcode", PathFormat::Gcode);
    if let Some(done) = thumbnail("gcode-preview/mach3.gcode", PathFormat::Gcode) {
        assert!(done.is_some(), "a flat job is drawn from above");
    }
}

#[test]
fn tracks_are_drawn_flat() {
    for (rel, format) in [
        ("fitparse/garmin-edge-500-activity.fit", PathFormat::Fit),
        ("fitparse/strava-android-app-201.10-b1218918.fit", PathFormat::Fit),
        ("togeojson/run.gpx", PathFormat::Gpx),
        ("togeojson/blue_hills.gpx", PathFormat::Gpx),
        ("tcxparser/test2.tcx", PathFormat::Tcx),
        ("togeojson/gxmultitrack.kml", PathFormat::Kml),
        ("togeojson/polygon.kml", PathFormat::Kml),
    ] {
        if let Some(size) = thumbnail(rel, format) {
            assert!(size.is_some(), "{rel} is drawn on the CPU");
        }
        assert!(!Kind::Path(format).usually_3d());
    }
}

/// Files that look a little like tracks or G-code but aren't, and
/// astronomy images that share FIT's extension, belong to other formats.
#[test]
fn lookalikes_are_left_alone() {
    let dir = std::env::temp_dir().join(format!("slerp-core-paths-lookalikes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, bytes: &[u8]| {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    };
    let kind = |p: &Path| slerp_core::sniff_file(p).unwrap();
    // A FITS image, NetCDF data, ordinary JSON and XML, and text: whatever
    // else takes them (FITS is a texture format now), these loaders don't.
    let not_paths = |p: PathBuf| {
        let k = kind(&p);
        assert!(!matches!(k, Some(Kind::Path(_))), "{} was taken for {k:?}", p.display());
    };
    not_paths(write("sky.fit", b"SIMPLE  =                    T / conforms to FITS"));
    not_paths(write("data.nc", b"CDF\x01\x00\x00\x00\x00\x00\x00\x00\x00"));
    not_paths(write("package.json", br#"{"name": "slerp", "version": "1.0"}"#));
    not_paths(write("page.xml", b"<?xml version=\"1.0\"?><html><body/></html>"));
    not_paths(write("notes.g", b"These are my notes\nfrom the meeting.\n"));
    not_paths(write("notes.gcode.txt", b"G1 X1\nG1 X2\n"));
    // Real ones, written by hand: each is claimed whatever the file's
    // spelling.
    let gpx = br#"<?xml version="1.0"?><gpx version="1.1"><trk><trkseg><trkpt lat="1" lon="2"/><trkpt lat="1.001" lon="2"/></trkseg></trk></gpx>"#;
    assert_eq!(kind(&write("a.gpx", gpx)), Some(Kind::Path(PathFormat::Gpx)));
    assert_eq!(kind(&write("a.xml", gpx)), Some(Kind::Path(PathFormat::Gpx)));
    let geo = br#"{"type":"LineString","coordinates":[[2,1],[2,1.001]]}"#;
    assert_eq!(kind(&write("a.geojson", geo)), Some(Kind::Path(PathFormat::GeoJson)));
    assert_eq!(kind(&write("a.gcode", b"G21\nG90\nG1 X1 Y1 E1\n")), Some(Kind::Path(PathFormat::Gcode)));
    assert_eq!(kind(&write("a.nc", b"G21\nG90\nG0 X1 Y1\nG1 Z-1 F100\n")), Some(Kind::Path(PathFormat::Gcode)));
    // Damaged files give an error or a blank, never a panic.
    for (name, bytes) in [
        ("b.gpx", &gpx[..60]),
        ("b.geojson", &geo[..30]),
        ("b.gcode", b"G1 X\xff\xfe Y1e999 E"),
        ("b.fit", b"\x0e\x20\x00\x00\xff\xff\xff\xff.FIT\x00\x00"),
    ] {
        let p = write(name, bytes);
        let _ = slerp_core::prepare_thumbnail(&p, 64);
        let _ = slerp_core::load(&p, &slerp_core::scene::Limits::viewer());
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
