//! RigLogic driving a built scene: raw control values in, joint offsets
//! and blend shape weights out, written into a `Pose` every time the pose
//! is recomputed, so corrective shapes and joints follow the controls the
//! way they do in Unreal.

use crate::ffi::bridge;
use crate::scene::Units;
use glam::{Quat, Vec3};
use slerp_scene::{Pose, Scene};

/// Which DNA file a rig comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RigKind {
    /// A face: raw controls such as `CTRL_expressions.jawOpen`, plus the
    /// neck and head rotations its neck correctives read.
    Head,
    /// A body: no controls to set by hand; its correctives (shoulders,
    /// elbows, knees, twist joints) read the rotations of the joints a
    /// clip moves.
    Body,
}

/// One DNA file's RigLogic, tied to the scene nodes of its joints and the
/// morph targets of its blend shape channels.
pub struct Rig {
    inner: cxx::UniquePtr<bridge::Rig>,
    pub kind: RigKind,
    /// The raw control names as the file gives them.
    pub controls: Vec<String>,
    /// Each control's value.  Those fed from joint rotations (`inputs`) are
    /// overwritten from the pose before every evaluation.
    pub values: Vec<f32>,
    units: Units,
    /// The scene node of each DNA joint.
    joint_nodes: Vec<usize>,
    /// Morph targets per blend shape channel, as (node, target).
    channel_targets: Vec<Vec<(usize, usize)>>,
    /// Controls fed from joint rotations: (control, node, quaternion
    /// component: 0 x, 1 y, 2 z, 3 w, the node's rest rotation).
    inputs: Vec<(usize, usize, usize, Quat)>,
    lod: u16,
    lod_count: u16,
}

impl Rig {
    /// Builds RigLogic from `dna` (read with the rig's layers) for a scene
    /// whose node of DNA joint `j` is `joint_nodes[j]`.
    pub(crate) fn new(
        dna: &bridge::Dna,
        budget: u64,
        kind: RigKind,
        units: Units,
        joint_nodes: Vec<usize>,
        channel_targets: Vec<Vec<(usize, usize)>>,
        scene: &Scene,
    ) -> Result<Rig, String> {
        let inner = bridge::make_rig(dna, budget).map_err(|e| e.to_string())?;
        let count = bridge::rig_control_count(&inner) as usize;
        let controls: Vec<String> = (0..count as u16).map(|i| bridge::raw_control_name(dna, i)).collect();
        // Controls named `<joint>.qx` .. `.qw` read that joint's rotation.
        let mut inputs = Vec::new();
        for (c, name) in controls.iter().enumerate() {
            let Some((joint, comp)) = name.rsplit_once('.') else { continue };
            let k = match comp {
                "qx" => 0,
                "qy" => 1,
                "qz" => 2,
                "qw" => 3,
                _ => continue,
            };
            if let Some(node) = scene.nodes.iter().position(|n| n.name == joint) {
                inputs.push((c, node, k, scene.nodes[node].transform.rotation));
            }
        }
        let lod_count = bridge::rig_lod_count(&inner);
        let mut rig = Rig {
            inner,
            kind,
            values: vec![0.0; controls.len()],
            controls,
            units,
            joint_nodes,
            channel_targets,
            inputs,
            lod: 0,
            lod_count,
        };
        rig.set_lod(0);
        Ok(rig)
    }

    pub fn lod_count(&self) -> u16 {
        self.lod_count
    }

    pub fn lod(&self) -> u16 {
        self.lod
    }

    /// Evaluates only what the level of detail `lod` uses, as Unreal does
    /// for distant characters.
    pub fn set_lod(&mut self, lod: u16) {
        self.lod = lod.min(self.lod_count.saturating_sub(1));
        bridge::rig_set_lod(self.inner.pin_mut(), self.lod);
    }

    /// Whether control `i` is read from a joint's rotation rather than set
    /// by hand.
    pub fn is_joint_input(&self, i: usize) -> bool {
        self.inputs.iter().any(|&(c, ..)| c == i)
    }

    /// The control named `name`, matched in full or by the part after its
    /// last dot (`jawOpen` finds `CTRL_expressions.jawOpen`).
    pub fn control(&self, name: &str) -> Option<usize> {
        self.controls
            .iter()
            .position(|c| c == name)
            .or_else(|| self.controls.iter().position(|c| c.rsplit('.').next() == Some(name)))
    }

