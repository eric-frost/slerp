//! Joint names: normalising them so the same joint matches across
//! exporters, and recognising humanoid skeletons from the naming schemes
//! that rigging tools use.
//!
//! Each scheme is a table from VRM 1.0 bone names (`hips`, `leftUpperArm`,
//! ...) to the normalised joint names that play them.  A skeleton is read
//! with every table and the one that finds the most bones wins.

use slerp_scene::{Humanoid, Scene};
use std::collections::HashMap;

/*┌─────────────────╮
| Bones           |
└─────────────────╯*/
//region Bones

/// The bones VRM 1.0 requires.  A skeleton needs most of them to be
/// retargeted as a humanoid.
pub const CORE: [&str; 15] = [
    "hips",
    "spine",
    "head",
    "leftUpperArm",
    "leftLowerArm",
    "leftHand",
    "rightUpperArm",
    "rightLowerArm",
    "rightHand",
    "leftUpperLeg",
    "leftLowerLeg",
    "leftFoot",
    "rightUpperLeg",
    "rightLowerLeg",
    "rightFoot",
];

/// VRM's fingers, and the names of their three segments from the palm out.
/// Only the thumb has a metacarpal; the other fingers' metacarpals are
/// inside the palm and VRM leaves them out.
const FINGERS: [(&str, [&str; 3]); 5] = [
    ("Thumb", ["Metacarpal", "Proximal", "Distal"]),
    ("Index", ["Proximal", "Intermediate", "Distal"]),
    ("Middle", ["Proximal", "Intermediate", "Distal"]),
    ("Ring", ["Proximal", "Intermediate", "Distal"]),
    ("Little", ["Proximal", "Intermediate", "Distal"]),
];

const SIDES: [(&str, &str); 2] = [("left", "l"), ("right", "r")];

/// Every VRM 1.0 humanoid bone, each after its parent.
pub fn all_bones() -> Vec<String> {
    let mut out: Vec<String> =
        ["hips", "spine", "chest", "upperChest", "neck", "head", "leftEye", "rightEye", "jaw"].map(String::from).into();
    for (side, _) in SIDES {
        for bone in ["UpperLeg", "LowerLeg", "Foot", "Toes", "Shoulder", "UpperArm", "LowerArm", "Hand"] {
            out.push(format!("{side}{bone}"));
        }
    }
    for (side, _) in SIDES {
        for (finger, segments) in FINGERS {
            for segment in segments {
                out.push(format!("{side}{finger}{segment}"));
            }
        }
    }
    out
}

//endregion

/*┌─────────────────╮
| Normalising     |
└─────────────────╯*/
//region Normalising

const SEPARATORS: [char; 4] = ['_', '-', '.', ' '];

/// A joint name reduced to what identifies the joint: lower case, without
/// a namespace (`mixamorig:`, Blender's `Armature|`, FBX's `Model::`),
/// without prefixes whole rigs share (Mixamo's, Rigify's `DEF-`, HumanIK's
/// `Character1_`, 3ds Max's `Bip01 `, Reallusion's `CC_Base_`), and
/// without separators, so `upper_arm.L` and `upperarm_l` meet.
pub fn normalise(name: &str) -> String {
    // 1. Namespaces.
    let base = name.rsplit([':', '|']).next().unwrap_or(name);
    let mut s = base.to_lowercase();
    // 2. Rig prefixes, which can stack ("DEF-" inside a namespace).
    loop {
        let before = s.len();
        if let Some(rest) = s.strip_prefix("mixamorig") {
            let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches(SEPARATORS);
            if !rest.is_empty() {
                s = rest.to_string();
            }
        }
        for prefix in ["def-", "def_", "def.", "cc_base_"] {
            if let Some(rest) = s.strip_prefix(prefix).filter(|r| !r.is_empty()) {
                s = rest.to_string();
            }
        }
        for word in ["character", "bip"] {
            if let Some(rest) = strip_numbered(&s, word) {
                s = rest;
            }
        }
        if s.len() == before {
            break;
        }
    }
    // 3. Separators.
    s.retain(|c| !SEPARATORS.contains(&c));
    s
}

/// Strips `word`, one or more digits and a separator from the front of
/// `s`, as in `character1_hips` or `bip01 l thigh`.
fn strip_numbered(s: &str, word: &str) -> Option<String> {
    let rest = s.strip_prefix(word)?;
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let rest = &rest[digits..];
    let sep = rest.chars().next().filter(|c| SEPARATORS.contains(c))?;
    let rest = &rest[sep.len_utf8()..];
    (digits > 0 && !rest.is_empty()).then(|| rest.to_string())
}

