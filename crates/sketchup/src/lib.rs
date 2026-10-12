//! SketchUp models (`.skp`), read without SketchUp or its SDK.
//!
//! SketchUp's format is closed.  The layouts read here were found by the
//! OpenSKP project (<https://github.com/iamahsanmehmood/openskp>, MIT
//! licence, by Ahsan Mehmood) through reverse engineering, and checked by
//! it against the SketchUp SDK; this crate is a Rust port of OpenSKP's
//! Python readers (see `NOTICE`).  Both containers are read:
//!
//! - SketchUp 2021 and later ([`vff`]): a header, then a zip holding
//!   `model.dat` (tag-length-value records), the materials as XML with
//!   their pictures, and the styles.
//! - SketchUp 3 to 2020 ([`legacy`]): one MFC object stream, walked object
//!   by object.  OpenSKP's own sweep reads most real files of every
//!   version; the rest fail with an error, and the viewer then shows the
//!   picture SketchUp stored in the file.
//!
//! What is drawn ([`build`]): faces with holes (triangulated with
//! `earcut`), each side with its own material (SketchUp's faces are
//! two-sided) or the style's front and back colours; textures, placed as
//! SketchUp places them or as positioned per face; colours and opacity;
//! groups and components with their transforms and nesting, each mesh
//! built once and shared; tags and hidden things kept but switched off;
//! edges as black lines (soft edges not drawn, smooth edges blending the
//! shading); inches converted to metres and Z-up to Y-up.  The view the
//! file was saved with is kept for the viewer ([`saved_view`]).
//!
//! Not drawn: guides, dimensions, text, section planes and section cuts,
//! the "always face camera" turn of 2D people and trees, styles other than
//! their face colours, and edge profiles.
//!
//! An undocumented format can be misread without any error, so the
//! builder checks what it got: loops of edges must close, faces must be
//! flat and have area.  When many don't, the scene carries a warning
//! rather than passing garbage off as the model.
//!
//! Thumbnails keep using the picture SketchUp saved in the file (that is
//! in `slerp-previews`); the model is drawn for files without one.

mod build;
pub mod legacy;
mod model;
mod tlv;
mod triangulate;
pub mod vff;
mod xml;

pub use model::*;

use glam::Vec3;
use slerp_scene::{Aabb, Limits, LoadError, OrbitCamera, Scene};
use std::path::Path;

/// The UTF-16 byte-order mark and text that start every SketchUp file.
const MAGIC: &[u8] = b"\xFF\xFE\xFF\x0ES\0k\0e\0t\0c\0h\0U\0p\0 \0M\0o\0d\0e\0l\0";

/// Caps on what a file may make the reader allocate.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    /// The largest zip entry unpacked (`model.dat`, a texture).
    pub max_entry_bytes: u64,
}

impl Caps {
    fn from(limits: &Limits) -> Caps {
        Caps { max_entry_bytes: limits.max_file_bytes.saturating_mul(64).clamp(64 << 20, 4 << 30) }
    }
}

/// Whether `head`, the start of a file, is a SketchUp model.
pub fn sniff(head: &[u8]) -> bool {
    head.starts_with(MAGIC)
}

/// The version in the header, such as "17.2.2555".
pub fn header_version(data: &[u8]) -> Option<String> {
    let end = data.len().min(0x200);
    let units: Vec<u16> = data[..end].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let text = String::from_utf16_lossy(&units);
    let open = text.find('{')?;
    let close = open + text[open..].find('}')?;
    Some(text[open + 1..close].to_string())
}

fn major(version: &str) -> u32 {
    version.split('.').next().and_then(|v| v.parse().ok()).unwrap_or(0)
}

/// SketchUp's name for a major version: "SketchUp 8", "SketchUp 2017".
pub fn release_name(major: u32) -> String {
    match major {
        0 => "SketchUp".into(),
        m if m < 13 => format!("SketchUp {m}"),
        m => format!("SketchUp {}", 2000 + m),
    }
}

/// Reads a file's model without building a scene.
pub fn read(data: &[u8], limits: &Limits) -> Result<Model, LoadError> {
    if !sniff(data) {
        return Err(LoadError::NotMine);
    }
    let caps = Caps::from(limits);
    let version = header_version(data).unwrap_or_default();
    // A panic would mean a bug in the readers; a damaged file mustn't take
    // the viewer or thumbnailer down with it.
    let read = std::panic::catch_unwind(|| {
        if vff::zip_start(data).is_some() {
            vff::read(data, &caps)
        } else if legacy::is_classic(data) {
            legacy::read(data, &caps)
        } else {
            Err(LoadError::Unsupported("a SketchUp file in neither known container".into()))
        }
    });
    let mut model = read.unwrap_or_else(|_| Err(LoadError::invalid("SketchUp reader failed on this file")))?;
    model.major = major(&version);
    model.version = version;
    Ok(model)
}

/// Loads a SketchUp file as a scene.
pub fn load_path(path: &Path, limits: &Limits) -> Result<Scene, LoadError> {
    let size = std::fs::metadata(path)?.len();
    if size > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{size} bytes")));
    }
    load(&std::fs::read(path)?, limits)
}

