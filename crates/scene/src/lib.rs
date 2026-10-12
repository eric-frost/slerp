//! The scene model every 3D loader fills in and the renderer draws: nodes,
//! meshes, materials, textures, skins, morph targets and animation clips,
//! plus what a loader returns for files that aren't scenes (pictures,
//! texture packs and materials).
//!
//! It follows glTF 2.0's model, since glTF is the richest format Slerp
//! reads and every other format maps onto a subset of it.  Coordinates are
//! right-handed, Y up, metres where the format says so; loaders convert
//! Z-up formats.

mod anim;
mod bounds;
mod camera;
mod flat;
mod geometry;
mod neighbours;
mod shapes;

pub use anim::*;
pub use bounds::*;
pub use camera::*;
pub use flat::*;
pub use glam;
pub use neighbours::*;
pub use slerp_textures as textures;
pub use slerp_textures::{ColorSpace, Image8, Texture};
pub use shapes::*;

use glam::{Mat4, Quat, Vec2, Vec3, Vec4};
use std::path::PathBuf;
use std::sync::Arc;

/*┌─────────────────╮
| Assets          |
└─────────────────╯*/
//region Assets

/// Whatever a file turned out to hold.
pub enum Asset {
    /// A picture: textures, HDR skies, a material's or Unreal asset's
    /// stored preview, a Doom WAD's title screen.
    Image(Arc<Texture>),
    /// Models, levels, and USD files with meshes.
    Scene(Scene),
    /// A set of named pictures, such as the textures in a Quake WAD or the
    /// maps of a Doom WAD.  Thumbnails show the first few as a grid.
    Gallery(Vec<NamedImage>),
    /// A material file, which the thumbnailer and viewer show on a ball.
    Material(MaterialAsset),
    /// A layered or animated 2D picture that its own crate composes: an
    /// Aseprite sprite or a Tiled map.
    Canvas(Arc<dyn Canvas>),
    /// A colour lookup table, shown applied to a reference photo.
    Lut(Arc<dyn ColorLut>),
}

pub struct NamedImage {
    pub name: String,
    pub texture: Arc<Texture>,
}

/// A material read from a material file (MaterialX, USD, Godot `.tres`).
pub struct MaterialAsset {
    pub name: String,
    /// The material, whose texture slots index `textures`.
    pub material: Material,
    pub textures: Vec<Arc<Texture>>,
    /// A preview picture the file names or ships, which thumbnails show
    /// instead of rendering a ball.
    pub preview: Option<Arc<Texture>>,
    pub info: Info,
}

/// How much a loader may load.  Thumbnails load little and fast; the viewer
/// loads everything.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Downscale textures so their longer side is at most this.
    pub max_texture_size: Option<u32>,
    /// Refuse files larger than this many bytes.
    pub max_file_bytes: u64,
    /// Refuse scenes with more triangles than this.
    pub max_triangles: u64,
    /// Load animation clips.  Thumbnails draw the rest pose and skip them.
    pub animations: bool,
    /// Read files next to the main one (`.bin` buffers, textures, `.mtl`,
    /// skins, WADs).  Off in GNOME's thumbnail sandbox, where they don't
    /// exist anyway; loaders must also cope when they're simply missing.
    pub neighbours: bool,
    /// Keep at most this many Gaussian splats.  Larger captures are thinned
    /// evenly, which a small picture can't tell from the whole.
    pub max_splats: u64,
    /// The block-compressed formats of the GPU that will draw the scene,
    /// which material textures may stay in (see
    /// `slerp_textures::decode_for_gpu`): the viewer's GPU.  None by
    /// default, giving texels anything can read.
    pub gpu_blocks: textures::GpuBlocks,
    /// Bytes of decoded frames an animated picture (GIF, APNG, WebP, AVIF)
    /// may keep in memory.  Beyond that, frames are decoded again when they
    /// are asked for, which costs time on a long animation but no memory.
    pub frame_bytes: u64,
    /// Keep copies of embedded pictures' encoded bytes in memory
    /// (`textures::Original::Memory`) where they aren't a plain byte range
    /// of a file, so they can be saved unchanged or written into a GLB.
    /// Thumbnails never save pictures and leave this off.
    pub originals: bool,
}

