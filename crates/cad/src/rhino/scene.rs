//! A Rhino model's meshes as a scene: one node per layer, with the meshes
//! of the layer merged per material, block instances expanded, the model's
//! units converted to metres and Z up turned into Y up.

use super::chunks::Archive;
use super::mesh::MeshData;
use super::model::{Attributes, Geometry, Layer, Model};
use super::release;
use glam::{DMat3, DMat4, DVec3, Vec3, Vec4};
use slerp_scene::{AlphaMode, Info, Limits, Material, Mesh, Primitive, Scene};
use std::collections::{BTreeMap, HashMap};

fn linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// What Rhino draws an object with when nothing says otherwise.
const DEFAULT_GREY: [u8; 3] = [204, 204, 204];

/// How deep blocks nest and how many instances are expanded, at most.
const MAX_DEPTH: u32 = 8;
const MAX_INSTANCES: usize = 200_000;

/// Primitives by layer and appearance (colour, transparency percentage).
type Key = ([u8; 3], u8);

struct Builder<'a> {
    model: &'a Model,
    scale: f64,
    limit: u64,
    by_index: BTreeMap<i32, usize>,
    by_id: BTreeMap<[u8; 16], usize>,
    objects_by_id: HashMap<[u8; 16], usize>,
    layers: BTreeMap<i32, BTreeMap<Key, Primitive>>,
    triangles: u64,
    instances: usize,
    over_budget: bool,
    hidden_objects: usize,
    missing_blocks: usize,
}

impl Builder<'_> {
    fn layer_of(&self, attrs: &Attributes) -> Option<&Layer> {
        self.by_index.get(&attrs.layer).map(|&i| &self.model.layers[i])
    }

    /// Whether `layer` (and every layer above it) is switched on.
    fn shown(&self, layer: Option<&Layer>) -> bool {
        let Some(mut l) = layer else { return true };
        for _ in 0..64 {
            if !l.visible {
                return false;
            }
            if l.parent == [0u8; 16] {
                return true;
            }
            match self.by_id.get(&l.parent) {
                Some(&i) => l = &self.model.layers[i],
                None => return true,
            }
        }
        true
    }

    /// The colour and transparency of an object: its material's diffuse, else
    /// the colour set on the object, else Rhino's default grey.
    fn appearance(&self, attrs: &Attributes) -> ([u8; 3], f32) {
        let layer = self.layer_of(attrs);
        let material_index = match attrs.material_source {
            1 => attrs.material,
            _ => layer.map_or(-1, |l| l.material),
        };
        if let Some(m) = self.model.materials.get(&material_index) {
            return (m.diffuse, m.transparency);
        }
        if attrs.color_source == 1 {
            return (attrs.color, 0.0);
        }
        (DEFAULT_GREY, 0.0)
    }

    /// Draws an object (and, for a block, what is in it) in `layer`, moved
    /// by `m`.  `parent` is the block it is part of.
    fn add(&mut self, index: usize, layer: i32, m: &DMat4, parent: Option<&Attributes>, depth: u32) {
        if self.over_budget {
            return;
        }
        let model = self.model;
        let object = &model.objects[index];
        // A member of a block whose own layer is off is hidden too.
        if parent.is_some()
            && (!object.attrs.visible || object.attrs.mode == 1 || !self.shown(self.layer_of(&object.attrs)))
        {
            return;
        }
        match &object.geometry {
            Geometry::Meshes(meshes) => {
                let source = if object.attrs.material_source == 3 || object.attrs.color_source == 3 {
                    parent.unwrap_or(&object.attrs)
                } else {
                    &object.attrs
                };
                let (color, transparency) = self.appearance(source);
                let key: Key = (color, (transparency.clamp(0.0, 1.0) * 100.0).round() as u8);
                let scale = self.scale;
                let mut added = 0u64;
                let prim = self.layers.entry(layer).or_default().entry(key).or_default();
                for mesh in meshes {
                    added += (mesh.indices.len() / 3) as u64;
                    append(prim, mesh, m, scale);
                }
                self.triangles += added;
                if self.triangles > self.limit {
                    self.over_budget = true;
                }
            }
            Geometry::Instance { definition, xform } => {
                if depth >= MAX_DEPTH {
                    return;
                }
                self.instances += 1;
                if self.instances > MAX_INSTANCES {
                    self.over_budget = true;
                    return;
                }
                let Some(members) = model.definitions.get(definition) else {
                    self.missing_blocks += 1;
                    return;
                };
                // The matrix is stored by rows, acting on column vectors;
                // glam's constructor takes columns.
                let x = DMat4::from_cols_array(xform).transpose();
                let inner = *m * x;
                for id in members {
                    if let Some(&member) = self.objects_by_id.get(id) {
                        // A block definition is never its own member.
                        if member != index {
                            self.add(member, layer, &inner, Some(&object.attrs), depth + 1);
                        }
                    }
                }
            }
        }
    }
}

