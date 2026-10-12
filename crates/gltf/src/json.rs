//! The parts of glTF's JSON that Slerp reads, as serde structs.
//!
//! These are written here rather than taken from `gltf-json` because that
//! crate requires `texture.source`, which files using only
//! `KHR_texture_basisu` leave out, and because unknown fields and extensions
//! must never fail a load: anything not declared is skipped.  Large
//! extension objects that are read only sometimes (VRM's) stay as raw JSON
//! until needed.

use serde::Deserialize;
use serde_json::value::RawValue;
use std::collections::HashMap;

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Root {
    pub asset: AssetJ,
    pub extensions_used: Vec<String>,
    pub extensions_required: Vec<String>,
    pub scene: Option<usize>,
    pub scenes: Vec<SceneJ>,
    pub nodes: Vec<NodeJ>,
    pub meshes: Vec<MeshJ>,
    pub accessors: Vec<AccessorJ>,
    pub buffer_views: Vec<BufferViewJ>,
    pub buffers: Vec<BufferJ>,
    pub materials: Vec<MaterialJ>,
    pub textures: Vec<TextureJ>,
    pub images: Vec<ImageJ>,
    pub samplers: Vec<SamplerJ>,
    pub skins: Vec<SkinJ>,
    pub animations: Vec<AnimationJ>,
    pub extensions: RootExtensions,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct AssetJ {
    pub version: String,
    pub min_version: Option<String>,
    pub generator: Option<String>,
    pub copyright: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct RootExtensions {
    #[serde(rename = "KHR_lights_punctual")]
    pub lights: Option<LightsJ>,
    #[serde(rename = "VRM")]
    pub vrm0: Option<Box<RawValue>>,
    #[serde(rename = "VRMC_vrm")]
    pub vrm1: Option<Box<RawValue>>,
    #[serde(rename = "VRMC_vrm_animation")]
    pub vrma: Option<Box<RawValue>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct LightsJ {
    pub lights: Vec<LightJ>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct LightJ {
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct SceneJ {
    pub name: Option<String>,
    pub nodes: Vec<usize>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct NodeJ {
    pub name: Option<String>,
    pub children: Vec<usize>,
    pub mesh: Option<usize>,
    pub skin: Option<usize>,
    pub matrix: Option<[f32; 16]>,
    pub translation: Option<[f32; 3]>,
    pub rotation: Option<[f32; 4]>,
    pub scale: Option<[f32; 3]>,
    pub weights: Option<Vec<f32>>,
    pub extensions: NodeExtensions,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct NodeExtensions {
    #[serde(rename = "EXT_mesh_gpu_instancing")]
    pub instancing: Option<InstancingJ>,
    #[serde(rename = "KHR_node_visibility")]
    pub visibility: Option<VisibilityJ>,
}

/// `KHR_node_visibility`: a hidden node hides its whole subtree.
#[derive(Deserialize)]
#[serde(default)]
pub struct VisibilityJ {
    pub visible: bool,
}

impl Default for VisibilityJ {
    fn default() -> Self {
        VisibilityJ { visible: true }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct InstancingJ {
    pub attributes: HashMap<String, usize>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct MeshJ {
    pub name: Option<String>,
    pub primitives: Vec<PrimitiveJ>,
    pub weights: Option<Vec<f32>>,
    pub extras: Option<MeshExtras>,
}

/// `targetNames` is the de facto place for morph target names (Blender,
/// three.js and glTF-Transform all write it).  Any other extras, or extras
/// of an unexpected shape, are ignored.
#[derive(Default)]
pub struct MeshExtras {
    pub target_names: Vec<String>,
}

impl<'de> Deserialize<'de> for MeshExtras {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Strict {
            #[serde(rename = "targetNames")]
            target_names: Vec<String>,
        }
        let value = serde_json::Value::deserialize(d)?;
        Ok(MeshExtras {
            target_names: serde_json::from_value::<Strict>(value).map(|s| s.target_names).unwrap_or_default(),
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct PrimitiveJ {
    pub attributes: HashMap<String, usize>,
    pub indices: Option<usize>,
    pub material: Option<usize>,
    pub mode: Option<u32>,
    pub targets: Vec<HashMap<String, usize>>,
    pub extensions: PrimitiveExtensions,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct PrimitiveExtensions {
    #[serde(rename = "KHR_draco_mesh_compression")]
    pub draco: Option<DracoJ>,
}

/// `KHR_draco_mesh_compression`: the buffer view holding the compressed
/// geometry, and the Draco attribute id of each glTF attribute it holds.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct DracoJ {
    pub buffer_view: usize,
    pub attributes: HashMap<String, u32>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct AccessorJ {
    pub buffer_view: Option<usize>,
    pub byte_offset: u64,
    pub component_type: u32,
    pub normalized: bool,
    pub count: u64,
    #[serde(rename = "type")]
    pub kind: String,
    pub sparse: Option<SparseJ>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct SparseJ {
    pub count: u64,
    pub indices: SparseIndicesJ,
    pub values: SparseValuesJ,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct SparseIndicesJ {
    pub buffer_view: usize,
    pub byte_offset: u64,
    pub component_type: u32,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct SparseValuesJ {
    pub buffer_view: usize,
    pub byte_offset: u64,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct BufferViewJ {
    pub buffer: usize,
    pub byte_offset: u64,
    pub byte_length: u64,
    pub byte_stride: Option<u64>,
    pub extensions: BufferViewExtensions,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct BufferViewExtensions {
    #[serde(rename = "EXT_meshopt_compression")]
    pub meshopt_ext: Option<MeshoptJ>,
    #[serde(rename = "KHR_meshopt_compression")]
    pub meshopt_khr: Option<MeshoptJ>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct MeshoptJ {
    pub buffer: usize,
    pub byte_offset: u64,
    pub byte_length: u64,
    pub byte_stride: u64,
    pub count: u64,
    pub mode: String,
    pub filter: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct BufferJ {
    pub uri: Option<String>,
    pub byte_length: u64,
    pub extensions: BufferExtensions,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct BufferExtensions {
    #[serde(rename = "EXT_meshopt_compression")]
    pub meshopt_ext: Option<FallbackJ>,
    #[serde(rename = "KHR_meshopt_compression")]
    pub meshopt_khr: Option<FallbackJ>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct FallbackJ {
    pub fallback: bool,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ImageJ {
    pub name: Option<String>,
    pub uri: Option<String>,
    pub mime_type: Option<String>,
    pub buffer_view: Option<usize>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct TextureJ {
    pub sampler: Option<usize>,
    pub source: Option<usize>,
    pub extensions: TextureExtensions,
}

/// Alternative image sources, in the order they are preferred.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct TextureExtensions {
    #[serde(rename = "KHR_texture_basisu")]
    pub basisu: Option<SourceJ>,
    #[serde(rename = "EXT_texture_webp")]
    pub webp: Option<SourceJ>,
    #[serde(rename = "MSFT_texture_dds")]
    pub dds: Option<SourceJ>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct SourceJ {
    pub source: Option<usize>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct SamplerJ {
    pub mag_filter: Option<u32>,
    pub min_filter: Option<u32>,
    pub wrap_s: Option<u32>,
    pub wrap_t: Option<u32>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct MaterialJ {
    pub name: Option<String>,
    pub pbr_metallic_roughness: Option<PbrJ>,
    pub normal_texture: Option<TextureInfoJ>,
    pub occlusion_texture: Option<TextureInfoJ>,
    pub emissive_texture: Option<TextureInfoJ>,
    pub emissive_factor: Option<[f32; 3]>,
    pub alpha_mode: Option<String>,
    pub alpha_cutoff: Option<f32>,
    pub double_sided: bool,
    pub extensions: MaterialExtensions,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct PbrJ {
    pub base_color_factor: Option<[f32; 4]>,
    pub base_color_texture: Option<TextureInfoJ>,
    pub metallic_factor: Option<f32>,
    pub roughness_factor: Option<f32>,
    pub metallic_roughness_texture: Option<TextureInfoJ>,
}

/// A material's reference to a texture.  `scale` is the normal texture's
/// strength and `strength` the occlusion texture's; other slots have
/// neither.
#[derive(Deserialize, Default, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct TextureInfoJ {
    pub index: Option<usize>,
    pub tex_coord: Option<u32>,
    pub scale: Option<f32>,
    pub strength: Option<f32>,
    pub extensions: TextureInfoExtensions,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct TextureInfoExtensions {
    #[serde(rename = "KHR_texture_transform")]
    pub transform: Option<TextureTransformJ>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct TextureTransformJ {
    pub offset: Option<[f32; 2]>,
    pub rotation: Option<f32>,
    pub scale: Option<[f32; 2]>,
    pub tex_coord: Option<u32>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct MaterialExtensions {
    #[serde(rename = "KHR_materials_emissive_strength")]
    pub emissive_strength: Option<EmissiveStrengthJ>,
    #[serde(rename = "KHR_materials_specular")]
    pub specular: Option<SpecularJ>,
    #[serde(rename = "KHR_materials_ior")]
    pub ior: Option<IorJ>,
    #[serde(rename = "KHR_materials_clearcoat")]
    pub clearcoat: Option<ClearcoatJ>,
    #[serde(rename = "KHR_materials_transmission")]
    pub transmission: Option<TransmissionJ>,
    #[serde(rename = "KHR_materials_volume")]
    pub volume: Option<VolumeJ>,
    #[serde(rename = "KHR_materials_unlit")]
    pub unlit: Option<serde::de::IgnoredAny>,
    #[serde(rename = "KHR_materials_pbrSpecularGlossiness")]
    pub spec_gloss: Option<SpecGlossJ>,
    #[serde(rename = "VRMC_materials_mtoon")]
    pub mtoon: Option<serde::de::IgnoredAny>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct EmissiveStrengthJ {
    pub emissive_strength: Option<f32>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct SpecularJ {
    pub specular_factor: Option<f32>,
    pub specular_color_factor: Option<[f32; 3]>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct IorJ {
    pub ior: Option<f32>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ClearcoatJ {
    pub clearcoat_factor: Option<f32>,
    pub clearcoat_roughness_factor: Option<f32>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct TransmissionJ {
    pub transmission_factor: Option<f32>,
    pub transmission_texture: Option<TextureInfoJ>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct VolumeJ {
    pub thickness_factor: Option<f32>,
    pub thickness_texture: Option<TextureInfoJ>,
    pub attenuation_distance: Option<f32>,
    pub attenuation_color: Option<[f32; 3]>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct SpecGlossJ {
    pub diffuse_factor: Option<[f32; 4]>,
    pub diffuse_texture: Option<TextureInfoJ>,
    pub glossiness_factor: Option<f32>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct SkinJ {
    pub name: Option<String>,
    pub joints: Vec<usize>,
    #[serde(rename = "inverseBindMatrices")]
    pub inverse_bind_matrices: Option<usize>,
    pub skeleton: Option<usize>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct AnimationJ {
    pub name: Option<String>,
    pub channels: Vec<ChannelJ>,
    pub samplers: Vec<AnimSamplerJ>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct ChannelJ {
    pub sampler: usize,
    pub target: ChannelTargetJ,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct ChannelTargetJ {
    pub node: Option<usize>,
    pub path: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct AnimSamplerJ {
    pub input: usize,
    pub output: usize,
    pub interpolation: Option<String>,
}
