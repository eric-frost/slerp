//! Quake 3's models (MD3, magic "IDP3", version 15): named surfaces, each
//! with its own vertices, triangles, texture coordinates and shader names,
//! and frames that store every vertex again as 16-bit positions (1/64 of a
//! unit) with packed normals.  Tags (attachment points) are ignored.
//!
//! Each surface becomes a primitive with a morph target per frame, and
//! named runs of frames become clips (see `morph`).  Frame 0 is the rest
//! pose.

use crate::morph::{self, Frames};
use crate::read::{Bytes, Result, cap};
use crate::{dir_to_y_up, find_in, find_skin, load_skin, read_neighbour, to_y_up};
use slerp_scene::glam::{Vec2, Vec3, Vec4};
use slerp_scene::{Info, Limits, LoadError, Material, Mesh, Primitive, Scene, TexRef};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

const MAX_FRAMES: usize = 1 << 16;
const MAX_SURFACES: usize = 1024;
const MAX_VERTICES: usize = 1 << 20;
const MAX_SHADERS: usize = 256;

pub fn load(path: &Path, data: &[u8], limits: &Limits) -> Result<Scene> {
    let b = Bytes::new(data);
    if !b.starts_with(b"IDP3") || b.i32(4)? != 15 {
        return Err(LoadError::NotMine);
    }
    let frame_count = cap(b.index(76)?, MAX_FRAMES, "frames")?;
    let surface_count = cap(b.index(84)?, MAX_SURFACES, "surfaces")?;
    let (ofs_frames, ofs_surfaces) = (b.index(92)?, b.index(100)?);
    if frame_count == 0 || surface_count == 0 {
        return Err(LoadError::invalid("the model has no geometry"));
    }
    let frame_table = Bytes::new(b.array(ofs_frames, frame_count, 56)?);
    let names: Vec<String> = (0..frame_count).map(|f| frame_table.name(f * 56 + 40, 16)).collect::<Result<_>>()?;

    // 1. Surfaces.  Read the headers first so the triangle limit and the
    //    animation budget are known before any vertex is converted.
    struct Surface<'a> {
        name: String,
        b: Bytes<'a>,
        verts: usize,
        tris: usize,
        ofs: [usize; 4],
        shaders: Vec<String>,
    }
    let mut surfaces = Vec::with_capacity(surface_count);
    let mut at = ofs_surfaces;
    let (mut total_tris, mut total_verts) = (0u64, 0usize);
    for _ in 0..surface_count {
        let head = b.sub(at, 108)?;
        if head.slice(0, 4)? != b"IDP3" {
            return Err(LoadError::invalid("a surface lacks its IDP3 tag"));
        }
        let end = head.index(104)?;
        let s = b.sub(at, end.max(108))?;
        if s.index(72)? != frame_count {
            return Err(LoadError::invalid("a surface has a different frame count from the model"));
        }
        let shader_count = cap(s.index(76)?, MAX_SHADERS, "shaders")?;
        let verts = cap(s.index(80)?, MAX_VERTICES, "vertices")?;
        let tris = s.index(84)?;
        let ofs = [s.index(88)?, s.index(92)?, s.index(96)?, s.index(100)?];
        let shaders = (0..shader_count).map(|i| s.name(ofs[1] + 68 * i, 64)).collect::<Result<Vec<_>>>()?;
        total_tris += tris as u64;
        total_verts += verts;
        surfaces.push(Surface { name: s.name(4, 64)?, b: s, verts, tris, ofs, shaders });
        at += end.max(108);
    }
    if total_tris > limits.max_triangles {
        return Err(LoadError::TooLarge(format!("{total_tris} triangles")));
    }
    let want_all = limits.animations && frame_count.saturating_mul(total_verts) <= morph::MAX_FRAME_VERTICES;
    let used_frames = if want_all { frame_count } else { 1 };

    // 2. Skins: a `.skin` file next to the model maps surface names to
    //    images; otherwise each surface's shader name is the image.
    let mut info = Info { format: "Quake 3 MD3".into(), ..Default::default() };
    let skin_map = if limits.neighbours { skin_file(path) } else { HashMap::new() };
    let mut scene = Scene::default();
    let mut loaded: HashMap<String, Option<usize>> = HashMap::new();
    let mut primitives = Vec::with_capacity(surfaces.len());
    let mut target_names = Vec::new();
    for s in &surfaces {
        // 3. Geometry of the rest pose and, when animating, every frame.
        let tri_data = Bytes::new(s.b.array(s.ofs[0], s.tris, 12)?);
        let mut indices = Vec::with_capacity(s.tris * 3);
        for t in 0..s.tris {
            let tri = [tri_data.u32(t * 12)?, tri_data.u32(t * 12 + 4)?, tri_data.u32(t * 12 + 8)?];
            if tri.iter().any(|&v| v as usize >= s.verts) {
                return Err(LoadError::invalid(format!("surface {} indexes past its vertices", s.name)));
            }
            // Quake 3 winds triangles clockwise seen from outside.
            indices.extend_from_slice(&[tri[0], tri[2], tri[1]]);
        }
        let st = Bytes::new(s.b.array(s.ofs[2], s.verts, 8)?);
        let uv0 =
            (0..s.verts).map(|v| Ok(Vec2::new(st.f32(v * 8)?, st.f32(v * 8 + 4)?))).collect::<Result<Vec<_>>>()?;
        let xyz = Bytes::new(s.b.array(s.ofs[3], s.verts * frame_count, 8)?);
        let mut frames = Frames::default();
        for (f, name) in names.iter().enumerate().take(used_frames) {
            let mut positions = Vec::with_capacity(s.verts);
            let mut normals = Vec::with_capacity(s.verts);
            for v in 0..s.verts {
                let at = (f * s.verts + v) * 8;
                let p = Vec3::new(xyz.i16(at)? as f32, xyz.i16(at + 2)? as f32, xyz.i16(at + 4)? as f32) / 64.0;
                positions.push(to_y_up(p));
                normals.push(dir_to_y_up(decode_normal(xyz.u16(at + 6)?)));
            }
            frames.positions.push(positions);
            frames.normals.push(normals);
            frames.names.push(name.clone());
        }
        let identity: Vec<u32> = (0..s.verts as u32).collect();
        let mut prim = Primitive {
            positions: frames.positions[0].clone(),
            normals: frames.normals[0].clone(),
            uv0,
            indices,
            ..Default::default()
        };
        if want_all && frame_count > 1 {
            prim.targets = frames.targets(&identity);
            target_names = frames.names.clone();
        }

        // 4. The surface's material and image.
        let image = skin_map.get(&s.name.to_ascii_lowercase()).cloned().or_else(|| s.shaders.first().cloned());
        let texture = match &image {
            Some(img) if limits.neighbours => *loaded.entry(img.to_ascii_lowercase()).or_insert_with(|| {
                let file = find_skin(path, img);
                match file.map(|f| load_skin(&f, limits.max_texture_size)) {
                    Some(Ok(t)) => {
                        scene.textures.push(Arc::new(t));
                        Some(scene.textures.len() - 1)
                    }
                    Some(Err(e)) => {
                        info.warnings.push(format!("skin {e}"));
                        None
                    }
                    None => {
                        info.warnings.push(format!("skin not found: {img}"));
                        None
                    }
                }
            }),
            _ => None,
        };
        scene.materials.push(Material {
            name: s.name.clone(),
            base_color: if texture.is_some() { Vec4::ONE } else { Material::default().base_color },
            base_color_texture: texture.map(TexRef::new),
            roughness: 0.9,
            specular: 0.3,
            ..Default::default()
        });
        prim.material = Some(scene.materials.len() - 1);
        primitives.push(prim);
    }

    // 5. One node holding every surface, with clips over its frames.
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("model").to_string();
    let animated = !target_names.is_empty();
    let node = scene.add_mesh_node(Mesh { name: name.clone(), primitives, target_names });
    if animated {
        scene.nodes[node].weights = vec![0.0; frame_count];
        scene.animations = morph::clips(&names, None, node);
    }
    scene.name = name;
    info.details.push(("Surfaces".into(), surfaces.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")));
    info.details.push(("Frames".into(), frame_count.to_string()));
    if limits.animations && !want_all {
        info.warnings.push("too many frames to animate; showing the first".into());
    }
    scene.info = info;
    Ok(scene)
}

/// Unpacks Quake 3's normal: latitude in the high byte and longitude in the
/// low byte, each a fraction of a full turn.
fn decode_normal(packed: u16) -> Vec3 {
    let lat = (packed >> 8) as f32 * std::f32::consts::TAU / 256.0;
    let lng = (packed & 0xff) as f32 * std::f32::consts::TAU / 256.0;
    Vec3::new(lat.cos() * lng.sin(), lat.sin() * lng.sin(), lng.cos())
}

/// Reads `<model>.skin` or `<model>_default.skin`: lines of
/// `surface,image` (tags and blank images skipped), keyed by lowercase
/// surface name.
fn skin_file(model: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let (Some(dir), Some(stem)) = (model.parent(), model.file_stem().and_then(|s| s.to_str())) else { return out };
    let Some(file) = find_in(dir, &format!("{stem}.skin")).or_else(|| find_in(dir, &format!("{stem}_default.skin")))
    else {
        return out;
    };
    let Some(bytes) = read_neighbour(&file) else { return out };
    for line in String::from_utf8_lossy(&bytes).lines() {
        let Some((surface, image)) = line.split_once(',') else { continue };
        let (surface, image) = (surface.trim(), image.trim());
        if surface.is_empty() || image.is_empty() || surface.starts_with("tag_") {
            continue;
        }
        out.insert(surface.to_ascii_lowercase(), image.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{self, asset};

    /// A two-frame tetrahedron with one surface, its second frame moved up
    /// by 64 units, frames named "bob1" and "bob2".
    fn tetrahedron() -> Vec<u8> {
        let verts: [[i16; 3]; 4] = [[0, 0, 0], [64, 0, 0], [0, 64, 0], [0, 0, 64]];
        let tris: [[u32; 3]; 4] = [[0, 1, 2], [0, 3, 1], [0, 2, 3], [1, 3, 2]];
        let frames = 2;
        let mut surf = b"IDP3".to_vec();
        let mut name = [0u8; 64];
        name[..4].copy_from_slice(b"body");
        surf.extend_from_slice(&name);
        let tri_ofs = 108;
        let shader_ofs = tri_ofs + 12 * tris.len();
        let st_ofs = shader_ofs + 68;
        let xyz_ofs = st_ofs + 8 * verts.len();
        let end = xyz_ofs + 8 * verts.len() * frames;
        for v in [0, frames, 1, verts.len(), tris.len(), tri_ofs, shader_ofs, st_ofs, xyz_ofs, end] {
            surf.extend_from_slice(&(v as i32).to_le_bytes());
        }
        for t in tris {
            for i in t {
                surf.extend_from_slice(&i.to_le_bytes());
            }
        }
        surf.extend_from_slice(&[0u8; 68]);
        for _ in 0..verts.len() {
            surf.extend_from_slice(&[0u8; 8]);
        }
        for f in 0..frames {
            for v in verts {
                for (c, coord) in v.iter().enumerate() {
                    let lift = if c == 2 { f as i16 * 64 * 64 } else { 0 };
                    surf.extend_from_slice(&(coord * 64 + lift).to_le_bytes());
                }
                surf.extend_from_slice(&0u16.to_le_bytes());
            }
        }
        let mut out = b"IDP3".to_vec();
        out.extend_from_slice(&15i32.to_le_bytes());
        out.extend_from_slice(&[0u8; 64]);
        let ofs_frames = 108;
        let ofs_surfaces = ofs_frames + 56 * frames;
        for v in [0, frames, 0, 1, 0, ofs_frames, ofs_surfaces, ofs_surfaces, ofs_surfaces + surf.len()] {
            out.extend_from_slice(&(v as i32).to_le_bytes());
        }
        for f in 0..frames {
            out.extend_from_slice(&[0u8; 40]);
            let mut n = [0u8; 16];
            n[..4].copy_from_slice(format!("bob{}", f + 1).as_bytes());
            out.extend_from_slice(&n);
        }
        out.extend_from_slice(&surf);
        out
    }

    #[test]
    fn frames_become_targets_and_a_clip() {
        let bytes = tetrahedron();
        let s = load(Path::new("/nonexistent/t.md3"), &bytes, &Limits::viewer()).unwrap();
        let prim = &s.meshes[0].primitives[0];
        assert_eq!(prim.targets.len(), 2);
        assert!((prim.targets[1].positions[0] - Vec3::new(0.0, 64.0 * 0.0254, 0.0)).length() < 1e-5);
        assert_eq!(s.animations.len(), 1);
        assert_eq!(s.animations[0].name, "bob");
        assert!(testutil::outwardness(&s) > 0.3, "{}", testutil::outwardness(&s));
        let thumb = load(Path::new("/nonexistent/t.md3"), &bytes, &Limits::thumbnail()).unwrap();
        assert!(thumb.animations.is_empty() && thumb.meshes[0].primitives[0].targets.is_empty());
        for (i, m) in testutil::mutations(&bytes).iter().enumerate() {
            let r = std::panic::catch_unwind(|| {
                load(Path::new("/nonexistent/t.md3"), m, &Limits::viewer()).map(|s| s.triangle_count())
            });
            assert!(r.is_ok(), "mutation {i} panicked");
        }
    }

    #[test]
    fn real_models_with_shader_images() {
        for rel in [
            "quake-extra/md3/watercan.md3",
            "quake-extra/md3/q3root/models/mapobjects/kt_kubalwagon/european_fnt_v2.md3",
        ] {
            let Some(p) = asset(rel) else { return };
            let bytes = std::fs::read(&p).unwrap();
            let s = load(&p, &bytes, &Limits::viewer()).unwrap();
            eprintln!(
                "{rel}: {} tris, {} textures, outward {:.2}, warnings {:?}",
                s.triangle_count(),
                s.textures.len(),
                testutil::outwardness(&s),
                s.info.warnings
            );
            assert!(!s.textures.is_empty());
            let img = testutil::render(&s, Vec3::new(-1.0, -0.6, -1.0), 512, false);
            let name = p.file_stem().unwrap().to_str().unwrap();
            testutil::write_png(Path::new(&format!("/tmp/slerp-quake/{name}.png")), 512, 512, &img);
        }
    }
}
