//! The shapes parts are drawn with: a unit block, wedge, corner wedge,
//! cylinder, ball, Roblox's classic head and a pyramid, each filling the
//! cube from -0.5 to 0.5 and stretched to a part's size; and trusses, built
//! at their size because their bars keep their thickness.
//!
//! Orientation follows Roblox: a wedge's slope faces front (-Z) and climbs
//! to its vertical back face at +Z; a corner wedge rises to a vertical edge
//! at (+X, -Z); a cylinder part runs along X, a `CylinderMesh` along Y.

use glam::Vec3;
use std::f32::consts::TAU;
use std::sync::OnceLock;

/// Triangles with a normal per vertex.
#[derive(Clone, Debug, Default)]
pub(crate) struct Unit {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub indices: Vec<u32>,
}

impl Unit {
    /// A flat polygon (convex, corners in order), turned to face away from
    /// `inside`.
    fn flat(&mut self, inside: Vec3, corners: &[Vec3]) {
        let mut n = (corners[1] - corners[0]).cross(corners[2] - corners[0]).normalize_or_zero();
        let mut corners = corners.to_vec();
        if n.dot(corners[0] - inside) < 0.0 {
            n = -n;
            corners.reverse();
        }
        let base = self.positions.len() as u32;
        for &c in &corners {
            self.positions.push(c);
            self.normals.push(n);
        }
        for k in 1..corners.len() as u32 - 1 {
            self.indices.extend_from_slice(&[base, base + k, base + k + 1]);
        }
    }

    #[cfg(test)]
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Shape {
    Block,
    Wedge,
    CornerWedge,
    /// Along X, as a cylinder part.
    CylinderX,
    /// Along Y, as a `CylinderMesh`.
    CylinderY,
    Ball,
    /// Roblox's classic head: a rounded upright cylinder.
    Head,
    Pyramid,
}

/// The unit mesh of a shape, made once.
pub(crate) fn unit(shape: Shape) -> &'static Unit {
    static CELLS: [OnceLock<Unit>; 8] = [const { OnceLock::new() }; 8];
    let i = shape as usize;
    CELLS[i].get_or_init(|| match shape {
        Shape::Block => block(),
        Shape::Wedge => wedge(),
        Shape::CornerWedge => corner_wedge(),
        Shape::CylinderX => cylinder_x(24),
        Shape::CylinderY => {
            let mut u = cylinder_x(24);
            // A quarter turn about Z takes X to Y.
            let turn = |v: Vec3| Vec3::new(-v.y, v.x, v.z);
            u.positions.iter_mut().for_each(|p| *p = turn(*p));
            u.normals.iter_mut().for_each(|n| *n = turn(*n));
            u
        }
        Shape::Ball => ball(24, 12),
        Shape::Head => head(24),
        Shape::Pyramid => pyramid(),
    })
}

const H: f32 = 0.5;

fn block() -> Unit {
    let mut u = Unit::default();
    let c = |x: f32, y: f32, z: f32| Vec3::new(x * H, y * H, z * H);
    let faces = [
        [c(1., -1., -1.), c(1., 1., -1.), c(1., 1., 1.), c(1., -1., 1.)],
        [c(-1., -1., -1.), c(-1., -1., 1.), c(-1., 1., 1.), c(-1., 1., -1.)],
        [c(-1., 1., -1.), c(-1., 1., 1.), c(1., 1., 1.), c(1., 1., -1.)],
        [c(-1., -1., -1.), c(1., -1., -1.), c(1., -1., 1.), c(-1., -1., 1.)],
        [c(-1., -1., 1.), c(1., -1., 1.), c(1., 1., 1.), c(-1., 1., 1.)],
        [c(-1., -1., -1.), c(-1., 1., -1.), c(1., 1., -1.), c(1., -1., -1.)],
    ];
    for f in faces {
        u.flat(Vec3::ZERO, &f);
    }
    u
}

