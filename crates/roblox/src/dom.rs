//! The instances of a Roblox file and the few properties Slerp draws them
//! with, as both encodings read them.
//!
//! A Roblox file is a tree of instances, each with a class name and a bag of
//! properties.  A place holds hundreds of classes and thousands of property
//! kinds (scripts, sounds, user interface, physics settings); Slerp keeps the
//! class and name of every instance, for the inspector's counts and the scene
//! hierarchy, and only the properties that decide what a part looks like and
//! where it is.  Everything else is skipped while reading, which keeps a large
//! place's memory to a fraction of a general-purpose reader's.

use glam::{Mat3, Vec3};

/// The instances read from one file.
#[derive(Default)]
pub(crate) struct Dom {
    /// Class names, indexed by `Instance::class`.
    pub classes: Vec<String>,
    pub instances: Vec<Instance>,
    /// Instances without a parent, in file order.
    pub roots: Vec<usize>,
    /// The file's shared strings (the binary `SSTR` chunk, or XML's
    /// `SharedStrings`), which `Blob::Shared` indexes.
    pub shared: Vec<Vec<u8>>,
    /// Problems the reader worked around.
    pub warnings: Vec<String>,
    /// The order instances join their parents, when it isn't their order
    /// in `instances`.
    pub order: Vec<usize>,
}

pub(crate) struct Instance {
    /// Index into `Dom::classes`.
    pub class: u32,
    pub name: String,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub props: Props,
}

impl Dom {
    pub fn class(&self, instance: usize) -> &str {
        self.instances.get(instance).and_then(|i| self.classes.get(i.class as usize)).map_or("", |s| s.as_str())
    }

    /// Interns a class name.
    pub fn class_index(&mut self, name: &str, lookup: &mut std::collections::HashMap<String, u32>) -> u32 {
        if let Some(&i) = lookup.get(name) {
            return i;
        }
        let i = self.classes.len() as u32;
        self.classes.push(name.to_owned());
        lookup.insert(name.to_owned(), i);
        i
    }

    /// The bytes of a blob property.
    pub fn blob<'a>(&'a self, blob: &'a Blob) -> &'a [u8] {
        match blob {
            Blob::Own(bytes) => bytes,
            Blob::Shared(i) => self.shared.get(*i as usize).map_or(&[][..], |v| v.as_slice()),
        }
    }

    /// Fills in every instance's children from its parent, in instance
    /// order, after a reader has set the parents.  Parents that would make a
    /// loop are cut, so the tree below the roots is finite and every instance
    /// is reached at most once.
    pub fn link(&mut self) {
        // 1. A parent chain that comes back to where it started is a loop;
        //    the instance that closes it becomes a root.  Each walk stops at
        //    an instance already proven to reach a root, so the whole pass
        //    is linear.
        let n = self.instances.len();
        let mut state = vec![0u8; n]; // 0 unknown, 1 on the current walk, 2 reaches a root
        let mut walk = Vec::new();
        for start in 0..n {
            let mut at = start;
            loop {
                match state[at] {
                    2 => break,
                    1 => {
                        // A loop: cut it here.
                        self.instances[at].parent = None;
                        self.warnings.push("an instance is its own ancestor; the loop was cut".into());
                        break;
                    }
                    _ => {}
                }
                state[at] = 1;
                walk.push(at);
                match self.instances[at].parent {
                    Some(p) if p < n => at = p,
                    _ => {
                        self.instances[at].parent = None;
                        break;
                    }
                }
            }
            for &i in &walk {
                state[i] = 2;
            }
            walk.clear();
        }
        // 2. Children in the order the reader gives (a binary file's parent
        //    chunk), else in file order; roots likewise.
        for i in 0..n {
            self.instances[i].children.clear();
        }
        let mut seen = vec![false; n];
        let mut order: Vec<usize> = std::mem::take(&mut self.order)
            .into_iter()
            .filter(|&i| i < n && !std::mem::replace(&mut seen[i], true))
            .collect();
        order.extend((0..n).filter(|&i| !seen[i]));
        let mut roots = Vec::new();
        for i in order {
            match self.instances[i].parent {
                Some(p) => self.instances[p].children.push(i),
                None => roots.push(i),
            }
        }
        if self.roots.is_empty() {
            self.roots = roots;
        } else {
            // Keep the reader's root order, adding roots made by cut loops.
            let listed: std::collections::HashSet<usize> = self.roots.iter().copied().collect();
            self.roots.retain(|&r| self.instances.get(r).is_some_and(|i| i.parent.is_none()));
            self.roots.extend(roots.into_iter().filter(|r| !listed.contains(r)));
        }
    }
}

