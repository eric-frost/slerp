//! A CAD drawing flattened into what can be drawn: polylines, arcs, dots
//! and filled triangles, each on a layer with a resolved colour.  A DXF file
//! is turned into this once (blocks expanded, curves flattened, text turned
//! into strokes), after which drawing it on a thumbnail, in the viewer at
//! any zoom, or as a 3D scene is a matter of looking at these lists.
//!
//! Colours are 0xRRGGBB, except [`FOREGROUND`]: CAD's "white" (colour index 7,
//! and true colours of pure white or black) means the pen colour that
//! contrasts with the paper, black on a light background and white on a dark
//! one.

use glam::DVec3;
use slerp_scene::Info;

/// The colour that contrasts with the background.
pub const FOREGROUND: u32 = 0x0100_0000;

/// One layer of the drawing.
#[derive(Clone, Debug)]
pub struct DrawLayer {
    pub name: String,
    pub color: u32,
    /// Whether the file shows it: neither switched off nor frozen.
    pub visible: bool,
    /// How many polylines, arcs, dots and fills are on it.
    pub items: u32,
}

/// A run of points drawn as one line.
#[derive(Clone, Copy, Debug)]
pub struct Poly {
    pub layer: u32,
    /// The layer of the block reference this belongs to, when it is the
    /// contents of one: hiding that layer hides it too.  The same as
    /// `layer` otherwise.
    pub owner: u32,
    pub color: u32,
    /// Index of the first point in `Drawing::pts`.
    pub first: u32,
    pub len: u32,
    /// Corners of the box around the points, in x and y.
    pub min: [f64; 2],
    pub max: [f64; 2],
}

/// A circle, ellipse or arc: the points `c + u cos t + v sin t` for t from
/// `t0` to `t1`.  Kept as it is, rather than as segments, so it can be
/// flattened to suit however large it is drawn.
#[derive(Clone, Copy, Debug)]
pub struct Arc {
    pub layer: u32,
    pub owner: u32,
    pub color: u32,
    pub c: DVec3,
    pub u: DVec3,
    pub v: DVec3,
    pub t0: f64,
    pub t1: f64,
}

impl Arc {
    /// The box around the arc's own points in x and y: its end points and
    /// every extreme of the curve between them.
    pub fn bbox(&self) -> ([f64; 2], [f64; 2]) {
        let at = |t: f64| self.c + self.u * t.cos() + self.v * t.sin();
        let (mut min, mut max) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        let mut add = |t: f64| {
            let p = at(t);
            min = [min[0].min(p.x), min[1].min(p.y)];
            max = [max[0].max(p.x), max[1].max(p.y)];
        };
        add(self.t0);
        add(self.t1);
        let (lo, hi) = (self.t0.min(self.t1), self.t0.max(self.t1));
        if hi - lo >= std::f64::consts::TAU {
            // A full turn reaches every extreme.
            for k in 0..4 {
                add(k as f64 * std::f64::consts::FRAC_PI_2);
            }
        }
        // The extremes of x and y are where the derivative vanishes:
        // tan t = v / u, and half a turn later.
        for (u, v) in [(self.u.x, self.v.x), (self.u.y, self.v.y)] {
            let t = v.atan2(u);
            for base in [t, t + std::f64::consts::PI] {
                // The turn of that angle that falls in [lo, hi], if any.
                let turns = ((lo - base) / std::f64::consts::TAU).ceil();
                let cand = base + turns * std::f64::consts::TAU;
                if cand <= hi {
                    add(cand);
                }
            }
        }
        (min, max)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Dot {
    pub layer: u32,
    pub owner: u32,
    pub color: u32,
    pub p: DVec3,
}

/// A batch of triangles of one colour: a hatch, a solid, a wide polyline, a
/// 3D face.
#[derive(Clone, Copy, Debug)]
pub struct Fill {
    pub layer: u32,
    pub owner: u32,
    pub color: u32,
    /// Index of the first vertex in `Drawing::fill_pts`; every three
    /// vertices are a triangle.
    pub first: u32,
    pub len: u32,
    /// A surface to shade (3D faces and meshes) rather than a flat area.
    pub lit: bool,
    pub min: [f64; 2],
    pub max: [f64; 2],
}

/// A box in drawing units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: DVec3,
    pub max: DVec3,
}

impl Bounds {
    pub const EMPTY: Bounds = Bounds { min: DVec3::splat(f64::INFINITY), max: DVec3::splat(f64::NEG_INFINITY) };

    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x
    }

