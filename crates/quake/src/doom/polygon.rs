//! Floors and ceilings: each sector's area as triangles.
//!
//! Doom never stores sector shapes.  A sector is whatever its linedefs
//! enclose, so its boundary is traced from the lines that name it on one
//! side, the loops are sorted into outer edges and holes by nesting, and
//! each outer loop is triangulated together with its holes by earcut.
//!
//! Real maps break this: sectors left unclosed, lines whose side points at
//! the wrong sector, sectors touching themselves at a vertex.  Doom itself
//! draws floors from its BSP instead, whose convex subsectors cover the map
//! no matter how the lines are tangled.  So a sector whose loops don't
//! close is drawn from its subsectors when the map has vanilla nodes: each
//! one rebuilt by clipping the map's bounding box with the partition lines
//! above it and the segs around it.

use super::map::{Bsp, MapData, SUBSECTOR};
use std::collections::HashMap;

/// A sector's area as triangles in map coordinates (x east, y north),
/// counter-clockwise seen from above.
#[derive(Default)]
pub struct Area {
    pub points: Vec<[f64; 2]>,
    pub triangles: Vec<[u32; 3]>,
    /// The area the traced loops enclose (outer loops less their holes),
    /// for checking the triangulation.
    pub loop_area: f64,
    /// Whether the loops didn't close (and the BSP drew the sector instead,
    /// or nothing could).
    pub broken: bool,
    /// Whether the BSP's subsectors drew the sector: its loops didn't
    /// close, or they covered islands of other sectors.
    pub from_bsp: bool,
}

impl Area {
    pub fn triangle_area(&self) -> f64 {
        self.triangles
            .iter()
            .map(|t| tri_area(self.points[t[0] as usize], self.points[t[1] as usize], self.points[t[2] as usize]))
            .sum()
    }

    /// Whether the point lies on one of the triangles.
    pub fn contains(&self, p: [f64; 2]) -> bool {
        self.triangles.iter().any(|t| {
            let [a, b, c] = t.map(|i| self.points[i as usize]);
            let (d1, d2, d3) = (cross(a, b, p), cross(b, c, p), cross(c, a, p));
            let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
            let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
            !(neg && pos)
        })
    }
}

fn cross(o: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

fn tri_area(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    cross(a, b, c).abs() * 0.5
}

/// Twice the signed area: positive for counter-clockwise loops.
fn signed_area(points: &[[f64; 2]]) -> f64 {
    let mut s = 0.0;
    for i in 0..points.len() {
        let (a, b) = (points[i], points[(i + 1) % points.len()]);
        s += a[0] * b[1] - b[0] * a[1];
    }
    s * 0.5
}

/// Even-odd point in polygon.
fn inside(p: [f64; 2], poly: &[[f64; 2]]) -> bool {
    let mut c = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + n - 1) % n]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
            c = !c;
        }
    }
    c
}

/// Every sector's area, in sector order.
pub fn sector_areas(map: &MapData) -> Vec<Area> {
    let mut areas: Vec<Area> = trace_all(map);
    let Some(bsp) = &map.bsp else { return areas };
    // 1. Each sector's area by the BSP, which the game draws floors from.
    let subs = subsector_polygons(map, bsp);
    let mut bsp_area = vec![0.0f64; areas.len()];
    for (sector, poly) in &subs {
        if let Some(a) = bsp_area.get_mut(*sector) {
            *a += signed_area(poly);
        }
    }
    // 2. Sectors whose loops didn't close (or that no line bounds), and
    //    sectors whose loops cover more or less than the BSP says, come
    //    from the BSP.  The second kind are islands of other sectors inside
    //    a sector that borders none of their lines, which tracing can't
    //    cut out (Doom's maps use them for tricks such as TNT's MAP02).
    let mut redo = vec![false; areas.len()];
    for (s, a) in areas.iter().enumerate() {
        let tri = a.triangle_area();
        let differs = (tri - bsp_area[s]).abs() > 0.01 * tri.max(bsp_area[s]) + 64.0;
        redo[s] = a.broken || (differs && bsp_area[s] > 0.0);
    }
    for (sector, poly) in subs {
        let Some(a) = areas.get_mut(sector) else { continue };
        if !redo[sector] {
            continue;
        }
        if !a.from_bsp {
            a.points.clear();
            a.triangles.clear();
            a.from_bsp = true;
        }
        let base = a.points.len() as u32;
        a.points.extend_from_slice(&poly);
        for k in 1..poly.len() as u32 - 1 {
            a.triangles.push([base, base + k, base + k + 1]);
        }
    }
    areas
}