/// A position and rotation in studs, as Roblox's `CFrame` holds them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CFrame {
    pub position: Vec3,
    /// Columns are the part's right, up and back axes (`R00 R10 R20` is the
    /// first).
    pub rotation: Mat3,
}

impl CFrame {
    pub const IDENTITY: CFrame = CFrame { position: Vec3::ZERO, rotation: Mat3::IDENTITY };

    /// From the twelve numbers Roblox writes: position, then the rotation
    /// row by row.
    pub fn from_components(c: [f32; 12]) -> CFrame {
        CFrame {
            position: Vec3::new(c[0], c[1], c[2]),
            rotation: Mat3::from_cols(
                Vec3::new(c[3], c[6], c[9]),
                Vec3::new(c[4], c[7], c[10]),
                Vec3::new(c[5], c[8], c[11]),
            ),
        }
    }

    pub fn is_finite(&self) -> bool {
        self.position.is_finite() && self.rotation.is_finite()
    }
}

/// Bytes a property holds: its own, or one of the file's shared strings.
#[derive(Clone, Debug)]
pub(crate) enum Blob {
    Own(Vec<u8>),
    Shared(u32),
}

/// The properties Slerp reads.  Every other property is skipped.
#[derive(Default)]
pub(crate) struct Props {
    pub cframe: Option<CFrame>,
    pub size: Option<Vec3>,
    pub color: Option<[u8; 3]>,
    pub brick_color: Option<u32>,
    pub transparency: Option<f32>,
    pub reflectance: Option<f32>,
    pub material: Option<u32>,
    /// `Part.Shape`: Ball, Block, Cylinder, Wedge, CornerWedge.
    pub shape: Option<u32>,
    /// Properties only some classes have, kept apart so the common
    /// instances stay small.
    pub extra: Option<Box<Extra>>,
}

impl Props {
    pub fn extra(&self) -> &Extra {
        static NONE: std::sync::OnceLock<Extra> = std::sync::OnceLock::new();
        self.extra.as_deref().unwrap_or_else(|| NONE.get_or_init(Extra::default))
    }

    fn extra_mut(&mut self) -> &mut Extra {
        self.extra.get_or_insert_with(Default::default)
    }
}

#[derive(Default)]
pub(crate) struct Extra {
    /// `SpecialMesh.MeshType`.
    pub mesh_type: Option<u32>,
    /// A mesh's `Scale`, `Offset` and `VertexColor`.
    pub scale: Option<Vec3>,
    pub offset: Option<Vec3>,
    pub vertex_color: Option<Vec3>,
    /// `MeshPart.MeshId`/`MeshContent`, `SpecialMesh.MeshId`.
    pub mesh_id: Option<String>,
    /// A decal's or mesh's texture.
    pub texture: Option<String>,
    /// A union's asset on Roblox's servers.
    pub asset_id: Option<String>,
    pub initial_size: Option<Vec3>,
    /// A union's triangles, when the file keeps them.
    pub mesh_data: Option<Blob>,
    /// The parts a union was made of, as a binary model.
    pub child_data: Option<Blob>,
    /// Convex hulls for collisions (`PhysicalConfigData`).
    pub physics: Option<Blob>,
    /// The older property for the same (`PhysicsData`).
    pub physics_old: Option<Blob>,
    pub use_part_color: Option<bool>,
    pub smooth_grid: Option<Blob>,
    pub material_colors: Option<Blob>,
    pub water_color: Option<Vec3>,
    /// `TrussPart.Style`.
    pub style: Option<u32>,
    /// A decal's face.
    pub face: Option<u32>,
}

