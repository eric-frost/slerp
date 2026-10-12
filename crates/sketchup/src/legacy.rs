//! SketchUp 3 to 2020: after the UTF-16 header, one uncompressed MFC
//! `CArchive` object stream with a single store map.  Every object is
//! written in full the first time it is referenced and by its store-map
//! index ("slot") afterwards, so the stream has to be walked object by
//! object from a known starting point, and every class must be read to
//! stay aligned.
//!
//! The decoding is OpenSKP's (`legacy.py`), which found it by reverse
//! engineering files saved by SketchUp 3 to 2020 and checked it against
//! the same models saved again as 2021 files and against the SDK.  Its
//! notes on the details that differ from the public 2017 format notes are
//! kept here with the code they explain.  Briefly:
//!
//! - The walk starts at the material list, found by its first `CMaterial`
//!   class record (or, in files without materials, by the first `CLayer`).
//!   What comes before (the document header, the style, the model's own
//!   thumbnail) is skipped, so references into it can't be resolved.
//! - Slot numbers are absolute, so the walk must start numbering at the
//!   right one: with two or more materials, the second material's class
//!   reference names the slot of `CMaterial`; otherwise the definition
//!   list's reference to the active tag gives it, tried for each tag.
//! - Then come the tags, the component definitions (each with its own
//!   entities) and the model's own entity list.
//! - SketchUp 2020 adds an undocumented filler (an empty string and zero
//!   padding) before some counts, found by its shape after a count reads
//!   as implausible.  Dimensions and leader texts take store-map indices
//!   without writing anything ("burned" indices), which shift later
//!   references; the walk notices the shift at the next loop and
//!   translates references through it.

use crate::Caps;
use crate::model::*;
use glam::DVec3;
use slerp_scene::LoadError;
use std::collections::{HashMap, HashSet};

/// The marker before every string: `FF FE FF`, then the length.
const STR_MARKER: &[u8] = b"\xFF\xFE\xFF";

/// SketchUp 3 lacks several fields every later version writes.
const FIRST_V4: u32 = 4;

/// How deep objects may nest (a definition holding an instance whose
/// definition is written inline, and so on).
const MAX_NESTING: usize = 128;

/// How many starting slot numbers are tried when the file doesn't say.
const MAX_BASES: usize = 64;

/*┌─────────────────╮
| Errors          |
└─────────────────╯*/
//region Errors

/// Why the walk stopped: the bytes made no sense (`Format`, which entity
/// lists may recover from) or ran out (`Truncated`).
#[derive(Debug)]
enum Bad {
    Format(String),
    Truncated,
}

type R<T> = Result<T, Bad>;

fn bad<T>(msg: impl Into<String>) -> R<T> {
    Err(Bad::Format(msg.into()))
}

impl Bad {
    fn message(&self) -> String {
        match self {
            Bad::Format(m) => m.clone(),
            Bad::Truncated => "the file ends early".into(),
        }
    }
}

//endregion

/*┌─────────────────╮
| Cursor          |
└─────────────────╯*/
//region Cursor

/// A read position in the file.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn raw(&mut self, n: usize) -> R<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Bad::Truncated)?;
        let s = self.data.get(self.pos..end).ok_or(Bad::Truncated)?;
        self.pos = end;
        Ok(s)
    }

    fn skip(&mut self, n: usize) -> R<()> {
        self.raw(n).map(|_| ())
    }

    fn u8(&mut self) -> R<u8> {
        Ok(self.raw(1)?[0])
    }

    fn u16(&mut self) -> R<u16> {
        let b = self.raw(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> R<u32> {
        let b = self.raw(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn f64(&mut self) -> R<f64> {
        let b = self.raw(8)?;
        Ok(f64::from_le_bytes(b.try_into().map_err(|_| Bad::Truncated)?))
    }

    fn f64s<const N: usize>(&mut self) -> R<[f64; N]> {
        let mut out = [0.0; N];
        for v in &mut out {
            *v = self.f64()?;
        }
        Ok(out)
    }

    fn peek(&self, n: usize) -> &'a [u8] {
        let end = self.pos.saturating_add(n).min(self.data.len());
        self.data.get(self.pos..end).unwrap_or(&[])
    }

    fn peek_u16(&self) -> R<u16> {
        u16_at(self.data, self.pos).ok_or(Bad::Truncated)
    }

    /// A string: the marker, a length (one byte, escaping to two and then
    /// four) and that many UTF-16 code units.
    fn utf16(&mut self) -> R<String> {
        if self.peek(3) != STR_MARKER {
            return bad(format!("expected a string at {:#x}", self.pos));
        }
        self.pos += 3;
        let mut n = self.u8()? as usize;
        if n == 0xFF {
            n = self.u16()? as usize;
            if n == 0xFFFF {
                n = self.u32()? as usize;
            }
        }
        let bytes = self.raw(n.checked_mul(2).ok_or(Bad::Truncated)?)?;
        let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        Ok(String::from_utf16_lossy(&units))
    }
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    data.get(at..at.checked_add(2)?).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    data.get(at..at.checked_add(4)?).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn find(data: &[u8], needle: &[u8], from: usize, to: usize) -> Option<usize> {
    let to = to.min(data.len());
    if from >= to {
        return None;
    }
    data[from..to].windows(needle.len()).position(|w| w == needle).map(|i| from + i)
}

//endregion

/*┌─────────────────╮
| Objects         |
└─────────────────╯*/
//region Objects

/// The classes SketchUp writes; anything else stops the walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Class {
    Vertex,
    Edge,
    Curve,
    ArcCurve,
    EdgeUse,
    Loop,
    Face,
    Layer,
    Material,
    Dib,
    AttributeContainer,
    AttributeNamed,
    Camera,
    Thumbnail,
    Relationship,
    ComponentDefinition,
    Image,
    ComponentInstance,
    Group,
    FaceTextureCoords,
    ConstructionLine,
    ConstructionPoint,
    SectionPlane,
    SkFont,
    DimensionLinear,
    Text,
}

impl Class {
    fn from_name(name: &str) -> Option<Class> {
        use Class::*;
        Some(match name {
            "CVertex" => Vertex,
            "CEdge" => Edge,
            "CCurve" => Curve,
            "CArcCurve" => ArcCurve,
            "CEdgeUse" => EdgeUse,
            "CLoop" => Loop,
            "CFace" => Face,
            "CLayer" => Layer,
            "CMaterial" => Material,
            "CDib" => Dib,
            "CAttributeContainer" => AttributeContainer,
            "CAttributeNamed" => AttributeNamed,
            "CCamera" => Camera,
            "CThumbnail" => Thumbnail,
            "CRelationship" => Relationship,
            "CComponentDefinition" => ComponentDefinition,
            "CImage" => Image,
            "CComponentInstance" => ComponentInstance,
            "CGroup" => Group,
            "CFaceTextureCoords" => FaceTextureCoords,
            "CConstructionLine" => ConstructionLine,
            "CConstructionPoint" => ConstructionPoint,
            "CSectionPlane" => SectionPlane,
            "CSkFont" => SkFont,
            "CDimensionLinear" => DimensionLinear,
            "CText" => Text,
            _ => return None,
        })
    }
}

/// The drawing properties every visible entity has.
#[derive(Clone, Copy, Debug, Default)]
struct Drawbase {
    /// The material's slot, 0 for none.
    material: u16,
    hidden: bool,
    soft: bool,
    smooth: bool,
    /// The tag's slot, 0 for the default.
    layer: u32,
}

#[derive(Clone, Copy, Debug)]
struct Ftc {
    front: [f64; 9],
    back: [f64; 9],
    /// The direction a projected texture is projected along, per side.
    front_axis: Option<DVec3>,
    back_axis: Option<DVec3>,
}

#[derive(Debug)]
struct TextureV {
    dib: Option<u32>,
    /// SketchUp 3 keeps the picture inline: its range in the file.
    inline: Option<(usize, usize)>,
    width: f64,
    height: f64,
    file_name: String,
}

#[derive(Debug)]
struct MaterialV {
    name: String,
    rgba: [u8; 4],
    opacity: f64,
    use_opacity: bool,
    texture: Option<TextureV>,
    colorized: bool,
}

/// What each object holds, as far as building the model needs.
#[derive(Debug)]
enum Value {
    /// Being read: objects inside it may refer back to it.
    Pending,
    Vertex(DVec3),
    Edge {
        a: Option<u32>,
        b: Option<u32>,
        db: Drawbase,
    },
    EdgeUse {
        edge: Option<u32>,
        reversed: bool,
    },
    Loop(Vec<Option<u32>>),
    Face(Box<FaceV>),
    Layer {
        name: String,
        hidden: bool,
    },
    Material(Box<MaterialV>),
    Dib {
        start: usize,
        len: usize,
    },
    Attrs(Vec<Option<u32>>),
    Ftc(Box<Ftc>),
    Image(Box<ImageV>),
    Definition(Box<DefinitionV>),
    Instance(Box<InstanceV>),
    /// Read only to stay aligned.
    Other,
}

// The larger values are boxed: a big file holds millions of objects, most
// of them vertices and edges.

#[derive(Debug)]
struct FaceV {
    db: Drawbase,
    plane: [f64; 4],
    loops: Vec<Option<u32>>,
    back: u16,
    attrs: Option<u32>,
}

#[derive(Debug)]
struct ImageV {
    db: Drawbase,
    def: Option<u32>,
    xform: [f64; 12],
}

#[derive(Debug)]
struct DefinitionV {
    name: String,
    entities: Vec<u32>,
    faces_camera: bool,
}

#[derive(Debug)]
struct InstanceV {
    db: Drawbase,
    def: Option<u32>,
    xform: [f64; 13],
    name: String,
}

enum Slot {
    Class(Class),
    Obj(Class, Value),
}

/// What reading an object reference found.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Null,
    Obj(Class),
    /// A reference into the part of the file before the walk's start.
    Premodel,
    /// A burned index: nothing was ever written for it.
    Reserved,
}

#[derive(Clone, Copy, Debug)]
struct Ref {
    slot: Option<u32>,
    kind: Kind,
}

//endregion

/*┌─────────────────╮
| Archive         |
└─────────────────╯*/
//region Archive

/// The store map and the walk's state.
struct Archive<'a> {
    r: Cursor<'a>,
    ver: u32,
    /// Entity preambles carry persistent ids from SketchUp 2017 on.
    has_pid: bool,
    slots: HashMap<u32, Slot>,
    class_slot: HashMap<Class, u32>,
    next_slot: u32,
    /// Slots below this belong to the unwalked start of the file.
    walk_base: u32,
    current_loop: Option<u32>,
    current_obj: Option<u32>,
    in_entity_list: bool,
    /// Burned index bands, in the file's numbering: (start, width).
    burns: Vec<(u32, u32)>,
    cum_delta: u32,
    annot_watermark: Option<u32>,
    burn_stack: Vec<u32>,
    cline_tail: Option<usize>,
    depth: usize,
}