/*┌─────────────────╮
| Tracing loops   |
└─────────────────╯*/
//region Tracing loops

/// Traces every sector's loops and triangulates them.
fn trace_all(map: &MapData) -> Vec<Area> {
    // 1. Vertices that share coordinates become one, so loops close
    //    across duplicated vertices.
    let mut canon: HashMap<(u64, u64), u32> = HashMap::new();
    let mut points: Vec<[f64; 2]> = Vec::new();
    let ids: Vec<u32> = map
        .vertices
        .iter()
        .map(|v| {
            *canon.entry((v[0].to_bits(), v[1].to_bits())).or_insert_with(|| {
                points.push(*v);
                points.len() as u32 - 1
            })
        })
        .collect();

    // 2. Each sector's edges, directed so the sector lies on their left
    //    (a linedef's front side is on its right).  Lines with the same
    //    sector on both sides are inside it and bound nothing.
    let mut edges: Vec<Vec<(u32, u32)>> = vec![Vec::new(); map.sectors.len()];
    for l in &map.lines {
        let (Some(&a), Some(&b)) = (ids.get(l.v1), ids.get(l.v2)) else { continue };
        if a == b {
            continue;
        }
        let (front, back) = (map.sector_of(l.front), map.sector_of(l.back));
        if front == back {
            continue;
        }
        if let Some(f) = front {
            edges[f].push((b, a));
        }
        if let Some(k) = back {
            edges[k].push((a, b));
        }
    }

    edges
        .into_iter()
        .map(|mut e| {
            e.sort_unstable();
            e.dedup();
            sector_area(&points, &e)
        })
        .collect()
}

