//! What a reload keeps.  Reloading a file that changed on disk (or with
//! F5) builds a new document from the new contents, then carries over how
//! the old one was being looked at: the camera, the clip and its time,
//! morph weights, hidden nodes, the level of detail, a MetaHuman's face
//! controls, a material's shape, a texture's mip, layer, channels,
//! exposure, pan and zoom, a sprite's layers and frame, a gallery's open
//! picture.  Lighting and overlays live outside the document (`Look`), so
//! they stay by themselves.
//!
//! Things are matched by name where a re-export may renumber them (clips,
//! expressions, gallery pictures, sprite tags), and by index only where the
//! structure is the same.  `ViewState` (Copy View State) isn't used for
//! this: it records morphs a playing clip sets as fixed weights, pauses
//! the clip and rounds the camera, which suits a reference image but not a
//! view that should carry on as it was.

use crate::anim::{Morphs, Player};
use crate::document::{Content, Document, Panorama};
use crate::gfx::Gfx;
use crate::scene_view::SceneView;
use crate::texture_view::{GalleryView, TextureView};
use slerp_render::TextureDrawParams;
use slerp_scene::Scene;

/// Shows `new` (the reloaded file) the way `old` was shown.  When the file
/// now holds something else (a model became a texture), nothing carries
/// over.
pub fn carry(old: &Document, new: &mut Document, gfx: &mut Gfx, aspect: f32) {
    match (&old.content, &mut new.content) {
        (Content::Scene(o), Content::Scene(n)) => carry_scene(o, n),
        (Content::Material(o), Content::Material(n)) => {
            n.state = o.state.clone();
            n.rebuild(gfx, aspect);
            carry_scene(&o.view, &mut n.view);
        }
        (Content::Texture(o), Content::Texture(n)) => carry_texture(o, n, gfx, aspect),
        (Content::Gallery(o), Content::Gallery(n)) => carry_gallery(o, n, gfx, aspect),
        (Content::Canvas(o), Content::Canvas(n)) => {
            let names = |c: &dyn slerp_scene::Canvas| c.layers().iter().map(|l| l.name.clone()).collect::<Vec<_>>();
            if names(&*o.canvas) == names(&*n.canvas) && o.shown != n.shown {
                n.shown = o.shown.clone();
                n.layers_changed();
            }
            let canvas = n.canvas.clone();
            n.player.keep_from(&*canvas, &o.player, &*o.canvas);
            carry_texture(&o.texture, &mut n.texture, gfx, aspect);
        }
        (Content::Lut(o), Content::Lut(n)) => n.carry_over(o),
        (Content::Palette(o), Content::Palette(n)) => {
            n.carry_over(o);
            n.filter = o.filter.clone();
        }
        (Content::Cad(o), Content::Cad(n)) => n.carry_over(o),
        (Content::Pages(o), Content::Pages(n)) => n.carry_over(o),
        (Content::Volume(o), Content::Volume(n)) => n.carry_over(o),
        (Content::Dicom(o), Content::Dicom(n)) => {
            n.carry_over(o);
            n.texture.params = o.texture.params;
            n.texture.fit = o.texture.fit;
        }
        _ => {}
    }
}

/*┌─────────────────╮
| Scenes          |
└─────────────────╯*/
//region Scenes

