//! Reading structures in place: an [`Obj`] is one structure inside a block,
//! read field by field through the SDNA.  Every read is bounds-checked
//! against its block, and a missing field reads as absent rather than
//! failing, because fields come and go between Blender versions.

use crate::file::BlendFile;
use crate::sdna::{Field, Prim};

/// One structure inside a block.
#[derive(Clone, Copy)]
pub struct Obj<'a> {
    pub f: &'a BlendFile,
    /// Index of its structure in the SDNA.
    pub s: u32,
    /// Where it starts in the file's data.
    pub at: usize,
    /// Where its block ends, which no read may pass.
    pub end: usize,
    /// The block it lives in.
    pub block: u32,
}

impl<'a> Obj<'a> {
    /// The first structure in block `i`, as the block says it is.
    pub fn of_block(f: &'a BlendFile, i: u32) -> Option<Obj<'a>> {
        let b = f.block(i);
        f.sdna.get(b.sdna)?;
        Some(Obj { f, s: b.sdna, at: b.start, end: b.start + b.len, block: i })
    }

    /// The `n`th structure of the same kind in this block, for arrays.
    pub fn nth(&self, n: usize) -> Option<Obj<'a>> {
        let size = self.size();
        let at = self.at.checked_add(size.checked_mul(n)?)?;
        (at.checked_add(size)? <= self.end).then_some(Obj { at, ..*self })
    }

    /// How many structures of this kind fit from here to the block's end.
    pub fn fits(&self) -> usize {
        let size = self.size();
        if size == 0 { 0 } else { (self.end - self.at) / size }
    }

    pub fn size(&self) -> usize {
        self.f.sdna.get(self.s).map_or(0, |s| s.size as usize)
    }

