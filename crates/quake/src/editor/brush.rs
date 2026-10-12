//! Brush geometry shared by `.map` and `.vmf` files: a brush is the space
//! behind all of its planes, and each face is the part of a plane that the
//! other planes don't cut away.  Editors store only the planes (three
//! points each) and how textures lie on them; the faces are rebuilt here
//! the way the compilers (qbsp, VBSP) build them, in double precision.

use slerp_scene::glam::{DVec2, DVec3};

/// Points closer to a plane than this (in map units) count as on it.
/// Editors snap to the grid, so real vertices land far closer than this,
/// and anything thinner than this is not a face anyone drew.
pub(crate) const ON_EPSILON: f64 = 0.01;

/// The most planes a brush may have.  Clipping costs the square of the
/// count; real brushes have a handful, vertex-edited ones a few dozen.
pub(crate) const MAX_SIDES: usize = 256;

/// The half-size of the polygon each face starts as, before the other
/// planes cut it down.  It must reach past the whole map; `build` widens it
/// for maps larger than this.
pub(crate) const MIN_WORLD: f64 = 65_536.0;

/// The largest coordinate taken seriously.  Planes further out than this
/// are corrupt, and faces reaching it belong to brushes that aren't closed.
pub(crate) const MAX_WORLD: f64 = 4_194_304.0;

/*┌─────────────────╮
| Planes          |
└─────────────────╯*/
//region Planes

/// A brush side's plane: points `p` with `normal.dot(p) == dist` lie on it,
/// and the brush is on the side the normal points away from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Plane {
    pub normal: DVec3,
    pub dist: f64,
}

impl Plane {
    /// The plane through three points as Quake and Hammer store them:
    /// the normal is `(p0 - p1) x (p2 - p1)`, which points out of the
    /// brush.  `None` when the points are in a line or not finite.
    pub fn from_points(p: [DVec3; 3]) -> Option<Plane> {
        let n = (p[0] - p[1]).cross(p[2] - p[1]);
        let len = n.length();
        if !len.is_finite() || len < 1e-9 || p.iter().any(|v| !v.is_finite() || v.abs().max_element() > MAX_WORLD) {
            return None;
        }
        let normal = n / len;
        Some(Plane { normal, dist: normal.dot(p[1]) })
    }

    /// A plane from its normal and distance (Doom 3's brushes), normalised.
    pub fn from_equation(normal: DVec3, dist: f64) -> Option<Plane> {
        let len = normal.length();
        if !len.is_finite() || len < 1e-9 || !dist.is_finite() || (dist / len).abs() > MAX_WORLD {
            return None;
        }
        Some(Plane { normal: normal / len, dist: dist / len })
    }

    pub fn distance(&self, p: DVec3) -> f64 {
        self.normal.dot(p) - self.dist
    }

    /// True when both planes are the same surface facing the same way.
    pub fn same_as(&self, other: &Plane) -> bool {
        self.normal.dot(other.normal) > 1.0 - 1e-9 && (self.dist - other.dist).abs() < ON_EPSILON
    }
}

//endregion

/*┌─────────────────╮
| Faces           |
└─────────────────╯*/
//region Faces

/// A large square on `plane`, wound clockwise seen from the front as
/// Quake's tools wind faces (qbsp's `BaseWindingForPlane`).  Displacements
/// depend on that winding to find their corners.
fn base_winding(plane: &Plane, size: f64) -> Vec<DVec3> {
    let n = plane.normal;
    let a = n.abs();
    let up = if a.z >= a.x && a.z >= a.y { DVec3::X } else { DVec3::Z };
    let up = (up - n * up.dot(n)).normalize() * size;
    let right = up.cross(n).normalize() * size;
    let org = n * plane.dist;
    vec![org - right + up, org + right + up, org + right - up, org - right - up]
}

