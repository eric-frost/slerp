//! A Doom map read from any of its three formats into one model: Doom's own
//! binary lumps, Hexen's (wider linedefs and things, plus scripts), and
//! UDMF's text (a TEXTMAP lump).  The automap and the 3D level both start
//! from this.
//!
//! UDMF carries much more than the binary formats: sloped planes, per-plane
//! texture panning, scaling and rotation, coloured light and fog, glowing
//! planes, per-wall light and texture scale, translucent lines.  All of it
//! is kept here.  Line specials are kept in Hexen's numbering, which UDMF
//! and Hexen maps use; a Doom-format map's few specials that change how it
//! looks (Eternity's slopes, Legacy's 3D floors, Boom's translucency) are
//! translated as ZDoom's tables do.
//!
//! Indices between records (a linedef's vertices and sidedefs, a sidedef's
//! sector) are kept as the file stores them and checked where they are
//! used, so one bad reference costs one line, not the map.

use super::Lump;
use std::collections::HashMap;

/// Lumps that belong to a classic map after its marker.
const MAP_LUMPS: &[&str] = &[
    "THINGS", "LINEDEFS", "SIDEDEFS", "VERTEXES", "SEGS", "SSECTORS", "NODES", "SECTORS", "REJECT", "BLOCKMAP",
    "BEHAVIOR", "SCRIPTS",
];

/// Linedef flags shared by all three formats.
pub const ML_TWOSIDED: u32 = 0x4;
pub const ML_DONTPEGTOP: u32 = 0x8;
pub const ML_DONTPEGBOTTOM: u32 = 0x10;
pub const ML_SECRET: u32 = 0x20;
pub const ML_DONTDRAW: u32 = 0x80;
/// UDMF's `wrapmidtex` and `clipmidtex`, in bits Doom doesn't use.
pub const ML_WRAPMIDTEX: u32 = 0x10000;
pub const ML_CLIPMIDTEX: u32 = 0x20000;

/// Thing flags: the skill levels a thing appears on, and Doom's
/// multiplayer-only bit.
pub const MTF_EASY: u32 = 0x1;
pub const MTF_NORMAL: u32 = 0x2;
pub const MTF_HARD: u32 = 0x4;
pub const MTF_MULTIPLAYER: u32 = 0x10;

/// Line specials, in Hexen's numbering, that shape a level's start.
pub const PLANE_COPY: u32 = 118;
pub const LINE_SET_IDENTIFICATION: u32 = 121;
pub const SECTOR_SET_3D_FLOOR: u32 = 160;
pub const PLANE_ALIGN: u32 = 181;
pub const TRANSLUCENT_LINE: u32 = 208;

#[derive(Clone, Debug, PartialEq)]
pub enum MapFormat {
    Doom,
    Hexen,
    /// UDMF, with its namespace ("doom", "zdoom", "hexen", ...).
    Udmf(String),
}

impl MapFormat {
    pub fn name(&self) -> String {
        match self {
            MapFormat::Doom => "Doom format".into(),
            MapFormat::Hexen => "Hexen format".into(),
            MapFormat::Udmf(ns) if ns.is_empty() => "UDMF".into(),
            MapFormat::Udmf(ns) => format!("UDMF, namespace {ns}"),
        }
    }

    /// Whether line specials use Hexen's numbering (UDMF's namespaces do,
    /// except those named after the games that didn't).
    pub fn hexen_specials(&self) -> bool {
        match self {
            MapFormat::Doom => false,
            MapFormat::Hexen => true,
            MapFormat::Udmf(ns) => !matches!(ns.as_str(), "doom" | "heretic" | "strife"),
        }
    }

