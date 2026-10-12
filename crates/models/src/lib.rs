//! Loaders for the 3D model formats other than glTF, Quake's and USD: OBJ
//! with its `.mtl`, STL, PLY meshes and point clouds, FBX (through ufbx),
//! COLLADA, MagicaVoxel, 3MF, OFF, 3D Studio `.3ds`, DirectX `.x`, VRML 2.0
//! and X3D, Blockbench projects, and the point cloud formats LAS/LAZ, E57,
//! PCD, PTS and PTX.
//! Each turns a file into the shared `slerp_scene::Scene`, Y up and in
//! metres where the format says what a unit is.
//!
//! The newer polygon formats (3MF, OFF, 3DS, DirectX, VRML/X3D) describe
//! their meshes to `polymesh::PolyMesh`, which triangulates, generates
//! normals and splits by material for all of them.  Point clouds go through
//! `points::Cloud`, which thins, recentres and colours them.
//!
//! Every loader runs on files a user merely browses past, so none may panic,
//! hang or allocate without bound on malformed input.  They check counts
//! against the bytes actually present, respect `Limits`, and report what they
//! had to skip in `Info::warnings`.

mod archive;
mod bbmodel;
mod collada;
mod common;
#[cfg(test)]
mod corpus_tests;
mod directx;
mod e57_scan;
mod fbx;
mod fbx_thumbnail;
mod las;
mod max3ds;
mod obj;
mod off;
mod pcd;
mod ply;
mod points;
mod polymesh;
mod pts;
mod stl;
mod threemf;
mod vox;
mod vrml;

use slerp_scene::{Asset, Limits, LoadError};
use std::path::Path;

/// The formats this crate reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelFormat {
    Obj,
    Stl,
    Ply,
    Fbx,
    Collada,
    MagicaVoxel,
    Off,
    ThreeMf,
    Max3ds,
    DirectX,
    /// VRML 2.0 (VRML97), and VRML 1.0, which is recognised only to be
    /// refused with a clear message.
    Vrml,
    /// X3D in its classic (VRML-style) or XML encoding.
    X3d,
    /// LAS lidar points, or LAZ, their LASzip-compressed form.
    Las,
    E57,
    Pcd,
    Pts,
    Ptx,
    /// A Blockbench project (`.bbmodel`).
    Blockbench,
}

impl ModelFormat {
    /// A short human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            ModelFormat::Obj => "Wavefront OBJ",
            ModelFormat::Stl => "STL",
            ModelFormat::Ply => "PLY",
            ModelFormat::Fbx => "FBX",
            ModelFormat::Collada => "COLLADA",
            ModelFormat::MagicaVoxel => "MagicaVoxel",
            ModelFormat::Off => "OFF",
            ModelFormat::ThreeMf => "3MF",
            ModelFormat::Max3ds => "3D Studio",
            ModelFormat::DirectX => "DirectX",
            ModelFormat::Vrml => "VRML",
            ModelFormat::X3d => "X3D",
            ModelFormat::Las => "LAS",
            ModelFormat::E57 => "E57",
            ModelFormat::Pcd => "PCD",
            ModelFormat::Pts => "PTS",
            ModelFormat::Ptx => "PTX",
            ModelFormat::Blockbench => "Blockbench",
        }
    }
}

