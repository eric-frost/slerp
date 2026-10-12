//! DXF entities as plain data: what a LINE, a HATCH or a MTEXT holds, read
//! from the group codes with the defaults the format specifies.  Nothing
//! here draws; `emit` turns these into geometry.
//!
//! Every parser walks the pairs of one entity once and tolerates what real
//! files do: missing groups, groups out of order, counts that don't match
//! what follows.  Counts are never trusted to size a buffer.

use super::encoding::Decoder;
use super::pairs::Pair;
use super::tables::Layers;
use glam::{DVec2, DVec3};

/// An entity's colour as the file gives it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Color {
    ByLayer,
    /// The colour of the block reference the entity is inside.
    ByBlock,
    /// AutoCAD colour index 1 to 255.
    Aci(u8),
    /// 0xRRGGBB.
    True(u32),
}

/// Everything entities need to turn names into indices.
pub struct Env<'e> {
    pub dec: Decoder,
    pub layers: &'e mut Layers,
}

impl Env<'_> {
    pub fn layer(&mut self, name: &[u8]) -> u32 {
        let dec = self.dec;
        self.layers.id(name, |b| dec.decode(b).into_owned())
    }

    pub fn text(&self, bytes: &[u8]) -> String {
        self.dec.decode(bytes).into_owned()
    }
}

#[derive(Clone, Debug)]
pub struct Entity {
    pub kind: Kind,
    pub layer: u32,
    pub color: Color,
    /// Group 60: invisible.
    pub hidden: bool,
    /// Group 67: belongs to a paper-space layout rather than model space.
    pub paper: bool,
    /// The extrusion direction (the normal of the plane the entity's own
    /// coordinates lie in), unit length.
    pub normal: DVec3,
}

#[derive(Clone, Debug)]
pub enum Kind {
    Line(DVec3, DVec3),
    Point(DVec3),
    Circle {
        c: DVec3,
        r: f64,
    },
    /// Angles in radians, counterclockwise from `a0` to `a1`.
    Arc {
        c: DVec3,
        r: f64,
        a0: f64,
        a1: f64,
    },
    Ellipse {
        c: DVec3,
        major: DVec3,
        ratio: f64,
        t0: f64,
        t1: f64,
    },
    Spline(Box<Spline>),
    LwPoly(Box<LwPoly>),
    Poly(Box<Poly>),
    /// SOLID and TRACE: four corners in the order 1, 2, 4, 3.
    Solid([DVec3; 4]),
    /// 3DFACE: four corners.
    Face([DVec3; 4]),
    Hatch(Box<Hatch>),
    Text(Box<Text>),
    MText(Box<MText>),
    Insert(Box<Insert>),
    /// A DIMENSION is drawn by an anonymous block of its own.
    Dimension {
        block: String,
    },
    Leader {
        pts: Vec<DVec3>,
        arrow: bool,
    },
    /// RAY and XLINE: from `origin` along `dir`, and for XLINE also back.
    Ray {
        origin: DVec3,
        dir: DVec3,
        both: bool,
    },
    Mesh(Box<MeshData>),
}

