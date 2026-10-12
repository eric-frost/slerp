//! USD: text layers (`.usda`), binary crate layers (`.usdc`), and USDZ
//! packages, read by our own focused readers rather than a full USD library.
//!
//! The `openusd` crate was the candidate library, but its releases that
//! build with Rust 1.93 index tables and allocate counts straight from the
//! file, which panics or exhausts memory on damaged input, and it brings a
//! composition engine of 80 000 lines.  A thumbnailer needs much less:
//! prims, their properties and metadata, references, payloads, sublayers,
//! inherits and variant selections, composed in a simplified strength order.
//!
//! - `usda` and `usdc` read a layer into `Layer`.
//! - `stage` composes layers into a tree of `CPrim`s.
//! - `shade` reads a Material's shading network into the shared shader graph.
//! - `geom` turns meshes, simple shapes and transforms into a `Scene`.
//! - `skel` adds UsdSkel: skeletons as joint nodes, skins, blend shapes as
//!   morph targets, and SkelAnimations and animated transforms as clips.
//! - `instancer` expands point instancers into nodes sharing their
//!   prototypes' meshes.

mod geom;
mod instancer;
mod mtlx_layer;
mod shade;
mod skel;
mod stage;
mod usda;
mod usdc;

use crate::MaterialFormat;
use crate::realize::FileRef;
use slerp_archive::Archive;
use slerp_scene::{Asset, Info, Limits, LoadError};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The most layers one stage may pull in through sublayers, references and
/// payloads.
const MAX_LAYERS: usize = 256;

/*┌─────────────────╮
| Layers          |
└─────────────────╯*/
//region Layers

/// A property or metadata value, simplified to what Slerp uses.
/// Quaternions are stored real part first, as `.usda` writes them; matrices
/// row by row; doubles and halves as `f32` in arrays.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum Value {
    #[default]
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// A token or string.
    Str(String),
    Asset(String),
    Path(String),
    /// A vector, colour, quaternion or matrix.
    Tuple(Vec<f64>),
    /// A numeric array, flattened, with this many numbers per element.
    Floats(Vec<f32>, usize),
    Ints(Vec<i32>),
    Strs(Vec<String>),
    Paths(Vec<String>),
    Dict(Vec<(String, Value)>),
}

impl Value {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Int(i) => Some(*i as f64),
            Value::Bool(b) => Some(*b as i64 as f64),
            Value::Tuple(t) if t.len() == 1 => Some(t[0]),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) | Value::Asset(s) => Some(s),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(d) => d.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

/// A reference or payload: another layer's prim (or this layer's, when
/// `asset` is empty).  An empty `prim` means the layer's default prim.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Reference {
    pub asset: String,
    pub prim: String,
}

/// An attribute's values over time: time codes in increasing order and the
/// value at each.  Blocked samples (`None`) are left out.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Samples {
    pub times: Vec<f64>,
    pub values: Vec<Arc<Value>>,
}

/// A time-sampled property as read: the value at the earliest time, and
/// every sample when they are kept.
pub(crate) type TimeSampled = (Option<Arc<Value>>, Option<Arc<Samples>>);

