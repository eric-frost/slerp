//! Paths through space, outside game development: the toolpaths of 3D
//! printers, CNC mills and laser cutters (G-code), and GPS tracks.  Both
//! are polylines with something measured along them.
//!
//! # G-code
//!
//! Text G-code (`.gcode`, `.gco`, `.g`, `.nc`, `.ngc`, `.tap`, `.cnc`) and
//! Prusa's binary G-code (`.bgcode`).  A thumbnail is the largest preview
//! picture the slicer embedded (see `gcode::thumbs` for the slicers and
//! layouts read), when it is large enough; otherwise the toolpath is
//! drawn: extrusions as beads with their real width, coloured by feature
//! as slicers show them, from the usual three-quarter view.  CNC programs
//! (no extrusion) draw their cuts; flat ones (lasers) from above.  Files
//! of hundreds of megabytes stream through the interpreter without being
//! held, and thumbnails merge and thin what they draw to stay quick.
//!
//! # GPS tracks
//!
//! GPX, TCX, FIT, KML, KMZ and GeoJSON.  The thumbnail is the route from
//! above, coloured by elevation (or speed), start and end marked, shaped
//! like the track.  In 3D the route rides over its own (exaggerated)
//! elevation with a curtain down to its lowest point.  Statistics
//! (distance, climb, moving time, speeds, heart rate, cadence and power)
//! go to the inspector.  Nothing is fetched from the network.

mod draw;
pub mod gcode;
mod text;
pub mod track;

pub use gcode::{Feature, Layer, Meta, Move, MoveKind, Target, Toolpath, ToolpathLook, toolpath_scene};
pub use text::{format_duration, parse_duration};
pub use track::{
    ColorBy, LineKind, Profile, Projection, Stats, TrackLook, TrackPoint, TrackSet, metric_color, robust_range,
};

use gcode::{Keep, Parser};
use slerp_scene::textures::Texture;
use slerp_scene::{Image8, Limits, LoadError, Scene};
use std::io::Read;
use std::path::Path;

/// Points a viewer's toolpath keeps at most (20 bytes each).
const VIEWER_POINTS: usize = 30_000_000;
/// Points a thumbnail's toolpath keeps at most.
const THUMBNAIL_POINTS: usize = 10_000_000;
/// Track points kept, beyond which tracks are thinned evenly.
const VIEWER_TRACK_POINTS: usize = 5_000_000;
const THUMBNAIL_TRACK_POINTS: usize = 1_000_000;
/// The longest G-code line read; longer ones (a slicer's settings as
/// JSON) are cut, harmlessly.
const MAX_LINE: usize = 64 << 10;
/// Files read whole (FIT, KMZ, GeoJSON) larger than this are refused.
const MAX_WHOLE_FILE: u64 = 512 << 20;
/// GeoJSON is parsed into a tree of its coordinates, which takes several
/// times the file's size, and real route files are small.
const MAX_GEOJSON: u64 = 128 << 20;

/// The formats this crate reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PathFormat {
    Gcode,
    Bgcode,
    Gpx,
    Tcx,
    Fit,
    Kml,
    Kmz,
    GeoJson,
}

impl PathFormat {
    pub fn name(self) -> &'static str {
        match self {
            PathFormat::Gcode => "G-code",
            PathFormat::Bgcode => "Binary G-code",
            PathFormat::Gpx => "GPX",
            PathFormat::Tcx => "TCX",
            PathFormat::Fit => "FIT",
            PathFormat::Kml => "KML",
            PathFormat::Kmz => "KMZ",
            PathFormat::GeoJson => "GeoJSON",
        }
    }

    /// G-code rather than a GPS track.
    pub fn is_toolpath(self) -> bool {
        matches!(self, PathFormat::Gcode | PathFormat::Bgcode)
    }
}

/*┌─────────────────╮
| Sniffing        |
└─────────────────╯*/
//region Sniffing

/// Extensions of text G-code.  `.gcode` and `.gco` are taken on looks
/// alone; the others (shared with other formats) need G-code commands in
/// the first lines.
const GCODE_EXTENSIONS: &[&str] = &["gcode", "gco", "g", "gc", "nc", "ngc", "tap", "cnc", "ncc", "mpt", "mpf"];

