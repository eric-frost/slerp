//! Geometry helpers for DXF curves: the arbitrary-axis rule that turns an
//! entity's own plane into world coordinates, polyline bulges, ellipse arcs
//! and B-splines.

use super::entities::Spline;
use glam::{DMat3, DMat4, DVec2, DVec3};
use std::f64::consts::{PI, TAU};

/// The largest angle one segment of a flattened arc covers: 5 degrees, a
/// sagitta of 0.1 % of the radius.
pub const ARC_STEP: f64 = PI / 36.0;

/// True for coordinates a drawing can sensibly hold.  DXF files from
/// broken exporters contain NaN, infinities and values like 1e300 that
/// would ruin every later calculation.
pub fn sane(p: DVec3) -> bool {
    p.x.abs() < 1e13 && p.y.abs() < 1e13 && p.z.abs() < 1e13
}

/// The object coordinate system of an entity with extrusion direction
/// `n`: the basis the AutoCAD "arbitrary axis algorithm" gives, so that
/// the entity's x and y coordinates lie in the plane perpendicular to `n`.
pub fn ocs(n: DVec3) -> DMat3 {
    if (n - DVec3::Z).length_squared() < 1e-18 {
        return DMat3::IDENTITY;
    }
    let ax = if n.x.abs() < 1.0 / 64.0 && n.y.abs() < 1.0 / 64.0 { DVec3::Y.cross(n) } else { DVec3::Z.cross(n) };
    let ax = ax.try_normalize().unwrap_or(DVec3::X);
    let ay = n.cross(ax).try_normalize().unwrap_or(DVec3::Y);
    DMat3::from_cols(ax, ay, n)
}

pub fn ocs4(n: DVec3) -> DMat4 {
    DMat4::from_mat3(ocs(n))
}

/// Segments to flatten an arc sweeping `sweep` radians into.
pub fn arc_segments(sweep: f64) -> usize {
    ((sweep.abs() / ARC_STEP).ceil() as usize).clamp(1, 4096)
}

/// Points along the arc from `p0` to `p1` that a polyline bulge describes,
/// not including `p0` but including `p1`.  The bulge is the tangent of a
/// quarter of the arc's angle, positive when the arc runs counterclockwise.
pub fn bulge_points(p0: DVec2, p1: DVec2, bulge: f64, out: &mut Vec<DVec2>) {
    let chord = p1 - p0;
    let d = chord.length();
    if bulge.abs() < 1e-9 || d < 1e-12 || !bulge.is_finite() {
        out.push(p1);
        return;
    }
    let theta = 4.0 * bulge.atan();
    let mid = (p0 + p1) * 0.5;
    let left = DVec2::new(-chord.y, chord.x) / d;
    let centre = mid + left * (d * 0.5 * (1.0 - bulge * bulge) / (2.0 * bulge));
    let r = d * 0.5 * (1.0 + bulge * bulge) / (2.0 * bulge.abs());
    let a0 = (p0 - centre).to_angle();
    let n = arc_segments(theta);
    for i in 1..n {
        let a = a0 + theta * i as f64 / n as f64;
        out.push(centre + DVec2::new(a.cos(), a.sin()) * r);
    }
    out.push(p1);
}

/// The counterclockwise angle from `t0` to `t1`, a full turn when they
/// coincide or are a turn apart.
pub fn ellipse_sweep(t0: f64, t1: f64) -> f64 {
    if !t0.is_finite() || !t1.is_finite() {
        return TAU;
    }
    let mut s = (t1 - t0) % TAU;
    if s < 0.0 {
        s += TAU;
    }
    if s < 1e-9 { TAU } else { s }
}

//region Splines

/// The most points a spline is flattened into.
const MAX_SPLINE_POINTS: usize = 4096;

/// Flattens a spline into points in drawing order: from its control points
/// and knots when it has them, otherwise through its fit points.
pub fn spline_points(s: &Spline, out: &mut Vec<DVec3>) {
    let n = s.ctrl.len();
    if n >= 2 {
        let degree = (s.degree.max(1) as usize).min(n - 1).min(11);
        let knots = if s.knots.len() == n + degree + 1 && s.knots.windows(2).all(|w| w[0] <= w[1]) {
            s.knots.clone()
        } else {
            clamped_knots(n, degree)
        };
        let rational = s.flags & 4 != 0 && s.weights.len() == n && s.weights.iter().all(|w| *w > 0.0);
        let weights = if rational { Some(s.weights.as_slice()) } else { None };
        nurbs_points(&s.ctrl, weights, &knots, degree, out);
    } else if s.fit.len() >= 2 {
        fit_points(&s.fit, s.flags & 1 != 0, out);
    }
}

