//! 3D floors: slabs of one sector's floor and ceiling (the control
//! sector, usually a dummy outside the map) placed inside other sectors,
//! as `Sector_Set3DFloor` (160) defines them, and the light zones they cut
//! a sector into.
//!
//! A 3D floor's top is its control sector's ceiling and its bottom the
//! control's floor, both wearing the control's flats; its sides wear the
//! middle texture of the line that defines it.  Its type says whether it
//! is solid, water you can swim in (drawn from inside too), or a
//! non-solid volume; its alpha makes it translucent.
//!
//! Unless told not to ("disable light effects"), a 3D floor also lights
//! what lies below it with its control sector's light, down to the next
//! one.  GZDoom keeps these as a "light list" per sector, which walls are
//! split by; this follows `P_Recalculate3DFloors`.

use super::map::{CEILING, FLOOR, MapData, MapFormat, SECTOR_SET_3D_FLOOR};
use super::planes::Plane;

// GZDoom's 3D floor flags (`ffloortype_e`).
pub const FF_EXISTS: u32 = 0x1;
pub const FF_SOLID: u32 = 0x2;
pub const FF_RENDERSIDES: u32 = 0x4;
pub const FF_RENDERPLANES: u32 = 0x8;
pub const FF_RENDERALL: u32 = 0xc;
pub const FF_SWIMMABLE: u32 = 0x10;
pub const FF_NOSHADE: u32 = 0x20;
pub const FF_BOTHPLANES: u32 = 0x200;
pub const FF_TRANSLUCENT: u32 = 0x800;
pub const FF_FOG: u32 = 0x1000;
pub const FF_INVERTPLANES: u32 = 0x2000;
pub const FF_ALLSIDES: u32 = 0x4000;
pub const FF_INVERTSIDES: u32 = 0x8000;
pub const FF_DOUBLESHADOW: u32 = 0x10000;
pub const FF_UPPERTEXTURE: u32 = 0x20000;
pub const FF_LOWERTEXTURE: u32 = 0x40000;
pub const FF_THINFLOOR: u32 = 0x80000;
pub const FF_FIX: u32 = 0x200000;
pub const FF_INVERTSECTOR: u32 = 0x400000;
pub const FF_SEETHROUGH: u32 = 0x2000000;
pub const FF_SHOOTTHROUGH: u32 = 0x4000000;
pub const FF_ADDITIVETRANS: u32 = 0x10000000;
pub const FF_FLOOD: u32 = 0x20000000;
pub const FF_RESET: u32 = 0x80000000;

/// One of a 3D floor's two planes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Side {
    pub plane: Plane,
    /// The sector whose flat (and its panning and scale) the plane wears,
    /// and which of its planes: `FLOOR` or `CEILING`.
    pub model: usize,
    pub which: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct XFloor {
    pub control: usize,
    /// The line that defines it, whose middle texture its sides wear.
    pub line: usize,
    pub flags: u32,
    /// 0-255.
    pub alpha: u32,
    pub top: Side,
    pub bottom: Side,
}

impl XFloor {
    pub fn translucent(&self) -> bool {
        self.flags & (FF_TRANSLUCENT | FF_ADDITIVETRANS) != 0
    }
}

/// A light zone: from `plane` down to the next zone's plane, surfaces take
/// `light` (a light level) and the light colour of sector `color_from`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Zone {
    pub plane: Plane,
    pub light: f64,
    pub color_from: usize,
}

/// The 3D floors of every sector, top first, and every sector's light
/// zones (only sectors with 3D floors have more than one).
pub(crate) struct XFloors {
    pub floors: Vec<Vec<XFloor>>,
    pub zones: Vec<Vec<Zone>>,
}

/// A sector's middle, where GZDoom compares plane heights.
pub(crate) fn center(map: &MapData, lines_of: &[usize]) -> [f64; 2] {
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for &li in lines_of {
        let Some((a, z)) = map.ends(&map.lines[li]) else { continue };
        for p in [a, z] {
            lo = [lo[0].min(p[0]), lo[1].min(p[1])];
            hi = [hi[0].max(p[0]), hi[1].max(p[1])];
        }
    }
    if lo[0] > hi[0] { [0.0, 0.0] } else { [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0] }
}

