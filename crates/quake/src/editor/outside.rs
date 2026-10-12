//! Which faces of a map face the void outside it, as qbsp's "outside
//! filling" finds them: the world's solid brushes seal the level, empty
//! space reachable from beyond the map's bounds is outside, and a face
//! whose front looks only into that space is never seen by a player.  A
//! compiled BSP leaves those faces out, which is why its thumbnails show a
//! level's layout rather than its roof; source maps get the same look by
//! moving them to a hidden node.
//!
//! The space is a coarse grid of cells, each marked solid when any sealing
//! brush overlaps it (erring towards solid, so thin walls still seal).
//! When the outside reaches a point entity (the map "leaks", or the file is
//! a prefab with nothing inside), nothing counts as outside.

use super::brush::Plane;
use slerp_scene::glam::DVec3;

/// Cells along the map's longest side, at most: two million cells at
/// most, flooded in a few milliseconds.
const MAX_CELLS: f64 = 128.0;
/// The smallest cell, in map units.
const MIN_CELL: f64 = 8.0;

const EMPTY: u8 = 0;
const SOLID: u8 = 1;
const OUTSIDE: u8 = 2;

/// A sealing brush: its planes and bounds.
pub(crate) struct Sealer<'a> {
    pub planes: &'a [Plane],
    pub min: DVec3,
    pub max: DVec3,
}

pub(crate) struct Hull {
    origin: DVec3,
    cell: f64,
    dims: [usize; 3],
    cells: Vec<u8>,
}

impl Hull {
    /// Floods the space around `brushes` from outside, or `None` when the
    /// outside reaches one of `points` or no point is inside at all.
    pub fn new(brushes: &[Sealer], points: &[DVec3]) -> Option<Hull> {
        // 1. A grid around every sealing brush, two cells of margin wide.
        let (mut lo, mut hi) = (DVec3::splat(f64::MAX), DVec3::splat(f64::MIN));
        for b in brushes {
            lo = lo.min(b.min);
            hi = hi.max(b.max);
        }
        if brushes.is_empty() || !(lo.is_finite() && hi.is_finite()) || points.is_empty() {
            return None;
        }
        let cell = ((hi - lo).max_element() / MAX_CELLS).max(MIN_CELL);
        let origin = lo - DVec3::splat(cell * 2.0);
        let dims = ((hi - lo) / cell).ceil().as_uvec3().to_array().map(|d| d as usize + 4);
        let count = dims[0].checked_mul(dims[1])?.checked_mul(dims[2])?;
        if count > 16 << 20 {
            return None;
        }
        let mut hull = Hull { origin, cell, dims, cells: vec![EMPTY; count] };

        // 2. Cells any brush overlaps: the corner of the cell nearest each
        //    plane's back is behind it.
        let reach = |n: DVec3| (n.x.abs() + n.y.abs() + n.z.abs()) * cell * 0.5;
        for b in brushes {
            let (c0, c1) = (hull.index3(b.min), hull.index3(b.max));
            for z in c0[2]..=c1[2] {
                for y in c0[1]..=c1[1] {
                    for x in c0[0]..=c1[0] {
                        let centre = hull.centre([x, y, z]);
                        if b.planes.iter().all(|p| p.distance(centre) - reach(p.normal) <= 0.0) {
                            let i = hull.flat([x, y, z]);
                            hull.cells[i] = SOLID;
                        }
                    }
                }
            }
        }

        // 3. Flood the empty cells from the grid's border, which the
        //    margin leaves empty.
        let mut stack: Vec<u32> = Vec::new();
        let [dx, dy, dz] = dims;
        let mut seed = |hull: &mut Hull, c: [usize; 3]| {
            let i = hull.flat(c);
            if hull.cells[i] == EMPTY {
                hull.cells[i] = OUTSIDE;
                stack.push(i as u32);
            }
        };
        for a in 0..dx {
            for b in 0..dy {
                seed(&mut hull, [a, b, 0]);
                seed(&mut hull, [a, b, dz - 1]);
            }
            for b in 0..dz {
                seed(&mut hull, [a, 0, b]);
                seed(&mut hull, [a, dy - 1, b]);
            }
        }
        for a in 0..dy {
            for b in 0..dz {
                seed(&mut hull, [0, a, b]);
                seed(&mut hull, [dx - 1, a, b]);
            }
        }
        let (sx, sy) = (1, dx);
        let sz = dx * dy;
        while let Some(i) = stack.pop() {
            let i = i as usize;
            let (x, y, z) = (i % dx, (i / dx) % dy, i / sz);
            let neighbours = [
                (x > 0).then(|| i - sx),
                (x + 1 < dx).then(|| i + sx),
                (y > 0).then(|| i - sy),
                (y + 1 < dy).then(|| i + sy),
                (z > 0).then(|| i - sz),
                (z + 1 < dz).then(|| i + sz),
            ];
            for n in neighbours.into_iter().flatten() {
                if hull.cells[n] == EMPTY {
                    hull.cells[n] = OUTSIDE;
                    stack.push(n as u32);
                }
            }
        }

        // 4. A leak, or nothing inside: no outside at all.
        let states: Vec<u8> = points.iter().map(|&p| hull.state(p)).collect();
        if states.contains(&OUTSIDE) || !states.contains(&EMPTY) {
            return None;
        }
        Some(hull)
    }