//endregion

/*┌─────────────────╮
| Naming schemes  |
└─────────────────╯*/
//region Naming schemes

/// One rig's naming scheme, as VRM bones and the normalised joint names
/// that play them.
#[derive(Default)]
struct Scheme {
    /// For tests and diagnostics.
    #[cfg_attr(not(test), allow(dead_code))]
    name: &'static str,
    /// VRM bone and the keys that may name it, best first.
    bones: Vec<(String, Vec<String>)>,
    /// Numbered spine joints from the hips up, for rigs whose spine has
    /// any number of them (Unreal's `spine_01` to `spine_05`, Mixamo's
    /// `Spine` to `Spine3`): the first present becomes `spine`, the last
    /// `upperChest` and the middle one `chest`, the lower of two middle
    /// ones as VRM authors map four (Alicia's `Spine1`).
    spine: Vec<String>,
}

impl Scheme {
    fn new(name: &'static str) -> Self {
        Scheme { name, ..Scheme::default() }
    }

    /// Adds a bone.  In both `bone` and `keys`, `<side>` stands for `left`
    /// or `right` and `<s>` for `l` or `r`; a bone using either is added
    /// once per side.
    fn bone(&mut self, bone: &str, keys: &[&str]) -> &mut Self {
        let sided = |t: &str| t.contains("<side>") || t.contains("<s>");
        if sided(bone) || keys.iter().any(|k| sided(k)) {
            for (side, s) in SIDES {
                let fill = |t: &str| t.replace("<side>", side).replace("<s>", s);
                self.bones.push((fill(bone), keys.iter().map(|k| fill(k)).collect()));
            }
        } else {
            self.bones.push((bone.to_string(), keys.iter().map(|k| k.to_string()).collect()));
        }
        self
    }

    /// Adds the fingers.  `key` uses `<side>`/`<s>`, `<f>` for a name from
    /// `fingers` (thumb, index, middle, ring, little, in that order) and
    /// `<k>` for a name from `segments` (palm out).
    fn fingers(&mut self, key: &str, fingers: [&str; 5], segments: [&str; 3]) -> &mut Self {
        for (side, s) in SIDES {
            for (f, (finger, names)) in FINGERS.iter().enumerate() {
                for (k, segment) in names.iter().enumerate() {
                    let key = key.replace("<side>", side).replace("<s>", s).replace("<f>", fingers[f]);
                    self.bones.push((format!("{side}{finger}{segment}"), vec![key.replace("<k>", segments[k])]));
                }
            }
        }
        self
    }

    fn spine(&mut self, keys: &[&str]) -> &mut Self {
        self.spine = keys.iter().map(|k| k.to_string()).collect();
        self
    }

    /// The bones this scheme finds among `joints` (normalised name to node).
    fn read(&self, joints: &HashMap<String, usize>) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = Vec::new();
        let add = |out: &mut Vec<(String, usize)>, bone: &str, node: usize| {
            if !out.iter().any(|(b, n)| b == bone || *n == node) {
                out.push((bone.to_string(), node));
            }
        };
        for (bone, keys) in &self.bones {
            if let Some(&node) = keys.iter().find_map(|k| joints.get(k)) {
                add(&mut out, bone, node);
            }
        }
        let spine: Vec<usize> = self.spine.iter().filter_map(|k| joints.get(k).copied()).collect();
        match spine.len() {
            0 => {}
            1 => add(&mut out, "spine", spine[0]),
            2 => {
                add(&mut out, "spine", spine[0]);
                add(&mut out, "chest", spine[1]);
            }
            n => {
                add(&mut out, "spine", spine[0]);
                add(&mut out, "chest", spine[(n - 1) / 2]);
                add(&mut out, "upperChest", spine[n - 1]);
            }
        }
        out
    }
}