/// Finds every `Sector_Set3DFloor` and builds the 3D floors and light
/// zones.  `planes` are the sectors' floors and ceilings after slopes.
pub(crate) fn build(map: &MapData, planes: &[Vec<Plane>; 2], lines_of: &[Vec<usize>]) -> XFloors {
    let n = map.sectors.len();
    let mut floors: Vec<Vec<XFloor>> = vec![Vec::new(); n];
    let hexen = map.format == MapFormat::Hexen;
    for (li, l) in map.lines.iter().enumerate() {
        let (special, mut args) = map.hexen_special(l);
        if special != SECTOR_SET_3D_FLOOR {
            continue;
        }
        let Some(control) = map.sector_of(l.front) else { continue };
        // In Hexen's format the fifth argument is a line ID when bit 8 of
        // the type says so, else the tag's high byte.
        if hexen && args[1] & 8 == 0 {
            args[0] += 256 * args[4];
        }
        let (param, param2, alpha) = (args[1] & !8, args[2], args[3]);
        let (flags, alpha) = flags_of(param, param2, alpha);
        for target in map.tagged(args[0]) {
            if target == control || floors[target].len() >= 256 {
                continue;
            }
            floors[target].push(make(control, li, target, flags, alpha, planes));
        }
    }
    let mut zones = Vec::with_capacity(n);
    for s in 0..n {
        let c = center(map, &lines_of[s]);
        let top = |x: &XFloor| x.top.plane.z_at(c[0], c[1]);
        floors[s].sort_by(|a, b| top(b).total_cmp(&top(a)));
        zones.push(light_list(map, s, &floors[s], planes, c));
    }
    XFloors { floors, zones }
}

/// Turns `Sector_Set3DFloor`'s type, flags and alpha into GZDoom's flags,
/// as `MapLoader::Set3DFloor` does.
pub(crate) fn flags_of(param: i32, param2: i32, alpha: i32) -> (u32, u32) {
    if param == 0 {
        // Vavoom's: solid, its control sector's planes upside down.
        return (FF_EXISTS | FF_RENDERALL | FF_SOLID | FF_INVERTSECTOR, 255);
    }
    if param == 4 {
        return (FF_EXISTS | FF_RENDERPLANES | FF_INVERTPLANES | FF_NOSHADE | FF_FIX, 255);
    }
    const DEFAULTS: [u32; 4] = [
        0,
        FF_SOLID,
        FF_SWIMMABLE | FF_BOTHPLANES | FF_ALLSIDES | FF_SHOOTTHROUGH | FF_SEETHROUGH,
        FF_SHOOTTHROUGH | FF_SEETHROUGH,
    ];
    let mut flags = DEFAULTS[(param & 3) as usize] | FF_EXISTS | FF_RENDERALL;
    if param & 4 != 0 {
        flags |= FF_ALLSIDES | FF_BOTHPLANES;
    }
    if param & 16 != 0 {
        flags ^= FF_SEETHROUGH;
    }
    if param & 32 != 0 {
        flags ^= FF_SHOOTTHROUGH;
    }
    for (bit, f) in [
        (1, FF_NOSHADE),
        (2, FF_DOUBLESHADOW),
        (4, FF_FOG),
        (8, FF_THINFLOOR),
        (16, FF_UPPERTEXTURE),
        (32, FF_LOWERTEXTURE),
        (64, FF_ADDITIVETRANS | FF_TRANSLUCENT),
        (1024, FF_RESET),
    ] {
        if param2 & bit != 0 {
            flags |= f;
        }
    }
    if param2 & 128 != 0 && flags & FF_SOLID == 0 {
        flags |= FF_FLOOD | FF_SEETHROUGH | FF_SHOOTTHROUGH;
    }
    let alpha = alpha.clamp(0, 255) as u32;
    if alpha == 0 {
        flags &= !(FF_RENDERALL | FF_BOTHPLANES | FF_ALLSIDES);
    } else if alpha != 255 {
        flags |= FF_TRANSLUCENT;
    }
    (flags, alpha)
}