    pub fn add(&mut self, p: DVec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    pub fn size(&self) -> DVec3 {
        self.max - self.min
    }

    pub fn center(&self) -> DVec3 {
        (self.min + self.max) * 0.5
    }
}

#[derive(Clone, Debug, Default)]
pub struct Drawing {
    pub layers: Vec<DrawLayer>,
    pub pts: Vec<DVec3>,
    pub polys: Vec<Poly>,
    pub arcs: Vec<Arc>,
    pub dots: Vec<Dot>,
    pub fills: Vec<Fill>,
    pub fill_pts: Vec<DVec3>,
    /// What to frame: the box around everything except stray items far from
    /// the rest (a file's own extents are often stale).
    pub bounds: Option<Bounds>,
    /// The box around everything.
    pub full_bounds: Option<Bounds>,
    /// How many items lie outside `bounds`.
    pub outliers: usize,
    /// All of the drawing lies in one plane parallel to the page, with no
    /// shaded surfaces: drawn as a 2D drawing.
    pub flat: bool,
    pub info: Info,
}

impl Drawing {
    pub fn is_empty(&self) -> bool {
        self.polys.is_empty() && self.arcs.is_empty() && self.dots.is_empty() && self.fills.is_empty()
    }

    /// The points of polyline `p`.
    pub fn poly_pts(&self, p: &Poly) -> &[DVec3] {
        &self.pts[p.first as usize..(p.first + p.len) as usize]
    }

    /// The triangle vertices of fill `f`.
    pub fn fill_verts(&self, f: &Fill) -> &[DVec3] {
        &self.fill_pts[f.first as usize..(f.first + f.len) as usize]
    }

    /// How many layer toggles a view needs.
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// Which layers the file shows.
    pub fn saved_visibility(&self) -> Vec<bool> {
        self.layers.iter().map(|l| l.visible).collect()
    }

    /// Whether an item on `layer` inside a block on `owner` is shown.
    #[inline]
    pub fn is_shown(shown: &[bool], layer: u32, owner: u32) -> bool {
        shown.get(layer as usize).copied().unwrap_or(true) && shown.get(owner as usize).copied().unwrap_or(true)
    }

    //region Extents

    /// The box of every item (as a conservative box for arcs), and the
    /// centre of every item, for `finish` to judge strays by.
    fn item_boxes(&self) -> Vec<([f64; 2], [f64; 2])> {
        let mut out = Vec::with_capacity(self.polys.len() + self.arcs.len() + self.dots.len() + self.fills.len());
        out.extend(self.polys.iter().map(|p| (p.min, p.max)));
        out.extend(self.fills.iter().map(|f| (f.min, f.max)));
        out.extend(self.arcs.iter().map(Arc::bbox));
        out.extend(self.dots.iter().map(|d| ([d.p.x, d.p.y], [d.p.x, d.p.y])));
        out
    }