/// Loads a SketchUp file held in memory as a scene.
pub fn load(data: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    let model = read(data, limits)?;
    let scene = std::panic::catch_unwind(|| build::scene(&model, limits))
        .map_err(|_| LoadError::invalid("SketchUp model could not be built"))?;
    Ok(describe(&model, scene))
}

/// Adds the inspector's facts about `model` to its scene.
fn describe(model: &Model, mut scene: Scene) -> Scene {
    let info = &mut scene.info;
    info.format = format!("{} model", release_name(model.major));
    if !model.version.is_empty() {
        info.generator = Some(format!("SketchUp {}", model.version));
    }
    let mut details = vec![(
        "Container".to_string(),
        match model.container {
            Some(Container::Vff) => "zip with model.dat (2021 and later)".into(),
            _ => "classic object stream (2020 and earlier)".into(),
        },
    )];
    let (faces, edges, instances) = model.counts();
    details.push((
        "Components and groups".into(),
        format!("{} definitions, {} placements in them", model.definitions.len(), instances),
    ));
    details.push(("Faces".into(), faces.to_string()));
    details.push(("Edges".into(), edges.to_string()));
    let textured = model.materials.iter().filter(|m| m.texture.is_some()).count();
    details.push(("Materials".into(), format!("{} ({textured} textured)", model.materials.len())));
    let hidden = model.layers.iter().filter(|l| l.hidden).count();
    details.push(("Tags".into(), format!("{} ({hidden} hidden)", model.layers.len())));
    if !model.pages.is_empty() {
        let names: Vec<&str> = model.pages.iter().map(|p| p.name.as_str()).collect();
        details.push(("Scenes".into(), names.join(", ")));
    }
    if let Some(c) = model.camera {
        details.push((SAVED_VIEW.into(), describe_camera(&c)));
    }
    details.append(&mut info.details);
    info.details = details;
    info.warnings.splice(0..0, model.warnings.iter().cloned());
    scene
}

/*┌─────────────────╮
| Saved view      |
└─────────────────╯*/
//region Saved view

/// The name of the inspector fact that carries the saved view, which
/// [`saved_view`] reads back.
const SAVED_VIEW: &str = "Saved view";

/// A point in the file's inches and Z-up as the scene's metres and Y-up.
fn to_scene(p: glam::DVec3) -> Vec3 {
    (glam::DVec3::new(p.x, p.z, -p.y) * build::INCH).as_vec3()
}

/// The saved view as the inspector shows it, in the scene's coordinates:
/// "perspective, 35° field of view, from (x, y, z) m towards (x, y, z) m"
/// or "parallel projection 3.2 m high, from ...".
fn describe_camera(c: &Camera) -> String {
    let (eye, target) = (to_scene(c.eye), to_scene(c.target));
    let lens = if c.perspective {
        format!("perspective, {:.2}° field of view", c.fov)
    } else {
        format!("parallel projection {:.4} m high", c.height * build::INCH)
    };
    format!(
        "{lens}, from ({:.4}, {:.4}, {:.4}) m towards ({:.4}, {:.4}, {:.4}) m",
        eye.x, eye.y, eye.z, target.x, target.y, target.z
    )
}

/// The orbit camera that shows a SketchUp scene as it was saved, for a
/// scene whose visible parts fill `bounds`: the same eye, direction and
/// field of view, orbiting about the point of the line of sight nearest
/// the model's centre (SketchUp's own target is often far behind the
/// model).  `None` for other scenes.
pub fn saved_view(scene: &Scene, bounds: &Aabb) -> Option<OrbitCamera> {
    let text = &scene.info.details.iter().find(|(k, _)| k == SAVED_VIEW)?.1;
    let numbers: Vec<f32> = text
        .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == 'e'))
        .filter_map(|s| s.parse::<f32>().ok())
        .collect();
    let [lens, ex, ey, ez, tx, ty, tz] = numbers[..] else { return None };
    let (eye, target) = (Vec3::new(ex, ey, ez), Vec3::new(tx, ty, tz));
    let dir = (target - eye).try_normalize()?;
    let center = if bounds.is_empty() { target } else { bounds.center() };
    let along = (center - eye).dot(dir);
    let along = if along > 1e-4 { along } else { (target - eye).length() };
    let pivot = eye + dir * along;
    let back = -dir;
    let perspective = text.starts_with("perspective");
    let fov_y = if perspective { lens.to_radians().clamp(0.01, 3.0) } else { 35f32.to_radians() };
    // Parallel views keep their height: the orbit camera shows
    // distance x tan(fov / 2) above and below the pivot.
    let distance = if perspective { along } else { (lens * 0.5) / (fov_y * 0.5).tan() };
    let cam = OrbitCamera {
        target: pivot,
        yaw: back.x.atan2(back.z),
        pitch: back.y.clamp(-1.0, 1.0).asin(),
        distance: distance.max(1e-4),
        fov_y,
        ortho: !perspective,
    };
    (cam.target.is_finite() && cam.distance.is_finite()).then_some(cam)
}

//endregion

#[cfg(test)]
mod tests;
