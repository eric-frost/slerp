//! Hammer's `.vmf` maps (Source engine) as levels.
//!
//! The file is KeyValues text: `versioninfo`, `visgroups`, `viewsettings`,
//! a `world` block with the worldspawn's keys and its `solid` brushes,
//! `entity` blocks (point entities, and brush entities with their own
//! solids), `hidden` blocks around solids and entities hidden in the
//! editor, `cameras` and `cordons`.  A `solid` is `side` blocks, each with
//! three points, a material, texture axes (`uaxis`, `vaxis`: an axis, a
//! shift and a scale) and possibly a `dispinfo` displacement.
//!
//! Brushes are built as for `.map` files; materials come from `.vmt` and
//! `.vtf` files beside or above the map or in the game's VPK packs.

use super::brush::{self, Plane, TexVecs};
use super::displacement::{self, DispInfo};
use super::keyvalues::{self, Kv};
use super::level::{self, Group, Surface};
use super::vmt::{self, Library, SourceMaterial, Tool};
use super::{outside, sky};
use crate::entities::Entity;
use rayon::prelude::*;
use slerp_scene::glam::{DVec3, Vec2, Vec3, Vec4};
use slerp_scene::textures::Texture;
use slerp_scene::{AlphaMode, Effect, Info, Limits, LoadError, Material, Scene, SceneKind};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// Texture size assumed for missing materials, so that at Hammer's usual
/// scale of 0.25 a checker square covers 16 units.
const MISSING_SIZE: f64 = 256.0;

/// Eye height above a Source player start, in units.
const EYE_HEIGHT: f32 = 64.0;

/*┌─────────────────╮
| Reading         |
└─────────────────╯*/
//region Reading

pub(crate) struct VmfSide {
    pub plane: Plane,
    pub material: String,
    pub vecs: TexVecs,
    pub disp: Option<DispInfo>,
}

pub(crate) struct VmfSolid {
    pub sides: Vec<VmfSide>,
    pub hidden: bool,
}

pub(crate) struct VmfEntity {
    pub keys: Entity,
    pub solids: Vec<VmfSolid>,
    pub hidden: bool,
}

pub(crate) struct Vmf {
    /// The worldspawn first, then the other entities.
    pub entities: Vec<VmfEntity>,
    pub editor_version: Option<String>,
    pub bad_sides: usize,
}

/// Whether the start of a file is a Hammer map: KeyValues text opening,
/// after any comments, with one of the blocks Hammer writes first.
pub(crate) fn sniff(head: &[u8]) -> bool {
    let head = head.strip_prefix(b"\xef\xbb\xbf").unwrap_or(head);
    let mut at = 0;
    let skip = |at: &mut usize| loop {
        while *at < head.len() && head[*at].is_ascii_whitespace() {
            *at += 1;
        }
        if head[*at..].starts_with(b"//") {
            while *at < head.len() && head[*at] != b'\n' {
                *at += 1;
            }
            continue;
        }
        return;
    };
    skip(&mut at);
    let quoted = head.get(at) == Some(&b'"');
    if quoted {
        at += 1;
    }
    let start = at;
    while at < head.len() && (head[at].is_ascii_alphanumeric() || head[at] == b'_') {
        at += 1;
    }
    let word = &head[start..at];
    if quoted {
        if head.get(at) != Some(&b'"') {
            return false;
        }
        at += 1;
    }
    let known =
        ["versioninfo", "visgroups", "viewsettings", "world"].iter().any(|b| word.eq_ignore_ascii_case(b.as_bytes()));
    skip(&mut at);
    known && head.get(at) == Some(&b'{')
}

