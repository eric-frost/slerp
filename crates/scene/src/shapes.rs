//! Generated shapes for showing materials.

use crate::{Material, MaterialAsset, Mesh, Primitive, Scene, SceneKind};
use glam::{Vec2, Vec3, Vec4};
use std::f32::consts::{PI, TAU};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Shape {
    #[default]
    Sphere,
    Cube,
    Plane,
}

impl Shape {
    /// The shape, about 1 m across and centred on the origin, with normals,
    /// tangents and UVs.  `detail` is the number of segments around a
    /// sphere or along a plane's side; displacement needs a few hundred.
    pub fn primitive(self, detail: u32) -> Primitive {
        match self {
            Shape::Sphere => sphere(detail.max(8)),
            Shape::Cube => cube(),
            Shape::Plane => plane(detail.max(1)),
        }
    }
}

fn sphere(segments: u32) -> Primitive {
    let rings = segments / 2;
    let mut p = Primitive::default();
    for r in 0..=rings {
        let v = r as f32 / rings as f32;
        let theta = v * PI;
        for s in 0..=segments {
            let u = s as f32 / segments as f32;
            let phi = u * TAU;
            let n = Vec3::new(-phi.cos() * theta.sin(), theta.cos(), phi.sin() * theta.sin());
            p.positions.push(n * 0.5);
            p.normals.push(n);
            p.tangents.push(Vec4::new(phi.sin(), 0.0, phi.cos(), 1.0));
            // Two tiles around, one from pole to pole, so square textures
            // keep their proportions.
            p.uv0.push(Vec2::new(u * 2.0, v));
        }
    }
    let row = segments + 1;
    for r in 0..rings {
        for s in 0..segments {
            let a = r * row + s;
            let b = a + row;
            p.indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    p
}

fn cube() -> Primitive {
    let mut p = Primitive::default();
    let faces = [
        (Vec3::X, Vec3::NEG_Z),
        (Vec3::NEG_X, Vec3::Z),
        (Vec3::Y, Vec3::X),
        (Vec3::NEG_Y, Vec3::X),
        (Vec3::Z, Vec3::X),
        (Vec3::NEG_Z, Vec3::NEG_X),
    ];
    for (n, t) in faces {
        let b = n.cross(t);
        let base = p.positions.len() as u32;
        for (u, v) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] {
            let pos = n * 0.5 + t * (u - 0.5) - b * (v - 0.5);
            p.positions.push(pos);
            p.normals.push(n);
            p.tangents.push(t.extend(1.0));
            p.uv0.push(Vec2::new(u, v));
        }
        p.indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
    }
    p
}

fn plane(segments: u32) -> Primitive {
    let mut p = Primitive::default();
    for y in 0..=segments {
        for x in 0..=segments {
            let (u, v) = (x as f32 / segments as f32, y as f32 / segments as f32);
            p.positions.push(Vec3::new(u - 0.5, 0.0, v - 0.5));
            p.normals.push(Vec3::Y);
            p.tangents.push(Vec4::new(1.0, 0.0, 0.0, 1.0));
            p.uv0.push(Vec2::new(u, v));
        }
    }
    let row = segments + 1;
    for y in 0..segments {
        for x in 0..segments {
            let a = y * row + x;
            p.indices.extend_from_slice(&[a, a + row, a + 1, a + 1, a + row, a + row + 1]);
        }
    }
    p
}

impl Scene {
    /// A scene holding one shape wearing `material`.
    pub fn material_ball(asset: &MaterialAsset, shape: Shape, detail: u32) -> Scene {
        let mut scene = Scene {
            name: asset.name.clone(),
            kind: SceneKind::MaterialBall,
            materials: vec![asset.material.clone()],
            textures: asset.textures.clone(),
            info: asset.info.clone(),
            ..Default::default()
        };
        let mut prim = shape.primitive(detail);
        prim.material = Some(0);
        scene.add_mesh_node(Mesh { name: "shape".into(), primitives: vec![prim], target_names: vec![] });
        scene
    }

    /// A scene holding `material` on a sphere, with no textures.
    pub fn plain_ball(material: Material) -> Scene {
        let asset = MaterialAsset {
            name: material.name.clone(),
            material,
            textures: vec![],
            preview: None,
            info: Default::default(),
        };
        Scene::material_ball(&asset, Shape::Sphere, 64)
    }
}
