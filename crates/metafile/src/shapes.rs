//! Building the outlines GDI's shape calls describe: polygons, Bezier
//! chains, ellipses, rounded rectangles and the arc family.  Shapes are
//! worked out in logical coordinates and transformed point by point, so
//! rotations and skews of the world transform bend them correctly, and the
//! result is in canvas pixels, small enough numbers for tiny-skia.

use crate::affine::Affine;
use std::f64::consts::{FRAC_PI_2, TAU};
use tiny_skia::{Path, PathBuilder};

/// The largest canvas coordinate passed on.  Hostile files give 32-bit
/// coordinates that would overflow single precision; far beyond the canvas
/// the exact value no longer matters.
const FAR: f64 = 1.0e7;

/// A path being built from logical coordinates.
pub struct Geo {
    pb: PathBuilder,
    xf: Affine,
    /// True once a point has been added since the last `move_to`.
    open: bool,
    started: bool,
    pub points: usize,
}

impl Geo {
    pub fn new(xf: Affine) -> Geo {
        Geo { pb: PathBuilder::new(), xf, open: false, started: false, points: 0 }
    }

    fn p(&self, x: f64, y: f64) -> (f32, f32) {
        let (px, py) = self.xf.apply((x, y));
        (px.clamp(-FAR, FAR) as f32, py.clamp(-FAR, FAR) as f32)
    }

    pub fn move_to(&mut self, x: f64, y: f64) {
        let (px, py) = self.p(x, y);
        self.pb.move_to(px, py);
        self.open = false;
        self.started = true;
        self.points += 1;
    }

    pub fn line_to(&mut self, x: f64, y: f64) {
        if !self.started {
            return self.move_to(x, y);
        }
        let (px, py) = self.p(x, y);
        self.pb.line_to(px, py);
        self.open = true;
        self.points += 1;
    }

    pub fn cubic_to(&mut self, c1: (f64, f64), c2: (f64, f64), end: (f64, f64)) {
        if !self.started {
            self.move_to(c1.0, c1.1);
        }
        let (a, b) = self.p(c1.0, c1.1);
        let (c, d) = self.p(c2.0, c2.1);
        let (e, f) = self.p(end.0, end.1);
        self.pb.cubic_to(a, b, c, d, e, f);
        self.open = true;
        self.points += 3;
    }

    pub fn close(&mut self) {
        if self.started {
            self.pb.close();
        }
    }

    pub fn polyline(&mut self, pts: &[(f64, f64)]) {
        for (i, &(x, y)) in pts.iter().enumerate() {
            if i == 0 { self.move_to(x, y) } else { self.line_to(x, y) }
        }
    }

    pub fn polygon(&mut self, pts: &[(f64, f64)]) {
        self.polyline(pts);
        self.close();
    }

    /// A chain of cubic Beziers: a start point and three points per curve.
    pub fn bezier(&mut self, pts: &[(f64, f64)], continue_from_current: bool) {
        let mut i = 0;
        if !continue_from_current {
            let Some(&(x, y)) = pts.first() else { return };
            self.move_to(x, y);
            i = 1;
        }
        while i + 2 < pts.len() {
            self.cubic_to(pts[i], pts[i + 1], pts[i + 2]);
            i += 3;
        }
    }

    /// An elliptical arc by parametric angle: points `(cx + rx cos t,
    /// cy + ry sin t)` from `t0` through a sweep of `dt` radians.  Starts a
    /// new subpath unless `connect` asks to continue the current one.
    pub fn arc(&mut self, c: (f64, f64), r: (f64, f64), t0: f64, dt: f64, connect: bool) {
        let point = |t: f64| (c.0 + r.0 * t.cos(), c.1 + r.1 * t.sin());
        let (sx, sy) = point(t0);
        if connect {
            self.line_to(sx, sy)
        } else {
            self.move_to(sx, sy)
        }
        if dt == 0.0 || !dt.is_finite() {
            return;
        }
        let n = ((dt.abs() / FRAC_PI_2).ceil() as usize).clamp(1, 16);
        let step = dt / n as f64;
        let k = 4.0 / 3.0 * (step / 4.0).tan();
        for i in 0..n {
            let (a, b) = (t0 + step * i as f64, t0 + step * (i + 1) as f64);
            let (pa, pb) = (point(a), point(b));
            let da = (-r.0 * a.sin(), r.1 * a.cos());
            let db = (-r.0 * b.sin(), r.1 * b.cos());
            self.cubic_to((pa.0 + k * da.0, pa.1 + k * da.1), (pb.0 - k * db.0, pb.1 - k * db.1), pb);
        }
    }