impl<'a> Archive<'a> {
    fn new(data: &'a [u8], ver: u32, base: u32) -> Archive<'a> {
        Archive {
            r: Cursor { data, pos: 0 },
            ver,
            has_pid: ver >= 17,
            slots: HashMap::new(),
            class_slot: HashMap::new(),
            next_slot: base,
            walk_base: base,
            current_loop: None,
            current_obj: None,
            in_entity_list: false,
            burns: Vec::new(),
            cum_delta: 0,
            annot_watermark: None,
            burn_stack: Vec::new(),
            cline_tail: None,
            depth: 0,
        }
    }

    fn data(&self) -> &'a [u8] {
        self.r.data
    }

    fn alloc(&mut self, entry: Slot) -> R<u32> {
        let s = self.next_slot;
        self.slots.insert(s, entry);
        self.next_slot = s.checked_add(1).ok_or_else(|| Bad::Format("too many objects".into()))?;
        Ok(s)
    }

    fn value(&self, slot: Option<u32>) -> Option<&Value> {
        match self.slots.get(&slot?) {
            Some(Slot::Obj(_, v)) => Some(v),
            _ => None,
        }
    }

    fn is_class(&self, slot: u32) -> bool {
        matches!(self.slots.get(&slot), Some(Slot::Class(_)))
    }

    fn read_object(&mut self, expect: Option<Class>) -> R<Ref> {
        let tag = self.r.u16()?;
        if tag == 0 {
            return Ok(Ref { slot: None, kind: Kind::Null });
        }
        if tag == 0x7FFF {
            let big = self.r.u32()?;
            if big & 0x8000_0000 != 0 {
                return self.new_of_class(big & 0x7FFF_FFFF, expect);
            }
            return self.backref(big);
        }
        if tag == 0xFFFF {
            let _schema = self.r.u16()?;
            let len = self.r.u16()? as usize;
            if len > 40 {
                return bad(format!("implausible class name length at {:#x}", self.r.pos));
            }
            let name = self.r.raw(len)?;
            let name = std::str::from_utf8(name).ok().filter(|n| n.is_ascii()).unwrap_or("?");
            let Some(class) = Class::from_name(name) else {
                return bad(format!("no reader for class {name}"));
            };
            let s = self.alloc(Slot::Class(class))?;
            self.class_slot.insert(class, s);
            return self.new_obj(class);
        }
        if tag & 0x8000 != 0 {
            return self.new_of_class((tag & 0x7FFF) as u32, expect);
        }
        self.backref(tag as u32)
    }

    fn new_of_class(&mut self, cslot: u32, expect: Option<Class>) -> R<Ref> {
        let class = match self.slots.get(&cslot) {
            Some(Slot::Class(c)) => *c,
            Some(Slot::Obj(..)) => return bad(format!("class reference to an object at {:#x}", self.r.pos)),
            None => {
                // A class defined in the unwalked start of the file: learn it
                // from what the reader expects here.
                let Some(c) = expect else {
                    return bad(format!("class reference to unknown slot {cslot} at {:#x}", self.r.pos));
                };
                self.slots.insert(cslot, Slot::Class(c));
                self.class_slot.insert(c, cslot);
                c
            }
        };
        self.new_obj(class)
    }

    fn new_obj(&mut self, class: Class) -> R<Ref> {
        self.in_entity_list = false;
        if self.depth >= MAX_NESTING {
            return bad("objects nest too deeply");
        }
        let slot = self.alloc(Slot::Obj(class, Value::Pending))?;
        let (prev_obj, prev_depth) = (self.current_obj, self.depth);
        self.current_obj = Some(slot);
        self.depth += 1;
        let value = self.read_value(class);
        self.current_obj = prev_obj;
        self.depth = prev_depth;
        self.slots.insert(slot, Slot::Obj(class, value?));
        if matches!(class, Class::DimensionLinear | Class::Text) {
            self.annot_watermark = Some(self.next_slot);
        }
        Ok(Ref { slot: Some(slot), kind: Kind::Obj(class) })
    }

    fn read_value(&mut self, class: Class) -> R<Value> {
        use Class::*;
        match class {
            Vertex => self.read_vertex(),
            Edge => self.read_edge(),
            Curve => self.read_curve(),
            ArcCurve => self.read_arccurve(),
            EdgeUse => self.read_edgeuse(),
            Loop => self.read_loop(),
            Face => self.read_face(),
            Layer => self.read_layer(),
            Material => self.read_material(),
            Dib => self.read_dib(),
            AttributeContainer => self.read_attr_container(),
            AttributeNamed => self.read_attr_named(),
            Camera => self.read_camera().map(|_| Value::Other),
            Thumbnail => self.read_thumbnail(),
            Relationship => self.read_relationship(),
            ComponentDefinition => self.read_definition(),
            Image => self.read_image(),
            ComponentInstance | Group => self.read_instance(),
            FaceTextureCoords => self.read_ftc(),
            ConstructionLine => self.read_construction_line(),
            ConstructionPoint => self.read_construction_point(),
            SectionPlane => self.read_section_plane(),
            SkFont => self.read_font(),
            DimensionLinear => self.read_dimension(),
            Text => self.read_text(),
        }
    }

    /// Maps a slot in the file's numbering to the walk's, through the
    /// burned bands; `None` for a slot inside one.
    fn translate(&self, slot: u32) -> Option<u32> {
        let mut offset = 0u32;
        for &(start, width) in &self.burns {
            if slot < start {
                break;
            }
            if slot < start.saturating_add(width) {
                return None;
            }
            offset = offset.saturating_add(width);
        }
        Some(slot - offset.min(slot))
    }

    fn backref(&mut self, slot: u32) -> R<Ref> {
        let mut slot = slot;
        if self.burns.first().is_some_and(|b| slot >= b.0) {
            match self.translate(slot) {
                Some(s) => slot = s,
                None => return Ok(Ref { slot: Some(slot), kind: Kind::Reserved }),
            }
        }
        match self.slots.get(&slot) {
            None if slot < self.walk_base => Ok(Ref { slot: Some(slot), kind: Kind::Premodel }),
            None => bad(format!("reference to unread slot {slot} at {:#x}", self.r.pos)),
            Some(Slot::Class(_)) => bad(format!("reference to class slot {slot} at {:#x}", self.r.pos)),
            Some(Slot::Obj(c, _)) => Ok(Ref { slot: Some(slot), kind: Kind::Obj(*c) }),
        }
    }