    /// Works out the framing box, how many items are strays, and whether
    /// the drawing is flat.  Call after all items are added.
    ///
    /// A stray is an item whose centre lies further than a whole span of the
    /// bulk away from it, the bulk being the 1st to 99th percentile of the
    /// item centres on each axis.  Files list their own extents in the
    /// header, but those are stale after editing (a deleted item far away
    /// keeps them wide), so they are not used.
    pub fn finish(&mut self) {
        let boxes = self.item_boxes();
        let mut full = Bounds::EMPTY;
        for (min, max) in &boxes {
            full.add(DVec3::new(min[0], min[1], 0.0));
            full.add(DVec3::new(max[0], max[1], 0.0));
        }
        if boxes.is_empty() {
            self.bounds = None;
            self.full_bounds = None;
            self.flat = true;
            return;
        }
        // Z range of everything, for flatness.
        let (mut z0, mut z1) = (f64::INFINITY, f64::NEG_INFINITY);
        let mut zrange = |z: f64| {
            z0 = z0.min(z);
            z1 = z1.max(z);
        };
        self.pts.iter().for_each(|p| zrange(p.z));
        self.fill_pts.iter().for_each(|p| zrange(p.z));
        self.dots.iter().for_each(|d| zrange(d.p.z));
        for a in &self.arcs {
            let spread = a.u.z.abs() + a.v.z.abs();
            zrange(a.c.z - spread);
            zrange(a.c.z + spread);
        }
        full.min.z = z0;
        full.max.z = z1;

        // Judge strays by the centres of at most ~400k items.
        let stride = boxes.len().div_ceil(400_000).max(1);
        let centres: Vec<[f64; 2]> =
            boxes.iter().step_by(stride).map(|(a, b)| [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]).collect();
        let mut allowed = ([f64::NEG_INFINITY; 2], [f64::INFINITY; 2]);
        if centres.len() >= 25 {
            let cut = if centres.len() >= 100 { 0.01 } else { 0.04 };
            let mut lo = [0.0; 2];
            let mut hi = [0.0; 2];
            for axis in 0..2 {
                let mut v: Vec<f64> = centres.iter().map(|c| c[axis]).collect();
                v.sort_unstable_by(f64::total_cmp);
                let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
                lo[axis] = at(cut);
                hi[axis] = at(1.0 - cut);
            }
            let span = (hi[0] - lo[0]).max(hi[1] - lo[1]);
            if span > 0.0 {
                allowed = ([lo[0] - span, lo[1] - span], [hi[0] + span, hi[1] + span]);
            }
        }
        let mut core = Bounds::EMPTY;
        let mut outliers = 0;
        for (min, max) in &boxes {
            let c = [(min[0] + max[0]) * 0.5, (min[1] + max[1]) * 0.5];
            let inside = (0..2).all(|a| c[a] >= allowed.0[a] && c[a] <= allowed.1[a]);
            if inside {
                core.add(DVec3::new(min[0], min[1], 0.0));
                core.add(DVec3::new(max[0], max[1], 0.0));
            } else {
                outliers += 1;
            }
        }
        // Z of the core only, so that a stray far above doesn't make a flat
        // drawing look 3D.
        let (mut cz0, mut cz1) = (f64::INFINITY, f64::NEG_INFINITY);
        let inside_xy = |p: DVec3| (0..2).all(|a| p[a] >= allowed.0[a] && p[a] <= allowed.1[a]);
        for p in self.pts.iter().chain(&self.fill_pts) {
            if inside_xy(*p) {
                cz0 = cz0.min(p.z);
                cz1 = cz1.max(p.z);
            }
        }
        for d in &self.dots {
            if inside_xy(d.p) {
                cz0 = cz0.min(d.p.z);
                cz1 = cz1.max(d.p.z);
            }
        }
        for a in &self.arcs {
            if inside_xy(a.c) {
                let spread = a.u.z.abs() + a.v.z.abs();
                cz0 = cz0.min(a.c.z - spread);
                cz1 = cz1.max(a.c.z + spread);
            }
        }
        if cz0 > cz1 {
            (cz0, cz1) = (z0, z1);
        }
        core.min.z = cz0;
        core.max.z = cz1;
        let span_xy = core.size().x.max(core.size().y).max(1.0);
        let flat_z = (cz1 - cz0) <= 1e-6 * span_xy;
        let lit = self.fills.iter().any(|f| f.lit);
        self.flat = flat_z && !lit;
        self.outliers = outliers;
        self.bounds = Some(core);
        self.full_bounds = Some(full);
    }

    //endregion
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(d: &mut Drawing, a: DVec3, b: DVec3) {
        let first = d.pts.len() as u32;
        d.pts.push(a);
        d.pts.push(b);
        d.polys.push(Poly {
            layer: 0,
            owner: 0,
            color: FOREGROUND,
            first,
            len: 2,
            min: [a.x.min(b.x), a.y.min(b.y)],
            max: [a.x.max(b.x), a.y.max(b.y)],
        });
    }