impl Limits {
    pub fn thumbnail() -> Self {
        Limits {
            max_texture_size: Some(512),
            max_file_bytes: 1 << 30,
            max_triangles: 20_000_000,
            animations: false,
            neighbours: true,
            max_splats: 1_000_000,
            gpu_blocks: textures::GpuBlocks::default(),
            frame_bytes: 0,
            originals: false,
        }
    }

    pub fn viewer() -> Self {
        Limits {
            max_texture_size: None,
            max_file_bytes: u64::MAX,
            max_triangles: u64::MAX,
            animations: true,
            neighbours: true,
            max_splats: u64::MAX,
            gpu_blocks: textures::GpuBlocks::default(),
            frame_bytes: 512 << 20,
            originals: true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("not a file this loader reads")]
    NotMine,
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("invalid file: {0}")]
    Invalid(String),
    #[error("too large: {0}")]
    TooLarge(String),
    #[error(transparent)]
    Texture(#[from] slerp_textures::TextureError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl LoadError {
    pub fn invalid(msg: impl Into<String>) -> Self {
        LoadError::Invalid(msg.into())
    }
}

//endregion

/*┌─────────────────╮
| Scene           |
└─────────────────╯*/
//region Scene

/// What kind of thing a scene is, which picks the default camera.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SceneKind {
    /// Something to orbit around.
    #[default]
    Object,
    /// A place to walk through: opens in the first-person camera.
    Level,
    /// A generated ball wearing a material.
    MaterialBall,
}

#[derive(Clone, Default)]
pub struct Scene {
    pub name: String,
    pub nodes: Vec<Node>,
    /// Nodes without a parent, drawn in order.
    pub roots: Vec<usize>,
    pub meshes: Vec<Mesh>,
    pub materials: Vec<Material>,
    pub textures: Vec<Arc<Texture>>,
    pub skins: Vec<Skin>,
    pub animations: Vec<Animation>,
    pub kind: SceneKind,
    /// Where a level's player starts.
    pub spawn: Option<Spawn>,
    /// Points of interest drawn as an overlay: Quake entities, lights.
    pub markers: Vec<Marker>,
    /// For scenes that carry no light of their own and are lit by Slerp's
    /// (level-editor maps, mostly closed rooms that the key light never
    /// reaches): how much of its own colour each lit surface shows where
    /// no light falls, as a fraction.  0 for scenes with authored light,
    /// which is nearly all of them.  The viewer shows a slider for scenes
    /// where this is above 0.
    pub fill: f32,
    /// Gaussian splats, drawn by their own pipeline.
    pub splats: Option<Splats>,
    /// Named facial expressions and their presets, from VRM.
    pub expressions: Vec<Expression>,
    /// Which node plays which humanoid bone, when the file says (VRM) or a
    /// loader recognises the skeleton.  Used to retarget animations.
    pub humanoid: Option<Humanoid>,
    /// The named groups the authoring program organised the nodes in
    /// (Blender's collections), for listing; empty when the file has none.
    /// They don't affect drawing: the hierarchy is `Node::children`.
    pub collections: Vec<Collection>,
    pub info: Info,
}

/// A named group of nodes, such as a Blender collection.  Unlike the node
/// hierarchy, groups only organise: a node may be in several, and a group
/// may sit inside several others.  Groups that are no other's child are the
/// top level.  Indices may be out of range in a damaged file, and a
/// careless loader could make a cycle, so readers check both.
#[derive(Clone, Debug, Default)]
pub struct Collection {
    pub name: String,
    /// The nodes in it directly, not through the groups inside it (indices
    /// into `Scene::nodes`).
    pub nodes: Vec<usize>,
    /// The groups inside it (indices into `Scene::collections`).
    pub children: Vec<usize>,
}

/// Facts for the viewer's inspector, and problems the loader worked around.
#[derive(Clone, Debug, Default)]
pub struct Info {
    /// The format, such as "glTF 2.0 (binary)" or "Quake BSP2".
    pub format: String,
    pub generator: Option<String>,
    /// Things that didn't load, such as a missing texture file.
    pub warnings: Vec<String>,
    /// Extra facts as name and value, such as glTF extensions used.
    pub details: Vec<(String, String)>,
    /// Files other than the main one that the loader read or looked for:
    /// a glTF's `.bin` buffers and images, an OBJ's `.mtl` and textures, a
    /// map's WADs and `.lit` file, a material's textures, a Tiled map's
    /// tilesets.  The viewer watches them, so that it can reload when one
    /// changes on disk.
    pub neighbours: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug)]
