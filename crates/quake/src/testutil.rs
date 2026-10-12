//! Test helpers: finding downloaded test assets, writing PNGs, and a small
//! software rasteriser that draws a scene with its textures and lightmaps,
//! so geometry, UVs and lightmaps can be checked without the GPU renderer.

use slerp_scene::glam::{Mat4, Vec2, Vec3, Vec4};
use slerp_scene::textures::Pixels;
use slerp_scene::{AlphaMode, Pose, Scene, Texture};
use std::path::{Path, PathBuf};

/// A test asset under `local/assets/`, or `None` (and a note on stderr)
/// when it hasn't been downloaded, so tests skip instead of failing.
pub fn asset(relative: &str) -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets");
    let p = root.join(relative);
    if p.exists() {
        Some(p)
    } else {
        eprintln!("skipping: {} is missing", p.display());
        None
    }
}

/// A path in a checkout of chalk (a game whose maps and WADs some tests read)
/// in the folder beside this repository's.
macro_rules! chalk {
    ($path:literal) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../../chalk/", $path)
    };
}
pub(crate) use chalk;

/// A file outside the repository, such as chalk's maps, or `None`.
pub fn external(path: &str) -> Option<PathBuf> {
    let p = PathBuf::from(path);
    if p.exists() {
        Some(p)
    } else {
        eprintln!("skipping: {path} is missing");
        None
    }
}

/// Writes RGBA pixels as an uncompressed PNG.
pub fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(path, png_bytes(width, height, rgba)).unwrap();
}

