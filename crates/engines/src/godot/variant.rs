//! Godot's variant values as both of its resource formats store them, and
//! the parsed shape of a resource file, shared by the text and binary
//! readers so that everything after parsing reads one model.

use slerp_scene::glam::{Mat3, Mat4, Vec2, Vec3, Vec4};

/// A property value.  Numbers keep Godot's own names and order, so the
/// text and binary readers produce identical values.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// String, StringName and NodePath alike.
    Str(String),
    /// A fixed-size maths type by its Godot 4 name (Vector2, Vector3,
    /// Vector4, Color, Quaternion, Basis, Transform3D, AABB, Plane, Rect2,
    /// Transform2D, Projection and the integer vectors), with its numbers in
    /// the text format's order: matrices row by row, then the origin.
    Math(String, Vec<f64>),
    Bytes(Vec<u8>),
    Ints(Vec<i64>),
    /// Float arrays, and arrays of vectors and colours flattened, by Godot 4
    /// name (PackedFloat32Array, PackedVector3Array, PackedColorArray...).
    Floats(String, Vec<f32>),
    Strings(Vec<String>),
    Array(Vec<Value>),
    Dict(Vec<(Value, Value)>),
    /// A reference to an external resource by its id in this file.
    Ext(String),
    /// A reference to a resource embedded in this file by its id.
    Sub(String),
    /// Something this crate doesn't use (an Object, a Callable), by name.
    Other(String),
}

impl Value {
    pub fn num(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            Value::Bool(b) => Some(*b as i64 as f64),
            _ => None,
        }
    }

    pub fn int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Float(f) if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e18 => Some(*f as i64),
            Value::Bool(b) => Some(*b as i64),
            _ => None,
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            _ => false,
        }
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The numbers of a maths value whose name is one of `names`.
    pub fn math(&self, names: &[&str]) -> Option<&[f64]> {
        match self {
            Value::Math(n, v) if names.contains(&n.as_str()) => Some(v),
            _ => None,
        }
    }

    pub fn vec2(&self) -> Option<Vec2> {
        let v = self.math(&["Vector2", "Vector2i", "Size2"])?;
        (v.len() >= 2).then(|| Vec2::new(v[0] as f32, v[1] as f32)).filter(|v| v.is_finite())
    }

    pub fn vec3(&self) -> Option<Vec3> {
        let v = self.math(&["Vector3", "Vector3i"])?;
        (v.len() >= 3).then(|| Vec3::new(v[0] as f32, v[1] as f32, v[2] as f32)).filter(|v| v.is_finite())
    }

    /// A colour as stored (sRGB-encoded for Godot's colour properties).
    pub fn color(&self) -> Option<Vec4> {
        let v = self.math(&["Color"])?;
        (v.len() >= 3)
            .then(|| Vec4::new(v[0] as f32, v[1] as f32, v[2] as f32, v.get(3).copied().unwrap_or(1.0) as f32))
            .filter(|v| v.is_finite())
    }

    /// A Transform3D (Godot 3's Transform) as a matrix.
    pub fn transform(&self) -> Option<Mat4> {
        let v = self.math(&["Transform3D"])?;
        if v.len() < 12 {
            return None;
        }
        let basis = basis_from_rows(&v[..9]);
        let m = Mat4::from_translation(Vec3::new(v[9] as f32, v[10] as f32, v[11] as f32)) * Mat4::from_mat3(basis);
        m.is_finite().then_some(m)
    }

    /// A Basis as a matrix.
    pub fn basis(&self) -> Option<Mat3> {
        let v = self.math(&["Basis"])?;
        (v.len() >= 9).then(|| basis_from_rows(&v[..9])).filter(|m| m.is_finite())
    }

    pub fn floats(&self) -> Option<&[f32]> {
        match self {
            Value::Floats(_, v) => Some(v),
            _ => None,
        }
    }

    pub fn ints(&self) -> Option<&[i64]> {
        match self {
            Value::Ints(v) => Some(v),
            _ => None,
        }
    }

    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(v) => Some(v),
            _ => None,
        }
    }

    pub fn array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(v) => Some(v),
            _ => None,
        }
    }

    /// The value under a string key of a dictionary.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(pairs) => pairs.iter().find(|(k, _)| k.str() == Some(key)).map(|(_, v)| v),
            _ => None,
        }
    }

    /// A list of 2D points, from a PackedVector2Array.
    pub fn points2(&self) -> Option<Vec<Vec2>> {
        let f = self.floats()?;
        Some(f.chunks_exact(2).map(|c| Vec2::new(c[0], c[1])).collect())
    }

    /// Writes the value in Godot's text syntax, for handing resources to
    /// the materials crate's `.tres` reader.
    pub fn write_text(&self, out: &mut String) {
        use std::fmt::Write;
        match self {
            Value::Nil => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Int(i) => {
                let _ = write!(out, "{i}");
            }
            Value::Float(f) => write_number(out, *f),
            Value::Str(s) => write_string(out, s),
            Value::Math(name, v) => {
                out.push_str(name);
                out.push('(');
                for (i, x) in v.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write_number(out, *x);
                }
                out.push(')');
            }
            Value::Bytes(_) | Value::Ints(_) | Value::Floats(..) | Value::Strings(_) | Value::Other(_) => {
                out.push_str("null")
            }
            Value::Array(items) => {
                out.push('[');
                for (i, x) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    x.write_text(out);
                }
                out.push(']');
            }
            Value::Dict(pairs) => {
                out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    k.write_text(out);
                    out.push_str(": ");
                    v.write_text(out);
                }
                out.push('}');
            }
            Value::Ext(id) => {
                out.push_str("ExtResource(");
                write_string(out, id);
                out.push(')');
            }
            Value::Sub(id) => {
                out.push_str("SubResource(");
                write_string(out, id);
                out.push(')');
            }
        }
    }
}

