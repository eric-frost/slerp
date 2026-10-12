//! Point clouds: how far apart their points lie, which sizes their dots.
//!
//! Points draw as round dots (`vs_points` and `fs_points` in scene.wgsl):
//! a triangle around each point, cut to a disc with a soft rim.  A dot's
//! diameter is `DOT_OVERLAP` (1.5, in scene.wgsl) times the typical
//! distance between neighbouring points, so a scanned surface looks solid
//! rather than sprinkled, whatever the scan's density.
//! `RenderSettings::point_size` multiplies it, and
//! `RenderSettings::point_world_size` chooses whether
//! dots keep that size in the world (and grow as the camera comes closer)
//! or on screen, where the size is what makes neighbouring dots meet when
//! the whole cloud fills the view.
//!
//! The typical distance comes from the density around a few hundred
//! points: the distance to their eighth-nearest neighbour, found with a
//! uniform grid, says how much surface eight points share, and the spacing
//! is the side of the square each point gets.  (The nearest neighbour alone
//! misleads: in a random scatter it is about half the spacing, in a grid
//! the whole of it.)  Large clouds are measured on a random sample, twice at
//! two sample sizes, which also tells how the spacing shrinks with density
//! (a scanned surface's as the square root of the count, a volume's as the
//! cube root), and so what it is for the whole cloud.

use glam::Vec3;

/// Points measured directly; larger clouds are sampled.
const SAMPLE: usize = 200_000;

/// Points whose neighbours are looked up.
const QUERIES: usize = 512;

/// Which neighbour's distance measures the density.
const K: usize = 8;

/// The typical distance between neighbouring points, or 0 when it can't
/// be told (fewer than two distinct points).
pub(crate) fn point_spacing(positions: &[Vec3]) -> f32 {
    let finite: Vec<Vec3> = positions.iter().copied().filter(|p| p.is_finite()).collect();
    let n = finite.len();
    if n < 2 {
        return 0.0;
    }
    if n <= SAMPLE {
        // Taken as a surface, which nearly every scan is.
        return median_reach(&finite).map_or_else(|| fallback(&finite), |r| spacing(r, 2.0));
    }
    // A random sample at two sizes: how the reach grows when a quarter of
    // the points remain gives the cloud's dimension.
    let big = sample(&finite, SAMPLE);
    let small = sample(&big, SAMPLE / 4);
    let (Some(r_big), Some(r_small)) = (median_reach(&big), median_reach(&small)) else {
        return fallback(&finite);
    };
    let ratio = (r_small / r_big).max(1.0 + 1e-3);
    let dimension = (4f32.ln() / ratio.ln()).clamp(1.0, 3.0);
    spacing(r_big, dimension) * (big.len() as f32 / n as f32).powf(1.0 / dimension)
}

/// The spacing of points of a `dimension`-dimensional cloud whose `K`th
/// neighbours lie `reach` away: the side of the cell each point gets when
/// `K` points share a ball of radius `reach`.
fn spacing(reach: f32, dimension: f32) -> f32 {
    // The unit ball's measure in 1, 2 and 3 dimensions, in between linearly.
    let ball = if dimension < 2.0 {
        2.0 + (std::f32::consts::PI - 2.0) * (dimension - 1.0)
    } else {
        std::f32::consts::PI + (4.0 / 3.0 - 1.0) * std::f32::consts::PI * (dimension - 2.0)
    };
    reach * (ball / K as f32).powf(1.0 / dimension)
}

/// A spacing for clouds whose points all coincide with their neighbours:
/// the bounds' area shared out, as if the points covered a surface.
fn fallback(points: &[Vec3]) -> f32 {
    let (min, max) = bounds(points);
    let size = max - min;
    let area = size.x * size.y + size.y * size.z + size.z * size.x;
    (area / points.len() as f32).sqrt()
}

fn bounds(points: &[Vec3]) -> (Vec3, Vec3) {
    points
        .iter()
        .fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(lo, hi), p| (lo.min(*p), hi.max(*p)))
}

