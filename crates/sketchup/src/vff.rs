//! SketchUp 2021 and later: the UTF-16 header, then a zip holding
//! `model.dat` (the entities, as tag-length-value records), one
//! `material.xml` per material with its texture beside it, and the styles.
//!
//! The record layouts are OpenSKP's (`_core.py`, `docs/BINARY_FORMAT.md`):
//! a vertex is `C409` with its position in `C509`; an edge `B80B` names its
//! vertices in `B90B` and `BA0B`; a face `AC0D` has its normal in `AD0D`
//! and loops of coedges (`A00F`: an edge and a direction) under `AE0D`;
//! definitions are `7C15`, instances `6419`, and entity properties
//! (material, tag, hidden and soft flags, texture positions) sit in a
//! `D007` record on each entity.  References between entities are entity
//! ids, mapped to indices once every record has been read.

use crate::Caps;
use crate::model::*;
use crate::tlv::{self, Node, Records};
use glam::DVec3;
use slerp_archive::{Archive, Entry};
use slerp_scene::LoadError;
use std::collections::HashMap;

/*┌─────────────────╮
| Container       |
└─────────────────╯*/
//region Container

/// Where the zip starts: right after the header, within the first
/// kilobytes.
pub fn zip_start(data: &[u8]) -> Option<usize> {
    let head = &data[..data.len().min(4096)];
    head.windows(4).position(|w| w == b"PK\x03\x04")
}

pub fn read(data: &[u8], caps: &Caps) -> Result<Model, LoadError> {
    let start = zip_start(data).ok_or_else(|| LoadError::invalid("SketchUp file without its zip"))?;
    let limits = slerp_archive::Limits {
        max_entries: 100_000,
        max_read: caps.max_entry_bytes,
        ..slerp_archive::Limits::default()
    };
    let zip = Archive::from_bytes_with(data[start..].to_vec(), limits)
        .map_err(|e| LoadError::invalid(format!("SketchUp zip: {e}")))?;
    let mut model = Model { container: Some(Container::Vff), ..Model::default() };

    // 1. Materials, each a folder with `material.xml` and its picture.
    let mut by_name: HashMap<String, u32> = HashMap::new();
    for e in zip.entries() {
        let lower = e.path.to_ascii_lowercase();
        if !(lower.starts_with("materials/") && lower.ends_with("/material.xml")) {
            continue;
        }
        let Some(m) = zip.read(e).ok().and_then(|b| crate::xml::material(&b)) else {
            model.warnings.push(format!("unreadable material description {}", e.path));
            continue;
        };
        let folder = e.folder().to_string();
        let texture = m.texture.as_ref().and_then(|t| texture(&zip, &folder, &e.path, t, &mut model.warnings));
        let index = model.materials.len() as u32;
        by_name.entry(m.name.clone()).or_insert(index);
        if let Some(f) = folder.rsplit('/').next() {
            by_name.entry(f.to_string()).or_insert(index);
        }
        model.materials.push(Material {
            name: m.name,
            color: m.color,
            opacity: m.opacity,
            colorized: m.colorized && texture.is_some(),
            texture,
        });
    }

    // 2. The style's face colours (the first style that sets them).
    for e in zip.entries() {
        let lower = e.path.to_ascii_lowercase();
        if lower.starts_with("styles/") && lower.ends_with("style.xml") {
            let (front, back) = zip.read(e).map(|b| crate::xml::style_colors(&b)).unwrap_or((None, None));
            if front.is_some() || back.is_some() {
                model.style.front = front.unwrap_or(model.style.front);
                model.style.back = back.unwrap_or(model.style.back);
                break;
            }
        }
    }

    // 3. The entities.
    let dat = zip
        .entries()
        .iter()
        .find(|e| e.path.eq_ignore_ascii_case("model.dat") || e.path.to_ascii_lowercase().ends_with("/model.dat"))
        .ok_or_else(|| LoadError::invalid("SketchUp zip without model.dat"))?;
    let dat = zip.read(dat).map_err(|e| LoadError::invalid(format!("SketchUp model.dat: {e}")))?;
    entities(&dat, &mut model, &by_name);
    Ok(model)
}

