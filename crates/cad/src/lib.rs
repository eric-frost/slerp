//! CAD files: AutoCAD DXF drawings and Rhino `.3dm` models.
//!
//! # DXF
//!
//! Text and binary DXF of every version (R10 to the current one), read by an
//! own tolerant reader (`dxf`): lines, circles, arcs, ellipses, splines,
//! polylines with bulges and widths, hatches, text and multi-line text in a
//! built-in stroke font, block references (nested, in arrays, with
//! attributes), dimensions, leaders, 3D faces and meshes.  Everything is
//! flattened into a [`Drawing`]; a flat one is drawn as a drawing on paper
//! (see [`raster`]), a solid one as a 3D scene.
//!
//! # Rhino
//!
//! The viewer shows the render meshes of a `.3dm` file's objects, or the
//! preview picture Rhino stores in every file when it has none; thumbnails
//! show the preview picture (see [`saved_picture`]).

pub mod drawing;
mod dxf;
pub mod raster;
mod rhino;
mod scene3d;
mod view2d;

pub use drawing::{Bounds, Drawing, FOREGROUND};
pub use raster::{Paint, View};
pub use view2d::{FlatDrawing, picture_size};

/// What the Rhino reader finds in a file's bytes, in a line, for debugging.
#[doc(hidden)]
pub fn rhino_summary(data: &[u8], limits: &Limits) -> String {
    rhino::summary(data, limits)
}

use slerp_scene::textures::Texture;
use slerp_scene::{Asset, Image8, Limits, LoadError, Scene};
use std::path::Path;
use std::sync::Arc;

/// The formats this crate reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CadFormat {
    /// AutoCAD DXF, text or binary.
    Dxf,
    /// A Rhino model.
    Rhino,
}

impl CadFormat {
    pub fn name(self) -> &'static str {
        match self {
            CadFormat::Dxf => "DXF",
            CadFormat::Rhino => "Rhino 3dm",
        }
    }

    /// Whether files of this format usually need the GPU for a thumbnail.
    /// Most DXF files are flat drawings, which the CPU draws.
    pub fn usually_3d(self) -> bool {
        match self {
            CadFormat::Dxf => false,
            // Most hold meshes to render; the helper handles the rest.
            CadFormat::Rhino => true,
        }
    }
}

/// Recognises a CAD file from the first kilobytes of its content.
pub fn sniff(_path: &Path, head: &[u8]) -> Option<CadFormat> {
    if rhino::sniff(head) {
        return Some(CadFormat::Rhino);
    }
    if dxf::sniff(head) {
        return Some(CadFormat::Dxf);
    }
    None
}

/// How many points a drawing may hold: a thumbnail's limit follows the
/// triangle limit loaders share, and the viewer gets room for big plans.
fn point_budget(limits: &Limits) -> usize {
    limits.max_triangles.min(40_000_000) as usize
}

/// A file opened for the viewer.
pub enum Opened {
    /// A flat drawing, which the viewer draws itself at any zoom.
    Flat(Arc<FlatDrawing>),
    /// Anything with depth, as a scene.
    Model(Box<Scene>),
    /// A picture a file stores of itself, for files without anything to
    /// draw (a Rhino file saved without render meshes).
    Picture(Arc<Texture>),
}

fn read_whole(path: &Path, limits: &Limits) -> Result<Vec<u8>, LoadError> {
    let size = std::fs::metadata(path)?.len();
    if size > limits.max_file_bytes.min(dxf::MAX_BYTES) {
        return Err(LoadError::TooLarge(format!("{size} bytes")));
    }
    Ok(std::fs::read(path)?)
}

fn read_drawing(path: &Path, limits: &Limits) -> Result<Drawing, LoadError> {
    let data = read_whole(path, limits)?;
    dxf::load(&data, point_budget(limits))
}

/// Opens `path` for the viewer.
pub fn open(format: CadFormat, path: &Path, limits: &Limits) -> Result<Opened, LoadError> {
    match format {
        CadFormat::Dxf => {
            let d = read_drawing(path, limits)?;
            if d.is_empty() {
                return Err(LoadError::invalid("nothing in model space to draw"));
            }
            if d.flat {
                Ok(Opened::Flat(Arc::new(FlatDrawing::new(d))))
            } else {
                Ok(Opened::Model(Box::new(scene3d::to_scene(&d, limits))))
            }
        }
        CadFormat::Rhino => Ok(match rhino::load_path(path, limits)? {
            rhino::Content::Model(scene) => Opened::Model(scene),
            rhino::Content::Preview(texture) => Opened::Picture(Arc::new(texture)),
        }),
    }
}

/// Loads `path` as an asset: a layered picture for flat drawings, a scene
/// otherwise.
pub fn load_path(format: CadFormat, path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    Ok(match open(format, path, limits)? {
        Opened::Flat(d) => Asset::Canvas(d),
        Opened::Model(s) => Asset::Scene(*s),
        Opened::Picture(t) => Asset::Image(t),
    })
}

/// The picture the file's program saved of it, read without the drawing or
/// model: AutoCAD's `THUMBNAILIMAGE` in a DXF file, the preview every Rhino
/// file keeps in its properties table.  `None` when there is none.  Slerp's
/// core uses it for thumbnails in place of drawing; see
/// `slerp_core::saved_picture`.
pub fn saved_picture(format: CadFormat, path: &Path, limits: &Limits) -> Result<Option<Texture>, LoadError> {
    match format {
        CadFormat::Dxf => dxf::thumbnail::read(path, limits.max_texture_size),
        CadFormat::Rhino => rhino::preview_path(path),
    }
}

/// What a thumbnail is made of.
pub enum Thumb {
    /// A picture stored in the file.
    Picture(Arc<Texture>),
    /// A finished picture.
    Drawn(Image8),
    /// A scene for the GPU.
    Scene(Box<Scene>),
}

/// The thumbnail of `path`, about `size` pixels across.
pub fn thumbnail(format: CadFormat, path: &Path, size: u32, limits: &Limits) -> Result<Thumb, LoadError> {
    match format {
        CadFormat::Dxf => {
            let d = read_drawing(path, limits)?;
            if d.is_empty() {
                return Err(LoadError::invalid("nothing in model space to draw"));
            }
            if d.flat {
                Ok(Thumb::Drawn(view2d::thumbnail(&d, size)))
            } else {
                Ok(Thumb::Scene(Box::new(scene3d::to_scene(&d, limits))))
            }
        }
        CadFormat::Rhino => Ok(match rhino::load_path(path, limits)? {
            rhino::Content::Model(scene) => Thumb::Scene(scene),
            rhino::Content::Preview(texture) => Thumb::Picture(Arc::new(texture)),
        }),
    }
}