    pub fn ellipse(&mut self, l: f64, t: f64, r: f64, b: f64) {
        let c = ((l + r) / 2.0, (t + b) / 2.0);
        self.arc(c, ((r - l).abs() / 2.0, (b - t).abs() / 2.0), 0.0, TAU, false);
        self.close();
    }

    pub fn rect(&mut self, l: f64, t: f64, r: f64, b: f64) {
        self.polygon(&[(l, t), (r, t), (r, b), (l, b)]);
    }

    /// A rectangle with elliptical corners `cw` wide and `ch` high.
    pub fn round_rect(&mut self, l: f64, t: f64, r: f64, b: f64, cw: f64, ch: f64) {
        let (l, r) = (l.min(r), l.max(r));
        let (t, b) = (t.min(b), t.max(b));
        let rx = (cw.abs() / 2.0).min((r - l) / 2.0);
        let ry = (ch.abs() / 2.0).min((b - t) / 2.0);
        if rx <= 0.0 || ry <= 0.0 {
            return self.rect(l, t, r, b);
        }
        self.arc((r - rx, t + ry), (rx, ry), -FRAC_PI_2, FRAC_PI_2, false);
        self.arc((r - rx, b - ry), (rx, ry), 0.0, FRAC_PI_2, true);
        self.arc((l + rx, b - ry), (rx, ry), FRAC_PI_2, FRAC_PI_2, true);
        self.arc((l + rx, t + ry), (rx, ry), std::f64::consts::PI, FRAC_PI_2, true);
        self.close();
    }

    pub fn finish(self) -> Option<Path> {
        self.pb.finish()
    }
}

/// The parametric angle of the point where the ray from an ellipse's
/// centre in direction `(dx, dy)` meets the ellipse with radii `rx`, `ry`.
pub fn parametric_angle(rx: f64, ry: f64, dx: f64, dy: f64) -> f64 {
    (rx * dy).atan2(ry * dx)
}

/// Which outline an arc call asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArcKind {
    /// An open curve.
    Arc,
    /// A curve closed through the centre.
    Pie,
    /// A curve closed by its chord.
    Chord,
}

/// The pieces an arc call is made of, in logical coordinates: the
/// ellipse's centre and radii, the start parametric angle and the signed
/// sweep.  `clockwise` is the direction on screen after any mirroring the
/// mapping does, in logical y-down angles.
pub struct ArcSpec {
    pub centre: (f64, f64),
    pub radii: (f64, f64),
    pub start: f64,
    pub sweep: f64,
}