/// Reads the map's blocks.  Solids are converted in parallel, since a
/// large map's displacements are millions of numbers.
pub(crate) fn parse(bytes: &[u8]) -> Vmf {
    let top = keyvalues::parse(bytes);
    let mut vmf = Vmf { entities: Vec::new(), editor_version: None, bad_sides: 0 };

    // 1. Entities in file order, the world first, with their solid blocks.
    struct Pending<'k, 'a> {
        keys: Entity,
        hidden: bool,
        solids: Vec<(&'k Kv<'a>, bool)>,
    }
    fn gather<'k, 'a>(k: &'k Kv<'a>, hidden: bool) -> Pending<'k, 'a> {
        let keys = Entity {
            pairs: k.children().iter().filter_map(|c| c.text().map(|v| (c.key.to_string(), v.to_string()))).collect(),
        };
        let hidden = hidden || editor_hidden(k);
        let mut solids: Vec<(&Kv, bool)> = k.blocks("solid").map(|s| (s, hidden || editor_hidden(s))).collect();
        for h in k.blocks("hidden") {
            solids.extend(h.blocks("solid").map(|s| (s, true)));
        }
        Pending { keys, hidden, solids }
    }
    let mut entities: Vec<Pending> = Vec::new();
    match top.iter().find(|k| k.key.eq_ignore_ascii_case("world")) {
        Some(w) => entities.push(gather(w, false)),
        None => entities.push(Pending {
            keys: Entity { pairs: vec![("classname".into(), "worldspawn".into())] },
            hidden: false,
            solids: Vec::new(),
        }),
    }
    for k in &top {
        if k.key.eq_ignore_ascii_case("versioninfo") {
            vmf.editor_version = k.get("editorversion").map(String::from);
        } else if k.key.eq_ignore_ascii_case("entity") {
            entities.push(gather(k, false));
        } else if k.key.eq_ignore_ascii_case("hidden") {
            entities.extend(k.blocks("entity").map(|e| gather(e, true)));
        }
    }

    // 2. Displacements are read up to a vertex count in proportion to the
    //    file: real ones spell out a dozen numbers per vertex, while a few
    //    bytes of `power 4` alone would ask for 289 vertices.  Which ones
    //    fit is decided in file order.
    let mut budget = (bytes.len() / 16).max(4096);
    let jobs: Vec<(usize, &Kv, bool, Vec<bool>)> = entities
        .iter()
        .enumerate()
        .flat_map(|(ei, e)| e.solids.iter().map(move |&(s, hidden)| (ei, s, hidden)))
        .map(|(ei, s, hidden)| {
            let allowed = s
                .blocks("side")
                .map(|side| {
                    let n =
                        side.block("dispinfo").and_then(|d| d.get("power")).and_then(|p| p.trim().parse::<u32>().ok());
                    match n.filter(|p| (2..=4).contains(p)).map(|p| ((1usize << p) + 1).pow(2)) {
                        Some(n) if n <= budget => {
                            budget -= n;
                            true
                        }
                        _ => false,
                    }
                })
                .collect();
            (ei, s, hidden, allowed)
        })
        .collect();

    // 3. Solids, in parallel.
    let solids: Vec<(usize, VmfSolid, usize)> = jobs
        .par_iter()
        .map(|(ei, s, hidden, allowed)| {
            let (mut sides, mut bad) = (Vec::new(), 0);
            for (k, side_kv) in s.blocks("side").enumerate() {
                match side(side_kv, allowed.get(k).copied().unwrap_or(false)) {
                    Some(v) => sides.push(v),
                    None => bad += 1,
                }
            }
            (*ei, VmfSolid { sides, hidden: *hidden }, bad)
        })
        .collect();
    vmf.entities =
        entities.into_iter().map(|e| VmfEntity { keys: e.keys, solids: Vec::new(), hidden: e.hidden }).collect();
    for (ei, solid, bad) in solids {
        vmf.bad_sides += bad;
        vmf.entities[ei].solids.push(solid);
    }
    vmf
}

/// Whether an object's `editor` block hides it (a hidden visgroup).
fn editor_hidden(k: &Kv) -> bool {
    k.block("editor").is_some_and(|e| e.get("visgroupshown") == Some("0") || e.get("visgroupautoshown") == Some("0"))
}

/// A side; its displacement is read only when `displace` allows.
fn side(k: &Kv, displace: bool) -> Option<VmfSide> {
    let p: Vec<f64> = keyvalues::numbers(k.get("plane")?).collect();
    if p.len() < 9 {
        return None;
    }
    let plane =
        Plane::from_points([DVec3::new(p[0], p[1], p[2]), DVec3::new(p[3], p[4], p[5]), DVec3::new(p[6], p[7], p[8])])?;
    let axis = |key: &str| -> Option<(DVec3, f64, f64)> {
        let v: Vec<f64> = keyvalues::numbers(k.get(key)?).collect();
        (v.len() >= 5).then(|| (DVec3::new(v[0], v[1], v[2]), v[3], v[4]))
    };
    let (u, us, uscale) = axis("uaxis").unwrap_or((DVec3::X, 0.0, 0.25));
    let (v, vs, vscale) = axis("vaxis").unwrap_or((-DVec3::Z, 0.0, 0.25));
    let disp = if displace { k.block("dispinfo").and_then(dispinfo) } else { None };
    Some(VmfSide {
        plane,
        material: k.get("material").unwrap_or("").to_string(),
        vecs: TexVecs::axes(u, v, [us, vs], [uscale, vscale]),
        disp,
    })
}

