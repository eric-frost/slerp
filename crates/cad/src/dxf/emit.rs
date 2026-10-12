//! Turning entities into drawing geometry: expanding block references,
//! flattening curves, filling hatches, writing text as strokes.
//!
//! Everything a file can make this do is bounded: block nesting is limited
//! and self-referencing blocks are cut, arrays are capped, curves have
//! a fixed maximum of segments, and the total number of points a drawing may
//! hold is a budget that, once spent, stops further geometry.

use super::curves::{self, ARC_STEP};
use super::entities::{Color, Entity, Hatch, HatchEdge, HatchLoop, Insert, Kind, LwPoly, Poly, Spline, Text};
use super::tables::{Layers, Styles};
use super::{aci, text};
use crate::drawing::{Arc, Dot, Drawing, FOREGROUND, Fill, Poly as DPoly};
use glam::{DMat4, DVec2, DVec3};
use std::collections::{BTreeMap, HashMap};
use std::f64::consts::TAU;
use std::rc::Rc;

/// How deep block references may nest.
const MAX_DEPTH: u32 = 24;
/// The most copies one array reference makes.
const MAX_ARRAY: i64 = 100_000;

/// A block definition: entities in the block's own coordinates.
pub struct Block {
    pub base: DVec3,
    pub entities: Vec<Entity>,
    /// An external reference (the drawing is in another file).
    pub xref: bool,
}

/// What the enclosing block reference passes down.
#[derive(Clone, Copy)]
pub struct Ctx {
    pub m: DMat4,
    /// The layer the reference is on, which entities on layer 0 take.
    pub layer: Option<u32>,
    /// The layer of the outermost reference.
    pub owner: Option<u32>,
    /// The colour of the reference, which "by block" entities take.
    pub color: Option<u32>,
    pub depth: u32,
}

impl Ctx {
    pub const ROOT: Ctx = Ctx { m: DMat4::IDENTITY, layer: None, owner: None, color: None, depth: 0 };
}

/// A drawing item's layer and colour once the rules of inheritance are
/// applied.
#[derive(Clone, Copy)]
struct Attr {
    layer: u32,
    owner: u32,
    color: u32,
}

struct RayItem {
    attr: Attr,
    origin: DVec3,
    dir: DVec3,
    both: bool,
}

pub struct Builder {
    pub layers: Layers,
    pub styles: Styles,
    pub blocks: HashMap<Vec<u8>, Rc<Block>>,
    pub d: Drawing,
    /// Points that may still be added.
    budget: usize,
    pub over_budget: bool,
    /// Entity kinds not drawn, by name, with how many there were.
    pub skipped: BTreeMap<String, usize>,
    pub warnings: Vec<String>,
    stack: Vec<Vec<u8>>,
    rays: Vec<RayItem>,
    missing_blocks: usize,
    cut_blocks: usize,
}

/// The upper-case form block names are looked up by.
pub fn block_key(name: &[u8]) -> Vec<u8> {
    name.trim_ascii().to_ascii_uppercase()
}

/// Turns a colour from a file into the drawing's colour: pure white and
/// pure black mean "the pen that shows on this paper".
fn normal_color(rgb: u32) -> u32 {
    match rgb & 0xFF_FFFF {
        0xFFFFFF | 0x000000 => FOREGROUND,
        c => c,
    }
}

fn aci_color(i: u8) -> u32 {
    if i == 7 { FOREGROUND } else { normal_color(aci::ACI[i as usize]) }
}

impl Builder {
    pub fn new(budget: usize) -> Builder {
        Builder {
            layers: Layers::new(),
            styles: Styles::default(),
            blocks: HashMap::new(),
            d: Drawing::default(),
            budget,
            over_budget: false,
            skipped: BTreeMap::new(),
            warnings: Vec::new(),
            stack: Vec::new(),
            rays: Vec::new(),
            missing_blocks: 0,
            cut_blocks: 0,
        }
    }

    pub fn layer_color(&self, layer: u32) -> u32 {
        match self.layers.get(layer).map(|l| l.color) {
            Some(Color::Aci(i)) => aci_color(i),
            Some(Color::True(rgb)) => normal_color(rgb),
            _ => FOREGROUND,
        }
    }

    fn attr(&self, e: &Entity, ctx: &Ctx) -> Attr {
        let layer = if e.layer == 0 { ctx.layer.unwrap_or(0) } else { e.layer };
        let color = match e.color {
            Color::True(rgb) => normal_color(rgb),
            Color::Aci(i) => aci_color(i),
            Color::ByLayer => self.layer_color(layer),
            Color::ByBlock => ctx.color.unwrap_or(FOREGROUND),
        };
        Attr { layer, owner: ctx.owner.unwrap_or(layer), color }
    }