/// Works out an arc from its bounding box and the two points that define
/// its start and end rays.  Equal points make a full ellipse.  `increasing`
/// is true when the sweep goes through increasing logical angles.
pub fn arc_spec(bbox: [f64; 4], start: (f64, f64), end: (f64, f64), increasing: bool) -> Option<ArcSpec> {
    let (l, r) = (bbox[0].min(bbox[2]), bbox[0].max(bbox[2]));
    let (t, b) = (bbox[1].min(bbox[3]), bbox[1].max(bbox[3]));
    let centre = ((l + r) / 2.0, (t + b) / 2.0);
    let radii = ((r - l) / 2.0, (b - t) / 2.0);
    if radii.0 <= 0.0 || radii.1 <= 0.0 {
        return None;
    }
    let angle = |p: (f64, f64)| {
        let (dx, dy) = (p.0 - centre.0, p.1 - centre.1);
        if dx == 0.0 && dy == 0.0 { 0.0 } else { parametric_angle(radii.0, radii.1, dx, dy) }
    };
    let (a0, a1) = (angle(start), angle(end));
    let mut sweep = a1 - a0;
    if increasing {
        while sweep <= 0.0 {
            sweep += TAU;
        }
    } else {
        while sweep >= 0.0 {
            sweep -= TAU;
        }
    }
    if sweep.abs() > TAU {
        sweep = TAU.copysign(sweep);
    }
    Some(ArcSpec { centre, radii, start: a0, sweep })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(g: Geo) -> [f32; 4] {
        let p = g.finish().unwrap();
        let b = p.bounds();
        [b.left(), b.top(), b.right(), b.bottom()]
    }

    #[test]
    fn ellipse_fills_its_box() {
        let mut g = Geo::new(Affine::IDENTITY);
        g.ellipse(10.0, 20.0, 30.0, 60.0);
        let b = bounds(g);
        assert!((b[0] - 10.0).abs() < 0.01 && (b[2] - 30.0).abs() < 0.01);
        assert!((b[1] - 20.0).abs() < 0.01 && (b[3] - 60.0).abs() < 0.01);
    }

    #[test]
    fn quarter_arc_goes_the_requested_way() {
        // From the right-hand point to the lowest point, increasing angle:
        // a quarter turn through the lower right in y-down coordinates.
        let spec = arc_spec([0.0, 0.0, 20.0, 20.0], (20.0, 10.0), (10.0, 20.0), true).unwrap();
        assert!((spec.sweep - FRAC_PI_2).abs() < 1e-9, "{}", spec.sweep);
        let spec = arc_spec([0.0, 0.0, 20.0, 20.0], (20.0, 10.0), (10.0, 20.0), false).unwrap();
        assert!((spec.sweep + 3.0 * FRAC_PI_2).abs() < 1e-9, "{}", spec.sweep);
        let mut g = Geo::new(Affine::IDENTITY);
        g.arc(spec.centre, spec.radii, spec.start, spec.sweep, false);
        let b = bounds(g);
        assert!(b[0] < 0.1 && b[1] < 0.1, "the long way round reaches the top left: {b:?}");
    }

    #[test]
    fn equal_points_make_a_full_turn() {
        let spec = arc_spec([0.0, 0.0, 10.0, 10.0], (10.0, 5.0), (10.0, 5.0), true).unwrap();
        assert!((spec.sweep - TAU).abs() < 1e-9);
    }

    #[test]
    fn elliptical_arcs_use_parametric_angles() {
        // A point on the x axis of a wide ellipse is at angle zero however
        // far out it is.
        let spec = arc_spec([0.0, 0.0, 40.0, 10.0], (100.0, 5.0), (20.0, 100.0), true).unwrap();
        assert!(spec.start.abs() < 1e-9);
        assert!((spec.sweep - FRAC_PI_2).abs() < 1e-9);
    }

    #[test]
    fn degenerate_shapes_are_empty_not_panics() {
        assert!(arc_spec([0.0, 0.0, 0.0, 10.0], (0.0, 0.0), (1.0, 1.0), true).is_none());
        let mut g = Geo::new(Affine::IDENTITY);
        g.round_rect(0.0, 0.0, 0.0, 0.0, 5.0, 5.0);
        let _ = g.finish();
        let mut g = Geo::new(Affine::scale(1e30, 1e30));
        g.polygon(&[(1.0, 1.0), (1e30, 5.0), (3.0, 4.0)]);
        let _ = g.finish();
    }

    #[test]
    fn round_rect_stays_inside_its_box() {
        let mut g = Geo::new(Affine::IDENTITY);
        g.round_rect(0.0, 0.0, 100.0, 50.0, 30.0, 30.0);
        let b = bounds(g);
        assert!(b[0] >= -0.01 && b[1] >= -0.01 && b[2] <= 100.01 && b[3] <= 50.01, "{b:?}");
    }
}
