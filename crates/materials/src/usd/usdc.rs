//! USD's binary "crate" format (`PXR-USDC`), versions 0.4.0 and newer.
//!
//! A crate file ends in a table of contents naming six sections: TOKENS (a
//! compressed list of strings), STRINGS (string indices into the tokens),
//! FIELDS (a name token and a packed value for each field), FIELDSETS (runs
//! of field indices), PATHS (a compressed path tree) and SPECS (a path, a
//! field set and a kind for each spec).  Packed values ("value reps") either
//! hold small values inline or point to their data elsewhere in the file.
//! Integer tables are delta-coded with two-bit size codes and then LZ4
//! compressed; large float and int arrays may be compressed the same way.
//!
//! Only the fields Slerp uses are decoded.  Every count and offset read
//! from the file is checked against the file and the allocation budget.

use super::usda::child_path;
use super::{Layer, Property, Reference, Samples, Specifier, TimeSampled, Value};
use slerp_scene::LoadError;
use std::collections::HashMap;
use std::sync::Arc;

fn err(msg: impl Into<String>) -> LoadError {
    LoadError::invalid(format!("USD crate: {}", msg.into()))
}

/*┌─────────────────╮
| Reading         |
└─────────────────╯*/
//region Reading

#[derive(Clone)]
struct R<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> R<'a> {
    fn at(d: &'a [u8], pos: u64) -> Result<Self, LoadError> {
        let pos = usize::try_from(pos).ok().filter(|&p| p <= d.len()).ok_or_else(|| err("offset outside the file"))?;
        Ok(R { d, pos })
    }

    fn bytes(&mut self, n: u64) -> Result<&'a [u8], LoadError> {
        let n = usize::try_from(n).map_err(|_| err("size too large"))?;
        let end = self.pos.checked_add(n).filter(|&e| e <= self.d.len()).ok_or_else(|| err("read past the end"))?;
        let out = &self.d[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], LoadError> {
        let b = self.bytes(N as u64)?;
        let mut a = [0u8; N];
        a.copy_from_slice(b);
        Ok(a)
    }

    fn u8(&mut self) -> Result<u8, LoadError> {
        Ok(self.array::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32, LoadError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, LoadError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn i64(&mut self) -> Result<i64, LoadError> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    fn f64(&mut self) -> Result<f64, LoadError> {
        Ok(f64::from_le_bytes(self.array()?))
    }

    fn remaining(&self) -> u64 {
        self.d.len().saturating_sub(self.pos) as u64
    }

    /// A count of items at least `item_bytes` long each, which must fit in
    /// the rest of the file.
    fn count(&mut self, item_bytes: u64) -> Result<usize, LoadError> {
        let n = self.u64()?;
        if n.saturating_mul(item_bytes.max(1)) > self.remaining() {
            return Err(err("count larger than the file"));
        }
        Ok(n as usize)
    }
}

fn take(budget: &mut u64, bytes: u64) -> Result<(), LoadError> {
    if bytes > *budget {
        return Err(LoadError::TooLarge("USD crate data larger than its file allows".into()));
    }
    *budget -= bytes;
    Ok(())
}

/// Undoes TfFastCompression: a chunk count byte, then either one LZ4 block
/// or that many size-prefixed blocks.
fn decompress(input: &[u8], out_size: u64, budget: &mut u64) -> Result<Vec<u8>, LoadError> {
    take(budget, out_size)?;
    let mut out = vec![0u8; out_size as usize];
    let lz4 = |e: lz4_flex::block::DecompressError| err(format!("LZ4: {e}"));
    let (&chunks, mut rest) = input.split_first().ok_or_else(|| err("empty compressed block"))?;
    let total = if chunks == 0 {
        lz4_flex::block::decompress_into(rest, &mut out).map_err(lz4)?
    } else {
        let mut total = 0;
        for _ in 0..chunks {
            let size = rest.get(..4).ok_or_else(|| err("truncated chunk"))?;
            let size = i32::from_le_bytes([size[0], size[1], size[2], size[3]]);
            let size = usize::try_from(size).map_err(|_| err("bad chunk size"))?;
            let chunk = rest.get(4..4 + size).ok_or_else(|| err("truncated chunk"))?;
            total += lz4_flex::block::decompress_into(chunk, &mut out[total..]).map_err(lz4)?;
            rest = &rest[4 + size..];
        }
        total
    };
    out.truncate(total);
    Ok(out)
}

/// Size of the buffer integer compression needs for `count` integers of
/// `width` bytes: the common value, two bits of code each, and the values.
fn encoded_size(count: usize, width: u64) -> u64 {
    let count = count as u64;
    width.saturating_add(count.saturating_mul(2).div_ceil(8)).saturating_add(count.saturating_mul(width))
}

/// Reads `count` compressed 32-bit integers: a compressed size, then the
/// LZ4 block holding the delta coding.
fn compressed_ints32(r: &mut R, count: usize, budget: &mut u64) -> Result<Vec<i32>, LoadError> {
    let size = r.u64()?;
    let data = r.bytes(size)?;
    take(budget, (count as u64).saturating_mul(4))?;
    let buf = decompress(data, encoded_size(count, 4), budget)?;
    let out = decode_ints(&buf, count, 4).map(|v| v.into_iter().map(|x| x as i32).collect());
    *budget += buf.len() as u64;
    out
}

fn compressed_ints64(r: &mut R, count: usize, budget: &mut u64) -> Result<Vec<i64>, LoadError> {
    let size = r.u64()?;
    let data = r.bytes(size)?;
    take(budget, (count as u64).saturating_mul(8))?;
    let buf = decompress(data, encoded_size(count, 8), budget)?;
    let out = decode_ints(&buf, count, 8);
    *budget += buf.len() as u64;
    out
}

/// Undoes USD's integer coding: a common delta, then two-bit codes per
/// value (common, small, medium, full width), then the non-common deltas.
/// Values are running sums of the deltas.
fn decode_ints(buf: &[u8], count: usize, width: usize) -> Result<Vec<i64>, LoadError> {
    let get = |at: usize, n: usize| -> Result<i64, LoadError> {
        let b = buf.get(at..at + n).ok_or_else(|| err("truncated integers"))?;
        Ok(match n {
            1 => b[0] as i8 as i64,
            2 => i16::from_le_bytes([b[0], b[1]]) as i64,
            4 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as i64,
            _ => i64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        })
    };
    let common = get(0, width)?;
    let codes_len = (count * 2).div_ceil(8);
    let codes = buf.get(width..width + codes_len).ok_or_else(|| err("truncated integer codes"))?;
    let mut at = width + codes_len;
    let (small, medium) = if width == 4 { (1, 2) } else { (2, 4) };
    let mut out = Vec::with_capacity(count);
    let mut prev: i64 = 0;
    for i in 0..count {
        let code = (codes[i / 4] >> ((i % 4) * 2)) & 3;
        let delta = match code {
            0 => common,
            1 => {
                at += small;
                get(at - small, small)?
            }
            2 => {
                at += medium;
                get(at - medium, medium)?
            }
            _ => {
                at += width;
                get(at - width, width)?
            }
        };
        prev = if width == 4 { (prev as i32).wrapping_add(delta as i32) as i64 } else { prev.wrapping_add(delta) };
        out.push(prev);
    }
    Ok(out)
}

fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = (h >> 10) & 0x1F;
    let frac = (h & 0x3FF) as f32;
    sign * match exp {
        0 => frac * (2.0f32).powi(-24),
        31 => {
            if frac == 0.0 {
                f32::INFINITY
            } else {
                f32::NAN
            }
        }
        e => (1.0 + frac / 1024.0) * (2.0f32).powi(e as i32 - 15),
    }
}