/// Knots of an open (clamped) uniform B-spline.
fn clamped_knots(n: usize, degree: usize) -> Vec<f64> {
    let mut k = vec![0.0; degree + 1];
    let spans = n - degree;
    for i in 1..spans {
        k.push(i as f64);
    }
    k.extend(std::iter::repeat_n(spans as f64, degree + 1));
    k
}

/// The span index `i` with `knots[i] <= u < knots[i + 1]`, for a spline of
/// `n` control points and `degree`.
fn find_span(knots: &[f64], n: usize, degree: usize, u: f64) -> usize {
    if u >= knots[n] {
        return n - 1;
    }
    if u <= knots[degree] {
        return degree;
    }
    let (mut lo, mut hi) = (degree, n);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if u < knots[mid] {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    lo
}

/// The `degree + 1` basis functions that are not zero at `u` in span
/// `span` (the Cox-de Boor recurrence).
fn basis(knots: &[f64], span: usize, degree: usize, u: f64) -> [f64; 12] {
    let mut n = [0.0; 12];
    let mut left = [0.0; 12];
    let mut right = [0.0; 12];
    n[0] = 1.0;
    for j in 1..=degree {
        left[j] = u - knots[span + 1 - j];
        right[j] = knots[span + j] - u;
        let mut saved = 0.0;
        for r in 0..j {
            let denom = right[r + 1] + left[j - r];
            let temp = if denom.abs() < 1e-300 { 0.0 } else { n[r] / denom };
            n[r] = saved + right[r + 1] * temp;
            saved = left[j - r] * temp;
        }
        n[j] = saved;
    }
    n
}

fn nurbs_points(ctrl: &[DVec3], weights: Option<&[f64]>, knots: &[f64], degree: usize, out: &mut Vec<DVec3>) {
    let n = ctrl.len();
    let (u0, u1) = (knots[degree], knots[n]);
    if u1 <= u0 || !u0.is_finite() || !u1.is_finite() {
        // A degenerate knot vector: the control polygon is the best guess.
        out.extend(ctrl.iter().copied());
        return;
    }
    let samples = ((n - degree) * 16).clamp(32, MAX_SPLINE_POINTS);
    for i in 0..=samples {
        let u = if i == samples { u1 } else { u0 + (u1 - u0) * i as f64 / samples as f64 };
        let span = find_span(knots, n, degree, u);
        let b = basis(knots, span, degree, u);
        let mut p = DVec3::ZERO;
        let mut w = 0.0;
        for (j, bj) in b.iter().take(degree + 1).enumerate() {
            let idx = span - degree + j;
            let wt = weights.map_or(1.0, |ws| ws[idx]);
            p += ctrl[idx] * (bj * wt);
            w += bj * wt;
        }
        if w.abs() > 1e-300 {
            out.push(p / w);
        }
    }
}

/// A smooth curve through the fit points: a centripetal Catmull-Rom spline,
/// which doesn't overshoot at uneven spacing.
fn fit_points(fit: &[DVec3], closed: bool, out: &mut Vec<DVec3>) {
    let n = fit.len();
    let at = |i: isize| -> DVec3 {
        if closed {
            fit[i.rem_euclid(n as isize) as usize]
        } else if i < 0 {
            2.0 * fit[0] - fit[1]
        } else if i >= n as isize {
            2.0 * fit[n - 1] - fit[n - 2]
        } else {
            fit[i as usize]
        }
    };
    let segments = if closed { n } else { n - 1 };
    let per = (MAX_SPLINE_POINTS / segments.max(1)).clamp(2, 16);
    out.push(fit[0]);
    for s in 0..segments as isize {
        let (p0, p1, p2, p3) = (at(s - 1), at(s), at(s + 1), at(s + 2));
        let d = |a: DVec3, b: DVec3| a.distance(b).sqrt().max(1e-9);
        let t0 = 0.0;
        let t1 = t0 + d(p0, p1);
        let t2 = t1 + d(p1, p2);
        let t3 = t2 + d(p2, p3);
        for k in 1..=per {
            let tt = t1 + (t2 - t1) * k as f64 / per as f64;
            let a1 = p0 * ((t1 - tt) / (t1 - t0)) + p1 * ((tt - t0) / (t1 - t0));
            let a2 = p1 * ((t2 - tt) / (t2 - t1)) + p2 * ((tt - t1) / (t2 - t1));
            let a3 = p2 * ((t3 - tt) / (t3 - t2)) + p3 * ((tt - t2) / (t3 - t2));
            let b1 = a1 * ((t2 - tt) / (t2 - t0)) + a2 * ((tt - t0) / (t2 - t0));
            let b2 = a2 * ((t3 - tt) / (t3 - t1)) + a3 * ((tt - t1) / (t3 - t1));
            out.push(b1 * ((t2 - tt) / (t2 - t1)) + b2 * ((tt - t1) / (t2 - t1)));
        }
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: DVec3, b: DVec3) -> bool {
        (a - b).length() < 1e-9
    }

    #[test]
    fn ocs_follows_the_arbitrary_axis_rule() {
        assert_eq!(ocs(DVec3::Z), DMat3::IDENTITY);
        // Looking down the negative z axis mirrors x.
        let m = ocs(-DVec3::Z);
        assert!(near(m * DVec3::X, -DVec3::X), "{:?}", m * DVec3::X);
        assert!(near(m * DVec3::Y, DVec3::Y));
        // For an entity facing +x the rule gives x = Z cross N = +y, and the
        // basis is right-handed.
        let m = ocs(DVec3::X);
        assert!(near(m.x_axis, DVec3::Y));
        assert!(near(m.x_axis.cross(m.y_axis), m.z_axis));
        let m = ocs(DVec3::new(1.0, 2.0, 3.0).normalize());
        assert!((m.determinant() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_bulge_of_one_is_a_semicircle() {
        let mut pts = Vec::new();
        bulge_points(DVec2::new(0.0, 0.0), DVec2::new(2.0, 0.0), 1.0, &mut pts);
        assert_eq!(pts.len(), arc_segments(PI));
        // Counterclockwise from (0,0) to (2,0) passes below the chord.
        let lowest = pts.iter().map(|p| p.y).fold(f64::MAX, f64::min);
        assert!((lowest + 1.0).abs() < 1e-3, "{lowest}");
        assert!(pts.iter().all(|p| (p.distance(DVec2::new(1.0, 0.0)) - 1.0).abs() < 1e-9));
        assert_eq!(*pts.last().unwrap(), DVec2::new(2.0, 0.0));
        // A negative bulge goes the other way.
        let mut pts = Vec::new();
        bulge_points(DVec2::new(0.0, 0.0), DVec2::new(2.0, 0.0), -1.0, &mut pts);
        let highest = pts.iter().map(|p| p.y).fold(f64::MIN, f64::max);
        assert!((highest - 1.0).abs() < 1e-3);
        // A shallow bulge: a quarter of the angle has tangent bulge.
        let mut pts = Vec::new();
        bulge_points(DVec2::new(0.0, 0.0), DVec2::new(2.0, 0.0), 0.5, &mut pts);
        let lowest = pts.iter().map(|p| p.y).fold(f64::MAX, f64::min);
        // Sagitta = bulge * chord / 2.
        assert!((lowest + 0.5).abs() < 2e-3, "{lowest}");
        // No bulge, no points added.
        let mut pts = Vec::new();
        bulge_points(DVec2::ZERO, DVec2::X, 0.0, &mut pts);
        assert_eq!(pts, vec![DVec2::X]);
        bulge_points(DVec2::ZERO, DVec2::ZERO, 1.0, &mut pts);
        bulge_points(DVec2::ZERO, DVec2::X, f64::NAN, &mut pts);
    }

    #[test]
    fn ellipse_sweeps() {
        // Equal parameters mean a full ellipse; a quarter is a quarter.
        assert_eq!(ellipse_sweep(1.0, 1.0), TAU);
        assert!((ellipse_sweep(0.0, PI / 2.0) - PI / 2.0).abs() < 1e-12);
        assert!((ellipse_sweep(6.0, 0.5) - (TAU - 5.5)).abs() < 1e-12);
        assert_eq!(ellipse_sweep(f64::NAN, 1.0), TAU);
    }

    #[test]
    fn quadratic_bezier_spline() {
        let s = Spline {
            degree: 2,
            knots: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            ctrl: vec![DVec3::new(0.0, 0.0, 0.0), DVec3::new(1.0, 2.0, 0.0), DVec3::new(2.0, 0.0, 0.0)],
            ..Default::default()
        };
        let mut pts = Vec::new();
        spline_points(&s, &mut pts);
        assert!(near(pts[0], s.ctrl[0]));
        assert!(near(*pts.last().unwrap(), s.ctrl[2]));
        // The midpoint of a quadratic Bezier is (P0 + 2 P1 + P2) / 4.
        let mid = pts[pts.len() / 2];
        assert!(near(mid, DVec3::new(1.0, 1.0, 0.0)), "{mid:?}");
    }

    #[test]
    fn rational_spline_makes_a_circle_quadrant() {
        // A quarter circle as a rational quadratic: weight sqrt(2)/2 in the middle.
        let w = std::f64::consts::FRAC_1_SQRT_2;
        let s = Spline {
            degree: 2,
            flags: 4,
            knots: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            weights: vec![1.0, w, 1.0],
            ctrl: vec![DVec3::new(1.0, 0.0, 0.0), DVec3::new(1.0, 1.0, 0.0), DVec3::new(0.0, 1.0, 0.0)],
            ..Default::default()
        };
        let mut pts = Vec::new();
        spline_points(&s, &mut pts);
        assert!(pts.len() > 10);
        assert!(pts.iter().all(|p| (p.length() - 1.0).abs() < 1e-9));
    }

    #[test]
    fn splines_with_missing_or_broken_knots_still_draw() {
        let ctrl: Vec<DVec3> = (0..6).map(|i| DVec3::new(i as f64, (i % 2) as f64, 0.0)).collect();
        for knots in [vec![], vec![0.0, 1.0], vec![5.0, 4.0, 3.0, 2.0, 1.0, 0.0, 0.0, 0.0, 0.0], vec![f64::NAN; 10]] {
            let s = Spline { degree: 3, knots, ctrl: ctrl.clone(), ..Default::default() };
            let mut pts = Vec::new();
            spline_points(&s, &mut pts);
            assert!(pts.len() >= 6, "{}", pts.len());
            assert!(pts.iter().all(|p| p.is_finite()));
            assert!(near(pts[0], ctrl[0]) && near(*pts.last().unwrap(), ctrl[5]));
        }
        // Degree higher than the points allow, and absurd degrees.
        let s = Spline { degree: 99, ctrl: ctrl[..3].to_vec(), ..Default::default() };
        let mut pts = Vec::new();
        spline_points(&s, &mut pts);
        assert!(!pts.is_empty());
        let s = Spline { degree: -4, ctrl: ctrl.clone(), ..Default::default() };
        spline_points(&s, &mut Vec::new());
    }

    #[test]
    fn fit_point_splines_pass_through_their_points() {
        let fit: Vec<DVec3> = vec![
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(1.0, 1.0, 0.0),
            DVec3::new(3.0, 0.0, 0.0),
            DVec3::new(4.0, 2.0, 0.0),
        ];
        let s = Spline { fit: fit.clone(), ..Default::default() };
        let mut pts = Vec::new();
        spline_points(&s, &mut pts);
        for f in &fit {
            assert!(pts.iter().any(|p| near(*p, *f)), "{f:?}");
        }
        let s = Spline { fit, flags: 1, ..Default::default() };
        let mut closed = Vec::new();
        spline_points(&s, &mut closed);
        assert!(near(closed[0], *closed.last().unwrap()));
    }

    #[test]
    fn many_control_points_stay_bounded() {
        let ctrl: Vec<DVec3> = (0..100_000).map(|i| DVec3::new(i as f64, 0.0, 0.0)).collect();
        let s = Spline { degree: 3, ctrl, ..Default::default() };
        let mut pts = Vec::new();
        spline_points(&s, &mut pts);
        assert!(pts.len() <= MAX_SPLINE_POINTS + 1);
    }
}