/// `P_Add3DFloor`: the 3D floor's planes from its control sector.
fn make(control: usize, line: usize, target: usize, flags: u32, alpha: u32, planes: &[Vec<Plane>; 2]) -> XFloor {
    let side = |model: usize, which: usize| Side { plane: planes[which][model], model, which };
    let mut bottom = if flags & FF_THINFLOOR == 0 { side(control, FLOOR) } else { side(control, CEILING) };
    let mut top = if flags & FF_FIX == 0 {
        side(control, CEILING)
    } else {
        Side { plane: planes[FLOOR][target], model: control, which: FLOOR }
    };
    if flags & FF_INVERTSECTOR != 0 {
        std::mem::swap(&mut top, &mut bottom);
        if flags & FF_SWIMMABLE != 0 {
            bottom = Side { plane: planes[FLOOR][target], model: target, which: FLOOR };
        }
    }
    // The ceiling plane faces down and the floor plane up; a slab's top
    // faces up and its bottom down whichever plane they came from.
    top.plane = facing(top.plane, false);
    bottom.plane = facing(bottom.plane, true);
    let (mut flags, mut alpha) = (flags, alpha);
    // GZDoom can't draw sloped translucent 3D floors and makes them
    // opaque.
    if top.plane.is_slope() || bottom.plane.is_slope() {
        alpha = 255;
        flags &= !FF_ADDITIVETRANS;
        if flags & FF_TRANSLUCENT != 0 && alpha == 255 {
            flags &= !FF_TRANSLUCENT;
        }
    }
    XFloor { control, line, flags, alpha, top, bottom }
}

/// The same plane facing down (`down`) or up.
pub(crate) fn facing(p: Plane, down: bool) -> Plane {
    if (p.c < 0.0) == down { p } else { Plane { a: -p.a, b: -p.b, c: -p.c, d: -p.d } }
}

/// A sector's light zones, top down: its own light from the ceiling, then
/// each 3D floor's control light from its top down, as
/// `P_Recalculate3DFloors` makes GZDoom's light list.
fn light_list(map: &MapData, s: usize, floors: &[XFloor], planes: &[Vec<Plane>; 2], c: [f64; 2]) -> Vec<Zone> {
    let own = Zone { plane: planes[CEILING][s], light: map.sectors[s].light, color_from: s };
    let mut list = vec![own];
    if floors.is_empty() {
        return list;
    }
    let mut reset = own;
    let max = planes[CEILING][s].z_at(c[0], c[1]);
    let min = planes[FLOOR][s].z_at(c[0], c[1]);
    for x in floors {
        if x.flags & FF_EXISTS == 0 || x.flags & FF_NOSHADE != 0 {
            continue;
        }
        let light = Zone { plane: x.top.plane, light: map.sectors[x.control].light, color_from: x.control };
        let top = x.top.plane.z_at(c[0], c[1]);
        if top < min {
            break;
        }
        if top < max {
            list.push(light);
        } else if x.bottom.plane.z_at(c[0], c[1]) < max {
            // It starts above the ceiling and reaches below it.
            list[0] = Zone { plane: list[0].plane, ..light };
        }
        if x.flags & (FF_DOUBLESHADOW | FF_RESET) == 0 {
            reset = *list.last().unwrap_or(&own);
        } else if x.flags & FF_RESET != 0 {
            reset = own;
        }
        if x.flags & FF_DOUBLESHADOW != 0 {
            let bottom = x.bottom.plane.z_at(c[0], c[1]);
            if bottom < max && bottom > min {
                list.push(Zone { plane: x.bottom.plane, ..reset });
            }
        }
    }
    list
}

/// The zone lighting a plane at height `z` (at the sector's middle), seen
/// from above or, with `underside`, from below: `P_GetPlaneLight`.
pub(crate) fn plane_zone(zones: &[Zone], c: [f64; 2], z: f64, underside: bool) -> usize {
    let h = if underside { z - 1.0 / 65536.0 } else { z };
    (1..zones.len()).find(|&i| zones[i].plane.z_at(c[0], c[1]) <= h).map_or(zones.len() - 1, |i| i - 1)
}

#[cfg(test)]
mod tests {
    use super::super::map::{LineDef, MapData, MapFormat, Sector, SideDef};
    use super::*;
    use std::collections::HashMap;