    /// Whether the map was made for ZDoom's family of ports, whose
    /// lighting (and fog) model the level then follows.
    pub fn zdoom(&self) -> bool {
        match self {
            MapFormat::Doom => false,
            MapFormat::Hexen => true,
            MapFormat::Udmf(ns) => !matches!(ns.as_str(), "doom" | "heretic" | "hexen" | "strife"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct LineDef {
    pub v1: usize,
    pub v2: usize,
    pub flags: u32,
    /// The special in the format's own numbering: Doom's for Doom-format
    /// maps, Hexen's for the others.
    pub special: u32,
    pub args: [i32; 5],
    /// Line IDs (Doom's tag, UDMF's `id` and `moreids`, Hexen's
    /// Line_SetIdentification).
    pub ids: Vec<i32>,
    pub front: Option<usize>,
    pub back: Option<usize>,
    /// Translucency of the middle texture, 0-1.
    pub alpha: f32,
    pub additive: bool,
}

#[derive(Clone, Debug)]
pub struct SideDef {
    /// Texture offsets in map units: x along the line, y down the wall.
    pub x_offset: f64,
    pub y_offset: f64,
    /// UDMF's extra offsets for each part, added to the shared ones.
    pub part_offsets: [(f64, f64); 3],
    /// UDMF's texture scale for each part: texels per unit, on top of the
    /// texture's own.
    pub part_scales: [(f64, f64); 3],
    /// Texture names, upper case; "-" or empty means none.
    pub upper: String,
    pub lower: String,
    pub middle: String,
    pub sector: usize,
    /// Light added to (or, when absolute, replacing) the sector's.
    pub light: i32,
    pub light_absolute: bool,
    /// The same for each part.
    pub part_light: [i32; 3],
    pub part_light_absolute: [bool; 3],
    pub no_fake_contrast: bool,
    pub smooth_lighting: bool,
    /// Relative light applies even in fog.
    pub light_fog: bool,
}

impl Default for SideDef {
    fn default() -> Self {
        SideDef {
            x_offset: 0.0,
            y_offset: 0.0,
            part_offsets: [(0.0, 0.0); 3],
            part_scales: [(1.0, 1.0); 3],
            upper: "-".into(),
            lower: "-".into(),
            middle: "-".into(),
            sector: usize::MAX,
            light: 0,
            light_absolute: false,
            part_light: [0; 3],
            part_light_absolute: [false; 3],
            no_fake_contrast: false,
            smooth_lighting: false,
            light_fog: false,
        }
    }
}

/// Indices into `SideDef::part_offsets` and friends.
pub const UPPER: usize = 0;
pub const MIDDLE: usize = 1;
pub const LOWER: usize = 2;

/// Indices of a sector's two planes.
pub const FLOOR: usize = 0;
pub const CEILING: usize = 1;

/// How a flat is laid on its plane: UDMF's panning (in texels of the
/// texture), scale (texels per unit) and rotation (degrees).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlatTransform {
    pub x_pan: f64,
    pub y_pan: f64,
    pub x_scale: f64,
    pub y_scale: f64,
    pub rotation: f64,
}

impl Default for FlatTransform {
    fn default() -> Self {
        FlatTransform { x_pan: 0.0, y_pan: 0.0, x_scale: 1.0, y_scale: 1.0, rotation: 0.0 }
    }
}

#[derive(Clone, Debug)]
pub struct Sector {
    pub floor: f64,
    pub ceiling: f64,
    pub floor_flat: String,
    pub ceiling_flat: String,
    /// 0 (black) to 255 (full bright).
    pub light: f64,
    pub special: u32,
    pub tags: Vec<i32>,
    /// UDMF plane equations (a, b, c, d with ax + by + cz + d = 0), floor
    /// then ceiling, when all four values are given.
    pub planes: [Option<[f64; 4]>; 2],
    pub flats: [FlatTransform; 2],
    /// Each plane's light, relative to the sector's or absolute.
    pub plane_light: [(i32, bool); 2],
    /// Light and fog colours as 0xRRGGBB.
    pub light_color: Option<u32>,
    pub fade_color: Option<u32>,
    /// UDMF's `fogdensity` (0-510).
    pub fog_density: Option<f64>,
    pub desaturation: f64,
    /// UDMF's colours of the floor, ceiling, and the tops and bottoms of
    /// walls, multiplied with the light.
    pub colors: [Option<u32>; 4],
    /// Glow colour (0xRRGGBB) and height of the floor and ceiling.
    pub glow: [(Option<u32>, f64); 2],
    pub plane_alpha: [f64; 2],
}

impl Default for Sector {
    fn default() -> Self {
        Sector {
            floor: 0.0,
            ceiling: 0.0,
            floor_flat: "-".into(),
            ceiling_flat: "-".into(),
            light: 160.0,
            special: 0,
            tags: Vec::new(),
            planes: [None; 2],
            flats: [FlatTransform::default(); 2],
            plane_light: [(0, false); 2],
            light_color: None,
            fade_color: None,
            fog_density: None,
            desaturation: 0.0,
            colors: [None; 4],
            glow: [(None, 64.0); 2],
            plane_alpha: [1.0; 2],
        }
    }
}

/// Indices into `Sector::colors`.
pub const COLOR_WALLTOP: usize = 2;
pub const COLOR_WALLBOTTOM: usize = 3;

impl Sector {
    pub fn has_tag(&self, tag: i32) -> bool {
        self.tags.contains(&tag)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Thing {
    pub x: f64,
    pub y: f64,
    /// Height above the floor (Hexen and UDMF only).
    pub z: f64,
    /// Degrees counter-clockwise from east.
    pub angle: f64,
    pub kind: u32,
    pub flags: u32,
    /// Thing ID (Hexen and UDMF).
    pub id: i32,
    pub special: u32,
    pub args: [i32; 5],
}

/// The BSP that Doom's node builders store: partition lines, convex
/// subsectors and the wall pieces (segs) bounding them.  Used to find the
/// sector under a point and to draw floors the way the game does where a
/// sector's lines don't form closed loops.
#[derive(Default)]
pub struct Bsp {
    pub nodes: Vec<Node>,
    /// (first seg, seg count) per subsector.
    pub subsectors: Vec<(usize, usize)>,
    pub segs: Vec<Seg>,
    /// Vertices beyond the map's own that the node builder added (ZDoom's
    /// extended nodes); seg vertex indices past the map's count index
    /// these.
    pub extra_vertices: Vec<[f64; 2]>,
}

#[derive(Clone, Copy, Debug)]
pub struct Node {
    /// The partition line: a point and a direction.
    pub x: f64,
    pub y: f64,
    pub dx: f64,
    pub dy: f64,
    /// Front (right) and back (left) children; `SUBSECTOR` marks a leaf.
    pub children: [u32; 2],
}

pub const SUBSECTOR: u32 = 0x8000_0000;

#[derive(Clone, Copy, Debug)]
pub struct Seg {
    pub v1: usize,
    pub v2: usize,
    /// The linedef, or `usize::MAX` for a GL node builder's "miniseg".
    pub line: usize,
    /// 0 when the seg runs along the linedef's front side, 1 for the back.
    pub side: u8,
}

pub struct MapData {
    pub format: MapFormat,
    pub vertices: Vec<[f64; 2]>,
    /// UDMF's per-vertex floor and ceiling heights, for sloping
    /// triangular sectors.
    pub vertex_heights: HashMap<usize, [Option<f64>; 2]>,
    pub lines: Vec<LineDef>,
    pub sides: Vec<SideDef>,
    pub sectors: Vec<Sector>,
    pub things: Vec<Thing>,
    pub bsp: Option<Bsp>,
}

impl MapData {
    /// The sector on one side of a line, when the references are valid.
    pub fn sector_of(&self, side: Option<usize>) -> Option<usize> {
        let s = self.sides.get(side?)?.sector;
        (s < self.sectors.len()).then_some(s)
    }

    /// A line's two end points, when both exist.
    pub fn ends(&self, line: &LineDef) -> Option<([f64; 2], [f64; 2])> {
        Some((*self.vertices.get(line.v1)?, *self.vertices.get(line.v2)?))
    }

    /// Whether Doom treats the line as two-sided: it has the flag and a
    /// back side.  A back sidedef without the flag is drawn as a solid
    /// wall from each side, as vanilla Doom does.
    pub fn two_sided(&self, line: &LineDef) -> bool {
        line.flags & ML_TWOSIDED != 0 && self.sector_of(line.back).is_some()
    }

    /// A vertex of the map or of the node builder.
    pub fn vertex(&self, i: usize) -> Option<[f64; 2]> {
        self.vertices.get(i).copied().or_else(|| {
            let extra = &self.bsp.as_ref()?.extra_vertices;
            extra.get(i.checked_sub(self.vertices.len())?).copied()
        })
    }

    /// A line's special and arguments in Hexen's numbering, translating
    /// the Doom-format specials that shape a level as ZDoom's `xlat`
    /// tables do: Eternity's slopes, Legacy's 3D floors and Boom's
    /// translucent lines.  Others come back as 0.
    pub fn hexen_special(&self, l: &LineDef) -> (u32, [i32; 5]) {
        if self.format.hexen_specials() {
            return (l.special, l.args);
        }
        let tag = l.ids.first().copied().unwrap_or(0);
        let align = |f: i32, c: i32| (PLANE_ALIGN, [f, c, 0, 0, 0]);
        let floor3d = |t: i32, f: i32, a: i32| (SECTOR_SET_3D_FLOOR, [tag, t, f, a, 0]);
        match l.special {
            340 => align(1, 0),
            341 => align(0, 1),
            342 => align(1, 1),
            343 => align(2, 0),
            344 => align(0, 2),
            345 => align(2, 2),
            346 => align(2, 1),
            347 => align(1, 2),
            281 => floor3d(1, 0, 255),
            289 => floor3d(1, 1, 255),
            300 => floor3d(1, 1, 127),
            301 => floor3d(2, 2, 127),
            302 => floor3d(3, 6, 127),
            303 => floor3d(3, 0, 255),
            304 => floor3d(2, 2, 255),
            305 => floor3d(3, 2, 255),
            306 => floor3d(1, 0, 255),
            260 => (TRANSLUCENT_LINE, [tag, 168, 0, 0, 0]),
            _ => (0, [0; 5]),
        }
    }

    /// The sectors with tag `tag`; none for tag 0, which means "no tag".
    pub fn tagged(&self, tag: i32) -> impl Iterator<Item = usize> + '_ {
        self.sectors.iter().enumerate().filter(move |(_, s)| tag != 0 && s.has_tag(tag)).map(|(i, _)| i)
    }
}

/*┌─────────────────╮
| Reading         |
└─────────────────╯*/
//region Reading

/// Reads the map whose marker lump is followed by `after`: classic lumps
/// starting with THINGS, or a TEXTMAP.
pub fn read(after: &[Lump]) -> Option<MapData> {
    match after.first()?.name.as_str() {
        "THINGS" => classic(after),
        "TEXTMAP" => {
            let mut map = udmf(after[0].data)?;
            // ZDoom's compressed GL nodes follow as ZNODES.
            if let Some(z) = after.iter().take_while(|l| l.name != "ENDMAP").find(|l| l.name == "ZNODES") {
                map.bsp = super::nodes::read_extended(z.data, map.vertices.len());
            }
            Some(map)
        }
        _ => None,
    }
}

fn le16(d: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([d[at], d[at + 1]])
}

fn sle16(d: &[u8], at: usize) -> f64 {
    i16::from_le_bytes([d[at], d[at + 1]]) as f64
}

/// A sidedef or sector index, with 0xffff meaning none.
fn index16(v: u16) -> Option<usize> {
    (v != 0xffff).then_some(v as usize)
}

/// An 8-byte texture or flat name, upper case.
fn name8(d: &[u8]) -> String {
    super::cstr(d).trim().to_ascii_uppercase()
}

/// Reads a Doom- or Hexen-format map from the lumps after its marker.
fn classic(after: &[Lump]) -> Option<MapData> {
    let mut parts: HashMap<&str, &[u8]> = HashMap::new();
    for l in after.iter().take_while(|l| MAP_LUMPS.contains(&l.name.as_str())) {
        parts.entry(l.name.as_str()).or_insert(l.data);
    }
    let hexen = parts.contains_key("BEHAVIOR");
    let lump = |n: &str| parts.get(n).copied().unwrap_or(&[]);
    let vertices: Vec<[f64; 2]> = lump("VERTEXES").chunks_exact(4).map(|v| [sle16(v, 0), sle16(v, 2)]).collect();
    let sides = lump("SIDEDEFS")
        .chunks_exact(30)
        .map(|s| SideDef {
            x_offset: sle16(s, 0),
            y_offset: sle16(s, 2),
            upper: name8(&s[4..12]),
            lower: name8(&s[12..20]),
            middle: name8(&s[20..28]),
            sector: le16(s, 28) as usize,
            ..Default::default()
        })
        .collect();
    let sectors = lump("SECTORS")
        .chunks_exact(26)
        .map(|s| Sector {
            floor: sle16(s, 0),
            ceiling: sle16(s, 2),
            floor_flat: name8(&s[4..12]),
            ceiling_flat: name8(&s[12..20]),
            light: sle16(s, 20).clamp(0.0, 255.0),
            special: le16(s, 22) as u32,
            tags: vec![le16(s, 24) as i16 as i32],
            ..Default::default()
        })
        .collect();
    let mut lines: Vec<LineDef> = if hexen {
        lump("LINEDEFS")
            .chunks_exact(16)
            .map(|l| {
                let args = [l[7], l[8], l[9], l[10], l[11]].map(|a| a as i32);
                LineDef {
                    v1: le16(l, 0) as usize,
                    v2: le16(l, 2) as usize,
                    flags: le16(l, 4) as u32,
                    special: l[6] as u32,
                    args,
                    ids: hexen_line_ids(l[6] as u32, &args),
                    front: index16(le16(l, 12)),
                    back: index16(le16(l, 14)),
                    alpha: 1.0,
                    additive: false,
                }
            })
            .collect()
    } else {
        lump("LINEDEFS")
            .chunks_exact(14)
            .map(|l| LineDef {
                v1: le16(l, 0) as usize,
                v2: le16(l, 2) as usize,
                flags: le16(l, 4) as u32 & 0x1ff,
                special: le16(l, 6) as u32,
                ids: vec![le16(l, 8) as i16 as i32],
                front: index16(le16(l, 10)),
                back: index16(le16(l, 12)),
                alpha: 1.0,
                ..Default::default()
            })
            .collect()
    };
    let things: Vec<Thing> = if hexen {
        lump("THINGS")
            .chunks_exact(20)
            .map(|t| Thing {
                id: le16(t, 0) as i32,
                x: sle16(t, 2),
                y: sle16(t, 4),
                z: sle16(t, 6),
                angle: sle16(t, 8),
                kind: le16(t, 10) as u32,
                flags: le16(t, 12) as u32,
                special: t[14] as u32,
                args: [t[15], t[16], t[17], t[18], t[19]].map(|a| a as i32),
            })
            .collect()
    } else {
        lump("THINGS")
            .chunks_exact(10)
            .map(|t| Thing {
                x: sle16(t, 0),
                y: sle16(t, 2),
                angle: sle16(t, 4),
                kind: le16(t, 6) as u32,
                flags: le16(t, 8) as u32,
                ..Default::default()
            })
            .collect()
    };
    if lines.is_empty() && vertices.is_empty() {
        return None;
    }
    let format = if hexen { MapFormat::Hexen } else { MapFormat::Doom };
    if !hexen {
        // Boom's translucent lines (260): this line, or every line sharing
        // its tag.
        let tagged: Vec<i32> = lines.iter().filter(|l| l.special == 260).map(|l| l.ids[0]).collect();
        for l in &mut lines {
            if (l.special == 260 && l.ids[0] == 0) || (l.ids[0] != 0 && tagged.contains(&l.ids[0])) {
                l.alpha = 168.0 / 255.0;
            }
        }
    }
    let bsp = classic_bsp(lump("NODES"), lump("SSECTORS"), lump("SEGS"), vertices.len());
    let mut map = MapData { format, vertices, vertex_heights: HashMap::new(), lines, sides, sectors, things, bsp };
    if hexen {
        translucent_lines(&mut map);
    }
    Some(map)
}

/// The IDs a Hexen-format line gives itself through its special: Line_SetIdentification's
/// first argument (plus the fifth times 256), and the line ID some other
/// specials carry.
fn hexen_line_ids(special: u32, args: &[i32; 5]) -> Vec<i32> {
    match special {
        LINE_SET_IDENTIFICATION => vec![args[0] + args[4] * 256],
        TRANSLUCENT_LINE if args[0] != 0 => vec![args[0]],
        SECTOR_SET_3D_FLOOR if args[1] & 8 != 0 => vec![args[4]],
        _ => Vec::new(),
    }
}

/// Applies TranslucentLine (208): to its own line when its ID argument is
/// 0, else to every line with that ID.
fn translucent_lines(map: &mut MapData) {
    let mut set: Vec<(Option<i32>, usize, f32, bool)> = Vec::new();
    for (i, l) in map.lines.iter().enumerate() {
        let (special, args) = map.hexen_special(l);
        if special == TRANSLUCENT_LINE {
            let id = (args[0] != 0).then_some(args[0]);
            set.push((id, i, args[1].clamp(0, 255) as f32 / 255.0, args[2] != 0));
        }
    }
    for (id, i, alpha, additive) in set {
        for (k, l) in map.lines.iter_mut().enumerate() {
            if id.map_or(k == i, |id| l.ids.contains(&id)) {
                l.alpha = alpha;
                l.additive = additive;
            }
        }
    }
}

/// Reads vanilla Doom's nodes, or ZDoom's extended ones when the NODES
/// lump holds those.
fn classic_bsp(nodes: &[u8], subsectors: &[u8], segs: &[u8], vertex_count: usize) -> Option<Bsp> {
    if nodes.starts_with(b"XNOD")
        || nodes.starts_with(b"ZNOD")
        || nodes.starts_with(b"XGLN")
        || nodes.starts_with(b"ZGLN")
        || nodes.starts_with(b"XGL2")
        || nodes.starts_with(b"ZGL2")
        || nodes.starts_with(b"XGL3")
        || nodes.starts_with(b"ZGL3")
    {
        return super::nodes::read_extended(nodes, vertex_count);
    }
    if subsectors.is_empty() || segs.is_empty() || nodes.starts_with(b"xNd4") {
        return None;
    }
    let child = |c: u16| if c & 0x8000 != 0 { SUBSECTOR | (c & 0x7fff) as u32 } else { c as u32 };
    let nodes = nodes
        .chunks_exact(28)
        .map(|n| Node {
            x: sle16(n, 0),
            y: sle16(n, 2),
            dx: sle16(n, 4),
            dy: sle16(n, 6),
            children: [child(le16(n, 24)), child(le16(n, 26))],
        })
        .collect();
    let subsectors = subsectors.chunks_exact(4).map(|s| (le16(s, 2) as usize, le16(s, 0) as usize)).collect();
    let segs = segs
        .chunks_exact(12)
        .map(|s| Seg {
            v1: le16(s, 0) as usize,
            v2: le16(s, 2) as usize,
            line: le16(s, 6) as usize,
            side: (le16(s, 8) != 0) as u8,
        })
        .collect();
    Some(Bsp { nodes, subsectors, segs, extra_vertices: Vec::new() })
}

//endregion

/*┌─────────────────╮
| UDMF            |
└─────────────────╯*/
//region UDMF

/// Reads a UDMF map's TEXTMAP: blocks such as `vertex { x = 0.0; y = 0.0; }`
/// in order.  Megawads' maps run to 20 MB of text, so fields are matched
/// as they stream past, without building a table per block.
fn udmf(text: &[u8]) -> Option<MapData> {
    let mut lex = Lexer { s: text, i: 0 };
    let mut map = MapData {
        format: MapFormat::Udmf(String::new()),
        vertices: Vec::new(),
        vertex_heights: HashMap::new(),
        lines: Vec::new(),
        sides: Vec::new(),
        sectors: Vec::new(),
        things: Vec::new(),
        bsp: None,
    };
    let mut word: Option<&[u8]> = None;
    while let Some(tok) = lex.next() {
        match tok {
            Tok::Word(w) => word = Some(w),
            Tok::Str(_) => word = None,
            Tok::Sym(b'=') => {
                // A top-level assignment; only the namespace matters.
                let v = lex.next();
                if word.is_some_and(|w| w.eq_ignore_ascii_case(b"namespace"))
                    && let Some(Tok::Str(v) | Tok::Word(v)) = v
                {
                    map.format = MapFormat::Udmf(String::from_utf8_lossy(v).to_ascii_lowercase());
                }
                word = None;
            }
            Tok::Sym(b'{') => {
                let kind = word.take().unwrap_or(b"");
                let lower = kind.to_ascii_lowercase();
                match lower.as_slice() {
                    b"vertex" => vertex(&mut lex, &mut map),
                    b"linedef" => map.lines.push(linedef(&mut lex)),
                    b"sidedef" => map.sides.push(sidedef(&mut lex)),
                    b"sector" => map.sectors.push(sector(&mut lex)),
                    b"thing" => map.things.push(thing(&mut lex)),
                    _ => fields(&mut lex, |_, _| {}),
                }
            }
            Tok::Sym(_) => word = None,
        }
    }
    if map.lines.is_empty() && map.vertices.is_empty() {
        return None;
    }
    translucent_lines(&mut map);
    Some(map)
}

/// A field's value: a number, a quoted string or a word (`true`).
#[derive(Clone, Copy)]
struct Value<'a> {
    raw: &'a [u8],
}

impl Value<'_> {
    fn num(self) -> Option<f64> {
        let s = std::str::from_utf8(self.raw).ok()?;
        let v = if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            i64::from_str_radix(h, 16).ok()? as f64
        } else {
            s.parse::<f64>().ok()?
        };
        v.is_finite().then_some(v)
    }

    fn int(self) -> Option<i32> {
        self.num().map(|v| v.clamp(i32::MIN as f64, i32::MAX as f64) as i32)
    }

    fn index(self) -> Option<usize> {
        self.num().filter(|v| (0.0..1e9).contains(v)).map(|v| v as usize)
    }

    fn flag(self) -> bool {
        self.raw.eq_ignore_ascii_case(b"true")
    }

    fn texture(self) -> String {
        String::from_utf8_lossy(self.raw).to_ascii_uppercase()
    }

    /// A colour as 0xRRGGBB.
    fn color(self) -> Option<u32> {
        self.num().map(|v| (v as i64 & 0xff_ffff) as u32)
    }

    /// A list of IDs, as UDMF's `moreids` writes them: "1 2 3".
    fn ids(self) -> Vec<i32> {
        String::from_utf8_lossy(self.raw).split_whitespace().filter_map(|s| s.parse().ok()).take(64).collect()
    }
}

/// Feeds each `key = value;` of a block to `f`, keys lower case, up to the
/// closing brace.
fn fields<'a>(lex: &mut Lexer<'a>, mut f: impl FnMut(&[u8], Value<'a>)) {
    let mut key: Option<&'a [u8]> = None;
    let mut buf = [0u8; 32];
    while let Some(tok) = lex.next() {
        match tok {
            Tok::Sym(b'}') => break,
            Tok::Word(w) => key = Some(w),
            Tok::Sym(b'=') => {
                let (Some(k), Some(Tok::Word(v) | Tok::Str(v))) = (key.take(), lex.next()) else { continue };
                if k.len() > buf.len() {
                    continue;
                }
                let lower = &mut buf[..k.len()];
                lower.copy_from_slice(k);
                lower.make_ascii_lowercase();
                f(lower, Value { raw: v });
            }
            _ => key = None,
        }
    }
}

fn vertex(lex: &mut Lexer, map: &mut MapData) {
    let (mut x, mut y, mut zf, mut zc) = (0.0, 0.0, None, None);
    fields(lex, |k, v| match k {
        b"x" => x = v.num().unwrap_or(0.0),
        b"y" => y = v.num().unwrap_or(0.0),
        b"zfloor" => zf = v.num(),
        b"zceiling" => zc = v.num(),
        _ => {}
    });
    if zf.is_some() || zc.is_some() {
        map.vertex_heights.insert(map.vertices.len(), [zf, zc]);
    }
    map.vertices.push([x, y]);
}

fn linedef(lex: &mut Lexer) -> LineDef {
    let mut l = LineDef { v1: usize::MAX, v2: usize::MAX, alpha: 1.0, ..Default::default() };
    let mut id: Option<i32> = None;
    let mut more = Vec::new();
    fields(lex, |k, v| {
        let bit = match k {
            b"twosided" => ML_TWOSIDED,
            b"dontpegtop" => ML_DONTPEGTOP,
            b"dontpegbottom" => ML_DONTPEGBOTTOM,
            b"secret" => ML_SECRET,
            b"dontdraw" => ML_DONTDRAW,
            b"wrapmidtex" => ML_WRAPMIDTEX,
            b"clipmidtex" => ML_CLIPMIDTEX,
            _ => 0,
        };
        if bit != 0 {
            if v.flag() {
                l.flags |= bit;
            }
            return;
        }
        match k {
            b"v1" => l.v1 = v.index().unwrap_or(usize::MAX),
            b"v2" => l.v2 = v.index().unwrap_or(usize::MAX),
            b"sidefront" => l.front = v.index(),
            b"sideback" => l.back = v.index(),
            b"special" => l.special = v.num().unwrap_or(0.0).clamp(0.0, 65535.0) as u32,
            b"arg0" => l.args[0] = v.int().unwrap_or(0),
            b"arg1" => l.args[1] = v.int().unwrap_or(0),
            b"arg2" => l.args[2] = v.int().unwrap_or(0),
            b"arg3" => l.args[3] = v.int().unwrap_or(0),
            b"arg4" => l.args[4] = v.int().unwrap_or(0),
            b"id" => id = v.int(),
            b"moreids" => more = v.ids(),
            b"alpha" => l.alpha = v.num().unwrap_or(1.0).clamp(0.0, 1.0) as f32,
            b"renderstyle" => l.additive = v.raw.eq_ignore_ascii_case(b"add"),
            _ => {}
        }
    });
    if let Some(id) = id.filter(|&i| i != -1) {
        l.ids.push(id);
    }
    l.ids.extend(more);
    l
}

fn sidedef(lex: &mut Lexer) -> SideDef {
    let mut s = SideDef::default();
    fields(lex, |k, v| {
        let part = |suffix: &[u8]| match suffix {
            b"top" => Some(UPPER),
            b"mid" => Some(MIDDLE),
            b"bottom" => Some(LOWER),
            _ => None,
        };
        match k {
            b"offsetx" => s.x_offset = v.num().unwrap_or(0.0),
            b"offsety" => s.y_offset = v.num().unwrap_or(0.0),
            b"texturetop" => s.upper = v.texture(),
            b"texturebottom" => s.lower = v.texture(),
            b"texturemiddle" => s.middle = v.texture(),
            b"sector" => s.sector = v.index().unwrap_or(usize::MAX),
            b"light" => s.light = v.int().unwrap_or(0),
            b"lightabsolute" => s.light_absolute = v.flag(),
            b"nofakecontrast" => s.no_fake_contrast = v.flag(),
            b"smoothlighting" => s.smooth_lighting = v.flag(),
            b"lightfog" => s.light_fog = v.flag(),
            _ => {
                let scale = |x: Option<f64>| x.filter(|x| x.abs() > 1e-4 && x.abs() < 1e4).unwrap_or(1.0);
                if let Some(p) = k.strip_prefix(b"offsetx_").and_then(part) {
                    s.part_offsets[p].0 = v.num().unwrap_or(0.0);
                } else if let Some(p) = k.strip_prefix(b"offsety_").and_then(part) {
                    s.part_offsets[p].1 = v.num().unwrap_or(0.0);
                } else if let Some(p) = k.strip_prefix(b"scalex_").and_then(part) {
                    s.part_scales[p].0 = scale(v.num());
                } else if let Some(p) = k.strip_prefix(b"scaley_").and_then(part) {
                    s.part_scales[p].1 = scale(v.num());
                } else if let Some(p) = k.strip_prefix(b"light_").and_then(part) {
                    s.part_light[p] = v.int().unwrap_or(0);
                } else if let Some(p) = k.strip_prefix(b"lightabsolute_").and_then(part) {
                    s.part_light_absolute[p] = v.flag();
                }
            }
        }
    });
    s
}

fn sector(lex: &mut Lexer) -> Sector {
    let mut s = Sector::default();
    let mut planes = [[None::<f64>; 4]; 2];
    let mut id: Option<i32> = None;
    let mut more = Vec::new();
    fields(lex, |k, v| {
        let scale = |x: Option<f64>| x.filter(|x| x.abs() > 1e-4 && x.abs() < 1e4).unwrap_or(1.0);
        match k {
            b"heightfloor" => s.floor = v.num().unwrap_or(0.0),
            b"heightceiling" => s.ceiling = v.num().unwrap_or(0.0),
            b"texturefloor" => s.floor_flat = v.texture(),
            b"textureceiling" => s.ceiling_flat = v.texture(),
            b"lightlevel" => s.light = v.num().unwrap_or(160.0).clamp(0.0, 255.0),
            b"special" => s.special = v.num().unwrap_or(0.0).clamp(0.0, 65535.0) as u32,
            b"id" => id = v.int(),
            b"moreids" => more = v.ids(),
            b"xpanningfloor" => s.flats[FLOOR].x_pan = v.num().unwrap_or(0.0),
            b"ypanningfloor" => s.flats[FLOOR].y_pan = v.num().unwrap_or(0.0),
            b"xpanningceiling" => s.flats[CEILING].x_pan = v.num().unwrap_or(0.0),
            b"ypanningceiling" => s.flats[CEILING].y_pan = v.num().unwrap_or(0.0),
            b"xscalefloor" => s.flats[FLOOR].x_scale = scale(v.num()),
            b"yscalefloor" => s.flats[FLOOR].y_scale = scale(v.num()),
            b"xscaleceiling" => s.flats[CEILING].x_scale = scale(v.num()),
            b"yscaleceiling" => s.flats[CEILING].y_scale = scale(v.num()),
            b"rotationfloor" => s.flats[FLOOR].rotation = v.num().unwrap_or(0.0),
            b"rotationceiling" => s.flats[CEILING].rotation = v.num().unwrap_or(0.0),
            b"lightfloor" => s.plane_light[FLOOR].0 = v.int().unwrap_or(0),
            b"lightceiling" => s.plane_light[CEILING].0 = v.int().unwrap_or(0),
            b"lightfloorabsolute" => s.plane_light[FLOOR].1 = v.flag(),
            b"lightceilingabsolute" => s.plane_light[CEILING].1 = v.flag(),
            b"lightcolor" => s.light_color = v.color(),
            b"fadecolor" => s.fade_color = v.color(),
            b"fogdensity" => s.fog_density = v.num().map(|d| d.clamp(0.0, 510.0)),
            b"desaturation" => s.desaturation = v.num().unwrap_or(0.0).clamp(0.0, 1.0),
            b"color_floor" => s.colors[FLOOR] = v.color(),
            b"color_ceiling" => s.colors[CEILING] = v.color(),
            b"color_walltop" => s.colors[COLOR_WALLTOP] = v.color(),
            b"color_wallbottom" => s.colors[COLOR_WALLBOTTOM] = v.color(),
            // -1 turns a glow off, and 0 leaves it to GLDEFS.
            b"floorglowcolor" => s.glow[FLOOR].0 = v.num().filter(|c| *c > 0.0).and(v.color()),
            b"ceilingglowcolor" => s.glow[CEILING].0 = v.num().filter(|c| *c > 0.0).and(v.color()),
            b"floorglowheight" => s.glow[FLOOR].1 = v.num().unwrap_or(64.0).clamp(0.0, 1e5),
            b"ceilingglowheight" => s.glow[CEILING].1 = v.num().unwrap_or(64.0).clamp(0.0, 1e5),
            b"alphafloor" => s.plane_alpha[FLOOR] = v.num().unwrap_or(1.0).clamp(0.0, 1.0),
            b"alphaceiling" => s.plane_alpha[CEILING] = v.num().unwrap_or(1.0).clamp(0.0, 1.0),
            _ => {
                let plane = |p: &[u8]| match p {
                    b"a" => Some(0),
                    b"b" => Some(1),
                    b"c" => Some(2),
                    b"d" => Some(3),
                    _ => None,
                };
                if let Some(i) = k.strip_prefix(b"floorplane_").and_then(plane) {
                    planes[FLOOR][i] = v.num();
                } else if let Some(i) = k.strip_prefix(b"ceilingplane_").and_then(plane) {
                    planes[CEILING][i] = v.num();
                }
            }
        }
    });
    for (p, values) in planes.iter().enumerate() {
        if let [Some(a), Some(b), Some(c), Some(d)] = *values {
            let len = (a * a + b * b + c * c).sqrt();
            if len > 1e-9 && c.abs() > 1e-6 {
                s.planes[p] = Some([a / len, b / len, c / len, d / len]);
            }
        }
    }
    s.tags.push(id.unwrap_or(0));
    s.tags.extend(more);
    s
}

fn thing(lex: &mut Lexer) -> Thing {
    let mut t = Thing::default();
    let (mut single, mut multi) = (false, false);
    fields(lex, |k, v| match k {
        b"x" => t.x = v.num().unwrap_or(0.0),
        b"y" => t.y = v.num().unwrap_or(0.0),
        b"height" => t.z = v.num().unwrap_or(0.0),
        b"angle" => t.angle = v.num().unwrap_or(0.0),
        b"type" => t.kind = v.num().unwrap_or(0.0).clamp(0.0, 1e6) as u32,
        b"id" => t.id = v.int().unwrap_or(0),
        b"special" => t.special = v.num().unwrap_or(0.0).clamp(0.0, 65535.0) as u32,
        b"arg0" => t.args[0] = v.int().unwrap_or(0),
        b"arg1" => t.args[1] = v.int().unwrap_or(0),
        b"arg2" => t.args[2] = v.int().unwrap_or(0),
        b"arg3" => t.args[3] = v.int().unwrap_or(0),
        b"arg4" => t.args[4] = v.int().unwrap_or(0),
        b"skill2" if v.flag() => t.flags |= MTF_EASY,
        b"skill3" if v.flag() => t.flags |= MTF_NORMAL,
        b"skill4" if v.flag() => t.flags |= MTF_HARD,
        b"single" => single = v.flag(),
        b"coop" | b"dm" => multi |= v.flag(),
        _ => {}
    });
    if !single && multi {
        t.flags |= MTF_MULTIPLAYER;
    }
    t
}

#[derive(Clone, Copy)]
enum Tok<'a> {
    /// A name, number or `true`.
    Word(&'a [u8]),
    /// A quoted string without its quotes (escapes are rare in maps and
    /// kept as written).
    Str(&'a [u8]),
    /// One of `{`, `}`, `=` and `;`.
    Sym(u8),
}

/// Splits UDMF text into tokens one at a time, borrowing from the text,
/// skipping comments.
struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Lexer<'a> {
    fn next(&mut self) -> Option<Tok<'a>> {
        let s = self.s;
        loop {
            let c = *s.get(self.i)?;
            if c.is_ascii_whitespace() {
                self.i += 1;
            } else if c == b'/' && s.get(self.i + 1) == Some(&b'/') {
                while self.i < s.len() && s[self.i] != b'\n' {
                    self.i += 1;
                }
            } else if c == b'/' && s.get(self.i + 1) == Some(&b'*') {
                self.i += 2;
                while self.i + 1 < s.len() && !(s[self.i] == b'*' && s[self.i + 1] == b'/') {
                    self.i += 1;
                }
                self.i = (self.i + 2).min(s.len());
            } else {
                break;
            }
        }
        let c = s[self.i];
        if b"{}=;".contains(&c) {
            self.i += 1;
            return Some(Tok::Sym(c));
        }
        if c == b'"' {
            let start = self.i + 1;
            self.i = start;
            while self.i < s.len() && s[self.i] != b'"' {
                if s[self.i] == b'\\' {
                    self.i += 1;
                }
                self.i += 1;
            }
            let end = self.i.min(s.len());
            self.i = (self.i + 1).min(s.len());
            return Some(Tok::Str(&s[start..end]));
        }
        let start = self.i;
        while self.i < s.len() && !s[self.i].is_ascii_whitespace() && !b"{}=;\"".contains(&s[self.i]) {
            self.i += 1;
        }
        Some(Tok::Word(&s[start..self.i]))
    }
}

//endregion
