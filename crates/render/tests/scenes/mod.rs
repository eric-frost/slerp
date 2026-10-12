//! Generated test scenes, shared by the reference-image tests and the
//! render_png example.

#![allow(dead_code)]

use glam::{Mat4, Quat, Vec2, Vec3, Vec4};
use slerp_scene::*;
use std::sync::Arc;

fn node_with(scene: &mut Scene, prim: Primitive, material: Material, t: Transform) -> usize {
    let mut prim = prim;
    prim.material = Some(scene.materials.len());
    scene.materials.push(material);
    let n = scene.add_mesh_node(Mesh { name: "m".into(), primitives: vec![prim], target_names: vec![] });
    scene.nodes[n].transform = t;
    n
}

fn at(x: f32, y: f32, z: f32) -> Transform {
    Transform { translation: Vec3::new(x, y, z), ..Transform::IDENTITY }
}

/// One grey sphere, the simplest thumbnail.
pub fn ball() -> Scene {
    Scene::plain_ball(Material::default())
}

/// A ball displaced by a height map of raised stripes, as material files
/// with height maps are shown.
pub fn displaced() -> Scene {
    let mut data = Vec::new();
    for y in 0..64u32 {
        for _x in 0..64u32 {
            let h = if (y / 4) % 2 == 0 { 255 } else { 0 };
            data.extend_from_slice(&[h, h, h, 255]);
        }
    }
    let mut material = Material {
        base_color: Vec4::new(0.6, 0.62, 0.65, 1.0),
        roughness: 0.4,
        height_scale: 0.04,
        ..Default::default()
    };
    material.height_texture = Some(TexRef::new(0));
    let asset = MaterialAsset {
        name: "stripes".into(),
        material,
        textures: vec![Arc::new(Texture::from_rgba8(64, 64, data, ColorSpace::Linear))],
        preview: None,
        info: Default::default(),
    };
    Scene::material_ball(&asset, Shape::Sphere, 256)
}

/// Spheres in rows of rising metalness (bottom to top) and columns of
/// rising roughness (left to right).
pub fn spheres() -> Scene {
    let mut scene = Scene::default();
    let sphere = Shape::Sphere.primitive(48);
    for row in 0..5 {
        for col in 0..5 {
            let m = Material {
                base_color: Vec4::new(1.0, 0.71, 0.29, 1.0),
                metallic: row as f32 / 4.0,
                roughness: (col as f32 / 4.0).max(0.03),
                ..Default::default()
            };
            node_with(&mut scene, sphere.clone(), m, at((col as f32 - 2.0) * 1.1, (row as f32 - 2.0) * 1.1, 0.0));
        }
    }
    scene
}

fn checker(size: u32, cells: u32, a: [u8; 4], b: [u8; 4]) -> Texture {
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let even = ((x * cells / size) + (y * cells / size)).is_multiple_of(2);
            data.extend_from_slice(if even { &a } else { &b });
        }
    }
    Texture::from_rgba8(size, size, data, ColorSpace::Unspecified)
}

/// A red cube and a glossy white sphere on a checkered plane.
pub fn shapes() -> Scene {
    let mut scene = Scene::default();
    scene.textures.push(Arc::new(checker(256, 8, [200, 200, 200, 255], [120, 120, 125, 255])));
    let mut floor = Material { roughness: 0.8, ..Default::default() };
    let mut tex = TexRef::new(0);
    tex.transform = Some(UvTransform { offset: Vec2::ZERO, rotation: 0.0, scale: Vec2::splat(2.0) });
    floor.base_color_texture = Some(tex);
    node_with(
        &mut scene,
        Shape::Plane.primitive(1),
        floor,
        Transform { scale: Vec3::splat(4.0), ..Transform::IDENTITY },
    );
    let red = Material { base_color: Vec4::new(0.8, 0.05, 0.04, 1.0), roughness: 0.35, ..Default::default() };
    node_with(
        &mut scene,
        Shape::Cube.primitive(1),
        red,
        Transform { translation: Vec3::new(-0.7, 0.5, 0.0), rotation: Quat::from_rotation_y(0.4), scale: Vec3::ONE },
    );
    let white =
        Material { base_color: Vec4::new(0.9, 0.9, 0.9, 1.0), roughness: 0.15, clearcoat: 1.0, ..Default::default() };
    node_with(&mut scene, Shape::Sphere.primitive(64), white, at(0.75, 0.5, 0.3));
    let gold =
        Material { base_color: Vec4::new(1.0, 0.78, 0.34, 1.0), metallic: 1.0, roughness: 0.25, ..Default::default() };
    node_with(
        &mut scene,
        Shape::Sphere.primitive(64),
        gold,
        Transform { translation: Vec3::new(0.1, 0.25, -0.9), scale: Vec3::splat(0.5), ..Transform::IDENTITY },
    );
    scene
}

