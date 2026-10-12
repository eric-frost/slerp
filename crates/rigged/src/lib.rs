//! Skinned model formats from games and animation tools, read into Slerp's
//! scene model with their skeletons, skins, morph targets and animation
//! clips:
//!
//! - MikuMikuDance: PMX 2.0 and 2.1 models, PMD models, and VMD motions,
//!   which play on a model when they sit beside it or are added in the
//!   viewer (`mmd`).
//! - Doom 3: `.md5mesh` models and `.md5anim` animations, found beside the
//!   mesh (`md5`).
//! - Inter-Quake Model: binary `.iqm` and its text form `.iqe` (`iqm`,
//!   `iqe`).
//! - Valve's Source SMD: reference meshes and skeletal animations, the
//!   animations in the mesh's folder offered as clips (`smd`).
//! - Unreal's ActorX: `.psk` and `.pskx` meshes and `.psa` animations
//!   (`psk`).
//!
//! Every format converts to right-handed, Y-up metres with the model
//! facing +Z (`basis`), and describes its skeleton and poses to
//! `skeleton`, which builds the nodes, skin and clips.  Animation files
//! opened on their own (VMD, MD5 anims, PSA, animation SMDs) load as a
//! skeleton with clips and no mesh, which is what the viewer's "Add
//! animations" needs; their thumbnails are left to nothing.
//!
//! Loaders run on files a user merely browses past, so none may panic,
//! hang or allocate without bound on malformed input: counts are checked
//! against the bytes present, decoding is charged to a budget relative to
//! the file's size (`skeleton::Budget`), and `load` catches any panic as a
//! last line of defence.

pub mod basis;
mod iqe;
mod iqm;
mod md5;
mod mesh;
mod mmd;
mod psk;
pub mod read;
pub mod skeleton;
pub mod smd;
mod textures;

use slerp_scene::{Asset, Limits, LoadError, Scene};
use std::path::Path;

/// The formats this crate reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RiggedFormat {
    /// MikuMikuDance PMX 2.0 or 2.1 model.
    Pmx,
    /// MikuMikuDance PMD model.
    Pmd,
    /// MikuMikuDance VMD motion.
    Vmd,
    /// Doom 3 model.
    Md5Mesh,
    /// Doom 3 animation.
    Md5Anim,
    /// Inter-Quake Model, binary.
    Iqm,
    /// Inter-Quake Export, the text form.
    Iqe,
    /// Valve's Source (and GoldSrc) SMD: a reference mesh or an animation.
    Smd,
    /// Unreal ActorX skeletal mesh (`.psk`, `.pskx`).
    Psk,
    /// Unreal ActorX animation set.
    Psa,
}

impl RiggedFormat {
    /// A short human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            RiggedFormat::Pmx => "MikuMikuDance PMX",
            RiggedFormat::Pmd => "MikuMikuDance PMD",
            RiggedFormat::Vmd => "MikuMikuDance motion (VMD)",
            RiggedFormat::Md5Mesh => "Doom 3 MD5 mesh",
            RiggedFormat::Md5Anim => "Doom 3 MD5 animation",
            RiggedFormat::Iqm => "Inter-Quake Model (IQM)",
            RiggedFormat::Iqe => "Inter-Quake Export (IQE)",
            RiggedFormat::Smd => "Source SMD",
            RiggedFormat::Psk => "Unreal ActorX mesh (PSK)",
            RiggedFormat::Psa => "Unreal ActorX animation (PSA)",
        }
    }

    /// True for files that hold only animation: a skeleton and clips,
    /// nothing to draw.
    pub fn is_animation(self) -> bool {
        matches!(self, RiggedFormat::Vmd | RiggedFormat::Md5Anim | RiggedFormat::Psa)
    }
}

