//! Turning brush faces, patches and displacements into a level scene, and
//! the pieces both map formats share: the tinted checker that stands in
//! for a missing texture, and the materials of an editor-like look.
//!
//! Source maps carry no lightmaps, so levels are lit by Slerp's own lights
//! (the environment and a key light), as level editors light their 3D
//! views, rather than drawn black.

use crate::{dir_to_y_up, to_y_up};
use slerp_scene::glam::{DVec3, Vec2, Vec3, Vec4};
use slerp_scene::textures::{ColorSpace, Texture};
use slerp_scene::{Filter, Limits, LoadError, Material, Mesh, Primitive, Sampler, Scene, TexRef, Wrap};
use std::collections::HashMap;

/*┌─────────────────╮
| Geometry        |
└─────────────────╯*/
//region Geometry

/// Something to draw, in map space (Z up, map units): a convex polygon
/// (when `indices` is empty), or triangles.
#[derive(Clone, Debug, Default)]
pub(crate) struct Surface {
    /// Index into the scene's materials.
    pub material: usize,
    pub positions: Vec<DVec3>,
    /// Texture coordinates in texture widths.
    pub uvs: Vec<Vec2>,
    /// One per position, in map space.
    pub normals: Vec<Vec3>,
    /// Empty, or one linear RGBA colour per position.
    pub colors: Vec<Vec4>,
    /// Triangles wound anticlockwise seen from the front; empty for a
    /// polygon, which is fanned in whichever direction faces along its
    /// normal.
    pub indices: Vec<u32>,
}

impl Surface {
    fn triangles(&self) -> u64 {
        if self.indices.is_empty() {
            self.positions.len().saturating_sub(2) as u64
        } else {
            self.indices.len() as u64 / 3
        }
    }
}

/// A node of the level: the world, an editor layer, or a brush entity.
pub(crate) struct Group {
    pub name: String,
    pub visible: bool,
    pub surfaces: Vec<Surface>,
}

/// Adds one node per group (with one primitive per material) to `scene`,
/// converting to Y-up metres.  Fails when the whole level has more
/// triangles than `limits` allow.
pub(crate) fn add_groups(scene: &mut Scene, groups: Vec<Group>, limits: &Limits) -> Result<u64, LoadError> {
    let triangles: u64 = groups.iter().flat_map(|g| &g.surfaces).map(Surface::triangles).sum();
    if triangles > limits.max_triangles {
        return Err(LoadError::TooLarge(format!("{triangles} triangles")));
    }
    for group in groups {
        let mut prims: HashMap<usize, Primitive> = HashMap::new();
        for s in group.surfaces {
            let n = s.positions.len();
            if n < 3 || s.uvs.len() != n || s.normals.len() != n || !(s.colors.is_empty() || s.colors.len() == n) {
                continue;
            }
            let prim = prims
                .entry(s.material)
                .or_insert_with(|| Primitive { material: Some(s.material), ..Default::default() });
            let first = prim.positions.len() as u32;
            // Colours are all or nothing per primitive.
            if !s.colors.is_empty() && prim.colors.len() < prim.positions.len() {
                prim.colors.resize(prim.positions.len(), Vec4::ONE);
            }
            prim.positions.extend(s.positions.iter().map(|p| to_y_up(p.as_vec3())));
            prim.normals.extend(s.normals.iter().map(|&v| dir_to_y_up(v)));
            prim.uv0.extend_from_slice(&s.uvs);
            if !s.colors.is_empty() {
                prim.colors.extend_from_slice(&s.colors);
            } else if !prim.colors.is_empty() {
                prim.colors.resize(prim.positions.len(), Vec4::ONE);
            }
            if s.indices.is_empty() {
                // The Y-up conversion is a rotation, so a winding seen from
                // the front stays as it was.
                let forward = super::brush::newell(&s.positions).dot(s.normals[0].as_dvec3()) >= 0.0;
                for k in 1..n as u32 - 1 {
                    let (b, c) = if forward { (k, k + 1) } else { (k + 1, k) };
                    prim.indices.extend_from_slice(&[first, first + b, first + c]);
                }
            } else {
                prim.indices.extend(s.indices.iter().filter(|&&i| (i as usize) < n).map(|i| first + i));
                let extra = prim.indices.len() % 3;
                prim.indices.truncate(prim.indices.len() - extra);
            }
        }
        if prims.is_empty() {
            continue;
        }
        let mut primitives: Vec<Primitive> = prims.into_values().collect();
        primitives.sort_by_key(|p| p.material);
        let node = scene.add_mesh_node(Mesh { name: group.name, primitives, target_names: Vec::new() });
        scene.nodes[node].visible = group.visible;
    }
    Ok(triangles)
}

//endregion

/*┌─────────────────╮
| Materials       |
└─────────────────╯*/
//region Materials

/// A plain matte material for a level lit by Slerp's own lights.
pub(crate) fn matte(name: &str) -> Material {
    Material { name: name.to_string(), roughness: 1.0, specular: 0.2, base_color: Vec4::ONE, ..Default::default() }
}

