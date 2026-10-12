//! A Rhino file's layers, materials and objects, reduced to what drawing
//! the meshes in it takes: where each mesh is, which layer it is on, what
//! colour it has and whether it shows.

use super::chunks::{self as ch, Archive, Chunk, Cursor};
use super::mesh::MeshData;
use super::object;
use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub struct Layer {
    pub name: String,
    pub index: i32,
    pub id: [u8; 16],
    pub parent: [u8; 16],
    /// The layer's own switch (false when hidden).
    pub visible: bool,
    pub material: i32,
}

#[derive(Clone, Debug, Default)]
pub struct Material {
    pub index: i32,
    pub diffuse: [u8; 3],
    /// 0 opaque, 1 invisible.
    pub transparency: f32,
}

/// What an object says about how it is drawn.
#[derive(Clone, Debug)]
pub struct Attributes {
    pub id: [u8; 16],
    pub name: String,
    pub layer: i32,
    pub material: i32,
    pub color: [u8; 3],
    /// 0 by layer, 1 by object, 2 by material, 3 by parent.
    pub color_source: u8,
    /// 0 by layer, 1 by object, 3 by parent.
    pub material_source: u8,
    pub visible: bool,
    /// 0 normal, 1 hidden, 2 locked, 3 part of a block definition.
    pub mode: u8,
}

impl Default for Attributes {
    fn default() -> Attributes {
        Attributes {
            id: [0; 16],
            name: String::new(),
            layer: 0,
            material: -1,
            color: [0; 3],
            color_source: 0,
            material_source: 0,
            visible: true,
            mode: 0,
        }
    }
}

/// What an object draws.
pub enum Geometry {
    Meshes(Vec<MeshData>),
    /// A block: the definition's id and the transformation of its objects
    /// into place (rows of a 4x4 matrix, acting on column vectors).
    Instance {
        definition: [u8; 16],
        xform: [f64; 16],
    },
}

/// An object with something to draw.
pub struct Object {
    pub attrs: Attributes,
    pub geometry: Geometry,
}

#[derive(Default)]
pub struct Model {
    /// Metres per model unit, when the file's units are known.
    pub metres_per_unit: Option<f64>,
    pub layers: Vec<Layer>,
    pub materials: HashMap<i32, Material>,
    pub objects: Vec<Object>,
    /// Block definitions: the ids of the objects each is made of.
    pub definitions: HashMap<[u8; 16], Vec<[u8; 16]>>,
    /// Objects of kinds this reader draws nothing for, by name.
    pub skipped: HashMap<&'static str, usize>,
    /// Objects of a drawable kind that carry no mesh (Breps saved without
    /// render meshes).
    pub without_meshes: usize,
    pub object_count: usize,
}

/// The stored version of the openNURBS that wrote the file (a date-like
/// number), which some layouts depend on.
fn opennurbs_version(a: &Archive, props: &Chunk) -> u32 {
    for c in a.chunks(props.body.clone()) {
        if c.tcode == ch::PROPERTIES_OPENNURBS_VERSION {
            return c.value as u32;
        }
    }
    0
}

/// Reads the tables.  `max_triangles` stops reading meshes once that many
/// triangles are held.
pub fn read(a: &Archive, max_triangles: u64) -> Model {
    let mut model = Model::default();
    let mut tables: HashMap<u32, Chunk> = HashMap::new();
    for c in a.chunks(32..a.data.len()) {
        if c.tcode == ch::END_OF_FILE {
            break;
        }
        tables.entry(c.tcode).or_insert(c);
    }
    let on_version = tables.get(&ch::TABLE_PROPERTIES).map_or(0, |p| opennurbs_version(a, p));
    if let Some(t) = tables.get(&ch::TABLE_SETTINGS) {
        model.metres_per_unit = units(a, t);
    }
    if let Some(t) = tables.get(&ch::TABLE_LAYER) {
        for rec in a.chunks(t.body.clone()).filter(|c| c.tcode == ch::LAYER_RECORD) {
            if let Some(layer) = layer_record(a, &rec) {
                model.layers.push(layer);
            }
        }
    }
    if let Some(t) = tables.get(&ch::TABLE_MATERIAL) {
        for (position, rec) in a.chunks(t.body.clone()).filter(|c| c.tcode == ch::MATERIAL_RECORD).enumerate() {
            if let Some(mut m) = material_record(a, &rec, on_version) {
                if m.index < 0 {
                    m.index = position as i32;
                }
                model.materials.insert(m.index, m);
            }
        }
    }
    if let Some(t) = tables.get(&ch::TABLE_INSTANCE_DEFINITION) {
        for rec in a.chunks(t.body.clone()).filter(|c| c.tcode == ch::INSTANCE_DEFINITION_RECORD) {
            if let Some((id, members)) = definition_record(a, &rec) {
                model.definitions.insert(id, members);
            }
        }
    }
    if let Some(t) = tables.get(&ch::TABLE_OBJECT) {
        let mut triangles = 0u64;
        for rec in a.chunks(t.body.clone()).filter(|c| c.tcode == ch::OBJECT_RECORD) {
            model.object_count += 1;
            if triangles > max_triangles {
                continue;
            }
            object_record(a, &rec, on_version, &mut model, &mut triangles);
        }
    }
    model
}