    /// Whether the bytes at `at` start a `CLayer` record: its class
    /// definition, a reference to its known class, or (only for the
    /// throwaway probe, whose numbering can't match) any class reference.
    fn clayer_record_at(&self, at: usize, unmatched_class_ref: bool) -> bool {
        let data = self.data();
        let Some(tag) = u16_at(data, at) else { return false };
        if tag == 0xFFFF && u16_at(data, at + 4) == Some(6) && data.get(at + 6..at + 12) == Some(b"CLayer".as_slice()) {
            return true;
        }
        if let Some(&cs) = self.class_slot.get(&Class::Layer)
            && is_class_ref(data, at, cs)
        {
            return true;
        }
        unmatched_class_ref && tag & 0x8000 != 0 && tag != 0xFFFF
    }

    /// Some 2018 files without materials write 12 zero bytes and a u32
    /// after a coloured tag's tail before the next tag.
    fn skip_layer_colour_extension(&mut self) {
        let (data, pos) = (self.data(), self.r.pos);
        if pos + 18 > data.len() || data[pos..pos + 12].iter().any(|&b| b != 0) {
            return;
        }
        if self.clayer_record_at(pos + 16, true) {
            self.r.pos += 16;
        }
    }

    /// A 2018 template's default tag after a custom one: a parent
    /// reference before the active-tag reference, skipped only when both
    /// name tags already read.
    fn skip_layer_parent_ref(&mut self, me: Option<u32>) {
        let (data, pos) = (self.data(), self.r.pos);
        let (Some(a), Some(b), Some(me)) = (u16_at(data, pos), u16_at(data, pos + 2), me) else { return };
        let plain = |v: u16| v != 0 && v & 0x8000 == 0 && v != 0x7FFF;
        if !plain(a) || !plain(b) || a as u32 == me {
            return;
        }
        let is_layer = |v: u16| matches!(self.slots.get(&(v as u32)), Some(Slot::Obj(Class::Layer, _)));
        if is_layer(a) && is_layer(b) {
            self.r.pos += 2;
        }
    }

    /// Tag records past the declared count, with null separators between.
    fn collect_trailing_layers(&mut self, out: &mut Vec<u32>, unmatched_class_ref: bool) -> R<()> {
        while self.r.pos + 2 <= self.data().len() {
            if self.r.peek_u16()? == 0 {
                self.r.pos += 2;
                continue;
            }
            if !self.clayer_record_at(self.r.pos, unmatched_class_ref) {
                break;
            }
            let r = self.read_object(Some(Class::Layer))?;
            if let (Some(s), Kind::Obj(Class::Layer)) = (r.slot, r.kind) {
                out.push(s);
            }
        }
        Ok(())
    }

    /// Whether a list item can start at `at`: null, an escape, a class
    /// definition or reference, or a reference to an object already read.
    fn plausible_list_tag(&self, at: usize) -> bool {
        let Some(t) = u16_at(self.data(), at) else { return false };
        match t {
            0 | 0x7FFF | 0xFFFF => true,
            t if t & 0x8000 != 0 => self.is_class((t & 0x7FFF) as u32),
            t => (t as u32) < self.next_slot,
        }
    }

    /// Whether `at` starts an object in an unambiguous form (null only when
    /// `allow_null`): plain back-references don't count, since any two bytes
    /// below 0x8000 would pass.
    fn strict_next_tag(&self, at: usize, allow_null: bool) -> bool {
        let Some(t) = u16_at(self.data(), at) else { return false };
        match t {
            0 => allow_null,
            0x7FFF | 0xFFFF => true,
            t if t & 0x8000 != 0 => self.is_class((t & 0x7FFF) as u32),
            _ => false,
        }
    }

    /// SketchUp 2020 writes an empty string and zero padding ahead of some
    /// counts, which leaves the reader early.  After a count read as
    /// implausible (or zero) at `count_pos`, finds the real count: the
    /// first non-zero u32 after the padding (or up to three bytes before it,
    /// for counts with a zero low byte) that is at most `limit` and is
    /// followed by a plausible list item.
    fn retry_count_after_filler(&mut self, count_pos: usize, limit: u32) -> Option<u32> {
        let data = self.data();
        let end = (count_pos + 12).min(data.len().saturating_sub(3));
        let marker = (count_pos..end).find(|&i| &data[i..i + 3] == STR_MARKER)?;
        if *data.get(marker + 3)? != 0 {
            return None;
        }
        let mut at = marker + 4;
        while data.get(at) == Some(&0) {
            at += 1;
        }
        for back in 0..4 {
            let Some(at2) = at.checked_sub(back) else { continue };
            if at2 < marker + 4 {
                continue;
            }
            let Some(count) = u32_at(data, at2) else { continue };
            if count == 0 || count > limit || !self.plausible_list_tag(at2 + 4) {
                continue;
            }
            self.r.pos = at2 + 4;
            return Some(count);
        }
        None
    }

    fn register_burn(&mut self, delta: u32) {
        let watermark = self.annot_watermark.unwrap_or(0);
        self.burns.push((watermark.saturating_add(self.cum_delta), delta));
        self.cum_delta = self.cum_delta.saturating_add(delta);
        self.annot_watermark = None;
        // Each burn is one phantom entity the list's count includes.
        if let Some(last) = self.burn_stack.last_mut() {
            *last += 1;
        }
    }
}

/// Whether the bytes at `p` reference class `slot`: `0x8000 | slot`, or for
/// large slots the escape `0x7FFF` and `0x80000000 | slot`.
fn is_class_ref(data: &[u8], p: usize, slot: u32) -> bool {
    if slot < 0x7FFF {
        u16_at(data, p) == Some(0x8000 | slot as u16)
    } else {
        u16_at(data, p) == Some(0x7FFF) && u32_at(data, p + 2) == Some(0x8000_0000 | slot)
    }
}

//endregion

/*┌─────────────────╮
| Readers         |
└─────────────────╯*/
//region Readers

impl<'a> Archive<'a> {
    /// The start of every entity: its attribute container and, from 2017
    /// on, a persistent id (a mask of which bytes follow).  Returns the
    /// container.
    fn preamble(&mut self) -> R<Ref> {
        let attrs = self.read_object(Some(Class::AttributeContainer))?;
        if self.has_pid {
            let mask = self.r.u8()?;
            self.r.skip(mask.count_ones() as usize)?;
        }
        Ok(attrs)
    }

    /// The preamble of vertices, loops, edge uses, materials and tags,
    /// which SketchUp 3 doesn't write.
    fn early_preamble(&mut self) -> R<()> {
        if self.ver >= FIRST_V4 {
            self.preamble()?;
        }
        Ok(())
    }

    fn drawbase(&mut self) -> R<Drawbase> {
        let b = self.r.raw(if self.ver < FIRST_V4 { 5 } else { 8 })?;
        // The tag is normally its slot, but an entity can name it by object:
        // a whole CLayer record on first use or an escaped reference later.
        // Tag slots never have the high bit and never equal 0x7FFF.
        let tag = self.r.peek_u16()?;
        let layer = match self.class_slot.get(&Class::Layer) {
            Some(&cs) if tag as u32 == (0x8000 | cs) => {
                self.read_object(Some(Class::Layer))?;
                0
            }
            _ if tag == 0x7FFF => {
                self.r.u16()?;
                if self.r.u32()? & 0x8000_0000 != 0 {
                    return bad("drawbase: unexpected class");
                }
                0
            }
            _ => self.r.u16()? as u32,
        };
        if self.ver < FIRST_V4 {
            return Ok(Drawbase { layer, ..Drawbase::default() });
        }
        Ok(Drawbase {
            material: u16::from_le_bytes([b[0], b[1]]),
            hidden: b[2] != 0,
            soft: b[5] != 0,
            smooth: b[6] != 0,
            layer,
        })
    }

    fn read_vertex(&mut self) -> R<Value> {
        self.early_preamble()?;
        Ok(Value::Vertex(DVec3::from_array(self.r.f64s::<3>()?)))
    }

    fn read_edge(&mut self) -> R<Value> {
        self.preamble()?;
        let db = self.drawbase()?;
        // The two vertices come before the curve.
        let a = self.read_object(Some(Class::Vertex))?;
        let b = self.read_object(Some(Class::Vertex))?;
        let curve = self.read_object(None)?;
        if !matches!(curve.kind, Kind::Null | Kind::Obj(Class::Curve | Class::ArcCurve)) {
            return bad(format!("edge curve is {:?}", curve.kind));
        }
        let vertex = |r: Ref| (r.kind == Kind::Obj(Class::Vertex)).then_some(r.slot).flatten();
        Ok(Value::Edge { a: vertex(a), b: vertex(b), db })
    }