/// The properties Slerp reads, by the names either encoding writes them
/// under (serialized names such as `size` and `Color3uint8`, the canonical
/// names, and old spellings).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    CFrame,
    Size,
    Color,
    BrickColor,
    Transparency,
    Reflectance,
    Material,
    Shape,
    MeshType,
    Scale,
    Offset,
    VertexColor,
    MeshId,
    Texture,
    AssetId,
    InitialSize,
    MeshData,
    ChildData,
    Physics,
    PhysicsOld,
    UsePartColor,
    SmoothGrid,
    MaterialColors,
    WaterColor,
    Style,
    Face,
}

impl Key {
    pub fn from_name(name: &str) -> Option<Key> {
        Some(match name {
            "CFrame" | "CoordinateFrame" => Key::CFrame,
            "size" | "Size" => Key::Size,
            "Color3uint8" | "Color" => Key::Color,
            "BrickColor" | "brickColor" => Key::BrickColor,
            "Transparency" | "transparency" => Key::Transparency,
            "Reflectance" | "reflectance" => Key::Reflectance,
            "Material" | "material" => Key::Material,
            "shape" | "Shape" => Key::Shape,
            "MeshType" => Key::MeshType,
            "Scale" => Key::Scale,
            "Offset" => Key::Offset,
            "VertexColor" => Key::VertexColor,
            "MeshId" | "MeshContent" => Key::MeshId,
            "TextureID" | "TextureId" | "TextureContent" | "Texture" => Key::Texture,
            "AssetId" | "Content" => Key::AssetId,
            "InitialSize" => Key::InitialSize,
            "MeshData" | "MeshData2" => Key::MeshData,
            "ChildData" | "ChildData2" => Key::ChildData,
            "PhysicalConfigData" => Key::Physics,
            "PhysicsData" => Key::PhysicsOld,
            "UsePartColor" => Key::UsePartColor,
            "SmoothGrid" => Key::SmoothGrid,
            "MaterialColors" => Key::MaterialColors,
            "WaterColor" => Key::WaterColor,
            "style" | "Style" => Key::Style,
            "Face" => Key::Face,
            _ => return None,
        })
    }
}

/// One property value, as read from either encoding.  Values whose type
/// doesn't fit the key (a `Scale` that is a number on a `UIScale`, say) are
/// ignored by [`Props::set`].
pub(crate) enum Value {
    Bool(bool),
    Int(i64),
    Float(f32),
    Vector3(Vec3),
    CFrame(CFrame),
    Color3(Vec3),
    Color3u8([u8; 3]),
    Text(String),
    Bytes(Vec<u8>),
    Shared(u32),
}