/// The unit system of the model, as metres per unit.  The record is a
/// version number (100 to 199), the unit system, three tolerances, and in
/// later versions the display mode and precision, and the metres per unit
/// of a custom system.
fn units(a: &Archive, table: &Chunk) -> Option<f64> {
    let rec = a.chunks(table.body.clone()).find(|c| c.tcode == ch::SETTINGS_UNITS)?;
    let mut cur = Cursor::new(a.body(&rec));
    let version = cur.i32()?;
    if !(100..200).contains(&version) {
        return None;
    }
    let system = cur.i32()?;
    let known = match system {
        1 => 1e-6,
        2 => 1e-3,
        3 => 1e-2,
        4 => 1.0,
        5 => 1e3,
        6 => 2.54e-8,
        7 => 2.54e-5,
        8 => 0.0254,
        9 => 0.3048,
        10 => 1609.344,
        12 => 1e-10,
        13 => 1e-9,
        14 => 0.1,
        15 => 10.0,
        16 => 100.0,
        17 => 1e6,
        18 => 1e9,
        19 => 0.9144,
        20 => 0.0254 / 72.0,
        21 => 0.0254 / 6.0,
        22 => 1852.0,
        23 => 1.495_978_707e11,
        24 => 9.460_730_472_580_8e15,
        25 => 3.085_677_581_491_367e16,
        _ => 0.0,
    };
    if system == 11 && version >= 102 {
        cur.skip(24 + 8)?;
        let per_unit = cur.f64()?;
        return (per_unit.is_finite() && per_unit > 0.0).then_some(per_unit);
    }
    (known > 0.0).then_some(known)
}

fn color_bytes(c: &mut Cursor) -> Option<[u8; 3]> {
    let b = c.bytes(4)?;
    Some([b[0], b[1], b[2]])
}

/// The class data of a table record that holds a class chunk.
fn record_class<'a>(a: &Archive<'a>, rec: &Chunk) -> Option<&'a [u8]> {
    let class_chunk = a.chunks(rec.body.clone()).find(|c| c.tcode == ch::OPENNURBS_CLASS)?;
    object::class(a, &class_chunk).map(|c| c.data)
}

fn layer_record(a: &Archive, rec: &Chunk) -> Option<Layer> {
    let data = record_class(a, rec)?;
    let mut cur = Cursor::new(data);
    let (major, minor) = cur.version()?;
    if major != 1 {
        return None;
    }
    let mode = cur.i32()?;
    let index = cur.i32()?;
    cur.skip(4)?; // IGES level
    let material = cur.i32()?;
    cur.skip(4)?;
    cur.skip(4)?; // colour
    // An obsolete line style: two shorts and two doubles.
    cur.skip(20)?;
    let name = cur.string()?;
    let mut layer = Layer { name, index, visible: mode != 1, material, ..Default::default() };
    if minor >= 1 {
        layer.visible &= cur.u8()? != 0;
    }
    if minor >= 2 {
        cur.skip(4)?; // line pattern
    }
    if minor >= 3 {
        cur.skip(4 + 8)?; // plot colour and weight
    }
    if minor >= 4 {
        cur.skip(1)?; // locked
    }
    if minor >= 5 {
        layer.id = cur.uuid()?;
    }
    if minor >= 6 {
        layer.parent = cur.uuid().unwrap_or_default();
    }
    Some(layer)
}