/// Reads `model.dat` into `model`, whose materials are already known by
/// name.
pub(crate) fn entities(dat: &[u8], model: &mut Model, materials_by_name: &HashMap<String, u32>) {
    let mut walk = Walk::default();
    let mut top: Vec<Node> = Records::new(dat).collect();
    if top.len() == 1 && top[0].tag == 0xF401 {
        top = Records::new(top[0].body).collect();
    }
    for n in &top {
        match n.tag {
            0xF601 => extract(n.kids(), &mut walk.root, 0),
            0xFA01 => model.camera = tlv::item(n.body, 0xBC34).and_then(camera),
            _ => {}
        }
        walk.visit(*n, 0);
    }
    if walk.definitions.is_empty() && walk.root.is_empty() && dat.len() > 64 {
        model.warnings.push("model.dat holds no entities this reader recognises".into());
    }
    walk.finish(model, materials_by_name);
}

/// `model.dat` of a 2021 or later file, for tests that damage it.
#[cfg(test)]
pub(crate) fn model_dat(data: &[u8]) -> Option<Vec<u8>> {
    let zip = Archive::from_bytes(data[zip_start(data)?..].to_vec()).ok()?;
    let e = zip.entries().iter().find(|e| e.path == "model.dat")?;
    zip.read(e).ok()
}

/// A material's picture: the file `material.xml` names in its folder, else
/// the folder's other file, else (for a colourised copy) the picture in the
/// original material's folder.
fn texture(
    zip: &Archive,
    folder: &str,
    xml_path: &str,
    t: &crate::xml::TextureXml,
    warnings: &mut Vec<String>,
) -> Option<TextureImage> {
    let named = (!t.file_name.is_empty()).then(|| format!("{folder}/{}", t.file_name));
    let in_folder =
        |e: &&Entry| e.folder() == folder && e.path != xml_path && !e.path.to_ascii_lowercase().ends_with(".xml");
    let image = t.image_path.trim_start_matches(['.', '/']);
    let entry = named
        .as_deref()
        .and_then(|p| zip.find(p).first().and_then(|&i| zip.entry(i)))
        .or_else(|| zip.entries().iter().find(in_folder))
        .or_else(|| {
            [image.to_string(), format!("{folder}/{image}")]
                .iter()
                .filter(|p| !image.is_empty() && !p.is_empty())
                .find_map(|p| zip.find(p).first().and_then(|&i| zip.entry(i)))
        });
    let Some(entry) = entry else {
        warnings.push(format!("texture of {folder} is missing"));
        return None;
    };
    match zip.read(entry) {
        Ok(bytes) => Some(TextureImage {
            bytes,
            width: t.x_scale,
            height: t.y_scale,
            file_name: if t.file_name.is_empty() { entry.name().to_string() } else { t.file_name.clone() },
        }),
        Err(e) => {
            warnings.push(format!("texture {}: {e}", entry.path));
            None
        }
    }
}

/// A camera record (`34BC`): eye, target and up in inches, the field of
/// view, and for parallel projection the height shown.
fn camera(body: &[u8]) -> Option<Camera> {
    let v = |tag: u16| tlv::item(body, tag).and_then(tlv::doubles::<3>).map(DVec3::from_array);
    let f = |tag: u16| tlv::item(body, tag).and_then(tlv::doubles::<1>).map(|d| d[0]);
    let cam = Camera {
        eye: v(0xBD34)?,
        target: v(0xBE34)?,
        up: v(0xBF34).unwrap_or(DVec3::Z),
        fov: f(0xC434).unwrap_or(35.0),
        perspective: tlv::item(body, 0xC234).is_none_or(|b| b.first() != Some(&0)),
        height: f(0xC334).unwrap_or(0.0),
    };
    (cam.eye.is_finite() && cam.target.is_finite()).then_some(cam)
}

//endregion

/*┌─────────────────╮
| Records         |
└─────────────────╯*/
//region Records

/// A definition's entities with the file's ids, before they are resolved.
#[derive(Default)]
struct Raw {
    name: String,
    image: bool,
    faces_camera: bool,
    vertices: HashMap<u64, DVec3>,
    vertex_order: Vec<u64>,
    edges: Vec<RawEdge>,
    faces: Vec<RawFace>,
    instances: Vec<RawInstance>,
}

impl Raw {
    fn is_empty(&self) -> bool {
        self.vertices.is_empty() && self.faces.is_empty() && self.instances.is_empty()
    }
}

struct RawEdge {
    id: u64,
    a: u64,
    b: u64,
    flags: u8,
    layer: Option<u64>,
}

struct RawFace {
    loops: Vec<Vec<(u64, bool)>>,
    normal: DVec3,
    front: Option<u64>,
    back: Option<u64>,
    mapping: [Option<[f64; 9]>; 2],
    hidden: bool,
    layer: Option<u64>,
}