/// Recognises a file by its first bytes and its name.
pub fn sniff(path: &Path, head: &[u8]) -> Option<PathFormat> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if head.starts_with(gcode::BGCODE_MAGIC) {
        return Some(PathFormat::Bgcode);
    }
    if track::is_fit(head) {
        return Some(PathFormat::Fit);
    }
    if head.starts_with(b"PK\x03\x04") {
        // A truncated header has no name length: not a KMZ by content.
        let first_kml = head
            .get(26..28)
            .and_then(|len| head.get(30..30 + u16::from_le_bytes([len[0], len[1]]) as usize))
            .is_some_and(|n| String::from_utf8_lossy(n).to_ascii_lowercase().ends_with(".kml"));
        return (ext == "kmz" || first_kml).then_some(PathFormat::Kmz);
    }
    let text = head.strip_prefix(b"\xef\xbb\xbf").unwrap_or(head);
    let trimmed = text.trim_ascii_start();
    if trimmed.starts_with(b"<") {
        return match xml_root(trimmed)?.as_slice() {
            b"gpx" => Some(PathFormat::Gpx),
            b"TrainingCenterDatabase" => Some(PathFormat::Tcx),
            b"kml" => Some(PathFormat::Kml),
            _ => None,
        };
    }
    if trimmed.starts_with(b"{") || trimmed.starts_with(b"[") {
        let geo_name = ext == "geojson" || name.ends_with(".geo.json");
        let has = |s: &[u8]| memchr::memmem::find(trimmed, s).is_some();
        let looks_geo = has(b"\"type\"")
            && (has(b"FeatureCollection")
                || has(b"\"Feature\"")
                || has(b"LineString")
                || has(b"\"Point\"")
                || has(b"Polygon")
                || has(b"GeometryCollection"));
        return (geo_name || (ext == "json" && looks_geo)).then_some(PathFormat::GeoJson);
    }
    if GCODE_EXTENSIONS.contains(&ext.as_str()) {
        let lenient = ext == "gcode" || ext == "gco";
        return looks_like_gcode(text, lenient).then_some(PathFormat::Gcode);
    }
    None
}

/// The local name of an XML document's root element.
fn xml_root(text: &[u8]) -> Option<Vec<u8>> {
    let mut rest = text;
    loop {
        let i = memchr::memchr(b'<', rest)?;
        rest = &rest[i + 1..];
        match rest.first()? {
            b'?' | b'!' => {
                // A declaration, comment or doctype: skip past its end.
                let end = if rest.starts_with(b"!--") {
                    memchr::memmem::find(rest, b"-->")?
                } else {
                    memchr::memchr(b'>', rest)?
                };
                rest = &rest[end..];
            }
            c if c.is_ascii_alphabetic() || *c == b'_' => {
                let end = rest.iter().position(|&b| b.is_ascii_whitespace() || b == b'>' || b == b'/')?;
                let qname = &rest[..end];
                let local = qname.rsplit(|&b| b == b':').next().unwrap_or(qname);
                return Some(local.to_vec());
            }
            _ => return None,
        }
    }
}

/// Whether text looks like G-code: comment lines and command lines
/// (a G, M, T or N word, or an axis word as CNC programs write them),
/// with nothing binary.  `lenient` accepts files that are all comments so
/// far (a slicer's thumbnail at the top); otherwise commands must appear.
fn looks_like_gcode(text: &[u8], lenient: bool) -> bool {
    if text.contains(&0) {
        return false;
    }
    let (mut commands, mut comments, mut other) = (0, 0, 0);
    let mut lines: Vec<&[u8]> = text.split(|&b| b == b'\n').collect();
    // The last line may be cut by the end of the head.
    if lines.len() > 1 {
        lines.pop();
    }
    for line in lines {
        let l = line.trim_ascii();
        let Some(&c) = l.first() else { continue };
        let next_digit = l.get(1).is_some_and(|d| d.is_ascii_digit() || *d == b'-' || *d == b'.' || *d == b' ');
        match c.to_ascii_uppercase() {
            b';' | b'(' | b'%' => comments += 1,
            b'G' | b'M' | b'T' | b'N' | b'X' | b'Y' | b'Z' | b'F' | b'S' | b'O' if next_digit => commands += 1,
            _ => other += 1,
        }
    }
    if lenient {
        commands + comments > 0 && other <= (commands + comments) / 2
    } else {
        commands >= 2 && other < commands
    }
}

//endregion

/*┌─────────────────╮
| Reading         |
└─────────────────╯*/
//region Reading

