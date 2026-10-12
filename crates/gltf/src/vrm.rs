//! VRM avatars, versions 0.x (`VRM` extension) and 1.0 (`VRMC_vrm`), and
//! VRM animation files (`VRMC_vrm_animation`).
//!
//! VRM is glTF with a humanoid bone map, facial expressions and a toon
//! material (MToon).  Slerp reads the bone map and expressions into the
//! scene, draws MToon as a matte lit material, and turns VRM 0.x models,
//! which face -Z, around to face +Z like glTF and VRM 1.0.

use crate::Ctx;
use crate::json::Root;
use crate::materials::{TexRefs, make_toon};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_json::value::RawValue;
use slerp_scene::glam::Vec3;
use slerp_scene::{Expression, Humanoid, Info, Material, MorphBind};
use std::collections::{BTreeMap, HashMap};

/*┌─────────────────╮
| JSON            |
└─────────────────╯*/
//region JSON

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Vrm0 {
    #[serde(rename = "specVersion")]
    spec_version: Option<String>,
    meta: HashMap<String, Value>,
    humanoid: Vrm0Humanoid,
    #[serde(rename = "blendShapeMaster")]
    blend_shapes: Vrm0BlendShapes,
    #[serde(rename = "materialProperties")]
    materials: Vec<Vrm0Material>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Vrm0Humanoid {
    #[serde(rename = "humanBones")]
    bones: Vec<Vrm0Bone>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Vrm0Bone {
    bone: String,
    node: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Vrm0BlendShapes {
    #[serde(rename = "blendShapeGroups")]
    groups: Vec<Vrm0Group>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Vrm0Group {
    name: String,
    #[serde(rename = "presetName")]
    preset: String,
    binds: Vec<Vrm0Bind>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Vrm0Bind {
    mesh: Option<i64>,
    index: Option<i64>,
    /// 0 to 100.
    weight: Option<f32>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Vrm0Material {
    shader: String,
    #[serde(rename = "floatProperties")]
    floats: HashMap<String, Value>,
    #[serde(rename = "vectorProperties")]
    vectors: HashMap<String, Value>,
    #[serde(rename = "textureProperties")]
    textures: HashMap<String, Value>,
}

impl Vrm0Material {
    fn float(&self, name: &str) -> Option<f32> {
        self.floats.get(name)?.as_f64().map(|v| v as f32).filter(|v| v.is_finite())
    }

    fn texture(&self, name: &str) -> Option<usize> {
        self.textures.get(name)?.as_u64().map(|v| v as usize)
    }

    fn color(&self, name: &str) -> Option<Vec3> {
        let v = self.vectors.get(name)?.as_array()?;
        let c: Vec<f32> = v.iter().take(3).filter_map(|x| x.as_f64()).map(|x| x as f32).collect();
        (c.len() == 3 && c.iter().all(|x| x.is_finite())).then(|| Vec3::new(c[0], c[1], c[2]))
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Vrm1 {
    #[serde(rename = "specVersion")]
    spec_version: Option<String>,
    meta: HashMap<String, Value>,
    humanoid: Vrm1Humanoid,
    expressions: Vrm1Expressions,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Vrm1Humanoid {
    #[serde(rename = "humanBones")]
    bones: BTreeMap<String, NodeRef>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct NodeRef {
    node: Option<usize>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Vrm1Expressions {
    preset: BTreeMap<String, Vrm1Expression>,
    custom: BTreeMap<String, Vrm1Expression>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Vrm1Expression {
    #[serde(rename = "morphTargetBinds")]
    binds: Vec<Vrm1Bind>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Vrm1Bind {
    node: Option<usize>,
    index: Option<usize>,
    weight: Option<f32>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Vrma {
    #[serde(rename = "specVersion")]
    spec_version: Option<String>,
    humanoid: Vrm1Humanoid,
    expressions: VrmaExpressions,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct VrmaExpressions {
    preset: BTreeMap<String, NodeRef>,
    custom: BTreeMap<String, NodeRef>,
}

//endregion

/*┌─────────────────╮
| Avatar          |
└─────────────────╯*/
//region Avatar

/// The VRM extension a file carries, parsed.
pub enum Vrm {
    None,
    V0(Box<Vrm0>),
    V1(Box<Vrm1>),
    Animation(Box<Vrma>),
}

fn parse<T: DeserializeOwned + Default>(raw: &RawValue, ctx: &Ctx, what: &str) -> T {
    serde_json::from_str(raw.get()).unwrap_or_else(|e| {
        ctx.warn(format!("the {what} extension couldn't be read: {e}"));
        T::default()
    })
}

impl Vrm {
    pub fn parse(root: &Root, ctx: &Ctx) -> Vrm {
        let ext = &root.extensions;
        if let Some(raw) = &ext.vrm1 {
            Vrm::V1(Box::new(parse(raw, ctx, "VRMC_vrm")))
        } else if let Some(raw) = &ext.vrm0 {
            Vrm::V0(Box::new(parse(raw, ctx, "VRM")))
        } else if let Some(raw) = &ext.vrma {
            Vrm::Animation(Box::new(parse(raw, ctx, "VRMC_vrm_animation")))
        } else {
            Vrm::None
        }
    }

    /// The format name for `Info::format`, when this is VRM.
    pub fn format(&self) -> Option<&'static str> {
        match self {
            Vrm::None => None,
            Vrm::V0(_) => Some("VRM 0.x"),
            Vrm::V1(_) => Some("VRM 1.0"),
            Vrm::Animation(_) => Some("VRM animation"),
        }
    }

    /// True for VRM 0.x, whose models face -Z and need turning around.
    pub fn faces_back(&self) -> bool {
        matches!(self, Vrm::V0(_))
    }

    /// Adds the avatar's name, author and licence to the details.
    pub fn describe(&self, info: &mut Info) {
        let text = |v: Option<&Value>| -> Option<String> {
            match v? {
                Value::String(s) if !s.is_empty() => Some(s.clone()),
                Value::Array(a) => {
                    let parts: Vec<&str> = a.iter().filter_map(|x| x.as_str()).collect();
                    (!parts.is_empty()).then(|| parts.join(", "))
                }
                _ => None,
            }
        };
        let mut add = |name: &str, value: Option<String>| {
            if let Some(v) = value {
                info.details.push((name.to_string(), v));
            }
        };
        match self {
            Vrm::None => {}
            Vrm::V0(v) => {
                add("VRM version", v.spec_version.clone());
                add("Title", text(v.meta.get("title")));
                add("Version", text(v.meta.get("version")));
                add("Author", text(v.meta.get("author")));
                let licence = text(v.meta.get("licenseName")).filter(|l| l != "Other");
                add("Licence", licence.or_else(|| text(v.meta.get("otherLicenseUrl"))));
            }
            Vrm::V1(v) => {
                add("VRM version", v.spec_version.clone());
                add("Title", text(v.meta.get("name")));
                add("Version", text(v.meta.get("version")));
                add("Authors", text(v.meta.get("authors")));
                add("Licence", text(v.meta.get("licenseUrl")));
            }
            Vrm::Animation(v) => {
                add("VRM animation version", v.spec_version.clone());
                let names: Vec<&str> =
                    v.expressions.preset.keys().chain(v.expressions.custom.keys()).map(|s| s.as_str()).collect();
                if !names.is_empty() {
                    add("Expression tracks", Some(names.join(", ")));
                }
            }
        }
    }

    /// Textures VRM 0.x's toon material uses beyond the glTF material's.
    pub fn extra_textures(&self, material: usize) -> Vec<usize> {
        let Vrm::V0(v) = self else { return Vec::new() };
        let Some(m) = v.materials.get(material).filter(|m| m.shader == "VRM/MToon") else { return Vec::new() };
        ["_EmissionMap", "_BumpMap"].iter().filter_map(|t| m.texture(t)).collect()
    }

    /// Fills in what the glTF material of a VRM 0.x toon material leaves
    /// out (older exporters wrote only the base colour there), and draws it
    /// lit.  VRM 1.0 materials are handled by `build_material`, since they
    /// mark themselves with an extension.
    pub fn adjust_material(&self, material: usize, m: &mut Material, refs: &TexRefs) {
        let Vrm::V0(v) = self else { return };
        let Some(props) = v.materials.get(material).filter(|p| p.shader == "VRM/MToon") else { return };
        make_toon(m);
        if m.emissive == Vec3::ZERO && m.emissive_texture.is_none() {
            // Unity stores colours in sRGB.
            m.emissive = props.color("_EmissionColor").map(srgb_to_linear).unwrap_or(Vec3::ZERO);
            m.emissive_texture = props.texture("_EmissionMap").and_then(|t| refs.plain(t));
        }
        if m.normal_texture.is_none()
            && let Some(t) = props.texture("_BumpMap").and_then(|t| refs.plain(t))
        {
            m.normal_texture = Some(t);
            m.normal_scale = props.float("_BumpScale").unwrap_or(1.0);
        }
        if props.float("_CullMode") == Some(0.0) {
            m.double_sided = true;
        }
    }

    /// The humanoid bone map, in VRM 1.0's bone names.
    pub fn humanoid(&self, node_map: &[Option<usize>]) -> Option<Humanoid> {
        let map = |n: Option<usize>| n.and_then(|n| node_map.get(n).copied().flatten());
        let bones: Vec<(String, usize)> = match self {
            Vrm::None => return None,
            Vrm::V0(v) => v
                .humanoid
                .bones
                .iter()
                .filter_map(|b| Some((vrm0_bone_name(&b.bone), map(usize::try_from(b.node?).ok())?)))
                .collect(),
            Vrm::V1(v) => v.humanoid.bones.iter().filter_map(|(name, r)| Some((name.clone(), map(r.node)?))).collect(),
            Vrm::Animation(v) => {
                v.humanoid.bones.iter().filter_map(|(name, r)| Some((name.clone(), map(r.node)?))).collect()
            }
        };
        (!bones.is_empty()).then_some(Humanoid { bones })
    }

    /// Facial expressions and their presets.  VRM 0.x binds name a mesh,
    /// which becomes every loaded node showing that mesh, and weigh 0 to
    /// 100; VRM 1.0 binds name a node and weigh 0 to 1.  `morphs` gives the
    /// number of morph targets of each of the file's meshes, to drop binds
    /// to targets that don't exist.
    pub fn expressions(&self, root: &Root, node_map: &[Option<usize>], morphs: &[usize]) -> Vec<Expression> {
        let node_morphs = |old: usize| root.nodes.get(old).and_then(|n| n.mesh).and_then(|m| morphs.get(m)).copied();
        match self {
            Vrm::V0(v) => {
                let mut users: HashMap<usize, Vec<(usize, usize)>> = HashMap::new();
                for (old, n) in root.nodes.iter().enumerate() {
                    if let (Some(mesh), Some(new)) = (n.mesh, node_map.get(old).copied().flatten()) {
                        users.entry(mesh).or_default().push((old, new));
                    }
                }
                v.blend_shapes
                    .groups
                    .iter()
                    .map(|g| {
                        let preset = vrm0_preset(&g.preset);
                        let name =
                            if g.name.is_empty() { preset.unwrap_or("expression").to_string() } else { g.name.clone() };
                        let mut binds = Vec::new();
                        for b in &g.binds {
                            let (Some(mesh), Some(target)) = (b.mesh, b.index) else { continue };
                            let (Ok(mesh), Ok(target)) = (usize::try_from(mesh), usize::try_from(target)) else {
                                continue;
                            };
                            for &(old, node) in users.get(&mesh).into_iter().flatten() {
                                if node_morphs(old).is_some_and(|m| target < m) {
                                    binds.push(MorphBind { node, target, weight: b.weight.unwrap_or(100.0) / 100.0 });
                                }
                            }
                        }
                        Expression { name, preset: preset.map(String::from), binds }
                    })
                    .collect()
            }
            Vrm::V1(v) => {
                let convert = |(name, e): (&String, &Vrm1Expression), preset: bool| Expression {
                    name: name.clone(),
                    preset: preset.then(|| name.clone()),
                    binds: e
                        .binds
                        .iter()
                        .filter_map(|b| {
                            let (old, target) = (b.node?, b.index?);
                            let node = node_map.get(old).copied().flatten()?;
                            node_morphs(old).is_some_and(|m| target < m).then(|| MorphBind {
                                node,
                                target,
                                weight: b.weight.unwrap_or(1.0),
                            })
                        })
                        .collect(),
                };
                let presets = v.expressions.preset.iter().map(|e| convert(e, true));
                presets.chain(v.expressions.custom.iter().map(|e| convert(e, false))).collect()
            }
            _ => Vec::new(),
        }
    }
}

/// VRM 0.x bone names are VRM 1.0's, except that the thumb's three bones
/// were renamed one step down the finger.
fn vrm0_bone_name(name: &str) -> String {
    let mut chars = name.chars();
    let name: String = match chars.next() {
        Some(c) => c.to_lowercase().chain(chars).collect(),
        None => String::new(),
    };
    match name.as_str() {
        "leftThumbProximal" => "leftThumbMetacarpal".into(),
        "leftThumbIntermediate" => "leftThumbProximal".into(),
        "rightThumbProximal" => "rightThumbMetacarpal".into(),
        "rightThumbIntermediate" => "rightThumbProximal".into(),
        _ => name,
    }
}

/// VRM 0.x expression presets in VRM 1.0's names.
fn vrm0_preset(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        "neutral" => "neutral",
        "a" => "aa",
        "i" => "ih",
        "u" => "ou",
        "e" => "ee",
        "o" => "oh",
        "blink" => "blink",
        "blink_l" => "blinkLeft",
        "blink_r" => "blinkRight",
        "joy" => "happy",
        "angry" => "angry",
        "sorrow" => "sad",
        "fun" => "relaxed",
        "lookup" => "lookUp",
        "lookdown" => "lookDown",
        "lookleft" => "lookLeft",
        "lookright" => "lookRight",
        _ => return None,
    })
}

fn srgb_to_linear(c: Vec3) -> Vec3 {
    let f = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    Vec3::new(f(c.x), f(c.y), f(c.z))
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vrm0_names() {
        assert_eq!(vrm0_bone_name("leftThumbProximal"), "leftThumbMetacarpal");
        assert_eq!(vrm0_bone_name("RightThumbIntermediate"), "rightThumbProximal");
        assert_eq!(vrm0_bone_name("leftThumbDistal"), "leftThumbDistal");
        assert_eq!(vrm0_bone_name("Hips"), "hips");
        assert_eq!(vrm0_bone_name(""), "");
        assert_eq!(vrm0_preset("Joy"), Some("happy"));
        assert_eq!(vrm0_preset("blink_l"), Some("blinkLeft"));
        assert_eq!(vrm0_preset("unknown"), None);
    }
}