/// Whether readers keep every time sample of a property, rather than only
/// the earliest.  Only animation that Slerp plays is kept: a SkelAnimation's
/// joint transforms and blend shape weights, and transform operations.
/// Other attributes (time-varying points of a simulation cache, say) can
/// hold far more data than a viewer needs.
pub(crate) fn keeps_samples(name: &str) -> bool {
    matches!(name, "translations" | "rotations" | "scales" | "blendShapeWeights") || name.starts_with("xformOp:")
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Property {
    pub name: String,
    /// Such as "float3[]"; empty for relationships.
    pub type_name: String,
    pub default: Option<Arc<Value>>,
    /// The value at the earliest time sample, which wins over the default
    /// as samples do at any time in USD.
    pub sample: Option<Arc<Value>>,
    /// Every time sample, for the properties `keeps_samples` names and only
    /// when the layer was read with samples wanted.
    pub samples: Option<Arc<Samples>>,
    /// Connection sources of an attribute, or targets of a relationship.
    pub targets: Vec<String>,
    pub interpolation: Option<String>,
    pub element_size: Option<i64>,
    pub color_space: Option<String>,
    /// A material binding authored `bindMaterialAs = "strongerThanDescendants"`.
    pub stronger: Option<bool>,
    /// The `weight` metadata of a blend shape's inbetween shape.
    pub weight: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Specifier {
    #[default]
    Over,
    Def,
    Class,
}

/// One prim's opinions in one layer.  Variants are separate specs at paths
/// such as `/Prim{set=variant}`, and their children at
/// `/Prim{set=variant}Child`, as in USD's own layers.
#[derive(Clone, Debug, Default)]
pub(crate) struct PrimSpec {
    pub specifier: Specifier,
    pub type_name: String,
    pub children: Vec<String>,
    pub props: Vec<Property>,
    pub api_schemas: Vec<String>,
    pub references: Vec<Reference>,
    pub payloads: Vec<Reference>,
    pub inherits: Vec<String>,
    pub variant_sets: Vec<String>,
    pub variant_selection: Vec<(String, String)>,
    pub active: Option<bool>,
    pub asset_info: Option<Value>,
    /// A point instancer's `inactiveIds` metadata: instances switched off.
    pub inactive_ids: Option<Vec<i64>>,
}

impl PrimSpec {
    pub fn prop_mut(&mut self, name: &str) -> &mut Property {
        match self.props.iter().position(|p| p.name == name) {
            Some(i) => &mut self.props[i],
            None => {
                self.props.push(Property { name: name.to_string(), ..Default::default() });
                self.props.last_mut().expect("just pushed")
            }
        }
    }
}

/// One layer: prim specs by path (`/` holds the layer's root prims as
/// children) and the layer metadata Slerp uses.
#[derive(Debug, Default)]
pub(crate) struct Layer {
    pub prims: HashMap<String, PrimSpec>,
    pub default_prim: Option<String>,
    pub up_axis: Option<String>,
    pub meters_per_unit: Option<f64>,
    pub sublayers: Vec<String>,
    /// "USD text 1.0" or "USD crate 0.8.0".
    pub format: String,
    /// How many time codes make a second, and the frame rate, which stands
    /// in for it when it is missing.
    pub time_codes_per_second: Option<f64>,
    pub frames_per_second: Option<f64>,
    /// Where playback starts, in time codes (`startTimeCode`, or the older
    /// `startFrame`).
    pub start_time: Option<f64>,
}

impl Layer {
    pub fn prim_mut(&mut self, path: &str) -> &mut PrimSpec {
        self.prims.entry(path.to_string()).or_default()
    }

    /// The path of the prim a reference without a prim path means.
    pub fn default_prim_path(&self) -> Option<String> {
        match &self.default_prim {
            Some(name) if !name.is_empty() => {
                Some(if name.starts_with('/') { name.clone() } else { format!("/{name}") })
            }
            _ => self.prims.get("/")?.children.first().map(|c| format!("/{c}")),
        }
    }
}

/// Reads a layer, text or binary, from its bytes.  `budget` is how many
/// more bytes binary layers may allocate for decoded tables and arrays;
/// `samples` keeps every time sample of the properties `keeps_samples`
/// names (the viewer plays them; thumbnails don't need them).
fn parse_layer(bytes: &[u8], budget: &mut u64, samples: bool) -> Result<Layer, LoadError> {
    if bytes.starts_with(b"PXR-USDC") {
        usdc::read(bytes, budget, samples)
    } else if bytes.starts_with(b"#usda") {
        usda::parse(bytes, samples)
    } else {
        Err(LoadError::invalid("not a USD layer"))
    }
}

//endregion

/*┌─────────────────╮
| Loading         |
└─────────────────╯*/
//region Loading

/// Where a layer lives: a file, or an entry of the package being loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Loc {
    Disk(PathBuf),
    Entry(String),
}

pub(crate) struct Ctx<'a> {
    pub limits: &'a Limits,
    /// The package when loading a USDZ.
    pub package: Option<Archive>,
    pub layers: Vec<(Loc, Layer)>,
    /// Each loaded layer's layer stack: itself and its sublayers.
    pub stacks: HashMap<usize, Vec<usize>>,
    pub budget: u64,
    pub warnings: Vec<String>,
}