/// Reads a G-code file's toolpath, keeping what `keep` says.
pub fn read_toolpath(format: PathFormat, path: &Path, keep: Keep, max_points: usize) -> Result<Toolpath, LoadError> {
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    // A thumbnail of a very large file reads a sample of it.
    let sample = format == PathFormat::Gcode
        && keep == Keep::Drawn
        && bytes > gcode::SAMPLE_FILE_BYTES
        && gcode::has_layer_markers(path)?;
    // Binary G-code holds about four times its size in text.
    let text_bytes = match format {
        _ if sample => 0,
        PathFormat::Bgcode => bytes.saturating_mul(4),
        _ => bytes,
    };
    let mut parser = Parser::with_capacity(keep, max_points, text_bytes);
    let mut lines = text::Lines::new(MAX_LINE);
    match format {
        PathFormat::Gcode if sample => {
            let layers = gcode::read_sampled(path, bytes, &mut parser)?;
            parser.warn(format!("a sample of the file: {layers} layers from its middle, with its start and end"));
        }
        PathFormat::Gcode => {
            let file = std::fs::File::open(path)?;
            text::for_chunks(file, 1 << 20, |chunk| {
                lines.push(chunk, &mut |l| {
                    parser.line(l);
                    true
                })
            })?;
        }
        PathFormat::Bgcode => {
            let mut feed = |text: &[u8]| {
                lines.push(text, &mut |l| {
                    parser.line(l);
                    true
                })
            };
            let sink = gcode::read_bgcode(path, Some(&mut feed))?;
            for m in &sink.metadata {
                parser.meta_line(m);
            }
            for w in sink.warnings {
                parser.warn(w);
            }
            let mut format = "Binary G-code".to_string();
            if !sink.compression.is_empty() {
                format = format!("{format} ({})", sink.compression.join(", "));
            }
            parser.set_format(format);
        }
        _ => return Err(LoadError::NotMine),
    }
    lines.finish(&mut |l| {
        parser.line(l);
        true
    });
    Ok(parser.finish())
}

/// A track file read for viewing: its contents, projection, profile and
/// statistics.
pub struct Track {
    pub set: TrackSet,
    pub projection: Projection,
    pub profile: Profile,
    pub stats: Stats,
}

impl Track {
    pub fn new(mut set: TrackSet, max_points: usize) -> Result<Track, LoadError> {
        set.tidy(max_points);
        if set.is_empty() {
            return Err(LoadError::invalid("no positions in the file"));
        }
        let projection = set.projection();
        let profile = Profile::of(&set, &projection);
        let stats = Stats::of(&set, &profile);
        Ok(Track { set, projection, profile, stats })
    }

    /// The 3D scene as `look` says, with the statistics as its details.
    pub fn scene(&self, look: &TrackLook, markers: bool) -> Scene {
        let mut scene = track::track_scene(&self.set, &self.profile, &self.projection, look, markers);
        scene.info.details = self.stats.details(&self.set);
        scene
    }
}

/// Reads a GPS track file.
pub fn read_track(format: PathFormat, path: &Path, max_points: usize) -> Result<Track, LoadError> {
    let whole = |limit: u64| -> Result<Vec<u8>, LoadError> {
        let len = std::fs::metadata(path)?.len();
        if len > limit {
            return Err(LoadError::TooLarge(format!("{len} bytes")));
        }
        let mut data = Vec::with_capacity(len as usize);
        std::fs::File::open(path)?.take(limit).read_to_end(&mut data)?;
        Ok(data)
    };
    let buffered = || -> Result<std::io::BufReader<std::fs::File>, LoadError> {
        Ok(std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(path)?))
    };
    let set = match format {
        PathFormat::Gpx => track::parse_gpx(buffered()?),
        PathFormat::Tcx => track::parse_tcx(buffered()?),
        PathFormat::Kml => track::parse_kml(buffered()?),
        PathFormat::Kmz => track::parse_kmz(&whole(MAX_WHOLE_FILE)?)?,
        PathFormat::GeoJson => track::parse_geojson(&whole(MAX_GEOJSON)?)?,
        PathFormat::Fit => track::parse_fit(&whole(MAX_WHOLE_FILE)?)?,
        _ => return Err(LoadError::NotMine),
    };
    Track::new(set, max_points)
}

//endregion

/*┌─────────────────╮
| Entry points    |
└─────────────────╯*/
//region Entry points

/// What a thumbnail needs.
pub enum Thumb {
    /// A finished drawing (tracks, flat CNC jobs).
    Drawn(Image8),
    /// A toolpath to render.
    Scene(Box<Scene>),
}

