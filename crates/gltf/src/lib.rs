//! glTF 2.0, GLB, VRM and VRMA loading into Slerp's scene model.
//!
//! Besides core glTF this reads what games and avatar tools ship: meshopt
//! and Draco compression, quantized vertex data, Basis Universal textures, texture
//! transforms, the common material extensions, skins with any number of
//! joints, morph targets with names, and VRM's humanoid bone map,
//! expressions and toon materials.  The JSON is read with serde structs of
//! our own (see `json.rs`), so files requiring extensions Slerp doesn't
//! know still load as far as possible.
//!
//! Loading never panics on malformed input.  Problems that only cost part
//! of the file (a missing texture, an unreadable primitive) become
//! warnings in `Info::warnings`; a file that isn't glTF, or that would need
//! more memory than its size can justify, fails.
//!
//! Plain Draco files (`.drc`), which hold one mesh or point cloud, load
//! through `load_drc`.
//!
//! The crate also writes: `write_glb` turns a scene, or a node, mesh or
//! clip of one, into a GLB file (see `write`).

mod accessor;
mod anim;
mod data;
mod draco;
mod json;
mod loader;
mod materials;
mod meshes;
mod thumbnail;
mod vrm;
mod write;

pub use write::{Glb, GlbOptions, Placement, Selection, WriteError, write_glb};

use slerp_scene::{Asset, Limits, LoadError};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// True when the first bytes (or the extension, for .gltf JSON) say this is
/// a glTF, GLB or VRM file.
pub fn sniff(path: &Path, head: &[u8]) -> bool {
    if head.starts_with(b"glTF") {
        return true;
    }
    let gltf_extension = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("gltf"));
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    gltf_extension && head.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{')
}

/// True when the first bytes start a plain Draco file (`.drc`).
pub fn sniff_drc(head: &[u8]) -> bool {
    draco::sniff(head)
}

/// Loads a plain Draco file: a mesh, or a point cloud when it has no faces.
pub fn load_drc(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Asset, LoadError> {
    draco::load_drc(path, bytes, limits).map(Asset::Scene)
}

/// The picture a VRM avatar carries of itself (VRM 0.x `meta.texture`,
/// 1.0 `meta.thumbnailImage`), read without the model; `None` for files
/// without one, which includes every plain glTF.  Slerp's core uses it for
/// thumbnails in place of a render; see `slerp_core::saved_picture`.
pub fn saved_picture(path: &Path, limits: &Limits) -> Result<Option<slerp_scene::Texture>, LoadError> {
    thumbnail::read(path, limits.max_texture_size)
}

/// Loads a glTF/GLB/VRM file.  Returns Asset::Scene.
pub fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Asset, LoadError> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{} bytes", bytes.len())));
    }
    if !sniff(path, &bytes[..bytes.len().min(64)]) && !bytes.trim_ascii_start().starts_with(b"{") {
        return Err(LoadError::NotMine);
    }
    loader::load_scene(path, bytes, limits).map(Asset::Scene)
}

/*┌─────────────────╮
| Context         |
└─────────────────╯*/
//region Context

/// The smallest and largest number of bytes a load may decode, whatever the
/// file's size.
const MIN_DECODE_BUDGET: u64 = 256 << 20;
const MAX_DECODE_BUDGET: u64 = 16 << 30;

/// How many decoded bytes each input byte may justify.  Meshopt and sparse
/// accessors legitimately expand data many times over (morph targets most
/// of all), but a few bytes claiming gigabytes of vertices are an attack or
/// a corrupt file.
const DECODE_RATIO: u64 = 64;

/// Shared state of one load, used from many threads: the budget of bytes
/// to decode, the budget of neighbouring files to read, warnings, and the
/// neighbouring files read.
pub(crate) struct Ctx {
    input: AtomicU64,
    used: AtomicU64,
    file_budget: AtomicU64,
    warnings: Mutex<Vec<String>>,
    files: Mutex<Vec<PathBuf>>,
}

const MAX_WARNINGS: usize = 100;