/// Recognises a format by its first bytes; every one has a signature, so
/// names are only a tiebreak (an MD5 mesh and animation share theirs).
/// `head` should hold the first few kilobytes.
pub fn sniff(_path: &Path, head: &[u8]) -> Option<RiggedFormat> {
    if head.starts_with(b"PMX ") && head.len() >= 8 {
        let version = f32::from_le_bytes([head[4], head[5], head[6], head[7]]);
        return (2.0..2.2).contains(&version).then_some(RiggedFormat::Pmx);
    }
    if head.starts_with(b"Pmd") && head.get(3..7) == Some(1.0f32.to_le_bytes().as_slice()) {
        return Some(RiggedFormat::Pmd);
    }
    if head.starts_with(b"Vocaloid Motion Data") {
        return Some(RiggedFormat::Vmd);
    }
    if head.starts_with(b"INTERQUAKEMODEL\0") {
        return Some(RiggedFormat::Iqm);
    }
    if head.starts_with(b"ACTRHEAD\0") {
        return Some(RiggedFormat::Psk);
    }
    if head.starts_with(b"ANIMHEAD\0") {
        return Some(RiggedFormat::Psa);
    }
    let text = read::text(&head[..head.len().min(4096)]);
    if iqe::looks_like_iqe(&text) {
        return Some(RiggedFormat::Iqe);
    }
    if let Some(anim) = md5::kind(&text) {
        return Some(if anim { RiggedFormat::Md5Anim } else { RiggedFormat::Md5Mesh });
    }
    if smd::looks_like_smd(&text) {
        return Some(RiggedFormat::Smd);
    }
    None
}

/// Loads a file already read into `bytes`.  `path` names it, so that
/// textures and animation files beside it can be found when
/// `limits.neighbours` allows.
pub fn load(format: RiggedFormat, path: &Path, bytes: &[u8], limits: &Limits) -> Result<Asset, LoadError> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{} bytes", bytes.len())));
    }
    // The readers are written not to panic; this is a last line of defence,
    // so that a bug costs one thumbnail rather than the helper process.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        slerp_scene::collect_neighbours(|| match format {
            RiggedFormat::Pmx => mmd::load_pmx(path, bytes, limits),
            RiggedFormat::Pmd => mmd::load_pmd(path, bytes, limits),
            RiggedFormat::Vmd => mmd::load_vmd(path, bytes, limits),
            RiggedFormat::Md5Mesh => md5::load_mesh(path, bytes, limits),
            RiggedFormat::Md5Anim => md5::load_anim(path, bytes, limits),
            RiggedFormat::Iqm => iqm::load(path, bytes, limits),
            RiggedFormat::Iqe => iqe::load(path, bytes, limits),
            RiggedFormat::Smd => smd::load(path, bytes, limits),
            RiggedFormat::Psk => psk::load_psk(path, bytes, limits),
            RiggedFormat::Psa => psk::load_psa(path, bytes, limits),
        })
    }));
    let (scene, neighbours) = match result {
        Ok(r) => r,
        Err(_) => return Err(LoadError::invalid(format!("{} reader failed unexpectedly", format.name()))),
    };
    let mut scene = scene?;
    scene.info.neighbours = neighbours;
    if scene.name.is_empty() {
        scene.name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    }
    if scene.info.format.is_empty() {
        scene.info.format = format.name().to_string();
    }
    if scene.triangle_count() as u64 > limits.max_triangles {
        return Err(LoadError::TooLarge(format!("{} triangles", scene.triangle_count())));
    }
    if !scene.animations.is_empty() {
        scene.info.details.push(("Animations".into(), scene.animations.len().to_string()));
    }
    Ok(Asset::Scene(scene))
}

/// Adds the facts every skinned scene shares to its inspector details.
fn describe_rig(scene: &mut Scene, rig: &skeleton::Rig) {
    scene.info.details.push(("Bones".into(), rig.len().to_string()));
    if rig.repaired > 0 {
        scene.info.warnings.push(format!("{} bones named a parent that doesn't exist or made a loop", rig.repaired));
    }
}

#[cfg(test)]
mod tests;