    fn read_curve(&mut self) -> R<Value> {
        self.preamble()?;
        self.r.u8()?;
        self.r.u32()?;
        Ok(Value::Other)
    }

    /// An arc: centre, normal, x axis, start and end angles, y axis.
    fn read_arccurve(&mut self) -> R<Value> {
        self.preamble()?;
        self.r.skip(5)?;
        self.r.f64s::<14>()?;
        Ok(Value::Other)
    }

    /// One use of an edge in a loop.  It ends with a reference to its loop,
    /// which keeps the walk honest: a mismatch is either burned indices
    /// (after a dimension or text) or a misread.
    fn read_edgeuse(&mut self) -> R<Value> {
        self.early_preamble()?;
        let edge = self.read_object(Some(Class::Edge))?;
        let sense = self.r.u8()?;
        let p0 = self.r.pos;
        let tag = self.r.u16()?;
        let parent = if tag == 0x7FFF {
            let ps = self.r.u32()?;
            if ps & 0x8000_0000 != 0 {
                return bad("edge use parent is a new object");
            }
            Some(ps)
        } else if tag == 0xFFFF || tag & 0x8000 != 0 {
            return bad("edge use parent is a new object");
        } else {
            (tag != 0).then_some(tag as u32)
        };
        let expected = self.current_loop.map(|l| l.saturating_add(self.cum_delta));
        if parent != expected {
            let delta = match (parent, expected) {
                (Some(p), Some(e)) if p > e => p - e,
                _ => 0,
            };
            if delta > 0 && delta <= 4096 && self.annot_watermark.is_some() {
                self.register_burn(delta);
            } else {
                self.r.pos = p0;
                return bad(format!("edge use's loop {parent:?} isn't the current loop {expected:?}"));
            }
        }
        let edge = (edge.kind == Kind::Obj(Class::Edge)).then_some(edge.slot).flatten();
        Ok(Value::EdgeUse { edge, reversed: sense != 0 })
    }

    fn read_loop(&mut self) -> R<Value> {
        let me = self.next_slot - 1;
        let prev = self.current_loop;
        self.current_loop = Some(me);
        self.early_preamble()?;
        self.r.skip(2)?;
        let mut uses = Vec::new();
        loop {
            if self.r.peek_u16()? == 0 {
                self.r.pos += 2;
                break;
            }
            let u = self.read_object(Some(Class::EdgeUse))?;
            uses.push(u.slot);
        }
        self.current_loop = prev;
        Ok(Value::Loop(uses))
    }

    fn read_face(&mut self) -> R<Value> {
        let pre = self.preamble()?;
        let mut db = self.drawbase()?;
        let mut attrs = pre.slot;
        // SketchUp 3: the reference that opens a face is its front material.
        if self.ver < FIRST_V4 && pre.kind == Kind::Obj(Class::Material) {
            db.material = pre.slot.unwrap_or(0) as u16;
            attrs = None;
        }
        let plane = self.r.f64s::<4>()?;
        let count = self.r.u32()?;
        if count > 10_000 {
            return bad(format!("implausible loop count {count}"));
        }
        let mut loops = Vec::new();
        for _ in 0..count {
            loops.push(self.read_object(Some(Class::Loop))?.slot);
        }
        // Edges first written inside this face's loops follow as back
        // references in the entity list, which reads them as items.
        let back = self.r.u16()?;
        Ok(Value::Face(Box::new(FaceV { db, plane, loops, back, attrs })))
    }

    fn read_attr_container(&mut self) -> R<Value> {
        self.preamble()?;
        let mut children = Vec::new();
        loop {
            if self.r.peek_u16()? == 0 {
                self.r.pos += 2;
                break;
            }
            children.push(self.read_object(Some(Class::AttributeNamed))?.slot);
        }
        Ok(Value::Attrs(children))
    }

    fn read_attr_named(&mut self) -> R<Value> {
        self.preamble()?;
        self.r.skip(4)?;
        self.r.utf16()?;
        loop {
            if self.r.utf16()?.is_empty() {
                break;
            }
            let t = self.r.u8()?;
            self.skip_typed(t, 0)?;
        }
        // A trailing u32 from SketchUp 7 on; SketchUp 6 ends the record at
        // the empty key.
        if self.ver >= 7 {
            self.r.u32()?;
        }
        Ok(Value::Other)
    }

    /// Skips one attribute value of type `t`.
    fn skip_typed(&mut self, t: u8, depth: usize) -> R<()> {
        match t {
            0x00 => {}
            0x04 | 0x09 => self.r.skip(4)?,
            0x06 | 0x0C => self.r.skip(8)?,
            0x07 => self.r.skip(1)?,
            0x0A => {
                self.r.utf16()?;
            }
            0x0B => {
                let n = self.r.u32()?;
                if n > 100_000 || depth > 32 {
                    return bad("implausible attribute array");
                }
                for _ in 0..n {
                    let t = self.r.u8()?;
                    self.skip_typed(t, depth + 1)?;
                }
            }
            0x11 | 0x12 => self.r.skip(24)?,
            t => return bad(format!("unknown attribute value type {t:#x}")),
        }
        Ok(())
    }

    fn read_layer(&mut self) -> R<Value> {
        self.early_preamble()?;
        let name = self.r.utf16()?;
        // Flags: three bytes in 2016, four from 2017.
        let mut hidden = None;
        let mut n = 0;
        while n < 8 && self.r.peek(3) != STR_MARKER {
            let b = self.r.u8()?;
            hidden.get_or_insert(b);
            n += 1;
        }
        self.r.utf16()?;
        let flags = self.r.u16()?;
        let hidden = hidden.unwrap_or(0) != 0;
        if flags & 0x00FF != 0 {
            // Colour by tag with a textured material: the texture block a
            // material carries.
            self.texture_block()?;
            self.r.skip(4)?;
            return Ok(Value::Layer { name, hidden });
        }
        self.r.skip(4)?;
        self.r.utf16()?;
        self.r.skip(if self.ver < FIRST_V4 { 17 } else { 21 })?;
        self.skip_layer_colour_extension();
        self.skip_layer_parent_ref(self.current_obj);
        Ok(Value::Layer { name, hidden })
    }

    /// A textured material's payload, shared with tags: a picture (a CDib
    /// after its own preamble), the tile size, the file name, the average
    /// colour and the opacity.
    fn texture_block(&mut self) -> R<MaterialV> {
        let (dib, inline) = if self.ver < FIRST_V4 {
            let start = self.r.pos;
            self.read_dib()?;
            let len = self.r.pos - start;
            (None, Some((start + 8, len - 8)))
        } else {
            // The caller read the flag as a u16, whose high byte is the
            // first byte of this preamble.
            self.r.pos -= 1;
            self.preamble()?;
            let r = self.read_object(Some(Class::Dib))?;
            if !matches!(self.value(r.slot), Some(Value::Dib { .. })) {
                return bad("texture object is not a picture");
            }
            (r.slot, None)
        };
        // An optional u32 before the tile size.
        let pos = self.r.pos;
        match find(self.data(), STR_MARKER, pos, pos + 28).map(|m| m - pos) {
            Some(20) => self.r.skip(4)?,
            Some(16) => {}
            _ => return bad("texture size block misaligned"),
        }
        let width = self.r.f64()?;
        let height = self.r.f64()?;
        let file_name = self.r.utf16()?;
        // RGBA, a zero byte, and the colour again.
        let avg = self.r.raw(9)?;
        let rgba = [avg[0], avg[1], avg[2], avg[3]];
        self.r.utf16()?;
        let blob = self.r.raw(8)?;
        let opacity = self.r.f64()?;
        let use_opacity = self.r.u8()? != 0;
        // A colourised texture keeps the original picture and the tint as
        // its colour: flagged in the blob, or by alpha 0xFF on the colour.
        let colorized = blob[4] != 0 || rgba[3] == 0xFF;
        Ok(MaterialV {
            name: String::new(),
            rgba,
            opacity,
            use_opacity,
            texture: Some(TextureV { dib, inline, width, height, file_name }),
            colorized,
        })
    }

    fn read_material(&mut self) -> R<Value> {
        self.early_preamble()?;
        let name = self.r.utf16()?;
        let flag = self.r.u16()?;
        let mut m = if flag == 0 {
            let c = self.r.raw(4)?;
            let rgba = [c[0], c[1], c[2], c[3]];
            self.r.utf16()?;
            self.r.skip(8)?;
            let opacity = self.r.f64()?;
            let use_opacity = self.r.u8()? != 0;
            MaterialV { name: String::new(), rgba, opacity, use_opacity, texture: None, colorized: false }
        } else {
            self.texture_block()?
        };
        m.name = name;
        Ok(Value::Material(Box::new(m)))
    }