fn carry_scene(old: &SceneView, new: &mut SceneView) {
    // 1. Clips added from animation libraries, while the skeleton they
    //    were fitted to stays the same.
    let same = same_nodes(&old.scene, &new.scene);
    if same && old.scene.animations.len() > old.own_clips {
        new.scene.animations.extend_from_slice(&old.scene.animations[old.own_clips..]);
        new.foot_ik = old.foot_ik.clone();
    }
    new.foot_ik.on = old.foot_ik.on;
    // 2. The clip and its time, and the morph weights set.
    new.player = carry_player(&old.player, &old.scene, &new.scene);
    new.morphs = carry_morphs(&old.morphs, &old.scene, &new.scene);
    // 3. The level of detail, then the nodes the user hid or showed.
    if let (Some(o), Some(n)) = (&old.lods, &new.lods)
        && o.current != n.current
        && o.current.is_none_or(|l| n.levels.contains(&l))
    {
        new.set_lod(o.current);
    }
    for (node, visible) in carry_visibility(&old.scene, &old.initial_visible, &new.scene, same) {
        new.set_visible(node, visible);
    }
    new.selected_material = old.selected_material.filter(|&m| m < new.scene.materials.len());
    if let (Some(o), Some(n)) = (&old.dna, &mut new.dna) {
        n.keep_from(o);
    }
    // 4. The camera, and Quake's liquids where they were.
    new.cameras.keep_view_of(&old.cameras);
    new.effects_time = old.effects_time;
    new.pose_dirty = true;
}

/// Whether two scenes have the same nodes in the same order, so indices
/// (of joints in added clips, of hidden nodes) mean the same in both.
fn same_nodes(a: &Scene, b: &Scene) -> bool {
    a.nodes.len() == b.nodes.len() && a.nodes.iter().zip(&b.nodes).all(|(x, y)| x.name == y.name)
}

/// A player for `new` that carries on where `old` was: the same clip (by
/// name), time, speed, looping, root motion and play state.  When the clip
/// is gone, the new file's default clip plays.
pub fn carry_player(old: &Player, old_scene: &Scene, new_scene: &Scene) -> Player {
    let mut p = Player::new(new_scene);
    p.speed = old.speed;
    p.looping = old.looping;
    p.root_motion = old.root_motion;
    match old.clip.and_then(|c| old_scene.animations.get(c).map(|a| (c, a))) {
        // The rest pose stays the rest pose.
        None => {
            p.clip = None;
            p.playing = false;
        }
        Some((index, clip)) => {
            let found = if clip.name.is_empty() {
                (old_scene.animations.len() == new_scene.animations.len()).then_some(index)
            } else {
                new_scene.animations.iter().position(|a| a.name == clip.name)
            };
            if let Some(c) = found {
                p.clip = Some(c);
                p.time = old.time.clamp(0.0, new_scene.animations[c].duration.max(0.0));
                p.playing = old.playing;
            }
        }
    }
    p.update_hold(new_scene);
    p
}

/// The morph weights `old` set, for `new`: named ones as they are,
/// expressions by name, and unnamed ones by node and index where the new
/// scene still has them.
pub fn carry_morphs(old: &Morphs, old_scene: &Scene, new_scene: &Scene) -> Morphs {
    let mut m = Morphs::new(new_scene);
    m.named = old.named.clone();
    m.indexed = old
        .indexed
        .iter()
        .filter(|((node, target), _)| *target < morph_count(new_scene, *node))
        .map(|(k, w)| (*k, *w))
        .collect();
    for (i, e) in old_scene.expressions.iter().enumerate() {
        if let (Some(&w), Some(j)) =
            (old.expressions.get(i), new_scene.expressions.iter().position(|x| x.name == e.name))
        {
            m.expressions[j] = w;
        }
    }
    m
}

fn morph_count(scene: &Scene, node: usize) -> usize {
    let mesh = scene.nodes.get(node).and_then(|n| n.mesh).and_then(|m| scene.meshes.get(m));
    mesh.map_or(0, |m| m.primitives.iter().map(|p| p.targets.len()).max().unwrap_or(0))
}

/// The nodes of `new` to show or hide because the user changed them in
/// `old` (where `initial` is how the file had them): by index when the
/// nodes are the same, otherwise by name.
pub fn carry_visibility(old: &Scene, initial: &[bool], new: &Scene, same: bool) -> Vec<(usize, bool)> {
    let changed = old.nodes.iter().enumerate().filter(|(i, n)| initial.get(*i).is_some_and(|v| *v != n.visible));
    if same {
        return changed.map(|(i, n)| (i, n.visible)).collect();
    }
    let mut out = Vec::new();
    for (_, n) in changed {
        out.extend(new.nodes.iter().enumerate().filter(|(_, m)| m.name == n.name).map(|(j, _)| (j, n.visible)));
    }
    out
}

