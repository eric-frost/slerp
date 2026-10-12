//! Renders loaded models into a contact sheet with a tiny CPU rasterizer,
//! for eyeballing the loaders (orientation, UVs, materials, skinning) before
//! the real renderer exists.  Each cell is an orthographic 3/4 view of the
//! rest pose, Lambert-lit, with base colour, vertex colours and the base
//! colour texture.  Cells are listed on stdout in reading order.
//!
//! `cargo run --release -p slerp-models --example contact -- out.png [--cell N] [--cols N] [--pose CLIP:SECONDS] <file>...`

use slerp_models::{load, sniff};
use slerp_scene::glam::{Mat3, Mat4, Vec2, Vec3, Vec4};
use slerp_scene::textures::Pixels;
use slerp_scene::{Asset, Limits, Pose, Scene, Topology};
use std::io::Write;

struct Canvas {
    w: usize,
    color: Vec<[u8; 3]>,
    depth: Vec<f32>,
}

fn sample(scene: &Scene, tex: usize, uv: Vec2) -> Vec4 {
    let Some(t) = scene.textures.get(tex) else { return Vec4::ONE };
    let level = &t.levels[0];
    let (w, h) = (level.width as usize, level.height as usize);
    let x = ((uv.x.rem_euclid(1.0) * w as f32) as usize).min(w - 1);
    let y = ((uv.y.rem_euclid(1.0) * h as f32) as usize).min(h - 1);
    let i = (y * w + x) * 4;
    match &level.surfaces[0] {
        Pixels::Rgba8(p) => {
            let c = |v: u8| (v as f32 / 255.0).powf(2.2);
            Vec4::new(c(p[i]), c(p[i + 1]), c(p[i + 2]), p[i + 3] as f32 / 255.0)
        }
        Pixels::Rgba32F(p) => Vec4::new(p[i], p[i + 1], p[i + 2], p[i + 3]),
        Pixels::Blocks(..) => Vec4::ONE,
    }
}