/// A Quake-looking scene: a brick wall with a baked lightmap through UV
/// set 1, a warping liquid and a scrolling sky.
pub fn quake() -> Scene {
    let mut scene = Scene { kind: SceneKind::Object, ..Default::default() };
    // 0: bricks, 64x64, nearest filtered like Quake's software renderer.
    let mut bricks = Vec::new();
    for y in 0..64u32 {
        for x in 0..64u32 {
            let row = y / 8;
            let mortar = y % 8 == 0 || (x + if row % 2 == 0 { 0 } else { 8 }) % 16 == 0;
            let n = ((x * 7 + y * 13) % 11) as u8;
            bricks.extend_from_slice(&if mortar { [90, 85, 75, 255] } else { [140, 70 + n, 50, 255] });
        }
    }
    scene.textures.push(Arc::new(Texture::from_rgba8(64, 64, bricks, ColorSpace::Srgb)));
    // 1: lightmap, 16x16, a warm pool of light.
    let mut lm = Vec::new();
    for y in 0..16 {
        for x in 0..16 {
            let d = Vec2::new(x as f32 - 5.0, y as f32 - 6.0).length();
            let l = (0.25 + 0.75 * (-d * d / 30.0).exp()).min(1.0);
            lm.extend_from_slice(&[(l * 230.0) as u8, (l * 200.0) as u8, (l * 150.0) as u8, 255]);
        }
    }
    scene.textures.push(Arc::new(Texture::from_rgba8(16, 16, lm, ColorSpace::Linear)));
    // 2: liquid.
    let mut water = Vec::new();
    for y in 0..64u32 {
        for x in 0..64u32 {
            let v = (((x as f32 * 0.4).sin() + (y as f32 * 0.3).cos()) * 20.0) as i32;
            water.extend_from_slice(&[(30 + v / 2).clamp(0, 255) as u8, (60 + v).clamp(0, 255) as u8, 140, 255]);
        }
    }
    scene.textures.push(Arc::new(Texture::from_rgba8(64, 64, water, ColorSpace::Srgb)));
    // 3: sky, 256x128: left half clouds over black, right half the back.
    let mut sky = Vec::new();
    for y in 0..128u32 {
        for x in 0..256u32 {
            if x < 128 {
                let c = ((x as f32 * 0.2).sin() * (y as f32 * 0.15).cos() * 255.0).max(0.0) as u8;
                sky.extend_from_slice(&if c > 120 { [c, c, c, 255] } else { [0, 0, 0, 255] });
            } else {
                sky.extend_from_slice(&[40, 30, (60 + y / 4) as u8, 255]);
            }
        }
    }
    scene.textures.push(Arc::new(Texture::from_rgba8(256, 128, sky, ColorSpace::Srgb)));

    let nearest = Sampler { mag: Filter::Nearest, min: Filter::Nearest, mipmaps: false, ..Sampler::default() };
    let mut wall = Shape::Plane.primitive(1);
    // Stand the plane up facing +Z, with UV set 1 for the lightmap.
    for p in &mut wall.positions {
        *p = Vec3::new(p.x, -p.z, 0.0);
    }
    for n in &mut wall.normals {
        *n = Vec3::Z;
    }
    wall.tangents.clear();
    wall.uv1 = wall.uv0.clone();
    for uv in &mut wall.uv0 {
        *uv *= 3.0;
    }
    let mut m = Material { lightmap_scale: 2.0, ..Default::default() };
    m.base_color_texture = Some(TexRef { sampler: nearest, ..TexRef::new(0) });
    m.lightmap_texture = Some(TexRef { uv_set: 1, ..TexRef::new(1) });
    node_with(
        &mut scene,
        wall.clone(),
        m,
        Transform { translation: Vec3::new(-1.05, 0.0, 0.0), ..Transform::IDENTITY },
    );
    let mut m = Material { effect: Effect::Warp, ..Default::default() };
    m.base_color_texture = Some(TexRef { sampler: nearest, ..TexRef::new(2) });
    let mut liquid = wall.clone();
    liquid.uv0.iter_mut().for_each(|uv| *uv /= 1.5);
    node_with(&mut scene, liquid, m, Transform { translation: Vec3::new(0.0, 0.0, 0.0), ..Transform::IDENTITY });
    let mut m = Material { effect: Effect::Sky, ..Default::default() };
    m.base_color_texture = Some(TexRef::new(3));
    node_with(&mut scene, wall, m, Transform { translation: Vec3::new(1.05, 0.0, 0.0), ..Transform::IDENTITY });
    scene
}