/// Recognises a format from the extension and first bytes.  PLY files
/// whose header has Gaussian-splat properties (f_dc_0, opacity, scale_0,
/// rot_0) are not claimed: they're splats (a later phase).
///
/// `head` should hold the first 4 KB of the file or all of it, so a PLY
/// header with many splat properties is seen whole.
pub fn sniff(path: &Path, head: &[u8]) -> Option<ModelFormat> {
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    // 1. Magic numbers win over extensions, since thumbnailers see renamed
    //    and extensionless downloads too.
    if head.starts_with(b"VOX ") {
        return Some(ModelFormat::MagicaVoxel);
    }
    if head.starts_with(fbx::BINARY_MAGIC) {
        return Some(ModelFormat::Fbx);
    }
    if head.starts_with(b"ply\n") || head.starts_with(b"ply\r\n") {
        return (!ply::is_splat_header(head)).then_some(ModelFormat::Ply);
    }
    if common::find(&common::utf8_text(head), b"<COLLADA").is_some() {
        return Some(ModelFormat::Collada);
    }
    if directx::looks_like_x(head) {
        return Some(ModelFormat::DirectX);
    }
    if max3ds::looks_like_3ds(head, ext == "3ds") {
        return Some(ModelFormat::Max3ds);
    }
    if threemf::looks_like_3mf(head, ext == "3mf") {
        return Some(ModelFormat::ThreeMf);
    }
    // VRML and X3D by their header lines, also inside gzip.
    let inflated = archive::is_gzip(head).then(|| archive::gunzip_head(head, 1024)).flatten();
    if let Some(d) = vrml::dialect(inflated.as_deref().unwrap_or(head)) {
        return Some(match d {
            vrml::Dialect::Vrml2 | vrml::Dialect::Vrml1 => ModelFormat::Vrml,
            vrml::Dialect::X3dClassic | vrml::Dialect::X3dXml => ModelFormat::X3d,
        });
    }
    if off::looks_like_off(head) {
        return Some(ModelFormat::Off);
    }
    if las::looks_like_las(head) {
        return Some(ModelFormat::Las);
    }
    if e57_scan::looks_like_e57(head) {
        return Some(ModelFormat::E57);
    }
    if pcd::looks_like_pcd(head) {
        return Some(ModelFormat::Pcd);
    }
    if bbmodel::looks_like_bbmodel(head, ext == "bbmodel") {
        return Some(ModelFormat::Blockbench);
    }
    // 2. Text formats whose content can't be told apart reliably go by
    //    extension, checked for plausibility so that, say, a Windows COFF
    //    object file named `.obj` isn't taken for a Wavefront OBJ.
    match ext.as_str() {
        "fbx" => fbx::looks_like_ascii(head).then_some(ModelFormat::Fbx),
        "obj" => obj::looks_like_obj(head).then_some(ModelFormat::Obj),
        "stl" => Some(ModelFormat::Stl),
        "pts" => pts::looks_like_pts(head).then_some(ModelFormat::Pts),
        "ptx" => pts::looks_like_ptx(head).then_some(ModelFormat::Ptx),
        "dae" => (common::looks_like_text(head) || common::is_utf16(head)).then_some(ModelFormat::Collada),
        _ if head.starts_with(b"solid ") && common::find(head, b"facet").is_some() => Some(ModelFormat::Stl),
        _ => None,
    }
}

/// The picture a file's authoring program saved of it, read without the
/// model: a 3MF package's thumbnail (or a slicer project's plate picture),
/// an FBX file's header thumbnail (3ds Max, Maya, MotionBuilder).  `None`
/// for other formats and for files without one.  Slerp's core uses it for
/// thumbnails in place of a render; see `slerp_core::saved_picture`.
pub fn saved_picture(
    format: ModelFormat,
    path: &Path,
    limits: &Limits,
) -> Result<Option<slerp_scene::Texture>, LoadError> {
    let found = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match format {
        ModelFormat::ThreeMf => threemf::saved_picture(path, limits),
        ModelFormat::Fbx => fbx_thumbnail::read(path),
        _ => Ok(None),
    }));
    found.unwrap_or_else(|_| Err(LoadError::invalid(format!("{} picture reader failed unexpectedly", format.name()))))
}