    fn index3(&self, p: DVec3) -> [usize; 3] {
        let i = ((p - self.origin) / self.cell).floor();
        [0, 1, 2].map(|k| (i[k].max(0.0) as usize).min(self.dims[k] - 1))
    }

    fn flat(&self, [x, y, z]: [usize; 3]) -> usize {
        (z * self.dims[1] + y) * self.dims[0] + x
    }

    fn centre(&self, c: [usize; 3]) -> DVec3 {
        self.origin + (DVec3::new(c[0] as f64, c[1] as f64, c[2] as f64) + 0.5) * self.cell
    }

    /// The state of the cell holding `p`; points beyond the grid are
    /// outside.
    fn state(&self, p: DVec3) -> u8 {
        let i = (p - self.origin) / self.cell;
        if i.min_element() < 0.0 || (0..3).any(|k| i[k] >= self.dims[k] as f64) {
            return OUTSIDE;
        }
        self.cells[self.flat(self.index3(p))]
    }

    /// Whether the face `poly` with outward `normal` looks only into the
    /// void: points a cell and a half in front of it (its centre, and its
    /// corners drawn a quarter of the way in) reach outside cells and none
    /// reaches an empty cell inside the level.
    pub fn faces_outside(&self, poly: &[DVec3], normal: DVec3) -> bool {
        if poly.is_empty() {
            return false;
        }
        let centre = poly.iter().copied().sum::<DVec3>() / poly.len() as f64;
        let ahead = normal * self.cell * 1.5;
        let mut any_outside = false;
        for p in std::iter::once(centre).chain(poly.iter().map(|&v| v + (centre - v) * 0.25)) {
            match self.state(p + ahead) {
                EMPTY => return false,
                OUTSIDE => any_outside = true,
                _ => {}
            }
        }
        any_outside
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::brush;

    /// The planes of a box.
    fn cube(min: [f64; 3], max: [f64; 3]) -> Vec<Plane> {
        let p = |n: DVec3, d: f64| Plane { normal: n, dist: d };
        vec![
            p(-DVec3::X, -min[0]),
            p(DVec3::X, max[0]),
            p(-DVec3::Y, -min[1]),
            p(DVec3::Y, max[1]),
            p(-DVec3::Z, -min[2]),
            p(DVec3::Z, max[2]),
        ]
    }

    /// A room 256 units across inside walls 16 thick, as six brushes.
    fn room() -> Vec<(Vec<Plane>, DVec3, DVec3)> {
        let (a, b, t) = (-128.0, 128.0, 16.0);
        let boxes = [
            ([a - t, a - t, a - t], [b + t, b + t, a]),
            ([a - t, a - t, b], [b + t, b + t, b + t]),
            ([a - t, a - t, a], [a, b + t, b]),
            ([b, a - t, a], [b + t, b + t, b]),
            ([a, a - t, a], [b, a, b]),
            ([a, b, a], [b, b + t, b]),
        ];
        boxes.iter().map(|(lo, hi)| (cube(*lo, *hi), DVec3::from(*lo), DVec3::from(*hi))).collect()
    }

    fn sealers(r: &[(Vec<Plane>, DVec3, DVec3)]) -> Vec<Sealer<'_>> {
        r.iter().map(|(p, lo, hi)| Sealer { planes: p, min: *lo, max: *hi }).collect()
    }

    #[test]
    fn a_sealed_room_has_outside_faces_only_outside() {
        let r = room();
        let hull = Hull::new(&sealers(&r), &[DVec3::ZERO]).unwrap();
        let (mut outside, mut inside) = (0, 0);
        for (planes, _, _) in &r {
            let faces = brush::build(planes, brush::MIN_WORLD).unwrap();
            for (f, p) in faces.iter().zip(planes) {
                let Some(f) = f else { continue };
                if hull.faces_outside(f, p.normal) { outside += 1 } else { inside += 1 }
            }
        }
        // Each wall has one face towards the void (6) and one towards the
        // room (6); the rest touch other walls.
        assert_eq!(outside, 6 + 4 * 4 - 4, "outside {outside}, inside {inside}");
        let floor_top =
            [DVec3::new(-100.0, -100.0, -128.0), DVec3::new(100.0, -100.0, -128.0), DVec3::new(100.0, 100.0, -128.0)];
        assert!(!hull.faces_outside(&floor_top, DVec3::Z));
        let floor_bottom =
            [DVec3::new(-100.0, -100.0, -144.0), DVec3::new(100.0, -100.0, -144.0), DVec3::new(100.0, 100.0, -144.0)];
        assert!(hull.faces_outside(&floor_bottom, -DVec3::Z));
    }

    #[test]
    fn leaks_and_empty_insides_find_nothing() {
        let mut r = room();
        assert!(Hull::new(&sealers(&r), &[DVec3::new(1000.0, 0.0, 0.0)]).is_none(), "an entity outside");
        assert!(Hull::new(&sealers(&r), &[]).is_none(), "no entities");
        // A hole in a wall lets the outside in.
        r.remove(3);
        assert!(Hull::new(&sealers(&r), &[DVec3::ZERO]).is_none());
    }
}