/// Keeps the part of `poly` behind `plane` (or on it), as qbsp's
/// `ChopWinding` does.
fn clip(poly: &[DVec3], plane: &Plane) -> Vec<DVec3> {
    let dists: Vec<f64> = poly.iter().map(|&p| plane.distance(p)).collect();
    if dists.iter().all(|&d| d <= ON_EPSILON) {
        return poly.to_vec();
    }
    if dists.iter().all(|&d| d >= -ON_EPSILON) {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(poly.len() + 1);
    for i in 0..poly.len() {
        let (p, d) = (poly[i], dists[i]);
        let j = (i + 1) % poly.len();
        let (q, e) = (poly[j], dists[j]);
        if d <= ON_EPSILON {
            out.push(p);
        }
        // An edge crossing the plane (strictly) gets the crossing point.
        if (d < -ON_EPSILON && e > ON_EPSILON) || (d > ON_EPSILON && e < -ON_EPSILON) {
            let t = d / (d - e);
            let mut m = p + (q - p) * t;
            // Exact on axial planes, as qbsp does, so walls stay straight.
            for k in 0..3 {
                if plane.normal[k] == 1.0 {
                    m[k] = plane.dist;
                } else if plane.normal[k] == -1.0 {
                    m[k] = -plane.dist;
                }
            }
            out.push(m);
        }
    }
    out
}

/// Why a brush has no faces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BrushError {
    /// Too many sides to clip in reasonable time.
    TooManySides,
    /// Its planes don't close a finite volume.
    Unbounded,
    /// It encloses no volume (all faces were cut away).
    Empty,
}

/// The faces of the brush behind `planes`: one polygon per side, or `None`
/// for sides that don't touch the brush (cut away, or repeating an earlier
/// side's plane).  Polygons wind clockwise seen from the front, and
/// vertices shared by faces are welded to one position.  `world` is the
/// half-size of the starting polygons, which must reach past the brush.
pub(crate) fn build(planes: &[Plane], world: f64) -> Result<Vec<Option<Vec<DVec3>>>, BrushError> {
    if planes.len() > MAX_SIDES {
        return Err(BrushError::TooManySides);
    }
    let world = world.clamp(MIN_WORLD, MAX_WORLD);
    let mut faces: Vec<Option<Vec<DVec3>>> = Vec::with_capacity(planes.len());
    for (i, plane) in planes.iter().enumerate() {
        // A repeated plane keeps only its first face.
        if planes[..i].iter().any(|p| p.same_as(plane)) {
            faces.push(None);
            continue;
        }
        let mut poly = base_winding(plane, world);
        for (j, other) in planes.iter().enumerate() {
            if i == j || other.same_as(plane) {
                continue;
            }
            // Facing the other way on the same spot: the brush is flat.
            if other.normal.dot(plane.normal) < -1.0 + 1e-9 && (other.dist + plane.dist).abs() < ON_EPSILON {
                poly.clear();
                break;
            }
            poly = clip(&poly, other);
            if poly.len() < 3 {
                poly.clear();
                break;
            }
        }
        faces.push((poly.len() >= 3).then_some(poly));
    }

    // A face still reaching the starting polygon's edge was never closed off.
    let limit = world * 0.5;
    if faces.iter().flatten().flatten().any(|v| v.abs().max_element() > limit) {
        return Err(BrushError::Unbounded);
    }
    weld(&mut faces);
    if faces.iter().all(Option::is_none) {
        return Err(BrushError::Empty);
    }
    Ok(faces)
}

/// Snaps nearly whole coordinates to whole ones and merges vertices of
/// different faces that lie within `ON_EPSILON` of each other, so faces
/// meet without cracks; then drops repeated vertices and faces that became
/// slivers.
fn weld(faces: &mut [Option<Vec<DVec3>>]) {
    let mut unique: Vec<DVec3> = Vec::new();
    for face in faces.iter_mut() {
        let Some(poly) = face else { continue };
        for v in poly.iter_mut() {
            for k in 0..3 {
                let r = v[k].round();
                if (v[k] - r).abs() < 1e-3 {
                    v[k] = r;
                }
            }
            match unique.iter().find(|u| u.distance_squared(*v) < ON_EPSILON * ON_EPSILON) {
                Some(u) => *v = *u,
                None => unique.push(*v),
            }
        }
        poly.dedup();
        while poly.len() > 1 && poly.first() == poly.last() {
            poly.pop();
        }
        if poly.len() < 3 || area(poly) < ON_EPSILON {
            *face = None;
        }
    }
}

/// A polygon's area, by Newell's method.
pub(crate) fn area(poly: &[DVec3]) -> f64 {
    newell(poly).length() * 0.5
}

/// The (unnormalised) normal of a polygon wound anticlockwise around it,
/// by Newell's method, which works for any planar polygon.
pub(crate) fn newell(poly: &[DVec3]) -> DVec3 {
    let mut n = DVec3::ZERO;
    for (i, a) in poly.iter().enumerate() {
        let b = poly[(i + 1) % poly.len()];
        n += DVec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
    }
    n
}

//endregion

/*┌─────────────────╮
| Texture axes    |
└─────────────────╯*/
//region Texture axes