/// RGBA pixels as an uncompressed PNG file's bytes.
pub fn png_bytes(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
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
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let c = crc(&out[start..]);
        out.extend_from_slice(&c.to_be_bytes());
    }
    let mut raw = Vec::new();
    for row in rgba.chunks_exact(width as usize * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    // zlib with stored blocks.
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, b) in blocks.iter().enumerate() {
        z.push((i + 1 == blocks.len()) as u8);
        z.extend_from_slice(&(b.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(b.len() as u16)).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut bsum) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        bsum = (bsum + a) % 65521;
    }
    z.extend_from_slice(&((bsum << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// A zip archive of `(path, data, deflated)` entries, as tools write
/// them: local headers, then the central directory.
pub fn zip(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut dir = Vec::new();
    for (path, data, deflate) in entries {
        let body = if *deflate { miniz_oxide::deflate::compress_to_vec(data, 6) } else { data.to_vec() };
        let method: u16 = if *deflate { 8 } else { 0 };
        let at = out.len() as u32;
        let header = |sig: &[u8], central: bool| {
            let mut h = sig.to_vec();
            if central {
                h.extend_from_slice(&20u16.to_le_bytes());
            }
            h.extend_from_slice(&20u16.to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes());
            h.extend_from_slice(&method.to_le_bytes());
            h.extend_from_slice(&[0; 8]);
            h.extend_from_slice(&(body.len() as u32).to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(path.len() as u16).to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes());
            if central {
                // Comment length, disk, internal and external attributes.
                h.extend_from_slice(&[0; 10]);
                h.extend_from_slice(&at.to_le_bytes());
            }
            h.extend_from_slice(path.as_bytes());
            h
        };
        out.extend(header(b"PK\x03\x04", false));
        out.extend_from_slice(&body);
        dir.extend(header(b"PK\x01\x02", true));
    }
    let dir_at = out.len() as u32;
    out.extend_from_slice(&dir);
    out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(dir.len() as u32).to_le_bytes());
    out.extend_from_slice(&dir_at.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn sample(tex: &Texture, uv: Vec2) -> Vec4 {
    let (w, h) = (tex.width() as i64, tex.height() as i64);
    let x = ((uv.x * w as f32).floor() as i64).rem_euclid(w);
    let y = ((uv.y * h as f32).floor() as i64).rem_euclid(h);
    let i = (y * w + x) as usize * 4;
    match tex.base() {
        Pixels::Rgba8(p) => Vec4::new(p[i] as f32, p[i + 1] as f32, p[i + 2] as f32, p[i + 3] as f32) / 255.0,
        Pixels::Rgba32F(p) => Vec4::new(p[i], p[i + 1], p[i + 2], p[i + 3]),
        Pixels::Blocks(..) => Vec4::ONE,
    }
}

/// Draws `scene` in its rest pose with an orthographic camera looking
/// along `dir`, sampling base textures and lightmaps (or simple shading
/// when there's no lightmap), on a transparent background.  Skinned meshes
/// are skinned on the CPU.  Faces pointing away are culled when `cull`.
pub fn render(scene: &Scene, dir: Vec3, size: u32, cull: bool) -> Vec<u8> {
    let pose = Pose::rest(scene);
    let view = slerp_scene::glam::camera::rh::view::look_to_mat4(
        Vec3::ZERO,
        dir.normalize(),
        if dir.normalize().y.abs() > 0.99 { Vec3::NEG_Z } else { Vec3::Y },
    );

    // 1. World-space triangles with their attributes.
    struct Tri {
        p: [Vec3; 3],
        uv0: [Vec2; 3],
        uv1: [Vec2; 3],
        n: Vec3,
        mat: Option<usize>,
    }
    let mut tris = Vec::new();
    for (ni, node) in scene.nodes.iter().enumerate() {
        let (Some(mi), true) = (node.mesh, node.visible) else { continue };
        let joints = node.skin.map(|s| pose.joint_matrices(scene, s));
        for prim in &scene.meshes[mi].primitives {
            let world: Vec<Vec3> = prim
                .positions
                .iter()
                .enumerate()
                .map(|(i, &p)| match (&joints, prim.is_skinned()) {
                    (Some(j), true) => {
                        let (ji, w) = (prim.joints[i], prim.weights[i]);
                        let mut m = Mat4::ZERO;
                        for k in 0..4 {
                            if w[k] != 0.0 {
                                m += j[ji[k] as usize] * w[k];
                            }
                        }
                        m.transform_point3(p)
                    }
                    _ => pose.world[ni].transform_point3(p),
                })
                .collect();
            for t in prim.indices.chunks_exact(3) {
                let ix = [t[0] as usize, t[1] as usize, t[2] as usize];
                let p = ix.map(|i| world[i]);
                let get = |v: &Vec<Vec2>, i: usize| v.get(i).copied().unwrap_or(Vec2::ZERO);
                tris.push(Tri {
                    p,
                    uv0: ix.map(|i| get(&prim.uv0, i)),
                    uv1: ix.map(|i| get(&prim.uv1, i)),
                    n: (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero(),
                    mat: prim.material,
                });
            }
        }
    }

    // 2. Fit the view to the projected bounds.
    let (mut lo, mut hi) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
    for t in &tris {
        for p in t.p {
            let v = view.transform_point3(p);
            lo = lo.min(v.truncate());
            hi = hi.max(v.truncate());
        }
    }
    let span = (hi - lo).max_element().max(1e-6);
    let scale = (size as f32 - 8.0) / span;
    let offset = Vec2::new(size as f32, size as f32) * 0.5 - (lo + hi) * 0.5 * scale;
    let to_screen = |p: Vec3| {
        let v = view.transform_point3(p);
        Vec3::new(v.x * scale + offset.x, size as f32 - (v.y * scale + offset.y), v.z)
    };

    // 3. Rasterise with a depth buffer.
    let mut color = vec![0u8; (size * size * 4) as usize];
    let mut depth = vec![f32::NEG_INFINITY; (size * size) as usize];
    let light = Vec3::new(0.4, 0.8, 0.3).normalize();
    for t in &tris {
        let s = t.p.map(to_screen);
        let area = (s[1].x - s[0].x) * (s[2].y - s[0].y) - (s[2].x - s[0].x) * (s[1].y - s[0].y);
        // Screen y points down, so counter-clockwise front faces have
        // negative area here.
        if area.abs() < 1e-9 || (cull && area > 0.0) {
            continue;
        }
        let mat = t.mat.and_then(|m| scene.materials.get(m));
        let (minx, maxx) = (
            s.iter().map(|v| v.x).fold(f32::MAX, f32::min).max(0.0) as u32,
            s.iter().map(|v| v.x).fold(f32::MIN, f32::max).min(size as f32 - 1.0) as u32,
        );
        let (miny, maxy) = (
            s.iter().map(|v| v.y).fold(f32::MAX, f32::min).max(0.0) as u32,
            s.iter().map(|v| v.y).fold(f32::MIN, f32::max).min(size as f32 - 1.0) as u32,
        );
        for y in miny..=maxy {
            for x in minx..=maxx {
                let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                let w0 = ((s[1].x - p.x) * (s[2].y - p.y) - (s[2].x - p.x) * (s[1].y - p.y)) / area;
                let w1 = ((s[2].x - p.x) * (s[0].y - p.y) - (s[0].x - p.x) * (s[2].y - p.y)) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let z = s[0].z * w0 + s[1].z * w1 + s[2].z * w2;
                let di = (y * size + x) as usize;
                if z <= depth[di] {
                    continue;
                }
                let uv0 = t.uv0[0] * w0 + t.uv0[1] * w1 + t.uv0[2] * w2;
                let uv1 = t.uv1[0] * w0 + t.uv1[1] * w1 + t.uv1[2] * w2;
                let mut c = Vec4::new(0.7, 0.7, 0.7, 1.0);
                let mut lit = Vec3::splat(0.35 + 0.65 * t.n.dot(light).abs());
                if let Some(m) = mat {
                    c = m.base_color;
                    if let Some(tr) = m.base_color_texture {
                        c *= sample(&scene.textures[tr.texture], if tr.uv_set == 1 { uv1 } else { uv0 });
                    }
                    if matches!(m.alpha_mode, AlphaMode::Mask(_)) && c.w < 0.5 {
                        continue;
                    }
                    if let Some(lm) = m.lightmap_texture {
                        lit = sample(&scene.textures[lm.texture], uv1).truncate() * m.lightmap_scale;
                    }
                    if m.unlit {
                        lit = Vec3::ONE;
                    }
                    if let Some(e) = m.emissive_texture {
                        lit = lit.max(sample(&scene.textures[e.texture], uv0).truncate() * 4.0);
                    }
                }
                depth[di] = z;
                let rgb = (c.truncate() * lit).clamp(Vec3::ZERO, Vec3::ONE) * 255.0;
                color[di * 4..di * 4 + 4].copy_from_slice(&[rgb.x as u8, rgb.y as u8, rgb.z as u8, 255]);
            }
        }
    }
    color
}

/// Draws markers and a spawn point as small squares over a top-down render
/// made with `render(scene, -Y, size, ..)`.
pub fn overlay_points(scene: &Scene, color: &mut [u8], size: u32, points: &[(Vec3, [u8; 3])]) {
    let view = slerp_scene::glam::camera::rh::view::look_to_mat4(Vec3::ZERO, Vec3::NEG_Y, Vec3::NEG_Z);
    let pose = Pose::rest(scene);
    let b = scene.bounds(&pose);
    let corners: Vec<Vec3> = (0..8)
        .map(|c| {
            Vec3::new(
                if c & 1 == 0 { b.min.x } else { b.max.x },
                if c & 2 == 0 { b.min.y } else { b.max.y },
                if c & 4 == 0 { b.min.z } else { b.max.z },
            )
        })
        .collect();
    let (mut lo, mut hi) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
    for p in corners {
        let v = view.transform_point3(p).truncate();
        lo = lo.min(v);
        hi = hi.max(v);
    }
    let span = (hi - lo).max_element().max(1e-6);
    let scale = (size as f32 - 8.0) / span;
    let offset = Vec2::splat(size as f32) * 0.5 - (lo + hi) * 0.5 * scale;
    for (p, rgb) in points {
        let v = view.transform_point3(*p);
        let (cx, cy) = ((v.x * scale + offset.x) as i32, (size as f32 - (v.y * scale + offset.y)) as i32);
        for y in cy - 2..=cy + 2 {
            for x in cx - 2..=cx + 2 {
                if x >= 0 && y >= 0 && (x as u32) < size && (y as u32) < size {
                    let i = (y as u32 * size + x as u32) as usize * 4;
                    color[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
                }
            }
        }
    }
}

/// Sum over triangles of each face normal's agreement with the direction
/// from the mesh centre, as a fraction: near 1 when triangles wind
/// counter-clockwise seen from outside, near -1 when they're inside out.
pub fn outwardness(scene: &Scene) -> f32 {
    let (mut sum, mut total) = (0.0f32, 0.0f32);
    for mesh in &scene.meshes {
        for prim in &mesh.primitives {
            let c = prim.positions.iter().fold(Vec3::ZERO, |a, &p| a + p) / prim.positions.len().max(1) as f32;
            for t in prim.indices.chunks_exact(3) {
                let p = [prim.positions[t[0] as usize], prim.positions[t[1] as usize], prim.positions[t[2] as usize]];
                let n = (p[1] - p[0]).cross(p[2] - p[0]);
                let a = n.length();
                if a == 0.0 {
                    continue;
                }
                let centroid = (p[0] + p[1] + p[2]) / 3.0;
                sum += n.dot((centroid - c).normalize_or_zero());
                total += a;
            }
        }
    }
    if total > 0.0 { sum / total } else { 0.0 }
}

/// Every prefix of `bytes` and many single-byte corruptions of it, for
/// checking a loader never panics.  Cuts are spread over the file and
/// corruptions favour the header, where the offsets live.
pub fn mutations(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let n = bytes.len();
    let step = (n / 64).max(1);
    for cut in (0..n).step_by(step).chain([n.saturating_sub(1)]) {
        out.push(bytes[..cut].to_vec());
    }
    let mut seed = 0x1234_5678u32;
    let mut rand = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for i in 0..400 {
        let mut b = bytes.to_vec();
        if b.is_empty() {
            break;
        }
        let at = if i % 2 == 0 { rand() as usize % b.len().min(512) } else { rand() as usize % b.len() };
        b[at] = match i % 4 {
            0 => 0xff,
            1 => 0x7f,
            2 => 0,
            _ => rand() as u8,
        };
        out.push(b);
    }
    out
}