    //region Output

    /// Reserves room for `n` more points; false (and a note) when the
    /// budget is spent.
    fn spend(&mut self, n: usize) -> bool {
        if self.over_budget {
            return false;
        }
        if n > self.budget {
            self.over_budget = true;
            self.budget = 0;
            return false;
        }
        self.budget -= n;
        true
    }

    fn count(&mut self, a: Attr) {
        if let Some(l) = self.layers.list.get_mut(a.layer as usize) {
            l.items += 1;
        }
    }

    fn poly(&mut self, a: Attr, pts: &[DVec3]) {
        if pts.len() < 2 || !pts.iter().all(|p| curves::sane(*p)) || !self.spend(pts.len()) {
            return;
        }
        let first = self.d.pts.len() as u32;
        let (mut min, mut max) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for p in pts {
            min = [min[0].min(p.x), min[1].min(p.y)];
            max = [max[0].max(p.x), max[1].max(p.y)];
        }
        self.d.pts.extend_from_slice(pts);
        self.d.polys.push(DPoly {
            layer: a.layer,
            owner: a.owner,
            color: a.color,
            first,
            len: pts.len() as u32,
            min,
            max,
        });
        self.count(a);
    }

    /// Adds triangles (three vertices each) as one fill.
    fn fill(&mut self, a: Attr, tris: &[DVec3], lit: bool) {
        if tris.len() < 3 || !tris.iter().all(|p| curves::sane(*p)) || !self.spend(tris.len()) {
            return;
        }
        let first = self.d.fill_pts.len() as u32;
        let (mut min, mut max) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for p in tris {
            min = [min[0].min(p.x), min[1].min(p.y)];
            max = [max[0].max(p.x), max[1].max(p.y)];
        }
        self.d.fill_pts.extend_from_slice(tris);
        self.d.fills.push(Fill {
            layer: a.layer,
            owner: a.owner,
            color: a.color,
            first,
            len: (tris.len() / 3 * 3) as u32,
            lit,
            min,
            max,
        });
        self.count(a);
    }

    fn arc(&mut self, a: Attr, c: DVec3, u: DVec3, v: DVec3, t0: f64, sweep: f64) {
        if ![c, u, v].iter().all(|p| curves::sane(*p)) || !t0.is_finite() || !self.spend(8) {
            return;
        }
        self.d.arcs.push(Arc { layer: a.layer, owner: a.owner, color: a.color, c, u, v, t0, t1: t0 + sweep });
        self.count(a);
    }

    fn dot(&mut self, a: Attr, p: DVec3) {
        if curves::sane(p) && self.spend(1) {
            self.d.dots.push(Dot { layer: a.layer, owner: a.owner, color: a.color, p });
            self.count(a);
        }
    }

    fn skip(&mut self, what: &str) {
        *self.skipped.entry(what.to_string()).or_insert(0) += 1;
    }

    //endregion