/// How a face's texture lies on it: texture coordinates are
/// `(s.xyz . p + s.w, t.xyz . p + t.w)` for a point `p`, in texels unless
/// `normalized`, in which case they are already in texture widths.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TexVecs {
    pub s: [f64; 4],
    pub t: [f64; 4],
    pub normalized: bool,
}

impl TexVecs {
    pub fn at(&self, p: DVec3) -> DVec2 {
        let dot = |v: &[f64; 4]| v[0] * p.x + v[1] * p.y + v[2] * p.z + v[3];
        DVec2::new(dot(&self.s), dot(&self.t))
    }

    /// Quake's standard projection: the texture lies flat on whichever of
    /// the floor, ceiling or a wall the face is closest to facing, then is
    /// rotated, scaled and shifted (qbsp's `TextureAxisFromPlane` and its
    /// caller, rotation and all).
    pub fn standard(normal: DVec3, shift: [f64; 2], rotation: f64, scale: [f64; 2]) -> TexVecs {
        const BASE: [[DVec3; 3]; 6] = [
            [DVec3::new(0.0, 0.0, 1.0), DVec3::new(1.0, 0.0, 0.0), DVec3::new(0.0, -1.0, 0.0)],
            [DVec3::new(0.0, 0.0, -1.0), DVec3::new(1.0, 0.0, 0.0), DVec3::new(0.0, -1.0, 0.0)],
            [DVec3::new(1.0, 0.0, 0.0), DVec3::new(0.0, 1.0, 0.0), DVec3::new(0.0, 0.0, -1.0)],
            [DVec3::new(-1.0, 0.0, 0.0), DVec3::new(0.0, 1.0, 0.0), DVec3::new(0.0, 0.0, -1.0)],
            [DVec3::new(0.0, 1.0, 0.0), DVec3::new(1.0, 0.0, 0.0), DVec3::new(0.0, 0.0, -1.0)],
            [DVec3::new(0.0, -1.0, 0.0), DVec3::new(1.0, 0.0, 0.0), DVec3::new(0.0, 0.0, -1.0)],
        ];
        // 1. The base axes of the closest direction; ties go to the earlier
        //    one, so floors win over walls at 45 degrees.
        let mut best = 0;
        let mut best_dot = 0.0;
        for (i, b) in BASE.iter().enumerate() {
            let d = normal.dot(b[0]);
            if d > best_dot {
                best_dot = d;
                best = i;
            }
        }
        let mut vecs = [BASE[best][1], BASE[best][2]];

        // 2. Rotation within the axes' own plane, exact at right angles.
        let (sinv, cosv) = if rotation == 0.0 {
            (0.0, 1.0)
        } else if rotation == 90.0 {
            (1.0, 0.0)
        } else if rotation == 180.0 {
            (0.0, -1.0)
        } else if rotation == 270.0 {
            (-1.0, 0.0)
        } else {
            (rotation.to_radians().sin(), rotation.to_radians().cos())
        };
        let index = |v: DVec3| {
            if v.x != 0.0 {
                0
            } else if v.y != 0.0 {
                1
            } else {
                2
            }
        };
        let (sv, tv) = (index(vecs[0]), index(vecs[1]));
        for v in &mut vecs {
            let (s, t) = (v[sv], v[tv]);
            v[sv] = cosv * s - sinv * t;
            v[tv] = sinv * s + cosv * t;
        }

        // 3. Scale (zero means one) and shift.
        let sc = |s: f64| if s == 0.0 || !s.is_finite() { 1.0 } else { s };
        let (a, b) = (vecs[0] / sc(scale[0]), vecs[1] / sc(scale[1]));
        TexVecs { s: [a.x, a.y, a.z, shift[0]], t: [b.x, b.y, b.z, shift[1]], normalized: false }
    }

    /// Valve's projection (Valve 220 `.map` files and Hammer's `.vmf`):
    /// explicit axes, each divided by its scale, plus a shift in texels.
    pub fn axes(u: DVec3, v: DVec3, shift: [f64; 2], scale: [f64; 2]) -> TexVecs {
        let sc = |s: f64| if s == 0.0 || !s.is_finite() { 1.0 } else { s };
        let (a, b) = (u / sc(scale[0]), v / sc(scale[1]));
        TexVecs { s: [a.x, a.y, a.z, shift[0]], t: [b.x, b.y, b.z, shift[1]], normalized: false }
    }

