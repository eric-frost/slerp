//! Sloped floors and ceilings.  Every sector's floor and ceiling is a
//! plane `a x + b y + c z + d = 0`; vanilla Doom's are level, and ZDoom's
//! ports tilt them in five ways, applied in the order GZDoom's map loader
//! applies them:
//!
//! 1. UDMF plane equations (`floorplane_a` ... `ceilingplane_d`).
//! 2. `Plane_Align` (181) lines: a sector's plane tilts from the height of
//!    the sector across the line up (or down) to its own height at its
//!    vertex farthest from the line.
//! 3. Slope things: `SlopeFloorPointLine` (9500, 9501) tilts through a line
//!    and the thing; `SetFloorSlope` (9502, 9503) by angles; Vavoom's
//!    (1500, 1501); then vertex heights (things 1504 and 1505, UDMF's
//!    `zfloor` and `zceiling`) for triangular sectors; then
//!    `CopyFloorPlane` (9510, 9511).
//! 4. `Plane_Copy` (118) lines copy planes between tagged sectors.
//!
//! The arithmetic follows GZDoom's `p_slopes.cpp` so the same map tilts
//! the same way.

use super::map::{CEILING, FLOOR, LineDef, MapData, PLANE_ALIGN, PLANE_COPY};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Plane {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
}

impl Plane {
    /// A level plane at height `z`: a floor faces up, a ceiling down.
    pub fn level(z: f64, ceiling: bool) -> Plane {
        if ceiling { Plane { a: 0.0, b: 0.0, c: -1.0, d: z } } else { Plane { a: 0.0, b: 0.0, c: 1.0, d: -z } }
    }

    /// The plane through `p` facing along the unit normal `n`.
    fn through(n: [f64; 3], p: [f64; 3]) -> Plane {
        Plane { a: n[0], b: n[1], c: n[2], d: -(n[0] * p[0] + n[1] * p[1] + n[2] * p[2]) }
    }

    pub fn z_at(&self, x: f64, y: f64) -> f64 {
        -(self.a * x + self.b * y + self.d) / self.c
    }

    pub fn is_slope(&self) -> bool {
        self.a != 0.0 || self.b != 0.0
    }

