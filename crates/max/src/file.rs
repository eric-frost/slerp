//! A 3ds Max file's scene as objects: what each object is (its class, from
//! the class directory) and what it refers to.
//!
//! The `Scene` stream is one container whose children are the scene's
//! objects in order; an object's position is its number, which references
//! use.  A child's chunk id is an index into the `ClassDirectory3` stream's
//! list of classes (each a DLL index, a two-word class id, a superclass id
//! and a name); ids from 0x2000 up are built-in kinds such as derived
//! objects (a base object with its modifier stack).  An object's own chunks
//! hold its data and its references: chunk 0x2034 lists the referenced
//! objects in slot order, chunk 0x2035 gives a slot count and then slot and
//! object pairs.
//!
//! Parameters live in parameter blocks: the old `ParamBlock` (one container
//! per parameter: its index, then its value or, when animated, a reference
//! to a controller in the slot of the same number) and `ParamBlock2` (one
//! chunk per parameter: id, type, flags, and the value at the end; its
//! references count only the parameters that are references, such as maps,
//! in the order the block lists them).

use crate::chunks::{Body, Chunk, f32_at, floats, i32_at, u16_at, u32_at, utf16};
use std::collections::HashMap;

/// Chunk ids of built-in object kinds that aren't in the class directory.
pub const DERIVED_OBJECT: u16 = 0x2032;

/// A class from the class directory.
#[derive(Clone, Debug, Default)]
pub struct Class {
    pub dll: i32,
    pub id: (u32, u32),
    pub super_id: u32,
    pub name: String,
}

/// Superclass ids.
pub mod super_class {
    pub const GEOMETRY: u32 = 0x10;
    pub const MATERIAL: u32 = 0xC00;
    pub const TEXMAP: u32 = 0xC10;
    pub const MODIFIER: u32 = 0x810;
    pub const FLOAT_CONTROL: u32 = 0x9003;
    pub const POSITION_CONTROL: u32 = 0x900B;
    pub const ROTATION_CONTROL: u32 = 0x900C;
    pub const SCALE_CONTROL: u32 = 0x900D;
    pub const MATRIX3_CONTROL: u32 = 0x9008;
}

pub struct MaxFile<'a> {
    pub classes: Vec<Class>,
    pub objects: Vec<Chunk<'a>>,
    /// Asset ids and the file paths they stand for, from
    /// `FileAssetMetaData2`/`3`.
    pub assets: HashMap<[u8; 16], String>,
}

impl<'a> MaxFile<'a> {
    pub fn new(classes: Vec<Class>, scene: Vec<Chunk<'a>>, assets: HashMap<[u8; 16], String>) -> Self {
        // The stream is one container holding the objects.
        let objects = scene
            .into_iter()
            .find_map(|c| match c.body {
                Body::Children(kids) => Some(kids),
                Body::Data(_) => None,
            })
            .unwrap_or_default();
        MaxFile { classes, objects, assets }
    }

    pub fn class(&self, i: usize) -> Option<&Class> {
        self.classes.get(self.objects.get(i)?.id as usize)
    }

    pub fn class_id(&self, i: usize) -> Option<(u32, u32)> {
        self.class(i).map(|c| c.id)
    }

    pub fn super_id(&self, i: usize) -> Option<u32> {
        self.class(i).map(|c| c.super_id)
    }

    pub fn name_of(&self, i: usize) -> &str {
        match self.objects.get(i) {
            Some(c) if c.id == DERIVED_OBJECT => "Derived Object",
            _ => self.class(i).map_or("?", |c| c.name.as_str()),
        }
    }

    pub fn is_derived(&self, i: usize) -> bool {
        self.objects.get(i).is_some_and(|c| c.id == DERIVED_OBJECT)
    }