//endregion

/*┌─────────────────╮
| Tables          |
└─────────────────╯*/
//region Tables

struct Crate<'a> {
    d: &'a [u8],
    version: (u8, u8, u8),
    /// Keep every time sample of the properties `keeps_samples` names.
    samples: bool,
    tokens: Vec<String>,
    strings: Vec<u32>,
    /// Name token and value rep of each field.
    fields: Vec<(u32, u64)>,
    fieldsets: Vec<u32>,
    paths: Vec<String>,
    /// Path index, field set index and spec kind of each spec.
    specs: Vec<(u32, u32, u32)>,
}

pub(crate) fn read(d: &[u8], budget: &mut u64, samples: bool) -> Result<Layer, LoadError> {
    // 1. Header and table of contents.
    let mut r = R { d, pos: 8 };
    let v = r.array::<8>()?;
    let version = (v[0], v[1], v[2]);
    if version < (0, 4, 0) {
        return Err(LoadError::Unsupported(format!("USD crate version {}.{}.{}", v[0], v[1], v[2])));
    }
    let toc = r.u64()?;
    let mut r = R::at(d, toc)?;
    let section_count = r.count(32)?;
    let mut sections: Vec<([u8; 16], u64)> = Vec::new();
    for _ in 0..section_count.min(64) {
        let name = r.array::<16>()?;
        let start = r.u64()?;
        let _size = r.u64()?;
        sections.push((name, start));
    }
    let section =
        |name: &str| sections.iter().find(|(n, _)| n.split(|&b| b == 0).next() == Some(name.as_bytes())).map(|s| s.1);
    let mut c = Crate {
        d,
        version,
        samples,
        tokens: Vec::new(),
        strings: Vec::new(),
        fields: Vec::new(),
        fieldsets: Vec::new(),
        paths: Vec::new(),
        specs: Vec::new(),
    };

    // 2. TOKENS: counts, sizes, then null-separated strings.
    if let Some(start) = section("TOKENS") {
        let mut r = R::at(d, start)?;
        let count = r.u64()?;
        let size = r.u64()?;
        let compressed = r.u64()?;
        if count > size {
            return Err(err("more tokens than bytes"));
        }
        let text = decompress(r.bytes(compressed)?, size, budget)?;
        take(budget, count.saturating_mul(24))?;
        c.tokens =
            text.split(|&b| b == 0).take(count as usize).map(|t| String::from_utf8_lossy(t).into_owned()).collect();
        if (c.tokens.len() as u64) < count {
            return Err(err("fewer tokens than the table claims"));
        }
    }

    // 3. STRINGS: token indices.
    if let Some(start) = section("STRINGS") {
        let mut r = R::at(d, start)?;
        let count = r.count(4)?;
        c.strings = (0..count).map(|_| r.u32()).collect::<Result<_, _>>()?;
    }

    // 4. FIELDS: name tokens, then value reps as one LZ4 block.
    if let Some(start) = section("FIELDS") {
        let mut r = R::at(d, start)?;
        let count = usize::try_from(r.u64()?).map_err(|_| err("too many fields"))?;
        let names = compressed_ints32(&mut r, count, budget)?;
        let reps_size = r.u64()?;
        let reps = decompress(r.bytes(reps_size)?, count as u64 * 8, budget)?;
        if reps.len() < count * 8 {
            return Err(err("truncated field values"));
        }
        take(budget, count as u64 * 16)?;
        c.fields = names
            .iter()
            .zip(reps.chunks_exact(8))
            .map(|(&n, rep)| (n as u32, u64::from_le_bytes(rep.try_into().unwrap_or([0; 8]))))
            .collect();
    }

    // 5. FIELDSETS: runs of field indices ended by -1.
    if let Some(start) = section("FIELDSETS") {
        let mut r = R::at(d, start)?;
        let count = usize::try_from(r.u64()?).map_err(|_| err("too many field sets"))?;
        c.fieldsets = compressed_ints32(&mut r, count, budget)?.into_iter().map(|x| x as u32).collect();
    }

    // 6. PATHS and SPECS.
    if let Some(start) = section("PATHS") {
        c.read_paths(start, budget)?;
    }
    if let Some(start) = section("SPECS") {
        let mut r = R::at(d, start)?;
        let count = usize::try_from(r.u64()?).map_err(|_| err("too many specs"))?;
        let paths = compressed_ints32(&mut r, count, budget)?;
        let sets = compressed_ints32(&mut r, count, budget)?;
        let kinds = compressed_ints32(&mut r, count, budget)?;
        c.specs = (0..count).map(|i| (paths[i] as u32, sets[i] as u32, kinds[i] as u32)).collect();
    }

    c.layer(budget)
}

