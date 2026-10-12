//! The structure description (SDNA) every `.blend` file carries in its
//! `DNA1` block: the name, size and fields of every structure Blender wrote.
//! Reading through it, rather than through layouts compiled in, is what lets
//! one reader open files from Blender 2.4 to 5.x: a field is found by name
//! wherever that version put it, and a field the version didn't have is
//! simply absent.
//!
//! The block holds four tables: `NAME` (field names with their pointer stars
//! and array sizes, such as `*next` or `mat[4][4]`), `TYPE` (type names),
//! `TLEN` (each type's size in bytes) and `STRC` (each structure's type and
//! its fields as type and name pairs).  Structures have no hidden padding,
//! so a field's offset is the sum of the sizes before it.

use crate::file::Endian;
use slerp_scene::LoadError;
use std::collections::HashMap;

/// A primitive type, or a structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prim {
    Char,
    UChar,
    Short,
    UShort,
    Int,
    UInt,
    Int64,
    UInt64,
    Float,
    Double,
    /// A structure, or a type Slerp doesn't read directly (void).
    Other,
}

impl Prim {
    fn of(name: &str) -> Prim {
        match name {
            "char" | "int8_t" | "bool" => Prim::Char,
            "uchar" | "uint8_t" => Prim::UChar,
            "short" | "int16_t" => Prim::Short,
            "ushort" | "uint16_t" => Prim::UShort,
            "int" | "long" | "int32_t" => Prim::Int,
            "uint" | "ulong" | "uint32_t" => Prim::UInt,
            "int64_t" => Prim::Int64,
            "uint64_t" => Prim::UInt64,
            "float" => Prim::Float,
            "double" => Prim::Double,
            _ => Prim::Other,
        }
    }