fn wedge() -> Unit {
    let mut u = Unit::default();
    let (btl, bbl, btr, bbr) = (Vec3::new(-H, H, H), Vec3::new(-H, -H, H), Vec3::new(H, H, H), Vec3::new(H, -H, H));
    let (fbl, fbr) = (Vec3::new(-H, -H, -H), Vec3::new(H, -H, -H));
    let inside = (btl + bbl + btr + bbr + fbl + fbr) / 6.0;
    u.flat(inside, &[bbl, bbr, btr, btl]);
    u.flat(inside, &[fbl, fbr, bbr, bbl]);
    u.flat(inside, &[fbl, fbr, btr, btl]);
    u.flat(inside, &[fbl, bbl, btl]);
    u.flat(inside, &[fbr, btr, bbr]);
    u
}

fn corner_wedge() -> Unit {
    let mut u = Unit::default();
    let (fl, fr, br, bl) = (Vec3::new(-H, -H, -H), Vec3::new(H, -H, -H), Vec3::new(H, -H, H), Vec3::new(-H, -H, H));
    let apex = Vec3::new(H, H, -H);
    let inside = (fl + fr + br + bl + apex) / 5.0;
    u.flat(inside, &[fl, fr, br, bl]);
    for (a, b) in [(fl, fr), (fr, br), (br, bl), (bl, fl)] {
        u.flat(inside, &[a, b, apex]);
    }
    u
}

fn pyramid() -> Unit {
    let mut u = Unit::default();
    let (fl, fr, br, bl) = (Vec3::new(-H, -H, -H), Vec3::new(H, -H, -H), Vec3::new(H, -H, H), Vec3::new(-H, -H, H));
    let apex = Vec3::new(0.0, H, 0.0);
    let inside = Vec3::new(0.0, -0.25, 0.0);
    u.flat(inside, &[fl, fr, br, bl]);
    for (a, b) in [(fl, fr), (fr, br), (br, bl), (bl, fl)] {
        u.flat(inside, &[a, b, apex]);
    }
    u
}

/// A cylinder along X with `n` sides, smooth around and flat at the ends.
fn cylinder_x(n: u32) -> Unit {
    let mut u = Unit::default();
    let ring = |i: u32| {
        let a = i as f32 / n as f32 * TAU;
        Vec3::new(0.0, a.cos() * H, a.sin() * H)
    };
    // 1. The side.
    for i in 0..=n {
        let r = ring(i);
        for x in [-H, H] {
            u.positions.push(Vec3::new(x, r.y, r.z));
            u.normals.push(Vec3::new(0.0, r.y, r.z) / H);
        }
    }
    for i in 0..n {
        let a = i * 2;
        // Faces outward: (a, a+2, a+1) winds anticlockwise seen from outside.
        u.indices.extend_from_slice(&[a, a + 2, a + 1, a + 1, a + 2, a + 3]);
    }
    fix_winding(&mut u, 0);
    // 2. The two ends.
    for x in [-H, H] {
        let corners: Vec<Vec3> = (0..n).map(|i| ring(i) + Vec3::new(x, 0.0, 0.0)).collect();
        u.flat(Vec3::ZERO, &corners);
    }
    u
}

/// A sphere of diameter 1 with `around` segments and `rings` rings.
fn ball(around: u32, rings: u32) -> Unit {
    let mut u = Unit::default();
    for r in 0..=rings {
        let theta = r as f32 / rings as f32 * std::f32::consts::PI;
        for s in 0..=around {
            let phi = s as f32 / around as f32 * TAU;
            let n = Vec3::new(theta.sin() * phi.cos(), theta.cos(), theta.sin() * phi.sin());
            u.positions.push(n * H);
            u.normals.push(n);
        }
    }
    let row = around + 1;
    for r in 0..rings {
        for s in 0..around {
            let a = r * row + s;
            let b = a + row;
            u.indices.extend_from_slice(&[a, a + 1, b, a + 1, b + 1, b]);
        }
    }
    fix_winding(&mut u, 0);
    u
}