/// A material's diffuse colour and transparency, from the three layouts a
/// file can have (Rhino 3, Rhino 4 and 5, Rhino 6 and later).
fn material_record(a: &Archive, rec: &Chunk, on_version: u32) -> Option<Material> {
    let data = record_class(a, rec)?;
    let mut cur = Cursor::new(data);
    let mut m = Material { index: -1, ..Default::default() };
    if a.version >= 60 && on_version >= 2_348_833_910 {
        // An anonymous chunk with a version, the component attributes (a
        // chunk of their own), then the colours.
        let sub = Archive { data, version: a.version };
        let chunk = sub.chunk_at(0, data.len())?;
        let mut c = Cursor::new(sub.body(&chunk));
        let (major, _) = c.chunk_version()?;
        if major != 1 {
            return None;
        }
        // The attributes chunk: which are written, then each.
        let attrs = Archive { data: c.data, version: a.version }.chunk_at(c.pos, c.data.len())?;
        {
            let mut ac = Cursor::new(&c.data[attrs.body.clone()]);
            ac.chunk_version()?;
            let bits = ac.u32()?;
            if bits & 1 != 0 {
                ac.skip(16)?;
            }
            if bits & 2 != 0 {
                ac.skip(16)?;
            }
            if bits & 4 != 0 {
                m.index = ac.i32()?;
            }
        }
        c.pos = attrs.next;
        c.skip(16)?; // plug-in id
        c.skip(4)?; // ambient
        m.diffuse = color_bytes(&mut c)?;
        c.skip(4 * 4)?; // emission, specular, reflection, transparent
        c.skip(3 * 8)?; // index of refraction, reflectivity, shine
        m.transparency = c.f64()? as f32;
        return Some(m);
    }
    let (major, minor) = cur.version()?;
    match major {
        1 => {
            // Rhino 3: ambient, diffuse, emission, specular, shine,
            // transparency.
            let _ = minor;
            cur.skip(4)?;
            m.diffuse = color_bytes(&mut cur)?;
            cur.skip(4 + 4 + 8)?;
            m.transparency = cur.f64()? as f32;
        }
        2 => {
            // Rhino 4 and 5: an anonymous chunk with an id, an index, a
            // name, a plug-in id and six colours.
            let sub = Archive { data, version: a.version };
            let chunk = sub.chunk_at(cur.pos, data.len())?;
            let mut c = Cursor::new(sub.body(&chunk));
            c.chunk_version()?;
            c.skip(16)?;
            m.index = c.i32()?;
            c.string()?;
            c.skip(16)?;
            c.skip(4)?;
            m.diffuse = color_bytes(&mut c)?;
            c.skip(4 * 4)?;
            c.skip(3 * 8)?;
            m.transparency = c.f64()? as f32;
        }
        _ => return None,
    }
    Some(m)
}

fn attributes(a: &Archive, body: &[u8], on_version: u32) -> Option<Attributes> {
    let mut cur = Cursor::new(body);
    let mut at = Attributes::default();
    if a.version >= 5 && on_version >= 200_712_190 {
        let (major, minor) = cur.version()?;
        if major != 2 {
            return None;
        }
        at.id = cur.uuid()?;
        at.layer = cur.i32()?;
        loop {
            let item = cur.u8()?;
            match item {
                0 => break,
                1 => at.name = cur.string()?,
                2 => {
                    cur.string()?;
                }
                3 | 4 => {
                    let v = cur.i32()?;
                    if item == 4 {
                        at.material = v;
                    }
                }
                5 => {
                    // Rendering attributes: a chunk.
                    let sub = Archive { data: cur.data, version: a.version };
                    cur.pos = sub.chunk_at(cur.pos, cur.data.len())?.next;
                }
                6 => at.color = color_bytes(&mut cur)?,
                7 => cur.skip(4)?,
                8 => cur.skip(8)?,
                9 => cur.skip(1)?,
                10 => cur.skip(4)?,
                11 => at.visible = cur.u8()? != 0,
                12 => at.mode = cur.u8()?,
                13 => at.color_source = cur.u8()?,
                14 | 15 | 17 | 19 => cur.skip(1)?,
                16 => at.material_source = cur.u8()?,
                18 => {
                    let n = cur.i32()?;
                    cur.skip(usize::try_from(n).ok()?.checked_mul(4)?)?;
                }
                20 => cur.skip(16)?,
                21 => {
                    // Display material references: a count, then pairs of ids.
                    let n = cur.i32()?;
                    cur.skip(usize::try_from(n).ok()?.checked_mul(32)?)?;
                }
                22 if minor >= 1 => cur.skip(4)?,
                // Newer items are optional and come after everything this
                // needs; stop at the first one not known.
                _ => break,
            }
        }
        return Some(at);
    }
    // Older layout: version 1.x.
    let (major, minor) = cur.version()?;
    if major != 1 {
        return None;
    }
    at.id = cur.uuid()?;
    at.layer = cur.i32()?;
    at.material = cur.i32()?;
    at.color = color_bytes(&mut cur)?;
    // The obsolete line style: two shorts, two doubles.
    cur.skip(20)?;
    cur.skip(4)?; // wire density
    at.mode = cur.u8()?;
    at.color_source = cur.u8()?;
    cur.skip(1)?; // line type source
    at.material_source = cur.u8()?;
    at.name = cur.string()?;
    cur.string()?;
    at.visible = at.mode != 1;
    if minor >= 1 {
        let n = cur.i32()?;
        cur.skip(usize::try_from(n).ok()?.checked_mul(4)?)?;
        if minor >= 2 {
            at.visible = cur.u8()? != 0;
        }
    }
    Some(at)
}