    /// The objects object `i` refers to, by slot.
    pub fn refs(&self, i: usize) -> Vec<Option<usize>> {
        let Some(c) = self.objects.get(i) else { return Vec::new() };
        let valid = |v: i32| (v >= 0 && (v as usize) < self.objects.len()).then_some(v as usize);
        if let Some(d) = c.child_data(0x2034) {
            return d
                .chunks_exact(4)
                .take(1 << 20)
                .map(|b| valid(i32::from_le_bytes([b[0], b[1], b[2], b[3]])))
                .collect();
        }
        if let Some(d) = c.child_data(0x2035) {
            let ints: Vec<i32> = d.chunks_exact(4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
            let mut out: Vec<Option<usize>> = Vec::new();
            for pair in ints.get(1..).unwrap_or(&[]).chunks_exact(2) {
                let slot = pair[0];
                if !(0..1 << 16).contains(&slot) {
                    continue;
                }
                if out.len() <= slot as usize {
                    out.resize(slot as usize + 1, None);
                }
                out[slot as usize] = valid(pair[1]);
            }
            return out;
        }
        Vec::new()
    }

    pub fn reference(&self, i: usize, slot: usize) -> Option<usize> {
        self.refs(i).get(slot).copied().flatten()
    }

    /// An object's name from its chunk 0x4000/0x4001 (materials and maps)
    /// or 0x0962 (nodes).
    pub fn label(&self, i: usize) -> Option<String> {
        let c = self.objects.get(i)?;
        if let Some(d) = c.child_data(0x0962) {
            return Some(utf16(d));
        }
        let group = c.child(0x4000).or_else(|| c.child(0x0100))?;
        group.child_data(0x4001).map(utf16)
    }
}

/*┌─────────────────╮
| Parameters      |
└─────────────────╯*/
//region Parameters

/// ParamBlock2 parameter types.
pub mod ptype {
    pub const FLOAT: u16 = 0;
    pub const INT: u16 = 1;
    pub const RGBA: u16 = 2;
    pub const POINT3: u16 = 3;
    pub const BOOL: u16 = 4;
    pub const ANGLE: u16 = 5;
    pub const PCNT_FRAC: u16 = 6;
    pub const WORLD: u16 = 7;
    pub const MTL: u16 = 14;
    pub const TEXMAP: u16 = 15;
    pub const BITMAP: u16 = 16;
    pub const INODE: u16 = 17;
    pub const REFTARG: u16 = 18;
    pub const PBLOCK2: u16 = 21;
    pub const FRGBA: u16 = 23;
    pub const TAB: u16 = 0x800;
}

/// One ParamBlock2 parameter.
pub struct Param<'c, 'a> {
    pub id: u16,
    pub ty: u16,
    pub data: &'a [u8],
    pub chunk: &'c Chunk<'a>,
}

impl Param<'_, '_> {
    fn base(&self) -> u16 {
        self.ty & !ptype::TAB
    }

    pub fn is_reference(&self) -> bool {
        matches!(self.base(), ptype::MTL | ptype::TEXMAP | ptype::INODE | ptype::REFTARG | ptype::PBLOCK2)
    }

    /// A number: the last four bytes.
    pub fn float(&self) -> Option<f32> {
        match self.base() {
            ptype::INT | ptype::BOOL => i32_at(self.data, self.data.len().checked_sub(4)?).map(|v| v as f32),
            _ => f32_at(self.data, self.data.len().checked_sub(4)?).filter(|v| v.is_finite()),
        }
    }

    pub fn int(&self) -> Option<i32> {
        match self.base() {
            ptype::FLOAT | ptype::ANGLE | ptype::PCNT_FRAC | ptype::WORLD => self.float().map(|v| v as i32),
            _ => i32_at(self.data, self.data.len().checked_sub(4)?),
        }
    }

    /// A colour: the last twelve or sixteen bytes.
    pub fn color(&self) -> Option<[f32; 3]> {
        let n = if self.base() == ptype::FRGBA { 16 } else { 12 };
        let v = floats(self.data, self.data.len().checked_sub(n)?, 3)?;
        v.iter().all(|c| c.is_finite()).then(|| [v[0], v[1], v[2]])
    }