    /// The unit normal (facing up for floors, down for ceilings).
    pub fn normal(&self) -> [f64; 3] {
        [self.a, self.b, self.c]
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// Normalises a plane normal and turns it up for a floor, down for a
/// ceiling.  `None` for a degenerate (zero or vertical) one.
fn orient(n: [f64; 3], ceiling: bool) -> Option<[f64; 3]> {
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len.is_nan() || len <= 1e-12 || (n[2] / len).abs() < 1e-6 {
        return None;
    }
    let mut n = n.map(|v| v / len);
    if (n[2] < 0.0 && !ceiling) || (n[2] > 0.0 && ceiling) {
        n = n.map(|v| -v);
    }
    Some(n)
}

/// Which side of a line a point is on: 0 front (right of v1 to v2), 1
/// back, as `P_PointOnLineSidePrecise`.
fn side_of(p: [f64; 2], a: [f64; 2], z: [f64; 2]) -> usize {
    let (dx, dy) = (z[0] - a[0], z[1] - a[1]);
    ((p[1] - a[1]) * dx + (a[0] - p[0]) * dy > 1.0 / 65536.0) as usize
}

/// Every sector's floor and ceiling plane, with every way of sloping them
/// applied.  `sector_at` finds the sector under a point, for things.
pub(crate) fn resolve(map: &MapData, sector_at: &dyn Fn(f64, f64) -> Option<usize>) -> [Vec<Plane>; 2] {
    let mut planes: [Vec<Plane>; 2] = [Vec::new(), Vec::new()];
    for (which, list) in planes.iter_mut().enumerate() {
        *list = map
            .sectors
            .iter()
            .map(|s| match s.planes[which] {
                Some([a, b, c, d]) => Plane { a, b, c, d },
                None => Plane::level(if which == CEILING { s.ceiling } else { s.floor }, which == CEILING),
            })
            .collect();
    }
    let lines_of = sector_lines(map);
    let texz = |s: usize, which: usize| if which == CEILING { map.sectors[s].ceiling } else { map.sectors[s].floor };

    // 1. Plane_Align.
    for l in &map.lines {
        let (special, args) = map.hexen_special(l);
        if special != PLANE_ALIGN {
            continue;
        }
        let (Some(front), Some(back)) = (map.sector_of(l.front), map.sector_of(l.back)) else { continue };
        for which in [FLOOR, CEILING] {
            let mut bits = args[which] & 3;
            if which == CEILING && bits == 0 {
                bits = (args[0] >> 2) & 3;
            }
            let (sec, other) = match bits {
                1 => (front, back),
                2 => (back, front),
                _ => continue,
            };
            if let Some(p) = align(map, &lines_of[sec], l, texz(sec, which), texz(other, which), which == CEILING) {
                planes[which][sec] = p;
            }
        }
    }

    // 2. Slope things, then vertex heights, then plane-copying things.
    for t in &map.things {
        let ceiling = match t.kind {
            9500 | 9502 | 1500 => false,
            9501 | 9503 | 1501 => true,
            _ => continue,
        };
        let which = ceiling as usize;
        let Some(sec) = sector_at(t.x, t.y) else { continue };
        match t.kind {
            9500 | 9501 => {
                let pos = [t.x, t.y, planes[which][sec].z_at(t.x, t.y) + t.z];
                slope_line_to_point(map, &mut planes, t.args[0], pos, ceiling);
            }
            9502 | 9503 => {
                let pos = [t.x, t.y, planes[which][sec].z_at(t.x, t.y) + t.z];
                planes[which][sec] = set_slope(t.angle, t.args[0] as f64, pos, ceiling);
            }
            _ => {
                // Vavoom's: z is absolute, and the line is the sector's
                // whose first argument is the thing's ID.
                let pos = [t.x, t.y, t.z];
                if let Some(p) = vavoom(map, &lines_of[sec], t.id, pos, texz(sec, which), ceiling) {
                    planes[which][sec] = p;
                }
            }
        }
    }
    vertex_heights(map, &lines_of, &mut planes);
    for t in &map.things {
        let which = match t.kind {
            9510 => FLOOR,
            9511 => CEILING,
            _ => continue,
        };
        if let (Some(dest), Some(src)) = (sector_at(t.x, t.y), map.tagged(t.args[0]).next()) {
            planes[which][dest] = planes[which][src];
        }
    }

    // 3. Plane_Copy.
    for l in &map.lines {
        let (special, args) = map.hexen_special(l);
        if special != PLANE_COPY {
            continue;
        }
        let (front, back) = (map.sector_of(l.front), map.sector_of(l.back));
        for (s, &tag) in args.iter().take(4).enumerate() {
            let dest = if s & 2 != 0 { back } else { front };
            let which = s & 1;
            if tag != 0
                && let (Some(dest), Some(src)) = (dest, map.tagged(tag).next())
            {
                planes[which][dest] = planes[which][src];
            }
        }
        if let (Some(f), Some(b)) = (front, back) {
            match args[4] & 3 {
                1 => planes[FLOOR][b] = planes[FLOOR][f],
                2 => planes[FLOOR][f] = planes[FLOOR][b],
                _ => {}
            }
            match args[4] & 12 {
                4 => planes[CEILING][b] = planes[CEILING][f],
                8 => planes[CEILING][f] = planes[CEILING][b],
                _ => {}
            }
        }
    }
    planes
}

/// Each sector's lines (either side), in line order, each once.
pub(crate) fn sector_lines(map: &MapData) -> Vec<Vec<usize>> {
    let mut out = vec![Vec::new(); map.sectors.len()];
    for (i, l) in map.lines.iter().enumerate() {
        let (f, b) = (map.sector_of(l.front), map.sector_of(l.back));
        if let Some(f) = f {
            out[f].push(i);
        }
        if let Some(b) = b
            && Some(b) != f
        {
            out[b].push(i);
        }
    }
    out
}

/// `P_AlignPlane`: the plane through `line` at the other sector's height
/// `dest` and through the sector's vertex farthest from the line at its
/// own height `src`.
fn align(map: &MapData, lines: &[usize], line: &LineDef, src: f64, dest: f64, ceiling: bool) -> Option<Plane> {
    let (a, z) = map.ends(line)?;
    let (dx, dy) = (z[0] - a[0], z[1] - a[1]);
    let mut far = map.lines.get(*lines.first()?).and_then(|l| map.vertices.get(l.v1).copied())?;
    let mut best = 0.0;
    for &li in lines {
        let l = &map.lines[li];
        for v in [l.v1, l.v2] {
            let Some(&p) = map.vertices.get(v) else { continue };
            let dist = ((a[1] - p[1]) * dx - (a[0] - p[0]) * dy).abs();
            if dist > best {
                best = dist;
                far = p;
            }
        }
    }
    let v1 = [dx, dy, 0.0];
    let v2 = [far[0] - a[0], far[1] - a[1], src - dest];
    let n = orient(cross(v1, v2), ceiling)?;
    Some(Plane::through(n, [a[0], a[1], dest]))
}

/// `SlopeLineToPoint`: tilts the plane of the sector on the thing's side of
/// each line with ID `id` through that line and the point.
fn slope_line_to_point(map: &MapData, planes: &mut [Vec<Plane>; 2], id: i32, pos: [f64; 3], ceiling: bool) {
    let which = ceiling as usize;
    for l in &map.lines {
        if !l.ids.contains(&id) {
            continue;
        }
        let Some((a, z)) = map.ends(l) else { continue };
        let side = side_of([pos[0], pos[1]], a, z);
        let Some(sec) = map.sector_of(if side == 0 { l.front } else { l.back }) else { continue };
        let plane = planes[which][sec];
        let p = [a[0], a[1], plane.z_at(a[0], a[1])];
        let v1 = [z[0] - a[0], z[1] - a[1], plane.z_at(z[0], z[1]) - p[2]];
        let v2 = sub(pos, p);
        if let Some(n) = orient(cross(v1, v2), ceiling) {
            planes[which][sec] = Plane::through(n, pos);
        }
    }
}

/// `SetSlope`: a plane through `pos` tilted `zang` degrees from vertical
/// (clamped to 1-179), facing compass angle `xyang`.
fn set_slope(xyang: f64, zang: f64, pos: [f64; 3], ceiling: bool) -> Plane {
    let mut zang = zang.clamp(1.0, 179.0);
    if ceiling {
        zang += 180.0;
    }
    let (z, xy) = (zang.to_radians(), xyang.to_radians());
    let n = [z.cos() * xy.cos(), z.cos() * xy.sin(), z.sin()];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    Plane::through(n.map(|v| v / len), pos)
}

/// Vavoom's slope things: through the thing and the sector's line whose
/// first argument is the thing's ID.
fn vavoom(map: &MapData, lines: &[usize], id: i32, pos: [f64; 3], src: f64, ceiling: bool) -> Option<Plane> {
    let l = lines.iter().map(|&i| &map.lines[i]).find(|l| l.args[0] == id)?;
    let (a, z) = map.ends(l)?;
    let v1 = [pos[0] - z[0], pos[1] - z[1], pos[2] - src];
    let v2 = [pos[0] - a[0], pos[1] - a[1], pos[2] - src];
    let n = orient(cross(v1, v2), ceiling)?;
    Some(Plane::through(n, pos))
}

/// Tilts triangular sectors through the heights of their three corners
/// (UDMF's `zfloor` and `zceiling`, or things 1504 and 1505 on a vertex);
/// corners without one stay at the sector's height.
fn vertex_heights(map: &MapData, lines_of: &[Vec<usize>], planes: &mut [Vec<Plane>; 2]) {
    let mut heights = map.vertex_heights.clone();
    for t in &map.things {
        let which = match t.kind {
            1504 => FLOOR,
            1505 => CEILING,
            _ => continue,
        };
        for (i, v) in map.vertices.iter().enumerate() {
            if v[0] == t.x && v[1] == t.y {
                heights.entry(i).or_insert([None; 2])[which] = Some(t.z);
            }
        }
    }
    if heights.is_empty() {
        return;
    }
    for (sec, lines) in lines_of.iter().enumerate() {
        if lines.len() != 3 {
            continue;
        }
        let (l0, l1) = (&map.lines[lines[0]], &map.lines[lines[1]]);
        let (vi1, vi2) = (l0.v1, l0.v2);
        let vi3 = if l1.v1 == vi1 || l1.v1 == vi2 { l1.v2 } else { l1.v1 };
        let (Some(&p1), Some(&p2), Some(&p3)) = (map.vertices.get(vi1), map.vertices.get(vi2), map.vertices.get(vi3))
        else {
            continue;
        };
        for which in [FLOOR, CEILING] {
            let h = |v: usize| heights.get(&v).and_then(|h| h[which]);
            if h(vi1).is_none() && h(vi2).is_none() && h(vi3).is_none() {
                continue;
            }
            let base = if which == CEILING { map.sectors[sec].ceiling } else { map.sectors[sec].floor };
            let t1 = [p1[0], p1[1], h(vi1).unwrap_or(base)];
            let t2 = [p2[0], p2[1], h(vi2).unwrap_or(base)];
            let t3 = [p3[0], p3[1], h(vi3).unwrap_or(base)];
            let (v1, v2) =
                if side_of(p3, p1, p2) == 0 { (sub(t2, t3), sub(t1, t3)) } else { (sub(t1, t3), sub(t2, t3)) };
            if let Some(n) = orient(cross(v1, v2), which == CEILING) {
                planes[which][sec] = Plane::through(n, t3);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::map::{LineDef, MapData, MapFormat, Sector, SideDef, Thing};
    use super::*;
    use std::collections::HashMap;

    /// Two 128-unit squares side by side: sector 0 (west, floor 0,
    /// ceiling 128) and sector 1 (east, floor 64, ceiling 192), joined by
    /// line 0 from (128, 128) down to (128, 0), front side east.
    fn two_rooms() -> MapData {
        let v = vec![[0.0, 0.0], [128.0, 0.0], [128.0, 128.0], [0.0, 128.0], [256.0, 0.0], [256.0, 128.0]];
        let line = |v1: usize, v2: usize, front: usize, back: Option<usize>| LineDef {
            v1,
            v2,
            flags: if back.is_some() { 4 } else { 0 },
            front: Some(front),
            back,
            alpha: 1.0,
            ..Default::default()
        };
        let side = |sector| SideDef { sector, ..Default::default() };
        let sector = |floor, ceiling| Sector { floor, ceiling, tags: vec![0], ..Default::default() };
        MapData {
            format: MapFormat::Udmf("zdoom".into()),
            vertices: v,
            vertex_heights: HashMap::new(),
            lines: vec![
                line(2, 1, 1, Some(0)),
                line(0, 1, 2, None),
                line(3, 0, 3, None),
                line(2, 3, 4, None),
                line(1, 4, 5, None),
                line(4, 5, 6, None),
                line(5, 2, 7, None),
            ],
            sides: vec![side(1), side(0), side(0), side(0), side(0), side(1), side(1), side(1)],
            sectors: vec![sector(0.0, 128.0), sector(64.0, 192.0)],
            things: Vec::new(),
            bsp: None,
        }
    }

    fn at(x: f64, _y: f64) -> Option<usize> {
        Some(if x < 128.0 { 0 } else { 1 })
    }

    #[test]
    fn plane_equations() {
        let mut map = two_rooms();
        // A 45-degree floor rising to the east: z = x - 64.
        let n = 0.5f64.sqrt();
        map.sectors[1].planes[FLOOR] = Some([-n, 0.0, n, 64.0 * n]);
        let [floors, ceilings] = resolve(&map, &at);
        assert!((floors[1].z_at(200.0, 50.0) - 136.0).abs() < 1e-9);
        assert!(floors[1].is_slope() && !floors[0].is_slope());
        assert_eq!(ceilings[0].z_at(10.0, 10.0), 128.0);
        assert!(ceilings[1].normal()[2] < 0.0, "ceilings face down");
        assert_eq!(Plane::level(5.0, false).z_at(1e6, -1e6), 5.0);
    }

    #[test]
    fn plane_align() {
        // Line 0 runs south along x = 128 with the west room in front.
        // Align the back (east) floor to the front: from the west floor's
        // height 0 at the line up to its own 64 at its far side.
        let mut map = two_rooms();
        map.lines[0].special = PLANE_ALIGN;
        map.lines[0].args = [2, 0, 0, 0, 0];
        let [floors, ceilings] = resolve(&map, &at);
        assert!(floors[1].z_at(128.0, 30.0).abs() < 1e-9);
        assert!((floors[1].z_at(256.0, 100.0) - 64.0).abs() < 1e-9);
        assert!((floors[1].z_at(192.0, 0.0) - 32.0).abs() < 1e-9);
        assert!(!floors[0].is_slope() && !ceilings[1].is_slope());
        // The ceiling through the second argument: the east ceiling from
        // the west's 128 at the line up to its own 192.
        map.lines[0].args = [0, 2, 0, 0, 0];
        let [_, ceilings] = resolve(&map, &at);
        assert!((ceilings[1].z_at(128.0, 0.0) - 128.0).abs() < 1e-9);
        assert!((ceilings[1].z_at(256.0, 0.0) - 192.0).abs() < 1e-9);
        assert!(ceilings[1].normal()[2] < 0.0);
        // Bits 2-3 of the first argument stand in for an empty second.
        map.lines[0].args = [2 << 2, 0, 0, 0, 0];
        let [floors, ceilings] = resolve(&map, &at);
        assert!(ceilings[1].is_slope() && !floors[1].is_slope());
        // Doom-format Eternity specials translate: 340 aligns the front
        // (west) floor, from the east's 64 at the line down to 0.
        map.format = MapFormat::Doom;
        map.lines[0].special = 340;
        let [floors, _] = resolve(&map, &at);
        assert!((floors[0].z_at(128.0, 0.0) - 64.0).abs() < 1e-9);
        assert!(floors[0].z_at(0.0, 0.0).abs() < 1e-9);
    }

    #[test]
    fn vertex_heights_tilt_triangles() {
        // A right triangle (0,0) (128,0) (0,128), floor 0, with the
        // corner at (128, 0) raised to 32.
        let v = vec![[0.0, 0.0], [128.0, 0.0], [0.0, 128.0]];
        let line = |v1, v2| LineDef { v1, v2, front: Some(0), alpha: 1.0, ..Default::default() };
        let mut map = MapData {
            format: MapFormat::Udmf("zdoom".into()),
            vertices: v,
            vertex_heights: HashMap::from([(1, [Some(32.0), None])]),
            lines: vec![line(0, 2), line(2, 1), line(1, 0)],
            sides: vec![SideDef { sector: 0, ..Default::default() }],
            sectors: vec![Sector { floor: 0.0, ceiling: 128.0, ..Default::default() }],
            things: Vec::new(),
            bsp: None,
        };
        let [floors, ceilings] = resolve(&map, &|_, _| Some(0));
        assert!(floors[0].z_at(0.0, 0.0).abs() < 1e-9);
        assert!((floors[0].z_at(128.0, 0.0) - 32.0).abs() < 1e-9);
        assert!(floors[0].z_at(0.0, 128.0).abs() < 1e-9);
        assert!((floors[0].z_at(64.0, 0.0) - 16.0).abs() < 1e-9);
        assert!(!ceilings[0].is_slope());
        // The same through a 1505 thing on the ceiling.
        map.vertex_heights.clear();
        map.things.push(Thing { x: 0.0, y: 128.0, z: 96.0, kind: 1505, ..Default::default() });
        let [_, ceilings] = resolve(&map, &|_, _| Some(0));
        assert!((ceilings[0].z_at(0.0, 128.0) - 96.0).abs() < 1e-9);
        assert!((ceilings[0].z_at(0.0, 0.0) - 128.0).abs() < 1e-9);
    }

    #[test]
    fn slope_things_and_copies() {
        // 9500: tilt the west floor through line ID 7 (line 0) and a
        // point 32 units above the floor at x = 0.
        let mut map = two_rooms();
        map.lines[0].ids = vec![7];
        map.things.push(Thing { x: 0.0, y: 64.0, z: 32.0, kind: 9500, args: [7, 0, 0, 0, 0], ..Default::default() });
        let [floors, _] = resolve(&map, &at);
        assert!(floors[0].z_at(128.0, 10.0).abs() < 1e-9);
        assert!((floors[0].z_at(0.0, 10.0) - 32.0).abs() < 1e-9);
        // 9502: 45 degrees facing east (angle 0) through (64, 64, 0).
        map.things = vec![Thing { x: 64.0, y: 64.0, kind: 9502, args: [45, 0, 0, 0, 0], ..Default::default() }];
        let [floors, _] = resolve(&map, &at);
        assert!((floors[0].z_at(64.0, 0.0)).abs() < 1e-9);
        assert!((floors[0].z_at(0.0, 0.0) - 64.0).abs() < 1e-6, "{}", floors[0].z_at(0.0, 0.0));
        // 9510 copies the tagged sector's floor; Plane_Copy too.
        map.sectors[1].tags = vec![3];
        map.sectors[1].planes[FLOOR] = Some([0.0, 0.6, 0.8, -8.0]);
        map.things = vec![Thing { x: 32.0, y: 32.0, kind: 9510, args: [3, 0, 0, 0, 0], ..Default::default() }];
        let [floors, _] = resolve(&map, &at);
        assert_eq!(floors[0], floors[1]);
        map.things.clear();
        map.lines[1].special = PLANE_COPY;
        map.lines[1].args = [3, 0, 0, 0, 0];
        let [floors, _] = resolve(&map, &at);
        assert_eq!(floors[0], floors[1]);
    }

    #[test]
    fn degenerate_slopes_are_ignored() {
        let mut map = two_rooms();
        // A thing lying on its line, and a vertical plane equation.
        map.lines[0].ids = vec![1];
        map.things.push(Thing { x: 128.0, y: 64.0, kind: 9500, args: [1, 0, 0, 0, 0], ..Default::default() });
        map.sectors[0].planes[CEILING] = None;
        let [floors, _] = resolve(&map, &at);
        assert!(!floors[0].is_slope() && !floors[1].is_slope());
        assert!(orient([1.0, 0.0, 0.0], false).is_none());
        assert!(orient([0.0, 0.0, f64::NAN], false).is_none());
    }
}