/// About `count` points chosen at random (by a hash of their index), so
/// that a scan's regular rows don't line up with the choice.
fn sample(points: &[Vec3], count: usize) -> Vec<Vec3> {
    let keep = (count as u64).saturating_mul(1 << 32) / points.len().max(1) as u64;
    points
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            // SplitMix64's finaliser: every bit of the index stirs every
            // bit of the hash.
            let mut h = (*i as u64).wrapping_add(0x9e37_79b9_7f4a_7c15);
            h = (h ^ (h >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            h = (h ^ (h >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            h ^= h >> 31;
            (h & 0xffff_ffff) < keep
        })
        .map(|(_, p)| *p)
        .collect()
}

/// The median distance from up to `QUERIES` evenly chosen points to their
/// `K`th nearest other point, ignoring exact duplicates.
fn median_reach(points: &[Vec3]) -> Option<f32> {
    let grid = Grid::new(points);
    let step = (points.len() / QUERIES).max(1);
    let mut found: Vec<f32> = (0..points.len()).step_by(step).filter_map(|i| grid.reach(points, i)).collect();
    if found.is_empty() {
        return None;
    }
    let mid = found.len() / 2;
    let (_, median, _) = found.select_nth_unstable_by(mid, f32::total_cmp);
    Some(*median)
}

/// Points sorted into the cells of a uniform grid over their bounds, with
/// about one point per cell for a cloud that fills its box.
struct Grid {
    min: Vec3,
    cell: f32,
    dims: [usize; 3],
    /// Index into `order` of each cell's first point; one entry more than
    /// there are cells.
    starts: Vec<u32>,
    order: Vec<u32>,
}

impl Grid {
    fn new(points: &[Vec3]) -> Grid {
        let (min, max) = bounds(points);
        let size = max - min;
        let floor = size.max_element().max(1e-12) * 1e-3;
        let volume = size.x.max(floor) * size.y.max(floor) * size.z.max(floor);
        let mut cell = (volume / points.len() as f32).cbrt().max(1e-12);
        // Keep the cell count reasonable whatever the shape of the bounds.
        let cells = |c: f32| size.to_array().map(|s| (s / c) as usize + 1);
        while cells(cell).iter().product::<usize>() > 4 * points.len() + 64 {
            cell *= 1.25;
        }
        let dims = cells(cell);
        let index = |p: Vec3| {
            let c = ((p - min) / cell).as_uvec3();
            let c = [c.x as usize, c.y as usize, c.z as usize];
            (c[2].min(dims[2] - 1) * dims[1] + c[1].min(dims[1] - 1)) * dims[0] + c[0].min(dims[0] - 1)
        };
        // A counting sort by cell.
        let total = dims.iter().product::<usize>();
        let mut starts = vec![0u32; total + 1];
        let cells_of: Vec<u32> = points.iter().map(|p| index(*p) as u32).collect();
        for &c in &cells_of {
            starts[c as usize + 1] += 1;
        }
        for i in 0..total {
            starts[i + 1] += starts[i];
        }
        let mut next = starts.clone();
        let mut order = vec![0u32; points.len()];
        for (i, &c) in cells_of.iter().enumerate() {
            order[next[c as usize] as usize] = i as u32;
            next[c as usize] += 1;
        }
        Grid { min, cell, dims, starts, order }
    }

    /// The distance from point `i` to its `K`th nearest other point at a
    /// different place, searching rings of cells outwards until no closer
    /// point can remain.  With fewer than `K` others, the farthest.
    fn reach(&self, points: &[Vec3], i: usize) -> Option<f32> {
        let p = points[i];
        let c = ((p - self.min) / self.cell).as_uvec3();
        let c = [c.x as i64, c.y as i64, c.z as i64].map(|v| v.max(0));
        let c = [0, 1, 2].map(|k| c[k].min(self.dims[k] as i64 - 1));
        let reach = self.dims.iter().copied().max().unwrap_or(1) as i64;
        let in_grid = |v: i64, k: usize| v >= 0 && (v as usize) < self.dims[k];
        // The K smallest squared distances so far, in increasing order.
        let mut best = [f32::INFINITY; K];
        // Isolated points could make the search sweep the whole grid; they
        // are rare enough to leave out of the median.
        let mut budget = 20_000usize;
        for ring in 0..=reach {
            for z in (c[2] - ring..=c[2] + ring).filter(|&z| in_grid(z, 2)) {
                for y in (c[1] - ring..=c[1] + ring).filter(|&y| in_grid(y, 1)) {
                    // Rows inside the ring's shell touch it only at their
                    // two ends.
                    let shell = (z - c[2]).abs() == ring || (y - c[1]).abs() == ring;
                    let step = if shell { 1 } else { 2 * ring as usize };
                    for x in (c[0] - ring..=c[0] + ring).step_by(step).filter(|&x| in_grid(x, 0)) {
                        let cell = (z as usize * self.dims[1] + y as usize) * self.dims[0] + x as usize;
                        let range = self.starts[cell] as usize..self.starts[cell + 1] as usize;
                        budget = budget.saturating_sub(1 + range.len());
                        for &j in &self.order[range] {
                            let d = points[j as usize].distance_squared(p);
                            if d > 0.0 && d < best[K - 1] {
                                let at = best.partition_point(|&b| b <= d);
                                best.copy_within(at..K - 1, at + 1);
                                best[at] = d;
                            }
                        }
                    }
                }
            }
            // Anything in the next ring is at least `ring` cells away.
            if best[K - 1].sqrt() <= ring as f32 * self.cell {
                return Some(best[K - 1].sqrt());
            }
            if budget == 0 {
                break;
            }
        }
        best.iter().rev().find(|d| d.is_finite()).map(|d| d.sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(n: usize, spacing: f32) -> Vec<Vec3> {
        let mut v = Vec::with_capacity(n * n);
        for j in 0..n {
            for i in 0..n {
                // Gently curved, so the grid isn't axis-aligned in 3D.
                let x = i as f32 * spacing;
                v.push(Vec3::new(x, 0.02 * (x * 3.0).sin(), j as f32 * spacing));
            }
        }
        v
    }

    /// Within 15% of the truth (a grid's eighth neighbours are a little
    /// closer than a random scatter's of the same density).
    fn near(d: f32, want: f32) -> bool {
        (d / want - 1.0).abs() < 0.15
    }

    #[test]
    fn spacing_of_a_regular_surface() {
        let d = point_spacing(&plane(200, 0.01));
        assert!(near(d, 0.01), "{d}");
    }

    /// A million points on a surface, measured from samples and
    /// extrapolated.
    #[test]
    fn spacing_of_a_large_surface_from_samples() {
        let d = point_spacing(&plane(1000, 0.002));
        assert!(near(d, 0.002), "{d}");
    }

    /// A random scatter over a square has the spacing its density says.
    #[test]
    fn spacing_of_a_random_surface() {
        let mut seed = 12345u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 40) as f32 / (1u64 << 24) as f32
        };
        let v: Vec<Vec3> = (0..40_000).map(|_| Vec3::new(next(), 0.0, next())).collect();
        let d = point_spacing(&v);
        assert!(near(d, 1.0 / 200.0), "{d}");
    }

    /// Points filling a cube shrink their spacing as the cube root of the
    /// count.
    #[test]
    fn spacing_of_a_volume() {
        let mut v = Vec::new();
        let n = 80;
        for z in 0..n {
            for y in 0..n {
                for x in 0..n {
                    v.push(Vec3::new(x as f32, y as f32, z as f32) * 0.05);
                }
            }
        }
        let d = point_spacing(&v);
        assert!(near(d, 0.05), "{d}");
    }

    #[test]
    fn degenerate_clouds() {
        assert_eq!(point_spacing(&[]), 0.0);
        assert_eq!(point_spacing(&[Vec3::ONE]), 0.0);
        assert_eq!(point_spacing(&[Vec3::ONE; 100]), 0.0);
        assert!(point_spacing(&[Vec3::ZERO, Vec3::X, Vec3::NAN]) > 0.0);
        // A line, taken as a surface: about the spacing along it.
        let line: Vec<Vec3> = (0..1000).map(|i| Vec3::X * i as f32 * 0.5).collect();
        let d = point_spacing(&line);
        assert!(d > 0.3 && d < 1.5, "{d}");
    }
}