    fn read_dib(&mut self) -> R<Value> {
        self.r.u32()?;
        let len = self.r.u32()? as usize;
        let start = self.r.pos;
        self.r.skip(len)?;
        Ok(Value::Dib { start, len })
    }

    /// Texture positions of a face's two sides, with their pins.
    fn read_ftc(&mut self) -> R<Value> {
        self.preamble()?;
        self.r.u32()?;
        let ks = self.r.f64s::<24>()?;
        for _ in 0..2 {
            let pins = self.r.u32()? as usize;
            self.r.skip(pins.checked_mul(32).ok_or(Bad::Truncated)?)?;
        }
        let front_flags = self.r.u32()?;
        let back_flags = self.r.u32()?;
        let mut front = [0.0; 9];
        let mut back = [0.0; 9];
        front.copy_from_slice(&ks[0..9]);
        back.copy_from_slice(&ks[12..21]);
        // Each side's matrix is followed by three doubles: for a projected
        // texture (flag bit 1), the direction it is projected along, whose
        // plane its coordinates run in.
        let axis = |flags: u32, v: &[f64]| {
            let a = DVec3::new(v[0], v[1], v[2]);
            (flags & 2 != 0 && a.is_finite() && a.length_squared() > 1e-12).then(|| a.normalize())
        };
        Ok(Value::Ftc(Box::new(Ftc {
            front,
            back,
            front_axis: axis(front_flags, &ks[9..12]),
            back_axis: axis(back_flags, &ks[21..24]),
        })))
    }

    fn read_camera(&mut self) -> R<()> {
        self.r.skip(137)?;
        self.r.u16()?;
        self.r.utf16()?;
        self.r.skip(33)
    }

    fn read_thumbnail(&mut self) -> R<Value> {
        self.preamble()?;
        if self.ver < FIRST_V4 {
            self.r.skip(129)?;
        } else {
            self.read_object(Some(Class::Camera))?;
            self.read_object(Some(Class::Dib))?;
        }
        Ok(Value::Other)
    }

    /// An Image entity: placed like an instance, with its picture's
    /// definition.
    fn read_image(&mut self) -> R<Value> {
        self.preamble()?;
        let db = self.drawbase()?;
        let def = self.read_object(None)?;
        let xform = self.r.f64s::<12>()?;
        self.r.f64()?;
        self.r.utf16()?;
        self.r.skip(16)?;
        let def = (def.kind == Kind::Obj(Class::ComponentDefinition)).then_some(def.slot).flatten();
        Ok(Value::Image(Box::new(ImageV { db, def, xform })))
    }

    fn read_relationship(&mut self) -> R<Value> {
        self.preamble()?;
        self.entity_ref()?;
        self.entity_ref()?;
        Ok(Value::Other)
    }

    /// A reference that may point forward, past the walk (annotations are
    /// written before what they label).
    fn entity_ref(&mut self) -> R<Option<u32>> {
        let tag = self.r.u16()?;
        match tag {
            0 => Ok(None),
            0x7FFF => {
                let big = self.r.u32()?;
                if big & 0x8000_0000 != 0 {
                    return bad("entity reference is a new object");
                }
                Ok(Some(big))
            }
            t if t == 0xFFFF || t & 0x8000 != 0 => bad("entity reference is a new object"),
            t => Ok(Some(t as u32)),
        }
    }

    fn read_construction_line(&mut self) -> R<Value> {
        self.preamble()?;
        self.drawbase()?;
        self.r.f64s::<8>()?;
        // The tail's length depends on the build that wrote the file (7, 4
        // or 0 bytes): found on the first guide line by which length lands
        // on an unambiguous next record, then kept.
        let k = match self.cline_tail {
            Some(k) => k,
            None => {
                // Unambiguous next records first; then, for a guide that
                // ends its list (followed by a zero count), a null.
                let order = if self.ver == 17 { [7, 0, 4] } else { [4, 0, 7] };
                let pos = self.r.pos;
                let k = order
                    .into_iter()
                    .find(|&c| self.strict_next_tag(pos + c, false))
                    .or_else(|| [4, 0, 7].into_iter().find(|&c| self.strict_next_tag(pos + c, true)))
                    .unwrap_or(4);
                self.cline_tail = Some(k);
                k
            }
        };
        self.r.skip(k)?;
        Ok(Value::Other)
    }

    fn read_construction_point(&mut self) -> R<Value> {
        self.preamble()?;
        self.drawbase()?;
        self.r.f64s::<6>()?;
        self.r.u8()?;
        Ok(Value::Other)
    }

    fn read_section_plane(&mut self) -> R<Value> {
        self.preamble()?;
        self.drawbase()?;
        // An optional object before the plane: a plane starts with a unit
        // normal's component, which a tag word doesn't decode as.
        let first = self.r.peek(8);
        let first = if first.len() == 8 { f64::from_le_bytes(first.try_into().unwrap_or([0; 8])) } else { 0.0 };
        if first.is_nan() || first.abs() > 1.0001 {
            self.read_object(None)?;
        }
        self.r.f64s::<4>()?;
        if self.r.peek(3) == STR_MARKER {
            self.r.utf16()?;
            self.r.utf16()?;
        }
        Ok(Value::Other)
    }

    fn read_font(&mut self) -> R<Value> {
        self.read_object(Some(Class::AttributeContainer))?;
        if self.has_pid {
            let mask = self.r.u8()?;
            self.r.skip(mask.count_ones() as usize)?;
        }
        self.r.utf16()?;
        self.r.skip(15)?;
        Ok(Value::Other)
    }

    /// A linear dimension: 161 fixed bytes around two connection
    /// references, each followed by the instance paths of what it is
    /// attached to.
    fn read_dimension(&mut self) -> R<Value> {
        self.preamble()?;
        self.drawbase()?;
        self.r.utf16()?;
        self.read_object(Some(Class::SkFont))?;
        self.r.skip(37)?;
        self.entity_ref()?;
        self.connection_paths()?;
        self.r.skip(32)?;
        self.entity_ref()?;
        self.connection_paths()?;
        self.r.skip(72)?;
        Ok(Value::Other)
    }

    fn connection_paths(&mut self) -> R<()> {
        self.entity_ref()?;
        for _ in 0..2 {
            let count = self.r.u32()?;
            if count > 1000 {
                return bad("implausible dimension path length");
            }
            for _ in 0..count {
                self.path_ref()?;
            }
        }
        Ok(())
    }

    /// One instance on a dimension's path: usually a reference, but written
    /// in full when the dimension comes before the group it is in.
    fn path_ref(&mut self) -> R<()> {
        let tag = self.r.peek_u16()?;
        let new = if tag == 0x7FFF {
            u32_at(self.data(), self.r.pos + 2).is_some_and(|b| b & 0x8000_0000 != 0)
        } else {
            tag == 0xFFFF || tag & 0x8000 != 0
        };
        if new {
            self.read_object(None)?;
        } else {
            self.entity_ref()?;
        }
        Ok(())
    }

    fn read_text(&mut self) -> R<Value> {
        self.preamble()?;
        self.drawbase()?;
        self.read_object(Some(Class::SkFont))?;
        // A middle of varying length, ending in an 11-byte block
        // `01 00 00 00 ?? 00 <arrow u32> 01` right before the text.
        let data = self.data();
        let limit = self.r.pos + 512;
        let mut p = self.r.pos;
        let idx = loop {
            let Some(idx) = find(data, STR_MARKER, p, limit) else {
                return bad("text delimiter not found");
            };
            if idx >= 11 {
                let blk = &data[idx - 11..idx];
                let arrow = u32::from_le_bytes([blk[6], blk[7], blk[8], blk[9]]);
                if blk[..4] == [1, 0, 0, 0] && arrow <= 8 && blk[10] == 1 {
                    break idx;
                }
            }
            p = idx + 3;
        };
        self.r.pos = idx;
        self.r.utf16()?;
        self.r.skip(5)?;
        // Attachments, only in their escaped form.
        while self.r.peek(2) == [0xFF, 0x7F] {
            match u32_at(data, self.r.pos + 2) {
                Some(v) if v & 0x8000_0000 == 0 => self.r.skip(6)?,
                _ => break,
            }
        }
        Ok(Value::Other)
    }

    /// Whether the entity list being read has had burned indices.
    fn burned(&self) -> bool {
        self.burn_stack.last().is_some_and(|&b| b > 0)
    }

    fn read_entity_list(&mut self, count: u32, root: bool) -> R<Vec<u32>> {
        self.burn_stack.push(0);
        let out = self.read_entity_list_inner(count, root);
        self.burn_stack.pop();
        out
    }

