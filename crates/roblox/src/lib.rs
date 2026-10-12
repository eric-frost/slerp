//! Roblox places and models: binary `.rbxl`/`.rbxm` and XML `.rbxlx`/`.rbxmx`
//! files, drawn as scenes without Roblox's software and without the
//! network.
//!
//! A place is a tree of instances; what is seen in a game is the
//! `Workspace`: parts (blocks, balls, cylinders, wedges, corner wedges,
//! trusses), the meshes that reshape them, unions, mesh parts and the voxel
//! terrain.  Each part carries its own world position (`CFrame`, in studs),
//! size, colour, transparency and material.  Roblox draws its materials with
//! its own textures, which Slerp neither ships nor downloads; materials here
//! are colour, roughness and metalness.
//!
//! What lives on Roblox's servers stays there: a mesh part's mesh and a
//! union's triangles are usually only an asset id.  A union the file keeps
//! the triangles of (`MeshData`) is drawn exactly, and so is one whose
//! source parts are kept (`ChildData`) and that subtracts nothing; mesh parts
//! and other unions are drawn by the convex hulls Roblox keeps for their
//! collisions (`PhysicalConfigData`), which outline their shape, or as their
//! boxes, with a note saying how many.  Decals and textures name images on
//! the servers and are not drawn.
//!
//! The readers are written for this crate (`binary`, `xml`) rather than
//! taken from rbx-dom: its reader trusts the counts in a file and aborts the
//! process on an absurd one, and its XML reader overflows the stack on
//! deeply nested files.  rbx-dom's specifications describe both formats.
//! Units: 1 stud is 0.28 m, Roblox's own conversion; Roblox is Y-up and
//! right-handed like Slerp.

mod binary;
mod build;
mod csg;
mod dom;
mod palette;
mod shapes;
mod terrain;
mod xml;

#[cfg(test)]
mod tests;

use slerp_scene::{Limits, LoadError, Scene};
use std::path::Path;

/// Which of the two encodings a file uses.  Places and models share both;
/// a place is told apart by its `Workspace`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RobloxFormat {
    Binary,
    Xml,
}

/// Recognises a Roblox file by its first bytes: `<roblox!` and a fixed
/// signature for binary files, a `<roblox` root element for XML.  The name
/// isn't needed: old XML places used `.rbxl` too.
pub fn sniff(_path: &Path, head: &[u8]) -> Option<RobloxFormat> {
    if head.starts_with(binary::MAGIC) {
        Some(RobloxFormat::Binary)
    } else if xml::sniff(head) {
        Some(RobloxFormat::Xml)
    } else {
        None
    }
}

/// Loads a place or model.  Reading is wrapped in a panic guard as a last
/// line of defence: the readers check every count, but a scene builder
/// working on whatever a file says should never take the thumbnailer down.
pub fn load(format: RobloxFormat, path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    let _ = path;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| load_inner(format, bytes, limits)))
        .unwrap_or_else(|_| Err(LoadError::invalid("the Roblox file could not be read")))
}

fn load_inner(format: RobloxFormat, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    let mut budget = Budget::new(bytes.len() as u64, limits);
    let dom = read(format, bytes, &mut budget)?;
    let name = match format {
        RobloxFormat::Binary => "binary",
        RobloxFormat::Xml => "XML",
    };
    build::build(&dom, limits, &mut budget, name)
}

fn read(format: RobloxFormat, bytes: &[u8], budget: &mut Budget) -> Result<dom::Dom, LoadError> {
    match format {
        RobloxFormat::Binary => binary::read(bytes, budget),
        RobloxFormat::Xml => xml::read(bytes, budget),
    }
}

/// How much one file may make Slerp hold: bytes decompressed or decoded
/// from it, and instances.  Unions' source parts are models inside the file
/// and draw from the same budget, so nesting can't multiply it.
pub(crate) struct Budget {
    bytes: u64,
    instances: usize,
    /// Bytes of decoded terrain voxels.  Terrain compresses far more than
    /// anything else (a flat plain is a few runs per chunk), so it has its
    /// own allowance rather than a share of the file's.
    terrain: u64,
}

impl Budget {
    fn new(file_len: u64, limits: &Limits) -> Budget {
        let thumbnail = limits.max_texture_size.is_some();
        // Binary chunks compress about 3 to 10 times; 64 times the file is
        // far beyond any real place, and the ceiling stops a small file
        // claiming gigabytes.
        let ceiling: u64 = if thumbnail { 1 << 30 } else { 4 << 30 };
        Budget {
            bytes: file_len.saturating_mul(64).saturating_add(16 << 20).min(ceiling),
            instances: if thumbnail { 2_000_000 } else { 8_000_000 },
            terrain: if thumbnail { 256 << 20 } else { 1 << 30 },
        }
    }

    pub(crate) fn take_bytes(&mut self, n: u64) -> Result<(), LoadError> {
        self.bytes = self
            .bytes
            .checked_sub(n)
            .ok_or_else(|| LoadError::TooLarge("the file expands to more data than it could hold".into()))?;
        Ok(())
    }

    pub(crate) fn take_terrain(&mut self, n: u64) -> Result<(), LoadError> {
        self.terrain =
            self.terrain.checked_sub(n).ok_or_else(|| LoadError::TooLarge("more terrain than Slerp draws".into()))?;
        Ok(())
    }

    pub(crate) fn take_instances(&mut self, n: usize) -> Result<(), LoadError> {
        self.instances = self
            .instances
            .checked_sub(n)
            .ok_or_else(|| LoadError::TooLarge("more instances than Slerp reads".into()))?;
        Ok(())
    }
}