//endregion

/*┌─────────────────╮
| Pictures        |
└─────────────────╯*/
//region Pictures

fn carry_texture(old: &TextureView, new: &mut TextureView, gfx: &mut Gfx, aspect: f32) {
    let size = |t: &TextureView| (t.texture.width(), t.texture.height());
    new.params =
        carry_params(&old.params, size(old), size(new), (new.gpu.mips(), new.layer_count(), new.texture.faces.max(1)));
    new.fit = old.fit;
    // A stretched picture keeps its curve, points and plane.
    if let (Some(o), Some(n)) = (&old.stretch, &mut new.stretch) {
        n.carry_over(o);
        if let Err(e) = new.restretch(gfx) {
            eprintln!("slerp view: {e}");
        }
    }
    if let Some(o) = &old.relief {
        new.relief_height = old.relief_height;
        new.set_relief(true, gfx, aspect);
        if let Some(n) = &mut new.relief {
            n.cameras.keep_view_of(&o.cameras);
        }
    }
    if let Some(o) = &old.inside
        && new.is_panorama()
    {
        let mut inside = Panorama::new(gfx);
        inside.camera = o.camera;
        new.inside = Some(inside);
    }
}

/// The texture view's settings for a reloaded picture: what was picked
/// (mip, layer, face, channels, exposure, tone mapping, background),
/// within what the new picture has (`limits`: mips, layers, faces), and
/// the same part of it in view when its size changed.
pub fn carry_params(
    old: &TextureDrawParams,
    old_size: (u32, u32),
    new_size: (u32, u32),
    limits: (u32, u32, u32),
) -> TextureDrawParams {
    let (mips, layers, faces) = limits;
    let sx = new_size.0.max(1) as f32 / old_size.0.max(1) as f32;
    let sy = new_size.1.max(1) as f32 / old_size.1.max(1) as f32;
    TextureDrawParams {
        mip: old.mip.filter(|&m| m < mips),
        layer: old.layer.min(layers.saturating_sub(1)),
        face: old.face.min(faces.saturating_sub(1)),
        center: [old.center[0] * sx, old.center[1] * sy],
        zoom: old.zoom / sx,
        ..*old
    }
}