    fn read_entity_list_inner(&mut self, count: u32, root: bool) -> R<Vec<u32>> {
        let mut entities = Vec::new();
        let mut read = 0u32;
        while read < count {
            let p = self.r.pos;
            if !root && self.burned() {
                // Burned indices make the count include phantom entities;
                // the definition's tail (no relationships, padding, a GUID
                // and the name's marker) marks the real end.
                let zero = u32_at(self.data(), p).ok_or(Bad::Truncated)? == 0;
                if zero && self.data().get(p + 22..p + 25) == Some(STR_MARKER) {
                    break;
                }
            }
            let prev = self.in_entity_list;
            self.in_entity_list = true;
            let result = self.read_object(None);
            self.in_entity_list = prev;
            match result {
                Ok(r) => {
                    if let (Some(s), Kind::Obj(_)) = (r.slot, r.kind) {
                        entities.push(s);
                    }
                }
                // An over-declared root count runs into the document's tail;
                // a list with burned indices overshoots its real end.
                Err(Bad::Format(_)) if root || self.burned() => {
                    self.r.pos = p;
                    break;
                }
                Err(e) => return Err(e),
            }
            read += 1;
        }
        Ok(entities)
    }

    fn read_definition(&mut self) -> R<Value> {
        self.preamble()?;
        self.r.skip(if self.ver >= 17 {
            22
        } else if self.ver >= FIRST_V4 {
            20
        } else {
            13
        })?;
        let layers = self.r.u32()?;
        if layers > 10_000 {
            return bad("implausible tag count in a definition");
        }
        // SketchUp 2020 puts null separators between the tags.
        let mut got = 0;
        while got < layers {
            if self.r.peek_u16()? == 0 {
                self.r.pos += 2;
                continue;
            }
            self.read_object(Some(Class::Layer))?;
            got += 1;
        }
        if self.r.u16()? == 0x7FFF {
            self.r.u32()?;
        }
        let mut count = None;
        if self.ver >= 20 {
            count = self.retry_count_after_filler(self.r.pos, 5_000_000);
        }
        let mut count = match count {
            Some(c) => c,
            None => {
                self.r.u32()?;
                self.r.u32()?
            }
        };
        if (count > 5_000_000 || count == 0)
            && let Some(retry) = self.retry_count_after_filler(self.r.pos - 4, 5_000_000)
        {
            count = retry;
        }
        if count > 5_000_000 {
            return bad("implausible entity count in a definition");
        }
        let entities = self.read_entity_list(count, false)?;
        let mut relationships = self.r.u32()?;
        if relationships > 100_000
            && let Some(retry) = self.retry_count_after_filler(self.r.pos - 4, 100_000)
        {
            relationships = retry;
        }
        if relationships > 100_000 {
            return bad("definition list misaligned");
        }
        for _ in 0..relationships {
            self.read_object(Some(Class::Relationship))?;
        }
        self.r.u16()?;
        self.r.pos = reanchor(self.data(), self.r.pos);
        self.r.skip(16)?;
        let name = self.r.utf16()?;
        self.r.utf16()?;
        self.r.utf16()?;
        self.r.u32()?;
        // An undecoded block, then the definition's thumbnail.
        let data = self.data();
        let thumb = self.class_slot.get(&Class::Thumbnail).copied();
        let tpos = (0..96).map(|off| self.r.pos + off).find(|&p| {
            let declared = data.get(p..p + 2) == Some(b"\xff\xff".as_slice())
                && data.get(p + 4..p + 6) == Some(b"\x0a\x00".as_slice())
                && data.get(p + 6..p + 16) == Some(b"CThumbnail".as_slice());
            declared || thumb.is_some_and(|t| is_class_ref(data, p, t))
        });
        let Some(tpos) = tpos else { return bad("definition tail: thumbnail not found") };
        let gap = self.r.raw(tpos - self.r.pos)?;
        // Behaviour flags 9 bytes before the thumbnail: bit 0 is "always
        // face camera".
        let behaviour = if gap.len() >= 9 { gap[gap.len() - 9] } else { 0 };
        self.read_object(Some(Class::Thumbnail))?;
        Ok(Value::Definition(Box::new(DefinitionV { name, entities, faces_camera: behaviour & 1 != 0 })))
    }

    fn read_instance(&mut self) -> R<Value> {
        self.preamble()?;
        let db = self.drawbase()?;
        let def = self.read_object(Some(Class::ComponentDefinition))?;
        if def.kind != Kind::Obj(Class::ComponentDefinition) {
            return bad(format!("instance definition is {:?}", def.kind));
        }
        let xform = self.r.f64s::<13>()?;
        let name = if self.ver >= FIRST_V4 || self.r.peek(3) == STR_MARKER { self.r.utf16()? } else { String::new() };
        // A trailing GUID from SketchUp 2014 on, for groups and components
        // alike.
        if self.ver >= 14 {
            self.r.skip(16)?;
        }
        Ok(Value::Instance(Box::new(InstanceV { db, def: def.slot, xform, name })))
    }
}

/// A definition's GUID is followed by its name's marker; some files put
/// the GUID a few bytes off where the fixed layout expects it.
fn reanchor(data: &[u8], pos: usize) -> usize {
    let at = |p: usize| data.get(p + 16..p + 19) == Some(STR_MARKER);
    if at(pos) {
        return pos;
    }
    for skip in [-1isize, 1, -2, 2, -3, 3, -4, 4] {
        if let Some(p) = pos.checked_add_signed(skip)
            && at(p)
        {
            return p;
        }
    }
    pos
}

//endregion

/*┌─────────────────╮
| Walk            |
└─────────────────╯*/
//region Walk

/// Whether `data` is a classic SketchUp file (and not a 2021 one).
pub fn is_classic(data: &[u8]) -> bool {
    data.starts_with(b"\xff\xfe\xff\x0e")
        && find(data, b"PK\x03\x04", 0, 0x100).is_none()
        && find(data, b"CVersionMap", 0, 0x200).is_some()
}

/// The major version from the header's "{17.2.2555}".
fn major_version(data: &[u8]) -> Option<u32> {
    let text: Vec<u8> = data[..data.len().min(0x60)].iter().copied().filter(|&b| b != 0).collect();
    let open = text.iter().position(|&b| b == b'{')?;
    let digits: String = text[open + 1..].iter().take_while(|b| b.is_ascii_digit()).map(|&b| b as char).collect();
    (text.get(open + 1 + digits.len()) == Some(&b'.')).then(|| digits.parse().ok()).flatten()
}

/// What a successful walk found.
struct Walked<'a> {
    ar: Archive<'a>,
    root: Vec<u32>,
    layers: Vec<u32>,
    materials: Vec<u32>,
}

