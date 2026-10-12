//! Godot text resources (`.tres`) holding a material: Godot 4's
//! `StandardMaterial3D` and `ORMMaterial3D`, Godot 3's `SpatialMaterial`,
//! and a guess at `ShaderMaterial` parameters.  Any other resource type is
//! not a material and gets no thumbnail.
//!
//! Textures are `ext_resource` entries whose `res://` paths are relative to
//! the project folder (the nearest parent holding `project.godot`); paths
//! without `res://` are relative to the resource.  Godot colours are
//! sRGB-encoded, as its colour picker shows them.

use crate::realize::{Channel, FileRef, MatDesc, TexInput};
use crate::srgb_to_linear;
use slerp_scene::glam::{Vec2, Vec3};
use slerp_scene::{AlphaMode, Asset, Filter, Info, Limits, LoadError, UvTransform, Wrap};
use std::path::{Path, PathBuf};

pub(crate) fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Asset, LoadError> {
    let parsed = parse(path, bytes)?;
    let info = Info { format: format!("Godot {} (format {})", parsed.kind, parsed.format), ..Default::default() };
    Ok(crate::realize::material_asset(path, None, parsed.desc, info, None, limits))
}

/// Several material resources at once, decoding shared textures once (see
/// `crate::godot_materials`).
pub(crate) fn many(resources: &[(&Path, &[u8])], limits: &Limits) -> crate::GodotMaterials {
    let mut warnings = Vec::new();
    let parsed: Vec<Option<MatDesc>> = resources
        .iter()
        .map(|(path, bytes)| match parse(path, bytes) {
            Ok(p) => Some(p.desc),
            Err(LoadError::NotMine | LoadError::Unsupported(_)) => None,
            Err(e) => {
                warnings.push(format!("material in {}: {e}", path.display()));
                None
            }
        })
        .collect();
    let descs: Vec<&MatDesc> = parsed.iter().flatten().collect();
    let realized = crate::realize::realize(&descs, None, limits, false, &mut warnings);
    let mut done = realized.materials.into_iter();
    let materials = parsed.iter().map(|p| p.as_ref().and_then(|_| done.next())).collect();
    crate::GodotMaterials { materials, textures: realized.textures, warnings }
}

pub(crate) struct Parsed {
    pub kind: String,
    pub format: i64,
    pub desc: MatDesc,
}

/*┌─────────────────╮
| Text format     |
└─────────────────╯*/
//region Text format

#[derive(Clone, Debug, PartialEq)]
enum Value {
    Num(f64),
    Str(String),
    Bool(bool),
    Null,
    Ident(String),
    /// A constructor such as `Color(1, 1, 1, 1)` or `ExtResource("1_abc")`.
    Ctor(String, Vec<Value>),
    Array(Vec<Value>),
    Dict(Vec<(Value, Value)>),
}