fn write_number(out: &mut String, f: f64) {
    use std::fmt::Write;
    if f.is_nan() {
        out.push_str("nan");
    } else if f.is_infinite() {
        out.push_str(if f > 0.0 { "inf" } else { "inf_neg" });
    } else {
        let _ = write!(out, "{f}");
    }
}

pub(crate) fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
}

/// A 3x3 matrix from nine numbers written row by row, as Godot writes its
/// Basis.
fn basis_from_rows(v: &[f64]) -> Mat3 {
    let f = |i: usize| v[i] as f32;
    Mat3::from_cols(Vec3::new(f(0), f(3), f(6)), Vec3::new(f(1), f(4), f(7)), Vec3::new(f(2), f(5), f(8)))
}

/*┌─────────────────╮
| Resource files  |
└─────────────────╯*/
//region Resource files

/// A resource file of either format, parsed.
#[derive(Debug, Default)]
pub(crate) struct ResFile {
    /// Saved by Godot 3 or earlier, whose defaults and axes differ.
    pub godot3: bool,
    /// The main resource's type: "PackedScene" for scenes.
    pub kind: String,
    pub ext: Vec<ExtRes>,
    pub subs: Vec<SubRes>,
    /// The main resource's properties (`[resource]` in text files).
    pub main: Vec<(String, Value)>,
    /// A scene's nodes, parents before children.
    pub nodes: Vec<NodeEntry>,
}

#[derive(Debug, Clone)]
pub(crate) struct ExtRes {
    pub id: String,
    pub kind: String,
    /// As written: usually `res://...`, sometimes relative to the file.
    pub path: String,
}

#[derive(Debug, Clone)]
pub(crate) struct SubRes {
    pub id: String,
    pub kind: String,
    pub props: Vec<(String, Value)>,
}

/// One `[node]` of a scene.
#[derive(Debug, Clone, Default)]
pub(crate) struct NodeEntry {
    pub name: String,
    /// The node's class; None for a node that only changes one an
    /// instanced scene brought (or the root of an inherited scene).
    pub class: Option<String>,
    /// The parent's path from the root ("." for the root itself); None for
    /// the root.
    pub parent: Option<String>,
    /// The scene this node instances, as an `Ext` (or `Sub`) value.
    pub instance: Option<Value>,
    pub props: Vec<(String, Value)>,
}

impl ResFile {
    pub fn ext(&self, id: &str) -> Option<&ExtRes> {
        self.ext.iter().find(|e| e.id == id)
    }

    pub fn main_prop(&self, key: &str) -> Option<&Value> {
        prop(&self.main, key)
    }
}

/// The last value given for `key`.
pub(crate) fn prop<'a>(props: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    props.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
}

//endregion