struct RawInstance {
    definition: Option<u64>,
    matrix: Option<[f64; 13]>,
    material: Option<u64>,
    layer: Option<u64>,
    hidden: bool,
    name: String,
}

/// What the walk over the whole `model.dat` collects.
#[derive(Default)]
struct Walk {
    root: Raw,
    definitions: Vec<(Option<u64>, Raw)>,
    /// Tag ids, names and hidden flags, in file order.
    layers: Vec<(u64, String, bool)>,
    /// Material entity ids and names.
    material_names: HashMap<u64, String>,
    pages: Option<Vec<RawPage>>,
}

struct RawPage {
    name: String,
    camera: Option<Camera>,
    hidden: Vec<u64>,
}

impl Walk {
    /// Looks at every record for tags, material names, definitions and
    /// scenes, wherever they sit.
    fn visit(&mut self, n: Node, depth: usize) {
        if depth >= tlv::MAX_DEPTH {
            return;
        }
        match n.tag {
            0x993A => {
                for l in n.kids().filter(|k| k.tag == 0x8C3C) {
                    let (Some(id), Some(name)) = (l.find(0xDC05).map(|d| dc05_id(d.body)), l.find(0x8D3C)) else {
                        continue;
                    };
                    let hidden = l.find(0x8E3C).is_some_and(|h| h.body.first() == Some(&1));
                    self.layers.push((id, text(name.body), hidden));
                }
            }
            0xC832 => {
                if let (Some(id), Some(name)) = (n.find(0xDC05).map(|d| dc05_id(d.body)), n.find(0xCC32)) {
                    self.material_names.insert(id, text(name.body));
                }
            }
            0x7C15 => {
                let mut raw = Raw::default();
                for c in n.kids() {
                    match c.tag {
                        0x7E15 => raw.name = text(c.body),
                        // Behaviour: `5D1B` = 1 is "always face camera".
                        0x581B => raw.faces_camera = tlv::item(c.body, 0x5D1B).is_some_and(|b| tlv::var_int(b) == 1),
                        // The kind: 2 for the quad behind an Image entity.
                        0x8315 => raw.image = !c.body.is_empty() && tlv::var_int(c.body) == 2,
                        _ => {}
                    }
                }
                extract(n.kids(), &mut raw, 0);
                self.definitions.push((n.entity_id(), raw));
            }
            0x0702 if self.pages.is_none() => self.pages = Some(pages(n.body)),
            _ => {}
        }
        for k in n.kids() {
            self.visit(k, depth + 1);
        }
    }