    /// Draws one entity (and, for a block reference, the block).
    pub fn emit(&mut self, e: &Entity, ctx: &Ctx) {
        if e.hidden || self.over_budget {
            return;
        }
        let a = self.attr(e, ctx);
        // Entities in their own plane (OCS) move to the world through this.
        let m = ctx.m * curves::ocs4(e.normal);
        match &e.kind {
            Kind::Line(p, q) => self.poly(a, &[ctx.m.transform_point3(*p), ctx.m.transform_point3(*q)]),
            Kind::Point(p) => self.dot(a, ctx.m.transform_point3(*p)),
            Kind::Circle { c, r } => {
                let r = r.abs();
                self.arc(
                    a,
                    m.transform_point3(*c),
                    m.transform_vector3(DVec3::X * r),
                    m.transform_vector3(DVec3::Y * r),
                    0.0,
                    TAU,
                );
            }
            Kind::Arc { c, r, a0, a1 } => {
                let r = r.abs();
                self.arc(
                    a,
                    m.transform_point3(*c),
                    m.transform_vector3(DVec3::X * r),
                    m.transform_vector3(DVec3::Y * r),
                    *a0,
                    curves::ellipse_sweep(*a0, *a1),
                );
            }
            Kind::Ellipse { c, major, ratio, t0, t1 } => {
                let ratio = if ratio.is_finite() && *ratio > 0.0 { *ratio } else { 1.0 };
                let minor = e.normal.cross(*major).try_normalize().unwrap_or(DVec3::Y) * major.length() * ratio;
                self.arc(
                    a,
                    ctx.m.transform_point3(*c),
                    ctx.m.transform_vector3(*major),
                    ctx.m.transform_vector3(minor),
                    *t0,
                    curves::ellipse_sweep(*t0, *t1),
                );
            }
            Kind::Spline(s) => self.spline(a, s, &ctx.m),
            Kind::LwPoly(w) => self.lw_polyline(a, w, &m),
            Kind::Poly(p) => self.polyline(a, p, &m, &ctx.m),
            Kind::Solid(q) => {
                let p: Vec<DVec3> = q.iter().map(|p| m.transform_point3(*p)).collect();
                // The corners are in the order 1, 2, 4, 3.
                self.fill(a, &[p[0], p[1], p[2], p[2], p[1], p[3]], false);
            }
            Kind::Face(q) => {
                let p: Vec<DVec3> = q.iter().map(|p| ctx.m.transform_point3(*p)).collect();
                if p[3] == p[2] {
                    self.fill(a, &[p[0], p[1], p[2]], true);
                } else {
                    self.fill(a, &[p[0], p[1], p[2], p[0], p[2], p[3]], true);
                }
            }
            Kind::Hatch(h) => self.hatch(a, h, &m),
            Kind::Text(t) => self.text(a, t, &m),
            Kind::MText(t) => {
                // The insertion point of an MTEXT is in world coordinates;
                // its plane comes from its extrusion.
                let style = self.styles.get(&t.style);
                let strokes = text::mtext_strokes(t, style);
                let mw = ctx.m * curves::ocs4(e.normal);
                self.strokes(a, strokes, t.p.z, &mw);
            }
            Kind::Insert(ins) => self.insert(e, ins, a, ctx, &m),
            Kind::Dimension { block } => {
                // The dimension was drawn into a block of its own, in world
                // coordinates.
                let key = block_key(block.as_bytes());
                let ins = Insert {
                    block: key,
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
                let flat = Entity { normal: DVec3::Z, ..e.clone() };
                self.insert_block(&flat, &ins, a, ctx, true);
            }
            Kind::Leader { pts, arrow } => self.leader(a, pts, *arrow, &ctx.m),
            Kind::Ray { origin, dir, both } => {
                if let Some(dir) = ctx.m.transform_vector3(*dir).try_normalize() {
                    self.rays.push(RayItem { attr: a, origin: ctx.m.transform_point3(*origin), dir, both: *both });
                }
            }
            Kind::Mesh(mesh) => {
                let mut tris = Vec::new();
                let mut i = 0;
                while i < mesh.faces.len() {
                    let n = mesh.faces[i] as usize;
                    let Some(idx) = mesh.faces.get(i + 1..i + 1 + n) else { break };
                    i += 1 + n;
                    let v: Option<Vec<DVec3>> =
                        idx.iter().map(|&k| mesh.verts.get(k as usize).map(|p| ctx.m.transform_point3(*p))).collect();
                    if let Some(v) = v {
                        for k in 1..v.len().saturating_sub(1) {
                            tris.extend_from_slice(&[v[0], v[k], v[k + 1]]);
                        }
                    }
                    if tris.len() > self.budget {
                        break;
                    }
                }
                self.fill(a, &tris, true);
            }
        }
    }

    /// Draws strokes given in a plane at height `z` through `m`.
    fn strokes(&mut self, a: Attr, strokes: Vec<Vec<DVec2>>, z: f64, m: &DMat4) {
        for s in strokes {
            let pts: Vec<DVec3> = s.iter().map(|p| m.transform_point3(DVec3::new(p.x, p.y, z))).collect();
            self.poly(a, &pts);
        }
    }

    fn text(&mut self, a: Attr, t: &Text, m: &DMat4) {
        // An attribute marked invisible isn't drawn, and a definition only
        // shows (with its default value) when it is constant.
        if (t.attribute == 1 && t.attr_flags & 1 != 0) || (t.attribute == 2 && t.attr_flags & 2 == 0) {
            return;
        }
        let style = self.styles.get(&t.style);
        let strokes = text::text_strokes(t, style);
        self.strokes(a, strokes, t.p.z, m);
    }

    fn leader(&mut self, a: Attr, pts: &[DVec3], arrow: bool, m: &DMat4) {
        let p: Vec<DVec3> = pts.iter().map(|p| m.transform_point3(*p)).collect();
        self.poly(a, &p);
        if arrow && p.len() >= 2 {
            let len: f64 = p.windows(2).map(|w| w[0].distance(w[1])).sum();
            let d = (p[1] - p[0]).try_normalize();
            if let (Some(d), true) = (d, len > 0.0) {
                let l = (len * 0.1).min(p[0].distance(p[1]) * 0.5);
                let side = d.cross(DVec3::Z).try_normalize().unwrap_or(DVec3::Y) * (l / 3.0);
                let base = p[0] + d * l;
                self.fill(a, &[p[0], base + side, base - side], false);
            }
        }
    }

    //region Splines and polylines

    fn spline(&mut self, a: Attr, s: &Spline, m: &DMat4) {
        let mut pts = Vec::new();
        curves::spline_points(s, &mut pts);
        let mut world: Vec<DVec3> = pts.iter().map(|p| m.transform_point3(*p)).collect();
        if s.flags & 1 != 0 && world.len() > 2 && world[0] != *world.last().unwrap() {
            world.push(world[0]);
        }
        self.poly(a, &world);
    }

    /// The outline points of a polyline with bulges, in its plane.
    fn outline(&self, verts: &[(DVec2, f64)], closed: bool) -> Vec<DVec2> {
        let n = verts.len();
        let mut out = Vec::with_capacity(n + 1);
        if n == 0 {
            return out;
        }
        out.push(verts[0].0);
        let segments = if closed { n } else { n - 1 };
        for i in 0..segments {
            curves::bulge_points(verts[i].0, verts[(i + 1) % n].0, verts[i].1, &mut out);
            if out.len() > self.budget {
                break;
            }
        }
        out
    }

    fn lw_polyline(&mut self, a: Attr, w: &LwPoly, m: &DMat4) {
        let n = w.verts.len();
        if n < 2 {
            return;
        }
        let widths: Vec<(f64, f64)> = w
            .verts
            .iter()
            .map(|v| {
                if v.start_width > 0.0 || v.end_width > 0.0 { (v.start_width, v.end_width) } else { (w.width, w.width) }
            })
            .collect();
        let verts: Vec<(DVec2, f64)> = w.verts.iter().map(|v| (v.p, v.bulge)).collect();
        self.draw_polyline(a, &verts, &widths, w.closed, w.elevation, m);
    }

    /// A 2D polyline as a line, or as a filled strip when it has width.
    fn draw_polyline(
        &mut self,
        a: Attr,
        verts: &[(DVec2, f64)],
        widths: &[(f64, f64)],
        closed: bool,
        z: f64,
        m: &DMat4,
    ) {
        let wide = widths.iter().any(|&(s, e)| s > 0.0 || e > 0.0);
        if !wide {
            let pts: Vec<DVec3> = self.outline(verts, closed).iter().map(|p| m.transform_point3(p.extend(z))).collect();
            self.poly(a, &pts);
            return;
        }
        // Samples along the polyline with the half width at each.
        let n = verts.len();
        let segments = if closed { n } else { n - 1 };
        let mut tris: Vec<DVec3> = Vec::new();
        let mut prev: Option<(DVec2, DVec2, f64)> = None; // point, normal, half width at the end
        for i in 0..segments {
            let (w0, w1) = widths.get(i).copied().unwrap_or((0.0, 0.0));
            let mut pts = vec![verts[i].0];
            curves::bulge_points(verts[i].0, verts[(i + 1) % n].0, verts[i].1, &mut pts);
            let total = pts.len() - 1;
            for k in 0..total {
                let (p, q) = (pts[k], pts[k + 1]);
                let Some(dir) = (q - p).try_normalize() else { continue };
                let normal = DVec2::new(-dir.y, dir.x);
                let hw = |t: f64| (w0 + (w1 - w0) * t) * 0.5;
                let (h0, h1) = (hw(k as f64 / total as f64), hw((k + 1) as f64 / total as f64));
                let quad = [p + normal * h0, p - normal * h0, q - normal * h1, q + normal * h1];
                for idx in [0usize, 1, 2, 0, 2, 3] {
                    tris.push(m.transform_point3(quad[idx].extend(z)));
                }
                // A joint with the previous piece: fill the wedge on the
                // outside of the turn with both candidate triangles.
                if let Some((jp, jn, jh)) = prev
                    && jp.distance_squared(p) < 1e-18
                {
                    for s in [1.0, -1.0] {
                        for tri in [jp, jp + jn * (jh * s), p + normal * (h0 * s)] {
                            tris.push(m.transform_point3(tri.extend(z)));
                        }
                    }
                }
                prev = Some((q, normal, h1));
                if tris.len() > self.budget {
                    break;
                }
            }
        }
        self.fill(a, &tris, false);
    }

    fn polyline(&mut self, a: Attr, p: &Poly, m: &DMat4, world: &DMat4) {
        if p.flags & Poly::POLYFACE != 0 {
            let positions: Vec<DVec3> =
                p.verts.iter().filter(|v| v.flags & 64 != 0).map(|v| world.transform_point3(v.p)).collect();
            let mut tris = Vec::new();
            for v in p.verts.iter().filter(|v| v.flags & 64 == 0 && v.flags & 128 != 0) {
                let idx: Vec<usize> =
                    v.face.iter().filter(|&&i| i != 0).map(|&i| i.unsigned_abs() as usize - 1).collect();
                let pts: Option<Vec<DVec3>> = idx.iter().map(|&i| positions.get(i).copied()).collect();
                if let Some(pts) = pts {
                    for k in 1..pts.len().saturating_sub(1) {
                        tris.extend_from_slice(&[pts[0], pts[k], pts[k + 1]]);
                    }
                }
                if tris.len() > self.budget {
                    break;
                }
            }
            self.fill(a, &tris, true);
        } else if p.flags & Poly::MESH != 0 {
            let (rows, cols) = (p.m.max(0) as usize, p.n.max(0) as usize);
            let grid: Vec<DVec3> =
                p.verts.iter().filter(|v| v.flags & 64 != 0).map(|v| world.transform_point3(v.p)).collect();
            if rows < 2 || cols < 2 || grid.len() < rows * cols || rows.saturating_mul(cols) > self.budget {
                return;
            }
            let closed_m = p.flags & Poly::CLOSED != 0;
            let closed_n = p.flags & Poly::MESH_CLOSED_N != 0;
            let at = |i: usize, j: usize| grid[(i % rows) * cols + (j % cols)];
            let mut tris = Vec::new();
            for i in 0..if closed_m { rows } else { rows - 1 } {
                for j in 0..if closed_n { cols } else { cols - 1 } {
                    let (q00, q10, q11, q01) = (at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1));
                    tris.extend_from_slice(&[q00, q10, q11, q00, q11, q01]);
                }
            }
            self.fill(a, &tris, true);
        } else if p.flags & Poly::POLYLINE_3D != 0 {
            let mut pts: Vec<DVec3> =
                p.verts.iter().filter(|v| v.flags & 16 == 0).map(|v| world.transform_point3(v.p)).collect();
            if p.flags & Poly::CLOSED != 0 && pts.len() > 2 {
                pts.push(pts[0]);
            }
            self.poly(a, &pts);
        } else {
            let used: Vec<_> = p.verts.iter().filter(|v| v.flags & 16 == 0).collect();
            let verts: Vec<(DVec2, f64)> = used.iter().map(|v| (v.p.truncate(), v.bulge)).collect();
            let widths: Vec<(f64, f64)> = used
                .iter()
                .map(|v| {
                    if v.start_width > 0.0 || v.end_width > 0.0 {
                        (v.start_width, v.end_width)
                    } else {
                        (p.start_width, p.end_width)
                    }
                })
                .collect();
            if verts.len() >= 2 {
                let z = used.first().map_or(p.elevation, |v| v.p.z);
                self.draw_polyline(a, &verts, &widths, p.flags & Poly::CLOSED != 0, z, m);
            }
        }
    }