pub struct Spawn {
    /// Eye position.
    pub position: Vec3,
    /// Radians around +Y; 0 looks down -Z.
    pub yaw: f32,
}

#[derive(Clone, Debug)]
pub struct Marker {
    /// Such as "info_player_start" or "light".
    pub label: String,
    pub position: Vec3,
    pub color: Vec4,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    pub transform: Transform,
    pub children: Vec<usize>,
    pub mesh: Option<usize>,
    /// A skinned mesh ignores its node's transform and follows the joints.
    pub skin: Option<usize>,
    /// Default morph target weights for the node's mesh.
    pub weights: Vec<f32>,
    pub visible: bool,
}

impl Node {
    pub fn new(name: impl Into<String>) -> Self {
        Node {
            name: name.into(),
            transform: Transform::IDENTITY,
            children: Vec::new(),
            mesh: None,
            skin: None,
            weights: Vec::new(),
            visible: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Transform {
    pub const IDENTITY: Transform = Transform { translation: Vec3::ZERO, rotation: Quat::IDENTITY, scale: Vec3::ONE };

    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }

    /// Splits a matrix into translation, rotation and scale.  Shear is lost.
    pub fn from_matrix(m: Mat4) -> Self {
        let (scale, rotation, translation) = m.to_scale_rotation_translation();
        Transform { translation, rotation, scale }
    }

    /// Interpolates each part, with a shortest-path slerp for rotation.
    pub fn lerp(&self, other: &Transform, t: f32) -> Transform {
        Transform {
            translation: self.translation.lerp(other.translation, t),
            rotation: self.rotation.slerp(other.rotation, t),
            scale: self.scale.lerp(other.scale, t),
        }
    }
}

impl Default for Transform {
    fn default() -> Self {
        Transform::IDENTITY
    }
}

//endregion

/*┌─────────────────╮
| Meshes          |
└─────────────────╯*/
//region Meshes

#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub name: String,
    pub primitives: Vec<Primitive>,
    /// Names of the morph targets, shared by every primitive, when known.
    pub target_names: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Topology {
    #[default]
    Triangles,
    Lines,
    Points,
}

/// One draw: vertex attributes, indices and a material.  Optional
/// attributes are empty vectors when absent; when present they have one
/// entry per position.
#[derive(Clone, Debug, Default)]
pub struct Primitive {
    pub positions: Vec<Vec3>,
    /// Empty means "compute them": call `ensure_normals`.
    pub normals: Vec<Vec3>,
    /// xyz tangent and w handedness.  Empty means the renderer derives them
    /// from screen-space derivatives.
    pub tangents: Vec<Vec4>,
    pub uv0: Vec<Vec2>,
    /// Second UV set: lightmaps, glTF `TEXCOORD_1`.
    pub uv1: Vec<Vec2>,
    /// Linear RGBA vertex colours.
    pub colors: Vec<Vec4>,
    /// Indices into the node's skin's `joints`.
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<Vec4>,
    /// Always present: loaders generate 0..n for unindexed data.
    pub indices: Vec<u32>,
    pub topology: Topology,
    pub targets: Vec<MorphTarget>,
    /// Index into `Scene::materials`; `None` draws a neutral grey.
    pub material: Option<usize>,
}

impl Primitive {
    pub fn triangle_count(&self) -> usize {
        match self.topology {
            Topology::Triangles => self.indices.len() / 3,
            _ => 0,
        }
    }

    pub fn is_skinned(&self) -> bool {
        !self.joints.is_empty() && !self.weights.is_empty()
    }
}

/// Position and normal offsets added to a primitive's vertices, scaled by
/// the target's weight.  Normals are empty when the file has none.
#[derive(Clone, Debug, Default)]
pub struct MorphTarget {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
}

//endregion

/*┌─────────────────╮
| Materials       |
└─────────────────╯*/
//region Materials

/// A metallic-roughness PBR material in glTF's terms.  Texture references
/// index `Scene::textures` (or `MaterialAsset::textures`).
#[derive(Clone, Debug)]
pub struct Material {
    pub name: String,
    /// Linear RGBA, multiplied with the base colour texture.
    pub base_color: Vec4,
    pub base_color_texture: Option<TexRef>,
    pub metallic: f32,
    pub roughness: f32,
    /// Roughness in green, metallic in blue, as glTF stores them.
    pub metallic_roughness_texture: Option<TexRef>,
    pub normal_texture: Option<TexRef>,
    pub normal_scale: f32,
    /// Ambient occlusion in red.
    pub occlusion_texture: Option<TexRef>,
    pub occlusion_strength: f32,
    /// Linear RGB, already multiplied by any emissive strength.
    pub emissive: Vec3,
    pub emissive_texture: Option<TexRef>,
    pub alpha_mode: AlphaMode,
    pub double_sided: bool,
    /// Draw the base colour without lighting.
    pub unlit: bool,
    /// KHR_materials_specular: strength and tint of the dielectric
    /// reflection.
    pub specular: f32,
    pub specular_color: Vec3,
    pub ior: f32,
    pub clearcoat: f32,
    pub clearcoat_roughness: f32,
    /// KHR_materials_transmission: the fraction of light that passes
    /// through the surface instead of being scattered by it, as in glass.
    /// 0 is an ordinary surface.
    pub transmission: f32,
    /// Multiplies `transmission`, in red.
    pub transmission_texture: Option<TexRef>,
    /// KHR_materials_volume: how thick the volume under the surface is, in
    /// the mesh's own units, which bends what shows through it.  0 is a
    /// thin wall, such as a soap bubble or a window pane.
    pub thickness: f32,
    /// Multiplies `thickness`, in green.
    pub thickness_texture: Option<TexRef>,
    /// Linear RGB colour that white light becomes after travelling
    /// `attenuation_distance` through the volume (Beer's law).
    pub attenuation_color: Vec3,
    /// Infinite means the volume doesn't absorb light.
    pub attenuation_distance: f32,
    /// Pre-baked lighting read through the second UV set, as in Quake maps.
    /// When present it replaces the scene lighting for this material.
    pub lightmap_texture: Option<TexRef>,
    /// Lightmap brightness multiplier (Quake's overbright is 2).
    pub lightmap_scale: f32,
    /// A height map in red, for displacing a material ball.
    pub height_texture: Option<TexRef>,
    /// Displacement in metres at height 1.
    pub height_scale: f32,
    pub effect: Effect,
    /// Distance fog over the surface, as Doom's source ports draw sector
    /// fog; `None` for none.
    pub fog: Option<Fog>,
}

/// Doom's way of shading a surface, which Doom maps' materials carry.
/// Doom's source ports multiply a texel by its light in display values,
/// not in linear light; a material with `Fog` is drawn that way: its
/// colour (texture times vertex colour, each decoded with a 2.2 gamma) is
/// taken back to display values, brightened near the viewer, faded into
/// the fog, and shown as those display values, so dark rooms keep the
/// depth they have in the games.
///
/// The fog: the colour fades towards `color` (linear RGB) by
/// `exp(-density * distance)`, with the distance in metres from the
/// camera; density 0 is none.  GZDoom's Doom light mode also brightens
/// surfaces near the viewer: by `near_light` times at the camera, fading
/// to none at `near_distance` metres (0 for none).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fog {
    pub color: Vec3,
    pub density: f32,
    pub near_light: f32,
    pub near_distance: f32,
}

impl Default for Material {
    /// A neutral light-grey dielectric, for formats that don't describe
    /// materials.
    fn default() -> Self {
        Material {
            name: String::new(),
            base_color: Vec4::new(0.8, 0.8, 0.8, 1.0),
            base_color_texture: None,
            metallic: 0.0,
            roughness: 0.5,
            metallic_roughness_texture: None,
            normal_texture: None,
            normal_scale: 1.0,
            occlusion_texture: None,
            occlusion_strength: 1.0,
            emissive: Vec3::ZERO,
            emissive_texture: None,
            alpha_mode: AlphaMode::Opaque,
            double_sided: false,
            unlit: false,
            specular: 1.0,
            specular_color: Vec3::ONE,
            ior: 1.5,
            clearcoat: 0.0,
            clearcoat_roughness: 0.0,
            transmission: 0.0,
            transmission_texture: None,
            thickness: 0.0,
            thickness_texture: None,
            attenuation_color: Vec3::ONE,
            attenuation_distance: f32::INFINITY,
            lightmap_texture: None,
            lightmap_scale: 1.0,
            height_texture: None,
            height_scale: 0.0,
            effect: Effect::None,
            fog: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum AlphaMode {
    #[default]
    Opaque,
    /// Discard texels with alpha below the cutoff.
    Mask(f32),
    Blend,
}

/// Special surface behaviour from game formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Effect {
    #[default]
    None,
    /// Quake's turbulent liquids: UVs wobble over time, no lightmap.
    Warp,
    /// Quake's sky: two scrolling layers, unlit.  The base colour texture
    /// holds both halves side by side, as in the BSP.
    Sky,
    /// Doom's sky: the base colour texture wrapped around the viewer like
    /// a cylinder, unlit and infinitely far away.  Before the base slot's
    /// `UvTransform`, u counts turns anticlockwise seen from above,
    /// starting at +X, and v is the tangent of the elevation (0 at the
    /// horizon, 1 at 45 degrees up).
    PanoramaSky,
    /// Sprites that turn about +Y to face the camera, as Doom's things do.
    /// Each vertex sits at its sprite's anchor (the foot of the thing) and
    /// its second UV set holds the corner's offset from there in metres,
    /// to the right and up as the camera sees it.  Unlit.
    Billboard,
}

impl Effect {
    /// Skies of either kind: far away by intent, so they are left out of
    /// the bounds that frame a level, and cast no shadow.
    pub fn is_sky(self) -> bool {
        matches!(self, Effect::Sky | Effect::PanoramaSky)
    }
}

/// A material's use of a texture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TexRef {
    pub texture: usize,
    /// Which UV set: 0 or 1.
    pub uv_set: u8,
    pub transform: Option<UvTransform>,
    pub sampler: Sampler,
}

impl TexRef {
    pub fn new(texture: usize) -> Self {
        TexRef { texture, uv_set: 0, transform: None, sampler: Sampler::default() }
    }
}

/// KHR_texture_transform: uv' = translate(offset) * rotate(rotation) *
/// scale(scale) * uv.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UvTransform {
    pub offset: Vec2,
    pub rotation: f32,
    pub scale: Vec2,
}

impl UvTransform {
    /// The 3x3 matrix applied to (u, v, 1).
    pub fn matrix(&self) -> glam::Mat3 {
        glam::Mat3::from_translation(self.offset)
            * glam::Mat3::from_angle(-self.rotation)
            * glam::Mat3::from_scale(self.scale)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrap {
    Repeat,
    MirroredRepeat,
    ClampToEdge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    Nearest,
    Linear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sampler {
    pub wrap_u: Wrap,
    pub wrap_v: Wrap,
    pub mag: Filter,
    pub min: Filter,
    pub mipmaps: bool,
}

impl Default for Sampler {
    fn default() -> Self {
        Sampler { wrap_u: Wrap::Repeat, wrap_v: Wrap::Repeat, mag: Filter::Linear, min: Filter::Linear, mipmaps: true }
    }
}

//endregion

/*┌─────────────────╮
| Skins and clips |
└─────────────────╯*/
//region Skins and clips

#[derive(Clone, Debug, Default)]
pub struct Skin {
    pub name: String,
    /// Node indices; a vertex's joint index `j` means `joints[j]`.
    pub joints: Vec<usize>,
    /// One per joint; identity when the file has none.
    pub inverse_bind: Vec<Mat4>,
    pub skeleton: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Animation {
    pub name: String,
    /// Seconds; the end of the last keyframe.
    pub duration: f32,
    pub channels: Vec<Channel>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Property {
    Translation,
    Rotation,
    Scale,
    /// Morph target weights of the node's mesh.
    Weights,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    Step,
    Linear,
    /// glTF's cubic spline: each key stores in-tangent, value, out-tangent.
    CubicSpline,
}

/// Keyframes for one property of one node.  `values` is flat: per key, 3
/// floats for translation and scale, 4 (x, y, z, w) for rotation, or one per
/// morph target for weights; three times that for cubic splines.
#[derive(Clone, Debug)]
pub struct Channel {
    pub node: usize,
    pub property: Property,
    pub interpolation: Interpolation,
    pub times: Vec<f32>,
    pub values: Vec<f32>,
}

/// A named expression that drives morph targets together, such as VRM's
/// "happy" or "blinkLeft".
#[derive(Clone, Debug, Default)]
pub struct Expression {
    pub name: String,
    /// VRM's preset name ("happy", "aa", "blink", ...) when it is one.
    pub preset: Option<String>,
    pub binds: Vec<MorphBind>,
}

#[derive(Clone, Copy, Debug)]
pub struct MorphBind {
    pub node: usize,
    pub target: usize,
    pub weight: f32,
}

/// Humanoid bone assignments, named as in VRM 1.0 (`hips`, `spine`,
/// `chest`, `upperChest`, `neck`, `head`, `leftUpperArm`, `leftLowerArm`,
/// `leftHand`, `leftUpperLeg`, `leftLowerLeg`, `leftFoot`, `leftToes`, and
/// so on).
#[derive(Clone, Debug, Default)]
pub struct Humanoid {
    pub bones: Vec<(String, usize)>,
}

impl Humanoid {
    pub fn node(&self, bone: &str) -> Option<usize> {
        self.bones.iter().find(|(name, _)| name == bone).map(|&(_, node)| node)
    }
}

/// Gaussian splats (3D Gaussian splatting), one entry per splat in each
/// vector.  Their colours are what training learned from photos: in the
/// photos' display encoding (sRGB), drawn and blended as they are rather
/// than lit.
#[derive(Clone, Debug, Default)]
pub struct Splats {
    pub positions: Vec<Vec3>,
    /// Standard deviations along the splat's own axes, in metres (not the
    /// logarithms PLY files store).
    pub scales: Vec<Vec3>,
    pub rotations: Vec<Quat>,
    /// The colour seen from every direction (spherical-harmonic band 0,
    /// already turned into a colour: 0.5 + 0.2821 × DC), display-encoded,
    /// plus opacity from 0 to 1.
    pub colors: Vec<Vec4>,
    /// The highest spherical-harmonic band in `sh`, 0 to 3.  Bands 1 to 3
    /// add colour that changes with the viewing direction: sheen and
    /// reflections.
    pub sh_degree: u8,
    /// Coefficients of bands 1 to `sh_degree`: per splat
    /// `Splats::sh_coefficients(sh_degree)` of them, each as red, green and
    /// blue, stored as IEEE half-precision bits, which halves the memory of
    /// multi-million-splat captures for no visible loss.  Basis functions
    /// and signs are those of the 3D Gaussian splatting reference code,
    /// evaluated in this crate's coordinates (Y up) for the direction from
    /// the camera to the splat.
    pub sh: Vec<u16>,
    /// Trained with Mip-Splatting's antialiasing filter, which changes how
    /// a splat's opacity is drawn.
    pub antialiased: bool,
}

impl Splats {
    /// Spherical-harmonic coefficients per colour channel above band 0 for
    /// `degree`: 0, 3, 8 or 15.
    pub fn sh_coefficients(degree: u8) -> usize {
        let d = degree.min(3) as usize;
        (d + 1) * (d + 1) - 1
    }

    pub fn len(&self) -> usize {
        self.positions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// The box around every splat centre.
    pub fn full_bounds(&self) -> Aabb {
        let mut b = Aabb::EMPTY;
        self.positions.iter().for_each(|&p| b.add(p));
        b
    }

    /// The box around the part of the cloud a camera should frame: the 2nd
    /// to 98th percentile of the centres on each axis, each splat weighted
    /// by its opacity over its squared size.  Captures keep far more than
    /// their subject: sky and distant walls drawn with large splats,
    /// floaters, and whole surroundings of a 360-degree capture, often more
    /// than half of all splats.  The subject is where training put the
    /// finest, most opaque splats, because every photo saw it up close, so
    /// the weighting finds it where plain percentiles would frame the
    /// surroundings and shrink the subject to a dot.  Percentiles come from
    /// at most 65,536 evenly spaced splats, plenty for a box and quick for
    /// millions.
    pub fn dense_bounds(&self) -> Aabb {
        let step = self.positions.len().div_ceil(65_536).max(1);
        let mut sample: Vec<(Vec3, f64)> = Vec::with_capacity(self.positions.len() / step + 1);
        for i in (0..self.positions.len()).step_by(step) {
            let p = self.positions[i];
            let size = self.scales.get(i).map_or(1.0, |s| s.max_element() as f64).max(1e-7);
            let opacity = self.colors.get(i).map_or(1.0, |c| c.w as f64);
            let weight = opacity / (size * size);
            if p.is_finite() && weight.is_finite() && weight > 0.0 {
                sample.push((p, weight));
            }
        }
        if sample.len() < 50 {
            return self.full_bounds();
        }
        let total: f64 = sample.iter().map(|s| s.1).sum();
        let mut min = Vec3::ZERO;
        let mut max = Vec3::ZERO;
        for a in 0..3 {
            sample.sort_unstable_by(|x, y| x.0[a].total_cmp(&y.0[a]));
            let mut sum = 0.0;
            let mut lo = None;
            for &(p, w) in &sample {
                sum += w;
                if lo.is_none() && sum >= total * 0.02 {
                    lo = Some(p[a]);
                }
                max[a] = p[a];
                if sum >= total * 0.98 {
                    break;
                }
            }
            min[a] = lo.unwrap_or(max[a]);
        }
        Aabb { min, max }
    }
}

//endregion

impl Scene {
    pub fn triangle_count(&self) -> usize {
        self.meshes.iter().flat_map(|m| &m.primitives).map(|p| p.triangle_count()).sum()
    }

    /// Triangles as drawn: each mesh counted once for every node that shows
    /// it, so a forest of one instanced tree counts every tree.  Meshes no
    /// node uses count once, as `triangle_count` counts them.
    pub fn drawn_triangle_count(&self) -> u64 {
        let per_mesh: Vec<u64> = self
            .meshes
            .iter()
            .map(|m| m.primitives.iter().map(|p| p.triangle_count() as u64).sum())
            .collect();
        let mut uses = vec![0u64; per_mesh.len()];
        for n in &self.nodes {
            if let Some(u) = n.mesh.and_then(|m| uses.get_mut(m)) {
                *u += 1;
            }
        }
        per_mesh.iter().zip(&uses).map(|(t, &u)| t * u.max(1)).sum()
    }

    pub fn vertex_count(&self) -> usize {
        self.meshes.iter().flat_map(|m| &m.primitives).map(|p| p.positions.len()).sum()
    }

    /// Recomputes `roots` from the nodes' children lists.
    pub fn compute_roots(&mut self) {
        let mut has_parent = vec![false; self.nodes.len()];
        for n in &self.nodes {
            for &c in &n.children {
                has_parent[c] = true;
            }
        }
        self.roots = (0..self.nodes.len()).filter(|&i| !has_parent[i]).collect();
    }

    /// Parent index of every node.
    pub fn parents(&self) -> Vec<Option<usize>> {
        let mut parents = vec![None; self.nodes.len()];
        for (i, n) in self.nodes.iter().enumerate() {
            for &c in &n.children {
                parents[c] = Some(i);
            }
        }
        parents
    }

    /// Adds a node holding `mesh` under the root and returns its index.
    pub fn add_mesh_node(&mut self, mesh: Mesh) -> usize {
        let name = mesh.name.clone();
        self.meshes.push(mesh);
        let mut node = Node::new(name);
        node.mesh = Some(self.meshes.len() - 1);
        self.nodes.push(node);
        let index = self.nodes.len() - 1;
        self.roots.push(index);
        index
    }

    /// Fills in missing normals on every triangle primitive.
    pub fn ensure_normals(&mut self) {
        for mesh in &mut self.meshes {
            for p in &mut mesh.primitives {
                p.ensure_normals();
            }
        }
    }

    /// Rotates the whole scene from Z-up to Y-up by adding a root node.
    pub fn convert_z_up(&mut self) {
        self.wrap_roots(Transform {
            rotation: Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
            ..Transform::IDENTITY
        });
    }

    /// Puts every root under a new node with `transform`.
    pub fn wrap_roots(&mut self, transform: Transform) {
        let mut node = Node::new("root");
        node.transform = transform;
        node.children = std::mem::take(&mut self.roots);
        self.nodes.push(node);
        self.roots = vec![self.nodes.len() - 1];
    }
}