/// A `dispinfo` block.  Rows missing from the file are taken as flat.
fn dispinfo(k: &Kv) -> Option<DispInfo> {
    let power = k.get("power")?.trim().parse::<u32>().ok().filter(|p| (2..=4).contains(p))?;
    let n = (1usize << power) + 1;
    let start: Vec<f64> = keyvalues::numbers(k.get("startposition")?).collect();
    let start = DVec3::new(*start.first()?, *start.get(1)?, *start.get(2)?);
    let elevation =
        k.get("elevation").and_then(|e| e.trim().parse::<f64>().ok()).filter(|e| e.is_finite()).unwrap_or(0.0);
    // One row's numbers, `per` to a vertex, padded or cut to the row.
    let rows = |name: &str, per: usize, fill: f64| -> Vec<f64> {
        let block = k.block(name);
        let mut out = Vec::with_capacity(n * n * per);
        for r in 0..n {
            let mut row: Vec<f64> = block
                .and_then(|b| b.get(&format!("row{r}")))
                .map(|t| keyvalues::numbers(t).take(n * per).collect())
                .unwrap_or_default();
            row.resize(n * per, fill);
            out.extend(row);
        }
        out
    };
    let vec3s = |v: Vec<f64>| v.chunks_exact(3).map(|c| DVec3::new(c[0], c[1], c[2])).collect::<Vec<_>>();
    let alphas = if k.block("alphas").is_some() {
        rows("alphas", 1, 0.0).into_iter().map(|a| a as f32).collect()
    } else {
        Vec::new()
    };
    Some(DispInfo {
        power,
        start,
        elevation,
        normals: vec3s(rows("normals", 3, 0.0)),
        distances: rows("distances", 1, 0.0),
        offsets: vec3s(rows("offsets", 3, 0.0)),
        alphas,
    })
}

//endregion

/*┌─────────────────╮
| Materials       |
└─────────────────╯*/
//region Materials

/// A material as found: its description and decoded textures.
struct Found {
    vmt: Option<SourceMaterial>,
    base: Option<Arc<Texture>>,
    base2: Option<Arc<Texture>>,
}

/// What kind of surface a material makes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Look {
    Normal,
    Hidden,
    Sky,
    Black,
}

fn look(name: &str, found: Option<&Found>) -> Look {
    match vmt::tool(name) {
        Some(Tool::Hidden) => return Look::Hidden,
        Some(Tool::Sky) => return Look::Sky,
        Some(Tool::Black) => return Look::Black,
        None => {}
    }
    match found.and_then(|f| f.vmt.as_ref()) {
        Some(m) if m.hidden => Look::Hidden,
        Some(m) if m.sky => Look::Sky,
        _ => Look::Normal,
    }
}

/// Reads every material's `.vmt` and texture files (on this thread, so
/// they are recorded for watching), then decodes the textures in
/// parallel.
fn find_materials(lib: &Library, names: &[String], max: Option<u32>) -> HashMap<String, Found> {
    struct Read {
        name: String,
        vmt: Option<SourceMaterial>,
        base: Option<Vec<u8>>,
        base2: Option<Vec<u8>>,
    }
    let read: Vec<Read> = names
        .iter()
        .filter(|n| vmt::tool(n) != Some(Tool::Hidden))
        .map(|name| {
            let m = vmt::material(lib, name);
            let tex = |t: &Option<String>| t.as_ref().and_then(|t| lib.read(&format!("materials/{t}.vtf")));
            let (base, base2) = match &m {
                Some(m) if !m.hidden => (tex(&m.base), if m.blends() { tex(&m.base2) } else { None }),
                _ => (None, None),
            };
            Read { name: name.clone(), vmt: m, base, base2 }
        })
        .collect();
    read.into_par_iter()
        .map(|r| {
            let decode = |b: &Option<Vec<u8>>| b.as_ref().and_then(|b| crate::vtf::decode(b, max).ok()).map(Arc::new);
            let found = Found { base: decode(&r.base), base2: decode(&r.base2), vmt: r.vmt };
            (r.name, found)
        })
        .collect()
}