fn carry_gallery(old: &GalleryView, new: &mut GalleryView, gfx: &mut Gfx, aspect: f32) {
    new.filter = old.filter.clone();
    new.cell = old.cell;
    let Some((i, view)) = &old.open else { return };
    let name = old.items.get(*i).map(|it| it.name.as_str()).unwrap_or_default();
    let found = new.items.iter().position(|it| it.name == name).or_else(|| (*i < new.items.len()).then_some(*i));
    if let Some(j) = found
        && new.open_item(j, gfx).is_ok()
        && let Some((_, t)) = &mut new.open
    {
        carry_texture(view, t, gfx, aspect);
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;
    use slerp_scene::{Animation, Expression, Mesh, MorphTarget, Node, Primitive};

    fn clip(name: &str, duration: f32) -> Animation {
        Animation { name: name.into(), duration, channels: Vec::new() }
    }

    fn scene(clips: &[(&str, f32)]) -> Scene {
        Scene { animations: clips.iter().map(|(n, d)| clip(n, *d)).collect(), ..Default::default() }
    }

    #[test]
    fn the_clip_carries_on_by_name() {
        let old_scene = scene(&[("Idle", 2.0), ("Walk", 1.0), ("Run", 0.8)]);
        let mut old = Player::new(&old_scene);
        old.clip = Some(1);
        old.time = 0.6;
        old.playing = false;
        old.speed = 0.5;
        old.root_motion = false;
        // A clip was added in front and Walk got shorter.
        let new_scene = scene(&[("Jump", 1.0), ("Idle", 2.0), ("Walk", 0.5), ("Run", 0.8)]);
        let p = carry_player(&old, &old_scene, &new_scene);
        assert_eq!(p.clip, Some(2));
        assert_eq!(p.time, 0.5, "clamped to the new length");
        assert!(!p.playing);
        assert_eq!(p.speed, 0.5);
        assert!(!p.root_motion);

        // Playing, and the clip is gone: the new file's default plays.
        old.playing = true;
        let p = carry_player(&old, &old_scene, &scene(&[("Idle", 2.0)]));
        assert_eq!(p.clip, Some(0));
        assert!(p.playing);

        // The rest pose stays the rest pose.
        old.clip = None;
        let p = carry_player(&old, &old_scene, &new_scene);
        assert_eq!(p.clip, None);
        assert!(!p.playing);
    }

    fn morph_scene(expressions: &[&str]) -> Scene {
        let mut s = Scene::default();
        let prim = Primitive { targets: vec![MorphTarget::default(); 3], ..Default::default() };
        s.meshes.push(Mesh { primitives: vec![prim], ..Default::default() });
        s.nodes.push(Node { mesh: Some(0), ..Node::new("head") });
        s.roots.push(0);
        s.expressions = expressions.iter().map(|n| Expression { name: n.to_string(), ..Default::default() }).collect();
        s
    }

    #[test]
    fn morphs_carry_by_name_and_where_they_still_exist() {
        let old_scene = morph_scene(&["happy", "blink"]);
        let mut old = Morphs::new(&old_scene);
        old.named.insert("jawOpen".into(), 0.7);
        old.indexed.insert((0, 2), 0.4);
        old.indexed.insert((0, 5), 0.9);
        old.expressions = vec![0.0, 1.0];
        let new_scene = morph_scene(&["blink", "angry", "happy"]);
        let m = carry_morphs(&old, &old_scene, &new_scene);
        assert_eq!(m.named.get("jawOpen"), Some(&0.7));
        assert_eq!(m.indexed.get(&(0, 2)), Some(&0.4));
        assert_eq!(m.indexed.get(&(0, 5)), None, "no sixth target");
        assert_eq!(m.expressions, vec![1.0, 0.0, 0.0]);
    }

    #[test]
    fn hidden_nodes_carry_by_index_or_name() {
        let mut old =
            Scene { nodes: ["body", "hat", "cape"].iter().map(|n| Node::new(*n)).collect(), ..Default::default() };
        let initial = vec![true, true, false];
        old.nodes[1].visible = false; // hidden by the user
        old.nodes[2].visible = true; // shown by the user
        let mut new = Scene { nodes: old.nodes.clone(), ..Default::default() };
        assert_eq!(carry_visibility(&old, &initial, &new, true), vec![(1, false), (2, true)]);
        // Reordered, with a node added: by name.
        new.nodes = ["cape", "boots", "hat", "body"].iter().map(|n| Node::new(*n)).collect();
        assert_eq!(carry_visibility(&old, &initial, &new, false), vec![(2, false), (0, true)]);
    }

    #[test]
    fn texture_settings_carry_within_the_new_picture() {
        let old = TextureDrawParams {
            mip: Some(5),
            layer: 3,
            face: 4,
            channels: [true, false, false, true],
            exposure: 1.5,
            tonemap: slerp_render::Tonemap::PbrNeutral,
            center: [100.0, 50.0],
            zoom: 4.0,
            checkerboard: false,
            background: [0.1, 0.2, 0.3],
        };
        // Same picture, re-exported: everything stays.
        assert_eq!(carry_params(&old, (256, 128), (256, 128), (9, 4, 6)), old);
        // Twice the size with fewer mips and layers and no faces: the same
        // part shows, at the same size on screen.
        let p = carry_params(&old, (256, 128), (512, 256), (3, 1, 1));
        assert_eq!((p.mip, p.layer, p.face), (None, 0, 0));
        assert_eq!(p.center, [200.0, 100.0]);
        assert_eq!(p.zoom, 2.0);
        assert_eq!((p.channels, p.exposure, p.checkerboard), (old.channels, 1.5, false));
    }
}