    pub fn type_name(&self) -> &'a str {
        self.f.sdna.struct_name(self.s)
    }

    pub fn field(&self, name: &str) -> Option<&'a Field> {
        self.locate(name).map(|(f, _)| f)
    }

    /// Field `name` and where it sits from the structure's start.  Blender
    /// builds some structures on others by embedding them first (each
    /// modifier starts with a `ModifierData`, each data-block with an
    /// `ID`), so a field missing at the top is looked for in the first
    /// embedded structure, as C code reaches it through a cast.
    fn locate(&self, name: &str) -> Option<(&'a Field, usize)> {
        let mut s = self.s;
        let mut base = 0usize;
        for _ in 0..4 {
            let st = self.f.sdna.get(s)?;
            if let Some(f) = st.field(name) {
                return Some((f, base + f.offset as usize));
            }
            let first = st.fields.first().filter(|f| !f.pointer && f.offset == 0)?;
            s = self.f.sdna.struct_of_type(first.ty)?;
            base += first.offset as usize;
        }
        None
    }

    pub fn has(&self, name: &str) -> bool {
        self.locate(name).is_some()
    }

    /// The bytes of a located field, if they lie inside the block.
    fn bytes(&self, (f, offset): (&Field, usize)) -> Option<&'a [u8]> {
        let start = self.at.checked_add(offset)?;
        let end = start.checked_add(f.size as usize)?;
        (end <= self.end).then(|| &self.f.data[start..end])
    }

    /// Element `i` of a located numeric field, converted to f64.
    fn number(&self, at: (&Field, usize), i: usize) -> Option<f64> {
        let f = at.0;
        if f.pointer || i >= f.elems as usize {
            return None;
        }
        let prim = self.f.sdna.types.get(f.ty as usize)?.prim;
        let size = prim.size();
        let b = self.bytes(at)?.get(i * size..(i + 1) * size)?;
        let rd = self.f.endian();
        Some(match prim {
            Prim::Char => b[0] as i8 as f64,
            Prim::UChar => b[0] as f64,
            Prim::Short => rd.u16(b) as i16 as f64,
            Prim::UShort => rd.u16(b) as f64,
            Prim::Int => rd.i32(b) as f64,
            Prim::UInt => rd.u32(b) as f64,
            Prim::Int64 => rd.i64(b) as f64,
            Prim::UInt64 => rd.u64(b) as f64,
            Prim::Float => rd.f32(b) as f64,
            Prim::Double => rd.f64(b),
            Prim::Other => return None,
        })
    }

    /// An integer field (of any integer or float type), trying each name in
    /// turn, since fields are renamed between versions.
    pub fn int(&self, names: &[&str]) -> Option<i64> {
        names.iter().find_map(|n| self.locate(n).and_then(|f| self.number(f, 0))).map(|v| v as i64)
    }

    pub fn float(&self, names: &[&str]) -> Option<f32> {
        names.iter().find_map(|n| self.locate(n).and_then(|f| self.number(f, 0))).map(|v| v as f32)
    }

    /// Element `i` of an array field.
    pub fn float_at(&self, name: &str, i: usize) -> Option<f32> {
        self.locate(name).and_then(|f| self.number(f, i)).map(|v| v as f32)
    }

    /// The first `N` elements of an array field such as `loc[3]` or
    /// `mat[4][4]`.
    pub fn floats<const N: usize>(&self, names: &[&str]) -> Option<[f32; N]> {
        let f = names.iter().find_map(|n| self.locate(n))?;
        if (f.0.elems as usize) < N {
            return None;
        }
        let mut out = [0.0; N];
        for (i, v) in out.iter_mut().enumerate() {
            *v = self.number(f, i)? as f32;
        }
        Some(out)
    }

    /// A char array as text, up to its first zero byte.
    pub fn string(&self, name: &str) -> String {
        let Some(b) = self.locate(name).filter(|f| !f.0.pointer).and_then(|f| self.bytes(f)) else {
            return String::new();
        };
        let len = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        String::from_utf8_lossy(&b[..len]).into_owned()
    }

    /// An ID's name without its two-letter type prefix ("OBCube" is "Cube").
    pub fn id_name(&self) -> String {
        let id = self.sub("id");
        let full = id.map_or_else(|| self.string("name"), |id| id.string("name"));
        full.get(2..).unwrap_or("").to_string()
    }

    /// The raw old address in pointer field `name` (element `i`).
    pub fn ptr_at(&self, name: &str, i: usize) -> u64 {
        let Some((f, offset)) = self.locate(name).filter(|f| f.0.pointer && i < f.0.elems as usize) else {
            return 0;
        };
        let _ = f;
        let n = self.f.header.pointer_size as usize;
        let pos = self.at + offset + i * n;
        if pos + n > self.end {
            return 0;
        }
        self.f.read_ptr(pos)
    }

    pub fn ptr(&self, name: &str) -> u64 {
        self.ptr_at(name, 0)
    }

    /// Follows pointer field `name` to the structure it points at.
    pub fn deref(&self, name: &str) -> Option<Obj<'a>> {
        let f = self.field(name)?;
        let want = self.f.sdna.struct_of_type(f.ty);
        self.f.resolve_near(self.ptr(name), want, self.block)
    }

    /// Follows pointer field `name` to a structure of the named type.
    pub fn deref_as(&self, name: &str, struct_name: &str) -> Option<Obj<'a>> {
        self.f.resolve_near(self.ptr(name), self.f.sdna.struct_named(struct_name), self.block)
    }

    /// Follows the address `old`, read from this structure, as a structure
    /// of the named type (or the block's own type).
    pub fn follow(&self, old: u64, struct_name: Option<&str>) -> Option<Obj<'a>> {
        self.f.resolve_near(old, struct_name.and_then(|n| self.f.sdna.struct_named(n)), self.block)
    }

    /// The bytes pointer field `name` points at, to the end of their block.
    pub fn raw(&self, name: &str) -> Option<&'a [u8]> {
        let (i, offset) = self.f.find_near(self.ptr(name), self.block)?;
        self.f.block_bytes(i).get(offset..)
    }

    /// The block pointer field `name` points at, and the offset in it.
    pub fn target(&self, name: &str) -> Option<(u32, usize)> {
        self.f.find_near(self.ptr(name), self.block)
    }

    /// An embedded structure field, such as an object's `id` or a mesh's
    /// `vdata`.
    pub fn sub(&self, name: &str) -> Option<Obj<'a>> {
        let (f, offset) = self.locate(name).filter(|f| !f.0.pointer)?;
        let s = self.f.sdna.struct_of_type(f.ty)?;
        let at = self.at.checked_add(offset)?;
        let size = self.f.sdna.get(s)?.size as usize;
        (at.checked_add(size)? <= self.end).then_some(Obj { s, at, ..*self })
    }

    /// The structures of a `ListBase` field, following each one's `next`
    /// pointer.  Stops at a loop or after as many steps as the file has
    /// blocks.
    pub fn list(&self, name: &str) -> Vec<Obj<'a>> {
        let Some(base) = self.sub(name) else { return Vec::new() };
        let mut out = Vec::new();
        let mut next = base.ptr("first");
        let mut seen = std::collections::HashSet::new();
        while next != 0 && seen.insert(next) && out.len() <= self.f.blocks.len() {
            let Some(o) = self.f.resolve_near(next, None, out.last().map_or(self.block, |o: &Obj| o.block)) else {
                break;
            };
            next = o.ptr("next");
            out.push(o);
        }
        out
    }

    /// A pointer to an array of `n` pointers, such as a mesh's `**mat`:
    /// the old addresses in it.
    pub fn ptr_array(&self, name: &str, n: usize) -> Vec<u64> {
        let Some((i, offset)) = self.f.find_near(self.ptr(name), self.block) else { return Vec::new() };
        let b = self.f.block(i);
        let size = self.f.header.pointer_size as usize;
        let n = n.min(b.len.saturating_sub(offset) / size);
        (0..n).map(|k| self.f.read_ptr(b.start + offset + k * size)).collect()
    }
}

impl BlendFile {
    /// The structure at old address `old`.  Its type is the block's, unless
    /// the block is untyped raw data (structure 0) and the pointer's own type
    /// says what it is.
    pub fn resolve(&self, old: u64, want: Option<u32>) -> Option<Obj<'_>> {
        let (i, offset) = self.find(old)?;
        self.obj_at(i, offset, want)
    }

    /// Like [`BlendFile::resolve`], for an address read from block `from`.
    pub fn resolve_near(&self, old: u64, want: Option<u32>, from: u32) -> Option<Obj<'_>> {
        let (i, offset) = self.find_near(old, from)?;
        self.obj_at(i, offset, want)
    }

    fn obj_at(&self, i: u32, offset: usize, want: Option<u32>) -> Option<Obj<'_>> {
        let b = self.block(i);
        let s = match want {
            Some(w) if b.sdna == 0 || b.sdna == w => w,
            _ => b.sdna,
        };
        self.sdna.get(s)?;
        Some(Obj { f: self, s, at: b.start + offset, end: b.start + b.len, block: i })
    }
}