impl Ctx<'_> {
    /// The package's entry called `name`, compared without case.
    fn entry(&self, name: &str) -> Option<&slerp_archive::Entry> {
        let package = self.package.as_ref()?;
        let at = *package.find(name).first()?;
        package.entry(at)
    }

    fn warn(&mut self, msg: String) {
        if self.warnings.len() < 100 && !self.warnings.contains(&msg) {
            self.warnings.push(msg);
        }
    }

    /// Where an asset path written in `layer` points.
    fn locate(&self, layer: usize, asset: &str) -> Option<Loc> {
        let asset = asset.replace("<UDIM>", "1001").replace('\\', "/");
        if asset.is_empty() || asset.contains('[') || asset.contains("://") {
            return None;
        }
        match &self.layers.get(layer)?.0 {
            Loc::Entry(base) => Some(Loc::Entry(join(base, &asset))),
            Loc::Disk(path) => {
                let p = Path::new(&asset);
                Some(Loc::Disk(if p.is_absolute() { p.to_path_buf() } else { path.parent()?.join(p) }))
            }
        }
    }

    /// The file an asset-valued property names, for textures and previews.
    pub fn file_ref(&self, layer: usize, asset: &str) -> FileRef {
        let missing = || FileRef::Embedded { name: asset.to_string(), index: usize::MAX };
        match self.locate(layer, asset) {
            Some(Loc::Disk(p)) => FileRef::Disk(p),
            Some(Loc::Entry(name)) => match self.entry(&name) {
                Some(e) => FileRef::Embedded { name: e.path.clone(), index: e.index },
                None => missing(),
            },
            None => missing(),
        }
    }

    /// A layer and its sublayers (recursively, loading them), strongest
    /// first: what a reference to the layer brings in.
    pub fn stack(&mut self, layer: usize) -> Vec<usize> {
        if let Some(s) = self.stacks.get(&layer) {
            return s.clone();
        }
        let mut stack = vec![layer];
        let mut i = 0;
        while i < stack.len() && stack.len() < 64 {
            let subs = self.layers[stack[i]].1.sublayers.clone();
            for s in subs {
                if let Some(l) = self.load_layer(stack[i], &s)
                    && !stack.contains(&l)
                {
                    stack.push(l);
                }
            }
            i += 1;
        }
        self.stacks.insert(layer, stack.clone());
        stack
    }

    /// Loads (once) the layer an asset path in `from` names, for
    /// sublayers, references and payloads.
    pub fn load_layer(&mut self, from: usize, asset: &str) -> Option<usize> {
        let loc = self.locate(from, asset)?;
        if let Some(i) = self.layers.iter().position(|(l, _)| *l == loc) {
            return Some(i);
        }
        if self.layers.len() >= MAX_LAYERS {
            self.warn("too many layers; the rest were skipped".into());
            return None;
        }
        let result = match &loc {
            Loc::Entry(name) => match self.entry(name).cloned() {
                Some(e) => match self.package.as_ref().map(|p| p.read(&e)) {
                    Some(Ok(bytes)) => parse_layer(&bytes, &mut self.budget, self.limits.animations),
                    Some(Err(err)) => Err(LoadError::invalid(format!("{}: {err}", e.path))),
                    None => Err(LoadError::invalid("not in the package")),
                },
                None => Err(LoadError::invalid("not in the package")),
            },
            Loc::Disk(path) => {
                let lower = path.to_string_lossy().to_ascii_lowercase();
                if self.limits.neighbours {
                    slerp_scene::record_neighbour(path);
                }
                if !self.limits.neighbours {
                    Err(LoadError::Unsupported("neighbouring files are off".into()))
                } else if lower.ends_with(".usdz") {
                    Err(LoadError::Unsupported("references into other USDZ packages aren't read yet".into()))
                } else {
                    match std::fs::metadata(path) {
                        Ok(m) if m.len() > self.limits.max_file_bytes || m.len() > self.budget => {
                            Err(LoadError::TooLarge(format!("{} bytes", m.len())))
                        }
                        Ok(_) => std::fs::read(path).map_err(LoadError::from).and_then(|bytes| {
                            self.budget = self.budget.saturating_sub(bytes.len() as u64);
                            // USD reads MaterialX documents as layers too.
                            if lower.ends_with(".mtlx") {
                                mtlx_layer::layer(path, &bytes, self.limits.neighbours)
                            } else {
                                parse_layer(&bytes, &mut self.budget, self.limits.animations)
                            }
                        }),
                        Err(e) => Err(e.into()),
                    }
                }
            }
        };
        match result {
            Ok(layer) => {
                self.layers.push((loc, layer));
                Some(self.layers.len() - 1)
            }
            Err(e) => {
                self.warn(format!("layer {asset}: {e}"));
                None
            }
        }
    }
}