impl Value {
    fn num(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            Value::Bool(b) => Some(*b as i64 as f64),
            _ => None,
        }
    }

    fn truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0,
            _ => false,
        }
    }

    /// The numbers of a constructor such as `Color(...)` or `Vector3(...)`.
    fn numbers(&self) -> Vec<f32> {
        match self {
            Value::Ctor(_, args) => args.iter().filter_map(|a| a.num()).map(|n| n as f32).collect(),
            _ => Vec::new(),
        }
    }

    /// The id an `ExtResource(...)` refers to, as text whatever its type.
    fn ext_id(&self) -> Option<String> {
        match self {
            Value::Ctor(name, args) if name == "ExtResource" => match args.first()? {
                Value::Str(s) => Some(s.clone()),
                Value::Num(n) => Some(format!("{n}")),
                _ => None,
            },
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
struct Section {
    tag: String,
    attrs: Vec<(String, Value)>,
    props: Vec<(String, Value)>,
}

impl Section {
    fn attr(&self, name: &str) -> Option<&Value> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    fn prop(&self, name: &str) -> Option<&Value> {
        self.props.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v)
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Punct(u8),
    Str(String),
    Num(f64),
    Ident(String),
    Eof,
}

struct Lexer<'a> {
    s: &'a [u8],
    pos: usize,
}

impl Lexer<'_> {
    fn next(&mut self) -> Result<Tok, LoadError> {
        loop {
            while self.pos < self.s.len() && self.s[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            // Comments run from `;` to the end of the line.
            if self.s.get(self.pos) == Some(&b';') {
                while self.pos < self.s.len() && self.s[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
        let Some(&c) = self.s.get(self.pos) else { return Ok(Tok::Eof) };
        match c {
            b'[' | b']' | b'(' | b')' | b'{' | b'}' | b',' | b':' | b'=' => {
                self.pos += 1;
                Ok(Tok::Punct(c))
            }
            // StringName (&"x") and NodePath (^"x") read as plain strings.
            b'&' | b'^' if self.s.get(self.pos + 1) == Some(&b'"') => {
                self.pos += 1;
                self.string()
            }
            b'"' => self.string(),
            b'-' | b'+' | b'.' | b'0'..=b'9' => {
                let start = self.pos;
                self.pos += 1;
                while self.pos < self.s.len()
                    && matches!(self.s[self.pos], b'0'..=b'9' | b'.' | b'e' | b'E' | b'-' | b'+')
                {
                    self.pos += 1;
                }
                let text = std::str::from_utf8(&self.s[start..self.pos]).unwrap_or("");
                text.parse().map(Tok::Num).map_err(|_| LoadError::invalid(format!("bad number {text}")))
            }
            _ if c.is_ascii_alphabetic() || c == b'_' => {
                let start = self.pos;
                while self.pos < self.s.len()
                    && (self.s[self.pos].is_ascii_alphanumeric()
                        || matches!(self.s[self.pos], b'_' | b'/' | b'.' | b'@'))
                {
                    self.pos += 1;
                }
                Ok(Tok::Ident(String::from_utf8_lossy(&self.s[start..self.pos]).into_owned()))
            }
            _ => Err(LoadError::invalid(format!("unexpected byte {c:#x} in Godot resource"))),
        }
    }

    fn string(&mut self) -> Result<Tok, LoadError> {
        self.pos += 1;
        let mut out = Vec::new();
        while let Some(&c) = self.s.get(self.pos) {
            self.pos += 1;
            match c {
                b'"' => return Ok(Tok::Str(String::from_utf8_lossy(&out).into_owned())),
                b'\\' => {
                    let e = *self.s.get(self.pos).ok_or_else(|| LoadError::invalid("unterminated string"))?;
                    self.pos += 1;
                    out.push(match e {
                        b'n' => b'\n',
                        b't' => b'\t',
                        other => other,
                    });
                }
                _ => out.push(c),
            }
        }
        Err(LoadError::invalid("unterminated string"))
    }
}

struct Parser<'a> {
    lex: Lexer<'a>,
    peeked: Option<Tok>,
}

impl Parser<'_> {
    fn peek(&mut self) -> Result<&Tok, LoadError> {
        if self.peeked.is_none() {
            self.peeked = Some(self.lex.next()?);
        }
        Ok(self.peeked.as_ref().unwrap_or(&Tok::Eof))
    }

    fn next(&mut self) -> Result<Tok, LoadError> {
        match self.peeked.take() {
            Some(t) => Ok(t),
            None => self.lex.next(),
        }
    }

    fn expect(&mut self, p: u8) -> Result<(), LoadError> {
        match self.next()? {
            Tok::Punct(c) if c == p => Ok(()),
            t => Err(LoadError::invalid(format!("expected '{}', found {t:?}", p as char))),
        }
    }

    fn sections(&mut self) -> Result<Vec<Section>, LoadError> {
        let mut out: Vec<Section> = Vec::new();
        loop {
            match self.next()? {
                Tok::Eof => return Ok(out),
                Tok::Punct(b'[') => {
                    if out.len() >= 100_000 {
                        return Err(LoadError::TooLarge("too many sections".into()));
                    }
                    out.push(self.header()?);
                }
                Tok::Ident(key) | Tok::Str(key) => {
                    self.expect(b'=')?;
                    let v = self.value(0)?;
                    if let Some(s) = out.last_mut() {
                        s.props.push((key, v));
                    }
                }
                t => return Err(LoadError::invalid(format!("unexpected {t:?} in Godot resource"))),
            }
        }
    }

    /// A section header after its `[`: a tag and `key=value` pairs.
    fn header(&mut self) -> Result<Section, LoadError> {
        let Tok::Ident(tag) = self.next()? else { return Err(LoadError::invalid("bad section header")) };
        let mut section = Section { tag, ..Default::default() };
        loop {
            match self.next()? {
                Tok::Punct(b']') => return Ok(section),
                Tok::Ident(key) => {
                    self.expect(b'=')?;
                    section.attrs.push((key, self.value(0)?));
                }
                t => return Err(LoadError::invalid(format!("unexpected {t:?} in section header"))),
            }
        }
    }

    fn value(&mut self, depth: u32) -> Result<Value, LoadError> {
        if depth > 64 {
            return Err(LoadError::invalid("values nested too deeply"));
        }
        match self.next()? {
            Tok::Num(n) => Ok(Value::Num(n)),
            Tok::Str(s) => Ok(Value::Str(s)),
            Tok::Ident(id) => {
                if self.peek()? == &Tok::Punct(b'(') {
                    self.next()?;
                    let args = self.list(b')', depth)?;
                    return Ok(Value::Ctor(id, args));
                }
                Ok(match id.as_str() {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    "null" => Value::Null,
                    "inf" => Value::Num(f64::INFINITY),
                    "inf_neg" => Value::Num(f64::NEG_INFINITY),
                    "nan" => Value::Num(f64::NAN),
                    _ => Value::Ident(id),
                })
            }
            Tok::Punct(b'[') => Ok(Value::Array(self.list(b']', depth)?)),
            Tok::Punct(b'{') => {
                let mut pairs = Vec::new();
                loop {
                    if self.peek()? == &Tok::Punct(b'}') {
                        self.next()?;
                        return Ok(Value::Dict(pairs));
                    }
                    let k = self.value(depth + 1)?;
                    self.expect(b':')?;
                    let v = self.value(depth + 1)?;
                    pairs.push((k, v));
                    match self.next()? {
                        Tok::Punct(b',') => {}
                        Tok::Punct(b'}') => return Ok(Value::Dict(pairs)),
                        t => return Err(LoadError::invalid(format!("unexpected {t:?} in dictionary"))),
                    }
                }
            }
            t => Err(LoadError::invalid(format!("unexpected {t:?} where a value belongs"))),
        }
    }

    fn list(&mut self, close: u8, depth: u32) -> Result<Vec<Value>, LoadError> {
        let mut out = Vec::new();
        loop {
            if self.peek()? == &Tok::Punct(close) {
                self.next()?;
                return Ok(out);
            }
            out.push(self.value(depth + 1)?);
            match self.next()? {
                Tok::Punct(b',') => {}
                Tok::Punct(c) if c == close => return Ok(out),
                t => return Err(LoadError::invalid(format!("unexpected {t:?} in list"))),
            }
        }
    }
}

//endregion

/*┌─────────────────╮
| Materials       |
└─────────────────╯*/
//region Materials

/// Godot's texture channel numbers.
fn channel(v: Option<&Value>, default: Channel) -> Channel {
    match v.and_then(Value::num).map(|n| n as i64) {
        Some(0) => Channel::R,
        Some(1) => Channel::G,
        Some(2) => Channel::B,
        Some(3) => Channel::A,
        Some(4) => Channel::Luminance,
        _ => default,
    }
}

pub(crate) fn parse(path: &Path, bytes: &[u8]) -> Result<Parsed, LoadError> {
    let mut p = Parser { lex: Lexer { s: crate::strip_bom(bytes), pos: 0 }, peeked: None };
    // 1. Check the type first, so other resources (tile sets, meshes) are
    // turned away without reading the rest.
    if p.next()? != Tok::Punct(b'[') {
        return Err(LoadError::NotMine);
    }
    let header = p.header()?;
    if header.tag != "gd_resource" {
        return Err(LoadError::NotMine);
    }
    let kind = match header.attr("type") {
        Some(Value::Str(t)) => t.clone(),
        _ => return Err(LoadError::invalid("Godot resource without a type")),
    };
    let format = header.attr("format").and_then(Value::num).unwrap_or(3.0) as i64;
    if !matches!(kind.as_str(), "StandardMaterial3D" | "ORMMaterial3D" | "SpatialMaterial" | "ShaderMaterial") {
        return Err(LoadError::Unsupported(format!("Godot {kind} resource")));
    }

    // 2. Read the rest.
    let mut sections = vec![header];
    sections.extend(p.sections()?);
    let res =
        sections.iter().find(|s| s.tag == "resource").ok_or_else(|| LoadError::invalid("no [resource] section"))?;
    let ctx = Ctx { path, sections: &sections, res, project: std::cell::OnceCell::new() };
    let name = match res.prop("resource_name") {
        Some(Value::Str(n)) => n.clone(),
        _ => path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
    };
    let desc = if kind == "ShaderMaterial" { ctx.shader_material(&name) } else { ctx.standard(&name, &kind) };
    Ok(Parsed { kind, format, desc })
}

struct Ctx<'a> {
    path: &'a Path,
    sections: &'a [Section],
    res: &'a Section,
    /// The folder holding `project.godot`, found on first use.
    project: std::cell::OnceCell<Option<PathBuf>>,
}

impl Ctx<'_> {
    fn prop(&self, name: &str) -> Option<&Value> {
        self.res.prop(name)
    }

    fn num(&self, name: &str, default: f32) -> f32 {
        self.prop(name).and_then(Value::num).map_or(default, |n| n as f32)
    }

    fn flag(&self, name: &str) -> bool {
        self.prop(name).is_some_and(Value::truthy)
    }

    /// A colour property, converted from sRGB to linear.
    fn color(&self, name: &str, default: [f32; 4]) -> [f32; 4] {
        let v = self.prop(name).map(Value::numbers).unwrap_or_default();
        if v.len() < 3 {
            return default;
        }
        [srgb_to_linear(v[0]), srgb_to_linear(v[1]), srgb_to_linear(v[2]), v.get(3).copied().unwrap_or(1.0)]
    }

    fn project_dir(&self) -> Option<&PathBuf> {
        self.project
            .get_or_init(|| {
                let mut dir = self.path.parent()?.to_path_buf();
                for _ in 0..64 {
                    if dir.join("project.godot").is_file() {
                        return Some(dir);
                    }
                    if !dir.pop() {
                        break;
                    }
                }
                None
            })
            .as_ref()
    }

    /// The texture an `ExtResource` value names.
    fn texture(&self, v: Option<&Value>, ch: Channel) -> Option<TexInput> {
        let id = v?.ext_id()?;
        let ext = self.sections.iter().find(|s| {
            s.tag == "ext_resource"
                && match s.attr("id") {
                    Some(Value::Str(s)) => *s == id,
                    Some(Value::Num(n)) => format!("{n}") == id,
                    _ => false,
                }
        })?;
        let Some(Value::Str(res_path)) = ext.attr("path") else { return None };
        let dir = self.path.parent().unwrap_or(Path::new(""));
        let file = match res_path.strip_prefix("res://") {
            Some(rest) => match self.project_dir() {
                Some(root) => root.join(rest),
                // Outside its project the folder layout is unknown; try
                // next to the resource.
                None => dir.join(Path::new(rest).file_name()?),
            },
            None => dir.join(res_path),
        };
        let mut t = TexInput::new(FileRef::Disk(file), res_path.clone(), ch);
        let scale = self.prop("uv1_scale").map(Value::numbers).unwrap_or_default();
        let offset = self.prop("uv1_offset").map(Value::numbers).unwrap_or_default();
        let scale = if scale.len() >= 2 { Vec2::new(scale[0], scale[1]) } else { Vec2::ONE };
        let offset = if offset.len() >= 2 { Vec2::new(offset[0], offset[1]) } else { Vec2::ZERO };
        if scale != Vec2::ONE || offset != Vec2::ZERO {
            t.transform = Some(UvTransform { offset, rotation: 0.0, scale });
        }
        // Godot's texture_filter: 0 and 2 are nearest, the rest linear.
        if matches!(self.prop("texture_filter").and_then(Value::num), Some(n) if n == 0.0 || n == 2.0) {
            t.sampler.mag = Filter::Nearest;
            t.sampler.min = Filter::Nearest;
        }
        if self.prop("texture_repeat").is_some_and(|v| !v.truthy()) {
            t.sampler.wrap_u = Wrap::ClampToEdge;
            t.sampler.wrap_v = Wrap::ClampToEdge;
        }
        Some(t)
    }

    fn standard(&self, name: &str, kind: &str) -> MatDesc {
        let mut d = MatDesc::new(name, kind);
        let godot3 = kind == "SpatialMaterial";
        let m = &mut d.material;

        // 1. Colour, metal and roughness.
        let albedo = self.color("albedo_color", [1.0; 4]);
        m.base_color = albedo.into();
        d.base_color = self.texture(self.prop("albedo_texture"), Channel::Rgb);
        d.material.metallic = self.num("metallic", 0.0);
        d.material.roughness = self.num("roughness", 1.0);
        d.material.specular = (self.num("metallic_specular", 0.5) * 2.0).clamp(0.0, 1.0);
        if kind == "ORMMaterial3D" {
            let orm = self.prop("orm_texture");
            d.roughness = self.texture(orm, Channel::G);
            d.metallic = self.texture(orm, Channel::B);
            if self.flag("ao_enabled") {
                d.occlusion = self.texture(orm, Channel::R);
            }
        } else {
            d.metallic =
                self.texture(self.prop("metallic_texture"), channel(self.prop("metallic_texture_channel"), Channel::R));
            d.roughness = self
                .texture(self.prop("roughness_texture"), channel(self.prop("roughness_texture_channel"), Channel::R));
            if self.flag("ao_enabled") {
                d.occlusion =
                    self.texture(self.prop("ao_texture"), channel(self.prop("ao_texture_channel"), Channel::R));
            }
        }

        // 2. Normals, emission and height.
        if self.flag("normal_enabled") {
            d.normal = self.texture(self.prop("normal_texture"), Channel::Rgb);
            d.material.normal_scale = self.num("normal_scale", 1.0);
        }
        if self.flag("emission_enabled") {
            let e = self.color("emission", [0.0, 0.0, 0.0, 1.0]);
            let energy = self.num(if godot3 { "emission_energy" } else { "emission_energy_multiplier" }, 1.0);
            d.material.emissive = Vec3::new(e[0], e[1], e[2]) * energy;
            d.emissive = self.texture(self.prop("emission_texture"), Channel::Rgb);
        }
        if godot3 {
            if self.flag("depth_enabled") {
                d.height = self.texture(self.prop("depth_texture"), Channel::R);
                d.material.height_scale = self.num("depth_scale", 0.05);
            }
        } else if self.flag("heightmap_enabled") {
            d.height = self.texture(self.prop("heightmap_texture"), Channel::R);
            // Godot tunes heightmap_scale so 5 lifts white 5 cm above black.
            d.material.height_scale = self.num("heightmap_scale", 5.0) * 0.01;
        }
        if self.flag("clearcoat_enabled") {
            d.material.clearcoat = self.num("clearcoat", 1.0);
            d.material.clearcoat_roughness = self.num("clearcoat_roughness", 0.5);
        }

        // 3. Transparency, culling and shading.
        let (transparency, scissor, threshold, cull, unshaded) = if godot3 {
            let t = if self.flag("flags_transparent") { 1 } else { 0 };
            let s = self.flag("params_use_alpha_scissor");
            let th = self.num("params_alpha_scissor_threshold", 0.5);
            (t, s, th, self.num("params_cull_mode", 0.0) as i64, self.flag("flags_unshaded"))
        } else {
            let t = self.num("transparency", 0.0) as i64;
            let th = self.num("alpha_scissor_threshold", 0.5);
            (t, t == 2, th, self.num("cull_mode", 0.0) as i64, self.num("shading_mode", 1.0) == 0.0)
        };
        d.material.alpha_mode = match transparency {
            _ if scissor => AlphaMode::Mask(threshold),
            1 | 4 => AlphaMode::Blend,
            3 => AlphaMode::Mask(0.5),
            _ => AlphaMode::Opaque,
        };
        d.material.double_sided = cull == 2;
        d.material.unlit = unshaded;
        d
    }

    /// A custom shader can't be run, so its parameters are matched by name:
    /// a colour texture and colour named like albedo, and a normal map.
    fn shader_material(&self, name: &str) -> MatDesc {
        let mut d = MatDesc::new(name, "ShaderMaterial (approximated)");
        for (key, value) in &self.res.props {
            let Some(param) = key.strip_prefix("shader_parameter/").or_else(|| key.strip_prefix("shader_param/"))
            else {
                continue;
            };
            let p = param.to_ascii_lowercase();
            let colourish =
                ["albedo", "diffuse", "base_color", "basecolor", "main_tex", "color"].iter().any(|n| p.contains(n));
            if value.ext_id().is_some() {
                if p.contains("normal") && d.normal.is_none() {
                    d.normal = self.texture(Some(value), Channel::Rgb);
                } else if colourish && d.base_color.is_none() {
                    d.base_color = self.texture(Some(value), Channel::Rgb);
                }
            } else if colourish && matches!(value, Value::Ctor(c, _) if c == "Color") {
                let v = value.numbers();
                if v.len() >= 3 {
                    let a = v.get(3).copied().unwrap_or(1.0);
                    d.material.base_color =
                        [srgb_to_linear(v[0]), srgb_to_linear(v[1]), srgb_to_linear(v[2]), a].into();
                }
            }
        }
        d
    }
}

//endregion