/// Follows a sector's edges into closed loops and triangulates them.
fn sector_area(points: &[[f64; 2]], edges: &[(u32, u32)]) -> Area {
    let mut area = Area { broken: edges.is_empty(), ..Default::default() };
    let mut out: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, &(a, _)) in edges.iter().enumerate() {
        out.entry(a).or_default().push(i);
    }
    let mut used = vec![false; edges.len()];
    let mut loops: Vec<Vec<u32>> = Vec::new();
    for start in 0..edges.len() {
        if used[start] {
            continue;
        }
        used[start] = true;
        let mut lp = vec![edges[start].0];
        let (origin, mut prev, mut at) = (edges[start].0, edges[start].0, edges[start].1);
        let mut closed = false;
        for _ in 0..edges.len() {
            if at == origin {
                closed = true;
                break;
            }
            lp.push(at);
            // Where the boundary touches itself, take the edge that turns
            // furthest clockwise from the way back, which keeps to this
            // face's side.
            let back = sub(points[prev as usize], points[at as usize]);
            let mut best: Option<(f64, usize)> = None;
            for &e in out.get(&at).map_or(&[][..], |v| v) {
                if used[e] {
                    continue;
                }
                let d = sub(points[edges[e].1 as usize], points[at as usize]);
                let mut turn = back[1].atan2(back[0]) - d[1].atan2(d[0]);
                while turn <= 1e-12 {
                    turn += std::f64::consts::TAU;
                }
                if best.is_none_or(|(t, _)| turn < t) {
                    best = Some((turn, e));
                }
            }
            let Some((_, e)) = best else { break };
            used[e] = true;
            prev = at;
            at = edges[e].1;
        }
        if closed && lp.len() >= 3 {
            loops.extend(split_at_repeats(lp));
        } else {
            area.broken = true;
        }
    }

    // Nesting decides outer loops and holes: a loop inside an odd number of
    // others is a hole, whatever its winding says.
    // Real sectors have at most a few hundred loops; past the cap the
    // quadratic nesting test would take too long, so the BSP draws it.
    if loops.len() > MAX_LOOPS {
        area.broken = true;
        return area;
    }
    let polys: Vec<Vec<[f64; 2]>> = loops.iter().map(|l| l.iter().map(|&i| points[i as usize]).collect()).collect();
    let probes: Vec<[f64; 2]> = polys.iter().map(|p| probe(p)).collect();
    let areas: Vec<f64> = polys.iter().map(|p| signed_area(p).abs()).collect();
    let boxes: Vec<([f64; 2], [f64; 2])> = polys.iter().map(|p| bbox(p)).collect();
    let within = |p: [f64; 2], j: usize| {
        let (lo, hi) = boxes[j];
        p[0] >= lo[0] && p[0] <= hi[0] && p[1] >= lo[1] && p[1] <= hi[1] && inside(p, &polys[j])
    };
    let depth: Vec<usize> = (0..polys.len())
        .map(|i| (0..polys.len()).filter(|&j| j != i && areas[j] > areas[i] && within(probes[i], j)).count())
        .collect();
    for (i, outer) in polys.iter().enumerate() {
        if depth[i] % 2 == 1 || areas[i] <= 0.0 {
            continue;
        }
        // This loop's holes: one level deeper and inside it.
        let holes: Vec<usize> = (0..polys.len())
            .filter(|&j| depth[j] == depth[i] + 1 && areas[j] < areas[i] && within(probes[j], i))
            .collect();
        area.loop_area += areas[i] - holes.iter().map(|&j| areas[j]).sum::<f64>();
        let mut ring: Vec<[f64; 2]> = outer.clone();
        let mut hole_starts = Vec::new();
        for &j in &holes {
            hole_starts.push(ring.len() as u32);
            ring.extend(shrink(&polys[j]));
        }
        if ring.len() > MAX_POINTS {
            area.broken = true;
            continue;
        }
        let tris = earcut(&ring, &hole_starts);
        let base = area.points.len() as u32;
        area.points.extend_from_slice(&ring);
        for t in tris {
            let [a, b, c] = t;
            let (pa, pb, pc) = (ring[a as usize], ring[b as usize], ring[c as usize]);
            // Counter-clockwise from above.
            if cross(pa, pb, pc) >= 0.0 {
                area.triangles.push([base + a, base + b, base + c]);
            } else {
                area.triangles.push([base + a, base + c, base + b]);
            }
        }
    }
    area
}

/// Splits a loop that passes through a vertex more than once (two rooms
/// of one sector meeting at a corner, two pillars touching) into simple
/// loops, which earcut needs.
fn split_at_repeats(lp: Vec<u32>) -> Vec<Vec<u32>> {
    let mut out = Vec::new();
    let mut stack: Vec<u32> = Vec::with_capacity(lp.len());
    let mut at: HashMap<u32, usize> = HashMap::new();
    for v in lp {
        if let Some(&i) = at.get(&v) {
            let tail = stack.split_off(i + 1);
            for t in &tail {
                at.remove(t);
            }
            let mut sub = vec![v];
            sub.extend(tail);
            if sub.len() >= 3 {
                out.push(sub);
            }
        } else {
            at.insert(v, stack.len());
            stack.push(v);
        }
    }
    if stack.len() >= 3 {
        out.push(stack);
    }
    out
}

/// Moves a hole's vertices a thousandth of a unit into the hole, so holes
/// that touch each other or the outer loop share no points: earcut can
/// overlap triangles where they do.  The floor reaching that far under a
/// pillar or over a pit can't be seen.
fn shrink(hole: &[[f64; 2]]) -> Vec<[f64; 2]> {
    const EPS: f64 = 1e-3;
    let n = hole.len();
    // The hole's inside is on the right of its edges when it winds
    // clockwise.
    let side = if signed_area(hole) < 0.0 { 1.0 } else { -1.0 };
    let normal = |a: [f64; 2], b: [f64; 2]| {
        let d = sub(b, a);
        let len = d[0].hypot(d[1]);
        if len > 0.0 { [side * d[1] / len, -side * d[0] / len] } else { [0.0, 0.0] }
    };
    (0..n)
        .map(|i| {
            let (p, c, q) = (hole[(i + n - 1) % n], hole[i], hole[(i + 1) % n]);
            let (n1, n2) = (normal(p, c), normal(c, q));
            let m = [n1[0] + n2[0], n1[1] + n2[1]];
            let len = m[0].hypot(m[1]);
            if len < 1e-9 { c } else { [c[0] + EPS * m[0] / len, c[1] + EPS * m[1] / len] }
        })
        .collect()
}