    //endregion

    //region Block references

    fn insert(&mut self, e: &Entity, ins: &Insert, a: Attr, ctx: &Ctx, _m: &DMat4) {
        let key = block_key(&ins.block);
        let ins = Insert { block: key, ..ins.clone() };
        self.insert_block(e, &ins, a, ctx, false);
        // Attributes are placed in the space the reference is in.
        for att in &ins.attribs {
            self.emit(att, ctx);
        }
    }

    /// Draws the block `ins.block` (already upper-case) as a reference with
    /// the transformation `ins` and entity `e` describe.  `keep_origin`
    /// ignores the block's base point, as dimensions do.
    fn insert_block(&mut self, e: &Entity, ins: &Insert, a: Attr, ctx: &Ctx, keep_origin: bool) {
        let Some(block) = self.blocks.get(&ins.block).cloned() else {
            self.missing_blocks += 1;
            return;
        };
        if block.xref {
            self.skip("XREF");
            return;
        }
        if ctx.depth >= MAX_DEPTH || self.stack.contains(&ins.block) {
            self.cut_blocks += 1;
            return;
        }
        let cols = (ins.cols.max(1) as i64).min(MAX_ARRAY);
        let rows = (ins.rows.max(1) as i64).min(MAX_ARRAY / cols);
        let o = curves::ocs4(e.normal);
        let rot = DMat4::from_rotation_z(ins.rotation);
        let scale = DMat4::from_scale(DVec3::new(
            if ins.scale.x == 0.0 { 1.0 } else { ins.scale.x },
            if ins.scale.y == 0.0 { 1.0 } else { ins.scale.y },
            if ins.scale.z == 0.0 { 1.0 } else { ins.scale.z },
        ));
        let base = if keep_origin { DVec3::ZERO } else { block.base };
        let unbase = DMat4::from_translation(-base);
        let child = |m: DMat4| Ctx {
            m,
            layer: Some(a.layer),
            owner: Some(a.owner),
            color: Some(a.color),
            depth: ctx.depth + 1,
        };
        self.stack.push(ins.block.clone());
        'array: for j in 0..rows {
            for i in 0..cols {
                if self.over_budget {
                    break 'array;
                }
                let off =
                    rot.transform_vector3(DVec3::new(i as f64 * ins.col_spacing, j as f64 * ins.row_spacing, 0.0));
                let local = o * DMat4::from_translation(ins.p + off) * rot * scale * unbase;
                let c = child(ctx.m * local);
                for ent in &block.entities {
                    self.emit(ent, &c);
                    if self.over_budget {
                        break 'array;
                    }
                }
            }
        }
        self.stack.pop();
    }

    //endregion

    //region Hatches

    fn hatch(&mut self, a: Attr, h: &Hatch, m: &DMat4) {
        let mut loops: Vec<Vec<DVec2>> = Vec::new();
        for l in &h.loops {
            let pts = match l {
                HatchLoop::Poly { verts, closed } => {
                    let v: Vec<(DVec2, f64)> = verts.iter().map(|&(p, b)| (p, b)).collect();
                    self.outline(&v, *closed)
                }
                HatchLoop::Edges(edges) => self.edge_points(edges),
            };
            if pts.len() >= 2 {
                loops.push(pts);
            }
        }
        if loops.is_empty() {
            return;
        }
        let z = h.elevation;
        if h.solid || h.gradient {
            let tris = triangulate(&loops, h.style);
            let world: Vec<DVec3> = tris.iter().map(|p| m.transform_point3(p.extend(z))).collect();
            self.fill(a, &world, false);
        } else {
            // Patterns aren't drawn; their boundary is.
            for l in &loops {
                let mut pts: Vec<DVec3> = l.iter().map(|p| m.transform_point3(p.extend(z))).collect();
                if pts.len() > 2 && pts[0] != *pts.last().unwrap() {
                    pts.push(pts[0]);
                }
                self.poly(a, &pts);
            }
        }
    }

    /// The boundary a chain of hatch edges makes, as points.
    fn edge_points(&self, edges: &[HatchEdge]) -> Vec<DVec2> {
        let mut out: Vec<DVec2> = Vec::new();
        for e in edges {
            let mut pts: Vec<DVec2> = Vec::new();
            match e {
                HatchEdge::Line(p, q) => pts.extend([*p, *q]),
                HatchEdge::Arc { c, r, a0, a1, ccw } => {
                    // Clockwise arcs store complementary angles.
                    let (s, t) = if *ccw { (*a0, *a1) } else { (360.0 - a1, 360.0 - a0) };
                    let (s, t) = (s.to_radians(), t.to_radians());
                    let sweep = curves::ellipse_sweep(s, t);
                    let n = curves::arc_segments(sweep);
                    for i in 0..=n {
                        let ang = s + sweep * i as f64 / n as f64;
                        pts.push(*c + DVec2::new(ang.cos(), ang.sin()) * r.abs());
                    }
                    if !ccw {
                        pts.reverse();
                    }
                }
                HatchEdge::Ellipse { c, major, ratio, a0, a1, ccw } => {
                    let (s, t) = if *ccw { (*a0, *a1) } else { (360.0 - a1, 360.0 - a0) };
                    let ratio = if ratio.is_finite() && *ratio > 0.0 { *ratio } else { 1.0 };
                    // The angles are measured from the major axis; the curve
                    // is walked by parameter.
                    let to_param = |ang: f64| (ang.sin() / ratio).atan2(ang.cos());
                    let (s, t) = (to_param(s.to_radians()), to_param(t.to_radians()));
                    let sweep = curves::ellipse_sweep(s, t);
                    let minor = DVec2::new(-major.y, major.x) * ratio;
                    let n = curves::arc_segments(sweep);
                    for i in 0..=n {
                        let ang = s + sweep * i as f64 / n as f64;
                        pts.push(*c + *major * ang.cos() + minor * ang.sin());
                    }
                    if !ccw {
                        pts.reverse();
                    }
                }
                HatchEdge::Spline { degree, knots, ctrl, fit } => {
                    let sp = Spline {
                        degree: *degree,
                        flags: if ctrl.iter().any(|c| c.1 != 1.0) { 4 } else { 0 },
                        knots: knots.clone(),
                        weights: ctrl.iter().map(|c| c.1).collect(),
                        ctrl: ctrl.iter().map(|c| c.0.extend(0.0)).collect(),
                        fit: fit.iter().map(|p| p.extend(0.0)).collect(),
                    };
                    let mut p3 = Vec::new();
                    curves::spline_points(&sp, &mut p3);
                    pts.extend(p3.iter().map(|p| p.truncate()));
                }
            }
            out.extend(pts);
            if out.len() > self.budget {
                break;
            }
        }
        out
    }

    //endregion

    /// Warnings about what was left out, and the clipped rays; call once
    /// everything has been drawn.
    pub fn finish(&mut self) -> Drawing {
        self.d.layers = self
            .layers
            .list
            .iter()
            .map(|l| crate::drawing::DrawLayer {
                name: l.name.clone(),
                color: match l.color {
                    Color::Aci(i) => aci_color(i),
                    Color::True(rgb) => normal_color(rgb),
                    _ => FOREGROUND,
                },
                visible: l.visible(),
                items: l.items,
            })
            .collect();
        // Rays and construction lines run to the edge of the drawing.
        self.d.finish();
        if let (Some(b), false) = (self.d.bounds, self.rays.is_empty()) {
            let pad = b.size().x.max(b.size().y).max(1.0) * 0.25;
            let (lo, hi) = (b.min - DVec3::splat(pad), b.max + DVec3::splat(pad));
            for r in std::mem::take(&mut self.rays) {
                if let Some((t0, t1)) = clip_ray(r.origin, r.dir, r.both, lo, hi) {
                    self.poly(r.attr, &[r.origin + r.dir * t0, r.origin + r.dir * t1]);
                }
            }
        }
        if self.over_budget {
            self.warnings.push("The drawing is too large to show whole: the rest was left out.".into());
        }
        if self.missing_blocks > 0 {
            self.warnings.push(format!("{} block references name blocks the file lacks.", self.missing_blocks));
        }
        if self.cut_blocks > 0 {
            self.warnings
                .push(format!("{} block references were left out for nesting too deep or looping.", self.cut_blocks));
        }
        if self.d.outliers > 0 {
            self.warnings.push(format!("{} items far from the rest are left out of the framing.", self.d.outliers));
        }
        if !self.skipped.is_empty() {
            let mut acis = Vec::new();
            let mut others = Vec::new();
            for (name, n) in &self.skipped {
                let item = format!("{name} x{n}");
                if super::entities::is_acis(name.as_bytes()) { acis.push(item) } else { others.push(item) }
            }
            let mut parts = acis;
            parts.extend(others);
            self.warnings.push(format!("Not drawn: {}.", parts.join(", ")));
        }
        std::mem::take(&mut self.d)
    }

    pub fn note_skipped(&mut self, name: &[u8]) {
        let n = String::from_utf8_lossy(name).to_ascii_uppercase();
        self.skip(&n);
    }
}

