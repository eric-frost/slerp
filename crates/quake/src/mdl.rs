//! Quake's alias models (MDL, magic "IDPO", version 6): 8-bit skins in the
//! Quake palette, texture coordinates with a seam trick, triangles, and
//! frames that store every vertex again, quantised to bytes.
//!
//! Frames become morph targets and named runs of frames become clips (see
//! `morph`).  Frame 0 is the rest pose.

use crate::morph::{self, Frames};
use crate::pixels::{self, QUAKE_PALETTE};
use crate::read::{Bytes, Result, cap};
use crate::to_y_up;
use slerp_scene::glam::{Vec2, Vec3};
use slerp_scene::{Info, Limits, LoadError, Material, Mesh, Primitive, Scene, TexRef};
use std::path::Path;
use std::sync::Arc;

const HEADER_BYTES: usize = 84;

/// Limits beyond anything Quake engines load, to stop corrupt counts early.
const MAX_SKINS: usize = 1024;
const MAX_VERTICES: usize = 1 << 20;
const MAX_FRAMES: usize = 1 << 16;

pub fn load(path: &Path, data: &[u8], limits: &Limits) -> Result<Scene> {
    let b = Bytes::new(data);
    if !b.starts_with(b"IDPO") || b.i32(4)? != 6 {
        return Err(LoadError::NotMine);
    }
    let scale = b.vec3(8)?;
    let origin = b.vec3(20)?;
    let skin_count = cap(b.index(48)?, MAX_SKINS, "skins")?;
    let (skin_w, skin_h) = (b.u32(52)?, b.u32(56)?);
    let vertex_count = cap(b.index(60)?, MAX_VERTICES, "vertices")?;
    let tri_count = b.index(64)?;
    let frame_count = cap(b.index(68)?, MAX_FRAMES, "frames")?;
    if tri_count as u64 > limits.max_triangles {
        return Err(LoadError::TooLarge(format!("{tri_count} triangles")));
    }
    if vertex_count == 0 || tri_count == 0 || frame_count == 0 {
        return Err(LoadError::invalid("the model has no geometry"));
    }

    // 1. Skins: single pictures, or groups that animate through several.
    let skin_bytes = if skin_count > 0 { pixels::check_size(skin_w, skin_h)? } else { 0 };
    let mut skins: Vec<&[u8]> = Vec::new();
    let mut at = HEADER_BYTES;
    for _ in 0..skin_count {
        let kind = b.i32(at)?;
        at += 4;
        if kind == 0 {
            skins.push(b.slice(at, skin_bytes)?);
            at += skin_bytes;
        } else {
            let n = cap(b.index(at)?, MAX_SKINS, "skins in a group")?;
            at = at.checked_add(4 + 4 * n).ok_or_else(|| LoadError::invalid("skin group overflows"))?;
            for _ in 0..n {
                skins.push(b.slice(at, skin_bytes)?);
                at += skin_bytes;
            }
        }
    }

    // 2. Texture coordinates and triangles.
    let st = Bytes::new(b.array(at, vertex_count, 12)?);
    at += vertex_count * 12;
    let tris = Bytes::new(b.array(at, tri_count, 16)?);
    at += tri_count * 16;

    // 3. Frames, single or grouped.  Thumbnails need only the first.
    let frame_bytes = 24 + vertex_count * 4;
    let want_all = limits.animations && frame_count.saturating_mul(vertex_count) <= morph::MAX_FRAME_VERTICES;
    let mut frame_data: Vec<(String, &[u8])> = Vec::new();
    let mut durations: Vec<f32> = Vec::new();
    'frames: for _ in 0..frame_count {
        let kind = b.i32(at)?;
        at += 4;
        if kind == 0 {
            let f = b.slice(at, frame_bytes)?;
            frame_data.push((crate::read::cstr(&f[8..24]), &f[24..]));
            durations.push(0.0);
            at += frame_bytes;
        } else {
            let n = cap(b.index(at)?, MAX_FRAMES, "frames in a group")?;
            let times = b.array(at + 12, n, 4)?;
            at = at.checked_add(12 + 4 * n).ok_or_else(|| LoadError::invalid("frame group overflows"))?;
            let mut last = 0.0;
            for k in 0..n {
                let f = b.slice(at, frame_bytes)?;
                // Group intervals are end times; keep each frame's length.
                let end = Bytes::new(times).f32(k * 4)?;
                durations.push((end - last).max(0.0));
                last = end;
                frame_data.push((crate::read::cstr(&f[8..24]), &f[24..]));
                at += frame_bytes;
                if !want_all {
                    break 'frames;
                }
            }
        }
        if !want_all {
            break;
        }
    }

    // 4. Vertices: a vertex on the skin's seam used by a triangle facing
    //    away takes its texture from the skin's right (back) half, so it is
    //    duplicated.
    let mut remap: Vec<u32> = Vec::new();
    let mut uvs: Vec<Vec2> = Vec::new();
    let mut slot = vec![u32::MAX; vertex_count * 2];
    let mut triangles: Vec<[u32; 3]> = Vec::with_capacity(tri_count);
    let mut indices: Vec<u32> = Vec::with_capacity(tri_count * 3);
    let (sw, sh) = (skin_w.max(1) as f32, skin_h.max(1) as f32);
    for t in 0..tri_count {
        let front = tris.i32(t * 16)? != 0;
        let mut tri = [0u32; 3];
        let mut out = [0u32; 3];
        for k in 0..3 {
            let v = tris.index(t * 16 + 4 + 4 * k)?;
            if v >= vertex_count {
                return Err(LoadError::invalid(format!("triangle {t} uses vertex {v} of {vertex_count}")));
            }
            let on_seam = st.i32(v * 12)? != 0;
            let back = (!front && on_seam) as usize;
            let key = v * 2 + back;
            if slot[key] == u32::MAX {
                slot[key] = remap.len() as u32;
                remap.push(v as u32);
                let s = st.i32(v * 12 + 4)? as f32 + if back == 1 { sw / 2.0 } else { 0.0 };
                let tt = st.i32(v * 12 + 8)? as f32;
                uvs.push(Vec2::new((s + 0.5) / sw, (tt + 0.5) / sh));
            }
            tri[k] = v as u32;
            out[k] = slot[key];
        }
        // Quake's triangles wind clockwise seen from outside.
        triangles.push([tri[0], tri[2], tri[1]]);
        indices.extend_from_slice(&[out[0], out[2], out[1]]);
    }

    // 5. Frame positions and normals for the model's own vertices.
    let mut frames = Frames::default();
    for (name, verts) in &frame_data {
        let positions: Vec<Vec3> = verts
            .chunks_exact(4)
            .map(|v| to_y_up(Vec3::new(v[0] as f32, v[1] as f32, v[2] as f32) * scale + origin))
            .collect();
        frames.normals.push(morph::smooth_normals(&positions, &triangles));
        frames.positions.push(positions);
        frames.names.push(name.clone());
    }
    let rest = &frames.positions[0];
    let mut prim = Primitive {
        positions: remap.iter().map(|&v| rest[v as usize]).collect(),
        normals: remap.iter().map(|&v| frames.normals[0][v as usize]).collect(),
        uv0: uvs,
        indices,
        material: Some(0),
        ..Default::default()
    };

    // 6. Skins and material.
    let mut scene = Scene::default();
    let mut material = Material { name: "skin".into(), roughness: 1.0, specular: 0.2, ..Default::default() };
    for (i, skin) in skins.iter().enumerate() {
        let rgba = pixels::expand(skin, skin_w, skin_h, QUAKE_PALETTE, None);
        let fmt = format!("skin {i} ({skin_w}x{skin_h} 8-bit)");
        scene.textures.push(Arc::new(pixels::texture(
            skin_w,
            skin_h,
            rgba,
            limits.max_texture_size,
            "Quake MDL",
            &fmt,
        )));
        if i == 0 {
            material.base_color = slerp_scene::glam::Vec4::ONE;
            material.base_color_texture = Some(TexRef::new(0));
            if let Some(fb) = pixels::fullbrights(skin, QUAKE_PALETTE, None) {
                scene.textures.push(Arc::new(pixels::texture(
                    skin_w,
                    skin_h,
                    fb,
                    limits.max_texture_size,
                    "Quake MDL",
                    "fullbright colours",
                )));
                material.emissive = Vec3::ONE;
                material.emissive_texture = Some(TexRef::new(scene.textures.len() - 1));
            }
        }
    }
    scene.materials.push(material);

    // 7. Animation: morph targets and clips.
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
        scene.animations = morph::clips(&frames.names, Some(&durations), node);
    }
    scene.name = name;
    let mut info = Info { format: "Quake MDL".into(), ..Default::default() };
    info.details.push(("Skins".into(), format!("{} ({skin_w}x{skin_h})", skins.len())));
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

    fn load_asset(rel: &str, limits: &Limits) -> Option<Scene> {
        let p = asset(rel)?;
        let bytes = std::fs::read(&p).unwrap();
        Some(load(&p, &bytes, limits).unwrap_or_else(|e| panic!("{rel}: {e}")))
    }

    #[test]
    fn libre_quake_models() {
        // Closed models, whose triangles should face outwards.
        for name in [
            "libquake/progs/player",
            "libquake/progs/soldier",
            "libquake/progs/dog",
            "libquake/progs/armor",
            "libquake/progs/shambler",
            "mdl/steg",
            "mdl/gijoe",
        ] {
            let Some(s) = load_asset(&format!("quake-extra/{name}.mdl"), &Limits::viewer()) else { return };
            let clips: Vec<String> =
                s.animations.iter().map(|a| format!("{}:{}", a.name, a.channels[0].times.len())).collect();
            let out = testutil::outwardness(&s);
            eprintln!(
                "{name}: {} tris, {} targets, outward {out:.2}, clips {}",
                s.triangle_count(),
                s.meshes[0].primitives[0].targets.len(),
                clips.join(" ")
            );
            assert!(out > 0.1, "{name} winds inside out ({out})");
        }
        // LibreQuake's frames are all named frameN; the GI Joe model's are
        // named by action.
        if let Some(s) = load_asset("quake-extra/mdl/gijoe.mdl", &Limits::viewer()) {
            assert!(s.animations.len() > 3);
        }
        let s = load_asset("quake-extra/libquake/progs/player.mdl", &Limits::viewer()).unwrap();
        let thumb = load_asset("quake-extra/libquake/progs/player.mdl", &Limits::thumbnail()).unwrap();
        assert!(thumb.animations.is_empty() && thumb.meshes[0].primitives[0].targets.is_empty());
        assert_eq!(thumb.meshes[0].primitives[0].positions, s.meshes[0].primitives[0].positions);
    }

    #[test]
    fn rest_pose_is_frame_zero_and_clips_play_frames() {
        let Some(s) = load_asset("quake-extra/mdl/gijoe.mdl", &Limits::viewer()) else { return };
        let mut pose = slerp_scene::Pose::rest(&s);
        assert!(pose.weights[0].iter().all(|&w| w == 0.0));
        let clip = &s.animations[1];
        pose.sample(&s, 1, clip.channels[0].times[1]);
        assert_eq!(pose.weights[0].iter().filter(|&&w| w == 1.0).count(), 1);
    }

    #[test]
    fn corrupt_models_never_panic() {
        let Some(p) = asset("quake-extra/libquake/progs/armor.mdl") else { return };
        let bytes = std::fs::read(&p).unwrap();
        for (i, m) in testutil::mutations(&bytes).iter().enumerate() {
            let r = std::panic::catch_unwind(|| load(&p, m, &Limits::viewer()).map(|s| s.triangle_count()));
            assert!(r.is_ok(), "mutation {i} panicked");
        }
        assert!(load(&p, &bytes[..83], &Limits::viewer()).is_err());
    }

    #[test]
    fn debug_render() {
        let Some(s) = load_asset("quake-extra/libquake/progs/shambler.mdl", &Limits::viewer()) else { return };
        let img = testutil::render(&s, Vec3::new(-1.0, -0.4, -1.0), 512, true);
        testutil::write_png(Path::new("/tmp/slerp-quake/shambler.png"), 512, 512, &img);
    }
}