/// The most loops one sector's lines may form, and the most points one
/// polygon handed to earcut may have: ear clipping takes time by the square
/// of the points or worse (a ring of repeated points takes seconds at a few
/// thousand), and larger sectors are drawn from the BSP instead.
const MAX_LOOPS: usize = 4096;
const MAX_POINTS: usize = 4096;

fn bbox(poly: &[[f64; 2]]) -> ([f64; 2], [f64; 2]) {
    poly.iter().fold(([f64::MAX; 2], [f64::MIN; 2]), |(lo, hi), p| {
        ([lo[0].min(p[0]), lo[1].min(p[1])], [hi[0].max(p[0]), hi[1].max(p[1])])
    })
}

fn sub(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

/// A point just inside a loop, off its longest edge, for nesting tests:
/// vertices are poor probes, since loops of one sector often touch there.
fn probe(poly: &[[f64; 2]]) -> [f64; 2] {
    let n = poly.len();
    let mut best = (0usize, -1.0f64);
    for i in 0..n {
        let d = sub(poly[(i + 1) % n], poly[i]);
        let len = d[0].hypot(d[1]);
        if len > best.1 {
            best = (i, len);
        }
    }
    let (a, b) = (poly[best.0], poly[(best.0 + 1) % n]);
    let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
    // Step a little to the side the loop's winding says is inside.
    let d = sub(b, a);
    let len = best.1.max(1e-9);
    let left = [-d[1] / len, d[0] / len];
    let s = if signed_area(poly) >= 0.0 { 0.01 } else { -0.01 };
    [mid[0] + left[0] * s, mid[1] + left[1] * s]
}

/// Triangulates a ring of points whose holes start at `holes`.  A panic in
/// the triangulator costs this one polygon.
fn earcut(ring: &[[f64; 2]], holes: &[u32]) -> Vec<[u32; 3]> {
    let run = std::panic::catch_unwind(|| {
        let mut e = earcut::Earcut::new();
        let mut out: Vec<u32> = Vec::new();
        e.earcut(ring.iter().copied(), holes, &mut out);
        out
    });
    match run {
        Ok(flat) => flat
            .chunks_exact(3)
            .map(|t| [t[0], t[1], t[2]])
            .filter(|t| t.iter().all(|&i| (i as usize) < ring.len()))
            .collect(),
        Err(_) => Vec::new(),
    }
}

//endregion

/*┌─────────────────╮
| From the BSP    |
└─────────────────╯*/
//region From the BSP

/// The side of a node's partition line a point is on: 0 front (right),
/// 1 back, as Doom's `R_PointOnSide` decides.
fn node_side(n: &super::map::Node, p: [f64; 2]) -> usize {
    if n.dy * (p[0] - n.x) - n.dx * (p[1] - n.y) > 0.0 { 0 } else { 1 }
}

/// The subsector under a point, by walking the BSP as Doom does.
pub fn subsector_at(bsp: &Bsp, p: [f64; 2]) -> Option<usize> {
    if bsp.nodes.is_empty() {
        return (bsp.subsectors.len() == 1).then_some(0);
    }
    let mut at = bsp.nodes.len() as u32 - 1;
    for _ in 0..=bsp.nodes.len() {
        let n = bsp.nodes.get(at as usize)?;
        let child = n.children[node_side(n, p)];
        if child & SUBSECTOR != 0 {
            return Some((child & !SUBSECTOR) as usize);
        }
        at = child;
    }
    None
}

/// The sector a subsector belongs to: its first seg's side's.
pub fn subsector_sector(map: &MapData, bsp: &Bsp, sub: usize) -> Option<usize> {
    let &(first, count) = bsp.subsectors.get(sub)?;
    (first..first.saturating_add(count)).find_map(|s| {
        let seg = bsp.segs.get(s)?;
        let line = map.lines.get(seg.line)?;
        map.sector_of(if seg.side == 0 { line.front } else { line.back })
    })
}

/// Keeps the part of a convex polygon where `f` is at least `-eps`.
fn clip(poly: &[[f64; 2]], f: impl Fn([f64; 2]) -> f64) -> Vec<[f64; 2]> {
    const EPS: f64 = 1e-7;
    let n = poly.len();
    let mut out = Vec::with_capacity(n + 2);
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        let (fa, fb) = (f(a), f(b));
        if fa >= -EPS {
            out.push(a);
        }
        if (fa >= -EPS) != (fb >= -EPS) {
            let t = fa / (fa - fb);
            out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
        }
    }
    out
}