impl Crate<'_> {
    fn token(&self, i: u64) -> Result<&str, LoadError> {
        self.tokens.get(i as usize).map(String::as_str).ok_or_else(|| err("token index out of range"))
    }

    fn string(&self, i: u64) -> Result<&str, LoadError> {
        let t = *self.strings.get(i as usize).ok_or_else(|| err("string index out of range"))?;
        self.token(t as u64)
    }

    fn path(&self, i: u64) -> Result<&str, LoadError> {
        self.paths.get(i as usize).map(String::as_str).ok_or_else(|| err("path index out of range"))
    }

    /// The path tree: for each node in depth-first order, its index in the
    /// path table, its name token (negative for properties) and a jump:
    /// -1 means only a child follows, 0 only a sibling, -2 neither, and a
    /// positive jump says how far ahead the sibling is (the child follows).
    fn read_paths(&mut self, start: u64, budget: &mut u64) -> Result<(), LoadError> {
        let mut r = R::at(self.d, start)?;
        let total = r.u64()?;
        let count = r.u64()?;
        if count > total.max(1) {
            return Err(err("damaged path table"));
        }
        take(budget, total.saturating_mul(48))?;
        let (total, count) = (total as usize, count as usize);
        let indices = compressed_ints32(&mut r, count, budget)?;
        let names = compressed_ints32(&mut r, count, budget)?;
        let jumps = compressed_ints32(&mut r, count, budget)?;
        let mut paths = vec![String::new(); total];
        let mut visited = vec![false; count];
        let mut stack: Vec<(usize, Option<String>)> = if count > 0 { vec![(0, None)] } else { Vec::new() };
        while let Some((mut i, mut parent)) = stack.pop() {
            loop {
                if i >= count || visited[i] {
                    return Err(err("damaged path tree"));
                }
                visited[i] = true;
                let path = match &parent {
                    None => "/".to_string(),
                    Some(p) => {
                        let tok = names[i];
                        let name = self.token(tok.unsigned_abs() as u64)?;
                        if tok < 0 {
                            format!("{p}.{name}")
                        } else if name.starts_with('{') {
                            format!("{p}{name}")
                        } else {
                            child_path(p, name)
                        }
                    }
                };
                let slot = indices[i] as usize;
                *paths.get_mut(slot).ok_or_else(|| err("path index out of range"))? = path.clone();
                let jump = jumps[i];
                let has_child = jump > 0 || jump == -1;
                let has_sibling = jump >= 0;
                if has_child && has_sibling {
                    let sibling = i.checked_add(jump as usize).ok_or_else(|| err("damaged path tree"))?;
                    stack.push((sibling, parent.clone()));
                }
                if has_child {
                    parent = Some(path);
                }
                if !has_child && !has_sibling {
                    break;
                }
                i += 1;
            }
        }
        self.paths = paths;
        Ok(())
    }

    /// The fields of the field set starting at `start`.
    fn fieldset(&self, start: u32) -> Vec<(&str, u64)> {
        let mut out = Vec::new();
        let mut i = start as usize;
        while let Some(&f) = self.fieldsets.get(i) {
            if f == u32::MAX || out.len() > 4096 {
                break;
            }
            if let Some(&(name, rep)) = self.fields.get(f as usize)
                && let Ok(name) = self.token(name as u64)
            {
                out.push((name, rep));
            }
            i += 1;
        }
        out
    }

    fn layer(&self, budget: &mut u64) -> Result<Layer, LoadError> {
        let (a, b, c) = self.version;
        let mut layer = Layer { format: format!("USD crate {a}.{b}.{c}"), ..Default::default() };
        layer.prims.insert("/".into(), Default::default());
        for &(path, set, kind) in &self.specs {
            let path = self.path(path as u64)?.to_string();
            let fields = self.fieldset(set);
            match kind {
                // The pseudo-root: layer metadata.
                7 => {
                    for (name, rep) in fields {
                        let v = self.value(rep, 0, budget)?;
                        match name {
                            "defaultPrim" => layer.default_prim = v.as_str().map(str::to_string),
                            "upAxis" => layer.up_axis = v.as_str().map(str::to_string),
                            "metersPerUnit" => layer.meters_per_unit = v.as_f64(),
                            "timeCodesPerSecond" => layer.time_codes_per_second = v.as_f64(),
                            "framesPerSecond" => layer.frames_per_second = v.as_f64(),
                            // The newer name wins over the older one.
                            "startTimeCode" => layer.start_time = v.as_f64(),
                            "startFrame" => layer.start_time = layer.start_time.or(v.as_f64()),
                            "subLayers" => {
                                if let Value::Strs(s) = v {
                                    layer.sublayers = s;
                                }
                            }
                            "primChildren" => {
                                if let Value::Strs(s) = v {
                                    layer.prim_mut("/").children = s;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                // Prims and variants.
                6 | 10 => {
                    let spec = layer.prim_mut(&path);
                    for (name, rep) in fields {
                        let wanted = matches!(
                            name,
                            "specifier"
                                | "typeName"
                                | "primChildren"
                                | "apiSchemas"
                                | "references"
                                | "payload"
                                | "inheritPaths"
                                | "specializes"
                                | "variantSetNames"
                                | "variantSelection"
                                | "active"
                                | "assetInfo"
                                | "inactiveIds"
                        );
                        if !wanted {
                            continue;
                        }
                        let v = self.value(rep, 0, budget)?;
                        match (name, v) {
                            ("specifier", v) => {
                                spec.specifier = match v.as_f64() {
                                    Some(0.0) => Specifier::Def,
                                    Some(2.0) => Specifier::Class,
                                    _ => Specifier::Over,
                                }
                            }
                            ("typeName", Value::Str(s)) => spec.type_name = s,
                            ("primChildren", Value::Strs(s)) => spec.children = s,
                            ("apiSchemas", Value::Strs(s)) => spec.api_schemas = s,
                            ("variantSetNames", Value::Strs(s)) => spec.variant_sets = s,
                            ("inheritPaths" | "specializes", Value::Paths(p)) => spec.inherits.extend(p),
                            ("references", Value::Dict(refs)) => spec.references = refs_of(refs),
                            ("payload", Value::Dict(refs)) => spec.payloads = refs_of(refs),
                            ("variantSelection", Value::Dict(d)) => {
                                spec.variant_selection =
                                    d.into_iter().filter_map(|(k, v)| Some((k, v.as_str()?.to_string()))).collect();
                            }
                            ("active", v) => spec.active = v.as_f64().map(|f| f != 0.0),
                            ("assetInfo", v @ Value::Dict(_)) => spec.asset_info = Some(v),
                            ("inactiveIds", Value::Ints(ids)) => {
                                spec.inactive_ids = Some(ids.into_iter().map(i64::from).collect());
                            }
                            _ => {}
                        }
                    }
                }
                // Attributes and relationships.
                1 | 8 => {
                    let Some((prim, prop_name)) = split_property(&path) else { continue };
                    let mut prop = Property { name: prop_name.to_string(), ..Default::default() };
                    for (name, rep) in fields {
                        match name {
                            "typeName" => prop.type_name = self.value(rep, 0, budget)?.as_str().unwrap_or("").into(),
                            "default" => prop.default = Some(Arc::new(self.value(rep, 0, budget)?)),
                            "timeSamples" => {
                                let all = self.samples && super::keeps_samples(prop_name);
                                (prop.sample, prop.samples) = self.time_samples(rep, all, budget)?;
                            }
                            "weight" => prop.weight = self.value(rep, 0, budget)?.as_f64(),
                            "connectionPaths" | "targetPaths" => {
                                if let Value::Paths(p) = self.value(rep, 0, budget)? {
                                    prop.targets = p;
                                }
                            }
                            "interpolation" => {
                                prop.interpolation = self.value(rep, 0, budget)?.as_str().map(Into::into)
                            }
                            "colorSpace" => prop.color_space = self.value(rep, 0, budget)?.as_str().map(Into::into),
                            "elementSize" => prop.element_size = self.value(rep, 0, budget)?.as_f64().map(|f| f as i64),
                            "bindMaterialAs" => {
                                let v = self.value(rep, 0, budget)?;
                                prop.stronger = Some(v.as_str() == Some("strongerThanDescendants"));
                            }
                            _ => {}
                        }
                    }
                    let spec = layer.prim_mut(prim);
                    let slot = spec.prop_mut(prop_name);
                    *slot = prop;
                }
                _ => {}
            }
        }
        Ok(layer)
    }
}

/// Splits `/Prim.prop` into the prim path and property name.
fn split_property(path: &str) -> Option<(&str, &str)> {
    let dot = path.rfind('.')?;
    let (prim, prop) = (&path[..dot], &path[dot + 1..]);
    if prop.contains(['/', '}', '[']) || prim.is_empty() {
        return None;
    }
    Some((prim, prop))
}

/// References come back from `value` as a dictionary of asset to prim path.
fn refs_of(entries: Vec<(String, Value)>) -> Vec<Reference> {
    entries
        .into_iter()
        .map(|(asset, prim)| Reference { asset, prim: prim.as_str().unwrap_or("").to_string() })
        .collect()
}

//endregion

/*┌─────────────────╮
| Values          |
└─────────────────╯*/
//region Values

/// A list operation on integers (`inactiveIds`, for one): a flag byte,
/// then the item lists it says it has, kept as `list_op` keeps them.
fn int_list_op(r: &mut R, ty: u8) -> Result<Value, LoadError> {
    let flags = r.u8()?;
    let wide = ty == 37 || ty == 39;
    let mut out = Vec::new();
    for bit in [2u8, 4, 32, 64, 8, 16] {
        if flags & bit == 0 {
            continue;
        }
        let keep = bit != 8 && bit != 16 && (flags & 1 == 0 || bit == 2);
        let n = r.count(if wide { 8 } else { 4 })?;
        for _ in 0..n {
            let v = if wide { r.i64()? } else { r.u32()? as i32 as i64 };
            if keep {
                out.push(v.clamp(i32::MIN as i64, i32::MAX as i64) as i32);
            }
        }
    }
    Ok(Value::Ints(out))
}

const IS_ARRAY: u64 = 1 << 63;
const IS_INLINED: u64 = 1 << 62;
const IS_COMPRESSED: u64 = 1 << 61;
const PAYLOAD: u64 = (1 << 48) - 1;

/// Arrays this short are never compressed.
const MIN_COMPRESSED: usize = 16;

/// Component type and count of the vector, quaternion and matrix types.
fn tuple_shape(ty: u8) -> Option<(u8, usize)> {
    // Component kinds: b'h' half, b'f' float, b'd' double, b'i' int.
    Some(match ty {
        13 => (b'd', 4),
        14 => (b'd', 9),
        15 => (b'd', 16),
        16 => (b'd', 4),
        17 => (b'f', 4),
        18 => (b'h', 4),
        19 => (b'd', 2),
        20 => (b'f', 2),
        21 => (b'h', 2),
        22 => (b'i', 2),
        23 => (b'd', 3),
        24 => (b'f', 3),
        25 => (b'h', 3),
        26 => (b'i', 3),
        27 => (b'd', 4),
        28 => (b'f', 4),
        29 => (b'h', 4),
        30 => (b'i', 4),
        _ => return None,
    })
}

fn is_quat(ty: u8) -> bool {
    (16..=18).contains(&ty)
}

fn component(r: &mut R, kind: u8) -> Result<f64, LoadError> {
    Ok(match kind {
        b'h' => half_to_f32(u16::from_le_bytes(r.array()?)) as f64,
        b'f' => f32::from_le_bytes(r.array()?) as f64,
        b'i' => i32::from_le_bytes(r.array()?) as f64,
        _ => r.f64()?,
    })
}

fn component_bytes(kind: u8) -> u64 {
    match kind {
        b'h' => 2,
        b'f' | b'i' => 4,
        _ => 8,
    }
}

/// Crate files store quaternions imaginary part first; `.usda` and this
/// crate's `Value` put the real part first.
fn real_first(v: &mut [f64]) {
    if v.len() == 4 {
        v.rotate_right(1);
    }
}

impl Crate<'_> {
    /// Unpacks a value rep.  Types Slerp doesn't use become `None`.
    fn value(&self, rep: u64, depth: u32, budget: &mut u64) -> Result<Value, LoadError> {
        if depth > 16 {
            return Err(err("values nested too deeply"));
        }
        let ty = ((rep >> 48) & 0xFF) as u8;
        let payload = rep & PAYLOAD;
        if rep & IS_ARRAY != 0 {
            return self.array(ty, rep, budget);
        }
        if rep & IS_INLINED != 0 {
            return self.inlined(ty, payload);
        }
        let mut r = R::at(self.d, payload)?;
        Ok(match ty {
            5 | 6 => Value::Int(r.i64()?),
            9 | 56 => Value::Float(r.f64()?),
            _ if tuple_shape(ty).is_some() => {
                let (kind, n) = tuple_shape(ty).unwrap_or((b'd', 0));
                let mut v = (0..n).map(|_| component(&mut r, kind)).collect::<Result<Vec<_>, _>>()?;
                if is_quat(ty) {
                    real_first(&mut v);
                }
                Value::Tuple(v)
            }
            31 => self.dict(&mut r, depth, budget)?,
            // List operations come back flattened.  References and payloads
            // as a dictionary of asset path to prim path.
            32 | 33 | 34 | 35 | 55 => self.list_op(&mut r, ty, depth, budget)?,
            36..=39 => int_list_op(&mut r, ty)?,
            47 => {
                let (asset, prim) = self.payload(&mut r)?;
                Value::Dict(vec![(asset, Value::Str(prim))])
            }
            40 | 41 | 50 => {
                let n = r.count(4)?;
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    let i = r.u32()? as u64;
                    out.push(
                        match ty {
                            40 => self.path(i)?,
                            41 => self.token(i)?,
                            _ => self.string(i)?,
                        }
                        .to_string(),
                    );
                }
                if ty == 40 { Value::Paths(out) } else { Value::Strs(out) }
            }
            48 => {
                let n = r.count(8)?;
                Value::Floats((0..n).map(|_| r.f64().map(|f| f as f32)).collect::<Result<_, _>>()?, 1)
            }
            45 => {
                let n = r.count(8)?;
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    let k = self.string(r.u32()? as u64)?.to_string();
                    let v = self.string(r.u32()? as u64)?.to_string();
                    out.push((k, Value::Str(v)));
                }
                Value::Dict(out)
            }
            _ => Value::None,
        })
    }

    fn inlined(&self, ty: u8, payload: u64) -> Result<Value, LoadError> {
        let bits = payload as u32;
        Ok(match ty {
            1 => Value::Bool(bits != 0),
            2 => Value::Int(bits as u8 as i64),
            3 => Value::Int(bits as i32 as i64),
            4 => Value::Int(bits as i64),
            5 | 6 => Value::Int(payload as i64),
            7 => Value::Float(half_to_f32(bits as u16) as f64),
            // Doubles that fit a float are stored as one.
            8 | 9 | 56 => Value::Float(f32::from_bits(bits) as f64),
            10 => Value::Str(self.string(payload)?.to_string()),
            11 => Value::Str(self.token(payload)?.to_string()),
            12 => Value::Asset(self.token(payload)?.to_string()),
            // Vectors with small whole components keep them as bytes;
            // matrices keep their diagonal.
            13..=15 => {
                let n = [2, 3, 4][(ty - 13) as usize];
                let mut m = vec![0.0; n * n];
                for i in 0..n {
                    m[i * n + i] = (payload >> (8 * i)) as u8 as i8 as f64;
                }
                Value::Tuple(m)
            }
            _ if tuple_shape(ty).is_some() => {
                let n = tuple_shape(ty).map_or(0, |s| s.1);
                let mut v: Vec<f64> = (0..n).map(|i| (payload >> (8 * i)) as u8 as i8 as f64).collect();
                if is_quat(ty) {
                    real_first(&mut v);
                }
                Value::Tuple(v)
            }
            42..=44 => Value::Int(bits as i64),
            31 => Value::Dict(Vec::new()),
            _ => Value::None,
        })
    }

    fn array(&self, ty: u8, rep: u64, budget: &mut u64) -> Result<Value, LoadError> {
        let payload = rep & PAYLOAD;
        let empty = match ty {
            10..=12 => Value::Strs(Vec::new()),
            1..=6 => Value::Ints(Vec::new()),
            _ => Value::Floats(Vec::new(), tuple_shape(ty).map_or(1, |s| s.1)),
        };
        if payload == 0 || rep & IS_INLINED != 0 {
            return Ok(empty);
        }
        let mut r = R::at(self.d, payload)?;
        if self.version < (0, 5, 0) {
            r.u32()?;
        }
        let count = if self.version < (0, 7, 0) { r.u32()? as u64 } else { r.u64()? };
        let compressed = rep & IS_COMPRESSED != 0 && count as usize >= MIN_COMPRESSED;
        let n = usize::try_from(count).map_err(|_| err("array too large"))?;
        match ty {
            // Integers: possibly compressed.
            1..=6 => {
                let width: u64 = match ty {
                    1 | 2 => 1,
                    5 | 6 => 8,
                    _ => 4,
                };
                let ints: Vec<i32> = if compressed && width == 4 && self.version >= (0, 5, 0) {
                    compressed_ints32(&mut r, n, budget)?
                } else if compressed && width == 8 && self.version >= (0, 5, 0) {
                    compressed_ints64(&mut r, n, budget)?
                        .into_iter()
                        .map(|x| x.clamp(-1, i32::MAX as i64) as i32)
                        .collect()
                } else {
                    let raw = r.bytes(count.saturating_mul(width))?;
                    take(budget, count.saturating_mul(4))?;
                    raw.chunks_exact(width as usize)
                        .map(|c| match width {
                            1 => c[0] as i32,
                            4 => i32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                            _ => i64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]])
                                .clamp(-1, i32::MAX as i64) as i32,
                        })
                        .collect()
                };
                Ok(Value::Ints(ints))
            }
            // Floats: possibly compressed as integers or a lookup table.
            7..=9 => {
                let kind = [b'h', b'f', b'd'][(ty - 7) as usize];
                take(budget, count.saturating_mul(4))?;
                if !compressed || self.version < (0, 6, 0) {
                    let raw = r.bytes(count.saturating_mul(component_bytes(kind)))?;
                    let mut rr = R { d: raw, pos: 0 };
                    let v = (0..n).map(|_| component(&mut rr, kind).map(|f| f as f32)).collect::<Result<_, _>>()?;
                    return Ok(Value::Floats(v, 1));
                }
                match r.u8()? {
                    b'i' => Ok(Value::Floats(
                        compressed_ints32(&mut r, n, budget)?.into_iter().map(|i| i as f32).collect(),
                        1,
                    )),
                    b't' => {
                        let lut_len = r.u32()? as u64;
                        let raw = r.bytes(lut_len.saturating_mul(component_bytes(kind)))?;
                        let mut rr = R { d: raw, pos: 0 };
                        let lut: Vec<f32> = (0..lut_len)
                            .map(|_| component(&mut rr, kind).map(|f| f as f32))
                            .collect::<Result<_, _>>()?;
                        let indices = compressed_ints32(&mut r, n, budget)?;
                        let v = indices
                            .into_iter()
                            .map(|i| {
                                lut.get(i as u32 as usize).copied().ok_or_else(|| err("lookup index out of range"))
                            })
                            .collect::<Result<_, _>>()?;
                        Ok(Value::Floats(v, 1))
                    }
                    _ => Err(err("unknown float array coding")),
                }
            }
            // Strings, tokens and asset paths: indices.
            10..=12 => {
                let raw = r.bytes(count.saturating_mul(4))?;
                take(budget, count.saturating_mul(32))?;
                let out = raw
                    .chunks_exact(4)
                    .map(|c| {
                        let i = u32::from_le_bytes([c[0], c[1], c[2], c[3]]) as u64;
                        // Asset paths in arrays are string indices, unlike inlined ones.
                        let s = if ty == 11 { self.token(i) } else { self.string(i) };
                        s.map(str::to_string)
                    })
                    .collect::<Result<_, _>>()?;
                Ok(Value::Strs(out))
            }
            // Vectors, quaternions, matrices: raw components.
            _ => {
                let Some((kind, width)) = tuple_shape(ty) else { return Ok(Value::None) };
                let total = count.saturating_mul(width as u64);
                let raw = r.bytes(total.saturating_mul(component_bytes(kind)))?;
                take(budget, total.saturating_mul(4))?;
                let mut rr = R { d: raw, pos: 0 };
                let mut v: Vec<f32> =
                    (0..total).map(|_| component(&mut rr, kind).map(|f| f as f32)).collect::<Result<_, _>>()?;
                if is_quat(ty) {
                    v.chunks_exact_mut(4).for_each(|q| q.rotate_right(1));
                }
                Ok(Value::Floats(v, width))
            }
        }
    }

    /// A dictionary: a count, then string keys each followed by a relative
    /// offset to the value's rep, after which the next key starts.
    fn dict(&self, r: &mut R, depth: u32, budget: &mut u64) -> Result<Value, LoadError> {
        let n = r.count(12)?;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let key = self.string(r.u32()? as u64)?.to_string();
            let start = r.pos as i64;
            let offset = r.i64()?;
            let at = start.checked_add(offset).and_then(|a| u64::try_from(a).ok()).ok_or_else(|| err("bad offset"))?;
            let mut rr = R::at(self.d, at)?;
            let rep = rr.u64()?;
            out.push((key, self.value(rep, depth + 1, budget)?));
            r.pos = rr.pos;
        }
        Ok(Value::Dict(out))
    }

    /// A list operation: a flag byte, then the item lists it says it has.
    /// The result keeps explicit items, or else prepended, added and
    /// appended ones; deletions and reorderings are ignored.
    fn list_op(&self, r: &mut R, ty: u8, depth: u32, budget: &mut u64) -> Result<Value, LoadError> {
        let flags = r.u8()?;
        let mut items: Vec<(String, Value)> = Vec::new();
        // Lists appear in this order: explicit, added, prepended, appended,
        // deleted, ordered.
        for bit in [2u8, 4, 32, 64, 8, 16] {
            if flags & bit == 0 {
                continue;
            }
            let keep = bit != 8 && bit != 16 && (flags & 1 == 0 || bit == 2);
            let n = r.count(4)?;
            for _ in 0..n {
                let item = match ty {
                    32 => (self.token(r.u32()? as u64)?.to_string(), Value::None),
                    33 => (self.string(r.u32()? as u64)?.to_string(), Value::None),
                    34 => (self.path(r.u32()? as u64)?.to_string(), Value::None),
                    35 => {
                        let asset = self.string(r.u32()? as u64)?.to_string();
                        let prim = self.path(r.u32()? as u64)?.to_string();
                        r.f64()?;
                        r.f64()?;
                        self.dict(r, depth + 1, budget)?;
                        (asset, Value::Str(prim))
                    }
                    _ => {
                        let (asset, prim) = self.payload(r)?;
                        (asset, Value::Str(prim))
                    }
                };
                if keep {
                    items.push(item);
                }
            }
        }
        Ok(match ty {
            32 | 33 => Value::Strs(items.into_iter().map(|i| i.0).collect()),
            34 => Value::Paths(items.into_iter().map(|i| i.0).collect()),
            _ => Value::Dict(items),
        })
    }

    /// The time codes of a time-sample map: a list of doubles.
    fn times(&self, rep: u64, budget: &mut u64) -> Result<Vec<f64>, LoadError> {
        let ty = ((rep >> 48) & 0xFF) as u8;
        if ty == 48 && rep & (IS_ARRAY | IS_INLINED) == 0 {
            let mut r = R::at(self.d, rep & PAYLOAD)?;
            let n = r.count(8)?;
            take(budget, n as u64 * 8)?;
            return (0..n).map(|_| r.f64()).collect();
        }
        Ok(match self.value(rep, 1, budget)? {
            Value::Floats(t, _) => t.into_iter().map(f64::from).collect(),
            v => v.as_f64().into_iter().collect(),
        })
    }

    fn payload(&self, r: &mut R) -> Result<(String, String), LoadError> {
        let asset = self.string(r.u32()? as u64)?.to_string();
        let prim = self.path(r.u32()? as u64)?.to_string();
        if self.version >= (0, 8, 0) {
            r.f64()?;
            r.f64()?;
        }
        Ok((asset, prim))
    }

    /// Reads a relative offset (from where it is stored) and returns where
    /// it points.
    fn jump(&self, r: &mut R) -> Result<u64, LoadError> {
        let start = r.pos as i64;
        let offset = r.i64()?;
        start.checked_add(offset).and_then(|a| u64::try_from(a).ok()).ok_or_else(|| err("bad offset"))
    }

    /// A time-sample map: a relative offset to the times' rep, then a
    /// relative offset to a count and the values' reps.  Returns the value
    /// at the earliest time and, with `all`, every sample in time order.
    /// Samples share reps when their values are equal (a constant scale,
    /// say), so each distinct rep is decoded once.  Blocked samples are
    /// left out.
    fn time_samples(&self, rep: u64, all: bool, budget: &mut u64) -> Result<TimeSampled, LoadError> {
        if rep & IS_INLINED != 0 {
            return Ok((None, None));
        }
        let mut r = R::at(self.d, rep & PAYLOAD)?;
        let at = self.jump(&mut r)?;
        let mut r = R::at(self.d, at)?;
        let times = self.times(r.u64()?, budget)?;
        let at = self.jump(&mut r)?;
        let mut r = R::at(self.d, at)?;
        let n = r.count(8)?.min(times.len());
        let reps: Vec<u64> = (0..n).map(|_| r.u64()).collect::<Result<_, _>>()?;
        let mut order: Vec<usize> = (0..n).filter(|&i| times[i].is_finite()).collect();
        // Later duplicates of a time win, as in USD's own reader.
        order.reverse();
        order.sort_by(|&a, &b| times[a].total_cmp(&times[b]));
        order.dedup_by(|a, b| times[*a] == times[*b]);
        if !all {
            for &i in &order {
                let v = self.value(reps[i], 1, budget)?;
                if v != Value::None {
                    return Ok((Some(Arc::new(v)), None));
                }
            }
            return Ok((None, None));
        }
        let mut decoded: HashMap<u64, Arc<Value>> = HashMap::new();
        let mut samples = Samples::default();
        for i in order {
            let v = match decoded.get(&reps[i]) {
                Some(v) => v.clone(),
                None => {
                    let v = Arc::new(self.value(reps[i], 1, budget)?);
                    decoded.insert(reps[i], v.clone());
                    v
                }
            };
            if *v != Value::None {
                samples.times.push(times[i]);
                samples.values.push(v);
            }
        }
        Ok((samples.values.first().cloned(), Some(Arc::new(samples))))
    }
}

//endregion