/// The parameter range of the part of a ray or line inside a box, in x and y.
fn clip_ray(o: DVec3, d: DVec3, both: bool, lo: DVec3, hi: DVec3) -> Option<(f64, f64)> {
    let (mut t0, mut t1) = (if both { f64::NEG_INFINITY } else { 0.0 }, f64::INFINITY);
    for axis in 0..2 {
        if d[axis].abs() < 1e-12 {
            if o[axis] < lo[axis] || o[axis] > hi[axis] {
                return None;
            }
        } else {
            let (a, b) = ((lo[axis] - o[axis]) / d[axis], (hi[axis] - o[axis]) / d[axis]);
            t0 = t0.max(a.min(b));
            t1 = t1.min(a.max(b));
        }
    }
    (t0 < t1 && t0.is_finite() && t1.is_finite()).then_some((t0, t1))
}

//region Triangulation

/// Triangulates hatch boundaries.  A loop inside an odd number of others is
/// a hole in the loop around it.  `style` 0 alternates like that, 1 fills
/// only the outermost loops minus their direct holes, and 2 fills the
/// outermost loops whole.
pub fn triangulate(loops: &[Vec<DVec2>], style: i32) -> Vec<DVec2> {
    // At most this many loops are compared for nesting; more are all filled
    // as outer loops.
    const NEST_LIMIT: usize = 300;
    let n = loops.len();
    let areas: Vec<f64> = loops.iter().map(|l| signed_area(l).abs()).collect();
    let mut parent: Vec<Option<usize>> = vec![None; n];
    let mut depth = vec![0usize; n];
    if n <= NEST_LIMIT {
        for i in 0..n {
            // The nearest loop that contains this one: the smallest.
            let probe = loops[i][0];
            let mut best: Option<usize> = None;
            let mut count = 0;
            for j in 0..n {
                if j != i && areas[j] > areas[i] && contains(&loops[j], probe) {
                    count += 1;
                    if best.is_none_or(|b| areas[j] < areas[b]) {
                        best = Some(j);
                    }
                }
            }
            parent[i] = best;
            depth[i] = count;
        }
    }
    let mut out = Vec::new();
    for i in 0..n {
        let outer = match style {
            0 => depth[i].is_multiple_of(2),
            _ => depth[i] == 0,
        };
        if !outer {
            continue;
        }
        let holes: Vec<usize> = if style == 2 {
            Vec::new()
        } else {
            (0..n).filter(|&j| parent[j] == Some(i) && (style == 0 || depth[j] == 1)).collect()
        };
        triangulate_one(&loops[i], holes.iter().map(|&j| loops[j].as_slice()).collect(), &mut out);
    }
    out
}