fn render(scene: &Scene, pose: &Pose, canvas: &mut Canvas, ox: usize, oy: usize, size: usize) {
    let bounds = scene.bounds(pose);
    if bounds.is_empty() {
        return;
    }
    let dir = Vec3::new(1.0, 0.7, 1.3).normalize();
    let view = slerp_scene::glam::camera::rh::view::look_at_mat4(
        bounds.center() + dir * bounds.radius() * 3.0,
        bounds.center(),
        Vec3::Y,
    );
    let scale = size as f32 * 0.45 / bounds.radius().max(1e-6);
    let light = Vec3::new(0.4, 0.8, 0.6).normalize();
    for (ni, node) in scene.nodes.iter().enumerate() {
        let (Some(mi), true) = (node.mesh, node.visible) else { continue };
        let joints = node.skin.filter(|&s| s < scene.skins.len()).map(|s| pose.joint_matrices(scene, s));
        for prim in &scene.meshes[mi].primitives {
            let material = prim.material.and_then(|m| scene.materials.get(m));
            let base = material.map_or(Vec4::splat(0.8), |m| m.base_color);
            let tex = material.and_then(|m| m.base_color_texture).map(|t| t.texture);
            let skinned = joints.is_some() && prim.is_skinned();
            let world: Vec<(Vec3, Vec3)> = prim
                .positions
                .iter()
                .enumerate()
                .map(|(i, &p)| {
                    let m = if skinned {
                        let js = joints.as_ref().unwrap();
                        let (j, w) = (prim.joints[i], prim.weights[i]);
                        (0..4).fold(Mat4::ZERO, |acc, k| {
                            acc + js.get(j[k] as usize).copied().unwrap_or(Mat4::IDENTITY) * w[k]
                        })
                    } else {
                        pose.world[ni]
                    };
                    let n = prim.normals.get(i).copied().unwrap_or(Vec3::Y);
                    (m.transform_point3(p), (Mat3::from_mat4(m).inverse().transpose() * n).normalize_or_zero())
                })
                .collect();
            let project = |p: Vec3| {
                let v = view.transform_point3(p);
                Vec3::new(
                    ox as f32 + size as f32 * 0.5 + v.x * scale,
                    oy as f32 + size as f32 * 0.5 - v.y * scale,
                    -v.z,
                )
            };
            let shade = |i: usize, n: Vec3| -> Vec3 {
                let mut c = base;
                if let Some(col) = prim.colors.get(i) {
                    c *= *col;
                }
                if let (Some(t), Some(&uv)) = (tex, prim.uv0.get(i)) {
                    c *= sample(scene, t, uv);
                }
                let lambert = n.dot(light).abs() * 0.75 + 0.25;
                c.truncate() * lambert + material.map_or(Vec3::ZERO, |m| m.emissive)
            };
            match prim.topology {
                Topology::Points | Topology::Lines => {
                    for &i in &prim.indices {
                        let Some(&(p, n)) = world.get(i as usize) else { continue };
                        let s = project(p);
                        let (x, y) = (s.x as isize, s.y as isize);
                        if x >= ox as isize && y >= oy as isize && (x as usize) < ox + size && (y as usize) < oy + size
                        {
                            let k = y as usize * canvas.w + x as usize;
                            if s.z < canvas.depth[k] {
                                canvas.depth[k] = s.z;
                                let c = shade(i as usize, n);
                                canvas.color[k] =
                                    c.to_array().map(|v| (v.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0) as u8);
                            }
                        }
                    }
                }
                Topology::Triangles => {
                    for t in prim.indices.chunks_exact(3) {
                        let idx = [t[0] as usize, t[1] as usize, t[2] as usize];
                        if idx.iter().any(|&i| i >= world.len()) {
                            continue;
                        }
                        let s = idx.map(|i| project(world[i].0));
                        let min = s.iter().fold(Vec2::splat(f32::MAX), |a, p| a.min(p.truncate()));
                        let max = s.iter().fold(Vec2::splat(f32::MIN), |a, p| a.max(p.truncate()));
                        let area = (s[1].x - s[0].x) * (s[2].y - s[0].y) - (s[2].x - s[0].x) * (s[1].y - s[0].y);
                        if area.abs() < 1e-9 || !area.is_finite() {
                            continue;
                        }
                        let x0 = (min.x.floor().max(ox as f32)) as usize;
                        let y0 = (min.y.floor().max(oy as f32)) as usize;
                        let x1 = (max.x.ceil().min((ox + size) as f32 - 1.0)).max(0.0) as usize;
                        let y1 = (max.y.ceil().min((oy + size) as f32 - 1.0)).max(0.0) as usize;
                        for y in y0..=y1 {
                            for x in x0..=x1 {
                                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                                let w0 = ((s[1].x - px) * (s[2].y - py) - (s[2].x - px) * (s[1].y - py)) / area;
                                let w1 = ((s[2].x - px) * (s[0].y - py) - (s[0].x - px) * (s[2].y - py)) / area;
                                let w2 = 1.0 - w0 - w1;
                                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                                    continue;
                                }
                                let z = s[0].z * w0 + s[1].z * w1 + s[2].z * w2;
                                let k = y * canvas.w + x;
                                if z >= canvas.depth[k] {
                                    continue;
                                }
                                canvas.depth[k] = z;
                                let n = (world[idx[0]].1 * w0 + world[idx[1]].1 * w1 + world[idx[2]].1 * w2)
                                    .normalize_or_zero();
                                let mut c = base;
                                if !prim.colors.is_empty() {
                                    c *= prim.colors[idx[0]] * w0 + prim.colors[idx[1]] * w1 + prim.colors[idx[2]] * w2;
                                }
                                if let (Some(t), false) = (tex, prim.uv0.is_empty()) {
                                    let uv = prim.uv0[idx[0]] * w0 + prim.uv0[idx[1]] * w1 + prim.uv0[idx[2]] * w2;
                                    c *= sample(scene, t, uv);
                                }
                                let lambert = n.dot(light).abs() * 0.75 + 0.25;
                                let mut emissive = material.map_or(Vec3::ZERO, |m| m.emissive);
                                if let (Some(t), false) =
                                    (material.and_then(|m| m.emissive_texture), prim.uv0.is_empty())
                                {
                                    let uv = prim.uv0[idx[0]] * w0 + prim.uv0[idx[1]] * w1 + prim.uv0[idx[2]] * w2;
                                    emissive *= sample(scene, t.texture, uv).truncate();
                                }
                                let rgb = c.truncate() * lambert + emissive;
                                canvas.color[k] =
                                    rgb.to_array().map(|v| (v.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0) as u8);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A PNG with stored (uncompressed) deflate blocks: no dependency needed.
fn write_png(path: &str, w: usize, h: usize, rgb: &[[u8; 3]]) -> std::io::Result<()> {
    fn crc(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    let mut raw = Vec::with_capacity(h * (w * 3 + 1));
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            raw.extend(rgb[y * w + x]);
        }
    }
    let mut z = vec![0x78, 0x01];
    for (i, chunk) in raw.chunks(65535).enumerate() {
        let last = (i + 1) * 65535 >= raw.len();
        z.push(last as u8);
        z.extend((chunk.len() as u16).to_le_bytes());
        z.extend((!(chunk.len() as u16)).to_le_bytes());
        z.extend(chunk);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &v in &raw {
        a = (a + v as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend(((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], data: &[u8]| {
        out.extend((data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend(data);
        out.extend(&body);
        out.extend(crc(&body).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend((w as u32).to_be_bytes());
    ihdr.extend((h as u32).to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    std::fs::File::create(path)?.write_all(&out)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut opt = |name: &str| -> Option<String> {
        let i = args.iter().position(|a| a == name)?;
        args.remove(i);
        (i < args.len()).then(|| args.remove(i))
    };
    let cell: usize = opt("--cell").and_then(|v| v.parse().ok()).unwrap_or(256);
    let cols: usize = opt("--cols").and_then(|v| v.parse().ok()).unwrap_or(6);
    let pose_arg = opt("--pose");
    let out = args.remove(0);
    let files = args;
    let rows = files.len().div_ceil(cols).max(1);
    let (w, h) = (cell * cols.min(files.len()).max(1), cell * rows);
    let mut canvas = Canvas { w, color: vec![[40, 40, 46]; w * h], depth: vec![f32::INFINITY; w * h] };
    let limits = Limits { animations: pose_arg.is_some(), ..Limits::thumbnail() };
    for (i, f) in files.iter().enumerate() {
        let path = std::path::Path::new(f);
        let Ok(bytes) = std::fs::read(path) else { continue };
        let Some(format) = sniff(path, &bytes[..bytes.len().min(4096)]) else { continue };
        let (cx, cy) = ((i % cols) * cell, (i / cols) * cell);
        match load(format, path, &bytes, &limits) {
            Ok(Asset::Scene(scene)) => {
                let mut pose = Pose::rest(&scene);
                if let Some((clip, t)) = pose_arg.as_deref().and_then(|p| p.split_once(':')) {
                    let clip = scene
                        .animations
                        .iter()
                        .position(|a| a.name == clip)
                        .or_else(|| clip.parse().ok())
                        .filter(|&c| c < scene.animations.len());
                    if let (Some(c), Ok(t)) = (clip, t.parse::<f32>()) {
                        pose.sample(&scene, c, t);
                        pose.update_world(&scene);
                    }
                }
                render(&scene, &pose, &mut canvas, cx, cy, cell);
                println!("{i:3}: {f} ({} tris, {} textures)", scene.triangle_count(), scene.textures.len());
            }
            Ok(_) => println!("{i:3}: {f} (not a scene)"),
            Err(e) => println!("{i:3}: {f} FAILED {e}"),
        }
    }
    write_png(&out, w, h, &canvas.color).expect("write png");
}