    pub fn size(self) -> usize {
        match self {
            Prim::Char | Prim::UChar => 1,
            Prim::Short | Prim::UShort => 2,
            Prim::Int | Prim::UInt | Prim::Float => 4,
            Prim::Int64 | Prim::UInt64 | Prim::Double => 8,
            Prim::Other => 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TypeInfo {
    pub name: Box<str>,
    pub size: u32,
    pub prim: Prim,
}

/// One field of a structure.
#[derive(Clone, Debug)]
pub struct Field {
    /// The bare name, without stars or array sizes: `next`, `mat`.
    pub name: Box<str>,
    /// Index into [`Sdna::types`].
    pub ty: u32,
    pub offset: u32,
    /// Bytes in all, arrays included.
    pub size: u32,
    pub pointer: bool,
    /// Elements in the array, 1 for a plain field.
    pub elems: u32,
}

#[derive(Clone, Debug)]
pub struct StructInfo {
    /// Index into [`Sdna::types`].
    pub ty: u32,
    pub size: u32,
    pub fields: Vec<Field>,
    by_name: HashMap<Box<str>, u32>,
}

impl StructInfo {
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.by_name.get(name).map(|&i| &self.fields[i as usize])
    }
}

pub struct Sdna {
    pub types: Vec<TypeInfo>,
    pub structs: Vec<StructInfo>,
    /// The structure of each type, or `u32::MAX`.
    struct_of_type: Vec<u32>,
    by_name: HashMap<Box<str>, u32>,
}

impl Sdna {
    pub fn parse(b: &[u8], rd: Endian, pointer_size: u8) -> Result<Sdna, LoadError> {
        let bad = || LoadError::invalid("damaged structure description (DNA1)");
        let mut pos = 0usize;
        let tag = |pos: &mut usize, want: &[u8; 4]| -> Result<(), LoadError> {
            *pos = (*pos + 3) & !3;
            if b.get(*pos..*pos + 4) != Some(&want[..]) {
                return Err(bad());
            }
            *pos += 4;
            Ok(())
        };
        let int = |pos: &mut usize| -> Result<usize, LoadError> {
            let v = b.get(*pos..*pos + 4).map(|s| rd.u32(s)).ok_or_else(bad)?;
            *pos += 4;
            // Each entry takes at least two bytes, which bounds every count
            // by the block's size.
            usize::try_from(v).ok().filter(|&n| n <= b.len() / 2).ok_or_else(bad)
        };
        let strings = |pos: &mut usize, n: usize| -> Result<Vec<Box<str>>, LoadError> {
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                let rest = b.get(*pos..).ok_or_else(bad)?;
                let len = rest.iter().position(|&c| c == 0).ok_or_else(bad)?;
                out.push(String::from_utf8_lossy(&rest[..len]).into());
                *pos += len + 1;
            }
            Ok(out)
        };
        // 1. The four tables.
        if b.get(..4) != Some(b"SDNA") {
            return Err(bad());
        }
        pos += 4;
        tag(&mut pos, b"NAME")?;
        let n = int(&mut pos)?;
        let names = strings(&mut pos, n)?;
        tag(&mut pos, b"TYPE")?;
        let n = int(&mut pos)?;
        let type_names = strings(&mut pos, n)?;
        tag(&mut pos, b"TLEN")?;
        let mut types = Vec::with_capacity(type_names.len());
        for name in type_names {
            let size = b.get(pos..pos + 2).map(|s| rd.u16(s)).ok_or_else(bad)? as u32;
            pos += 2;
            let prim = Prim::of(&name);
            types.push(TypeInfo { name, size, prim });
        }
        tag(&mut pos, b"STRC")?;
        let n = int(&mut pos)?;
        // 2. Structures, with offsets summed from field sizes.
        let parsed_names: Vec<(Box<str>, bool, u32)> = names.iter().map(|n| parse_name(n)).collect();
        let mut structs = Vec::with_capacity(n);
        let mut struct_of_type = vec![u32::MAX; types.len()];
        let mut by_name = HashMap::with_capacity(n);
        for si in 0..n {
            let mut short = || -> Result<usize, LoadError> {
                let v = b.get(pos..pos + 2).map(|s| rd.u16(s)).ok_or_else(bad)?;
                pos += 2;
                Ok(v as usize)
            };
            let ty = short()?;
            let nfields = short()?;
            if ty >= types.len() {
                return Err(bad());
            }
            let mut fields = Vec::with_capacity(nfields);
            let mut field_names = HashMap::with_capacity(nfields);
            let mut offset = 0u32;
            for _ in 0..nfields {
                let fty = short()?;
                let fname = short()?;
                let (Some(t), Some((name, pointer, elems))) = (types.get(fty), parsed_names.get(fname)) else {
                    return Err(bad());
                };
                let unit = if *pointer { pointer_size as u32 } else { t.size };
                let size = unit.saturating_mul(*elems);
                field_names.entry(name.clone()).or_insert(fields.len() as u32);
                fields.push(Field {
                    name: name.clone(),
                    ty: fty as u32,
                    offset,
                    size,
                    pointer: *pointer,
                    elems: *elems,
                });
                offset = offset.saturating_add(size);
            }
            struct_of_type[ty] = si as u32;
            by_name.entry(types[ty].name.clone()).or_insert(si as u32);
            structs.push(StructInfo { ty: ty as u32, size: types[ty].size, fields, by_name: field_names });
        }
        Ok(Sdna { types, structs, struct_of_type, by_name })
    }

    /// The structure called `name`, such as "Mesh".
    pub fn struct_named(&self, name: &str) -> Option<u32> {
        self.by_name.get(name).copied()
    }

    /// The structure a field's type is, when it is one.
    pub fn struct_of_type(&self, ty: u32) -> Option<u32> {
        self.struct_of_type.get(ty as usize).copied().filter(|&s| s != u32::MAX)
    }

    pub fn struct_name(&self, s: u32) -> &str {
        self.structs.get(s as usize).map_or("", |st| &self.types[st.ty as usize].name)
    }

    pub fn get(&self, s: u32) -> Option<&StructInfo> {
        self.structs.get(s as usize)
    }
}

/// Splits a field name such as `*next`, `mat[4][4]` or `(*func)()` into its
/// bare name, whether it is a pointer, and its array element count.
fn parse_name(raw: &str) -> (Box<str>, bool, u32) {
    let pointer = raw.starts_with('*') || raw.starts_with("(*");
    let bare: String =
        raw.trim_start_matches(['(', '*']).chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
    let mut elems = 1u32;
    // Function pointers have "()" but no array sizes.
    if !raw.starts_with("(*") {
        for part in raw.split('[').skip(1) {
            let n: u32 = part.split(']').next().and_then(|d| d.trim().parse().ok()).unwrap_or(1);
            elems = elems.saturating_mul(n.max(1));
        }
    }
    (bare.into(), pointer, elems)
}

#[cfg(test)]
mod tests {
    use super::parse_name;

    #[test]
    fn names_split_into_name_pointer_and_count() {
        assert_eq!(parse_name("*next"), ("next".into(), true, 1));
        assert_eq!(parse_name("mat[4][4]"), ("mat".into(), false, 16));
        assert_eq!(parse_name("name[66]"), ("name".into(), false, 66));
        assert_eq!(parse_name("(*func)()"), ("func".into(), true, 1));
        assert_eq!(parse_name("**mat"), ("mat".into(), true, 1));
        assert_eq!(parse_name("*data[2]"), ("data".into(), true, 2));
    }
}