    /// Turns ids into indices and moves everything into `model`.
    fn finish(self, model: &mut Model, materials_by_name: &HashMap<String, u32>) {
        // 1. Tags.  The default tag has id 1 when the file doesn't list it.
        let mut layer_index: HashMap<u64, u32> = HashMap::new();
        for (id, name, hidden) in self.layers {
            layer_index.entry(id).or_insert(model.layers.len() as u32);
            model.layers.push(Layer { name, hidden });
        }
        if let std::collections::hash_map::Entry::Vacant(v) = layer_index.entry(1) {
            v.insert(model.layers.len() as u32);
            model.layers.push(Layer { name: "Layer0".into(), hidden: false });
        }
        // 2. Materials: an entity id names a material, which material.xml
        //    describes under that name (or in a folder of that name).
        let material = |id: Option<u64>| -> Option<u32> {
            id.and_then(|id| self.material_names.get(&id)).and_then(|name| materials_by_name.get(name).copied())
        };
        let layer = |id: Option<u64>| id.and_then(|id| layer_index.get(&id).copied());
        // 3. Definitions by id.
        let mut def_index: HashMap<u64, u32> = HashMap::new();
        for (i, (id, _)) in self.definitions.iter().enumerate() {
            if let Some(id) = id {
                def_index.entry(*id).or_insert(i as u32);
            }
        }
        let mut dropped = 0usize;
        let mut resolve = |raw: Raw| -> Definition {
            let mut d =
                Definition { name: raw.name, image: raw.image, faces_camera: raw.faces_camera, ..Default::default() };
            let mut vertex_index: HashMap<u64, u32> = HashMap::with_capacity(raw.vertices.len());
            for id in raw.vertex_order {
                if let Some(&p) = raw.vertices.get(&id)
                    && !vertex_index.contains_key(&id)
                {
                    vertex_index.insert(id, d.vertices.len() as u32);
                    d.vertices.push(p);
                }
            }
            let mut edge_index: HashMap<u64, u32> = HashMap::with_capacity(raw.edges.len());
            for e in raw.edges {
                let (Some(&a), Some(&b)) = (vertex_index.get(&e.a), vertex_index.get(&e.b)) else {
                    dropped += 1;
                    continue;
                };
                edge_index.insert(e.id, d.edges.len() as u32);
                d.edges.push(Edge {
                    a,
                    b,
                    hidden: e.flags & 0x01 != 0,
                    soft: e.flags & 0x08 != 0,
                    smooth: e.flags & 0x10 != 0,
                    layer: layer(e.layer),
                });
            }
            for f in raw.faces {
                let loops = f
                    .loops
                    .into_iter()
                    .map(|l| {
                        l.into_iter()
                            .filter_map(|(e, reversed)| edge_index.get(&e).map(|&edge| CoEdge { edge, reversed }))
                            .collect::<Vec<_>>()
                    })
                    .filter(|l| !l.is_empty())
                    .collect();
                let mapping = f.mapping.map(|m| m.map(|matrix| Mapping { matrix, projection: None }));
                d.faces.push(Face {
                    loops,
                    normal: f.normal,
                    front: material(f.front),
                    back: material(f.back),
                    mapping,
                    hidden: f.hidden,
                    layer: layer(f.layer),
                });
            }
            for i in raw.instances {
                let Some(&definition) = i.definition.and_then(|id| def_index.get(&id)) else {
                    dropped += 1;
                    continue;
                };
                d.instances.push(Instance {
                    definition,
                    matrix: i.matrix.unwrap_or(IDENTITY),
                    material: material(i.material),
                    layer: layer(i.layer),
                    hidden: i.hidden,
                    name: i.name,
                });
            }
            d
        };
        model.root = resolve(self.root);
        model.definitions = self.definitions.into_iter().map(|(_, raw)| resolve(raw)).collect();
        if dropped > 0 {
            model.warnings.push(format!("{dropped} edges or placements refer to entities that aren't in the file"));
        }
        // 4. Scenes.
        for p in self.pages.unwrap_or_default() {
            let hidden_layers = p.hidden.iter().filter_map(|id| layer_index.get(id).copied()).collect();
            model.pages.push(Page { name: p.name, camera: p.camera, hidden_layers });
        }
    }
}

pub const IDENTITY: [f64; 13] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

/// The id in a `DC05` record: a wrapped `DE05`, or the id itself.
fn dc05_id(body: &[u8]) -> u64 {
    if body.starts_with(&[0xDE, 0x05]) { tlv::wrapped_id(body) } else { tlv::var_int(body) }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim_end_matches('\0').to_string()
}

/// Collects the entities among `nodes` into `raw`: vertices, edges, faces
/// and instances, looking inside other containers but not inside the
/// entities themselves.
fn extract(nodes: Records, raw: &mut Raw, depth: usize) {
    if depth >= tlv::MAX_DEPTH {
        return;
    }
    for n in nodes {
        match n.tag {
            0xC409 => {
                if let (Some(id), Some(p)) = (n.entity_id(), n.find(0xC509).and_then(|c| tlv::doubles::<3>(c.body)))
                    && raw.vertices.insert(id, DVec3::from_array(p)).is_none()
                {
                    raw.vertex_order.push(id);
                }
            }
            0xB80B => {
                let Some(id) = n.entity_id() else { continue };
                let (Some(a), Some(b)) = (n.find(0xB90B), n.find(0xBA0B)) else { continue };
                let props = n.child(0xD007);
                let flags = props.and_then(|p| p.child(0xD307)).and_then(|f| f.body.first().copied()).unwrap_or(0);
                raw.edges.push(RawEdge {
                    id,
                    a: tlv::var_int(a.body),
                    b: tlv::var_int(b.body),
                    flags,
                    layer: props
                        .and_then(|p| p.child(0xD207))
                        .filter(|l| !l.body.is_empty())
                        .map(|l| tlv::var_int(l.body)),
                });
            }
            0xAC0D => {
                if n.entity_id().is_some() {
                    raw.faces.push(face(n));
                }
            }
            0x6419 => raw.instances.push(instance(n)),
            // Guide lines and points.
            0x6942 | 0x6C42 => {}
            _ => extract(n.kids(), raw, depth + 1),
        }
    }
}