/// Roblox's head: an upright cylinder whose rims are rounded by a quarter
/// of its width, swept from a profile.
fn head(around: u32) -> Unit {
    // (radius, height, normal in the profile plane) from bottom to top.
    let mut profile: Vec<(f32, f32, [f32; 2])> = vec![(0.0, -H, [0.0, -1.0])];
    let round = 0.25;
    for k in 0..=4 {
        let a = std::f32::consts::FRAC_PI_2 * k as f32 / 4.0;
        let (s, c) = a.sin_cos();
        profile.push((H - round + round * s, -H + round - round * c, [s, -c]));
    }
    for k in 0..=4 {
        let a = std::f32::consts::FRAC_PI_2 * k as f32 / 4.0;
        let (s, c) = a.sin_cos();
        profile.push((H - round + round * c, H - round + round * s, [c, s]));
    }
    profile.push((0.0, H, [0.0, 1.0]));
    let mut u = Unit::default();
    for &(r, y, [nr, ny]) in &profile {
        for s in 0..=around {
            let phi = s as f32 / around as f32 * TAU;
            let (sin, cos) = phi.sin_cos();
            u.positions.push(Vec3::new(r * cos, y, r * sin));
            u.normals.push(Vec3::new(nr * cos, ny, nr * sin).normalize_or(Vec3::Y));
        }
    }
    let row = around + 1;
    for p in 0..profile.len() as u32 - 1 {
        for s in 0..around {
            let a = p * row + s;
            let b = a + row;
            u.indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    fix_winding(&mut u, 0);
    u
}

/// Turns every triangle from `first` on to wind anticlockwise seen from
/// outside, judged by its vertex normals: generated rings are easy to get
/// backwards, and a test checks every shape.
fn fix_winding(u: &mut Unit, first: usize) {
    for t in u.indices[first..].chunks_exact_mut(3) {
        let [a, b, c] = [t[0] as usize, t[1] as usize, t[2] as usize];
        let face = (u.positions[b] - u.positions[a]).cross(u.positions[c] - u.positions[a]);
        let normal = u.normals[a] + u.normals[b] + u.normals[c];
        if face.dot(normal) < 0.0 {
            t.swap(1, 2);
        }
    }
}

/*┌─────────────────╮
| Trusses         |
└─────────────────╯*/
//region Trusses

/// A truss of `size` studs: four rails along its longest axis and bracing
/// every 2 studs, in `style` (0 alternating diagonals, 1 crossed like a
/// bridge, 2 none).  Built in studs around the part's centre.
pub(crate) fn truss(size: Vec3, style: u32) -> Unit {
    let mut u = Unit::default();
    // 1. Lay the truss along local X, then swap axes back at the end.
    let axis = if size.y >= size.x && size.y >= size.z {
        1
    } else if size.z > size.x && size.z > size.y {
        2
    } else {
        0
    };
    // Cyclic swaps, which turn rather than mirror, so faces keep facing out.
    let to_local = |v: Vec3| match axis {
        1 => Vec3::new(v.y, v.z, v.x),
        2 => Vec3::new(v.z, v.x, v.y),
        _ => v,
    };
    let to_part = |v: Vec3| match axis {
        1 => Vec3::new(v.z, v.x, v.y),
        2 => Vec3::new(v.y, v.z, v.x),
        _ => v,
    };
    let size_x = to_local(size);
    let length = size_x.x;
    let (half_y, half_z) = (size_x.y * 0.5, size_x.z * 0.5);
    let bar = 0.125 * size_x.y.min(size_x.z).clamp(0.1, 8.0);
    let (ry, rz) = (half_y - bar, half_z - bar);
    let segments = ((length / 2.0).round() as i64).clamp(1, 256) as u32;
    let step = length / segments as f32;
    let x0 = -length * 0.5;
    let corners = [Vec3::new(0.0, ry, rz), Vec3::new(0.0, ry, -rz), Vec3::new(0.0, -ry, -rz), Vec3::new(0.0, -ry, rz)];
    // 2. Rails.
    for c in corners {
        bar_between(&mut u, c + Vec3::X * x0, c - Vec3::X * x0, bar);
    }
    // 3. Rungs at every segment boundary and bracing across each side.
    for s in 0..=segments {
        let x = Vec3::X * (x0 + s as f32 * step);
        for k in 0..4 {
            bar_between(&mut u, corners[k] + x, corners[(k + 1) % 4] + x, bar * 0.8);
        }
    }
    if style != 2 {
        for s in 0..segments {
            let (xa, xb) = (Vec3::X * (x0 + s as f32 * step), Vec3::X * (x0 + (s + 1) as f32 * step));
            for k in 0..4 {
                let (a, b) = (corners[k], corners[(k + 1) % 4]);
                if style == 1 || s % 2 == 0 {
                    bar_between(&mut u, a + xa, b + xb, bar * 0.7);
                }
                if style == 1 || s % 2 == 1 {
                    bar_between(&mut u, b + xa, a + xb, bar * 0.7);
                }
            }
        }
    }
    // 4. Bars poke out of the ends by their thickness; squeeze the truss
    //    back to its length.
    let reach = u.positions.iter().fold(0.0f32, |m, p| m.max(p.x.abs()));
    let squeeze = if reach > 0.0 && reach.is_finite() { (length * 0.5) / reach } else { 1.0 };
    for p in u.positions.iter_mut() {
        p.x *= squeeze;
        *p = to_part(*p);
    }
    for n in u.normals.iter_mut() {
        *n = to_part(*n);
    }
    u
}

/// A square bar of half-width `w` from `a` to `b`.
fn bar_between(u: &mut Unit, a: Vec3, b: Vec3, w: f32) {
    let d = b - a;
    let Some(dir) = d.try_normalize() else { return };
    let side = if dir.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
    let s = dir.cross(side).normalize() * w;
    let t = dir.cross(s).normalize() * w;
    let inside = (a + b) * 0.5;
    let q = [s + t, s - t, -s - t, -s + t];
    for k in 0..4 {
        let (p, r) = (q[k], q[(k + 1) % 4]);
        u.flat(inside, &[a + p, a + r, b + r, b + p]);
    }
    u.flat(inside, &[a + q[0], a + q[1], a + q[2], a + q[3]]);
    u.flat(inside, &[b + q[0], b + q[1], b + q[2], b + q[3]]);
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    /// Signed volume by the divergence theorem: positive for a closed mesh
    /// whose triangles face outward.
    fn volume(u: &Unit) -> f32 {
        u.indices
            .chunks_exact(3)
            .map(|t| {
                let [a, b, c] = [t[0], t[1], t[2]].map(|i| u.positions[i as usize]);
                a.dot(b.cross(c)) / 6.0
            })
            .sum()
    }

    #[test]
    fn shapes_are_closed_and_face_outward() {
        let cases = [
            (Shape::Block, 1.0),
            (Shape::Wedge, 0.5),
            (Shape::CornerWedge, 1.0 / 3.0),
            (Shape::Pyramid, 1.0 / 3.0),
            (Shape::CylinderX, std::f32::consts::PI / 4.0),
            (Shape::CylinderY, std::f32::consts::PI / 4.0),
            (Shape::Ball, std::f32::consts::PI / 6.0),
        ];
        for (shape, expected) in cases {
            let v = volume(unit(shape));
            assert!((v - expected).abs() < expected * 0.04, "{shape:?}: volume {v}, expected {expected}");
        }
        let head = volume(unit(Shape::Head));
        assert!(head > 0.65 && head < std::f32::consts::PI / 4.0, "head volume {head}");
        for shape in [Shape::Block, Shape::Wedge, Shape::Ball, Shape::Head, Shape::CylinderY] {
            let u = unit(shape);
            assert!(u.positions.iter().all(|p| p.abs().max_element() <= H + 1e-5), "{shape:?} leaves the unit cube");
            assert_eq!(u.positions.len(), u.normals.len());
        }
    }

    #[test]
    fn wedges_slope_to_the_front() {
        let u = unit(Shape::Wedge);
        assert!(u.normals.iter().filter(|n| n.y > 0.1).all(|n| n.z < 0.0));
        let c = unit(Shape::CornerWedge);
        let top = c.positions.iter().max_by(|a, b| a.y.total_cmp(&b.y)).copied().unwrap_or_default();
        assert_eq!(top, Vec3::new(H, H, -H));
    }

    #[test]
    fn trusses_run_along_their_longest_side() {
        let u = truss(Vec3::new(2.0, 10.0, 2.0), 0);
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for &p in &u.positions {
            lo = lo.min(p);
            hi = hi.max(p);
        }
        assert!((hi.y - lo.y - 10.0).abs() < 0.01 && hi.x - lo.x <= 2.01, "{lo} {hi}");
        assert!(volume(&u) > 0.0, "truss volume {}", volume(&u));
        assert!(truss(Vec3::new(f32::NAN, 1e30, 0.0), 1).triangle_count() < 200_000);
    }
}
