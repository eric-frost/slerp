//! What both readers produce: a SketchUp model as SketchUp itself keeps it,
//! before anything is triangulated.  Lengths are in inches and Z is up, as
//! in the file; `build` turns it into a scene.
//!
//! References between entities are indices into the lists here, so the
//! scene builder never sees the file's own identifiers (entity ids in 2021
//! and later files, store-map slots in older ones).

use glam::DVec3;

/// Which container the file used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    /// SketchUp 2021 and later: a header and a zip with `model.dat`.
    Vff,
    /// SketchUp 3 to 2020: one MFC object stream.
    Classic,
}

/// A whole model.
#[derive(Debug, Default)]
pub struct Model {
    /// The version in the file's header, such as "25.0.575".
    pub version: String,
    /// The major version: 3 to 8 for the early releases, then 13 for 2013
    /// up to 26 for 2026.
    pub major: u32,
    pub container: Option<Container>,
    /// The loose geometry of the model and its top-level groups and
    /// components.
    pub root: Definition,
    /// Component and group definitions, placed through [`Instance`]s.
    pub definitions: Vec<Definition>,
    pub materials: Vec<Material>,
    /// Tags (called layers before SketchUp 2020).
    pub layers: Vec<Layer>,
    /// The face colours of the style the model was saved with: what faces
    /// without a material show on their front and back.
    pub style: Style,
    /// The view the model was saved with.
    pub camera: Option<Camera>,
    /// Scenes (called pages in the file).
    pub pages: Vec<Page>,
    /// Things the reader skipped or guessed, for the viewer's warnings.
    pub warnings: Vec<String>,
}

/// A list of entities: the model's own, or a component's or group's.
#[derive(Debug, Default)]
pub struct Definition {
    pub name: String,
    /// Positions in inches, in the definition's own coordinates.
    pub vertices: Vec<DVec3>,
    pub edges: Vec<Edge>,
    pub faces: Vec<Face>,
    pub instances: Vec<Instance>,
    /// A definition that holds a placed picture (an Image entity).
    pub image: bool,
    /// "Always face camera": 2D people and trees that turn towards the
    /// viewer in SketchUp.  Drawn as saved here.
    pub faces_camera: bool,
}

/// A line between two vertices.  Faces are bounded by edges, and edges
/// without faces are lines in their own right.
#[derive(Clone, Copy, Debug)]
pub struct Edge {
    /// Indices into [`Definition::vertices`].
    pub a: u32,
    pub b: u32,
    /// Hidden with SketchUp's Hide command.
    pub hidden: bool,
    /// Soft edges are not drawn; they join faces into one smooth-looking
    /// surface.
    pub soft: bool,
    /// Smooth edges blend the shading of the faces on either side.
    pub smooth: bool,
    pub layer: Option<u32>,
}

/// One use of an edge in a face's loop.
#[derive(Clone, Copy, Debug)]
pub struct CoEdge {
    /// Index into [`Definition::edges`].
    pub edge: u32,
    /// The loop runs from the edge's `b` to its `a`.
    pub reversed: bool,
}

/// A planar face: an outer loop and any number of holes.  SketchUp faces
/// have two sides, each with its own material.
#[derive(Clone, Debug)]
pub struct Face {
    /// The outer loop first, then the holes.
    pub loops: Vec<Vec<CoEdge>>,
    /// The front side's normal, as stored.
    pub normal: DVec3,
    /// Indices into [`Model::materials`]; `None` is SketchUp's "default"
    /// material, which takes the material of the group or component the
    /// face is in, or else the style's face colour.
    pub front: Option<u32>,
    pub back: Option<u32>,
    /// How a texture is positioned on each side, when it was moved,
    /// rotated or distorted rather than left as SketchUp places it.
    pub mapping: [Option<Mapping>; 2],
    pub hidden: bool,
    pub layer: Option<u32>,
}

/// A texture position on one side of a face: a 3 by 3 matrix, row by row,
/// that takes texture coordinates (in inches) to the face's plane
/// coordinates.  `build::face_uv` inverts it.
#[derive(Clone, Copy, Debug)]
pub struct Mapping {
    pub matrix: [f64; 9],
    /// For a texture projected onto the face rather than wrapped on it
    /// (terrain from Add Location, pictures projected over curved
    /// surfaces): the direction it is projected along.  Its coordinates
    /// then run in the plane across that direction, not the face's.
    pub projection: Option<DVec3>,
}

/// A placed group or component.
#[derive(Clone, Debug)]
pub struct Instance {
    /// Index into [`Model::definitions`].
    pub definition: u32,
    /// The placement as SketchUp stores it: a 3 by 3 matrix row by row
    /// (applied as `m[0]*x + m[1]*y + m[2]*z`), the translation in inches
    /// and a homogeneous scale that divides the result.
    pub matrix: [f64; 13],
    /// The material faces inside take when they have none of their own.
    pub material: Option<u32>,
    pub layer: Option<u32>,
    pub hidden: bool,
    pub name: String,
}

/// A material: a colour, or a texture with its real-world size.
#[derive(Clone, Debug)]
pub struct Material {
    pub name: String,
    /// sRGB.  For a texture, its average colour (or for a colourised one,
    /// the colour it is tinted towards).
    pub color: [u8; 3],
    /// 1 is opaque.
    pub opacity: f32,
    pub texture: Option<TextureImage>,
    /// The texture is shown tinted towards `color`.
    pub colorized: bool,
}

/// A texture's picture and its size on a face.
#[derive(Clone, Debug)]
pub struct TextureImage {
    /// The image file's bytes as stored (PNG, JPEG, BMP and the like).
    pub bytes: Vec<u8>,
    /// Width and height of one tile of the picture on a face, in inches.
    pub width: f64,
    pub height: f64,
    pub file_name: String,
}

/// A tag (layer).
#[derive(Clone, Debug)]
pub struct Layer {
    pub name: String,
    /// Switched off in the Tags panel: everything on it is hidden.
    pub hidden: bool,
}

/// Face colours from the style, sRGB.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub front: [u8; 3],
    pub back: [u8; 3],
}

impl Default for Style {
    /// SketchUp's default style: white fronts, blue-grey backs.
    fn default() -> Style {
        Style { front: [255, 255, 255], back: [164, 178, 187] }
    }
}

/// A camera, in inches with Z up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub eye: DVec3,
    pub target: DVec3,
    pub up: DVec3,
    /// Vertical field of view in degrees.
    pub fov: f64,
    pub perspective: bool,
    /// The height the view shows with parallel projection, in inches.
    pub height: f64,
}

/// A scene: a named view with its own set of hidden tags.
#[derive(Clone, Debug)]
pub struct Page {
    pub name: String,
    pub camera: Option<Camera>,
    /// Indices into [`Model::layers`].
    pub hidden_layers: Vec<u32>,
}

impl Model {
    /// Every definition by index, the root last (as `definitions.len()`).
    pub fn definition(&self, i: usize) -> Option<&Definition> {
        if i == self.definitions.len() { Some(&self.root) } else { self.definitions.get(i) }
    }

    /// Faces, edges and instances in all definitions, for the inspector.
    pub fn counts(&self) -> (usize, usize, usize) {
        let all = self.definitions.iter().chain(std::iter::once(&self.root));
        all.fold((0, 0, 0), |(f, e, i), d| (f + d.faces.len(), e + d.edges.len(), i + d.instances.len()))
    }
}