    /// The integers of a table parameter, each stored with a flag byte.
    pub fn table_ints(&self) -> Vec<i32> {
        // The table's count sits before its elements; find the layout that
        // fits the data exactly.
        for start in (4..self.data.len().min(32)).rev() {
            let Some(n) = u32_at(self.data, start) else { continue };
            let n = n as usize;
            if n == 0 || n > 1 << 16 {
                continue;
            }
            let body = start + 4;
            if body + n * 5 == self.data.len() {
                return (0..n).filter_map(|k| i32_at(self.data, body + k * 5 + 1)).collect();
            }
        }
        Vec::new()
    }
}

/// The parameters of a ParamBlock2 object, in its order.
pub fn pb2_params<'c, 'a>(file: &'c MaxFile<'a>, pb: usize) -> Vec<Param<'c, 'a>> {
    let Some(c) = file.objects.get(pb) else { return Vec::new() };
    c.children()
        .iter()
        .filter(|k| k.id == 0x100E || k.id == 0x000A)
        .filter_map(|k| {
            let d = k.data()?;
            Some(Param { id: u16_at(d, 0)?, ty: u16_at(d, 2)?, data: d, chunk: k })
        })
        .collect()
}

/// A ParamBlock2's block id (which of an object's blocks it is).
pub fn pb2_block(file: &MaxFile, pb: usize) -> Option<u16> {
    u16_at(file.objects.get(pb)?.child_data(0x0009)?, 4)
}

/// A float parameter of a parameter block of either kind, by id (for old
/// blocks, the index).
pub fn param_float(file: &MaxFile, pb: usize, id: u16) -> Option<f32> {
    match file.name_of(pb) {
        "ParamBlock" => old_param(file, pb, id).and_then(|v| v.first().copied()),
        _ => pb2_params(file, pb).iter().find(|p| p.id == id)?.float(),
    }
}

pub fn param_color(file: &MaxFile, pb: usize, id: u16) -> Option<[f32; 3]> {
    match file.name_of(pb) {
        "ParamBlock" => old_param(file, pb, id).filter(|v| v.len() >= 3).map(|v| [v[0], v[1], v[2]]),
        _ => pb2_params(file, pb).iter().find(|p| p.id == id)?.color(),
    }
}

/// The object a ParamBlock2's reference parameter `id` points at.
pub fn param_ref(file: &MaxFile, pb: usize, id: u16) -> Option<usize> {
    let params = pb2_params(file, pb);
    let slot = params.iter().filter(|p| p.is_reference() && p.ty & ptype::TAB == 0).position(|p| p.id == id)?;
    file.reference(pb, slot)
}

/// An old ParamBlock's value at `index`: one number, or three for colours
/// and points; an animated value is read from its controller.
fn old_param(file: &MaxFile, pb: usize, index: u16) -> Option<Vec<f32>> {
    let c = file.objects.get(pb)?;
    for p in c.children().iter().filter(|k| k.id == 0x0002) {
        if p.child_data(0x0003).and_then(|d| u32_at(d, 0)) != Some(index as u32) {
            continue;
        }
        for v in p.children() {
            let Some(d) = v.data() else { continue };
            match v.id {
                0x100 => return f32_at(d, 0).filter(|x| x.is_finite()).map(|x| vec![x]),
                0x101 | 0x104 => return i32_at(d, 0).map(|x| vec![x as f32]),
                0x102 | 0x103 => return floats(d, 0, 3),
                0x200..=0x2FF => {
                    let ctrl = file.reference(pb, index as usize)?;
                    return controller_values(file, ctrl);
                }
                _ => {}
            }
        }
    }
    None
}

/// The value a controller holds now: a float, or three or four of them.
pub fn controller_values(file: &MaxFile, ctrl: usize) -> Option<Vec<f32>> {
    let c = file.objects.get(ctrl)?;
    for id in [0x2501u16, 0x2503, 0x2504, 0x2505] {
        if let Some(d) = c.find(id).and_then(Chunk::data) {
            let n = (d.len() / 4).min(7);
            let v = floats(d, 0, n)?;
            if !v.is_empty() && v.iter().all(|x| x.is_finite()) {
                return Some(v);
            }
        }
    }
    None
}

//endregion
