//! Level editors' source files, shown as the levels they describe: Quake,
//! Half-Life, Quake 2 and Quake 3 `.map` files (TrenchBroom, J.A.C.K.,
//! Radiant, ericw-tools' input) and Source's `.vmf` files (Hammer).
//!
//! Both store brushes as planes; `brush` rebuilds their faces as the
//! compilers do, `level` turns faces into a scene lit by Slerp's own
//! lights (source files have no lightmaps), and each format's module finds
//! its textures: WADs and loose pictures for `.map`, VMT materials and VTF
//! textures (loose or in VPK packs) for `.vmf`.

mod brush;
mod displacement;
mod keyvalues;
mod level;
mod mapfile;
mod outside;
mod patch;
mod quakemap;
mod sky;
mod vmf;
pub(crate) mod vmt;

use slerp_scene::{Limits, LoadError, Scene};
use std::path::Path;

/// Whether the start of a file is a Quake-family `.map`.  Many unrelated
/// formats use the extension (JavaScript and CSS source maps, Doxygen image
/// maps, linker maps), so this goes by content only.
pub(crate) fn sniff_map(head: &[u8]) -> bool {
    mapfile::sniff(head)
}

/// Loads a Quake-family `.map` as a level.
pub(crate) fn load_map(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    quakemap::load(path, bytes, limits)
}

/// Whether the start of a file is a Hammer `.vmf` map.
pub(crate) fn sniff_vmf(head: &[u8]) -> bool {
    vmf::sniff(head)
}

/// Loads a Hammer `.vmf` as a level.
pub(crate) fn load_vmf(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    vmf::load(path, bytes, limits)
}