fn triangulate_one(outer: &[DVec2], holes: Vec<&[DVec2]>, out: &mut Vec<DVec2>) {
    let mut pts: Vec<DVec2> = Vec::new();
    let mut hole_starts: Vec<u32> = Vec::new();
    let clean = |l: &[DVec2]| -> Vec<DVec2> {
        let mut v: Vec<DVec2> = l.to_vec();
        if v.len() > 1 && v[0] == *v.last().unwrap() {
            v.pop();
        }
        v
    };
    pts.extend(clean(outer));
    for h in holes {
        hole_starts.push(pts.len() as u32);
        pts.extend(clean(h));
    }
    // Ear clipping takes time by the square of the points or worse (a ring
    // of repeated points takes seconds at a few thousand): larger fills are
    // left out.
    if pts.len() < 3 || pts.len() > 16_384 || pts.iter().any(|p| !p.is_finite()) {
        return;
    }
    let result = std::panic::catch_unwind(|| {
        let mut tris: Vec<u32> = Vec::new();
        earcut::Earcut::new().earcut(pts.iter().map(|p| [p.x, p.y]), &hole_starts, &mut tris);
        (tris, pts)
    });
    if let Ok((tris, pts)) = result {
        for t in tris {
            if let Some(p) = pts.get(t as usize) {
                out.push(*p);
            }
        }
    }
}

