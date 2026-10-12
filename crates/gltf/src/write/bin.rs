//! The binary chunk: buffer views and accessors, and JSON numbers.
//!
//! Each accessor gets a view of its own, tightly packed and 4-byte
//! aligned, which glTF allows for every component type and which keeps the
//! writer simple.  GLB is little-endian, as every platform Slerp builds for
//! is, so slices are copied as they are.

use super::WriteError;
use serde_json::{Map, Value, json};

/// The largest binary chunk: GLB's lengths are 32-bit, and the JSON needs
/// room too.
const MAX_BIN: usize = u32::MAX as usize - (256 << 20);

pub const ARRAY_BUFFER: u32 = 34962;
pub const ELEMENT_ARRAY_BUFFER: u32 = 34963;

pub const BYTE: u32 = 5121;
pub const SHORT: u32 = 5123;
pub const INT: u32 = 5125;
pub const FLOAT: u32 = 5126;

/// An accessor's element type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Scalar,
    Vec2,
    Vec3,
    Vec4,
    Mat4,
}

impl Kind {
    pub fn components(self) -> usize {
        match self {
            Kind::Scalar => 1,
            Kind::Vec2 => 2,
            Kind::Vec3 => 3,
            Kind::Vec4 => 4,
            Kind::Mat4 => 16,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Scalar => "SCALAR",
            Kind::Vec2 => "VEC2",
            Kind::Vec3 => "VEC3",
            Kind::Vec4 => "VEC4",
            Kind::Mat4 => "MAT4",
        }
    }
}

#[derive(Default)]
pub struct Bin {
    pub data: Vec<u8>,
    pub views: Vec<Value>,
    pub accessors: Vec<Value>,
}

impl Bin {
    /// Appends `bytes` as a buffer view, 4-byte aligned.
    pub fn view(&mut self, bytes: &[u8], target: Option<u32>) -> Result<usize, WriteError> {
        let start = self.data.len().next_multiple_of(4);
        if start.saturating_add(bytes.len()) > MAX_BIN {
            return Err(WriteError::TooLarge(format!("over {} MB of data", MAX_BIN >> 20)));
        }
        self.data.resize(start, 0);
        self.data.extend_from_slice(bytes);
        let mut v = json!({ "buffer": 0, "byteOffset": start, "byteLength": bytes.len() });
        if let Some(t) = target {
            v["target"] = t.into();
        }
        self.views.push(v);
        Ok(self.views.len() - 1)
    }

    /// An accessor of floats.  `bounds` adds the `min` and `max` glTF
    /// requires of positions and keyframe times.
    pub fn floats(
        &mut self,
        values: &[f32],
        kind: Kind,
        target: Option<u32>,
        bounds: bool,
    ) -> Result<usize, WriteError> {
        let n = kind.components();
        let count = values.len() / n;
        let view = self.view(bytemuck::cast_slice(&values[..count * n]), target)?;
        let mut a = accessor(view, FLOAT, kind, count);
        if bounds {
            let (min, max) = min_max(values, n);
            a.insert("min".into(), min);
            a.insert("max".into(), max);
        }
        Ok(self.push(a))
    }

    /// An accessor of unsigned integers of `component` type (`BYTE`,
    /// `SHORT`, `INT`) from their little-endian bytes.  `normalized` reads
    /// them as fractions of the type's largest value.
    pub fn ints(
        &mut self,
        bytes: &[u8],
        component: u32,
        kind: Kind,
        target: Option<u32>,
        normalized: bool,
    ) -> Result<usize, WriteError> {
        let size = match component {
            BYTE => 1,
            SHORT => 2,
            _ => 4,
        };
        let count = bytes.len() / (size * kind.components());
        let view = self.view(bytes, target)?;
        let mut a = accessor(view, component, kind, count);
        if normalized {
            a.insert("normalized".into(), true.into());
        }
        Ok(self.push(a))
    }

    /// An accessor of `count` zeros with no data behind it, which glTF
    /// defines as all zero: padding for morph targets and texture
    /// coordinates that cost nothing in the file.
    pub fn zeros(&mut self, kind: Kind, count: usize, bounds: bool) -> usize {
        let mut a = Map::new();
        a.insert("componentType".into(), FLOAT.into());
        a.insert("count".into(), count.into());
        a.insert("type".into(), kind.name().into());
        if bounds {
            let zeros = Value::Array(vec![0.0.into(); kind.components()]);
            a.insert("min".into(), zeros.clone());
            a.insert("max".into(), zeros);
        }
        self.push(a)
    }

    fn push(&mut self, accessor: Map<String, Value>) -> usize {
        self.accessors.push(Value::Object(accessor));
        self.accessors.len() - 1
    }
}

fn accessor(view: usize, component: u32, kind: Kind, count: usize) -> Map<String, Value> {
    let mut a = Map::new();
    a.insert("bufferView".into(), view.into());
    a.insert("componentType".into(), component.into());
    a.insert("count".into(), count.into());
    a.insert("type".into(), kind.name().into());
    a
}

/// Per-component bounds, written as the exact values of the 32-bit floats
/// so a reader comparing them with the data finds them equal.
fn min_max(values: &[f32], n: usize) -> (Value, Value) {
    let mut min = vec![f32::INFINITY; n];
    let mut max = vec![f32::NEG_INFINITY; n];
    for chunk in values.chunks_exact(n) {
        for (k, &v) in chunk.iter().enumerate() {
            min[k] = min[k].min(v);
            max[k] = max[k].max(v);
        }
    }
    let exact =
        |v: Vec<f32>| Value::Array(v.into_iter().map(|x| if x.is_finite() { x as f64 } else { 0.0 }.into()).collect());
    (exact(min), exact(max))
}

/// A number for the JSON in its shortest form (0.1 rather than
/// 0.10000000149011612), which reads back as the same 32-bit float.
pub fn num(x: f32) -> Value {
    if !x.is_finite() {
        return 0.into();
    }
    format!("{x}").parse::<f64>().map_or_else(|_| (x as f64).into(), Value::from)
}

pub fn nums(xs: &[f32]) -> Value {
    Value::Array(xs.iter().map(|&x| num(x)).collect())
}