/// Rebuilds every subsector's convex polygon: (sector, polygon
/// counter-clockwise).  Subsectors that clip to nothing are left out.
pub fn subsector_polygons(map: &MapData, bsp: &Bsp) -> Vec<(usize, Vec<[f64; 2]>)> {
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for v in &map.vertices {
        lo = [lo[0].min(v[0]), lo[1].min(v[1])];
        hi = [hi[0].max(v[0]), hi[1].max(v[1])];
    }
    if lo[0] > hi[0] {
        return Vec::new();
    }
    let m = 64.0;
    let world = vec![[lo[0] - m, lo[1] - m], [hi[0] + m, lo[1] - m], [hi[0] + m, hi[1] + m], [lo[0] - m, hi[1] + m]];
    let mut out = Vec::new();
    let leaf = |sub: usize, poly: Vec<[f64; 2]>, out: &mut Vec<(usize, Vec<[f64; 2]>)>| {
        let Some(&(first, count)) = bsp.subsectors.get(sub) else { return };
        let mut poly = poly;
        // The subsector lies on the right of each of its segs.
        for s in first..first.saturating_add(count).min(bsp.segs.len()) {
            let seg = bsp.segs[s];
            let (Some(a), Some(b)) = (map.vertex(seg.v1), map.vertex(seg.v2)) else { continue };
            if a == b {
                continue;
            }
            poly = clip(&poly, |p| -cross(a, b, p));
            if poly.len() < 3 {
                return;
            }
        }
        if let Some(sector) = subsector_sector(map, bsp, sub)
            && signed_area(&poly) > 1e-6
        {
            out.push((sector, poly));
        }
    };
    if bsp.nodes.is_empty() {
        if bsp.subsectors.len() == 1 {
            leaf(0, world, &mut out);
        }
        return out;
    }
    let mut seen = vec![false; bsp.nodes.len()];
    let mut stack = vec![(bsp.nodes.len() as u32 - 1, world)];
    while let Some((at, poly)) = stack.pop() {
        let Some(n) = bsp.nodes.get(at as usize).copied() else { continue };
        if std::mem::replace(&mut seen[at as usize], true) {
            continue;
        }
        let front = clip(&poly, |p| n.dy * (p[0] - n.x) - n.dx * (p[1] - n.y));
        let back = clip(&poly, |p| -(n.dy * (p[0] - n.x) - n.dx * (p[1] - n.y)));
        for (child, part) in [(n.children[0], front), (n.children[1], back)] {
            if part.len() < 3 {
                continue;
            }
            if child & SUBSECTOR != 0 {
                leaf((child & !SUBSECTOR) as usize, part, &mut out);
            } else {
                stack.push((child, part));
            }
        }
    }
    out
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    /// Found by fuzzing (Blender's copy of the bug): a ring of a few
    /// repeated coordinates costs ear clipping time by the square of its
    /// points or worse.  A sector past the cap is left to the BSP.
    #[test]
    fn huge_degenerate_sector_is_left_to_the_bsp_quickly() {
        let n = 50_000u32;
        let points: Vec<[f64; 2]> = (0..n).map(|i| [(i % 3) as f64, ((i / 3) % 3) as f64]).collect();
        let edges: Vec<(u32, u32)> = (0..n).map(|i| (i, (i + 1) % n)).collect();
        let started = std::time::Instant::now();
        let area = sector_area(&points, &edges);
        assert!(area.broken);
        assert!(started.elapsed().as_secs_f32() < 5.0, "{:?}", started.elapsed());
    }
}