    #[test]
    fn a_stray_item_is_left_out_of_the_framing() {
        let mut d = Drawing::default();
        for i in 0..200 {
            let x = (i % 20) as f64 * 10.0;
            let y = (i / 20) as f64 * 10.0;
            line(&mut d, DVec3::new(x, y, 0.0), DVec3::new(x + 5.0, y + 5.0, 0.0));
        }
        line(&mut d, DVec3::new(1.0e6, 1.0e6, 0.0), DVec3::new(1.0e6 + 1.0, 1.0e6, 0.0));
        d.finish();
        let b = d.bounds.unwrap();
        assert_eq!(d.outliers, 1);
        assert!(b.max.x < 300.0 && b.max.y < 200.0, "{b:?}");
        assert!(d.full_bounds.unwrap().max.x > 1.0e6);
        assert!(d.flat);
    }

    #[test]
    fn small_drawings_keep_everything() {
        let mut d = Drawing::default();
        line(&mut d, DVec3::ZERO, DVec3::new(1.0, 0.0, 0.0));
        line(&mut d, DVec3::new(1.0e6, 0.0, 0.0), DVec3::new(1.0e6, 1.0, 0.0));
        d.finish();
        assert_eq!(d.outliers, 0);
        assert_eq!(d.bounds.unwrap().max.x, 1.0e6);
    }

    #[test]
    fn flatness_ignores_a_stray_above_and_sees_real_depth() {
        let mut d = Drawing::default();
        for i in 0..100 {
            line(&mut d, DVec3::new(i as f64, 0.0, 0.0), DVec3::new(i as f64 + 1.0, 1.0, 0.0));
        }
        line(&mut d, DVec3::new(5.0e6, 0.0, 9.0e5), DVec3::new(5.0e6, 1.0, 9.0e5));
        d.finish();
        assert!(d.flat);
        let mut d = Drawing::default();
        for i in 0..100 {
            line(&mut d, DVec3::new(i as f64, 0.0, 0.0), DVec3::new(i as f64 + 1.0, 1.0, i as f64));
        }
        d.finish();
        assert!(!d.flat);
    }

    #[test]
    fn arc_boxes_are_exact() {
        let arc = |u: DVec3, v: DVec3, t0: f64, t1: f64| Arc {
            layer: 0,
            owner: 0,
            color: 0,
            c: DVec3::new(10.0, 20.0, 0.0),
            u,
            v,
            t0,
            t1,
        };
        let q = std::f64::consts::FRAC_PI_2;
        // A quarter circle of radius 2 from the east to the north.
        let (min, max) = arc(DVec3::X * 2.0, DVec3::Y * 2.0, 0.0, q).bbox();
        assert!((min[0] - 10.0).abs() < 1e-9 && (max[0] - 12.0).abs() < 1e-9, "{min:?} {max:?}");
        assert!((min[1] - 20.0).abs() < 1e-9 && (max[1] - 22.0).abs() < 1e-9);
        // A full circle.
        let (min, max) = arc(DVec3::X * 2.0, DVec3::Y * 2.0, 0.0, 2.0 * std::f64::consts::PI).bbox();
        assert!((min[0] - 8.0).abs() < 1e-9 && (max[1] - 22.0).abs() < 1e-9);
        // A half circle that passes the top, starting past the east.
        let (min, max) = arc(DVec3::X, DVec3::Y, 0.5, 0.5 + std::f64::consts::PI).bbox();
        assert!((max[1] - 21.0).abs() < 1e-9 && min[1] < 20.0 && min[1] > 19.0, "{min:?} {max:?}");
        // A tilted ellipse: its extreme isn't on an axis.
        let (min, max) =
            arc(DVec3::new(3.0, 3.0, 0.0), DVec3::new(-1.0, 1.0, 0.0), 0.0, 2.0 * std::f64::consts::PI).bbox();
        assert!((max[0] - (10.0 + 10f64.sqrt())).abs() < 1e-9 && (min[0] - (10.0 - 10f64.sqrt())).abs() < 1e-9);
    }

    #[test]
    fn empty_drawings_have_no_bounds() {
        let mut d = Drawing::default();
        d.finish();
        assert!(d.bounds.is_none() && d.is_empty() && d.flat);
    }

    #[test]
    fn layer_visibility_includes_the_owner() {
        let shown = [true, false, true];
        assert!(Drawing::is_shown(&shown, 0, 0));
        assert!(!Drawing::is_shown(&shown, 0, 1));
        assert!(!Drawing::is_shown(&shown, 1, 2));
        assert!(Drawing::is_shown(&shown, 9, 9));
    }
}