/// Loads a file already read into `bytes`.  `path` names it, so neighbouring
/// files (`.mtl`, textures) can be found when `limits.neighbours` is set.
pub fn load(format: ModelFormat, path: &Path, bytes: &[u8], limits: &Limits) -> Result<Asset, LoadError> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{} bytes", bytes.len())));
    }
    // The loaders are written not to panic; this is a last line of defence
    // so a bug in one of them, or in the texture decoders they call, costs
    // one thumbnail rather than the helper process.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match format {
        // Blockbench's image projects are a picture, not a model.
        ModelFormat::Blockbench => {
            let (loaded, neighbours) = slerp_scene::collect_neighbours(|| bbmodel::load(path, bytes, limits));
            match loaded? {
                bbmodel::Loaded::Scene(scene) => finish(*scene, neighbours, path, limits).map(Asset::Scene),
                bbmodel::Loaded::Picture(texture) => Ok(Asset::Image(std::sync::Arc::new(texture))),
            }
        }
        _ => load_scene(format, path, bytes, limits).map(Asset::Scene),
    }));
    match result {
        Ok(asset) => asset,
        Err(_) => Err(LoadError::invalid(format!("{} loader failed unexpectedly", format.name()))),
    }
}

fn load_scene(
    format: ModelFormat,
    path: &Path,
    bytes: &[u8],
    limits: &Limits,
) -> Result<slerp_scene::Scene, LoadError> {
    // The neighbouring files read through `common::read_limited`.
    let (scene, neighbours) = slerp_scene::collect_neighbours(|| match format {
        ModelFormat::Obj => obj::load(path, bytes, limits),
        ModelFormat::Stl => stl::load(path, bytes, limits),
        ModelFormat::Ply => ply::load(path, bytes, limits),
        ModelFormat::Fbx => fbx::load(path, bytes, limits),
        ModelFormat::Collada => collada::load(path, bytes, limits),
        ModelFormat::MagicaVoxel => vox::load(path, bytes, limits),
        ModelFormat::Off => off::load(path, bytes, limits),
        ModelFormat::ThreeMf => threemf::load(path, bytes, limits),
        ModelFormat::Max3ds => max3ds::load(path, bytes, limits),
        ModelFormat::DirectX => directx::load(path, bytes, limits),
        ModelFormat::Vrml | ModelFormat::X3d => vrml::load(path, bytes, limits),
        ModelFormat::Las => las::load(path, bytes, limits),
        ModelFormat::E57 => e57_scan::load(path, bytes, limits),
        ModelFormat::Pcd => pcd::load(path, bytes, limits),
        ModelFormat::Pts => pts::load_pts(path, bytes, limits),
        ModelFormat::Ptx => pts::load_ptx(path, bytes, limits),
        ModelFormat::Blockbench => Err(LoadError::NotMine),
    });
    finish(scene?, neighbours, path, limits)
}