/// The naming schemes recognised, most specific first, since the first of
/// equally good matches wins.
fn schemes() -> Vec<Scheme> {
    let mut out = Vec::new();

    // Unreal's mannequins (UE4's three spine joints, UE5's five), MetaHumans
    // and the Universal Animation Library, which uses UE4's names.
    let mut s = Scheme::new("Unreal");
    s.spine(&["spine01", "spine02", "spine03", "spine04", "spine05"])
        .bone("hips", &["pelvis"])
        .bone("neck", &["neck01", "neck"])
        .bone("head", &["head"])
        .bone("<side>Eye", &["facial<s>eye", "eye<s>"])
        .bone("jaw", &["facialcjaw", "jaw"])
        .bone("<side>Shoulder", &["clavicle<s>"])
        .bone("<side>UpperArm", &["upperarm<s>"])
        .bone("<side>LowerArm", &["lowerarm<s>"])
        .bone("<side>Hand", &["hand<s>"])
        .bone("<side>UpperLeg", &["thigh<s>"])
        .bone("<side>LowerLeg", &["calf<s>"])
        .bone("<side>Foot", &["foot<s>"])
        .bone("<side>Toes", &["ball<s>"])
        .fingers("<f><k><s>", ["thumb", "index", "middle", "ring", "pinky"], ["01", "02", "03"]);
    out.push(s);

    // Mixamo, which Ready Player Me and Maya's HumanIK (`Character1_`) share.
    let mut s = Scheme::new("Mixamo");
    s.spine(&["spine", "spine1", "spine2", "spine3"])
        .bone("hips", &["hips"])
        .bone("neck", &["neck"])
        .bone("head", &["head"])
        .bone("<side>Eye", &["<side>eye", "eye<s>"])
        .bone("<side>Shoulder", &["<side>shoulder"])
        .bone("<side>UpperArm", &["<side>arm"])
        .bone("<side>LowerArm", &["<side>forearm"])
        .bone("<side>Hand", &["<side>hand"])
        .bone("<side>UpperLeg", &["<side>upleg"])
        .bone("<side>LowerLeg", &["<side>leg"])
        .bone("<side>Foot", &["<side>foot"])
        .bone("<side>Toes", &["<side>toebase"])
        .fingers("<side>hand<f><k>", ["thumb", "index", "middle", "ring", "pinky"], ["1", "2", "3"]);
    out.push(s);

    // VRoid Studio's `J_Bip_` joints.
    let mut s = Scheme::new("VRoid");
    s.bone("hips", &["jbipchips"])
        .bone("spine", &["jbipcspine"])
        .bone("chest", &["jbipcchest"])
        .bone("upperChest", &["jbipcupperchest"])
        .bone("neck", &["jbipcneck"])
        .bone("head", &["jbipchead"])
        .bone("<side>Eye", &["jadj<s>faceeye"])
        .bone("<side>Shoulder", &["jbip<s>shoulder"])
        .bone("<side>UpperArm", &["jbip<s>upperarm"])
        .bone("<side>LowerArm", &["jbip<s>lowerarm"])
        .bone("<side>Hand", &["jbip<s>hand"])
        .bone("<side>UpperLeg", &["jbip<s>upperleg"])
        .bone("<side>LowerLeg", &["jbip<s>lowerleg"])
        .bone("<side>Foot", &["jbip<s>foot"])
        .bone("<side>Toes", &["jbip<s>toebase"])
        .fingers("jbip<s><f><k>", ["thumb", "index", "middle", "ring", "little"], ["1", "2", "3"]);
    out.push(s);

    // Blender's human metarig and the deform bones of a Rigify rig made
    // from it.  The `spine` bone starts at the hips; newer metarigs have
    // two neck bones, so the head is `spine.006`, older ones `spine.005`.
    let mut s = Scheme::new("Rigify");
    s.bone("hips", &["spine"])
        .bone("spine", &["spine001"])
        .bone("chest", &["spine002"])
        .bone("upperChest", &["spine003"])
        .bone("neck", &["spine004"])
        .bone("head", &["spine006", "spine005"])
        .bone("<side>Eye", &["eye<s>"])
        .bone("jaw", &["jaw"])
        .bone("<side>Shoulder", &["shoulder<s>"])
        .bone("<side>UpperArm", &["upperarm<s>"])
        .bone("<side>LowerArm", &["forearm<s>"])
        .bone("<side>Hand", &["hand<s>"])
        .bone("<side>UpperLeg", &["thigh<s>"])
        .bone("<side>LowerLeg", &["shin<s>"])
        .bone("<side>Foot", &["foot<s>"])
        .bone("<side>Toes", &["toe<s>"])
        .fingers("<f><k><s>", ["thumb", "findex", "fmiddle", "fring", "fpinky"], ["01", "02", "03"]);
    out.push(s);

    // VRM's own bone names used as joint names, as VRM animation files and
    // some exporters do, in VRM 1.0's thumb naming.  Blender's VRM add-on
    // writes them with the side last (`upper_arm.L`).
    let vrm = |name: &'static str, thumb: [&str; 3]| {
        let mut s = Scheme::new(name);
        for bone in ["hips", "spine", "chest", "upperChest", "neck", "head", "jaw"] {
            s.bone(bone, &[&bone.to_lowercase()]);
        }
        for bone in ["Eye", "Shoulder", "UpperArm", "LowerArm", "Hand", "UpperLeg", "LowerLeg", "Foot", "Toes"] {
            let lower = bone.to_lowercase();
            s.bone(&format!("<side>{bone}"), &[&format!("<side>{lower}"), &format!("{lower}<s>")]);
        }
        for (side, s1) in SIDES {
            for (f, (finger, segments)) in FINGERS.iter().enumerate() {
                for (k, segment) in segments.iter().enumerate() {
                    let named = if f == 0 { thumb[k] } else { segment };
                    let (finger, named) = (finger.to_lowercase(), named.to_lowercase());
                    s.bones.push((
                        format!("{side}{}{segment}", FINGERS[f].0),
                        vec![format!("{side}{finger}{named}"), format!("{finger}{named}{s1}")],
                    ));
                }
            }
        }
        s
    };
    out.push(vrm("VRM", ["metacarpal", "proximal", "distal"]));
    // Unity's humanoid and VRM 0.x name the thumb's segments one step
    // further out: proximal, intermediate, distal.
    out.push(vrm("Unity", ["proximal", "intermediate", "distal"]));

    // 3ds Max's Biped (`Bip01 L UpperArm`), with numbered fingers 0 (thumb)
    // to 4, each segment one more digit.
    let mut s = Scheme::new("Biped");
    s.spine(&["spine", "spine1", "spine2", "spine3"])
        .bone("hips", &["pelvis"])
        .bone("neck", &["neck"])
        .bone("head", &["head"])
        .bone("<side>Shoulder", &["<s>clavicle"])
        .bone("<side>UpperArm", &["<s>upperarm"])
        .bone("<side>LowerArm", &["<s>forearm"])
        .bone("<side>Hand", &["<s>hand"])
        .bone("<side>UpperLeg", &["<s>thigh"])
        .bone("<side>LowerLeg", &["<s>calf"])
        .bone("<side>Foot", &["<s>foot"])
        .bone("<side>Toes", &["<s>toe0"])
        .fingers("<s>finger<f><k>", ["0", "1", "2", "3", "4"], ["", "1", "2"]);
    out.push(s);

    out
}