fn face(n: Node) -> RawFace {
    let normal = n.find(0xAD0D).and_then(|c| tlv::doubles::<3>(c.body)).map_or(DVec3::Z, DVec3::from_array);
    let mut loops = Vec::new();
    if let Some(list) = n.find(0xAE0D) {
        let mut found = Vec::new();
        list.find_all(0x9411, &mut found);
        for l in found {
            let mut coedges = Vec::new();
            l.find_all(0xA00F, &mut coedges);
            let uses: Vec<(u64, bool)> = coedges
                .iter()
                .filter_map(|c| {
                    let edge = tlv::item(c.body, 0xA10F).map(tlv::var_int)?;
                    // SketchUp's bit: 0 runs along the edge, 1 against it.
                    let reversed = tlv::item(c.body, 0xA20F).map(tlv::var_int)? != 0;
                    Some((edge, reversed))
                })
                .collect();
            if !uses.is_empty() {
                loops.push(uses);
            }
        }
    }
    let props = n.child(0xD007);
    let get = |tag: u16| props.and_then(|p| p.child(tag));
    RawFace {
        loops,
        normal,
        front: get(0xD107).map(|m| tlv::var_int(m.body)),
        // Painted on its back only: `AF0D` on the face itself.
        back: n.child(0xAF0D).filter(|b| !b.body.is_empty()).map(|b| tlv::var_int(b.body)),
        mapping: get(0xDC05).map_or([None, None], |d| mappings(d.body)),
        hidden: get(0xD307).and_then(|f| f.body.first()).is_some_and(|f| f & 1 != 0),
        layer: get(0xD207).filter(|l| !l.body.is_empty()).map(|l| tlv::var_int(l.body)),
    }
}

/// A face's texture positions, front and back: `DC05 > DD05 > B136 > B236 >
/// 1027 > 1127` (front) or `1227` (back) `> 1327 > 1527`, nine doubles.
fn mappings(dc05: &[u8]) -> [Option<[f64; 9]>; 2] {
    let side = |tag: u16| -> Option<[f64; 9]> {
        let mut b = dc05;
        for t in [0xDD05, 0xB136, 0xB236, 0x1027, tag, 0x1327, 0x1527] {
            b = tlv::item(b, t)?;
        }
        if b.len() != 72 {
            return None;
        }
        tlv::doubles::<9>(b).filter(|m| m.iter().all(|v| v.is_finite()))
    };
    [side(0x1127), side(0x1227)]
}

fn instance(n: Node) -> RawInstance {
    let props = n.child(0xD007);
    let get = |tag: u16| props.and_then(|p| p.child(tag));
    RawInstance {
        definition: n.find(0x6719).map(|d| tlv::var_int(d.body)),
        matrix: n.find(0x6619).and_then(|m| tlv::doubles::<13>(m.body)),
        material: get(0xD107).map(|m| tlv::var_int(m.body)),
        layer: get(0xD207).filter(|l| !l.body.is_empty()).map(|l| tlv::var_int(l.body)),
        hidden: get(0xD307).and_then(|f| f.body.first()).is_some_and(|f| f & 1 != 0),
        name: n.find(0x6519).map(|t| text(t.body)).unwrap_or_default(),
    }
}

/// Scenes: `0702 > 606D > 616D`, one `4871` per scene with its name
/// (`546F > 556F`), camera (`4A71 > BC34`) and hidden tags (`5071`: runs of
/// a length byte and an id).
fn pages(body: &[u8]) -> Vec<RawPage> {
    fn all(b: &[u8]) -> Vec<Node<'_>> {
        tlv::exact_items(b).unwrap_or_default()
    }
    fn find<'a>(items: &[Node<'a>], tag: u16) -> Option<&'a [u8]> {
        items.iter().find(|n| n.tag == tag).map(|n| n.body)
    }
    let mut out = Vec::new();
    for a in all(body).into_iter().filter(|n| n.tag == 0x606D) {
        for b in all(a.body).into_iter().filter(|n| n.tag == 0x616D) {
            for page in all(b.body).into_iter().filter(|n| n.tag == 0x4871) {
                let Some(items) = tlv::exact_items(page.body) else { continue };
                let name = find(&items, 0x546F).and_then(|h| find(&all(h), 0x556F)).map(text).unwrap_or_default();
                let cam = find(&items, 0x4A71).and_then(|c| find(&all(c), 0xBC34)).and_then(camera);
                let mut hidden = Vec::new();
                if let Some(h) = find(&items, 0x5071) {
                    let mut at = 0;
                    while let Some(&len) = h.get(at) {
                        let len = len as usize;
                        let Some(id) = h.get(at + 1..at + 1 + len).filter(|_| len > 0) else { break };
                        hidden.push(tlv::var_int(id));
                        at += 1 + len;
                    }
                }
                out.push(RawPage { name, camera: cam, hidden });
            }
        }
    }
    out
}

//endregion