pub fn build(a: &Archive, model: &Model, limits: &Limits) -> Scene {
    let mut b = Builder {
        model,
        scale: model.metres_per_unit.unwrap_or(1.0),
        limit: limits.max_triangles,
        by_index: model.layers.iter().enumerate().map(|(i, l)| (l.index, i)).collect(),
        by_id: model.layers.iter().enumerate().filter(|(_, l)| l.id != [0u8; 16]).map(|(i, l)| (l.id, i)).collect(),
        objects_by_id: model
            .objects
            .iter()
            .enumerate()
            .filter(|(_, o)| o.attrs.id != [0u8; 16])
            .map(|(i, o)| (o.attrs.id, i))
            .collect(),
        layers: BTreeMap::new(),
        triangles: 0,
        instances: 0,
        over_budget: false,
        hidden_objects: 0,
        missing_blocks: 0,
    };
    let mut in_blocks = 0usize;
    for (i, o) in model.objects.iter().enumerate() {
        if o.attrs.mode == 3 {
            // Part of a block definition: drawn by the instances.
            in_blocks += 1;
            continue;
        }
        if !o.attrs.visible || o.attrs.mode == 1 {
            b.hidden_objects += 1;
            continue;
        }
        b.add(i, o.attrs.layer, &DMat4::IDENTITY, None, 0);
    }
    let mut scene = Scene { name: "Rhino".into(), ..Default::default() };
    for (layer_index, prims) in std::mem::take(&mut b.layers) {
        let layer = b.by_index.get(&layer_index).map(|&i| &model.layers[i]);
        let mut mesh =
            Mesh { name: layer.map_or_else(|| "Layer".to_string(), |l| l.name.clone()), ..Default::default() };
        for ((color, transparency), mut prim) in prims {
            let m = Material {
                name: format!("{:02X}{:02X}{:02X}", color[0], color[1], color[2]),
                base_color: Vec4::new(
                    linear(color[0] as f32 / 255.0),
                    linear(color[1] as f32 / 255.0),
                    linear(color[2] as f32 / 255.0),
                    1.0 - transparency as f32 / 100.0,
                ),
                roughness: 0.6,
                alpha_mode: if transparency > 2 { AlphaMode::Blend } else { AlphaMode::Opaque },
                double_sided: true,
                ..Default::default()
            };
            scene.materials.push(m);
            prim.material = Some(scene.materials.len() - 1);
            prim.ensure_normals();
            mesh.primitives.push(prim);
        }
        let node = scene.add_mesh_node(mesh);
        scene.nodes[node].visible = b.shown(layer);
    }
    scene.convert_z_up();
    let mut warnings = Vec::new();
    if model.without_meshes > 0 {
        warnings.push(format!(
            "{} objects have no render mesh (saved without them) and are not drawn.",
            model.without_meshes
        ));
    }
    for (what, n) in &model.skipped {
        warnings.push(format!("{n} {what} are not drawn."));
    }
    if b.missing_blocks > 0 {
        warnings.push(format!("{} block instances name definitions the file lacks.", b.missing_blocks));
    }
    if b.over_budget {
        warnings
            .push("The model has more than the limit of triangles or block instances; the rest are left out.".into());
    }
    let mut details = vec![
        ("Layers".to_string(), model.layers.len().to_string()),
        ("Objects".to_string(), model.object_count.to_string()),
        ("Triangles".to_string(), b.triangles.min(limits.max_triangles).to_string()),
    ];
    if b.instances > 0 {
        details.push(("Block instances".into(), b.instances.to_string()));
    }
    if in_blocks > 0 {
        details.push(("Objects in block definitions".into(), in_blocks.to_string()));
    }
    if b.hidden_objects > 0 {
        details.push(("Hidden objects".into(), b.hidden_objects.to_string()));
    }
    if let Some(m) = model.metres_per_unit {
        details.push(("Metres per unit".into(), format!("{m}")));
    }
    scene.info = Info {
        format: format!("Rhino 3dm ({})", release(a.version)),
        generator: None,
        warnings,
        details,
        neighbours: Vec::new(),
    };
    scene
}

/// Adds a mesh to a primitive, moved by `m` and scaled to metres.
fn append(prim: &mut Primitive, mesh: &MeshData, m: &DMat4, scale: f64) {
    let base = prim.positions.len() as u32;
    let identity = *m == DMat4::IDENTITY;
    let linear_part = DMat3::from_mat4(*m);
    let mirrored = linear_part.determinant() < 0.0;
    let place = |p: &[f32; 3]| -> Vec3 {
        let v = DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64);
        let v = if identity { v } else { m.transform_point3(v) };
        (v * scale).as_vec3()
    };
    let mut part = Primitive {
        positions: mesh.positions.iter().map(place).collect(),
        indices: mesh.indices.clone(),
        ..Default::default()
    };
    if mirrored {
        // A mirror turns triangles inside out.
        for t in part.indices.chunks_exact_mut(3) {
            t.swap(1, 2);
        }
    }
    if mesh.normals.len() == mesh.positions.len() {
        // Normals follow the transpose of the inverse; for rotations and
        // uniform scales that is the matrix itself.
        let n_matrix = if identity { DMat3::IDENTITY } else { linear_part.inverse().transpose() };
        part.normals = mesh
            .normals
            .iter()
            .map(|n| {
                let v = n_matrix * DVec3::new(n[0] as f64, n[1] as f64, n[2] as f64);
                v.as_vec3().try_normalize().unwrap_or(Vec3::Z)
            })
            .collect();
    } else {
        part.ensure_normals();
    }
    // Vertex colours show when the mesh has them (as in Rhino's shaded
    // mode); white ones change nothing.
    if mesh.colors.len() == mesh.positions.len() && mesh.colors.iter().any(|&c| c & 0xFF_FFFF != 0xFF_FFFF) {
        part.colors = mesh
            .colors
            .iter()
            .map(|&c| {
                let ch = |s: u32| linear((c >> s & 255) as f32 / 255.0);
                Vec4::new(ch(0), ch(8), ch(16), 1.0)
            })
            .collect();
    }
    // Merge: attributes present in some meshes only are filled in.
    let had = prim.positions.len();
    prim.positions.extend(part.positions);
    prim.normals.extend(part.normals);
    if !part.colors.is_empty() || !prim.colors.is_empty() {
        prim.colors.resize(had, Vec4::ONE);
        if part.colors.is_empty() {
            prim.colors.resize(prim.positions.len(), Vec4::ONE);
        } else {
            prim.colors.extend(part.colors);
        }
    }
    prim.indices.extend(part.indices.iter().map(|i| i + base));
}