    /// Evaluates the rig with the current control values, the joint-driven
    /// ones read from `pose`, and adds its output to `pose`: each joint's
    /// translation, rotation and scale offsets on top of its local
    /// transform, and each blend shape channel's weight on its morph
    /// targets.  Call before `Pose::update_world`.
    pub fn apply(&mut self, pose: &mut Pose) -> Result<(), String> {
        self.evaluate(pose, true)
    }

    /// `apply` as far as the joints, for fitting skin weights.
    pub(crate) fn apply_joints(&mut self, pose: &mut Pose) -> Result<(), String> {
        self.evaluate(pose, false)
    }

    fn evaluate(&mut self, pose: &mut Pose, shapes: bool) -> Result<(), String> {
        // 1. Joint-driven inputs: how far the joint has turned from its
        //    rest orientation, in its own frame (Maya's rotate after the
        //    joint orient), as the clip left it.  The rest pose reads as the
        //    identity, for which RigLogic's correctives are all zero.
        for &(c, node, k, rest) in &self.inputs {
            if let Some(t) = pose.local.get(node) {
                let mut q = (rest.inverse() * t.rotation).normalize();
                if q.w < 0.0 {
                    q = -q;
                }
                if q.is_finite() {
                    self.values[c] = [q.x, q.y, q.z, q.w][k];
                }
            }
        }
        if shapes {
            bridge::rig_evaluate(self.inner.pin_mut(), &self.values).map_err(|e| e.to_string())?;
        } else {
            bridge::rig_evaluate_joints(self.inner.pin_mut(), &self.values).map_err(|e| e.to_string())?;
        }

        // 2. Joint offsets.
        let out = bridge::rig_joint_outputs(&self.inner);
        for (j, d) in out.chunks_exact(9).enumerate() {
            if d.iter().all(|&x| x == 0.0) {
                continue;
            }
            let Some(local) = self.joint_nodes.get(j).and_then(|&n| pose.local.get_mut(n)) else { continue };
            let t = Vec3::new(d[0], d[1], d[2]) * self.units.metres;
            let r = self.units.rotation(Vec3::new(d[3], d[4], d[5]));
            let s = Vec3::new(d[6], d[7], d[8]);
            if t.is_finite() && s.is_finite() {
                local.translation += t;
                local.rotation = (local.rotation * r).normalize();
                local.scale += s;
            }
        }

        // 3. Blend shape weights.
        if !shapes {
            return Ok(());
        }
        let weights = bridge::rig_channel_outputs(&self.inner);
        for (c, &w) in weights.iter().enumerate() {
            let Some(slots) = self.channel_targets.get(c) else { break };
            let w = if w.is_finite() { w } else { 0.0 };
            for &(node, t) in slots {
                if let Some(slot) = pose.weights.get_mut(node).and_then(|ws| ws.get_mut(t)) {
                    *slot = w;
                }
            }
        }
        Ok(())
    }

    /// The last evaluation's joint offsets, nine per DNA joint
    /// (translation, rotation, scale, in the file's units).
    pub fn joint_outputs(&self) -> &[f32] {
        bridge::rig_joint_outputs(&self.inner)
    }

    /// The last evaluation's weight per blend shape channel.
    pub fn channel_outputs(&self) -> &[f32] {
        bridge::rig_channel_outputs(&self.inner)
    }

    /// The last evaluation's value per animated map (the wrinkle map
    /// regions).
    pub fn map_outputs(&self) -> &[f32] {
        bridge::rig_map_outputs(&self.inner)
    }

    /// Points the rig at a rebuilt scene: the node of each DNA joint and
    /// the morph targets of each blend shape channel.
    pub(crate) fn retarget(&mut self, joint_nodes: Vec<usize>, channel_targets: Vec<Vec<(usize, usize)>>) {
        self.joint_nodes = joint_nodes;
        self.channel_targets = channel_targets;
    }

    /// The scene node of DNA joint `j`.
    pub fn joint_node(&self, j: usize) -> Option<usize> {
        self.joint_nodes.get(j).copied()
    }
}