    /// Quake 3's brush primitives (Radiant's `brushDef`): a 2x3 matrix over
    /// axes derived from the face's normal (Radiant's `ComputeAxisBase`),
    /// giving coordinates in texture widths directly.
    pub fn matrix(normal: DVec3, m: [[f64; 3]; 2]) -> TexVecs {
        let clean = |v: f64| if v.abs() < 1e-6 { 0.0 } else { v };
        let n = DVec3::new(clean(normal.x), clean(normal.y), clean(normal.z));
        let rot_y = -n.z.atan2((n.x * n.x + n.y * n.y).sqrt());
        let rot_z = n.y.atan2(n.x);
        let tex_s = DVec3::new(-rot_z.sin(), rot_z.cos(), 0.0);
        let tex_t = DVec3::new(-rot_y.sin() * rot_z.cos(), -rot_y.sin() * rot_z.sin(), -rot_y.cos());
        let s = tex_s * m[0][0] + tex_t * m[0][1];
        let t = tex_s * m[1][0] + tex_t * m[1][1];
        TexVecs { s: [s.x, s.y, s.z, m[0][2]], t: [t.x, t.y, t.z, m[1][2]], normalized: true }
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    /// The six planes of an axis-aligned box, as an editor writes them.
    pub fn cube(min: DVec3, max: DVec3) -> Vec<Plane> {
        let (a, b) = (min, max);
        let p = |x: [f64; 3], y: [f64; 3], z: [f64; 3]| {
            Plane::from_points([DVec3::from(x), DVec3::from(y), DVec3::from(z)]).unwrap()
        };
        vec![
            // Points in TrenchBroom's order for a box from (a) to (b).
            p([a.x, a.y, a.z], [a.x, a.y + 1.0, a.z], [a.x, a.y, a.z + 1.0]),
            p([a.x, a.y, a.z], [a.x, a.y, a.z + 1.0], [a.x + 1.0, a.y, a.z]),
            p([a.x, a.y, a.z], [a.x + 1.0, a.y, a.z], [a.x, a.y + 1.0, a.z]),
            p([b.x, b.y, b.z], [b.x, b.y + 1.0, b.z], [b.x + 1.0, b.y, b.z]),
            p([b.x, b.y, b.z], [b.x + 1.0, b.y, b.z], [b.x, b.y, b.z + 1.0]),
            p([b.x, b.y, b.z], [b.x, b.y, b.z + 1.0], [b.x, b.y + 1.0, b.z]),
        ]
    }

    fn bounds(faces: &[Option<Vec<DVec3>>]) -> (DVec3, DVec3) {
        let mut lo = DVec3::splat(f64::MAX);
        let mut hi = DVec3::splat(f64::MIN);
        for v in faces.iter().flatten().flatten() {
            lo = lo.min(*v);
            hi = hi.max(*v);
        }
        (lo, hi)
    }

    #[test]
    fn planes_point_out_of_the_brush() {
        let planes = cube(DVec3::new(-16.0, -16.0, 0.0), DVec3::new(16.0, 16.0, 32.0));
        let centre = DVec3::new(0.0, 0.0, 16.0);
        for p in &planes {
            assert!(p.distance(centre) < 0.0, "{p:?}");
        }
        assert!(Plane::from_points([DVec3::ZERO, DVec3::X, DVec3::X * 2.0]).is_none(), "collinear");
        assert!(Plane::from_points([DVec3::ZERO, DVec3::X, DVec3::splat(f64::NAN)]).is_none());
    }

    #[test]
    fn a_cube_has_six_square_faces() {
        let planes = cube(DVec3::new(-16.0, -16.0, 0.0), DVec3::new(16.0, 16.0, 32.0));
        let faces = build(&planes, MIN_WORLD).unwrap();
        assert_eq!(faces.len(), 6);
        for (face, plane) in faces.iter().zip(&planes) {
            let poly = face.as_ref().unwrap();
            assert_eq!(poly.len(), 4);
            assert!((area(poly) - 32.0 * 32.0).abs() < 1e-6);
            // Clockwise seen from the front: Newell's normal points inwards.
            assert!(newell(poly).normalize().dot(plane.normal) < -0.999);
            for v in poly {
                assert!(plane.distance(*v).abs() < 1e-9);
            }
        }
        assert_eq!(bounds(&faces), (DVec3::new(-16.0, -16.0, 0.0), DVec3::new(16.0, 16.0, 32.0)));
    }

    #[test]
    fn a_wedge_has_five_faces() {
        // A box cut diagonally from its top front edge to its bottom back
        // edge.
        let mut planes = cube(DVec3::ZERO, DVec3::splat(64.0));
        planes.push(Plane::from_equation(DVec3::new(0.0, 1.0, 1.0), 64.0).unwrap());
        let faces = build(&planes, MIN_WORLD).unwrap();
        let sizes: Vec<usize> = faces.iter().map(|f| f.as_ref().map_or(0, Vec::len)).collect();
        // The top (+Z) and back (+Y) faces are cut away entirely; the
        // sides become triangles.
        assert_eq!(faces.iter().filter(|f| f.is_some()).count(), 5, "{sizes:?}");
        assert_eq!(sizes.iter().filter(|&&n| n == 3).count(), 2, "{sizes:?}");
    }

    #[test]
    fn degenerate_and_duplicate_planes() {
        // A repeated plane keeps one face.
        let mut planes = cube(DVec3::ZERO, DVec3::splat(32.0));
        planes.push(planes[2]);
        let faces = build(&planes, MIN_WORLD).unwrap();
        assert_eq!(faces.iter().filter(|f| f.is_some()).count(), 6);
        assert!(faces[6].is_none());

        // A box with no height has no faces.
        let flat = cube(DVec3::ZERO, DVec3::new(32.0, 32.0, 0.0));
        assert!(build(&flat, MIN_WORLD).is_err());

        // Only five sides: open at the top.
        let open: Vec<Plane> = cube(DVec3::ZERO, DVec3::splat(32.0)).into_iter().filter(|p| p.normal.z < 0.5).collect();
        assert_eq!(build(&open, MIN_WORLD), Err(BrushError::Unbounded));

        // Planes that exclude each other: nothing.
        let mut inside_out = cube(DVec3::ZERO, DVec3::splat(32.0));
        inside_out[3] = Plane { normal: DVec3::Z, dist: -10.0 };
        assert!(build(&inside_out, MIN_WORLD).is_err());

        let many = vec![Plane { normal: DVec3::Z, dist: 0.0 }; MAX_SIDES + 1];
        assert_eq!(build(&many, MIN_WORLD), Err(BrushError::TooManySides));
    }

    #[test]
    fn far_coordinates_need_a_wider_world() {
        let planes = cube(DVec3::splat(100_000.0), DVec3::splat(100_064.0));
        assert!(build(&planes, MIN_WORLD).is_err());
        let faces = build(&planes, 400_000.0).unwrap();
        assert_eq!(faces.iter().flatten().count(), 6);
    }

    #[test]
    fn standard_projection_matches_qbsp() {
        // A floor: s along +X, t along -Y.
        let v = TexVecs::standard(DVec3::Z, [0.0, 0.0], 0.0, [1.0, 1.0]);
        assert_eq!(v.at(DVec3::new(10.0, 20.0, 5.0)), DVec2::new(10.0, -20.0));
        // A wall facing -Y (north wall in qbsp's table): s along +X, t
        // along -Z, shifted and scaled.
        let v = TexVecs::standard(-DVec3::Y, [8.0, 4.0], 0.0, [2.0, 0.5]);
        assert_eq!(v.at(DVec3::new(10.0, 0.0, 5.0)), DVec2::new(5.0 + 8.0, -10.0 + 4.0));
        // Rotated 90 degrees on the floor: s follows +Y, t follows +X.
        let v = TexVecs::standard(DVec3::Z, [0.0, 0.0], 90.0, [1.0, 1.0]);
        assert_eq!(v.at(DVec3::new(10.0, 20.0, 0.0)), DVec2::new(20.0, 10.0));
        // At 45 degrees between floor and wall, the floor wins.
        let v = TexVecs::standard(DVec3::new(1.0, 0.0, 1.0).normalize(), [0.0, 0.0], 0.0, [1.0, 1.0]);
        assert_eq!(v.at(DVec3::new(3.0, 4.0, 5.0)), DVec2::new(3.0, -4.0));
    }

    #[test]
    fn valve_and_matrix_projections() {
        let v = TexVecs::axes(DVec3::X, -DVec3::Z, [16.0, -8.0], [0.25, 0.5]);
        assert_eq!(v.at(DVec3::new(1.0, 2.0, 3.0)), DVec2::new(4.0 + 16.0, -6.0 - 8.0));
        // Radiant's default matrix on a floor: one texture per 128 units...
        let m = TexVecs::matrix(DVec3::Z, [[1.0 / 128.0, 0.0, 0.0], [0.0, 1.0 / 128.0, 0.0]]);
        assert!(m.normalized);
        let a = m.at(DVec3::new(0.0, 0.0, 0.0));
        let b = m.at(DVec3::new(0.0, 128.0, 0.0));
        // ...along +Y for s on a floor.
        assert!(((b - a).abs() - DVec2::new(1.0, 0.0)).length() < 1e-9, "{a} {b}");
    }
}
