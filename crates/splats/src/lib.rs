//! Loaders for Gaussian splats (3D Gaussian splatting): the reference
//! implementation's PLY files, with spherical harmonics up to degree 3,
//! SuperSplat's compressed PLY, antimatter15's `.splat`, Niantic's SPZ
//! (versions 1 to 4) and PlayCanvas's SOG.  Each fills `Scene::splats`
//! with Y-up coordinates.
//!
//! The formats disagree about axes.  PLY, SOG and `.splat` files keep the
//! training's camera convention (x right, y down, z forward), so they are
//! turned 180 degrees about X, as Niantic's own converter does; SPZ already
//! stores x right, y up, z back, which is Slerp's.  Turning a cloud also
//! turns the view-dependent colour, so spherical-harmonic coefficients
//! change sign where their basis functions do.
//!
//! Captures run to millions of splats and gigabytes, and thumbnailers see
//! them on every folder visit, so the large formats are read as streams
//! with counts checked against the bytes really present, and `Limits::
//! max_splats` thins a cloud evenly while reading rather than after.
//! Nothing here may panic on bad input; `load` also catches panics as a
//! last line of defence.

mod compressed;
mod ply;
mod sog;
mod splat;
mod spz;

use glam::{Quat, Vec3, Vec4};
use half::f16;
use half::slice::{HalfBitsSliceExt, HalfFloatSliceExt};
use slerp_scene::{Info, Limits, LoadError, Scene, Splats};
use std::path::Path;

/// The formats this crate reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplatFormat {
    /// The 3D Gaussian splatting reference PLY (`f_dc_0`, `f_rest_*`,
    /// `opacity`, `scale_*`, `rot_*`).
    Ply,
    /// SuperSplat's and PlayCanvas's PLY of 256-splat chunks with
    /// quantized fields.
    CompressedPly,
    /// antimatter15's 32-byte records.
    Splat,
    /// Niantic's gzip (versions 1 to 3) or zstd (version 4) format.
    Spz,
    /// PlayCanvas's zip of WebP images.
    Sog,
}

impl SplatFormat {
    /// A short human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            SplatFormat::Ply => "3D Gaussian splats (PLY)",
            SplatFormat::CompressedPly => "compressed Gaussian splats (SuperSplat PLY)",
            SplatFormat::Splat => "Gaussian splats (.splat)",
            SplatFormat::Spz => "Niantic SPZ Gaussian splats",
            SplatFormat::Sog => "PlayCanvas SOG Gaussian splats",
        }
    }
}

/// Recognises a splat file from its first bytes (a few kilobytes, so a
/// PLY header is seen whole) and its name.  PLY files only count when their
/// header has splat properties, so mesh and point-cloud PLYs stay with the
/// model loader.  `.splat` files have no signature at all; their first
/// record must look like a splat.
pub fn sniff(path: &Path, head: &[u8]) -> Option<SplatFormat> {
    if head.starts_with(b"ply\n") || head.starts_with(b"ply\r\n") {
        return ply::sniff(head);
    }
    if head.starts_with(spz::MAGIC) || spz::is_gzipped_spz(head) {
        return Some(SplatFormat::Spz);
    }
    if sog::sniff(path, head) {
        return Some(SplatFormat::Sog);
    }
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    if ext == "splat" && splat::plausible(head) {
        return Some(SplatFormat::Splat);
    }
    None
}

/// Loads a splat file into a scene holding only `Scene::splats`.  Reads
/// the file itself, streaming the large formats, and refuses files larger
/// than `limits.max_file_bytes`.
pub fn load(format: SplatFormat, path: &Path, limits: &Limits) -> Result<Scene, LoadError> {
    let size = std::fs::metadata(path)?.len();
    if size > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{size} bytes")));
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match format {
        SplatFormat::Ply => ply::load(path, size, limits),
        SplatFormat::CompressedPly => compressed::load(&std::fs::read(path)?, limits),
        SplatFormat::Splat => splat::load(path, size, limits),
        SplatFormat::Spz => spz::load(&std::fs::read(path)?, limits),
        SplatFormat::Sog => sog::load(&std::fs::read(path)?, limits),
    }));
    let mut scene = match result {
        Ok(scene) => scene?,
        Err(_) => return Err(LoadError::invalid(format!("{} loader failed unexpectedly", format.name()))),
    };
    scene.name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(scene)
}

/// Loads splats from bytes already in memory: what `load` does, for tests
/// and callers that have the file's contents.
pub fn load_bytes(format: SplatFormat, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{} bytes", bytes.len())));
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match format {
        SplatFormat::Ply => ply::load_reader(std::io::Cursor::new(bytes), bytes.len() as u64, limits),
        SplatFormat::CompressedPly => compressed::load(bytes, limits),
        SplatFormat::Splat => splat::load_reader(std::io::Cursor::new(bytes), bytes.len() as u64, limits),
        SplatFormat::Spz => spz::load(bytes, limits),
        SplatFormat::Sog => sog::load(bytes, limits),
    }));
    match result {
        Ok(scene) => scene,
        Err(_) => Err(LoadError::invalid(format!("{} loader failed unexpectedly", format.name()))),
    }
}

/*┌─────────────────╮
| Building        |
└─────────────────╯*/
//region Building

/// The spherical-harmonic band 0 constant: a DC coefficient `c` is the
/// colour 0.5 + `SH_C0` × `c`.
pub(crate) const SH_C0: f32 = 0.282_094_8;