/// A tube skinned to two joints, the upper one bent, with a morph target
/// that bulges it.
pub fn skinned() -> Scene {
    let mut scene = Scene::default();
    let mut prim = Primitive::default();
    let (rings, seg) = (16u32, 24u32);
    for r in 0..=rings {
        let y = r as f32 / rings as f32 * 2.0;
        for s in 0..=seg {
            let a = s as f32 / seg as f32 * std::f32::consts::TAU;
            let n = Vec3::new(a.cos(), 0.0, a.sin());
            prim.positions.push(n * 0.2 + Vec3::Y * y);
            prim.normals.push(n);
            prim.uv0.push(Vec2::new(s as f32 / seg as f32, y / 2.0));
            let w = (y - 0.6).clamp(0.0, 0.8) / 0.8;
            prim.joints.push([0, 1, 0, 0]);
            prim.weights.push(Vec4::new(1.0 - w, w, 0.0, 0.0));
        }
    }
    for r in 0..rings {
        for s in 0..seg {
            let a = r * (seg + 1) + s;
            let b = a + seg + 1;
            prim.indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    let bulge = MorphTarget {
        positions: prim
            .positions
            .iter()
            .map(|p| {
                let k = (-(p.y - 0.5).powi(2) * 20.0).exp() * 0.15;
                Vec3::new(p.x, 0.0, p.z).normalize_or_zero() * k
            })
            .collect(),
        normals: vec![],
    };
    prim.targets.push(bulge);
    prim.material = Some(0);
    scene.materials.push(Material { base_color: Vec4::new(0.2, 0.5, 0.9, 1.0), roughness: 0.4, ..Default::default() });
    scene.meshes.push(Mesh { name: "tube".into(), primitives: vec![prim], target_names: vec!["bulge".into()] });
    let mut mesh_node = Node::new("tube");
    mesh_node.mesh = Some(0);
    mesh_node.skin = Some(0);
    mesh_node.weights = vec![1.0];
    // Its own transform must be ignored: skinned meshes follow the joints.
    mesh_node.transform = at(5.0, 0.0, 0.0);
    let mut root = Node::new("root");
    root.children = vec![2];
    let mut upper = Node::new("upper");
    upper.transform =
        Transform { translation: Vec3::new(0.0, 1.0, 0.0), rotation: Quat::from_rotation_z(-0.7), scale: Vec3::ONE };
    scene.nodes = vec![mesh_node, root, upper];
    scene.nodes[1].children = vec![2];
    scene.roots = vec![0, 1];
    scene.skins.push(Skin {
        name: "skin".into(),
        joints: vec![1, 2],
        inverse_bind: vec![Mat4::IDENTITY, Mat4::from_translation(Vec3::new(0.0, -1.0, 0.0))],
        skeleton: Some(1),
    });
    scene
}

/// A masked leaf pattern in front of a blended glass pane, in front of a
/// coloured wall.
pub fn alpha() -> Scene {
    let mut scene = Scene::default();
    let mut leaves = Vec::new();
    for y in 0..128u32 {
        for x in 0..128u32 {
            let d = Vec2::new((x % 32) as f32 - 16.0, (y % 32) as f32 - 16.0).length();
            leaves.extend_from_slice(&[40, 160, 60, if d < 12.0 { 255 } else { 0 }]);
        }
    }
    scene.textures.push(Arc::new(Texture::from_rgba8(128, 128, leaves, ColorSpace::Srgb)));
    let mut quad = Shape::Plane.primitive(1);
    for p in &mut quad.positions {
        *p = Vec3::new(p.x, -p.z, 0.0);
    }
    quad.normals.iter_mut().for_each(|n| *n = Vec3::Z);
    quad.tangents.clear();
    let wall = Material { base_color: Vec4::new(0.7, 0.3, 0.1, 1.0), ..Default::default() };
    node_with(
        &mut scene,
        quad.clone(),
        wall,
        Transform { translation: Vec3::new(0.0, 0.0, -0.5), scale: Vec3::splat(2.0), ..Transform::IDENTITY },
    );
    let glass = Material {
        base_color: Vec4::new(0.3, 0.5, 1.0, 0.35),
        roughness: 0.05,
        alpha_mode: AlphaMode::Blend,
        double_sided: true,
        ..Default::default()
    };
    node_with(
        &mut scene,
        quad.clone(),
        glass,
        Transform { translation: Vec3::new(0.4, 0.0, 0.0), ..Transform::IDENTITY },
    );
    let mut leaf = Material { alpha_mode: AlphaMode::Mask(0.5), double_sided: true, ..Default::default() };
    leaf.base_color_texture = Some(TexRef::new(0));
    node_with(&mut scene, quad, leaf, Transform { translation: Vec3::new(-0.4, 0.0, 0.5), ..Transform::IDENTITY });
    scene
}

/// A small Gaussian splat cloud: a ring of round splats shading from red
/// to blue, long thin splats turned like the spokes of a wheel, a faint
/// translucent veil behind them, and a ball of splats whose colour
/// depends on the viewing direction (spherical-harmonic band 1), green
/// seen from +X and magenta from -X.  Its two stray splats, far off,
/// mustn't shrink the framing.
pub fn splats() -> Scene {
    let mut s = Splats { sh_degree: 1, ..Default::default() };
    let half = |v: f32| {
        // f32 to IEEE half bits, for the small values used here.
        let bits = v.to_bits();
        let sign = ((bits >> 16) & 0x8000) as u16;
        let exp = ((bits >> 23) & 0xff) as i32 - 127 + 15;
        if v == 0.0 || exp <= 0 {
            return sign;
        }
        sign | ((exp as u16) << 10) | ((bits >> 13) & 0x3ff) as u16
    };
    let mut push = |p: Vec3, scale: Vec3, rot: Quat, color: Vec4, sh_x: f32| {
        s.positions.push(p);
        s.scales.push(scale);
        s.rotations.push(rot);
        s.colors.push(color);
        // Band 1 is (-y, z, -x) times C1: a coefficient on the third adds
        // colour seen from -X and takes it away seen from +X.
        for k in 0..3 {
            let v = if k == 2 { [-sh_x, sh_x, -sh_x] } else { [0.0; 3] };
            s.sh.extend(v.map(half));
        }
    };
    for i in 0..48 {
        let a = i as f32 / 48.0 * std::f32::consts::TAU;
        let t = i as f32 / 47.0;
        push(
            Vec3::new(a.cos(), 0.0, a.sin()),
            Vec3::splat(0.08),
            Quat::IDENTITY,
            Vec4::new(0.9 - 0.8 * t, 0.2, 0.1 + 0.8 * t, 0.9),
            0.0,
        );
    }
    for i in 0..12 {
        let a = i as f32 / 12.0 * std::f32::consts::TAU;
        push(
            Vec3::new(a.cos() * 0.55, 0.0, a.sin() * 0.55),
            Vec3::new(0.3, 0.02, 0.02),
            Quat::from_rotation_y(-a),
            Vec4::new(0.95, 0.85, 0.3, 0.95),
            0.0,
        );
    }
    for i in 0..30 {
        let x = (i % 6) as f32 / 5.0 * 2.0 - 1.0;
        let y = (i / 6) as f32 / 4.0 * 1.2 - 0.2;
        push(Vec3::new(x, y, -1.4), Vec3::new(0.25, 0.25, 0.02), Quat::IDENTITY, Vec4::new(0.5, 0.7, 0.9, 0.25), 0.0);
    }
    for i in 0..200 {
        // A Fibonacci sphere of radius 0.3 above the ring's centre.
        let y = 1.0 - (i as f32 + 0.5) / 100.0;
        let r = (1.0 - y * y).max(0.0).sqrt();
        let a = i as f32 * 2.399_963;
        push(
            Vec3::new(a.cos() * r, y, a.sin() * r) * 0.3 + Vec3::Y * 0.45,
            Vec3::splat(0.05),
            Quat::IDENTITY,
            Vec4::new(0.5, 0.5, 0.5, 1.0),
            0.6,
        );
    }
    push(Vec3::splat(400.0), Vec3::splat(1.0), Quat::IDENTITY, Vec4::ONE, 0.0);
    push(Vec3::splat(-300.0), Vec3::splat(1.0), Quat::IDENTITY, Vec4::ONE, 0.0);
    Scene { name: "splats".into(), splats: Some(s), ..Default::default() }
}

/// A checkered wall with a red ball in front of it, seen through glass: a
/// clear ball with a volume (refraction), an amber one that absorbs light
/// as it passes (Beer's law), a rough frosted cube, and a thin pane.
pub fn glass() -> Scene {
    let mut scene = Scene { name: "glass".into(), ..Default::default() };
    let mut checker = Vec::new();
    for y in 0..64u32 {
        for x in 0..64u32 {
            let on = (x / 8 + y / 8) % 2 == 0;
            checker.extend_from_slice(if on { &[230, 230, 230, 255] } else { &[40, 40, 45, 255] });
        }
    }
    scene.textures.push(Arc::new(Texture::from_rgba8(64, 64, checker, ColorSpace::Srgb)));
    let mut wall = Shape::Plane.primitive(1);
    for p in &mut wall.positions {
        *p = Vec3::new(p.x, -p.z, 0.0);
    }
    wall.normals.iter_mut().for_each(|n| *n = Vec3::Z);
    wall.tangents.clear();
    let mut paper = Material { roughness: 0.9, ..Default::default() };
    paper.base_color_texture = Some(TexRef::new(0));
    node_with(
        &mut scene,
        wall.clone(),
        paper,
        Transform { translation: Vec3::new(0.0, 0.0, -0.6), scale: Vec3::new(3.2, 1.8, 1.0), ..Transform::IDENTITY },
    );
    let red = Material { base_color: Vec4::new(0.8, 0.05, 0.04, 1.0), roughness: 0.4, ..Default::default() };
    node_with(
        &mut scene,
        Shape::Sphere.primitive(24),
        red,
        Transform { scale: Vec3::splat(0.25), ..at(-0.2, -0.25, -0.35) },
    );
    let clear =
        Material { base_color: Vec4::ONE, roughness: 0.02, transmission: 1.0, thickness: 1.0, ..Default::default() };
    node_with(
        &mut scene,
        Shape::Sphere.primitive(32),
        clear,
        Transform { scale: Vec3::splat(0.5), ..at(-1.0, 0.0, 0.0) },
    );
    let amber = Material {
        base_color: Vec4::ONE,
        roughness: 0.05,
        transmission: 1.0,
        thickness: 1.0,
        attenuation_color: Vec3::new(0.9, 0.5, 0.1),
        attenuation_distance: 0.3,
        ..Default::default()
    };
    node_with(
        &mut scene,
        Shape::Sphere.primitive(32),
        amber,
        Transform { scale: Vec3::splat(0.5), ..at(0.0, 0.0, 0.0) },
    );
    let frosted = Material {
        base_color: Vec4::new(0.85, 0.95, 1.0, 1.0),
        roughness: 0.45,
        transmission: 1.0,
        thickness: 0.6,
        ..Default::default()
    };
    node_with(
        &mut scene,
        Shape::Cube.primitive(1),
        frosted,
        Transform { scale: Vec3::splat(0.6), ..at(1.0, 0.0, 0.0) },
    );
    let pane = Material {
        base_color: Vec4::new(0.6, 0.9, 0.7, 1.0),
        roughness: 0.0,
        transmission: 0.9,
        double_sided: true,
        ..Default::default()
    };
    node_with(
        &mut scene,
        wall,
        pane,
        Transform { translation: Vec3::new(0.0, -0.7, 0.3), scale: Vec3::new(2.6, 0.35, 1.0), ..Transform::IDENTITY },
    );
    scene
}

/// Point clouds: a plain torus of points, which ambient occlusion shapes
/// (darkest inside the ring), and beside it a coloured wavy sheet, twice
/// as dense and scaled up by its node, which shows its colours as they are.
pub fn points() -> Scene {
    let mut scene = Scene { name: "points".into(), ..Default::default() };
    let unlit = |c: Vec4| Material { base_color: c, unlit: true, double_sided: true, ..Default::default() };
    let mut torus = Primitive { topology: Topology::Points, ..Default::default() };
    let (around, across) = (120, 40);
    for i in 0..around {
        for j in 0..across {
            // Rows offset by half a step, as a scanner's would be.
            let a = (i as f32 + 0.5 * (j % 2) as f32) / around as f32 * std::f32::consts::TAU;
            let b = j as f32 / across as f32 * std::f32::consts::TAU;
            let r = 0.4 + 0.17 * b.cos();
            torus.positions.push(Vec3::new(r * a.cos(), 0.17 * b.sin(), r * a.sin()));
        }
    }
    torus.indices = (0..torus.positions.len() as u32).collect();
    let tilt = Transform { rotation: Quat::from_rotation_x(0.6), ..at(-0.6, 0.45, 0.0) };
    node_with(&mut scene, torus, unlit(Vec4::new(0.8, 0.8, 0.8, 1.0)), tilt);
    let mut sheet = Primitive { topology: Topology::Points, ..Default::default() };
    let m = 70;
    for j in 0..m {
        for i in 0..m {
            let (u, v) = (i as f32 / (m - 1) as f32, j as f32 / (m - 1) as f32);
            sheet.positions.push(Vec3::new(u - 0.5, 0.08 * (u * 9.0).sin() * (v * 6.0).cos(), v - 0.5) * 0.5);
            sheet.colors.push(Vec4::new(u, 0.3 + 0.5 * v, 1.0 - u, 1.0));
        }
    }
    sheet.indices = (0..(m * m) as u32).collect();
    node_with(&mut scene, sheet, unlit(Vec4::ONE), Transform { scale: Vec3::splat(2.0), ..at(0.7, 0.3, 0.0) });
    scene
}

/// A wide floor under a ceiling that shows a panorama
/// sky: four coloured stripes around the turn (so a seam would show), the
/// horizon a white line, a gradient above it.  Seen from inside.
pub fn panorama_sky() -> Scene {
    let mut scene = Scene { name: "panorama".into(), ..Default::default() };
    let (w, h) = (64u32, 32u32);
    let mut px = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let band = [[220u8, 60, 50], [60, 200, 80], [60, 90, 220], [230, 200, 60]][(x * 4 / w) as usize];
            let c = if y == 20 {
                [255, 255, 255]
            } else {
                let k = y as f32 / h as f32;
                band.map(|v| (v as f32 * (0.4 + 0.6 * k)) as u8)
            };
            px.extend_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    scene.textures.push(Arc::new(Texture::from_rgba8(w, h, px, ColorSpace::Srgb)));
    let mut sky = Material { unlit: true, effect: Effect::PanoramaSky, ..Default::default() };
    sky.base_color_texture = Some(TexRef {
        transform: Some(UvTransform {
            offset: Vec2::new(0.0, 20.0 / 32.0),
            rotation: 0.0,
            scale: Vec2::new(1.0, -0.5),
        }),
        sampler: Sampler { wrap_v: Wrap::ClampToEdge, ..Default::default() },
        ..TexRef::new(0)
    });
    // The ceiling faces down, the floor up.
    let mut ceiling = Shape::Plane.primitive(1);
    for p in &mut ceiling.positions {
        *p = Vec3::new(p.x, 0.0, -p.z);
    }
    ceiling.normals.iter_mut().for_each(|n| *n = -Vec3::Y);
    ceiling.tangents.clear();
    node_with(
        &mut scene,
        ceiling,
        sky,
        Transform { translation: Vec3::Y * 3.0, scale: Vec3::splat(80.0), ..Transform::IDENTITY },
    );
    let floor = Material { base_color: Vec4::new(0.5, 0.45, 0.4, 1.0), unlit: true, ..Default::default() };
    node_with(
        &mut scene,
        Shape::Plane.primitive(1),
        floor,
        Transform { scale: Vec3::splat(80.0), ..Transform::IDENTITY },
    );
    scene.kind = SceneKind::Level;
    scene
}
