//! Row pictures of a scene's meshes, nodes and collections, drawn on the
//! GPU.  Making one has two halves.  [`Contents::piece_scene`] copies what
//! the piece shows out of the scene on screen into a small scene of its
//! own: each mesh where the scene puts it (skinned meshes posed at rest),
//! as roots of their own, so nothing else of the scene comes along.  It
//! runs on the window's thread, where the scene lives, and only copies the
//! piece's own geometry.  [`Contents::draw_piece`] then draws that scene
//! as a thumbnail, off the window's thread, with its textures shrunk to
//! what a row picture shows (once per texture, kept for later rows).

use super::pieces::{Piece, nodes_of};
use super::{Contents, ItemId, Source};
use glam::Mat4;
use slerp_scene::{Image8, Node, Scene, Texture, Transform};
use slerp_textures::{Level, Pixels, TextureKind};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// A piece with more triangles or vertices than this gets no row picture:
/// copying it would hold the window up.
pub const MAX_PIECE_SIZE: usize = 1_000_000;
/// The longest side of the textures a row picture is drawn with.
const TEXTURE_SIDE: u32 = 128;

/// The shrunk copy of each texture drawn so far, by index, with the
/// texture it was made from (a reload may put another one at the index).
pub(crate) type SmallTextures = HashMap<usize, (Arc<Texture>, Arc<Texture>)>;

impl Contents {
    /// Whether an item's row picture is drawn on the GPU from the scene
    /// ([`Contents::piece_scene`]) rather than made by
    /// [`Contents::thumbnail`].
    pub fn is_scene_piece(&self, id: ItemId) -> bool {
        matches!(
            self.item(id).map(|i| &i.source),
            Some(Source::Piece(Piece::Mesh(_) | Piece::Node(_) | Piece::Collection(_)))
        )
    }

    /// A scene showing only a mesh, node or collection, for its row
    /// picture.  `scene` is the scene the pieces were listed from (the one
    /// on screen), and `visible` each node's visibility in the file, since
    /// the viewer hides nodes to show a piece alone (empty: the scene's
    /// own).  `None` for other items, for pieces with nothing to draw, and
    /// for pieces of more than [`MAX_PIECE_SIZE`] triangles or vertices
    /// (a point cloud has no triangles).
    pub fn piece_scene(&self, id: ItemId, scene: &Scene, visible: &[bool]) -> Option<Scene> {
        let Some(Source::Piece(piece)) = self.item(id).map(|i| &i.source) else { return None };
        let parents = &self.held.parents;
        let n = scene.nodes.len();
        let shows = |x: usize| visible.get(x).copied().unwrap_or_else(|| scene.nodes[x].visible);
        // 1. The nodes whose meshes show: a mesh at the first visible node
        //    that uses it; a node or collection with what is beneath it,
        //    leaving out what the file hides (a node picked itself shows
        //    even when hidden).
        let mut shown: Vec<usize> = Vec::new();
        match piece {
            Piece::Mesh(_) => {
                let users = nodes_of(&self.held, piece);
                let users: Vec<usize> = users.into_iter().filter(|&x| x < n).collect();
                shown.extend(users.iter().copied().find(|&x| shows(x)).or(users.first().copied()));
            }
            Piece::Node(_) | Piece::Collection(_) => {
                let picked = matches!(piece, Piece::Node(_));
                let mut seen = vec![false; n];
                let mut stack: Vec<usize> = nodes_of(&self.held, piece).into_iter().rev().filter(|&x| x < n).collect();
                stack.retain(|&x| picked || shows(x));
                while let Some(x) = stack.pop() {
                    if std::mem::replace(&mut seen[x], true) {
                        continue;
                    }
                    if scene.nodes[x].mesh.is_some_and(|m| m < scene.meshes.len()) {
                        shown.push(x);
                    }
                    let children = scene.nodes[x].children.iter().rev().copied();
                    stack.extend(children.filter(|&c| c < n && shows(c)));
                }
            }
            _ => return None,
        }
        let meshes_shown = shown.iter().filter_map(|&x| scene.meshes.get(scene.nodes[x].mesh?));
        let (triangles, vertices) = meshes_shown.flat_map(|m| &m.primitives).fold((0usize, 0usize), |(t, v), p| {
            (t.saturating_add(p.triangle_count()), v.saturating_add(p.positions.len()))
        });
        if shown.is_empty() || triangles.max(vertices) > MAX_PIECE_SIZE {
            return None;
        }

        // 2. Each shown node becomes a root, with its mesh and the
        //    materials that mesh wears copied.
        let mut out = Scene {
            name: scene.name.clone(),
            textures: scene.textures.clone(),
            fill: scene.fill,
            ..Default::default()
        };
        let mut meshes: HashMap<usize, usize> = HashMap::new();
        let mut materials: HashMap<usize, usize> = HashMap::new();
        for &x in &shown {
            let node = &scene.nodes[x];
            let Some(m) = node.mesh else { continue };
            let mesh = match meshes.get(&m) {
                Some(&i) => i,
                None => {
                    let mut copy = scene.meshes[m].clone();
                    for p in &mut copy.primitives {
                        p.material = p.material.filter(|&i| i < scene.materials.len()).map(|i| {
                            *materials.entry(i).or_insert_with(|| {
                                out.materials.push(scene.materials[i].clone());
                                out.materials.len() - 1
                            })
                        });
                    }
                    out.meshes.push(copy);
                    meshes.insert(m, out.meshes.len() - 1);
                    out.meshes.len() - 1
                }
            };
            let mut copy = Node::new(node.name.clone());
            copy.mesh = Some(mesh);
            copy.weights = node.weights.clone();
            copy.transform = Transform::from_matrix(placement(scene, parents, x));
            out.nodes.push(copy);
            out.roots.push(out.nodes.len() - 1);
        }
        Some(out)
    }