fn signed_area(l: &[DVec2]) -> f64 {
    let mut s = 0.0;
    for i in 0..l.len() {
        let (a, b) = (l[i], l[(i + 1) % l.len()]);
        s += a.x * b.y - b.x * a.y;
    }
    s * 0.5
}

/// Even-odd point in polygon test.
fn contains(l: &[DVec2], p: DVec2) -> bool {
    let mut inside = false;
    let mut j = l.len() - 1;
    for i in 0..l.len() {
        let (a, b) = (l[i], l[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

//endregion

#[allow(dead_code)]
const _: f64 = ARC_STEP;

#[cfg(test)]
mod tests {
    use super::*;

    /// Found by fuzzing (Blender's copy of the bug): an outline of a few
    /// repeated points costs ear clipping time by the square of its points
    /// or worse.  Past the cap the fill is left out at once.
    #[test]
    fn huge_degenerate_fill_is_left_out_quickly() {
        let outline: Vec<DVec2> = (0..50_000).map(|i| DVec2::new((i % 3) as f64, ((i * 7) % 3) as f64)).collect();
        let started = std::time::Instant::now();
        let mut out = Vec::new();
        triangulate_one(&outline, Vec::new(), &mut out);
        assert!(out.is_empty());
        assert!(started.elapsed().as_secs_f32() < 5.0, "{:?}", started.elapsed());
    }
}