/// Reads the root layer (the file, or a package's first layer) and composes
/// the stage.
fn open<'a>(
    format: MaterialFormat,
    path: &Path,
    bytes: &'a [u8],
    limits: &'a Limits,
) -> Result<(Ctx<'a>, stage::Stage), LoadError> {
    // Decoded tables and arrays may take this many bytes: generous for
    // real files, whose arrays compress little, but bounded.
    let budget = (bytes.len() as u64).saturating_mul(64).saturating_add(64 << 20).min(4 << 30);
    let mut ctx =
        Ctx { limits, package: None, layers: Vec::new(), stacks: HashMap::new(), budget, warnings: Vec::new() };

    // 1. Read the root layer: the file itself, or a package's first layer.
    let (root_loc, root_bytes): (Loc, std::borrow::Cow<[u8]>) = if format == MaterialFormat::Usdz {
        let package = Archive::from_bytes(bytes).map_err(|e| LoadError::invalid(format!("USDZ package: {e}")))?;
        let first = package
            .entries()
            .iter()
            .find(|e| {
                let n = e.path.to_ascii_lowercase();
                n.ends_with(".usdc") || n.ends_with(".usda") || n.ends_with(".usd")
            })
            .ok_or_else(|| LoadError::invalid("USDZ package without a layer"))?;
        let loc = Loc::Entry(first.path.clone());
        let layer = package.read(first).map_err(|e| LoadError::invalid(format!("USDZ package: {e}")))?;
        ctx.package = Some(package);
        (loc, layer.into())
    } else {
        (Loc::Disk(path.to_path_buf()), bytes.into())
    };
    let root = parse_layer(&root_bytes, &mut ctx.budget, limits.animations)?;
    ctx.layers.push((root_loc, root));

    let stage = stage::compose(&mut ctx)?;
    Ok((ctx, stage))
}

pub(crate) fn load(format: MaterialFormat, path: &Path, bytes: &[u8], limits: &Limits) -> Result<Asset, LoadError> {
    let (mut ctx, stage) = open(format, path, bytes, limits)?;
    let layer_format = ctx.layers[0].1.format.clone();
    let mut info = Info {
        format: if format == MaterialFormat::Usdz { format!("USDZ ({layer_format})") } else { layer_format },
        ..Default::default()
    };
    let name = stage
        .default_prim
        .map(|i| stage.prims[i].name.clone())
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let preview = stage.preview_image(&ctx);

    if geom::has_geometry(&stage) {
        let mut scene = geom::build(&mut ctx, &stage, name)?;
        if let Some((_, label)) = &preview {
            scene.info.details.push(("Preview".into(), label.clone()));
        }
        scene.info.format = info.format;
        scene.info.warnings.splice(0..0, ctx.warnings);
        return Ok(Asset::Scene(scene));
    }

    // 3. A stage of materials only: show its default (or first) material.
    let materials = stage.materials();
    let Some(&mat) = stage.default_prim.filter(|i| materials.contains(i)).as_ref().or(materials.first()) else {
        return Err(LoadError::Unsupported("USD stage without meshes or materials".into()));
    };
    let (desc, _) = shade::material_desc(&mut ctx, &stage, mat);
    if materials.len() > 1 {
        let names: Vec<&str> = materials.iter().take(20).map(|&i| stage.prims[i].name.as_str()).collect();
        info.details.push(("Materials".into(), names.join(", ")));
    }
    info.details.push(("Up axis".into(), if stage.up_z { "Z" } else { "Y" }.into()));
    info.warnings = std::mem::take(&mut ctx.warnings);
    Ok(crate::realize::material_asset(path, ctx.package.as_ref(), desc, info, preview, limits))
}