/// What every model loader's scene gets: the neighbouring files read, a
/// name, and the triangle limit checked.
fn finish(
    mut scene: slerp_scene::Scene,
    neighbours: Vec<std::path::PathBuf>,
    path: &Path,
    limits: &Limits,
) -> Result<slerp_scene::Scene, LoadError> {
    scene.info.neighbours = neighbours;
    if scene.name.is_empty() {
        scene.name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    }
    if scene.triangle_count() as u64 > limits.max_triangles {
        return Err(LoadError::TooLarge(format!("{} triangles", scene.triangle_count())));
    }
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_by_magic_and_extension() {
        let p = |s: &str| Path::new(s).to_path_buf();
        assert_eq!(sniff(&p("a.bin"), b"VOX \x96\0\0\0"), Some(ModelFormat::MagicaVoxel));
        assert_eq!(sniff(&p("a"), b"Kaydara FBX Binary  \0\x1a\0"), Some(ModelFormat::Fbx));
        assert_eq!(sniff(&p("a.fbx"), b"; FBX 7.4.0 project file\n"), Some(ModelFormat::Fbx));
        assert_eq!(
            sniff(&p("a.ply"), b"ply\nformat ascii 1.0\nelement vertex 3\nproperty float x\nend_header\n"),
            Some(ModelFormat::Ply)
        );
        let splat = b"ply\nformat binary_little_endian 1.0\nelement vertex 3\nproperty float x\nproperty float f_dc_0\nproperty float opacity\nend_header\n";
        assert_eq!(sniff(&p("a.ply"), splat), None);
        assert_eq!(sniff(&p("a.xml"), b"<?xml version=\"1.0\"?>\n<COLLADA xmlns="), Some(ModelFormat::Collada));
        assert_eq!(sniff(&p("a.obj"), b"# Blender\nmtllib a.mtl\nv 1 2 3\n"), Some(ModelFormat::Obj));
        assert_eq!(sniff(&p("a.obj"), b"\x64\x86\x05\x00\x00\x00\x00\x00"), None);
        assert_eq!(sniff(&p("a.STL"), b"\0\0\0\0"), Some(ModelFormat::Stl));
        assert_eq!(sniff(&p("noext"), b"solid cube\n  facet normal 0 0 1\n"), Some(ModelFormat::Stl));
        assert_eq!(sniff(&p("a.txt"), b"hello"), None);
        assert_eq!(sniff(&p("a"), b"xof 0303txt 0032\nFrame"), Some(ModelFormat::DirectX));
        assert_eq!(sniff(&p("a.off"), b"OFF\n8 6 0\n"), Some(ModelFormat::Off));
        assert_eq!(sniff(&p("a"), b"COFF\n8 6 0\n"), Some(ModelFormat::Off));
        assert_eq!(sniff(&p("a.off"), b"office notes\n"), None);
        assert_eq!(sniff(&p("a.wrl"), b"#VRML V2.0 utf8\n"), Some(ModelFormat::Vrml));
        assert_eq!(sniff(&p("a.wrl"), b"#VRML V1.0 ascii\n"), Some(ModelFormat::Vrml));
        assert_eq!(sniff(&p("a.x3dv"), b"#X3D V3.3 utf8\n"), Some(ModelFormat::X3d));
        assert_eq!(sniff(&p("a.xml"), b"<?xml version=\"1.0\"?>\n<X3D profile=\"Full\">"), Some(ModelFormat::X3d));
        // 3DS: by extension, or with the version chunk first; a big-endian
        // TIFF ("MM") isn't one.
        let main = [0x4d, 0x4d, 0x20, 0, 0, 0, 0x02, 0, 0x0a, 0, 0, 0, 3, 0, 0, 0];
        assert_eq!(sniff(&p("noext"), &main), Some(ModelFormat::Max3ds));
        assert_eq!(sniff(&p("a.tif"), b"MM\0*\0\0\0\x08\0\0\0\0\0\0\0\0"), None);
        assert_eq!(sniff(&p("a.3mf"), b"PK\x03\x04"), Some(ModelFormat::ThreeMf));
        assert_eq!(sniff(&p("a.zip"), b"PK\x03\x04"), None);
        assert_eq!(sniff(&p("a.laz"), b"LASF\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\x01\x04"), Some(ModelFormat::Las));
        assert_eq!(sniff(&p("a"), b"ASTM-E57"), Some(ModelFormat::E57));
        assert_eq!(sniff(&p("a"), b"# .PCD v0.7\nVERSION 0.7\nFIELDS x y z\n"), Some(ModelFormat::Pcd));
        assert_eq!(sniff(&p("a.pts"), b"2\n0 0 0 1\n1 1 1 2\n"), Some(ModelFormat::Pts));
        assert_eq!(sniff(&p("a.pts"), b"MPEG transport"), None);
        assert_eq!(sniff(&p("a.ptx"), b"10\n20\n0 0 0\n"), Some(ModelFormat::Ptx));
        let bb = b"{\"meta\":{\"format_version\":\"4.10\",\"model_format\":\"free\",\"box_uv\":false}";
        assert_eq!(sniff(&p("noext"), bb), Some(ModelFormat::Blockbench));
        assert_eq!(sniff(&p("a.bbmodel"), b"{\"name\":\"x\"}"), Some(ModelFormat::Blockbench));
        assert_eq!(sniff(&p("a.json"), b"{\"meta\":{}}"), None);
    }

    #[test]
    fn refuses_oversized_files() {
        let limits = Limits { max_file_bytes: 4, ..Limits::thumbnail() };
        let r = load(ModelFormat::Obj, Path::new("a.obj"), b"v 0 0 0\n", &limits);
        assert!(matches!(r, Err(LoadError::TooLarge(_))));
    }
}