/// The six faces of the sky box `skyname`, through their materials.
fn sky_box(lib: &Library, skyname: &str, max: Option<u32>) -> Option<[Texture; 6]> {
    let mut faces = Vec::with_capacity(6);
    for suffix in sky::SUFFIXES {
        let m = vmt::material(lib, &format!("skybox/{skyname}{suffix}"))?;
        let bytes = lib.read(&format!("materials/{}.vtf", m.base.as_ref()?))?;
        faces.push(crate::vtf::decode(&bytes, max).ok()?);
    }
    faces.try_into().ok()
}

/// Makes the scene's materials on demand, once each.
struct Materials<'a> {
    scene: &'a mut Scene,
    found: &'a HashMap<String, Found>,
    /// Material index and the texture size coordinates are divided by, for
    /// a material and whether it's the blended layer.
    made: HashMap<(String, bool), (usize, (f64, f64))>,
    missing: Vec<String>,
    sky: Option<usize>,
    sky_box: Option<Texture>,
    /// Draw tool materials too (as checkers): a map of nothing else would
    /// otherwise show nothing at all.
    show_tools: bool,
}

impl Materials<'_> {
    fn texture(&mut self, t: &Arc<Texture>) -> usize {
        self.scene.textures.push(t.clone());
        self.scene.textures.len() - 1
    }

    fn sky(&mut self) -> usize {
        if let Some(m) = self.sky {
            return m;
        }
        let tex = self.sky_box.take().unwrap_or_else(sky::gradient);
        let t = self.texture(&Arc::new(tex));
        self.scene.materials.push(Material {
            name: "sky".into(),
            unlit: true,
            effect: Effect::PanoramaSky,
            base_color: Vec4::ONE,
            base_color_texture: Some(sky::sky_ref(t)),
            ..Default::default()
        });
        let m = self.scene.materials.len() - 1;
        self.sky = Some(m);
        m
    }

    /// The material for `name` (lower case) and the size its coordinates
    /// are divided by; the second layer of a blend when `layer`.  `None`
    /// for hidden materials and for layers of materials that don't blend.
    fn get(&mut self, name: &str, layer: bool) -> Option<(usize, (f64, f64))> {
        if let Some(&m) = self.made.get(&(name.to_string(), layer)) {
            return Some(m);
        }
        let found = self.found.get(name);
        let mut kind = look(name, found);
        if kind == Look::Hidden && self.show_tools {
            kind = Look::Normal;
        }
        let made = match kind {
            Look::Hidden => return None,
            Look::Sky => (self.sky(), (MISSING_SIZE, MISSING_SIZE)),
            Look::Black => {
                let mut m = level::matte(name);
                m.base_color = Vec4::new(0.0, 0.0, 0.0, 1.0);
                m.unlit = true;
                self.scene.materials.push(m);
                (self.scene.materials.len() - 1, (MISSING_SIZE, MISSING_SIZE))
            }
            Look::Normal => {
                let vmt = found.and_then(|f| f.vmt.as_ref());
                let tex = if layer { found.and_then(|f| f.base2.clone()) } else { found.and_then(|f| f.base.clone()) };
                if layer && tex.is_none() {
                    return None;
                }
                let mut m = level::matte(name);
                // Sizes come from the base texture, as Source measures
                // coordinates in its texels.
                let size_of = |t: &Texture| {
                    let w = if t.source.width > 0 { t.source.width } else { t.width() };
                    let h = if t.source.height > 0 { t.source.height } else { t.height() };
                    (w.max(1) as f64, h.max(1) as f64)
                };
                let size = found.and_then(|f| f.base.as_deref()).map(size_of).unwrap_or((MISSING_SIZE, MISSING_SIZE));
                match &tex {
                    Some(t) => {
                        let i = self.texture(t);
                        m.base_color_texture = Some(level::tex_ref(i));
                        if vmt.is_some_and(|v| v.selfillum)
                            && let Some(glow) = vmt::self_illumination(t)
                        {
                            let g = self.texture(&Arc::new(glow));
                            m.emissive = Vec3::ONE;
                            m.emissive_texture = Some(level::tex_ref(g));
                        }
                    }
                    None if vmt.is_some_and(|v| v.water) => {
                        m.base_color = vmt.and_then(|v| v.fog_color).unwrap_or(Vec3::new(0.15, 0.25, 0.3)).extend(1.0);
                    }
                    None => {
                        if !self.missing.iter().any(|n| n == name) {
                            self.missing.push(name.to_string());
                        }
                        let i = self.texture(&Arc::new(level::checker(name)));
                        m.base_color_texture = Some(level::tex_ref(i));
                    }
                }
                if let Some(v) = vmt {
                    m.unlit = v.unlit();
                    if let Some(cut) = v.alphatest {
                        m.alpha_mode = AlphaMode::Mask(cut);
                    }
                    if v.translucent && !v.selfillum {
                        m.alpha_mode = AlphaMode::Blend;
                    }
                    if v.alpha < 1.0 {
                        m.alpha_mode = AlphaMode::Blend;
                        m.base_color.w = v.alpha;
                    }
                    if v.water {
                        m.alpha_mode = AlphaMode::Blend;
                        m.base_color.w = 0.6;
                        m.roughness = 0.2;
                        m.specular = 0.5;
                    }
                }
                if layer {
                    m.alpha_mode = AlphaMode::Blend;
                    m.name = format!("{name} (blend)");
                }
                self.scene.materials.push(m);
                (self.scene.materials.len() - 1, size)
            }
        };
        self.made.insert((name.to_string(), layer), made);
        Some(made)
    }
}