//endregion

/// The first material of a USD file as read, before textures are decoded.
#[cfg(test)]
pub(crate) fn test_material_desc(path: &Path) -> Result<crate::realize::MatDesc, LoadError> {
    let bytes = std::fs::read(path)?;
    let limits = Limits::viewer();
    let format = crate::sniff(path, &bytes).ok_or(LoadError::NotMine)?;
    let (mut ctx, stage) = open(format, path, &bytes, &limits)?;
    let mat = *stage.materials().first().ok_or(LoadError::NotMine)?;
    Ok(shade::material_desc(&mut ctx, &stage, mat).0)
}

#[cfg(test)]
pub(crate) fn usda_for_test(bytes: &[u8]) -> Result<Layer, LoadError> {
    usda::parse(bytes, true)
}

#[cfg(test)]
pub(crate) fn usdc_for_test(bytes: &[u8], budget: &mut u64) -> Result<Layer, LoadError> {
    usdc::read(bytes, budget, true)
}

/// Checks that two layers hold the same specs and values, allowing for
/// float rounding and dictionary order.
#[cfg(test)]
pub(crate) fn assert_layers_match(a: &Layer, b: &Layer, name: &str) {
    fn close(x: f64, y: f64) -> bool {
        (x - y).abs() <= 1e-6 * x.abs().max(y.abs()).max(1.0)
    }
    fn same(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Float(x), Value::Float(y)) => close(*x, *y),
            (Value::Int(x), Value::Float(y)) | (Value::Float(y), Value::Int(x)) => close(*x as f64, *y),
            (Value::Tuple(x), Value::Tuple(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| close(*p, *q)),
            (Value::Floats(x, w), Value::Floats(y, v)) => {
                (x.is_empty() && y.is_empty())
                    || (w == v && x.len() == y.len() && x.iter().zip(y).all(|(p, q)| close(*p as f64, *q as f64)))
            }
            (Value::Dict(x), Value::Dict(y)) => {
                let mut x: Vec<_> = x.iter().collect();
                let mut y: Vec<_> = y.iter().collect();
                x.sort_by(|p, q| p.0.cmp(&q.0));
                y.sort_by(|p, q| p.0.cmp(&q.0));
                x.len() == y.len() && x.iter().zip(&y).all(|(p, q)| p.0 == q.0 && same(&p.1, &q.1))
            }
            _ => a == b,
        }
    }
    let opt = |x: &Option<std::sync::Arc<Value>>, y: &Option<std::sync::Arc<Value>>| match (x, y) {
        (Some(x), Some(y)) => same(x, y),
        (None, None) => true,
        _ => false,
    };
    assert_eq!(a.default_prim, b.default_prim, "{name}: defaultPrim");
    assert_eq!(a.up_axis, b.up_axis, "{name}: upAxis");
    assert_eq!(a.meters_per_unit, b.meters_per_unit, "{name}: metersPerUnit");
    assert_eq!(a.time_codes_per_second, b.time_codes_per_second, "{name}: timeCodesPerSecond");
    assert_eq!(a.frames_per_second, b.frames_per_second, "{name}: framesPerSecond");
    assert_eq!(a.start_time, b.start_time, "{name}: startTimeCode");
    let mut keys: Vec<_> = a.prims.keys().collect();
    keys.sort();
    let mut bkeys: Vec<_> = b.prims.keys().collect();
    bkeys.sort();
    assert_eq!(keys, bkeys, "{name}: prim specs");
    for k in keys {
        let (p, q) = (&a.prims[k], &b.prims[k]);
        let at = format!("{name} {k}");
        assert_eq!(p.specifier, q.specifier, "{at}: specifier");
        assert_eq!(p.type_name, q.type_name, "{at}: type");
        assert_eq!(p.children, q.children, "{at}: children");
        assert_eq!(p.api_schemas, q.api_schemas, "{at}: apiSchemas");
        assert_eq!(p.references, q.references, "{at}: references");
        assert_eq!(p.payloads, q.payloads, "{at}: payloads");
        assert_eq!(p.inherits, q.inherits, "{at}: inherits");
        assert_eq!(p.variant_sets, q.variant_sets, "{at}: variant sets");
        let mut vs = p.variant_selection.clone();
        let mut ws = q.variant_selection.clone();
        vs.sort();
        ws.sort();
        assert_eq!(vs, ws, "{at}: variant selection");
        assert_eq!(p.active, q.active, "{at}: active");
        assert_eq!(p.inactive_ids, q.inactive_ids, "{at}: inactiveIds");
        match (&p.asset_info, &q.asset_info) {
            (Some(x), Some(y)) => assert!(same(x, y), "{at}: assetInfo {x:?} vs {y:?}"),
            (x, y) => assert_eq!(x.is_some(), y.is_some(), "{at}: assetInfo"),
        }
        let mut names: Vec<_> = p.props.iter().map(|x| &x.name).collect();
        names.sort();
        let mut qnames: Vec<_> = q.props.iter().map(|x| &x.name).collect();
        qnames.sort();
        assert_eq!(names, qnames, "{at}: properties");
        for x in &p.props {
            let y = q.props.iter().find(|y| y.name == x.name).unwrap();
            let at = format!("{at}.{}", x.name);
            assert_eq!(x.type_name, y.type_name, "{at}: type");
            assert_eq!(x.targets, y.targets, "{at}: targets");
            assert_eq!(x.interpolation, y.interpolation, "{at}: interpolation");
            assert_eq!(x.element_size, y.element_size, "{at}: elementSize");
            assert!(opt(&x.default, &y.default), "{at}: default {:?} vs {:?}", x.default, y.default);
            assert!(opt(&x.sample, &y.sample), "{at}: sample {:?} vs {:?}", x.sample, y.sample);
            assert_eq!(x.weight, y.weight, "{at}: weight");
            match (&x.samples, &y.samples) {
                (Some(s), Some(t)) => {
                    assert_eq!(s.times, t.times, "{at}: sample times");
                    let all = s.values.iter().zip(&t.values).all(|(v, w)| same(v, w));
                    assert!(all && s.values.len() == t.values.len(), "{at}: samples {s:?} vs {t:?}");
                }
                (s, t) => assert_eq!(s.is_some(), t.is_some(), "{at}: samples"),
            }
        }
    }
}

/// A named material of a USD file as read, before textures are decoded.
#[cfg(test)]
pub(crate) fn test_material_desc_named(path: &Path, name: &str) -> Result<crate::realize::MatDesc, LoadError> {
    let bytes = std::fs::read(path)?;
    let limits = Limits::viewer();
    let format = crate::sniff(path, &bytes).ok_or(LoadError::NotMine)?;
    let (mut ctx, stage) = open(format, path, &bytes, &limits)?;
    let mat = stage.materials().into_iter().find(|&m| stage.prims[m].name == name).ok_or(LoadError::NotMine)?;
    Ok(shade::material_desc(&mut ctx, &stage, mat).0)
}

/// Joins a path relative to a package entry, resolving `.` and `..`.
fn join(base_entry: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = match base_entry.rsplit_once('/') {
        Some((dir, _)) => dir.split('/').collect(),
        None => Vec::new(),
    };
    let rel = rel.strip_prefix("./").unwrap_or(rel);
    if rel.starts_with('/') {
        parts.clear();
    }
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}