//endregion

/*┌─────────────────╮
| Recognising     |
└─────────────────╯*/
//region Recognising

/// The scene's joints by normalised name.  When names repeat (a MetaHuman
/// has a `head` joint and a `head` mesh), skin joints win over other nodes
/// and nodes without meshes over nodes with them.
pub fn joint_names(scene: &Scene) -> HashMap<String, usize> {
    let mut joint = vec![false; scene.nodes.len()];
    for skin in &scene.skins {
        for &j in &skin.joints {
            if let Some(slot) = joint.get_mut(j) {
                *slot = true;
            }
        }
    }
    let rank = |i: usize| {
        if joint[i] {
            2
        } else if scene.nodes[i].mesh.is_none() {
            1
        } else {
            0
        }
    };
    let mut out: HashMap<String, usize> = HashMap::new();
    for (i, node) in scene.nodes.iter().enumerate() {
        let key = normalise(&node.name);
        if key.is_empty() {
            continue;
        }
        match out.get(&key) {
            Some(&j) if rank(j) >= rank(i) => {}
            _ => {
                out.insert(key, i);
            }
        }
    }
    out
}

/// How many of VRM's required bones a bone list has.
pub fn core_count(bones: &[(String, usize)]) -> usize {
    CORE.iter().filter(|c| bones.iter().any(|(b, _)| b == *c)).count()
}

/// The share of required bones a humanoid must have, 9 of 15 (60%).
pub const MIN_CORE: usize = 9;

