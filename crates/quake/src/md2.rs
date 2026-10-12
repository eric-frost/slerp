//! Quake 2's models (MD2, magic "IDP2", version 8): texture coordinates and
//! triangles indexed separately from positions, and frames that store
//! every position again, quantised to bytes with a per-frame scale.  Skins
//! are separate PCX files named in the header.
//!
//! Frames become morph targets and named runs of frames become clips (see
//! `morph`).  Frame 0 is the rest pose.

use crate::morph::{self, Frames};
use crate::read::{Bytes, Result, cap};
use crate::{find_skin, guess_skin, load_skin, to_y_up};
use slerp_scene::glam::{Vec2, Vec3, Vec4};
use slerp_scene::{Info, Limits, LoadError, Material, Mesh, Primitive, Scene, TexRef};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

const MAX_VERTICES: usize = 1 << 20;
const MAX_FRAMES: usize = 1 << 16;
const MAX_SKINS: usize = 256;

pub fn load(path: &Path, data: &[u8], limits: &Limits) -> Result<Scene> {
    let b = Bytes::new(data);
    if !b.starts_with(b"IDP2") || b.i32(4)? != 8 {
        return Err(LoadError::NotMine);
    }
    let (skin_w, skin_h) = (b.i32(8)?.max(1) as f32, b.i32(12)?.max(1) as f32);
    let frame_size = b.index(16)?;
    let skin_count = cap(b.index(20)?, MAX_SKINS, "skins")?;
    let xyz_count = cap(b.index(24)?, MAX_VERTICES, "vertices")?;
    let st_count = cap(b.index(28)?, MAX_VERTICES, "texture coordinates")?;
    let tri_count = b.index(32)?;
    let frame_count = cap(b.index(40)?, MAX_FRAMES, "frames")?;
    let (ofs_skins, ofs_st, ofs_tris, ofs_frames) = (b.index(44)?, b.index(48)?, b.index(52)?, b.index(56)?);
    if tri_count as u64 > limits.max_triangles {
        return Err(LoadError::TooLarge(format!("{tri_count} triangles")));
    }
    if xyz_count == 0 || tri_count == 0 || frame_count == 0 {
        return Err(LoadError::invalid("the model has no geometry"));
    }
    if frame_size < 40 + xyz_count * 4 {
        return Err(LoadError::invalid("frames are too small for their vertices"));
    }
    let st = Bytes::new(b.array(ofs_st, st_count, 4)?);
    let tris = Bytes::new(b.array(ofs_tris, tri_count, 12)?);
    let frames_raw = Bytes::new(b.array(ofs_frames, frame_count, frame_size)?);

    // 1. Vertices are distinct (position, texture coordinate) pairs.
    let mut map: HashMap<(u16, u16), u32> = HashMap::new();
    let mut remap = Vec::new();
    let mut uv0 = Vec::new();
    let mut indices = Vec::with_capacity(tri_count * 3);
    let mut triangles = Vec::with_capacity(tri_count);
    for t in 0..tri_count {
        let mut out = [0u32; 3];
        let mut tri = [0u32; 3];
        for k in 0..3 {
            let xyz = tris.u16(t * 12 + 2 * k)?;
            let sti = tris.u16(t * 12 + 6 + 2 * k)?;
            if xyz as usize >= xyz_count || sti as usize >= st_count {
                return Err(LoadError::invalid(format!("triangle {t} indexes past the vertex lists")));
            }
            let next = remap.len() as u32;
            let v = *map.entry((xyz, sti)).or_insert_with(|| {
                remap.push(xyz as u32);
                next
            });
            if v == next {
                let s = st.i16(sti as usize * 4)? as f32;
                let tt = st.i16(sti as usize * 4 + 2)? as f32;
                uv0.push(Vec2::new(s / skin_w, tt / skin_h));
            }
            out[k] = v;
            tri[k] = xyz as u32;
        }
        // Quake 2 winds triangles clockwise seen from outside.
        indices.extend_from_slice(&[out[0], out[2], out[1]]);
        triangles.push([tri[0], tri[2], tri[1]]);
    }

    // 2. Frames: a scale and offset, a name, then the positions.
    let want_all = limits.animations && frame_count.saturating_mul(xyz_count) <= morph::MAX_FRAME_VERTICES;
    let mut frames = Frames::default();
    for f in 0..if want_all { frame_count } else { 1 } {
        let at = f * frame_size;
        let (scale, translate) = (frames_raw.vec3(at)?, frames_raw.vec3(at + 12)?);
        frames.names.push(frames_raw.name(at + 24, 16)?);
        let verts = frames_raw.slice(at + 40, xyz_count * 4)?;
        let positions: Vec<Vec3> = verts
            .chunks_exact(4)
            .map(|v| to_y_up(Vec3::new(v[0] as f32, v[1] as f32, v[2] as f32) * scale + translate))
            .collect();
        frames.normals.push(morph::smooth_normals(&positions, &triangles));
        frames.positions.push(positions);
    }
    let mut prim = Primitive {
        positions: remap.iter().map(|&v| frames.positions[0][v as usize]).collect(),
        normals: remap.iter().map(|&v| frames.normals[0][v as usize]).collect(),
        uv0,
        indices,
        material: Some(0),
        ..Default::default()
    };

    // 3. Skins: the files the header names, or a guess from the folder.
    let mut scene = Scene::default();
    let mut info = Info { format: "Quake 2 MD2".into(), ..Default::default() };
    let mut material = Material { name: "skin".into(), roughness: 1.0, specular: 0.2, ..Default::default() };
    let names: Vec<String> =
        (0..skin_count).filter_map(|i| b.name(ofs_skins + 64 * i, 64).ok()).filter(|n| !n.trim().is_empty()).collect();
    if limits.neighbours {
        let mut files: Vec<_> = names.iter().filter_map(|n| find_skin(path, n)).collect();
        if files.is_empty() {
            files.extend(guess_skin(path));
        }
        for file in files {
            match load_skin(&file, limits.max_texture_size) {
                Ok(t) => scene.textures.push(Arc::new(t)),
                Err(e) => info.warnings.push(format!("skin {e}")),
            }
        }
        if scene.textures.is_empty() {
            info.warnings.push(match names.first() {
                Some(n) => format!("skin not found: {n}"),
                None => "the model names no skin".into(),
            });
        }
    }
    if !scene.textures.is_empty() {
        material.base_color = Vec4::ONE;
        material.base_color_texture = Some(TexRef::new(0));
    }
    scene.materials.push(material);

    // 4. Animation.
    let animated = want_all && frames.len() > 1;
    if animated {
        prim.targets = frames.targets(&remap);
    }
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("model").to_string();
    let node = scene.add_mesh_node(Mesh {
        name: name.clone(),
        primitives: vec![prim],
        target_names: if animated { frames.names.clone() } else { Vec::new() },
    });
    if animated {
        scene.nodes[node].weights = vec![0.0; frames.len()];
        scene.animations = morph::clips(&frames.names, None, node);
    }
    scene.name = name;
    info.details.push(("Skins".into(), names.join(", ")));
    info.details.push(("Frames".into(), frame_count.to_string()));
    if limits.animations && !want_all {
        info.warnings.push("too many frames to animate; showing the first".into());
    }
    scene.info = info;
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{self, asset};

    fn load_asset(rel: &str, limits: &Limits) -> Option<Result<Scene>> {
        let p = asset(rel)?;
        let bytes = std::fs::read(&p).unwrap();
        Some(load(&p, &bytes, limits))
    }

    #[test]
    fn models_load_with_clips() {
        for name in ["faerie", "sydney", "dolphin", "horse"] {
            let Some(s) = load_asset(&format!("quake-extra/md2/{name}.md2"), &Limits::viewer()) else { return };
            let s = s.unwrap();
            let out = testutil::outwardness(&s);
            let clips: Vec<String> =
                s.animations.iter().map(|a| format!("{}:{}", a.name, a.channels[0].times.len())).collect();
            eprintln!(
                "{name}: {} tris, {} textures, outward {out:.2}, warnings {:?}, clips {}",
                s.triangle_count(),
                s.textures.len(),
                s.info.warnings,
                clips.join(" ")
            );
            assert!(out > 0.1, "{name} winds inside out ({out})");
        }
        let s = load_asset("quake-extra/md2/faerie.md2", &Limits::viewer()).unwrap().unwrap();
        assert!(s.animations.iter().any(|a| a.name == "stand"));
        assert!(s.animations.iter().any(|a| a.name == "run"));
    }

    #[test]
    fn malformed_files_fail_cleanly() {
        for name in ["malformed_unterminated_skin_name", "malformed_zero_numvertices"] {
            let Some(r) = load_asset(&format!("quake-extra/md2/{name}.md2"), &Limits::viewer()) else { return };
            assert!(r.is_err(), "{name}");
        }
        let Some(p) = asset("quake-extra/md2/flag.md2") else { return };
        let bytes = std::fs::read(&p).unwrap();
        let limits = Limits { neighbours: false, ..Limits::viewer() };
        for (i, m) in testutil::mutations(&bytes).iter().enumerate() {
            let r = std::panic::catch_unwind(|| load(&p, m, &limits).map(|s| s.triangle_count()));
            assert!(r.is_ok(), "mutation {i} panicked");
        }
    }

    #[test]
    fn debug_render() {
        let Some(Ok(s)) = load_asset("quake-extra/md2/faerie.md2", &Limits::viewer()) else { return };
        let img = testutil::render(&s, Vec3::new(-1.0, -0.3, -1.0), 512, true);
        testutil::write_png(Path::new("/tmp/slerp-quake/faerie.png"), 512, 512, &img);
    }
}