fn object_record(a: &Archive, rec: &Chunk, on_version: u32, model: &mut Model, triangles: &mut u64) {
    let mut class_chunk = None;
    let mut attrs = Attributes::default();
    for c in a.chunks(rec.body.clone()) {
        match c.tcode {
            ch::OPENNURBS_CLASS => class_chunk = Some(c),
            ch::OBJECT_RECORD_ATTRIBUTES => {
                if let Some(at) = attributes(a, a.body(&c), on_version) {
                    attrs = at;
                }
            }
            _ => {}
        }
    }
    let Some(chunk) = class_chunk else { return };
    let Some(cls) = object::class(a, &chunk) else { return };
    let meshes: Vec<MeshData> = if cls.id == object::mesh_class() {
        super::mesh::read(a, cls.data).into_iter().collect()
    } else if object::is_brep_class(&cls.id) {
        object::brep_meshes(a, cls.data)
    } else if cls.id == object::extrusion_class() {
        object::extrusion_mesh(a, &cls).into_iter().collect()
    } else if cls.id == object::subd_class() {
        *model.skipped.entry("SubD surfaces").or_insert(0) += 1;
        return;
    } else if cls.id == object::instance_ref_class() {
        if let Some((definition, xform)) = instance_ref(cls.data) {
            model.objects.push(Object { attrs, geometry: Geometry::Instance { definition, xform } });
        }
        return;
    } else {
        // Curves, points, annotations, hatches and the like.
        return;
    };
    if meshes.is_empty() {
        model.without_meshes += 1;
        return;
    }
    *triangles += meshes.iter().map(|m| m.indices.len() as u64 / 3).sum::<u64>();
    model.objects.push(Object { attrs, geometry: Geometry::Meshes(meshes) });
}

/// The block and placement of an instance reference.
fn instance_ref(data: &[u8]) -> Option<([u8; 16], [f64; 16])> {
    let mut cur = Cursor::new(data);
    let (major, _) = cur.version()?;
    if major != 1 {
        return None;
    }
    let id = cur.uuid()?;
    let mut x = [0.0; 16];
    for v in &mut x {
        *v = cur.f64()?;
    }
    x.iter().all(|v| v.is_finite()).then_some((id, x))
}

/// A block definition's id and the ids of its objects.  Rhino 5 files have
/// a version byte first; later ones an anonymous chunk with component
/// attributes (of which the id is first) and a unit system chunk.
fn definition_record(a: &Archive, rec: &Chunk) -> Option<([u8; 16], Vec<[u8; 16]>)> {
    let data = record_class(a, rec)?;
    let sub = Archive { data, version: a.version };
    let uuids = |cur: &mut Cursor| -> Option<Vec<[u8; 16]>> {
        let n = usize::try_from(cur.i32()?).ok()?;
        if n.checked_mul(16)? > cur.remaining() {
            return None;
        }
        (0..n).map(|_| cur.uuid()).collect()
    };
    match sub.chunk_at(0, data.len()) {
        Some(chunk) if a.version > 50 && chunk.tcode == ch::ANONYMOUS_CHUNK => {
            let body = sub.body(&chunk);
            let inner = Archive { data: body, version: a.version };
            let mut cur = Cursor::new(body);
            let (major, _) = cur.chunk_version()?;
            if major != 1 {
                return None;
            }
            let attrs = inner.chunk_at(cur.pos, body.len())?;
            let mut ac = Cursor::new(&body[attrs.body.clone()]);
            ac.chunk_version()?;
            let bits = ac.u32()?;
            let id = if bits & 1 != 0 { ac.uuid()? } else { [0; 16] };
            cur.pos = attrs.next;
            cur.skip(4)?; // type
            cur.pos = inner.chunk_at(cur.pos, body.len())?.next; // unit system
            for _ in 0..3 {
                cur.string()?;
            }
            cur.skip(48)?;
            let members = if cur.u8()? != 0 { uuids(&mut cur)? } else { Vec::new() };
            Some((id, members))
        }
        _ => {
            let mut cur = Cursor::new(data);
            let (major, _) = cur.version()?;
            if major != 1 {
                return None;
            }
            let id = cur.uuid()?;
            Some((id, uuids(&mut cur)?))
        }
    }
}