/// Reads the skeleton with every naming scheme and takes each bone from
/// the scheme that found the most, falling back on the others for bones it
/// lacks, as long as the skeleton's hierarchy agrees: the hips must be
/// above the legs, and every bone below its humanoid parent.  That copes
/// with rigs that mix schemes, such as VRM torso names on Rigify limbs.
/// Needs the hips and at least `MIN_CORE` required bones.
pub fn recognise(scene: &Scene) -> Option<Humanoid> {
    let joints = joint_names(scene);
    let parents = parents(scene);
    let above = |ancestor: usize, node: usize| {
        let mut n = node;
        for _ in 0..parents.len() {
            match parents[n] {
                Some(p) if p == ancestor => return true,
                Some(p) => n = p,
                None => return false,
            }
        }
        false
    };

    // 1. Every scheme's reading, the one that found most first.
    let mut readings: Vec<Vec<(String, usize)>> =
        schemes().iter().map(|s| s.read(&joints)).filter(|r| !r.is_empty()).collect();
    readings.sort_by_key(|r| std::cmp::Reverse(r.len()));
    let candidates = |bone: &str| -> Vec<usize> {
        readings.iter().filter_map(|r| r.iter().find(|(b, _)| b == bone).map(|&(_, n)| n)).collect()
    };

    // 2. The hips: the first candidate above the legs the best reading
    //    found.
    let legs: Vec<usize> =
        ["leftUpperLeg", "rightUpperLeg"].iter().filter_map(|b| candidates(b).first().copied()).collect();
    let hips = candidates("hips");
    let hips = hips
        .iter()
        .copied()
        .find(|&h| !legs.is_empty() && legs.iter().all(|&l| above(h, l)))
        .or_else(|| hips.first().copied())?;

    // 3. The other bones, parents first, each below the nearest bone above
    //    it found so far.
    let mut bones: Vec<(String, usize)> = vec![("hips".to_string(), hips)];
    for bone in all_bones().into_iter().skip(1) {
        let mut up = parent_bone(&bone);
        let anchor = loop {
            match up {
                Some(p) => match bones.iter().find(|(b, _)| *b == p) {
                    Some(&(_, n)) => break n,
                    None => up = parent_bone(&p),
                },
                None => break hips,
            }
        };
        let found = candidates(&bone).into_iter().find(|&n| !bones.iter().any(|&(_, m)| m == n) && above(anchor, n));
        if let Some(n) = found {
            bones.push((bone, n));
        }
    }
    (core_count(&bones) >= MIN_CORE).then_some(Humanoid { bones })
}

/// The bone a humanoid bone hangs from in VRM's hierarchy.
fn parent_bone(bone: &str) -> Option<String> {
    let torso = match bone {
        "hips" => return None,
        "spine" => "hips",
        "chest" => "spine",
        "upperChest" => "chest",
        "neck" => "upperChest",
        "head" => "neck",
        "leftEye" | "rightEye" | "jaw" => "head",
        _ => "",
    };
    if !torso.is_empty() {
        return Some(torso.to_string());
    }
    let (side, part) =
        bone.strip_prefix("left").map(|p| ("left", p)).or_else(|| bone.strip_prefix("right").map(|p| ("right", p)))?;
    let parent = match part {
        "UpperLeg" => return Some("hips".into()),
        "Shoulder" => return Some("upperChest".into()),
        "LowerLeg" => "UpperLeg".to_string(),
        "Foot" => "LowerLeg".into(),
        "Toes" => "Foot".into(),
        "UpperArm" => "Shoulder".into(),
        "LowerArm" => "UpperArm".into(),
        "Hand" => "LowerArm".into(),
        "ThumbMetacarpal" => "Hand".into(),
        "ThumbProximal" => "ThumbMetacarpal".into(),
        "ThumbDistal" => "ThumbProximal".into(),
        _ => {
            if part.ends_with("Proximal") {
                "Hand".into()
            } else if let Some(finger) = part.strip_suffix("Intermediate") {
                format!("{finger}Proximal")
            } else if let Some(finger) = part.strip_suffix("Distal") {
                format!("{finger}Intermediate")
            } else {
                return None;
            }
        }
    };
    Some(format!("{side}{parent}"))
}

/// Each node's parent, ignoring children that don't exist.
fn parents(scene: &Scene) -> Vec<Option<usize>> {
    let mut out = vec![None; scene.nodes.len()];
    for (i, node) in scene.nodes.iter().enumerate() {
        for &c in &node.children {
            if c < out.len() && c != i {
                out[c] = Some(i);
            }
        }
    }
    out
}