    /// Draws a scene from [`Contents::piece_scene`] as a row picture `size`
    /// pixels square.  Call it off the window's thread: it waits for the
    /// GPU, and shrinks textures it hasn't shrunk before.
    pub fn draw_piece(&self, mut piece: Scene, renderer: &mut crate::ThumbRenderer, size: u32) -> Option<Image8> {
        let used: HashSet<usize> =
            piece.materials.iter().flat_map(|m| super::pieces::material_slots(m).into_iter().map(|(_, t)| t)).collect();
        {
            let mut small = self.small_textures.lock().ok()?;
            for (i, t) in piece.textures.iter_mut().enumerate().filter(|(i, _)| used.contains(i)) {
                let shrunk = match small.get(&i) {
                    Some((from, to)) if Arc::ptr_eq(from, t) => to.clone(),
                    _ => {
                        let to = shrink(t, TEXTURE_SIDE).map_or_else(|| t.clone(), Arc::new);
                        small.insert(i, (t.clone(), to.clone()));
                        to
                    }
                };
                *t = shrunk;
            }
        }
        renderer.render_thumbnail(&piece, size).ok()
    }
}

/// Where node `x`'s mesh is drawn, as the scene stands at rest: the node's
/// world matrix, or for a skinned mesh, where its first joint's bind
/// matrix puts the mesh.
fn placement(scene: &Scene, parents: &[Option<usize>], x: usize) -> Mat4 {
    let node = &scene.nodes[x];
    let skinned =
        node.mesh.and_then(|m| scene.meshes.get(m)).is_some_and(|m| m.primitives.iter().any(|p| p.is_skinned()));
    if skinned
        && let Some(skin) = node.skin.and_then(|s| scene.skins.get(s))
        && let Some((&joint, bind)) = skin.joints.iter().zip(&skin.inverse_bind).find(|(j, _)| **j < scene.nodes.len())
    {
        let m = world(scene, parents, joint) * *bind;
        if m.is_finite() {
            return m;
        }
    }
    world(scene, parents, x)
}

/// A node's world matrix at rest, through its parents.
fn world(scene: &Scene, parents: &[Option<usize>], x: usize) -> Mat4 {
    let mut m = Mat4::IDENTITY;
    let mut at = Some(x);
    // A damaged file's parents could go round for ever.
    for _ in 0..4096 {
        let Some(i) = at.filter(|&i| i < scene.nodes.len()) else { break };
        m = scene.nodes[i].transform.matrix() * m;
        at = parents.get(i).copied().flatten();
    }
    if m.is_finite() { m } else { Mat4::IDENTITY }
}