#[derive(Clone, Debug, Default)]
pub struct Spline {
    pub degree: i32,
    pub flags: i32,
    pub knots: Vec<f64>,
    pub weights: Vec<f64>,
    pub ctrl: Vec<DVec3>,
    pub fit: Vec<DVec3>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LwVertex {
    pub p: DVec2,
    pub start_width: f64,
    pub end_width: f64,
    pub bulge: f64,
}

#[derive(Clone, Debug, Default)]
pub struct LwPoly {
    pub verts: Vec<LwVertex>,
    pub closed: bool,
    pub width: f64,
    pub elevation: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PolyVertex {
    pub p: DVec3,
    pub start_width: f64,
    pub end_width: f64,
    pub bulge: f64,
    pub flags: i32,
    /// Polyface faces: the 1-based vertex numbers, negative for hidden edges.
    pub face: [i32; 4],
}

/// The old POLYLINE: 2D (with bulges and widths), 3D, a polygon mesh of
/// M by N vertices, or a polyface mesh.
#[derive(Clone, Debug, Default)]
pub struct Poly {
    pub flags: i32,
    pub verts: Vec<PolyVertex>,
    pub elevation: f64,
    pub start_width: f64,
    pub end_width: f64,
    pub m: i32,
    pub n: i32,
    /// Group 66: vertices follow.
    pub has_vertices: bool,
}

impl Poly {
    pub const CLOSED: i32 = 1;
    pub const POLYLINE_3D: i32 = 8;
    pub const MESH: i32 = 16;
    pub const MESH_CLOSED_N: i32 = 32;
    pub const POLYFACE: i32 = 64;
}

#[derive(Clone, Debug, Default)]
pub struct Hatch {
    pub solid: bool,
    /// 0 normal, 1 outermost only, 2 ignore islands.
    pub style: i32,
    pub elevation: f64,
    pub loops: Vec<HatchLoop>,
    pub pattern: String,
    /// A gradient fill (drawn as a solid in its first colour).
    pub gradient: bool,
}

#[derive(Clone, Debug)]
pub enum HatchLoop {
    Poly { verts: Vec<(DVec2, f64)>, closed: bool },
    Edges(Vec<HatchEdge>),
}

#[derive(Clone, Debug)]
pub enum HatchEdge {
    Line(DVec2, DVec2),
    /// Angles in degrees.
    Arc {
        c: DVec2,
        r: f64,
        a0: f64,
        a1: f64,
        ccw: bool,
    },
    /// Angles in degrees, as parameters of the ellipse.
    Ellipse {
        c: DVec2,
        major: DVec2,
        ratio: f64,
        a0: f64,
        a1: f64,
        ccw: bool,
    },
    Spline {
        degree: i32,
        knots: Vec<f64>,
        ctrl: Vec<(DVec2, f64)>,
        fit: Vec<DVec2>,
    },
}

/// TEXT, and ATTRIB and ATTDEF, which are texts with a tag.
#[derive(Clone, Debug, Default)]
pub struct Text {
    pub p: DVec3,
    pub p2: DVec3,
    pub height: f64,
    pub text: String,
    /// Radians.
    pub rotation: f64,
    pub x_scale: f64,
    /// Radians.
    pub oblique: f64,
    pub style: Vec<u8>,
    pub gen_flags: i32,
    pub halign: i32,
    pub valign: i32,
    /// 0 for TEXT, 1 for ATTRIB, 2 for ATTDEF.
    pub attribute: u8,
    pub attr_flags: i32,
}

#[derive(Clone, Debug, Default)]
pub struct MText {
    pub p: DVec3,
    pub height: f64,
    /// The width of the box the text wraps in; 0 for no wrapping.
    pub width: f64,
    pub attach: i32,
    pub text: String,
    pub x_dir: Option<DVec3>,
    pub rotation: f64,
    pub style: Vec<u8>,
    pub spacing: f64,
}

#[derive(Clone, Debug)]
pub struct Insert {
    pub block: Vec<u8>,
    pub p: DVec3,
    pub scale: DVec3,
    pub rotation: f64,
    pub cols: i32,
    pub rows: i32,
    pub col_spacing: f64,
    pub row_spacing: f64,
    pub has_attribs: bool,
    pub attribs: Vec<Entity>,
}

#[derive(Clone, Debug, Default)]
pub struct MeshData {
    pub verts: Vec<DVec3>,
    /// Faces as a count followed by that many vertex indices, repeated.
    pub faces: Vec<u32>,
}

/// What reading one entity's groups gave.
pub enum Parsed {
    Entity(Entity),
    /// A kind this reader doesn't draw, by name (for the warning).
    Skipped,
}

/// The groups every entity has.
struct Common {
    layer: u32,
    aci: Color,
    true_color: Option<u32>,
    hidden: bool,
    paper: bool,
    normal: DVec3,
}

impl Common {
    fn new() -> Common {
        Common { layer: 0, aci: Color::ByLayer, true_color: None, hidden: false, paper: false, normal: DVec3::Z }
    }

    /// Takes the pair if it is a common group.
    fn apply(&mut self, p: &Pair, env: &mut Env) -> bool {
        match p.code {
            8 => self.layer = env.layer(p.bytes()),
            62 => {
                self.aci = match p.i32() {
                    256 => Color::ByLayer,
                    0 => Color::ByBlock,
                    n => Color::Aci(n.unsigned_abs().clamp(1, 255) as u8),
                }
            }
            420 => self.true_color = Some(p.i64() as u32 & 0xFF_FFFF),
            60 => self.hidden = p.i32() != 0,
            67 => self.paper = p.i32() == 1,
            210 => self.normal.x = p.f64(),
            220 => self.normal.y = p.f64(),
            230 => self.normal.z = p.f64(),
            _ => return false,
        }
        true
    }