/// The naming scheme that finds the most bones, for tests.
#[cfg(test)]
fn scheme_of(scene: &Scene) -> Option<&'static str> {
    let joints = joint_names(scene);
    let mut best: Option<(&'static str, usize)> = None;
    for scheme in schemes() {
        let n = scheme.read(&joints).len();
        if n > 0 && best.is_none_or(|(_, b)| n > b) {
            best = Some((scheme.name, n));
        }
    }
    best.map(|(name, _)| name)
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;
    use slerp_scene::Node;

    /// A skeleton from (name, parent name) pairs; an empty parent makes a
    /// root.
    fn tree(joints: &[(&str, &str)]) -> Scene {
        let mut scene = Scene::default();
        for (name, _) in joints {
            scene.nodes.push(Node::new(*name));
        }
        for (i, (_, parent)) in joints.iter().enumerate() {
            if let Some(p) = joints.iter().position(|(n, _)| n == parent) {
                scene.nodes[p].children.push(i);
            }
        }
        scene.compute_roots();
        scene
    }

    fn named(scene: &Scene, h: &Humanoid, bone: &str) -> Option<String> {
        h.node(bone).map(|n| scene.nodes[n].name.clone())
    }

    /// A humanoid named by `name(bone)`, with limbs on both sides; bones
    /// `name` gives no name for are left out.
    fn humanoid(name: impl Fn(&str) -> Option<String>) -> Scene {
        let mut joints: Vec<(String, String)> = Vec::new();
        let mut add = |bone: &str, parent: &str| {
            if let Some(n) = name(bone) {
                let p = std::iter::successors(Some(parent.to_string()), |p| parent_bone(p))
                    .find_map(|p| joints.iter().find(|(b, _)| *b == p).map(|_| p));
                joints.push((bone.to_string(), p.unwrap_or_default()));
                let _ = n;
            }
        };
        for bone in all_bones() {
            let parent = parent_bone(&bone).unwrap_or_default();
            add(&bone, &parent);
        }
        let named: Vec<(String, String)> = joints
            .iter()
            .map(|(b, p)| (name(b).unwrap(), if p.is_empty() { String::new() } else { name(p).unwrap() }))
            .collect();
        let pairs: Vec<(&str, &str)> = named.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        tree(&pairs)
    }

    #[test]
    fn normalising() {
        assert_eq!(normalise("mixamorig:LeftUpLeg"), "leftupleg");
        assert_eq!(normalise("mixamorigLeftUpLeg"), "leftupleg");
        assert_eq!(normalise("mixamorig1:Hips"), "hips");
        assert_eq!(normalise("Armature|mixamorig:Hips"), "hips");
        assert_eq!(normalise("DEF-upper_arm.L"), "upperarml");
        assert_eq!(normalise("upperarm_l"), "upperarml");
        assert_eq!(normalise("Character1_LeftForeArm"), "leftforearm");
        assert_eq!(normalise("Bip01 L UpperArm"), "lupperarm");
        assert_eq!(normalise("Bip01"), "bip01");
        assert_eq!(normalise("Character"), "character");
        assert_eq!(normalise("J_Bip_C_Hips"), "jbipchips");
        assert_eq!(normalise("CC_Base_Hip"), "hip");
        assert_eq!(normalise("spine.001"), "spine001");
        assert_eq!(normalise(""), "");
        assert_eq!(normalise("mixamorig"), "mixamorig");
        assert_eq!(normalise("Ünïcödé:Kopf"), "kopf");
    }

    #[test]
    fn bone_list() {
        let bones = all_bones();
        assert_eq!(bones.len(), 55);
        assert!(bones.contains(&"leftThumbMetacarpal".to_string()));
        assert!(bones.contains(&"rightLittleDistal".to_string()));
        assert!(CORE.iter().all(|c| bones.iter().any(|b| b == c)));
        // Every bone's parent comes before it.
        for (i, b) in bones.iter().enumerate() {
            if let Some(p) = parent_bone(b) {
                assert!(bones.iter().position(|x| *x == p).unwrap() < i, "{b}");
            }
        }
    }

    #[test]
    fn unreal_spines() {
        let ue5 = tree(&[
            ("root", ""),
            ("pelvis", "root"),
            ("spine_01", "pelvis"),
            ("spine_02", "spine_01"),
            ("spine_03", "spine_02"),
            ("spine_04", "spine_03"),
            ("spine_05", "spine_04"),
            ("neck_01", "spine_05"),
            ("neck_02", "neck_01"),
            ("head", "neck_02"),
            ("clavicle_l", "spine_05"),
            ("upperarm_l", "clavicle_l"),
            ("lowerarm_l", "upperarm_l"),
            ("hand_l", "lowerarm_l"),
            ("clavicle_r", "spine_05"),
            ("upperarm_r", "clavicle_r"),
            ("lowerarm_r", "upperarm_r"),
            ("hand_r", "lowerarm_r"),
            ("thigh_l", "pelvis"),
            ("calf_l", "thigh_l"),
            ("foot_l", "calf_l"),
            ("ball_l", "foot_l"),
            ("thigh_r", "pelvis"),
            ("calf_r", "thigh_r"),
            ("foot_r", "calf_r"),
            ("ball_r", "foot_r"),
        ]);
        let h = recognise(&ue5).unwrap();
        let name = |b: &str| named(&ue5, &h, b).unwrap();
        assert_eq!(name("hips"), "pelvis");
        assert_eq!(name("spine"), "spine_01");
        assert_eq!(name("chest"), "spine_03");
        assert_eq!(name("upperChest"), "spine_05");
        assert_eq!(name("neck"), "neck_01");
        assert_eq!(name("leftToes"), "ball_l");
        assert_eq!(scheme_of(&ue5), Some("Unreal"));

        let ue4 = tree(&[("pelvis", ""), ("spine_01", "pelvis"), ("spine_02", "spine_01"), ("spine_03", "spine_02")]);
        let bones = schemes()[0].read(&joint_names(&ue4));
        let find = |b: &str| bones.iter().find(|(n, _)| n == b).map(|&(_, i)| ue4.nodes[i].name.as_str());
        assert_eq!(find("chest"), Some("spine_02"));
        assert_eq!(find("upperChest"), Some("spine_03"));
        // Four spine joints put the chest on the second, as VRM authors do.
        let four =
            tree(&[("Hips", ""), ("Spine", "Hips"), ("Spine1", "Spine"), ("Spine2", "Spine1"), ("Spine3", "Spine2")]);
        let bones = schemes()[1].read(&joint_names(&four));
        let find = |b: &str| bones.iter().find(|(n, _)| n == b).map(|&(_, i)| four.nodes[i].name.as_str());
        assert_eq!(find("chest"), Some("Spine1"));
        assert_eq!(find("upperChest"), Some("Spine3"));
    }

    #[test]
    fn rigify_and_biped() {
        let rigify = humanoid(|b| {
            let side = if b.starts_with("left") { ".L" } else { ".R" };
            let part = b.trim_start_matches("left").trim_start_matches("right");
            Some(match b {
                "hips" => "DEF-spine".into(),
                "spine" => "DEF-spine.001".into(),
                "chest" => "DEF-spine.002".into(),
                "upperChest" => "DEF-spine.003".into(),
                "neck" => "DEF-spine.004".into(),
                "head" => "DEF-spine.006".into(),
                _ => match part {
                    "Shoulder" => format!("DEF-shoulder{side}"),
                    "UpperArm" => format!("DEF-upper_arm{side}"),
                    "LowerArm" => format!("DEF-forearm{side}"),
                    "Hand" => format!("DEF-hand{side}"),
                    "UpperLeg" => format!("DEF-thigh{side}"),
                    "LowerLeg" => format!("DEF-shin{side}"),
                    "Foot" => format!("DEF-foot{side}"),
                    "Toes" => format!("DEF-toe{side}"),
                    "IndexProximal" => format!("DEF-f_index.01{side}"),
                    _ => return None,
                },
            })
        });
        let h = recognise(&rigify).unwrap();
        assert_eq!(scheme_of(&rigify), Some("Rigify"));
        assert_eq!(named(&rigify, &h, "hips").unwrap(), "DEF-spine");
        assert_eq!(named(&rigify, &h, "head").unwrap(), "DEF-spine.006");
        assert_eq!(named(&rigify, &h, "leftUpperArm").unwrap(), "DEF-upper_arm.L");
        assert_eq!(named(&rigify, &h, "rightIndexProximal").unwrap(), "DEF-f_index.01.R");

        let biped = humanoid(|b| {
            let side = if b.starts_with("left") { "L" } else { "R" };
            let part = b.trim_start_matches("left").trim_start_matches("right");
            Some(match b {
                "hips" => "Bip01 Pelvis".into(),
                "spine" => "Bip01 Spine".into(),
                "chest" => "Bip01 Spine1".into(),
                "neck" => "Bip01 Neck".into(),
                "head" => "Bip01 Head".into(),
                _ => match part {
                    "UpperArm" => format!("Bip01 {side} UpperArm"),
                    "LowerArm" => format!("Bip01 {side} Forearm"),
                    "Hand" => format!("Bip01 {side} Hand"),
                    "UpperLeg" => format!("Bip01 {side} Thigh"),
                    "LowerLeg" => format!("Bip01 {side} Calf"),
                    "Foot" => format!("Bip01 {side} Foot"),
                    "ThumbMetacarpal" => format!("Bip01 {side} Finger0"),
                    "ThumbProximal" => format!("Bip01 {side} Finger01"),
                    _ => return None,
                },
            })
        });
        let h = recognise(&biped).unwrap();
        assert_eq!(named(&biped, &h, "hips").unwrap(), "Bip01 Pelvis");
        assert_eq!(named(&biped, &h, "chest").unwrap(), "Bip01 Spine1");
        assert_eq!(named(&biped, &h, "leftThumbMetacarpal").unwrap(), "Bip01 L Finger0");
        assert_eq!(named(&biped, &h, "leftThumbProximal").unwrap(), "Bip01 L Finger01");
    }

    #[test]
    fn unity_thumbs() {
        let unity = humanoid(|b| {
            let mut name = b[..1].to_uppercase() + &b[1..];
            // Unity's thumb segments are one step further out than VRM 1.0's.
            if let Some(rest) = name.strip_suffix("ThumbProximal") {
                name = format!("{rest}ThumbIntermediate");
            } else if let Some(rest) = name.strip_suffix("ThumbMetacarpal") {
                name = format!("{rest}ThumbProximal");
            }
            (!b.contains("Eye") && b != "jaw").then_some(name)
        });
        let h = recognise(&unity).unwrap();
        assert_eq!(named(&unity, &h, "leftThumbMetacarpal").unwrap(), "LeftThumbProximal");
        assert_eq!(named(&unity, &h, "leftThumbProximal").unwrap(), "LeftThumbIntermediate");
        assert_eq!(named(&unity, &h, "upperChest").unwrap(), "UpperChest");
        assert_eq!(h.bones.len(), 52);
    }

    /// VRM torso names on Rigify limbs, as Seed-san has: the Rigify table
    /// finds the most bones but reads the `spine` joint as the hips, which
    /// the legs aren't under.
    #[test]
    fn mixed_schemes() {
        let seed = tree(&[
            ("hips", ""),
            ("spine", "hips"),
            ("chest", "spine"),
            ("neck", "chest"),
            ("head", "neck"),
            ("shoulder.L", "chest"),
            ("upper_arm.L", "shoulder.L"),
            ("forearm.L", "upper_arm.L"),
            ("hand.L", "forearm.L"),
            ("f_index.01.L", "hand.L"),
            ("f_index.02.L", "f_index.01.L"),
            ("shoulder.R", "chest"),
            ("upper_arm.R", "shoulder.R"),
            ("forearm.R", "upper_arm.R"),
            ("hand.R", "forearm.R"),
            ("thigh.L", "hips"),
            ("shin.L", "thigh.L"),
            ("foot.L", "shin.L"),
            ("thigh.R", "hips"),
            ("shin.R", "thigh.R"),
            ("foot.R", "shin.R"),
        ]);
        let h = recognise(&seed).unwrap();
        let name = |b: &str| named(&seed, &h, b);
        assert_eq!(name("hips").as_deref(), Some("hips"));
        assert_eq!(name("spine").as_deref(), Some("spine"));
        assert_eq!(name("chest").as_deref(), Some("chest"));
        assert_eq!(name("head").as_deref(), Some("head"));
        assert_eq!(name("leftUpperLeg").as_deref(), Some("thigh.L"));
        assert_eq!(name("leftLowerArm").as_deref(), Some("forearm.L"));
        assert_eq!(name("leftIndexIntermediate").as_deref(), Some("f_index.02.L"));
    }

    #[test]
    fn too_few_bones() {
        let some = tree(&[("Hips", ""), ("Spine", "Hips"), ("Head", "Spine"), ("LeftArm", "Spine")]);
        assert!(recognise(&some).is_none());
        assert!(recognise(&tree(&[])).is_none());
        assert!(recognise(&tree(&[("Bone", ""), ("Bone.001", "Bone"), ("Bone.002", "Bone.001")])).is_none());
        // Right names, but no hierarchy to support them.
        let flat = humanoid(|b| Some(b.to_string()));
        let mut flat = flat;
        for n in &mut flat.nodes {
            n.children.clear();
        }
        flat.compute_roots();
        assert!(recognise(&flat).is_none());
    }
}