/// The picture a slicer embedded in a G-code file (PrusaSlicer, Cura,
/// OrcaSlicer, Bambu Studio; the largest when there are several), read
/// without the moves; `None` for files without one and for tracks.
/// Slerp's core shows it as the file's thumbnail in place of a drawing;
/// see `slerp_core::saved_picture`.
pub fn saved_picture(format: PathFormat, path: &Path) -> Result<Option<Texture>, LoadError> {
    if !format.is_toolpath() {
        return Ok(None);
    }
    let found = match format {
        PathFormat::Bgcode => gcode::read_bgcode(path, None)?.thumbnails,
        _ => gcode::embedded_thumbnails(path)?,
    };
    Ok(gcode::best(found))
}

/// Draws the thumbnail of a file `size` pixels square at most, from its
/// moves or track points.
pub fn thumbnail(format: PathFormat, path: &Path, size: u32) -> Result<Thumb, LoadError> {
    if !format.is_toolpath() {
        let t = read_track(format, path, THUMBNAIL_TRACK_POINTS)?;
        return track::track_thumbnail(&t.set, &t.profile, size)
            .map(Thumb::Drawn)
            .ok_or_else(|| LoadError::invalid("nothing to draw"));
    }
    let tp = read_toolpath(format, path, Keep::Drawn, THUMBNAIL_POINTS)?;
    if tp.is_empty() {
        return Err(LoadError::invalid("no moves to draw"));
    }
    if tp.cnc && gcode::is_flat(&tp) {
        return gcode::cut_drawing(&tp, size).map(Thumb::Drawn).ok_or_else(|| LoadError::invalid("nothing to draw"));
    }
    let look = ToolpathLook::outside(&tp);
    Ok(Thumb::Scene(Box::new(toolpath_scene(&tp, &look, Target::Thumbnail(size)))))
}

/// A file opened for the viewer: the scene to show, and what it came from,
/// which the viewer redraws as its controls change.
pub struct Opened {
    pub scene: Scene,
    pub data: PathData,
}

pub enum PathData {
    Toolpath(Box<Toolpath>),
    Track(Box<Track>),
}

/// Opens a file for the viewer: a toolpath with one node per layer and
/// travel hidden, or a track in 3D (its waypoints left to the viewer).
pub fn open(format: PathFormat, path: &Path, limits: &Limits) -> Result<Opened, LoadError> {
    let size = std::fs::metadata(path)?.len();
    if size > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{size} bytes")));
    }
    if format.is_toolpath() {
        let tp = read_toolpath(format, path, Keep::All, VIEWER_POINTS)?;
        if tp.points.is_empty() {
            return Err(LoadError::invalid("no moves in the file"));
        }
        let scene = toolpath_scene(&tp, &ToolpathLook::all(), Target::Viewer);
        return Ok(Opened { scene, data: PathData::Toolpath(Box::new(tp)) });
    }
    let t = read_track(format, path, VIEWER_TRACK_POINTS)?;
    let scene = t.scene(&TrackLook::for_profile(&t.profile), false);
    Ok(Opened { scene, data: PathData::Track(Box::new(t)) })
}

/// Loads a file as a scene, for rendering it whole (`slerp render`, `slerp
/// info`, the Qt plugin's turntables): every layer of a toolpath, a track
/// with its waypoints as markers.  Limits that ask for small textures are
/// a request for a small picture (thumbnails, the Qt plugin), which gets a
/// toolpath thinned and merged to what that picture can show, as
/// [`thumbnail`] makes it.
pub fn load(format: PathFormat, path: &Path, limits: &Limits) -> Result<Scene, LoadError> {
    let size = std::fs::metadata(path)?.len();
    if size > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{size} bytes")));
    }
    let small = limits.max_texture_size;
    if format.is_toolpath() {
        let points = if small.is_some() { THUMBNAIL_POINTS } else { VIEWER_POINTS };
        let tp = read_toolpath(format, path, Keep::Drawn, points)?;
        if tp.is_empty() {
            return Err(LoadError::invalid("no moves to draw"));
        }
        return Ok(match small {
            Some(px) => toolpath_scene(&tp, &ToolpathLook::outside(&tp), Target::Thumbnail(px)),
            None => toolpath_scene(&tp, &ToolpathLook::all(), Target::Viewer),
        });
    }
    let t = read_track(format, path, if small.is_some() { THUMBNAIL_TRACK_POINTS } else { VIEWER_TRACK_POINTS })?;
    Ok(t.scene(&TrackLook::for_profile(&t.profile), true))
}

//endregion