impl Ctx {
    /// `input` is the size of the main file; `file_budget` how many bytes
    /// of neighbouring files may still be read.
    pub fn new(input: u64, file_budget: u64) -> Ctx {
        Ctx {
            input: AtomicU64::new(input),
            used: AtomicU64::new(0),
            file_budget: AtomicU64::new(file_budget),
            warnings: Mutex::new(Vec::new()),
            files: Mutex::new(Vec::new()),
        }
    }

    /// Claims `bytes` of the decoding budget before allocating them.
    pub fn reserve(&self, bytes: u64) -> Result<(), LoadError> {
        let input = self.input.load(Ordering::Relaxed);
        let limit = input.saturating_mul(DECODE_RATIO).clamp(MIN_DECODE_BUDGET, MAX_DECODE_BUDGET);
        let too_large = || LoadError::TooLarge(format!("decoding would need over {} MB", limit >> 20));
        if bytes > limit {
            return Err(too_large());
        }
        let before = self.used.fetch_add(bytes, Ordering::Relaxed);
        if before + bytes > limit {
            return Err(too_large());
        }
        Ok(())
    }

    /// How many bytes of the decoding budget are left, for decoders that
    /// take a ceiling up front (Draco).
    pub fn remaining(&self) -> u64 {
        let input = self.input.load(Ordering::Relaxed);
        let limit = input.saturating_mul(DECODE_RATIO).clamp(MIN_DECODE_BUDGET, MAX_DECODE_BUDGET);
        limit.saturating_sub(self.used.load(Ordering::Relaxed))
    }

    /// Claims `bytes` of the neighbouring-file budget, returning false when
    /// there isn't enough left.  What is read also raises the decoding
    /// budget, as the main file's size does.
    pub fn take_file_bytes(&self, bytes: u64) -> bool {
        let taken = self.file_budget.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| left.checked_sub(bytes));
        if taken.is_ok() {
            self.input.fetch_add(bytes, Ordering::Relaxed);
        }
        taken.is_ok()
    }

    /// Records a problem worked around.  Repeats are dropped, and so is
    /// everything after the first hundred.
    pub fn warn(&self, message: impl Into<String>) {
        let message = message.into();
        let mut warnings = self.warnings.lock().unwrap_or_else(|e| e.into_inner());
        if warnings.len() < MAX_WARNINGS && !warnings.contains(&message) {
            warnings.push(message);
        }
    }

    /// Notes a neighbouring file read or looked for, for `Info::neighbours`.
    pub fn note_file(&self, path: &Path) {
        let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        if !files.iter().any(|f| f == path) {
            files.push(path.to_path_buf());
        }
    }

    pub fn take_files(&self) -> Vec<PathBuf> {
        std::mem::take(&mut *self.files.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn into_warnings(self) -> Vec<String> {
        self.warnings.into_inner().unwrap_or_else(|e| e.into_inner())
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffing() {
        assert!(sniff(Path::new("a.glb"), b"glTF\x02\0\0\0"));
        assert!(sniff(Path::new("a.vrm"), b"glTF\x02\0\0\0"));
        assert!(sniff(Path::new("a.gltf"), b"  \n{\"asset\""));
        assert!(sniff(Path::new("A.GLTF"), b"\xEF\xBB\xBF{"));
        assert!(!sniff(Path::new("a.json"), b"{\"asset\""));
        assert!(!sniff(Path::new("a.gltf"), b"<xml"));
        assert!(!sniff(Path::new("a.glb"), b"PK\x03\x04"));
        assert!(!sniff(Path::new("a.gltf"), b""));
    }

    #[test]
    fn budget() {
        let ctx = Ctx::new(1, 10);
        assert!(ctx.reserve(MIN_DECODE_BUDGET).is_ok());
        assert!(ctx.reserve(1).is_err());
        assert!(Ctx::new(1, 0).reserve(u64::MAX).is_err());
        assert!(ctx.take_file_bytes(10));
        assert!(!ctx.take_file_bytes(1));
    }
}