/// The fill light a map starts with (`Scene::fill`): how much of a
/// surface's own colour it shows wherever no light reaches.  Levels are
/// mostly closed rooms, which the key light never enters; a fill keeps
/// them readable, as an editor's 3D view is.  The viewer lets the user
/// change it.
pub(crate) const FILL: f32 = 0.5;

/// Quake's and Half-Life's 8-bit textures are dark because their engines
/// doubled the light falling on them ("overbright"); doubling their colour
/// instead shows them as bright as the games do.
pub(crate) const OVERBRIGHT: f32 = 2.0;

/// A 64x64 checker of two tones, tinted by a hash of `name` so that
/// neighbouring missing textures can be told apart, as editors show them.
pub(crate) fn checker(name: &str) -> Texture {
    // FNV-1a over the lowercase name.
    let mut h: u32 = 0x811c_9dc5;
    for b in name.to_ascii_lowercase().bytes() {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    let hue = (h % 360) as f32;
    let tone = |v: f32| -> [u8; 3] {
        // HSV with a little saturation, so the checker stays neutral.
        let (s, c) = (0.3, v);
        let x = c * s * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
        let cs = c * s;
        let (r, g, b) = match (hue / 60.0) as u32 {
            0 => (cs, x, 0.0),
            1 => (x, cs, 0.0),
            2 => (0.0, cs, x),
            3 => (0.0, x, cs),
            4 => (x, 0.0, cs),
            _ => (cs, 0.0, x),
        };
        let m = c - cs;
        [((r + m) * 255.0) as u8, ((g + m) * 255.0) as u8, ((b + m) * 255.0) as u8]
    };
    let (light, dark) = (tone(0.72), tone(0.55));
    let mut rgba = Vec::with_capacity(64 * 64 * 4);
    for y in 0..64 {
        for x in 0..64 {
            let c = if (x / 16 + y / 16) % 2 == 0 { light } else { dark };
            rgba.extend_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    let mut t = Texture::from_rgba8(64, 64, rgba, ColorSpace::Srgb);
    t.source.container = "missing texture".into();
    t.source.format = name.to_string();
    t
}

/// "3 textures not found: a, b, c", listing at most eight names.
pub(crate) fn missing_warning(what: &str, names: &mut Vec<String>) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    names.sort_unstable();
    names.dedup();
    let shown = names.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
    let more = if names.len() > 8 { format!(" and {} more", names.len() - 8) } else { String::new() };
    Some(format!("{} {what} not found: {shown}{more}", names.len()))
}

/// A sampler that repeats in both directions with mipmaps, for textures
/// tiled across faces.
pub(crate) fn tiled() -> Sampler {
    Sampler { wrap_u: Wrap::Repeat, wrap_v: Wrap::Repeat, mag: Filter::Linear, min: Filter::Linear, mipmaps: true }
}

/// A texture reference with the tiling sampler.
pub(crate) fn tex_ref(texture: usize) -> TexRef {
    TexRef { sampler: tiled(), ..TexRef::new(texture) }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkers_differ_by_name_and_stay_neutral() {
        let a = checker("wall1");
        let b = checker("WALL1");
        let c = checker("floor");
        let px = |t: &Texture| match t.base() {
            slerp_scene::textures::Pixels::Rgba8(v) => v[..4].to_vec(),
            _ => panic!(),
        };
        assert_eq!(px(&a), px(&b));
        assert_ne!(px(&a), px(&c));
        let p = px(&a);
        let (lo, hi) = (*p[..3].iter().min().unwrap(), *p[..3].iter().max().unwrap());
        assert!(hi - lo < 70 && lo > 100, "{p:?}");
    }

    #[test]
    fn polygons_face_along_their_normal() {
        let mut scene = Scene::default();
        let square = vec![DVec3::ZERO, DVec3::X, DVec3::new(1.0, 1.0, 0.0), DVec3::Y];
        let up = |positions: Vec<DVec3>| Surface {
            material: 0,
            uvs: vec![Vec2::ZERO; positions.len()],
            normals: vec![Vec3::Z; positions.len()],
            positions,
            ..Default::default()
        };
        let mut reversed = square.clone();
        reversed.reverse();
        let groups = vec![Group { name: "g".into(), visible: true, surfaces: vec![up(square), up(reversed)] }];
        assert_eq!(add_groups(&mut scene, groups, &Limits::viewer()).unwrap(), 4);
        let p = &scene.meshes[0].primitives[0];
        for tri in p.indices.chunks(3) {
            let [a, b, c] = [0, 1, 2].map(|k| p.positions[tri[k] as usize]);
            let n = (b - a).cross(c - a);
            // Map +Z is Y-up +Y.
            assert!(n.y > 0.0, "{tri:?}");
        }
        let tiny = Limits { max_triangles: 1, ..Limits::viewer() };
        let groups = vec![Group {
            name: "g".into(),
            visible: true,
            surfaces: vec![up(vec![DVec3::ZERO, DVec3::X, DVec3::Y, DVec3::ONE])],
        }];
        assert!(matches!(add_groups(&mut Scene::default(), groups, &tiny), Err(LoadError::TooLarge(_))));
    }
}