    fn finish(self, kind: Kind) -> Entity {
        let normal = self.normal.try_normalize().unwrap_or(DVec3::Z);
        Entity {
            kind,
            layer: self.layer,
            color: self.true_color.map_or(self.aci, Color::True),
            hidden: self.hidden,
            paper: self.paper,
            normal,
        }
    }
}

/// Sets the x, y or z of `p` when `code` is `base`, `base + 10` or
/// `base + 20`.  Returns whether it was one.
pub fn set_axis(p: &mut DVec3, base: i32, code: i32, v: &Pair) -> bool {
    match code - base {
        0 => p.x = v.f64(),
        10 => p.y = v.f64(),
        20 => p.z = v.f64(),
        _ => return false,
    }
    true
}

/// Entity kinds that are drawn from ACIS solid data, which this reader
/// doesn't evaluate.
pub fn is_acis(name: &[u8]) -> bool {
    matches!(
        name,
        b"3DSOLID"
            | b"REGION"
            | b"BODY"
            | b"SURFACE"
            | b"PLANESURFACE"
            | b"EXTRUDEDSURFACE"
            | b"LOFTEDSURFACE"
            | b"REVOLVEDSURFACE"
            | b"SWEPTSURFACE"
            | b"NURBSURFACE"
    )
}

/// Reads the groups of the entity called `name`.
pub fn parse(name: &[u8], pairs: &[Pair], env: &mut Env) -> Parsed {
    // From R2018 on an MTEXT (and an ATTRIB holding one) ends with an
    // embedded object that reuses group codes for the column layout:
    // group 101 starts it, and none of it describes the entity itself.
    let pairs = match pairs.iter().position(|p| p.code == 101) {
        Some(end) => &pairs[..end],
        None => pairs,
    };
    let mut c = Common::new();
    let kind = match name {
        b"LINE" => {
            let (mut a, mut b) = (DVec3::ZERO, DVec3::ZERO);
            for p in pairs {
                if !(set_axis(&mut a, 10, p.code, p) || set_axis(&mut b, 11, p.code, p)) {
                    c.apply(p, env);
                }
            }
            Kind::Line(a, b)
        }
        b"POINT" => {
            let mut a = DVec3::ZERO;
            for p in pairs {
                if !set_axis(&mut a, 10, p.code, p) {
                    c.apply(p, env);
                }
            }
            Kind::Point(a)
        }
        b"CIRCLE" | b"ARC" => {
            let (mut centre, mut r, mut a0, mut a1) = (DVec3::ZERO, 0.0, 0.0, 360.0);
            for p in pairs {
                if set_axis(&mut centre, 10, p.code, p) {
                } else if p.code == 40 {
                    r = p.f64();
                } else if p.code == 50 {
                    a0 = p.f64();
                } else if p.code == 51 {
                    a1 = p.f64();
                } else {
                    c.apply(p, env);
                }
            }
            if name == b"CIRCLE" {
                Kind::Circle { c: centre, r }
            } else {
                Kind::Arc { c: centre, r, a0: a0.to_radians(), a1: a1.to_radians() }
            }
        }
        b"ELLIPSE" => {
            let (mut centre, mut major, mut ratio, mut t0, mut t1) =
                (DVec3::ZERO, DVec3::X, 1.0, 0.0, std::f64::consts::TAU);
            for p in pairs {
                if set_axis(&mut centre, 10, p.code, p) || set_axis(&mut major, 11, p.code, p) {
                } else if p.code == 40 {
                    ratio = p.f64();
                } else if p.code == 41 {
                    t0 = p.f64();
                } else if p.code == 42 {
                    t1 = p.f64();
                } else {
                    c.apply(p, env);
                }
            }
            Kind::Ellipse { c: centre, major, ratio, t0, t1 }
        }
        b"SPLINE" | b"HELIX" => Kind::Spline(Box::new(parse_spline(pairs, &mut c, env))),
        b"LWPOLYLINE" => Kind::LwPoly(Box::new(parse_lwpolyline(pairs, &mut c, env))),
        b"POLYLINE" => {
            let mut poly = Poly::default();
            for p in pairs {
                match p.code {
                    66 => poly.has_vertices = p.i32() != 0,
                    70 => poly.flags = p.i32(),
                    30 => poly.elevation = p.f64(),
                    40 => poly.start_width = p.f64(),
                    41 => poly.end_width = p.f64(),
                    71 => poly.m = p.i32(),
                    72 => poly.n = p.i32(),
                    _ => {
                        c.apply(p, env);
                    }
                }
            }
            // The default is that vertices follow, as in R12 files that
            // omit group 66.
            Kind::Poly(Box::new(poly))
        }
        b"SOLID" | b"TRACE" => {
            let mut pts = [DVec3::ZERO; 4];
            let mut seen = [false; 4];
            for p in pairs {
                let mut used = false;
                for (i, q) in pts.iter_mut().enumerate() {
                    if set_axis(q, 10 + i as i32, p.code, p) {
                        seen[i] = true;
                        used = true;
                        break;
                    }
                }
                if !used {
                    c.apply(p, env);
                }
            }
            // A triangle repeats its third corner as the fourth.
            if !seen[3] {
                pts[3] = pts[2];
            }
            Kind::Solid(pts)
        }
        b"3DFACE" => {
            let mut pts = [DVec3::ZERO; 4];
            let mut seen = [false; 4];
            for p in pairs {
                let mut used = false;
                for (i, q) in pts.iter_mut().enumerate() {
                    if set_axis(q, 10 + i as i32, p.code, p) {
                        seen[i] = true;
                        used = true;
                        break;
                    }
                }
                if !used {
                    c.apply(p, env);
                }
            }
            if !seen[3] {
                pts[3] = pts[2];
            }
            Kind::Face(pts)
        }
        b"HATCH" => Kind::Hatch(Box::new(parse_hatch(pairs, &mut c, env))),
        b"TEXT" | b"ATTRIB" | b"ATTDEF" => {
            let attribute = match name {
                b"TEXT" => 0,
                b"ATTRIB" => 1,
                _ => 2,
            };
            Kind::Text(Box::new(parse_text(pairs, &mut c, env, attribute)))
        }
        b"MTEXT" => Kind::MText(Box::new(parse_mtext(pairs, &mut c, env))),
        b"INSERT" => {
            let mut ins = Insert {
                block: Vec::new(),
                p: DVec3::ZERO,
                scale: DVec3::ONE,
                rotation: 0.0,
                cols: 1,
                rows: 1,
                col_spacing: 0.0,
                row_spacing: 0.0,
                has_attribs: false,
                attribs: Vec::new(),
            };
            for p in pairs {
                if set_axis(&mut ins.p, 10, p.code, p) {
                    continue;
                }
                match p.code {
                    2 => ins.block = p.bytes().trim_ascii().to_vec(),
                    41 => ins.scale.x = p.f64(),
                    42 => ins.scale.y = p.f64(),
                    43 => ins.scale.z = p.f64(),
                    50 => ins.rotation = p.f64().to_radians(),
                    70 => ins.cols = p.i32(),
                    71 => ins.rows = p.i32(),
                    44 => ins.col_spacing = p.f64(),
                    45 => ins.row_spacing = p.f64(),
                    66 => ins.has_attribs = p.i32() != 0,
                    _ => {
                        c.apply(p, env);
                    }
                }
            }
            Kind::Insert(Box::new(ins))
        }
        b"DIMENSION" => {
            let mut block = String::new();
            for p in pairs {
                if p.code == 2 {
                    block = env.text(p.bytes());
                } else {
                    c.apply(p, env);
                }
            }
            Kind::Dimension { block }
        }
        b"LEADER" => {
            let mut pts: Vec<DVec3> = Vec::new();
            let mut arrow = true;
            for p in pairs {
                match p.code {
                    10 => pts.push(DVec3::new(p.f64(), 0.0, 0.0)),
                    20 => {
                        if let Some(q) = pts.last_mut() {
                            q.y = p.f64()
                        }
                    }
                    30 => {
                        if let Some(q) = pts.last_mut() {
                            q.z = p.f64()
                        }
                    }
                    71 => arrow = p.i32() != 0,
                    _ => {
                        c.apply(p, env);
                    }
                }
            }
            Kind::Leader { pts, arrow }
        }
        b"RAY" | b"XLINE" => {
            let (mut origin, mut dir) = (DVec3::ZERO, DVec3::X);
            for p in pairs {
                if !(set_axis(&mut origin, 10, p.code, p) || set_axis(&mut dir, 11, p.code, p)) {
                    c.apply(p, env);
                }
            }
            Kind::Ray { origin, dir, both: name == b"XLINE" }
        }
        b"MESH" => Kind::Mesh(Box::new(parse_mesh(pairs, &mut c, env))),
        _ => return Parsed::Skipped,
    };
    Parsed::Entity(c.finish(kind))
}

/// One VERTEX record of a POLYLINE.
pub fn parse_vertex(pairs: &[Pair]) -> PolyVertex {
    let mut v = PolyVertex::default();
    for p in pairs {
        if set_axis(&mut v.p, 10, p.code, p) {
            continue;
        }
        match p.code {
            40 => v.start_width = p.f64(),
            41 => v.end_width = p.f64(),
            42 => v.bulge = p.f64(),
            70 => v.flags = p.i32(),
            71 => v.face[0] = p.i32(),
            72 => v.face[1] = p.i32(),
            73 => v.face[2] = p.i32(),
            74 => v.face[3] = p.i32(),
            _ => {}
        }
    }
    v
}

fn parse_spline(pairs: &[Pair], c: &mut Common, env: &mut Env) -> Spline {
    let mut s = Spline::default();
    for p in pairs {
        match p.code {
            70 => s.flags = p.i32(),
            71 => s.degree = p.i32(),
            40 => s.knots.push(p.f64()),
            41 => s.weights.push(p.f64()),
            10 => s.ctrl.push(DVec3::new(p.f64(), 0.0, 0.0)),
            20 => {
                if let Some(q) = s.ctrl.last_mut() {
                    q.y = p.f64()
                }
            }
            30 => {
                if let Some(q) = s.ctrl.last_mut() {
                    q.z = p.f64()
                }
            }
            11 => s.fit.push(DVec3::new(p.f64(), 0.0, 0.0)),
            21 => {
                if let Some(q) = s.fit.last_mut() {
                    q.y = p.f64()
                }
            }
            31 => {
                if let Some(q) = s.fit.last_mut() {
                    q.z = p.f64()
                }
            }
            _ => {
                c.apply(p, env);
            }
        }
    }
    s
}

fn parse_lwpolyline(pairs: &[Pair], c: &mut Common, env: &mut Env) -> LwPoly {
    let mut w = LwPoly::default();
    for p in pairs {
        match p.code {
            70 => w.closed = p.i32() & 1 != 0,
            43 => w.width = p.f64(),
            38 => w.elevation = p.f64(),
            10 => w.verts.push(LwVertex { p: DVec2::new(p.f64(), 0.0), ..Default::default() }),
            20 => {
                if let Some(v) = w.verts.last_mut() {
                    v.p.y = p.f64()
                }
            }
            40 => {
                if let Some(v) = w.verts.last_mut() {
                    v.start_width = p.f64()
                }
            }
            41 => {
                if let Some(v) = w.verts.last_mut() {
                    v.end_width = p.f64()
                }
            }
            42 => {
                if let Some(v) = w.verts.last_mut() {
                    v.bulge = p.f64()
                }
            }
            _ => {
                c.apply(p, env);
            }
        }
    }
    w
}

fn parse_text(pairs: &[Pair], c: &mut Common, env: &mut Env, attribute: u8) -> Text {
    let mut t = Text { x_scale: 1.0, attribute, ..Default::default() };
    let mut text_seen = false;
    for p in pairs {
        if set_axis(&mut t.p, 10, p.code, p) || set_axis(&mut t.p2, 11, p.code, p) {
            continue;
        }
        match p.code {
            40 => t.height = p.f64(),
            1 => {
                t.text = env.text(p.bytes());
                text_seen = true;
            }
            50 => t.rotation = p.f64().to_radians(),
            41 => t.x_scale = p.f64(),
            51 => t.oblique = p.f64().to_radians(),
            7 => t.style = p.bytes().to_vec(),
            71 => t.gen_flags = p.i32(),
            72 => t.halign = p.i32(),
            // ATTRIB and ATTDEF keep their vertical alignment in 74 (73 is
            // the field length there).
            73 if attribute == 0 => t.valign = p.i32(),
            74 if attribute != 0 => t.valign = p.i32(),
            70 if attribute != 0 => t.attr_flags = p.i32(),
            // An attribute definition's tag is shown when it has no value.
            2 if attribute == 2 && !text_seen => t.text = env.text(p.bytes()),
            _ => {
                c.apply(p, env);
            }
        }
    }
    t
}

fn parse_mtext(pairs: &[Pair], c: &mut Common, env: &mut Env) -> MText {
    let mut t = MText { spacing: 1.0, ..Default::default() };
    let mut x = DVec3::ZERO;
    let mut have_x = false;
    let mut chunks = String::new();
    for p in pairs {
        if set_axis(&mut t.p, 10, p.code, p) {
            continue;
        }
        if p.code == 11 || p.code == 21 || p.code == 31 {
            set_axis(&mut x, 11, p.code, p);
            have_x = true;
            continue;
        }
        match p.code {
            40 => t.height = p.f64(),
            41 => t.width = p.f64(),
            71 => t.attach = p.i32(),
            // The text comes in chunks of 250 characters: groups 3 first,
            // and group 1 for the last.
            3 | 1 => chunks.push_str(&env.text(p.bytes())),
            50 => t.rotation = p.f64(),
            7 => t.style = p.bytes().to_vec(),
            44 => t.spacing = p.f64(),
            _ => {
                c.apply(p, env);
            }
        }
    }
    t.text = chunks;
    if have_x && x.length_squared() > 0.0 {
        t.x_dir = Some(x);
    }
    t
}

fn parse_mesh(pairs: &[Pair], c: &mut Common, env: &mut Env) -> MeshData {
    let mut m = MeshData::default();
    // The sections come one after another, each introduced by its count:
    // 92 vertices (10/20/30), 93 the size of the face list (90 values).
    #[derive(PartialEq)]
    enum Part {
        Head,
        Verts,
        Faces,
        Rest,
    }
    let mut part = Part::Head;
    let mut want_faces = 0usize;
    for p in pairs {
        match (p.code, &part) {
            (92, Part::Head) => part = Part::Verts,
            (10, Part::Verts) => m.verts.push(DVec3::new(p.f64(), 0.0, 0.0)),
            (20, Part::Verts) => {
                if let Some(v) = m.verts.last_mut() {
                    v.y = p.f64()
                }
            }
            (30, Part::Verts) => {
                if let Some(v) = m.verts.last_mut() {
                    v.z = p.f64()
                }
            }
            (93, Part::Verts | Part::Head) => {
                part = Part::Faces;
                want_faces = p.i64().clamp(0, 50_000_000) as usize;
            }
            (90, Part::Faces) => {
                if m.faces.len() < want_faces {
                    m.faces.push(p.i64().clamp(0, u32::MAX as i64) as u32);
                }
                if m.faces.len() >= want_faces {
                    part = Part::Rest;
                }
            }
            (94, _) => part = Part::Rest,
            _ => {
                c.apply(p, env);
            }
        }
    }
    m
}

//region Hatch

fn parse_hatch(pairs: &[Pair], c: &mut Common, env: &mut Env) -> Hatch {
    let mut h = Hatch::default();
    let mut i = 0;
    let mut loops_done = false;
    while i < pairs.len() {
        let p = &pairs[i];
        match p.code {
            91 if !loops_done => {
                let count = p.i64().clamp(0, 1_000_000) as usize;
                i += 1;
                for _ in 0..count {
                    if i >= pairs.len() || pairs[i].code != 92 {
                        break;
                    }
                    let (l, next) = parse_hatch_loop(pairs, i);
                    h.loops.push(l);
                    i = next;
                }
                loops_done = true;
                continue;
            }
            30 if !loops_done => h.elevation = p.f64(),
            2 => h.pattern = env.text(p.bytes()),
            70 if !loops_done => h.solid = p.i32() == 1,
            75 => h.style = p.i32(),
            450 => h.gradient = p.i32() == 1,
            _ => {
                if p.code != 10 && p.code != 20 {
                    c.apply(p, env);
                }
            }
        }
        i += 1;
    }
    h
}

/// Reads one boundary path starting at the group 92 at `i`; returns it and
/// the index after it.
fn parse_hatch_loop(pairs: &[Pair], mut i: usize) -> (HatchLoop, usize) {
    let flags = pairs[i].i32();
    i += 1;
    let mut result;
    if flags & 2 != 0 {
        // A polyline path: 72 has-bulge, 73 closed, 93 count, vertices.
        let (mut has_bulge, mut closed) = (false, true);
        let mut count = 0usize;
        while i < pairs.len() {
            match pairs[i].code {
                72 => has_bulge = pairs[i].i32() != 0,
                73 => closed = pairs[i].i32() != 0,
                93 => {
                    count = pairs[i].i64().clamp(0, 50_000_000) as usize;
                    i += 1;
                    break;
                }
                _ => break,
            }
            i += 1;
        }
        let _ = has_bulge;
        let mut verts: Vec<(DVec2, f64)> = Vec::new();
        while i < pairs.len() && verts.len() <= count {
            match pairs[i].code {
                10 => {
                    if verts.len() == count {
                        break;
                    }
                    verts.push((DVec2::new(pairs[i].f64(), 0.0), 0.0));
                }
                20 => {
                    if let Some(v) = verts.last_mut() {
                        v.0.y = pairs[i].f64()
                    }
                }
                42 => {
                    if let Some(v) = verts.last_mut() {
                        v.1 = pairs[i].f64()
                    }
                }
                _ => break,
            }
            i += 1;
        }
        result = HatchLoop::Poly { verts, closed };
    } else {
        // An edge path: 93 edge count, then the edges.
        let mut count = 0usize;
        if i < pairs.len() && pairs[i].code == 93 {
            count = pairs[i].i64().clamp(0, 50_000_000) as usize;
            i += 1;
        }
        let mut edges = Vec::new();
        while edges.len() < count && i < pairs.len() && pairs[i].code == 72 {
            let (edge, next) = parse_hatch_edge(pairs, i);
            i = next;
            if let Some(e) = edge {
                edges.push(e);
            }
        }
        result = HatchLoop::Edges(edges);
    }
    // The source boundary objects (97, then handles in 330) follow.
    if i < pairs.len() && pairs[i].code == 97 {
        let n = pairs[i].i64().clamp(0, 1_000_000) as usize;
        i += 1;
        let mut seen = 0;
        while seen < n && i < pairs.len() && pairs[i].code == 330 {
            i += 1;
            seen += 1;
        }
    }
    if let HatchLoop::Poly { verts, .. } = &mut result {
        verts.shrink_to_fit();
    }
    (result, i)
}

/// Reads the edge whose type group (72) is at `i`.
fn parse_hatch_edge(pairs: &[Pair], mut i: usize) -> (Option<HatchEdge>, usize) {
    let kind = pairs[i].i32();
    i += 1;
    let start = i;
    // Groups of a line, arc or ellipse run until the next edge or loop.
    let simple_end = |mut j: usize| {
        while j < pairs.len() && matches!(pairs[j].code, 10 | 20 | 11 | 21 | 40 | 50 | 51 | 73) {
            j += 1;
        }
        j
    };
    match kind {
        1..=3 => {
            let end = simple_end(start);
            let (mut a, mut b) = (DVec2::ZERO, DVec2::ZERO);
            let (mut r, mut a0, mut a1, mut ccw) = (0.0, 0.0, 360.0, true);
            for p in &pairs[start..end] {
                match p.code {
                    10 => a.x = p.f64(),
                    20 => a.y = p.f64(),
                    11 => b.x = p.f64(),
                    21 => b.y = p.f64(),
                    40 => r = p.f64(),
                    50 => a0 = p.f64(),
                    51 => a1 = p.f64(),
                    73 => ccw = p.i32() != 0,
                    _ => {}
                }
            }
            let edge = match kind {
                1 => HatchEdge::Line(a, b),
                2 => HatchEdge::Arc { c: a, r, a0, a1, ccw },
                _ => HatchEdge::Ellipse { c: a, major: b, ratio: r, a0, a1, ccw },
            };
            (Some(edge), end)
        }
        4 => {
            // A spline edge: 94 degree, 73 rational, 74 periodic, 95 knot
            // count, 96 control point count, then the knots (40), the
            // control points (10, 20 and 42 for a weight) and, if there
            // are fit points, 97 their count and 11/21.
            let mut degree = 3;
            let (mut nk, mut nc, mut nf) = (0usize, 0usize, 0usize);
            let mut knots = Vec::new();
            let mut ctrl: Vec<(DVec2, f64)> = Vec::new();
            let mut fit: Vec<DVec2> = Vec::new();
            while i < pairs.len() {
                let p = &pairs[i];
                match p.code {
                    94 => degree = p.i32(),
                    73 | 74 => {}
                    95 => nk = p.i64().clamp(0, 50_000_000) as usize,
                    96 => nc = p.i64().clamp(0, 50_000_000) as usize,
                    40 if knots.len() < nk => knots.push(p.f64()),
                    10 if ctrl.len() < nc => ctrl.push((DVec2::new(p.f64(), 0.0), 1.0)),
                    20 if !ctrl.is_empty() => ctrl.last_mut().unwrap().0.y = p.f64(),
                    42 if !ctrl.is_empty() => ctrl.last_mut().unwrap().1 = p.f64(),
                    // 97 here is the fit point count only when it comes
                    // before the loop's source-object count: the control
                    // points are complete and no edge follows directly.
                    97 if ctrl.len() >= nc && fit.is_empty() && nf == 0 => {
                        let n = p.i64().clamp(0, 50_000_000) as usize;
                        // Fit points follow only if 11 does.
                        if i + 1 < pairs.len() && pairs[i + 1].code == 11 {
                            nf = n;
                        } else {
                            break;
                        }
                    }
                    11 if fit.len() < nf => fit.push(DVec2::new(p.f64(), 0.0)),
                    21 if !fit.is_empty() => fit.last_mut().unwrap().y = p.f64(),
                    12 | 22 | 13 | 23 => {}
                    _ => break,
                }
                i += 1;
            }
            (Some(HatchEdge::Spline { degree, knots, ctrl, fit }), i)
        }
        _ => (None, simple_end(start)),
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxf::pairs::Value;

    fn env_for(layers: &mut Layers) -> Env<'_> {
        Env { dec: Decoder::Utf8, layers }
    }

    fn parse_text_pairs(name: &str, groups: &[(i32, &str)]) -> Entity {
        let pairs: Vec<Pair> =
            groups.iter().map(|&(code, v)| Pair { code, value: Value::Text(v.as_bytes()) }).collect();
        let mut layers = Layers::new();
        let mut env = env_for(&mut layers);
        match parse(name.as_bytes(), &pairs, &mut env) {
            Parsed::Entity(e) => e,
            Parsed::Skipped => panic!("skipped"),
        }
    }

    #[test]
    fn line_and_common_groups() {
        let e = parse_text_pairs(
            "LINE",
            &[(8, "Walls"), (62, "3"), (10, "1"), (20, "2"), (30, "3"), (11, "4"), (21, "5"), (31, "6"), (60, "1")],
        );
        let Kind::Line(a, b) = e.kind else { panic!() };
        assert_eq!((a, b), (DVec3::new(1.0, 2.0, 3.0), DVec3::new(4.0, 5.0, 6.0)));
        assert_eq!(e.color, Color::Aci(3));
        assert_eq!(e.layer, 1);
        assert!(e.hidden);
        let e = parse_text_pairs("LINE", &[(62, "256"), (420, "16711680")]);
        assert_eq!(e.color, Color::True(0xFF0000));
        let e = parse_text_pairs("LINE", &[(62, "0")]);
        assert_eq!(e.color, Color::ByBlock);
        let e = parse_text_pairs("LINE", &[(62, "-5")]);
        assert_eq!(e.color, Color::Aci(5));
    }

    #[test]
    fn lwpolyline_vertices() {
        let e = parse_text_pairs(
            "LWPOLYLINE",
            &[
                (90, "3"),
                (70, "1"),
                (43, "0.5"),
                (10, "0"),
                (20, "0"),
                (42, "1"),
                (10, "10"),
                (20, "0"),
                (40, "2"),
                (41, "3"),
                (10, "10"),
                (20, "10"),
            ],
        );
        let Kind::LwPoly(w) = e.kind else { panic!() };
        assert!(w.closed);
        assert_eq!(w.verts.len(), 3);
        assert_eq!(w.verts[0].bulge, 1.0);
        assert_eq!((w.verts[1].start_width, w.verts[1].end_width), (2.0, 3.0));
        assert_eq!(w.width, 0.5);
    }

    #[test]
    fn solid_with_three_corners_repeats_the_third() {
        let e = parse_text_pairs("SOLID", &[(10, "0"), (20, "0"), (11, "1"), (21, "0"), (12, "0"), (22, "1")]);
        let Kind::Solid(p) = e.kind else { panic!() };
        assert_eq!(p[3], p[2]);
    }

    #[test]
    fn spline_groups() {
        let e = parse_text_pairs(
            "SPLINE",
            &[
                (70, "8"),
                (71, "2"),
                (72, "6"),
                (73, "3"),
                (40, "0"),
                (40, "0"),
                (40, "0"),
                (40, "1"),
                (40, "1"),
                (40, "1"),
                (10, "0"),
                (20, "0"),
                (30, "0"),
                (10, "1"),
                (20, "1"),
                (30, "0"),
                (10, "2"),
                (20, "0"),
                (30, "0"),
            ],
        );
        let Kind::Spline(s) = e.kind else { panic!() };
        assert_eq!((s.degree, s.knots.len(), s.ctrl.len()), (2, 6, 3));
        assert_eq!(s.ctrl[1], DVec3::new(1.0, 1.0, 0.0));
    }

    #[test]
    fn hatch_with_polyline_and_edge_loops() {
        let e = parse_text_pairs(
            "HATCH",
            &[
                (30, "5"),
                (2, "SOLID"),
                (70, "1"),
                (91, "2"),
                (92, "7"),
                (72, "1"),
                (73, "1"),
                (93, "3"),
                (10, "0"),
                (20, "0"),
                (42, "0.5"),
                (10, "4"),
                (20, "0"),
                (10, "4"),
                (20, "4"),
                (97, "0"),
                (92, "1"),
                (93, "2"),
                (72, "1"),
                (10, "0"),
                (20, "0"),
                (11, "1"),
                (21, "0"),
                (72, "2"),
                (10, "0"),
                (20, "0"),
                (40, "1"),
                (50, "0"),
                (51, "90"),
                (73, "1"),
                (97, "0"),
                (75, "0"),
                (76, "1"),
                (98, "1"),
                (10, "1"),
                (20, "1"),
            ],
        );
        let Kind::Hatch(h) = e.kind else { panic!() };
        assert!(h.solid);
        assert_eq!(h.elevation, 5.0);
        assert_eq!(h.loops.len(), 2);
        let HatchLoop::Poly { verts, closed } = &h.loops[0] else { panic!() };
        assert_eq!((verts.len(), *closed, verts[0].1), (3, true, 0.5));
        let HatchLoop::Edges(edges) = &h.loops[1] else { panic!() };
        assert_eq!(edges.len(), 2);
        assert!(matches!(edges[1], HatchEdge::Arc { r, a1, .. } if r == 1.0 && a1 == 90.0));
    }

    #[test]
    fn hatch_with_a_spline_edge_and_source_objects() {
        let e = parse_text_pairs(
            "HATCH",
            &[
                (70, "1"),
                (91, "1"),
                (92, "5"),
                (93, "1"),
                (72, "4"),
                (94, "2"),
                (73, "0"),
                (74, "0"),
                (95, "6"),
                (96, "3"),
                (40, "0"),
                (40, "0"),
                (40, "0"),
                (40, "1"),
                (40, "1"),
                (40, "1"),
                (10, "0"),
                (20, "0"),
                (10, "1"),
                (20, "2"),
                (10, "2"),
                (20, "0"),
                (97, "1"),
                (330, "1F"),
                (75, "0"),
            ],
        );
        let Kind::Hatch(h) = e.kind else { panic!() };
        let HatchLoop::Edges(edges) = &h.loops[0] else { panic!() };
        let HatchEdge::Spline { degree, knots, ctrl, fit } = &edges[0] else { panic!() };
        assert_eq!((*degree, knots.len(), ctrl.len(), fit.len()), (2, 6, 3, 0));
    }

    #[test]
    fn absurd_counts_do_not_allocate() {
        let e = parse_text_pairs("HATCH", &[(91, "4000000000"), (92, "7"), (93, "4000000000"), (10, "0"), (20, "0")]);
        let Kind::Hatch(h) = e.kind else { panic!() };
        assert_eq!(h.loops.len(), 1);
        let e = parse_text_pairs("MESH", &[(92, "4000000000"), (93, "4000000000"), (90, "3")]);
        assert!(matches!(e.kind, Kind::Mesh(_)));
        assert!(matches!(parse(b"WIDGET", &[], &mut env_for(&mut Layers::new())), Parsed::Skipped));
    }

    #[test]
    fn embedded_objects_do_not_overwrite_the_entity() {
        let e = parse_text_pairs(
            "MTEXT",
            &[
                (10, "5"),
                (20, "6"),
                (40, "2"),
                (41, "30"),
                (1, "text"),
                (101, "Embedded Object"),
                (10, "1"),
                (20, "0"),
                (11, "5"),
                (21, "6"),
                (40, "30"),
                (41, "0"),
            ],
        );
        let Kind::MText(t) = e.kind else { panic!() };
        assert_eq!((t.p.x, t.p.y, t.height, t.width), (5.0, 6.0, 2.0, 30.0));
        assert!(t.x_dir.is_none());
    }

    #[test]
    fn text_and_attribute_alignment_groups() {
        let e =
            parse_text_pairs("TEXT", &[(1, "hi"), (40, "2.5"), (50, "90"), (72, "1"), (73, "2"), (11, "5"), (21, "6")]);
        let Kind::Text(t) = e.kind else { panic!() };
        assert_eq!((t.halign, t.valign, t.height), (1, 2, 2.5));
        assert!((t.rotation - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        let e = parse_text_pairs("ATTRIB", &[(1, "v"), (2, "TAG"), (72, "1"), (73, "9"), (74, "2"), (70, "1")]);
        let Kind::Text(t) = e.kind else { panic!() };
        assert_eq!((t.text.as_str(), t.valign, t.attr_flags, t.attribute), ("v", 2, 1, 1));
        let e = parse_text_pairs("ATTDEF", &[(1, "default"), (2, "TAG"), (70, "2")]);
        let Kind::Text(t) = e.kind else { panic!() };
        assert_eq!(t.text, "default");
    }
}