//endregion

/*┌─────────────────╮
| Building        |
└─────────────────╯*/
//region Building

/// Brush entities the compiler merges into the world.
const WORLD_CLASSES: &[&str] = &["worldspawn", "func_detail"];

/// Classes whose brushes are never drawn.
fn skipped(class: &str) -> bool {
    class.starts_with("trigger_")
        || matches!(
            class,
            "func_areaportal" | "func_areaportalwindow" | "func_occluder" | "func_viscluster" | "func_clip_vphysics"
        )
}

pub(crate) fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{} bytes", bytes.len())));
    }
    let vmf = parse(bytes);
    let mut warnings = Vec::new();
    let mut details = Vec::new();
    let world_keys = vmf.entities[0].keys.clone();

    // 1. Materials: every one the brushes name, read once.
    let mut names: Vec<String> = vmf
        .entities
        .iter()
        .flat_map(|e| &e.solids)
        .flat_map(|s| &s.sides)
        .map(|s| s.material.replace('\\', "/").to_ascii_lowercase())
        .collect();
    names.sort_unstable();
    names.dedup();
    let lib = Library::new(path, limits);
    if !lib.is_empty() {
        details.push(("Materials from".into(), lib.describe()));
    }
    let found = if lib.is_empty() { HashMap::new() } else { find_materials(&lib, &names, limits.max_texture_size) };
    let skyname = world_keys.get("skyname").filter(|s| !s.trim().is_empty()).map(str::trim);
    let sky_faces = skyname.and_then(|s| sky_box(&lib, s, limits.max_texture_size));
    let sky_box = sky_faces.and_then(|f| sky::panorama(&f, limits.max_texture_size.map_or(4096, |m| m * 2)));

    // 2. Brush faces, built in parallel.
    let reach = vmf
        .entities
        .iter()
        .flat_map(|e| &e.solids)
        .flat_map(|s| &s.sides)
        .map(|s| s.plane.dist.abs())
        .fold(0.0f64, f64::max);
    let world = (reach * 4.0).max(brush::MIN_WORLD);
    let jobs: Vec<(usize, &VmfSolid)> =
        vmf.entities.iter().enumerate().flat_map(|(ei, e)| e.solids.iter().map(move |s| (ei, s))).collect();
    let built: Vec<_> = jobs
        .par_iter()
        .map(|(ei, s)| {
            let planes: Vec<Plane> = s.sides.iter().map(|side| side.plane).collect();
            (*ei, *s, brush::build(&planes, world))
        })
        .collect();

    // 3. The void around the level: the world's solids seal it, except
    //    detail, displacements, water and non-solid tools, and the point
    //    entities must be inside.
    let seals = |s: &VmfSolid| {
        !s.hidden
            && s.sides.iter().all(|side| {
                let name = side.material.replace('\\', "/").to_ascii_lowercase();
                side.disp.is_none()
                    && match vmt::tool(&name) {
                        Some(Tool::Hidden) => name.starts_with("tools/toolsnodraw"),
                        Some(_) => true,
                        None => !found.get(&name).and_then(|f| f.vmt.as_ref()).is_some_and(|m| m.water),
                    }
            })
    };
    let planes_of: Vec<Vec<Plane>> =
        built.iter().map(|(_, s, _)| s.sides.iter().map(|side| side.plane).collect()).collect();
    let sealers: Vec<outside::Sealer> = built
        .iter()
        .zip(&planes_of)
        .filter(|((ei, s, _), _)| *ei == 0 && seals(s))
        .filter_map(|((_, _, r), planes)| {
            let faces = r.as_ref().ok()?;
            let mut lo = DVec3::splat(f64::MAX);
            let mut hi = DVec3::splat(f64::MIN);
            for v in faces.iter().flatten().flatten() {
                lo = lo.min(*v);
                hi = hi.max(*v);
            }
            Some(outside::Sealer { planes, min: lo, max: hi })
        })
        .collect();
    let origins: Vec<DVec3> = vmf
        .entities
        .iter()
        .skip(1)
        .filter(|e| e.solids.is_empty() && !e.hidden)
        .filter_map(|e| e.keys.vec3("origin").map(|o| o.as_dvec3()))
        .collect();
    let hull = outside::Hull::new(&sealers, &origins);
    drop(sealers);

    // 4. Groups and surfaces.
    let mut scene = Scene { kind: SceneKind::Level, fill: level::FILL, ..Default::default() };
    let show_tools = !names.is_empty() && names.iter().all(|n| look(n, found.get(n)) == Look::Hidden);
    if show_tools {
        warnings.push("the map has only tool brushes, shown as they are".into());
    }
    let mut mats = Materials {
        scene: &mut scene,
        found: &found,
        made: HashMap::new(),
        missing: Vec::new(),
        sky: None,
        sky_box,
        show_tools,
    };
    let mut groups = vec![
        Group { name: "world".into(), visible: true, surfaces: Vec::new() },
        Group { name: "hidden in Hammer".into(), visible: false, surfaces: Vec::new() },
    ];
    let mut own: HashMap<usize, usize> = HashMap::new();
    let (mut bad, mut faces, mut displacements, mut outside_faces) = (0usize, 0usize, 0usize, 0usize);
    let mut outside_group: Option<usize> = None;
    for (ei, solid, result) in built {
        let e = &vmf.entities[ei];
        let class = e.keys.classname().to_ascii_lowercase();
        if skipped(&class) {
            continue;
        }
        let g = if WORLD_CLASSES.contains(&class.as_str()) {
            if solid.hidden { 1 } else { 0 }
        } else {
            *own.entry(ei).or_insert_with(|| {
                let name = match e.keys.get("targetname") {
                    Some(t) => format!("{class} {t}"),
                    None => format!("{class} #{}", e.keys.get("id").unwrap_or("?")),
                };
                groups.push(Group { name, visible: !e.hidden, surfaces: Vec::new() });
                groups.len() - 1
            })
        };
        let Ok(polys) = result else {
            bad += 1;
            continue;
        };
        // A brush with a displacement shows only its displacements.
        let has_disp = solid.sides.iter().any(|s| s.disp.is_some());
        for (si, poly) in polys.into_iter().enumerate() {
            let (Some(poly), Some(side)) = (poly, solid.sides.get(si)) else { continue };
            if has_disp && side.disp.is_none() {
                continue;
            }
            let name = side.material.replace('\\', "/").to_ascii_lowercase();
            let Some((material, size)) = mats.get(&name, false) else { continue };
            if let Some(d) = &side.disp {
                let blend = if found.get(&name).and_then(|f| f.vmt.as_ref()).is_some_and(SourceMaterial::blends) {
                    mats.get(&name, true).map(|(m, _)| m)
                } else {
                    None
                };
                match displacement::build(d, &poly, side.plane.normal, &side.vecs, size, material, blend) {
                    Some((base, layer)) => {
                        groups[g].surfaces.push(base);
                        groups[g].surfaces.extend(layer);
                        displacements += 1;
                    }
                    None => bad += 1,
                }
                continue;
            }
            let uvs = poly
                .iter()
                .map(|&p| {
                    let st = side.vecs.at(p);
                    Vec2::new((st.x / size.0) as f32, (st.y / size.1) as f32)
                })
                .collect();
            // Faces only the void sees go to their own hidden node.
            let to = match &hull {
                Some(h) if g == 0 && h.faces_outside(&poly, side.plane.normal) => {
                    outside_faces += 1;
                    *outside_group.get_or_insert_with(|| {
                        groups.push(Group { name: "outside the level".into(), visible: false, surfaces: Vec::new() });
                        groups.len() - 1
                    })
                }
                _ => g,
            };
            groups[to].surfaces.push(Surface {
                material,
                normals: vec![side.plane.normal.as_vec3(); poly.len()],
                positions: poly,
                uvs,
                colors: Vec::new(),
                indices: Vec::new(),
            });
            faces += 1;
        }
    }
    let mut missing = std::mem::take(&mut mats.missing);
    drop(mats);
    if let Some(w) = level::missing_warning("materials", &mut missing) {
        warnings.push(w);
    }
    if bad + vmf.bad_sides > 0 {
        warnings.push(format!("{} brushes, sides or displacements could not be built", bad + vmf.bad_sides));
    }
    let triangles = level::add_groups(&mut scene, groups, limits)?;
    if scene.nodes.is_empty() {
        return Err(LoadError::invalid("the map has no drawable brushes"));
    }

    // 5. Point entities as markers, props named after their models, and
    //    the player's start.
    let points: Vec<&VmfEntity> = vmf.entities.iter().skip(1).filter(|e| e.solids.is_empty() && !e.hidden).collect();
    scene.markers = points
        .iter()
        .filter_map(|e| {
            let class = e.keys.classname();
            let label = match (class.starts_with("prop_"), class) {
                (true, _) => e
                    .keys
                    .get("model")
                    .map(|m| crate::base_name(m).trim_end_matches(".mdl").to_string())
                    .unwrap_or_else(|| class.to_string()),
                (_, "func_instance") => format!("instance {}", crate::base_name(e.keys.get("file").unwrap_or(""))),
                _ => class.to_string(),
            };
            crate::bsp::marker(&e.keys, label)
        })
        .collect();
    let keys: Vec<Entity> = points.iter().map(|e| e.keys.clone()).collect();
    let starts = [
        "info_player_start",
        "info_player_teamspawn",
        "info_player_terrorist",
        "info_player_counterterrorist",
        "info_player_deathmatch",
        "info_player_combine",
        "info_player_rebel",
    ];
    scene.spawn = crate::bsp::spawn_at(&keys, &starts, EYE_HEIGHT);

    // 6. Facts for the inspector.
    scene.name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("map").to_string();
    let mut facts = Vec::new();
    if let Some(v) = &vmf.editor_version {
        facts.push(("Hammer version".into(), v.clone()));
    }
    if let Some(s) = skyname {
        facts.push(("Sky".into(), s.to_string()));
    }
    let solids: usize = vmf.entities.iter().map(|e| e.solids.len()).sum();
    facts.push(("Solids".into(), solids.to_string()));
    facts.push(("Faces".into(), faces.to_string()));
    if outside_faces > 0 {
        facts.push(("Faces outside the level".into(), format!("{outside_faces} (hidden)")));
    }
    if displacements > 0 {
        facts.push(("Displacements".into(), displacements.to_string()));
    }
    facts.push(("Triangles".into(), triangles.to_string()));
    facts.push(("Entities".into(), vmf.entities.len().to_string()));
    let with_pixels = found.values().filter(|f| f.base.is_some()).count();
    facts.push(("Materials".into(), format!("{} ({with_pixels} with textures)", names.len())));
    facts.extend(details);
    scene.info = Info { format: "Source map (Hammer VMF)".into(), warnings, details: facts, ..Info::default() };
    Ok(scene)
}

//endregion

#[cfg(test)]
mod tests;