/// Sign changes of the 15 coefficients of bands 1 to 3 when a cloud turns
/// 180 degrees about X (y and z negated): each basis function is a
/// polynomial in x, y and z, and flips when it is odd in y and z together.
const FLIP_X_SH: [f32; 15] = [-1., -1., 1., -1., 1., 1., -1., 1., -1., 1., -1., -1., 1., -1., 1.];

/// The degree of spherical harmonics with `per_channel` coefficients above
/// band 0, capped at 3 (SPZ allows 4, whose fourth band is dropped).
pub(crate) fn degree_for(per_channel: usize) -> Option<u8> {
    match per_channel {
        0 => Some(0),
        3 => Some(1),
        8 => Some(2),
        15 | 24 => Some(3),
        _ => None,
    }
}

pub(crate) fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Collects splats as a reader decodes them, keeping every `step`-th one
/// when the file holds more than `Limits::max_splats`, and turning them
/// into Slerp's axes.
pub(crate) struct Builder {
    pub splats: Splats,
    /// Splats in the file.
    total: u64,
    /// Keep splat `i` when `i % step == 0`.
    step: u64,
    /// Turn 180 degrees about X: the file is y down, z forward.
    flip: bool,
    /// Coefficients per channel kept (0, 3, 8 or 15).
    sh_kept: usize,
    /// Coefficients waiting to become half floats, converted in batches,
    /// which uses the CPU's conversion instructions where it has them.
    sh_stage: Vec<f32>,
    /// Splats dropped for non-finite values.
    pub dropped: u64,
}

impl Builder {
    pub fn new(total: u64, limits: &Limits, sh_degree: u8, flip: bool) -> Builder {
        let max = limits.max_splats.max(1);
        let step = total.div_ceil(max).max(1);
        // Reserve for what is kept, but no more than a few million: a count
        // from a damaged header mustn't allocate before the data is there.
        let kept = total.div_ceil(step).min(1 << 22) as usize;
        let sh_kept = Splats::sh_coefficients(sh_degree);
        let splats = Splats {
            positions: Vec::with_capacity(kept),
            scales: Vec::with_capacity(kept),
            rotations: Vec::with_capacity(kept),
            colors: Vec::with_capacity(kept),
            sh_degree: sh_degree.min(3),
            sh: Vec::with_capacity(kept * sh_kept * 3),
            antialiased: false,
        };
        Builder { splats, total, step, flip, sh_kept, sh_stage: Vec::new(), dropped: 0 }
    }

    /// Whether splat `i` of the file is kept.
    pub fn keeps(&self, i: u64) -> bool {
        i.is_multiple_of(self.step)
    }

    /// Adds one splat.  `scale` is in metres, `rotation` any non-zero
    /// quaternion, `color` the base colour and opacity, and `sh(k, c)`
    /// returns coefficient `k` (0 to 14) of channel `c` above band 0.  A
    /// splat with non-finite values is dropped.
    pub fn push(&mut self, position: Vec3, scale: Vec3, rotation: Quat, color: Vec4, sh: impl Fn(usize, usize) -> f32) {
        if !position.is_finite() || !scale.is_finite() || !rotation.is_finite() || !color.is_finite() {
            self.dropped += 1;
            return;
        }
        let rotation = if rotation.length_squared() > 1e-12 { rotation.normalize() } else { Quat::IDENTITY };
        let (position, rotation) = if self.flip {
            (
                Vec3::new(position.x, -position.y, -position.z),
                Quat::from_xyzw(rotation.x, -rotation.y, -rotation.z, rotation.w),
            )
        } else {
            (position, rotation)
        };
        self.splats.positions.push(position);
        self.splats.scales.push(scale.abs());
        self.splats.rotations.push(rotation);
        self.splats.colors.push(color.with_w(color.w.clamp(0.0, 1.0)));
        for (k, &flip) in FLIP_X_SH.iter().enumerate().take(self.sh_kept) {
            let sign = if self.flip { flip } else { 1.0 };
            for c in 0..3 {
                let v = sh(k, c) * sign;
                self.sh_stage.push(if v.is_finite() { v } else { 0.0 });
            }
        }
        if self.sh_stage.len() >= 1 << 16 {
            self.flush_sh();
        }
    }

    fn flush_sh(&mut self) {
        let start = self.splats.sh.len();
        self.splats.sh.resize(start + self.sh_stage.len(), 0);
        let out: &mut [f16] = self.splats.sh[start..].reinterpret_cast_mut();
        out.convert_from_f32_slice(&self.sh_stage);
        self.sh_stage.clear();
    }

    /// The scene, with facts about the file for the inspector.
    pub fn finish(mut self, mut info: Info) -> Result<Scene, LoadError> {
        self.flush_sh();
        if self.splats.is_empty() {
            return Err(LoadError::invalid("no usable splats"));
        }
        let kept = self.splats.len() as u64;
        info.details.insert(0, ("Splats".into(), group(self.total)));
        if self.splats.sh_degree > 0 {
            info.details.push(("Spherical harmonics".into(), format!("degree {}", self.splats.sh_degree)));
        }
        if self.splats.antialiased {
            info.details.push(("Antialiased".into(), "yes (Mip-Splatting)".into()));
        }
        if self.step > 1 {
            info.details.push(("Loaded".into(), format!("{} (every {} splats)", group(kept), group(self.step))));
        }
        if self.dropped > 0 {
            info.warnings.push(format!("skipped {} splats with invalid numbers", group(self.dropped)));
        }
        Ok(Scene { splats: Some(self.splats), info, ..Default::default() })
    }
}

/// 109278 as "109,278".
pub(crate) fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

//endregion

#[cfg(test)]
mod tests;