/// A copy of `t` no larger than `side` on its longer side, from the
/// smallest stored level at least that large, GPU blocks decoded, then
/// boxed down; its first layer or face only.  `None` when `t` is already
/// that small.
fn shrink(t: &Texture, side: u32) -> Option<Texture> {
    let first = t.levels.first()?;
    if first.width.max(first.height) <= side && !slerp_textures::is_compressed(t) && t.kind == TextureKind::D2 {
        return None;
    }
    let level = t.levels.iter().rposition(|l| l.width.max(l.height) >= side).unwrap_or(0);
    let l = &t.levels[level];
    let decoded;
    let pixels = match l.surfaces.first()? {
        Pixels::Blocks(..) => {
            decoded = super::save::surface(t, level as u32, 0, 0).ok()?.2;
            &decoded
        }
        p => p,
    };
    let (w, h) = (l.width, l.height);
    if w == 0 || h == 0 || pixels.texel_count() < w as usize * h as usize {
        return None;
    }
    let (w, h, pixels) = box_down(w as usize, h as usize, pixels, side.max(1) as usize)?;
    Some(Texture {
        kind: TextureKind::D2,
        layers: 1,
        faces: 1,
        levels: vec![Level { width: w as u32, height: h as u32, depth: 1, surfaces: vec![pixels] }],
        color_space: t.color_space,
        has_alpha: t.has_alpha,
        source: t.source.clone(),
    })
}

/// A `w` x `h` picture made at most `side` on its longer side in one pass:
/// each texel the mean of the block of `k` x `k` it covers (fewer at the
/// right and bottom edges).
fn box_down(w: usize, h: usize, pixels: &Pixels, side: usize) -> Option<(usize, usize, Pixels)> {
    let k = w.max(h).div_ceil(side).max(1);
    let (w2, h2) = (w.div_ceil(k), h.div_ceil(k));
    let column: Vec<usize> = (0..w).map(|x| x / k).collect();
    let mut sums = vec![0f32; w2 * h2 * 4];
    for y in 0..h {
        let out = &mut sums[(y / k) * w2 * 4..(y / k + 1) * w2 * 4];
        match pixels {
            Pixels::Rgba8(v) => {
                for (x, p) in v[y * w * 4..(y + 1) * w * 4].chunks_exact(4).enumerate() {
                    let o = column[x] * 4;
                    for c in 0..4 {
                        out[o + c] += p[c] as f32;
                    }
                }
            }
            Pixels::Rgba32F(v) => {
                for (x, p) in v[y * w * 4..(y + 1) * w * 4].chunks_exact(4).enumerate() {
                    let o = column[x] * 4;
                    for c in 0..4 {
                        out[o + c] += if p[c].is_finite() { p[c] } else { 0.0 };
                    }
                }
            }
            Pixels::Blocks(..) => return None,
        }
    }
    // Each output texel's share of the input, smaller along the edges.
    let count = |o: usize| ((w - (o % w2) * k).min(k) * (h - (o / w2) * k).min(k)) as f32;
    let means = sums.chunks_exact(4).enumerate().flat_map(|(o, s)| s.iter().map(move |v| v / count(o)));
    let out = match pixels {
        Pixels::Rgba8(_) => Pixels::Rgba8(means.map(|v| (v + 0.5) as u8).collect()),
        _ => Pixels::Rgba32F(means.collect()),
    };
    Some((w2, h2, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Blocks of 3 x 3 from a 5 x 4 picture; the edge blocks are smaller
    /// and averaged over what they cover.
    #[test]
    fn textures_shrink_by_the_mean_of_each_block() {
        let px: Vec<u8> = (0..20u8).flat_map(|i| [i * 10, 0, 0, 255]).collect();
        let (w, h, out) = box_down(5, 4, &Pixels::Rgba8(px), 2).unwrap();
        assert_eq!((w, h), (2, 2));
        let Pixels::Rgba8(v) = out else { panic!("8-bit stays 8-bit") };
        // Top left: texels 0-2, 5-7 and 10-12, mean 6 (times 10); top
        // right: 3, 4, 8, 9, 13, 14, mean 8.5; bottom right: 18, 19.
        assert_eq!([v[0], v[4], v[8], v[12]], [60, 85, 160, 185]);
        assert!(v.chunks_exact(4).all(|p| p[3] == 255));
        // A 2048-square texture comes down to 128, from its pixels' level.
        let t = Texture::from_rgba8(2048, 1024, vec![100; 2048 * 1024 * 4], slerp_textures::ColorSpace::Srgb);
        let small = shrink(&t, 128).unwrap();
        assert_eq!((small.width(), small.height(), small.color_space), (128, 64, slerp_textures::ColorSpace::Srgb));
        assert!(shrink(&small, 128).is_none());
    }
}