    /// A 256-unit room (sector 0, floor 0, ceiling 256, light 200, tag 5)
    /// and, far away, a control sector (1: floor 64, ceiling 96, light
    /// 100) whose first line carries Sector_Set3DFloor.
    fn map(args: [i32; 5]) -> MapData {
        let v = vec![
            [0.0, 0.0],
            [256.0, 0.0],
            [256.0, 256.0],
            [0.0, 256.0],
            [1000.0, 0.0],
            [1064.0, 0.0],
            [1064.0, 64.0],
            [1000.0, 64.0],
        ];
        let line = |v1, v2, side| LineDef { v1, v2, front: Some(side), alpha: 1.0, ..Default::default() };
        let mut lines = vec![line(0, 3, 0), line(3, 2, 0), line(2, 1, 0), line(1, 0, 0)];
        lines.extend([line(4, 7, 1), line(7, 6, 1), line(6, 5, 1), line(5, 4, 1)]);
        lines[4].special = SECTOR_SET_3D_FLOOR;
        lines[4].args = args;
        let sides = vec![
            SideDef { sector: 0, ..Default::default() },
            SideDef { sector: 1, middle: "SIDE".into(), ..Default::default() },
        ];
        let sectors = vec![
            Sector { floor: 0.0, ceiling: 256.0, light: 200.0, tags: vec![5], ..Default::default() },
            Sector { floor: 64.0, ceiling: 96.0, light: 100.0, tags: vec![0], ..Default::default() },
        ];
        MapData {
            format: MapFormat::Udmf("zdoom".into()),
            vertices: v,
            vertex_heights: HashMap::new(),
            lines,
            sides,
            sectors,
            things: Vec::new(),
            bsp: None,
        }
    }

    fn built(m: &MapData) -> XFloors {
        let lines_of = super::super::planes::sector_lines(m);
        let planes = super::super::planes::resolve(m, &|_, _| Some(0));
        build(m, &planes, &lines_of)
    }

    #[test]
    fn solid_slab_and_its_light() {
        let m = map([5, 1, 0, 255, 0]);
        let x = built(&m);
        assert!(x.floors[1].is_empty(), "the control sector itself has none");
        let f = &x.floors[0][0];
        assert_eq!((f.control, f.alpha), (1, 255));
        assert_eq!(f.flags & (FF_SOLID | FF_RENDERALL | FF_EXISTS), FF_SOLID | FF_RENDERALL | FF_EXISTS);
        assert_eq!(f.top.plane.z_at(10.0, 10.0), 96.0);
        assert_eq!(f.bottom.plane.z_at(10.0, 10.0), 64.0);
        assert!(f.top.plane.c > 0.0 && f.bottom.plane.c < 0.0, "top faces up, bottom down");
        assert_eq!((f.top.model, f.top.which, f.bottom.which), (1, CEILING, FLOOR));
        // Below the slab's top, the control sector's light.
        let z = &x.zones[0];
        assert_eq!(z.len(), 2);
        assert_eq!((z[0].light, z[1].light), (200.0, 100.0));
        let c = [128.0, 128.0];
        assert_eq!(plane_zone(z, c, 0.0, false), 1, "the floor lies in the slab's light");
        assert_eq!(plane_zone(z, c, 256.0, true), 0, "the ceiling in the room's");
        assert_eq!(plane_zone(z, c, 96.0, false), 0, "the slab's top in the room's");
        assert_eq!(plane_zone(z, c, 64.0, true), 1, "its bottom in its own");
    }

    #[test]
    fn types_and_flags() {
        // Swimmable, translucent (alpha 128), lights disabled.
        let m = map([5, 2, 1, 128, 0]);
        let x = built(&m);
        let f = &x.floors[0][0];
        assert!(f.flags & FF_SWIMMABLE != 0 && f.flags & FF_BOTHPLANES != 0 && f.translucent());
        assert_eq!(f.alpha, 128);
        assert_eq!(x.zones[0].len(), 1, "no light effect");
        // Alpha 0 draws nothing.
        let (flags, _) = flags_of(3, 0, 0);
        assert_eq!(flags & FF_RENDERALL, 0);
        // Vavoom's swaps the planes: the control's floor is the top.
        let (flags, alpha) = flags_of(0, 0, 0);
        assert!(flags & FF_INVERTSECTOR != 0 && alpha == 255);
        let m = map([5, 0, 0, 0, 0]);
        let f = &built(&m).floors[0][0];
        assert_eq!((f.top.plane.z_at(0.0, 0.0), f.bottom.plane.z_at(0.0, 0.0)), (64.0, 96.0));
        // Tag 0 and the control sector itself get nothing.
        let m = map([0, 1, 0, 255, 0]);
        assert!(built(&m).floors.iter().all(Vec::is_empty));
        // Upper and lower texture flags, additive.
        let (flags, _) = flags_of(1, 16 | 64, 255);
        assert!(flags & FF_UPPERTEXTURE != 0 && flags & FF_ADDITIVETRANS != 0);
    }
}