/// Finds the material list and the slot numbering, then walks the model.
fn walk(data: &[u8]) -> R<Walked<'_>> {
    let ver = major_version(data).ok_or_else(|| Bad::Format("no version in the header".into()))?;
    // 1. The start: the material list (its count is the u32 before the
    //    first CMaterial class record), or with no materials the tag list.
    let class_record = |name: &[u8]| {
        let mut needle = (name.len() as u16).to_le_bytes().to_vec();
        needle.extend_from_slice(name);
        let mut from = 0;
        while let Some(i) = find(data, &needle, from, data.len()) {
            if i >= 4 && data[i - 4..i - 2] == [0xFF, 0xFF] {
                return Some(i - 4);
            }
            from = i + 1;
        }
        None
    };
    let (start, materials) = match class_record(b"CMaterial") {
        Some(s) => {
            let count = s.checked_sub(4).and_then(|p| u32_at(data, p)).ok_or(Bad::Truncated)?;
            if count > 100_000 {
                return bad("implausible material count");
            }
            (s, count)
        }
        None => {
            let l = class_record(b"CLayer").ok_or_else(|| Bad::Format("no CMaterial or CLayer record".into()))?;
            let back = if ver >= 17 { 9 } else { 8 };
            (l.checked_sub(back).ok_or(Bad::Truncated)?, 0)
        }
    };
    // 2. The slot numbering.
    let bases = if materials >= 2 {
        vec![bootstrap_two_materials(data, ver, start)?]
    } else {
        probe_bases(data, ver, start, materials)?
    };
    // 3. The walk, from each candidate until one gets through.
    let mut last = Bad::Format("no starting slot found".into());
    for base in bases.into_iter().take(MAX_BASES) {
        match walk_model(data, ver, start, materials, base) {
            Ok(w) => return Ok(w),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// With two materials, reading the first with throwaway numbering shows the
/// second's class reference, which names CMaterial's real slot.
fn bootstrap_two_materials(data: &[u8], ver: u32, start: usize) -> R<u32> {
    let mut boot = Archive::new(data, ver, 1 << 20);
    boot.r.pos = start;
    boot.read_object(Some(Class::Material))?;
    let tag = boot.r.peek_u16()?;
    if tag == 0xFFFF || tag & 0x8000 == 0 {
        return bad("cannot find the slot numbering");
    }
    Ok((tag & 0x7FFF) as u32)
}

/// Without two materials: reads the materials and tags with throwaway
/// numbering; the reference after the tag list is an absolute reference to
/// the active tag, which gives one candidate numbering per tag.
fn probe_bases(data: &[u8], ver: u32, start: usize, materials: u32) -> R<Vec<u32>> {
    let b0 = 1u32 << 20;
    let mut boot = Archive::new(data, ver, b0);
    boot.r.pos = start;
    for _ in 0..materials {
        boot.read_object(Some(Class::Material))?;
    }
    boot.r.u32()?;
    if ver >= 17 {
        boot.r.u8()?;
    }
    let count = boot.r.u32()?;
    if !(1..=100_000).contains(&count) {
        return bad("implausible tag count");
    }
    let mut layer_slots = Vec::new();
    for _ in 0..count {
        if let Some(s) = boot.read_object(Some(Class::Layer))?.slot {
            layer_slots.push(s);
        }
    }
    boot.collect_trailing_layers(&mut layer_slots, true)?;
    let anchor = boot.read_object(None)?;
    let (Some(s), Kind::Premodel) = (anchor.slot, anchor.kind) else {
        return bad("the definition list's anchor isn't a tag");
    };
    Ok(layer_slots
        .iter()
        .filter_map(|&rel| (s as i64 - (rel as i64 - b0 as i64)).try_into().ok())
        .filter(|&base: &u32| base > 0 && base < b0)
        .collect())
}

fn walk_model(data: &[u8], ver: u32, start: usize, material_count: u32, base: u32) -> R<Walked<'_>> {
    let mut ar = Archive::new(data, ver, base);
    ar.r.pos = start;
    // 1. Materials.
    let mut materials = Vec::new();
    for _ in 0..material_count {
        if let Some(s) = ar.read_object(Some(Class::Material))?.slot {
            materials.push(s);
        }
    }
    // 2. Tags: <u32><u8 from 2017><u32 count>; SketchUp 3 writes no count.
    ar.r.u32()?;
    if ver >= 17 {
        ar.r.u8()?;
    }
    let count = if ver < FIRST_V4 { u32::MAX } else { ar.r.u32()? };
    if ver >= FIRST_V4 && count > 100_000 {
        return bad("implausible tag count");
    }
    let mut layers = Vec::new();
    while (layers.len() as u32) < count {
        let tag = ar.r.peek_u16()?;
        if tag == 0 {
            ar.r.pos += 2;
            continue;
        }
        if tag != 0xFFFF && tag & 0x8000 == 0 {
            break;
        }
        let r = ar.read_object(Some(Class::Layer))?;
        if let (Some(s), Kind::Obj(Class::Layer)) = (r.slot, r.kind) {
            layers.push(s);
        }
    }
    ar.collect_trailing_layers(&mut layers, false)?;
    // 3. Definitions, after a reference to the active tag.
    if ar.read_object(None)?.kind != Kind::Obj(Class::Layer) {
        return bad("the definition list's anchor isn't a tag");
    }
    let mut defs = ar.r.u32()?;
    if defs > 1_000_000
        && let Some(retry) = ar.retry_count_after_filler(ar.r.pos - 4, 1_000_000)
    {
        defs = retry;
    }
    if defs > 1_000_000 {
        return bad("implausible definition count");
    }
    for _ in 0..defs {
        ar.read_object(Some(Class::ComponentDefinition))?;
    }
    loop {
        let tag = ar.r.peek_u16()?;
        let known = ar.class_slot.get(&Class::ComponentDefinition).is_some_and(|&c| tag as u32 == 0x8000 | c);
        let declared = tag == 0xFFFF && ar.r.peek(26).get(6..26) == Some(b"CComponentDefinition".as_slice());
        if !known && !declared {
            break;
        }
        ar.read_object(None)?;
    }
    // 4. The model's own entities.
    let mut roots = ar.r.u32()?;
    if roots > 5_000_000
        && let Some(retry) = ar.retry_count_after_filler(ar.r.pos - 4, 5_000_000)
    {
        roots = retry;
    }
    if roots > 5_000_000 {
        return bad("implausible entity count");
    }
    let root = ar.read_entity_list(roots, true)?;
    Ok(Walked { ar, root, layers, materials })
}

//endregion

/*┌─────────────────╮
| Model           |
└─────────────────╯*/
//region Model

pub fn read(data: &[u8], _caps: &Caps) -> Result<Model, LoadError> {
    let walked = walk(data).map_err(|e| LoadError::invalid(format!("SketchUp file: {}", e.message())))?;
    let Walked { ar, root, layers, materials } = walked;
    let mut model = Model { container: Some(Container::Classic), ..Model::default() };

    // 1. Materials and tags by slot.
    let mut material_index: HashMap<u32, u32> = HashMap::new();
    for s in materials {
        let Some(Value::Material(m)) = ar.value(Some(s)) else { continue };
        let texture = m.texture.as_ref().and_then(|t| {
            let range = match (t.dib, t.inline) {
                (Some(d), _) => match ar.value(Some(d)) {
                    Some(&Value::Dib { start, len }) => Some((start, len)),
                    _ => None,
                },
                (None, inline) => inline,
            };
            let bytes = range.and_then(|(start, len)| data.get(start..start.checked_add(len)?))?;
            Some(TextureImage {
                bytes: bytes.to_vec(),
                width: t.width,
                height: t.height,
                file_name: t.file_name.clone(),
            })
        });
        // The stored double is a transparency, counting only with its flag.
        let opacity = if m.use_opacity { (1.0 - m.opacity).clamp(0.0, 1.0) } else { 1.0 };
        let alpha = if m.texture.is_some() { 1.0 } else { m.rgba[3] as f64 / 255.0 };
        material_index.insert(s, model.materials.len() as u32);
        model.materials.push(Material {
            name: m.name.clone(),
            color: [m.rgba[0], m.rgba[1], m.rgba[2]],
            opacity: (opacity * alpha) as f32,
            colorized: m.colorized && texture.is_some(),
            texture,
        });
    }
    let mut layer_index: HashMap<u32, u32> = HashMap::new();
    for s in layers {
        if let Some(Value::Layer { name, hidden }) = ar.value(Some(s)) {
            layer_index.insert(s, model.layers.len() as u32);
            model.layers.push(Layer { name: name.clone(), hidden: *hidden });
        }
    }

    // 2. Definitions by slot, then each one's entities.
    let mut def_slots: Vec<u32> = ar
        .slots
        .iter()
        .filter(|(_, s)| matches!(s, Slot::Obj(Class::ComponentDefinition, Value::Definition(_))))
        .map(|(&k, _)| k)
        .collect();
    def_slots.sort_unstable();
    let def_index: HashMap<u32, u32> = def_slots.iter().enumerate().map(|(i, &s)| (s, i as u32)).collect();
    let images: HashSet<u32> = ar
        .slots
        .values()
        .filter_map(|s| match s {
            Slot::Obj(_, Value::Image(i)) => i.def,
            _ => None,
        })
        .collect();
    let lookup = Lookup { ar: &ar, materials: &material_index, layers: &layer_index, defs: &def_index };
    let mut dropped = 0usize;
    for &s in &def_slots {
        let Some(Value::Definition(def)) = ar.value(Some(s)) else { continue };
        let DefinitionV { name, entities, faces_camera } = &**def;
        let mut d = Definition {
            name: name.clone(),
            image: images.contains(&s),
            faces_camera: *faces_camera,
            ..Default::default()
        };
        lookup.fill(&mut d, entities, &mut dropped);
        model.definitions.push(d);
    }
    lookup.fill(&mut model.root, &root, &mut dropped);
    if dropped > 0 {
        model.warnings.push(format!("{dropped} edges or faces refer to objects that couldn't be read"));
    }

    // 3. The saved view and the scenes.
    model.camera = first_camera(data);
    model.pages = scan_pages(&ar, &layer_index);
    Ok(model)
}

/// Turns slots into the model's indices.
struct Lookup<'x, 'a> {
    ar: &'x Archive<'a>,
    materials: &'x HashMap<u32, u32>,
    layers: &'x HashMap<u32, u32>,
    defs: &'x HashMap<u32, u32>,
}

impl Lookup<'_, '_> {
    fn material(&self, slot: u16) -> Option<u32> {
        (slot != 0).then(|| self.materials.get(&(slot as u32)).copied()).flatten()
    }

    fn layer(&self, slot: u32) -> Option<u32> {
        (slot != 0).then(|| self.layers.get(&slot).copied()).flatten()
    }

    fn fill(&self, d: &mut Definition, entities: &[u32], dropped: &mut usize) {
        let mut vertices: HashMap<u32, u32> = HashMap::new();
        let mut edges: HashMap<u32, u32> = HashMap::new();
        for &s in entities {
            match self.ar.value(Some(s)) {
                Some(Value::Edge { .. }) => {
                    if self.add_edge(d, s, &mut vertices, &mut edges).is_none() {
                        *dropped += 1;
                    }
                }
                Some(Value::Face(f)) => {
                    let FaceV { db, plane, loops, back, attrs } = &**f;
                    let mut out_loops = Vec::new();
                    for l in loops {
                        let Some(Value::Loop(uses)) = self.ar.value(*l) else { continue };
                        let mut coedges = Vec::new();
                        for u in uses {
                            let Some(&Value::EdgeUse { edge: Some(es), reversed }) = self.ar.value(*u) else {
                                continue;
                            };
                            match self.add_edge(d, es, &mut vertices, &mut edges) {
                                Some(edge) => coedges.push(CoEdge { edge, reversed }),
                                None => *dropped += 1,
                            }
                        }
                        if !coedges.is_empty() {
                            out_loops.push(coedges);
                        }
                    }
                    let mut mapping = [None, None];
                    if let Some(Value::Attrs(children)) = self.ar.value(*attrs) {
                        for c in children {
                            if let Some(Value::Ftc(f)) = self.ar.value(*c) {
                                mapping = [
                                    Some(Mapping { matrix: f.front, projection: f.front_axis }),
                                    Some(Mapping { matrix: f.back, projection: f.back_axis }),
                                ];
                            }
                        }
                    }
                    d.faces.push(Face {
                        loops: out_loops,
                        normal: DVec3::new(plane[0], plane[1], plane[2]),
                        front: self.material(db.material),
                        back: self.material(*back),
                        mapping,
                        hidden: db.hidden,
                        layer: self.layer(db.layer),
                    });
                }
                Some(Value::Instance(i)) => {
                    let InstanceV { db, def, xform, name } = &**i;
                    let Some(&definition) = def.and_then(|s| self.defs.get(&s)) else {
                        *dropped += 1;
                        continue;
                    };
                    d.instances.push(Instance {
                        definition,
                        matrix: *xform,
                        material: self.material(db.material),
                        layer: self.layer(db.layer),
                        hidden: db.hidden,
                        name: name.clone(),
                    });
                }
                Some(Value::Image(i)) => {
                    let ImageV { db, def, xform } = &**i;
                    let Some(&definition) = def.and_then(|s| self.defs.get(&s)) else { continue };
                    let mut matrix = [1.0; 13];
                    matrix[..12].copy_from_slice(xform);
                    d.instances.push(Instance {
                        definition,
                        matrix,
                        material: self.material(db.material),
                        layer: self.layer(db.layer),
                        hidden: db.hidden,
                        name: String::new(),
                    });
                }
                _ => {}
            }
        }
    }

    /// The edge at slot `s`, added once with its vertices; `None` when its
    /// vertices weren't read.
    fn add_edge(
        &self,
        d: &mut Definition,
        s: u32,
        vertices: &mut HashMap<u32, u32>,
        edges: &mut HashMap<u32, u32>,
    ) -> Option<u32> {
        if let Some(&e) = edges.get(&s) {
            return Some(e);
        }
        let Some(&Value::Edge { a: Some(a), b: Some(b), db }) = self.ar.value(Some(s)) else { return None };
        let mut vertex = |vs: u32| -> Option<u32> {
            if let Some(&v) = vertices.get(&vs) {
                return Some(v);
            }
            let Some(&Value::Vertex(p)) = self.ar.value(Some(vs)) else { return None };
            let i = d.vertices.len() as u32;
            d.vertices.push(p);
            vertices.insert(vs, i);
            Some(i)
        };
        let (a, b) = (vertex(a)?, vertex(b)?);
        let i = d.edges.len() as u32;
        d.edges.push(Edge { a, b, hidden: db.hidden, soft: db.soft, smooth: db.smooth, layer: self.layer(db.layer) });
        edges.insert(s, i);
        Some(i)
    }
}

/// The model's saved view: the first camera in the file, which belongs to
/// the model's own thumbnail before the walk's start.  After the class
/// name: eye, target and up (inches), two doubles, a perspective flag, the
/// field of view and the parallel projection's height.
fn first_camera(data: &[u8]) -> Option<Camera> {
    let at = find(data, b"\x07\x00CCamera", 0, data.len())?;
    if at < 4 || data[at - 4..at - 2] != [0xFF, 0xFF] {
        return None;
    }
    let mut r = Cursor { data, pos: at + 9 };
    let v = r.f64s::<9>().ok()?;
    r.f64s::<2>().ok()?;
    let perspective = r.u8().ok()? != 0;
    let fov = r.f64().ok()?;
    let height = r.f64().ok()?;
    let cam = Camera {
        eye: DVec3::new(v[0], v[1], v[2]),
        target: DVec3::new(v[3], v[4], v[5]),
        up: DVec3::new(v[6], v[7], v[8]),
        fov: if fov.is_finite() && fov > 0.0 && fov < 180.0 { fov } else { 35.0 },
        perspective,
        height,
    };
    (cam.eye.is_finite() && cam.target.is_finite() && cam.eye != cam.target).then_some(cam)
}

/// Scenes, found by shape in the rest of the file: a name, an empty second
/// string and a flags word this reader understands (nothing captured, or
/// only hidden tags), then the camera and picture references and the
/// hidden tags.  Scenes it can't read fully are left out.
fn scan_pages(ar: &Archive, layer_index: &HashMap<u32, u32>) -> Vec<Page> {
    let data = ar.data();
    let mut pages = Vec::new();
    let mut pos = ar.r.pos;
    while let Some(idx) = find(data, STR_MARKER, pos, data.len()) {
        pos = idx + 1;
        if pages.len() >= 1000 {
            break;
        }
        let Some(&len) = data.get(idx + 3) else { break };
        let len = len as usize;
        if len == 0 || len > 40 {
            continue;
        }
        let name_end = idx + 4 + 2 * len;
        if data.get(name_end..name_end + 4) != Some(b"\xff\xfe\xff\x00".as_slice()) {
            continue;
        }
        let Some(flags) = u32_at(data, name_end + 4) else { continue };
        if flags != 0xC00 && flags != 0xC20 {
            continue;
        }
        let units: Vec<u16> =
            data[idx + 4..name_end].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let mut r = Cursor { data, pos: name_end + 8 };
        let hidden = (|| -> R<Vec<u32>> {
            skip_typed_ref(ar, &mut r, true)?;
            skip_typed_ref(ar, &mut r, false)?;
            let mut hidden = Vec::new();
            if flags & 0x20 != 0 {
                loop {
                    let save = r.pos;
                    let v = r.u16()? as u32;
                    if matches!(ar.slots.get(&v), Some(Slot::Obj(Class::Layer, _))) {
                        hidden.push(v);
                    } else {
                        r.pos = save;
                        break;
                    }
                }
                if r.u32()? != 1 {
                    return bad("unexpected hidden tags marker");
                }
            }
            Ok(hidden)
        })();
        if let Ok(hidden) = hidden {
            let hidden_layers = hidden.iter().filter_map(|s| layer_index.get(s).copied()).collect();
            pages.push(Page { name: String::from_utf16_lossy(&units), camera: None, hidden_layers });
        }
    }
    pages
}

/// Skips a scene's camera (`camera`) or picture reference: null, a back
/// reference, or the object itself.
fn skip_typed_ref(ar: &Archive, r: &mut Cursor, camera: bool) -> R<()> {
    let tag = r.u16()?;
    if tag == 0 || tag & 0x8000 == 0 {
        return Ok(());
    }
    if tag == 0xFFFF {
        r.u16()?;
        let len = r.u16()? as usize;
        let name = r.raw(len)?;
        if name != if camera { b"CCamera".as_slice() } else { b"CDib".as_slice() } {
            return bad("unexpected class in a scene");
        }
    } else if let Some(Slot::Class(c)) = ar.slots.get(&((tag & 0x7FFF) as u32))
        && *c != if camera { Class::Camera } else { Class::Dib }
    {
        return bad("unexpected class in a scene");
    }
    if camera {
        r.skip(137)?;
        r.u16()?;
        r.utf16()?;
        r.skip(33)
    } else {
        r.u32()?;
        let len = r.u32()? as usize;
        r.skip(len)
    }
}

pub fn version_text(data: &[u8]) -> Option<String> {
    crate::header_version(data)
}

//endregion
