//! Plays animation clips made for one skeleton on another.  Clips whose
//! joints have the same names, on a skeleton with the same rest pose, play
//! directly; otherwise both skeletons are recognised as humanoids (Unreal's
//! mannequin and MetaHumans, Mixamo, VRM, VRoid, Rigify, Biped) and each
//! joint's rotation is transferred relative to its own rest pose in world
//! space.
//!
//! After a humanoid retarget, foot IK keeps planted feet in place on a
//! character whose legs are proportioned differently from the clip's (see
//! `feet`): contacts found on the source clip, each planted foot pinned
//! where the scaled source foot is, the legs solved with two-bone IK, and
//! the hips lowered when a leg can't reach.
//!
//! The viewer's "Add animations from file" calls `add_animations_with`
//! with the open scene, the loaded library file and its Foot IK switch.

mod direct;
mod feet;
mod humanoid;
mod names;
mod skeleton;

use skeleton::Skeleton;
use slerp_scene::{Animation, Humanoid, Scene};
use std::collections::HashSet;

#[derive(Debug, thiserror::Error)]
pub enum RetargetError {
    #[error("the file has no animation clips")]
    NoClips,
    #[error("the skeletons share no joint names and aren't both recognisable humanoids")]
    NoMatch,
}

/// How a library's clips reach a scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// The joints have the same names and rest poses: clips play as they
    /// are, with translations fitted to the scene's bone lengths.
    SameNames,
    /// Both skeletons are humanoids: each bone's world-space turn away from
    /// its rest pose is transferred, the hips' travel is scaled by hip
    /// height, and foot IK keeps planted feet in place.
    Humanoid,
}

/// Choices for `add_animations_with`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// After a humanoid retarget, keep planted feet in place with
    /// two-bone IK, lowering the hips when a leg can't reach.  On by
    /// default.  Clips that play by name are left alone: on the skeleton
    /// they were made for, their feet already land where they should.
    pub foot_ik: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { foot_ik: true }
    }
}

/// What `add_animations_with` added.
#[derive(Clone, Debug, Default)]
pub struct Added {
    /// The new clips' names (renamed when they clash with existing ones),
    /// in the library's order.
    pub names: Vec<String>,
    /// Each new clip foot IK changes, made with foot IK switched the other
    /// way (without it when the clip was added with it, and the reverse),
    /// under the same name, so a viewer can switch foot IK without
    /// retargeting again.
    pub others: Vec<Animation>,
}

/// Adds the clips in `library` to `scene`, matching joints by name or,
/// failing that, by a humanoid retarget with foot IK.  Returns the names
/// of the clips added (renamed when they clash with existing ones).
pub fn add_animations(scene: &mut Scene, library: &Scene) -> Result<Vec<String>, RetargetError> {
    add_animations_with(scene, library, Options::default()).map(|a| a.names)
}

/// `add_animations` with a choice of foot IK, also returning each clip's
/// other version.
pub fn add_animations_with(scene: &mut Scene, library: &Scene, options: Options) -> Result<Added, RetargetError> {
    add(scene, library, humanoid::Method::SLERP, options)
}

/// `add_animations` with the humanoid retarget made exactly as chalk's
/// `tools/humans/retarget.py` makes it (without foot IK), so tests can
/// compare the result with the clips chalk bakes into its MetaHumans.
#[doc(hidden)]
pub fn add_animations_as_chalk(scene: &mut Scene, library: &Scene) -> Result<Vec<String>, RetargetError> {
    add(scene, library, humanoid::Method::CHALK, Options { foot_ik: false }).map(|a| a.names)
}

fn add(scene: &mut Scene, library: &Scene, method: humanoid::Method, options: Options) -> Result<Added, RetargetError> {
    if library.animations.is_empty() {
        return Err(RetargetError::NoClips);
    }
    // 1. Bake every clip for the scene's skeleton, with and without foot
    //    IK where it applies.
    let clips: Vec<(Animation, Option<Animation>)> = {
        let (src, dst) = (Skeleton::new(library), Skeleton::new(scene));
        let count = library.animations.len();
        match plan(&src, &dst, method)? {
            Planned::Direct(m) => (0..count).map(|i| (m.bake(i), None)).collect(),
            Planned::Humanoid(p) => (0..count)
                .map(|i| {
                    let baked = p.bake(i);
                    match baked.feet {
                        Some(feet) if options.foot_ik => (feet, Some(baked.plain)),
                        feet => (baked.plain, feet),
                    }
                })
                .collect(),
        }
    };
    // 2. Keep the clips that move something, under unique names.
    let mut taken: HashSet<String> = scene.animations.iter().map(|a| a.name.clone()).collect();
    let mut added = Added::default();
    for (mut clip, other) in clips.into_iter().filter(|(c, _)| !c.channels.is_empty()) {
        let base = clip.name.trim();
        clip.name = unique_name(if base.is_empty() { "Animation" } else { base }, &taken);
        taken.insert(clip.name.clone());
        added.names.push(clip.name.clone());
        if let Some(mut other) = other {
            other.name = clip.name.clone();
            added.others.push(other);
        }
        scene.animations.push(clip);
    }
    if added.names.is_empty() {
        return Err(RetargetError::NoMatch);
    }
    Ok(added)
}

/// Recognises a humanoid skeleton from its joint names, for files that
/// don't declare one the way VRM does.  Knows Unreal's mannequins and
/// MetaHumans (and the Universal Animation Library, which uses their
/// names), Mixamo (and Ready Player Me and HumanIK), VRoid, VRM's own bone
/// names, Blender's metarig and Rigify, and 3ds Max's Biped.  Needs the
/// hips and at least 9 of the 15 bones VRM requires; fingers, toes, eyes
/// and jaw are filled in when present.
pub fn recognise_humanoid(scene: &Scene) -> Option<Humanoid> {
    names::recognise(scene)
}

/// Which way `add_animations` would bring `library`'s clips to `scene`.
pub fn strategy(scene: &Scene, library: &Scene) -> Result<Strategy, RetargetError> {
    if library.animations.is_empty() {
        return Err(RetargetError::NoClips);
    }
    let (src, dst) = (Skeleton::new(library), Skeleton::new(scene));
    Ok(match plan(&src, &dst, humanoid::Method::SLERP)? {
        Planned::Direct(_) => Strategy::SameNames,
        Planned::Humanoid(_) => Strategy::Humanoid,
    })
}

enum Planned<'a> {
    Direct(direct::Matching<'a>),
    Humanoid(humanoid::Plan<'a>),
}

/// Chooses a strategy: by name when the names match and the rest poses
/// agree; otherwise as humanoids when both are; otherwise by name when the
/// names match anyway (non-humanoid skeletons posed differently, where
/// there is nothing better to do).
fn plan<'a>(
    src: &'a Skeleton<'a>,
    dst: &'a Skeleton<'a>,
    method: humanoid::Method,
) -> Result<Planned<'a>, RetargetError> {
    let matching = direct::Matching::new(src, dst);
    if matching.enough() && matching.compatible() {
        return Ok(Planned::Direct(matching));
    }
    if let Some(plan) = humanoid::Plan::new(src, dst, method) {
        return Ok(Planned::Humanoid(plan));
    }
    if matching.enough() {
        return Ok(Planned::Direct(matching));
    }
    Err(RetargetError::NoMatch)
}

/// `name`, or `name (2)`, `name (3)` and so on when it's taken.
fn unique_name(name: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(name) {
        return name.to_string();
    }
    (2..).map(|n| format!("{name} ({n})")).find(|n| !taken.contains(n)).unwrap_or_default()
}

#[cfg(test)]
mod tests;