impl Props {
    /// Stores a value under its key when the type fits.  An empty content
    /// string or blob counts as absent.
    pub fn set(&mut self, key: Key, value: Value) {
        use Value as V;
        let float = |v: &Value| match *v {
            V::Float(f) => Some(f),
            V::Int(i) => Some(i as f32),
            _ => None,
        };
        let int = |v: &Value| match *v {
            V::Int(i) => u32::try_from(i).ok(),
            _ => None,
        };
        let vector = |v: &Value| match *v {
            V::Vector3(v) => Some(v),
            _ => None,
        };
        let blob = |v: Value| match v {
            V::Bytes(b) if !b.is_empty() => Some(Blob::Own(b)),
            V::Text(t) if !t.is_empty() => Some(Blob::Own(t.into_bytes())),
            V::Shared(i) => Some(Blob::Shared(i)),
            _ => None,
        };
        let text = |v: Value| match v {
            V::Text(t) if !t.is_empty() => Some(t),
            V::Bytes(b) if !b.is_empty() => Some(String::from_utf8_lossy(&b).into_owned()),
            _ => None,
        };
        match key {
            Key::CFrame => {
                if let V::CFrame(c) = value {
                    self.cframe = Some(c);
                }
            }
            Key::Size => self.size = vector(&value).or(self.size),
            Key::Color => match value {
                V::Color3u8(c) => self.color = Some(c),
                V::Color3(c) => {
                    self.color = Some(c.clamp(Vec3::ZERO, Vec3::ONE).to_array().map(|x| (x * 255.0).round() as u8))
                }
                // Old XML writes a packed number.
                V::Int(packed) => {
                    let p = packed as u32;
                    self.color = Some([(p >> 16) as u8, (p >> 8) as u8, p as u8]);
                }
                _ => {}
            },
            Key::BrickColor => self.brick_color = int(&value).or(self.brick_color),
            Key::Transparency => self.transparency = float(&value).or(self.transparency),
            Key::Reflectance => self.reflectance = float(&value).or(self.reflectance),
            Key::Material => self.material = int(&value).or(self.material),
            Key::Shape => self.shape = int(&value).or(self.shape),
            Key::MeshType => {
                if let Some(v) = int(&value) {
                    self.extra_mut().mesh_type = Some(v);
                }
            }
            Key::Scale => {
                if let Some(v) = vector(&value) {
                    self.extra_mut().scale = Some(v);
                }
            }
            Key::Offset => {
                if let Some(v) = vector(&value) {
                    self.extra_mut().offset = Some(v);
                }
            }
            Key::VertexColor => {
                if let Some(v) = vector(&value) {
                    self.extra_mut().vertex_color = Some(v);
                }
            }
            Key::InitialSize => {
                if let Some(v) = vector(&value) {
                    self.extra_mut().initial_size = Some(v);
                }
            }
            Key::MeshId => {
                if let Some(t) = text(value) {
                    self.extra_mut().mesh_id = Some(t);
                }
            }
            Key::Texture => {
                if let Some(t) = text(value) {
                    self.extra_mut().texture = Some(t);
                }
            }
            Key::AssetId => {
                if let Some(t) = text(value) {
                    self.extra_mut().asset_id = Some(t);
                }
            }
            Key::MeshData => {
                if let Some(b) = blob(value) {
                    self.extra_mut().mesh_data = Some(b);
                }
            }
            Key::ChildData => {
                if let Some(b) = blob(value) {
                    self.extra_mut().child_data = Some(b);
                }
            }
            Key::Physics => {
                if let Some(b) = blob(value) {
                    self.extra_mut().physics = Some(b);
                }
            }
            Key::PhysicsOld => {
                if let Some(b) = blob(value) {
                    self.extra_mut().physics_old = Some(b);
                }
            }
            Key::SmoothGrid => {
                if let Some(b) = blob(value) {
                    self.extra_mut().smooth_grid = Some(b);
                }
            }
            Key::MaterialColors => {
                if let Some(b) = blob(value) {
                    self.extra_mut().material_colors = Some(b);
                }
            }
            Key::UsePartColor => {
                if let V::Bool(b) = value {
                    self.extra_mut().use_part_color = Some(b);
                }
            }
            Key::WaterColor => {
                if let V::Color3(c) = value {
                    self.extra_mut().water_color = Some(c);
                }
            }
            Key::Style => {
                if let Some(v) = int(&value) {
                    self.extra_mut().style = Some(v);
                }
            }
            Key::Face => {
                if let Some(v) = int(&value) {
                    self.extra_mut().face = Some(v);
                }
            }
        }
    }
}
