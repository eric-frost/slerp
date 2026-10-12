//! A Doom map as a 3D level: floors and ceilings from each sector's area,
//! walls from its linedefs with Doom's texture alignment, sector light,
//! sky, the player's start, and the things as sprites and markers.
//!
//! GZDoom's additions are built too: sloped floors and ceilings (walls are
//! cut to the planes' heights at each end of the line), 3D floors (slabs
//! with their own planes and sides, and the light they cast below), flat
//! panning, scaling and rotation, per-wall texture scale and light,
//! coloured sector light, glowing planes, translucent lines, and distance
//! fog per surface (`Material::fog`).
//!
//! Scale: a map unit is an inch across, as for Quake, and 1.2 inches tall,
//! because Doom drew its 320x200 frames on 4:3 screens, so every pixel and
//! the world with it looked 1.2 times taller than wide.  The 56-unit-tall
//! player is then 1.71 m, with eyes at 1.25 m (41 units).

use super::gldefs::Gldefs;
use super::graphics::{Graphics, Tex};
use super::lighting::{Colormap, Fog, Lighting};
use super::map::{
    CEILING, COLOR_WALLBOTTOM, COLOR_WALLTOP, FLOOR, LOWER, MIDDLE, ML_DONTPEGBOTTOM, ML_DONTPEGTOP, ML_WRAPMIDTEX,
    MTF_MULTIPLAYER, MapData, MapFormat, SideDef, UPPER,
};
use super::mapinfo::MapInfo;
use super::planes::{self, Plane};
use super::xfloors::{
    self, FF_EXISTS, FF_FIX, FF_FOG, FF_LOWERTEXTURE, FF_RENDERPLANES, FF_RENDERSIDES, XFloor, XFloors,
};
use super::{polygon, sky, things};
use crate::pixels;
use crate::read::Result;
use slerp_scene::glam::{Vec2, Vec3, Vec4};
use slerp_scene::{
    AlphaMode, Effect, Filter, Info, Limits, LoadError, Marker, Material, Mesh, Primitive, Sampler, Scene, SceneKind,
    Spawn, TexRef, Texture, Wrap,
};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

/// Metres per map unit across.
pub const UNIT: f32 = crate::UNIT;
/// How much taller than wide a map unit is.
pub const STRETCH: f32 = 1.2;
/// The player's eyes above the floor, in map units.
pub const EYE_HEIGHT: f64 = 41.0;
/// How high the sky's walls reach, in map units, as in GZDoom.
const SKY_WALL_TOP: f64 = 32768.0;

/// Converts a Doom point (x east, y north, z up, map units) to Slerp's
/// space (Y up, metres).
pub fn to_world(x: f64, y: f64, z: f64) -> Vec3 {
    Vec3::new(x as f32 * UNIT, z as f32 * UNIT * STRETCH, -(y as f32) * UNIT)
}

/// What the WADs say about a map beyond its lumps: MAPINFO's entry, the
/// class names of editor numbers, and GLDEFS.
#[derive(Default)]
pub struct Extras {
    pub info: Option<MapInfo>,
    pub ednums: HashMap<u32, String>,
    pub gldefs: Gldefs,
}

/// What a surface wears, which picks its material.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Surface {
    Wall(String),
    /// A two-sided line's middle texture: see-through where it has holes.
    Masked(String),
    Flat(String),
    Sky,
    /// A texture the WADs don't have, or a camera's.
    Missing,
}

/// The node each surface goes under, so the viewer can hide ceilings to
/// look down into the map, or show the areas a player never sees.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Walls,
    Floors,
    Ceilings,
    Sky,
    ThreeD,
    Unseen,
}

const GROUPS: [&str; 6] = ["Walls", "Floors", "Ceilings", "Sky", "3D floors", "Unseen areas"];

/// Whether a flat name is Doom's sky marker (F_SKY1, and F_SKY in some
/// ports).
pub fn is_sky(flat: &str) -> bool {
    flat.starts_with("F_SKY")
}

/// The brightness Doom gives a surface of sector light `level` (0-255),
/// as a linear factor, with fake contrast `contrast` (-1, 0 or 1): the
/// software light model at about 4 m.
#[cfg(test)]
pub fn light_factor(level: f64, contrast: i32) -> f32 {
    let l = Lighting::new(&MapFormat::Doom, None);
    super::lighting::linear(l.brightness(level, contrast * 16))
}

/// Which sky texture a map shows: Doom's episodes and Doom II's thirds.
pub fn sky_name(map: &str) -> String {
    let m = map.to_ascii_uppercase();
    let b = m.as_bytes();
    if b.len() == 4 && b[0] == b'E' && b[2] == b'M' && b[1].is_ascii_digit() {
        return format!("SKY{}", (b[1] - b'0').clamp(1, 4));
    }
    if let Some(n) = m.strip_prefix("MAP").and_then(|n| n.parse::<u32>().ok()) {
        return match n {
            0..=11 => "SKY1",
            12..=20 => "SKY2",
            _ => "SKY3",
        }
        .into();
    }
    "SKY1".into()
}

/// A material and how its texture sits in the world.
#[derive(Clone, Copy)]
struct Mat {
    index: usize,
    /// Texels across and down.
    texels: (f64, f64),
    /// The texture's own texels per map unit.
    scale: (f64, f64),
    world_panning: bool,
}

impl Mat {
    /// Size in map units.
    fn size(&self) -> (f64, f64) {
        (self.texels.0 / self.scale.0, self.texels.1 / self.scale.1)
    }
}

/// How a surface is drawn beyond its texture.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Look {
    fog: Option<(u32, u32, u32)>,
    blend: bool,
}

struct Builder<'g, 'a> {
    gfx: &'g mut Graphics<'a>,
    max_texture: Option<u32>,
    scene: Scene,
    materials: HashMap<(Surface, Look), Mat>,
    textures: HashMap<Surface, Option<(usize, Arc<Tex>)>>,
    groups: [HashMap<usize, Primitive>; 6],
    missing: BTreeSet<String>,
    triangles: u64,
    sky: Option<Mat>,
    sky_name: String,
    extras: &'g Extras,
    /// Textures are filtered up close, as GZDoom draws them.
    smooth: bool,
}

impl Builder<'_, '_> {
    /// The texture a surface wears, uploaded once.
    fn texture(&mut self, surface: &Surface) -> Option<(usize, Arc<Tex>)> {
        if let Some(t) = self.textures.get(surface) {
            return t.clone();
        }
        let tex = match surface {
            Surface::Wall(n) | Surface::Masked(n) => self.gfx.wall(n),
            Surface::Flat(n) => self.gfx.flat(n),
            Surface::Sky | Surface::Missing => None,
        };
        let made = tex.map(|t| {
            let name = match surface {
                Surface::Wall(n) | Surface::Masked(n) | Surface::Flat(n) => n.as_str(),
                _ => "",
            };
            (self.push_texture(&t.picture, name), t)
        });
        self.textures.insert(surface.clone(), made.clone());
        made
    }

    fn material(&mut self, surface: &Surface, fog: Option<Fog>, blend: bool) -> Mat {
        if *surface == Surface::Sky {
            return self.sky_material();
        }
        let look = Look { fog: fog.map(|f| f.key()), blend };
        if let Some(m) = self.materials.get(&(surface.clone(), look)) {
            return *m;
        }
        let made = match (surface, self.texture(surface)) {
            (Surface::Wall(n) | Surface::Masked(n) | Surface::Flat(n), Some((tex, t))) => {
                let mut m =
                    Material { name: n.clone(), base_color_texture: Some(texref(tex, self.smooth)), ..base_material() };
                if blend {
                    m.alpha_mode = AlphaMode::Blend;
                } else if matches!(surface, Surface::Masked(_)) && t.picture.holes {
                    m.alpha_mode = AlphaMode::Mask(0.5);
                }
                m.fog = Some(scene_fog(fog));
                Mat {
                    index: self.push_material(m),
                    texels: (t.picture.width as f64, t.picture.height as f64),
                    scale: t.scale,
                    world_panning: t.world_panning,
                }
            }
            (Surface::Wall(n) | Surface::Masked(n) | Surface::Flat(n), None) => {
                if !self.gfx.is_camera(n) {
                    self.missing.insert(n.clone());
                }
                return self.material(&Surface::Missing, fog, blend);
            }
            _ => {
                let mut m =
                    Material { name: "missing".into(), base_color: Vec4::new(0.5, 0.5, 0.5, 1.0), ..base_material() };
                m.fog = Some(scene_fog(fog));
                if blend {
                    m.alpha_mode = AlphaMode::Blend;
                }
                Mat { index: self.push_material(m), texels: (64.0, 64.0), scale: (1.0, 1.0), world_panning: false }
            }
        };
        self.materials.insert((surface.clone(), look), made);
        made
    }

    /// The sky: MAPINFO's `sky1`, a GLDEFS sky box or a texture, wrapped
    /// around the viewer.
    fn sky_material(&mut self) -> Mat {
        if let Some(m) = self.sky {
            return m;
        }
        let name = self.sky_name.clone();
        let mut m = Material { name: "sky".into(), unlit: true, effect: Effect::PanoramaSky, ..base_material() };
        let picture = match self.extras.gldefs.skyboxes.get(&name) {
            Some(b) => {
                let faces: Option<Vec<_>> =
                    b.faces.iter().map(|f| self.gfx.wall(f).map(|t| t.picture.clone())).collect();
                faces.and_then(|f| sky::from_box(&f, b.flip_top))
            }
            None => self.gfx.wall(&name).or_else(|| self.gfx.wall("SKY1")).map(|t| sky::classic(&t)),
        };
        if let Some(p) = picture {
            m.name = format!("sky ({name})");
            let tex = self.push_texture(&p.picture, "sky");
            m.base_color_texture =
                Some(TexRef { sampler: p.sampler, transform: Some(p.transform), ..TexRef::new(tex) });
        }
        let made = Mat { index: self.push_material(m), texels: (64.0, 64.0), scale: (1.0, 1.0), world_panning: false };
        self.sky = Some(made);
        made
    }

    fn push_material(&mut self, m: Material) -> usize {
        self.scene.materials.push(m);
        self.scene.materials.len() - 1
    }

    fn push_texture(&mut self, p: &super::graphics::Picture, name: &str) -> usize {
        let t: Texture = pixels::texture(
            p.width,
            p.height,
            p.rgba.clone(),
            self.max_texture,
            "Doom WAD",
            &format!("{name} ({}x{})", p.width, p.height),
        );
        self.scene.textures.push(Arc::new(t));
        self.scene.textures.len() - 1
    }

    /// Adds one polygon's vertices and triangles to its material's
    /// primitive in `group`.
    fn add(&mut self, group: Group, material: usize, verts: &[(Vec3, Vec2, Vec4)], normal: Vec3, tris: &[[u32; 3]]) {
        let p = self.groups[group as usize]
            .entry(material)
            .or_insert_with(|| Primitive { material: Some(material), ..Default::default() });
        let base = p.positions.len() as u32;
        for &(pos, uv, color) in verts {
            p.positions.push(pos);
            p.normals.push(normal);
            p.uv0.push(uv);
            p.colors.push(color);
        }
        for t in tris {
            p.indices.extend_from_slice(&[base + t[0], base + t[1], base + t[2]]);
        }
        self.triangles += tris.len() as u64;
    }
}

/// A surface's fog as the scene keeps it.  Every Doom surface has one,
/// even without fog (density 0), so the renderer shades it as Doom does:
/// light times texture in display values.
fn scene_fog(fog: Option<Fog>) -> slerp_scene::Fog {
    match fog {
        Some(f) => slerp_scene::Fog {
            color: f.color,
            density: f.density,
            near_light: f.near_light,
            near_distance: f.near_distance,
        },
        None => slerp_scene::Fog { color: Vec3::ZERO, density: 0.0, near_light: 1.0, near_distance: 0.0 },
    }
}

fn base_material() -> Material {
    Material { base_color: Vec4::ONE, roughness: 1.0, specular: 0.0, unlit: true, ..Default::default() }
}

/// Doom's pixel look: hard texels up close, smoothed only when shrunk.
/// Maps made for GZDoom are smoothed up close too, as GZDoom's default
/// trilinear filtering draws them (`smooth`).
fn texref(texture: usize, smooth: bool) -> TexRef {
    TexRef {
        sampler: Sampler {
            wrap_u: Wrap::Repeat,
            wrap_v: Wrap::Repeat,
            mag: if smooth { Filter::Linear } else { Filter::Nearest },
            min: Filter::Linear,
            mipmaps: true,
        },
        ..TexRef::new(texture)
    }
}

/// Facts about the build, for tests and the inspector.
#[derive(Default, Debug)]
pub struct Stats {
    /// Sectors whose lines didn't close into loops.
    pub broken_sectors: usize,
    /// Of those, the ones drawn from the BSP's subsectors.
    pub bsp_sectors: usize,
    /// The traced loops' area and the triangles' area, summed over sectors
    /// drawn from loops.
    pub loop_area: f64,
    pub triangle_area: f64,
    /// Sloped floors and ceilings, and 3D floors.
    pub slopes: usize,
    pub three_d_floors: usize,
    /// Sectors with coloured light or fog.
    pub colored: usize,
    pub fogged: usize,
}

/*┌─────────────────╮
| Level           |
└─────────────────╯*/
//region Level

/// Everything the surfaces of a map share while it is built.
struct Ctx<'m> {
    map: &'m MapData,
    planes: [Vec<Plane>; 2],
    xf: XFloors,
    lighting: Lighting,
    colormaps: Vec<Colormap>,
    centers: Vec<[f64; 2]>,
    /// Sectors drawn under "Unseen areas", a hidden node: dummies a
    /// player never sees (see `unseen_sectors`).
    hidden: Vec<bool>,
    /// Floors and ceilings that are portals (`Sector_SetPortal`): sky
    /// boxes draw as sky, other portals (views into another part of the
    /// map) as nothing, rather than their placeholder textures.
    portals: Vec<[Portal; 2]>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Portal {
    None,
    Sky,
    Hidden,
}

/// Every sector plane `Sector_SetPortal` (57) makes a portal.
fn portal_planes(map: &MapData) -> Vec<[Portal; 2]> {
    let mut out = vec![[Portal::None; 2]; map.sectors.len()];
    for l in &map.lines {
        let (special, args) = map.hexen_special(l);
        if special != 57 || !map.format.hexen_specials() {
            continue;
        }
        let kind = match args[1] {
            2 => Portal::Sky,
            5 => continue,
            _ => Portal::Hidden,
        };
        let planes: &[usize] = match args[2] {
            0 => &[FLOOR],
            1 => &[CEILING],
            _ => &[FLOOR, CEILING],
        };
        let targets: Vec<usize> =
            if args[0] == 0 { map.sector_of(l.front).into_iter().collect() } else { map.tagged(args[0]).collect() };
        for s in targets {
            for &p in planes {
                out[s][p] = kind;
            }
        }
    }
    out
}

impl Ctx<'_> {
    fn z(&self, which: usize, s: usize, p: [f64; 2]) -> f64 {
        self.planes[which][s].z_at(p[0], p[1])
    }

    /// Whether a sector's ceiling shows the sky.
    fn sky_ceiling(&self, s: usize) -> bool {
        is_sky(&self.map.sectors[s].ceiling_flat) || self.portals[s][CEILING] == Portal::Sky
    }

    /// The light level and light-colour sector of zone `zone` of sector
    /// `s`, with a plane's own light when it is the sector's.
    fn zone_light(&self, s: usize, zone: usize, plane: Option<usize>) -> (f64, usize) {
        let z = &self.xf.zones[s][zone];
        let sec = &self.map.sectors[s];
        match plane {
            Some(which) if z.color_from == s => {
                let (rel, abs) = sec.plane_light[which];
                (if abs { rel as f64 } else { sec.light + rel as f64 }.clamp(0.0, 255.0), s)
            }
            _ => (z.light, z.color_from),
        }
    }

    /// The colours of light from sector `from` falling in sector `s`:
    /// the light's colour, and `s`'s fog.
    fn colormap(&self, s: usize, from: usize) -> Colormap {
        Colormap { light: self.colormaps[from].light, ..self.colormaps[s] }
    }

    /// A glow's colour (display RGB) and height for sector `s`'s plane
    /// `which`: UDMF's, else GLDEFS' for its flat.
    fn glow(&self, s: usize, which: usize, gfx: &mut Graphics, extras: &Extras) -> Option<([f32; 3], f64)> {
        let sec = &self.map.sectors[s];
        let (color, height) = sec.glow[which];
        if let Some(c) = color {
            return Some(([(c >> 16) & 255, (c >> 8) & 255, c & 255].map(|v| v as f32 / 255.0), height));
        }
        let flat = if which == FLOOR { &sec.floor_flat } else { &sec.ceiling_flat };
        let glow = extras.gldefs.glows.get(flat)?;
        let rgb = match glow {
            Some(c) => *c,
            None => {
                let t = gfx.flat(flat)?;
                average(&t)
            }
        };
        Some((rgb, 64.0))
    }
}

/// Sectors a player never sees directly: groups of sectors joined by
/// two-sided lines that hold nothing but markers for the editor and the
/// engine (dummies placed in the void: 3D floors' control sectors, the far
/// ends of portals, scripted scenery), and sky boxes' scenery (groups with
/// a sky viewpoint and no player start).  GZDoom never draws them (its BSP
/// gives no view into them); here they go under a hidden node.
fn unseen_sectors(
    map: &MapData,
    sector_at: &dyn Fn(f64, f64) -> Option<usize>,
    ednums: &HashMap<u32, String>,
) -> Vec<bool> {
    let n = map.sectors.len();
    // 1. Groups of sectors joined by two-sided lines.
    let mut group: Vec<usize> = (0..n).collect();
    fn root(g: &mut [usize], mut i: usize) -> usize {
        while g[i] != i {
            g[i] = g[g[i]];
            i = g[i];
        }
        i
    }
    for l in &map.lines {
        if let (Some(f), Some(b)) = (map.sector_of(l.front), map.sector_of(l.back)) {
            let (rf, rb) = (root(&mut group, f), root(&mut group, b));
            if rf != rb {
                group[rf] = rb;
            }
        }
    }
    let roots: Vec<usize> = (0..n).map(|s| root(&mut group, s)).collect();

    // 2. What each group holds.
    let (mut things, mut sky_view, mut start) = (vec![false; n], vec![false; n], vec![false; n]);
    for t in &map.things {
        let Some(s) = sector_at(t.x, t.y) else { continue };
        let g = roots[s];
        things[g] |= !editor_thing(t.kind);
        let class = ednums.get(&t.kind).map(|c| c.to_ascii_lowercase()).unwrap_or_default();
        if matches!(t.kind, 9080 | 9083 | 5006) || class.contains("skyviewpoint") || class.contains("skybox") {
            sky_view[g] = true;
        }
        if (1..=4).contains(&t.kind) {
            start[g] = true;
        }
    }

    // 3. Only maps with a player start have a place a player sees from.
    let has_start = start.iter().any(|&s| s);
    (0..n)
        .map(|s| {
            let g = roots[s];
            has_start && !start[g] && (!things[g] || sky_view[g])
        })
        .collect()
}

/// Things that only mark a spot for the engine or a script: map spots,
/// paths and cameras, sky viewpoints, polyobject anchors, slope and vertex
/// things, sector actions, sound sequences, ambient sounds and dynamic
/// lights.
fn editor_thing(kind: u32) -> bool {
    matches!(kind, 1400..=1411 | 1500..=1505 | 5004 | 5006 | 9001 | 9013 | 9024 | 9025 | 9038..=9048 | 9070..=9084
        | 9200 | 9300..=9303 | 9500..=9511 | 9600..=9605 | 9800..=9899 | 9982..=9999 | 14001..=14167)
}

/// A texture's average colour, for flats that glow.
fn average(t: &Tex) -> [f32; 3] {
    let p = &t.picture;
    let mut sum = [0f64; 3];
    let step = (p.rgba.len() / 4 / 4096).max(1);
    let mut n = 0.0f64;
    for px in p.rgba.chunks_exact(4).step_by(step) {
        for k in 0..3 {
            sum[k] += px[k] as f64;
        }
        n += 1.0;
    }
    sum.map(|v| (v / n.max(1.0) / 255.0) as f32)
}

/// Builds the level.  `name` is the map's marker (MAP01, E1M1).
pub fn build(
    map: &MapData,
    name: &str,
    gfx: &mut Graphics,
    limits: &Limits,
    extras: &Extras,
) -> Result<(Scene, Stats)> {
    let info = extras.info.as_ref();
    let mut b = Builder {
        gfx,
        max_texture: limits.max_texture_size,
        scene: Scene { kind: SceneKind::Level, ..Default::default() },
        materials: HashMap::new(),
        textures: HashMap::new(),
        groups: Default::default(),
        missing: BTreeSet::new(),
        triangles: 0,
        sky: None,
        sky_name: info.and_then(|i| i.sky1.clone()).unwrap_or_else(|| sky_name(name)),
        extras,
        smooth: map.format.zdoom(),
    };
    let mut warnings = Vec::new();
    let mut stats = Stats::default();

    // 1. Sector areas, planes, 3D floors and colours.
    let areas = polygon::sector_areas(map);
    let sector_at = |x: f64, y: f64| -> Option<usize> {
        if let Some(bsp) = &map.bsp
            && let Some(s) = polygon::subsector_at(bsp, [x, y]).and_then(|ss| polygon::subsector_sector(map, bsp, ss))
        {
            return Some(s);
        }
        areas.iter().position(|a| a.contains([x, y]))
    };
    let lines_of = planes::sector_lines(map);
    let planes = planes::resolve(map, &sector_at);
    let xf = xfloors::build(map, &planes, &lines_of);
    let lighting = Lighting::new(&map.format, info);
    let colormaps: Vec<Colormap> = map.sectors.iter().map(|s| lighting.colormap(s, is_sky(&s.ceiling_flat))).collect();
    let centers: Vec<[f64; 2]> = lines_of.iter().map(|l| xfloors::center(map, l)).collect();
    let hidden = unseen_sectors(map, &sector_at, &extras.ednums);
    let portals = portal_planes(map);
    let ctx = Ctx { map, planes, xf, lighting, colormaps, centers, hidden, portals };
    for s in 0..map.sectors.len() {
        stats.slopes += (0..2).filter(|&w| ctx.planes[w][s].is_slope()).count();
        stats.three_d_floors += ctx.xf.floors[s].len();
        let sec = &map.sectors[s];
        stats.colored += sec.light_color.is_some_and(|c| c != 0xffffff) as usize;
        stats.fogged += Lighting::foggy(&ctx.colormaps[s]) as usize;
    }

    // 2. Every texture the map names, read and decoded together.
    let mut walls: Vec<String> = Vec::new();
    for sd in &map.sides {
        walls.extend([&sd.upper, &sd.lower, &sd.middle].into_iter().filter(|n| n.as_str() != "-").cloned());
    }
    let flats: Vec<String> = map.sectors.iter().flat_map(|s| [s.floor_flat.clone(), s.ceiling_flat.clone()]).collect();
    // The sky's picture or sky box faces too.
    let sky = b.sky_name.clone();
    match extras.gldefs.skyboxes.get(&sky) {
        Some(skybox) => walls.extend(skybox.faces.iter().cloned()),
        None => walls.extend([sky, "SKY1".into()]),
    }
    walls.sort();
    walls.dedup();
    let mut flats = flats;
    flats.sort();
    flats.dedup();
    b.gfx.prefetch(&walls, &flats);

    // 3. Floors and ceilings, and 3D floors' planes.
    for (s, area) in areas.iter().enumerate() {
        if area.broken && !lines_of[s].is_empty() {
            stats.broken_sectors += 1;
            if area.from_bsp {
                stats.bsp_sectors += 1;
            }
        }
        if !area.from_bsp {
            stats.loop_area += area.loop_area;
            stats.triangle_area += area.triangle_area();
        }
        if area.triangles.is_empty() {
            continue;
        }
        flats_of(&mut b, &ctx, s, area);
        for x in &ctx.xf.floors[s] {
            slab_planes(&mut b, &ctx, s, area, x);
        }
    }
    if stats.broken_sectors > 0 {
        let fixed = if map.bsp.is_some() {
            format!(", {} drawn from the map's nodes instead", stats.bsp_sectors)
        } else {
            String::new()
        };
        warnings.push(format!("{} sectors aren't closed by their lines{fixed}", stats.broken_sectors));
    }

    // 4. Walls, one side of each line at a time.
    let mut bad_lines = 0;
    for (li, l) in map.lines.iter().enumerate() {
        let Some((a, z)) = map.ends(l) else {
            bad_lines += 1;
            continue;
        };
        if a == z {
            continue;
        }
        let (front, back) = (map.sector_of(l.front), map.sector_of(l.back));
        let two = map.two_sided(l);
        if let (Some(_), Some(s)) = (l.front.and_then(|i| map.sides.get(i)), front) {
            wall_side(&mut b, &ctx, li, l.front.unwrap_or(0), a, z, s, if two { back } else { None });
        } else {
            bad_lines += 1;
        }
        if let (Some(_), Some(s)) = (l.back.and_then(|i| map.sides.get(i)), back) {
            wall_side(&mut b, &ctx, li, l.back.unwrap_or(0), z, a, s, if two { front } else { None });
        }
    }
    if b.gfx.over_budget > 0 {
        warnings.push(format!("{} pictures left out to stay within memory limits", b.gfx.over_budget));
    }
    if bad_lines > 0 {
        warnings.push(format!("{bad_lines} lines with missing vertices or sides skipped"));
    }
    if b.triangles > limits.max_triangles {
        return Err(LoadError::TooLarge(format!("{} triangles", b.triangles)));
    }
    if !b.missing.is_empty() {
        let names: Vec<&str> = b.missing.iter().map(String::as_str).take(12).collect();
        let more = if b.missing.len() > 12 { format!(" and {} more", b.missing.len() - 12) } else { String::new() };
        warnings.push(format!("{} textures missing: {}{more}", b.missing.len(), names.join(", ")));
    }

    // 5. Nodes: one per group, primitives in material order.
    let groups = std::mem::take(&mut b.groups);
    for (gi, prims) in groups.into_iter().enumerate() {
        if prims.is_empty() {
            continue;
        }
        let mut primitives: Vec<Primitive> = prims.into_values().collect();
        primitives.sort_by_key(|p| p.material);
        let node = b.scene.add_mesh_node(Mesh { name: GROUPS[gi].into(), primitives, target_names: Vec::new() });
        if gi == Group::Unseen as usize {
            b.scene.nodes[node].visible = false;
        }
    }
    if b.scene.nodes.is_empty() {
        return Err(LoadError::invalid("the map has nothing to draw"));
    }

    // 6. The player's start, the things and their sprites.
    let floor_at = |x: f64, y: f64| sector_at(x, y).map_or(0.0, |s| ctx.z(FLOOR, s, [x, y]));
    // Doom puts the player at the last player 1 start; earlier ones become
    // "voodoo dolls".
    if let Some(t) = map.things.iter().rev().find(|t| t.kind == 1) {
        b.scene.spawn = Some(Spawn {
            position: to_world(t.x, t.y, floor_at(t.x, t.y) + EYE_HEIGHT),
            yaw: ((t.angle - 90.0) as f32).to_radians(),
        });
    }
    let doom_things = !matches!(map.format, MapFormat::Hexen);
    let mut notes = Notes::default();
    for t in &map.things {
        if doom_things && t.flags & MTF_MULTIPLAYER != 0 {
            continue;
        }
        let class = extras.ednums.get(&t.kind);
        notes.count(t.kind, class.map(String::as_str));
        let known = if doom_things && class.is_none() { things::describe(t.kind) } else { None };
        let (label, color) = marker_look(t, known, class.map(String::as_str), &extras.gldefs);
        b.scene.markers.push(Marker {
            label,
            position: to_world(t.x, t.y, floor_at(t.x, t.y) + t.z + 20.0),
            color: Vec4::from_array(color),
        });
    }
    if doom_things {
        sprites(&mut b, &ctx, &sector_at);
    }
    for l in &map.lines {
        notes.line(map.hexen_special(l).0);
    }
    notes.portal_planes = ctx.portals.iter().flatten().filter(|p| **p == Portal::Hidden).count();

    // 7. Facts for the inspector.
    let mut details = vec![("Map".to_string(), name.to_string())];
    if let Some(t) = info.and_then(|i| i.title.as_ref()) {
        details.push(("Title".into(), t.clone()));
    }
    details.push(("Sectors".into(), map.sectors.len().to_string()));
    details.push(("Linedefs".into(), map.lines.len().to_string()));
    details.push(("Things".into(), map.things.len().to_string()));
    let textures = b.textures.values().filter(|t| t.is_some()).count();
    details.push(("Textures and flats".into(), textures.to_string()));
    if map.format.zdoom() || info.is_some() {
        details.push(("Light mode".into(), ctx.lighting.describe()));
    }
    for (k, v) in [
        ("Sloped planes", stats.slopes),
        ("3D floors", stats.three_d_floors),
        ("Sectors with coloured light", stats.colored),
        ("Sectors with coloured fog", stats.fogged),
    ] {
        if v > 0 {
            details.push((k.into(), v.to_string()));
        }
    }
    details.extend(notes.details());
    b.scene.name = name.to_string();
    b.scene.info = Info { format: format!("Doom map ({})", map.format.name()), warnings, details, ..Info::default() };
    Ok((b.scene, stats))
}

/// What a level leaves out, counted for the inspector: portals, sky box
/// viewpoints, dynamic lights.
#[derive(Default)]
struct Notes {
    portals: usize,
    portal_planes: usize,
    skyboxes: usize,
    lights: usize,
}

impl Notes {
    fn count(&mut self, kind: u32, class: Option<&str>) {
        let class = class.unwrap_or("").to_ascii_lowercase();
        if matches!(kind, 9077 | 9078 | 9080 | 9081 | 9083 | 5006)
            || class.contains("skybox")
            || class.contains("skyview")
        {
            self.skyboxes += 1;
        }
        if (9800..=9899).contains(&kind) {
            self.lights += 1;
        }
    }

    fn line(&mut self, special: u32) {
        // Line_SetPortal, Sector_SetPortal, Line_Horizon, Line_Mirror.
        if matches!(special, 156 | 57 | 9 | 182) {
            self.portals += 1;
        }
    }

    fn details(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if self.portals > 0 {
            out.push(("Portals and mirrors (not drawn)".into(), format!("{} lines", self.portals)));
        }
        if self.portal_planes > 0 {
            out.push(("Portal floors and ceilings (left open)".into(), self.portal_planes.to_string()));
        }
        if self.skyboxes > 0 {
            out.push(("Sky box viewpoints (not drawn)".into(), self.skyboxes.to_string()));
        }
        if self.lights > 0 {
            out.push(("Dynamic lights (markers only)".into(), self.lights.to_string()));
        }
        out
    }
}

/// A thing's marker: its name (Doom's, the mod's class, or the editor
/// number) and colour (by kind, or the colour of its dynamic light).
fn marker_look(
    t: &super::map::Thing,
    known: Option<(&str, things::Kind)>,
    class: Option<&str>,
    gldefs: &Gldefs,
) -> (String, [f32; 4]) {
    if (9800..=9899).contains(&t.kind) {
        let rgb = [t.args[0], t.args[1], t.args[2]].map(|c| (c.clamp(0, 255) as f32 / 255.0).max(0.1));
        return (format!("Dynamic light ({})", class.unwrap_or("light")), [rgb[0], rgb[1], rgb[2], 1.0]);
    }
    if let Some(c) = class {
        if let Some(rgb) =
            gldefs.objects.get(&c.to_ascii_uppercase()).and_then(|l| l.first()).and_then(|l| gldefs.lights.get(l))
        {
            return (format!("{c} (light)"), [rgb[0], rgb[1], rgb[2], 1.0]);
        }
        let kind = things::describe(t.kind).filter(|_| t.kind < 4000).map(|k| k.1);
        return (c.to_string(), things::color(kind));
    }
    let label = known.map_or_else(|| format!("Thing {}", t.kind), |k| k.0.to_string());
    (label, things::color(known.map(|k| k.1)))
}

//endregion

/*┌─────────────────╮
| Flats           |
└─────────────────╯*/
//region Flats

/// A flat's texture coordinates at a map point, as GZDoom lays flats:
/// x/64 and -y/64 rotated by the plane's rotation, scaled to the texture's
/// size, panned, then scaled by the plane's scale.
pub(crate) fn flat_uv(p: [f64; 2], t: &super::map::FlatTransform, size: (f64, f64)) -> Vec2 {
    let (sin, cos) = (-t.rotation).to_radians().sin_cos();
    let (x, y) = (p[0], -p[1]);
    let (rx, ry) = (x * cos - y * sin, x * sin + y * cos);
    let u = t.x_scale * (rx / size.0 + t.x_pan / size.0);
    let v = t.y_scale * (ry / size.1 + t.y_pan / size.1);
    Vec2::new(u as f32, v as f32)
}

/// A plane's normal in the world, facing the way `plane` does.
fn world_normal(plane: &Plane) -> Vec3 {
    let n = plane.normal();
    Vec3::new(n[0] as f32, n[2] as f32 / STRETCH, -n[1] as f32).normalize_or(Vec3::Y)
}

/// Draws one flat polygon set (a sector's area) on `plane`, facing up
/// unless `down`.
#[allow(clippy::too_many_arguments)]
fn flat(
    b: &mut Builder,
    area: &polygon::Area,
    plane: &Plane,
    down: bool,
    surface: Surface,
    xform: &super::map::FlatTransform,
    group: Group,
    color: Vec4,
    fog: Option<Fog>,
) {
    let blend = color.w < 0.999;
    let mat = b.material(&surface, fog, blend);
    let size = mat.size();
    let verts: Vec<(Vec3, Vec2, Vec4)> = area
        .points
        .iter()
        .map(|p| (to_world(p[0], p[1], plane.z_at(p[0], p[1])), flat_uv(*p, xform, size), color))
        .collect();
    let tris: Vec<[u32; 3]> =
        if down { area.triangles.iter().map(|t| [t[0], t[2], t[1]]).collect() } else { area.triangles.clone() };
    let facing = xfloors::facing(*plane, down);
    b.add(group, mat.index, &verts, world_normal(&facing), &tris);
}

/// A sector's own floor and ceiling.
fn flats_of(b: &mut Builder, ctx: &Ctx, s: usize, area: &polygon::Area) {
    let sec = &ctx.map.sectors[s];
    let c = ctx.centers[s];
    for which in [FLOOR, CEILING] {
        let name = if which == FLOOR { &sec.floor_flat } else { &sec.ceiling_flat };
        let plane = ctx.planes[which][s];
        let ceiling = which == CEILING;
        // "-" is no flat at all, and a portal shows another place: nothing
        // is drawn.
        if name.is_empty() || name == "-" || ctx.portals[s][which] == Portal::Hidden {
            continue;
        }
        if is_sky(name) || ctx.portals[s][which] == Portal::Sky {
            let group = if ctx.hidden[s] { Group::Unseen } else { Group::Sky };
            flat(b, area, &plane, ceiling, Surface::Sky, &sec.flats[which], group, Vec4::ONE, None);
            continue;
        }
        let zone = xfloors::plane_zone(&ctx.xf.zones[s], c, plane.z_at(c[0], c[1]), ceiling);
        let (level, from) = ctx.zone_light(s, zone, Some(which));
        let cm = ctx.colormap(s, from);
        let tint = sec.colors[which].map_or([1.0; 3], rgb);
        let color = ctx.lighting.color(ctx.lighting.brightness(level, 0), &cm, [0.0; 3], tint).extend(1.0);
        let fog = ctx.lighting.fog(level, &cm);
        let group = if ctx.hidden[s] {
            Group::Unseen
        } else if ceiling {
            Group::Ceilings
        } else {
            Group::Floors
        };
        flat(b, area, &plane, ceiling, Surface::Flat(name.clone()), &sec.flats[which], group, color, fog);
    }
}

fn rgb(c: u32) -> [f32; 3] {
    [(c >> 16) & 255, (c >> 8) & 255, c & 255].map(|v| v as f32 / 255.0)
}

/// A 3D floor's top and bottom over sector `s`, and their insides for
/// floors seen from within (water).
fn slab_planes(b: &mut Builder, ctx: &Ctx, s: usize, area: &polygon::Area, x: &XFloor) {
    if x.flags & FF_EXISTS == 0 || x.flags & FF_RENDERPLANES == 0 || x.flags & (FF_FOG | FF_FIX) != 0 {
        return;
    }
    let c = ctx.centers[s];
    let alpha = if x.translucent() { x.alpha as f32 / 255.0 } else { 1.0 };
    let both = x.flags & xfloors::FF_BOTHPLANES != 0;
    let group = if ctx.hidden[s] { Group::Unseen } else { Group::ThreeD };
    // (side, facing down, lit from below)
    let mut faces = vec![(&x.top, false, false), (&x.bottom, true, true)];
    if both {
        faces.extend([(&x.top, true, true), (&x.bottom, false, false)]);
    }
    for (side, down, underside) in faces {
        let model = &ctx.map.sectors[side.model];
        let name = if side.which == FLOOR { &model.floor_flat } else { &model.ceiling_flat };
        if name.is_empty() || name == "-" {
            continue;
        }
        if is_sky(name) {
            flat(b, area, &side.plane, down, Surface::Sky, &model.flats[side.which], Group::Sky, Vec4::ONE, None);
            continue;
        }
        let z = side.plane.z_at(c[0], c[1]);
        let zone = xfloors::plane_zone(&ctx.xf.zones[s], c, z, underside);
        let (level, from) = ctx.zone_light(s, zone, None);
        let cm = ctx.colormap(s, from);
        let color = ctx.lighting.color(ctx.lighting.brightness(level, 0), &cm, [0.0; 3], [1.0; 3]).extend(alpha);
        let fog = ctx.lighting.fog(level, &cm);
        let surface = Surface::Flat(name.clone());
        flat(b, area, &side.plane, down, surface, &model.flats[side.which], group, color, fog);
    }
}

//endregion

/*┌─────────────────╮
| Walls           |
└─────────────────╯*/
//region Walls

/// A polygon in a wall's own coordinates: s from 0 at the line's start to
/// 1 at its end, and z the height.
type Poly = Vec<[f64; 2]>;

/// A height along a wall: `a` at its start, `z` at its end.
#[derive(Clone, Copy)]
struct Edge {
    a: f64,
    z: f64,
}

impl Edge {
    fn at(&self, s: f64) -> f64 {
        self.a + (self.z - self.a) * s
    }

    fn flat(h: f64) -> Edge {
        Edge { a: h, z: h }
    }
}

/// Keeps the part of a convex polygon where `f` is at least zero.
fn clip(poly: &[[f64; 2]], f: impl Fn([f64; 2]) -> f64) -> Poly {
    const EPS: f64 = 1e-9;
    let n = poly.len();
    let mut out = Vec::with_capacity(n + 2);
    for i in 0..n {
        let (p, q) = (poly[i], poly[(i + 1) % n]);
        let (fp, fq) = (f(p), f(q));
        if fp >= -EPS {
            out.push(p);
        }
        if (fp >= -EPS) != (fq >= -EPS) {
            let t = fp / (fp - fq);
            out.push([p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t]);
        }
    }
    out
}

/// The part of a wall between two edges, where the top is above the
/// bottom (sloped planes may cross mid-line).
fn between(bottom: Edge, top: Edge) -> Poly {
    let lo = bottom.a.min(bottom.z).min(top.a).min(top.z) - 1.0;
    let hi = bottom.a.max(bottom.z).max(top.a).max(top.z) + 1.0;
    let rect = vec![[0.0, lo], [1.0, lo], [1.0, hi], [0.0, hi]];
    let p = clip(&rect, |q| q[1] - bottom.at(q[0]));
    clip(&p, |q| top.at(q[0]) - q[1])
}

/// How a wall part's texture sits: u at the line's start and per unit
/// along it, the height where v is 0, and the texture's height in units.
#[derive(Clone, Copy)]
struct Mapping {
    u0: f64,
    du: f64,
    top: f64,
    height: f64,
}

/// GZDoom's `FTexCoordInfo` for a wall part: the texture's scale times
/// the side's, and offsets counted in texels unless the texture pans in
/// world units.
struct TexInfo {
    width: f64,
    height: f64,
    scale: (f64, f64),
    world_panning: bool,
}

impl TexInfo {
    fn new(mat: &Mat, side_scale: (f64, f64)) -> TexInfo {
        let scale = (mat.scale.0 * side_scale.0, mat.scale.1 * side_scale.1);
        TexInfo {
            width: mat.texels.0 / scale.0,
            height: mat.texels.1 / scale.1,
            scale,
            world_panning: mat.world_panning,
        }
    }

    fn x_offset(&self, o: f64) -> f64 {
        if self.world_panning || self.scale.0 == 1.0 { o } else { o / self.scale.0 }
    }

    fn y_offset(&self, o: f64) -> f64 {
        if self.world_panning || self.scale.1 == 1.0 { o } else { o / self.scale.1 }
    }
}

/// What lights one side of a line.
struct SideLight {
    s: usize,
    rel: i32,
    /// The side's own light for each part: (amount, absolute).
    part: [(i32, bool); 3],
    side_light: (i32, bool),
    foggy_skip: bool,
}

impl SideLight {
    /// A zone's light as this side and part change it (GZDoom's
    /// `side_t::GetLightLevel`).
    fn level(&self, zone_light: f64, part: usize) -> f64 {
        let (pl, pabs) = self.part[part];
        let (sl, sabs) = self.side_light;
        let l = if pabs {
            pl as f64
        } else if sabs {
            (sl + pl) as f64
        } else if self.foggy_skip {
            zone_light
        } else {
            zone_light + (sl + pl) as f64
        };
        l.clamp(0.0, 255.0)
    }
}

/// Emits a wall polygon: split by the sector's light zones and glow
/// heights, each piece lit at its corners.
#[allow(clippy::too_many_arguments)]
fn emit(
    b: &mut Builder,
    ctx: &Ctx,
    poly: Poly,
    a: [f64; 2],
    z: [f64; 2],
    surface: Surface,
    part: usize,
    mapping: &dyn Fn(&Mat) -> Mapping,
    light: &SideLight,
    alpha: f32,
    group: Group,
) {
    if poly.len() < 3 {
        return;
    }
    let ctx_s = light.s;
    let sec = &ctx.map.sectors[ctx_s];
    let len = (z[0] - a[0]).hypot(z[1] - a[1]);
    let normal = Vec3::new(((z[1] - a[1]) / len) as f32, 0.0, ((z[0] - a[0]) / len) as f32);
    let floor = Edge { a: ctx.z(FLOOR, ctx_s, a), z: ctx.z(FLOOR, ctx_s, z) };
    let ceiling = Edge { a: ctx.z(CEILING, ctx_s, a), z: ctx.z(CEILING, ctx_s, z) };
    let glows = [FLOOR, CEILING].map(|w| ctx.glow(ctx_s, w, b.gfx, b.extras));
    let tops = sec.colors[COLOR_WALLTOP].map_or([1.0; 3], rgb);
    let bottoms = sec.colors[COLOR_WALLBOTTOM].map_or([1.0; 3], rgb);
    let zones = &ctx.xf.zones[ctx_s];
    let sky = surface == Surface::Sky;
    for zi in 0..zones.len() {
        // 1. The part of the wall in this zone.
        let mut p = poly.clone();
        if zi > 0 {
            let pl = zones[zi].plane;
            let e = Edge { a: pl.z_at(a[0], a[1]), z: pl.z_at(z[0], z[1]) };
            p = clip(&p, |q| e.at(q[0]) - q[1]);
        }
        if zi + 1 < zones.len() {
            let pl = zones[zi + 1].plane;
            let e = Edge { a: pl.z_at(a[0], a[1]), z: pl.z_at(z[0], z[1]) };
            p = clip(&p, |q| q[1] - e.at(q[0]));
        }
        if p.len() < 3 {
            continue;
        }
        let (zone_light, from) = ctx.zone_light(ctx_s, zi, None);
        let level = light.level(zone_light, part);
        let cm = ctx.colormap(ctx_s, from);
        let fog = if sky { None } else { ctx.lighting.fog(level, &cm) };
        let mat = b.material(&surface, fog, alpha < 0.999);
        let m = mapping(&mat);
        let brightness = ctx.lighting.brightness(level, light.rel);

        // 2. Cut along the glow heights, so the glow fades linearly.
        let mut pieces = vec![p];
        for (w, g) in glows.iter().enumerate() {
            let Some((_, h)) = g else { continue };
            let e = if w == FLOOR {
                Edge { a: floor.a + h, z: floor.z + h }
            } else {
                Edge { a: ceiling.a - h, z: ceiling.z - h }
            };
            pieces = pieces
                .into_iter()
                .flat_map(|q| [clip(&q, |r| r[1] - e.at(r[0])), clip(&q, |r| e.at(r[0]) - r[1])])
                .filter(|q| q.len() >= 3)
                .collect();
        }

        // 3. Each piece's corners.
        for piece in pieces {
            let verts: Vec<(Vec3, Vec2, Vec4)> = piece
                .iter()
                .map(|&[s, h]| {
                    let (x, y) = (a[0] + (z[0] - a[0]) * s, a[1] + (z[1] - a[1]) * s);
                    let uv = Vec2::new((m.u0 + m.du * s * len) as f32, ((m.top - h) / m.height) as f32);
                    if sky {
                        return (to_world(x, y, h), uv, Vec4::ONE);
                    }
                    let mut add = [0.0f32; 3];
                    for (w, g) in glows.iter().enumerate() {
                        let Some((c, gh)) = g else { continue };
                        let d = if w == FLOOR { h - floor.at(s) } else { ceiling.at(s) - h };
                        let k = (1.0 - d / gh.max(1e-6)).clamp(0.0, 1.0) as f32;
                        for i in 0..3 {
                            add[i] += c[i] * k;
                        }
                    }
                    let span = (ceiling.at(s) - floor.at(s)).max(1e-6);
                    let t = ((ceiling.at(s) - h) / span).clamp(0.0, 1.0) as f32;
                    let tint = [0, 1, 2].map(|i| tops[i] + (bottoms[i] - tops[i]) * t);
                    let color = ctx.lighting.color(brightness, &cm, add, tint).extend(alpha);
                    (to_world(x, y, h), uv, color)
                })
                .collect();
            // Wall polygons wind anticlockwise in (s, z), which faces the
            // front side.
            let tris: Vec<[u32; 3]> = (1..verts.len() as u32 - 1).map(|k| [0, k, k + 1]).collect();
            b.add(group, mat.index, &verts, normal, &tris);
        }
    }
}

/// One side of a line, seen from sector `s` on the right of `a` to `z`,
/// with `other` the sector across a two-sided line.
#[allow(clippy::too_many_arguments)]
fn wall_side(
    b: &mut Builder,
    ctx: &Ctx,
    line: usize,
    side: usize,
    a: [f64; 2],
    z: [f64; 2],
    s: usize,
    other: Option<usize>,
) {
    let map = ctx.map;
    let l = &map.lines[line];
    let sd = &map.sides[side];
    let sec = &map.sectors[s];
    let foggy = Lighting::foggy(&ctx.colormaps[s]);
    let light = SideLight {
        s,
        rel: ctx.lighting.wall_contrast(sd, a, z, foggy),
        part: [0, 1, 2].map(|p| (sd.part_light[p], sd.part_light_absolute[p])),
        side_light: (sd.light, sd.light_absolute),
        foggy_skip: foggy && !sd.light_fog && map.format.zdoom(),
    };
    let hidden = ctx.hidden[s] && other.is_none_or(|o| ctx.hidden[o]);
    let group = if hidden { Group::Unseen } else { Group::Walls };
    let pegged_top = l.flags & ML_DONTPEGTOP != 0;
    let pegged_bottom = l.flags & ML_DONTPEGBOTTOM != 0;
    let fc = Edge { a: ctx.z(CEILING, s, a), z: ctx.z(CEILING, s, z) };
    let ff = Edge { a: ctx.z(FLOOR, s, a), z: ctx.z(FLOOR, s, z) };
    let texture = |name: &str, masked: bool| -> Option<Surface> {
        if name.is_empty() || name == "-" {
            None
        } else if masked {
            Some(Surface::Masked(name.to_string()))
        } else {
            Some(Surface::Wall(name.to_string()))
        }
    };
    // Texture coordinates for part `part` with its texture's top at
    // `textop(height in units)` before the side's offsets.
    let mapping = |part: usize, textop: &dyn Fn(f64) -> f64, mat: &Mat| {
        let ti = TexInfo::new(mat, sd.part_scales[part]);
        let (xo, yo) = (sd.x_offset + sd.part_offsets[part].0, sd.y_offset + sd.part_offsets[part].1);
        Mapping {
            u0: ti.x_offset(xo) / ti.width,
            du: 1.0 / ti.width,
            top: textop(ti.height) + ti.y_offset(yo),
            height: ti.height,
        }
    };
    // Above a sky ceiling the sky closes the view at every wall, so what
    // lies beyond the line higher up (other sectors' ceilings, the tops of
    // far walls) stays hidden, as GZDoom's `SkyNormal` and `SkyTop` draw
    // it: from the ceiling up, or from a closed sky sector's ceiling.
    let sky_wall = |b: &mut Builder, from: Edge| {
        if hidden {
            return;
        }
        let poly = between(from, Edge::flat(SKY_WALL_TOP.max(from.a.max(from.z) + 1.0)));
        let m = |_: &Mat| Mapping { u0: 0.0, du: 0.0, top: 0.0, height: 1.0 };
        emit(b, ctx, poly, a, z, Surface::Sky, UPPER, &m, &light, 1.0, Group::Sky);
    };
    let front_sky = ctx.sky_ceiling(s);
    let Some(o) = other else {
        // One-sided: the middle texture from floor to ceiling.
        if let Some(t) = texture(&sd.middle, false) {
            let (floor, ceiling) = (sec.floor, sec.ceiling);
            let textop = move |h: f64| if pegged_bottom { floor + h } else { ceiling };
            let m = |mat: &Mat| mapping(MIDDLE, &textop, mat);
            emit(b, ctx, between(ff, fc), a, z, t, MIDDLE, &m, &light, 1.0, group);
        }
        if front_sky {
            sky_wall(b, fc);
        }
        return;
    };
    let oth = &map.sectors[o];
    let bc = Edge { a: ctx.z(CEILING, o, a), z: ctx.z(CEILING, o, z) };
    let bf = Edge { a: ctx.z(FLOOR, o, a), z: ctx.z(FLOOR, o, z) };
    if front_sky {
        if !ctx.sky_ceiling(o) {
            sky_wall(b, fc);
        } else if bc.a <= bf.a && bc.z <= bf.z {
            // A closed sky sector (a door shut under the sky).
            sky_wall(b, bc);
        }
    }
    let both_sky = is_sky(&sec.ceiling_flat) && is_sky(&oth.ceiling_flat);
    let sloped = |w: usize| ctx.planes[w][s].is_slope() || ctx.planes[w][o].is_slope();

    // 1. The upper wall, from the back ceiling (or the front floor, when
    //    that is higher at both ends) up to the front ceiling.
    let low = if bc.a < ff.a && bc.z < ff.z { ff } else { bc };
    if fc.a > low.a || fc.z > low.z {
        let poly = between(low, fc);
        if both_sky {
            // Doom leaves out the upper wall between two sky ceilings, so
            // the sky shows through; here it is a piece of sky.
            let m = |_: &Mat| Mapping { u0: 0.0, du: 0.0, top: 0.0, height: 1.0 };
            emit(b, ctx, poly, a, z, Surface::Sky, UPPER, &m, &light, 1.0, Group::Sky);
        } else {
            let t = texture(&sd.upper, false).or_else(|| {
                // GZDoom fills a sloped gap with the ceiling's flat rather
                // than leave a hole.
                (sloped(CEILING) && !is_sky(&sec.ceiling_flat) && !is_sky(&oth.ceiling_flat))
                    .then(|| Surface::Flat(sec.ceiling_flat.clone()))
            });
            if let Some(t) = t {
                let (ceiling, back) = (sec.ceiling, oth.ceiling);
                let textop = move |h: f64| if pegged_top { ceiling } else { back + h };
                let m = |mat: &Mat| mapping(UPPER, &textop, mat);
                emit(b, ctx, poly, a, z, t, UPPER, &m, &light, 1.0, group);
            }
        }
    }

    // 2. The lower wall, from the front floor up to the back floor (or the
    //    front ceiling, when that is lower at both ends).
    let high = if bf.a > fc.a && bf.z > fc.z { fc } else { bf };
    if high.a > ff.a || high.z > ff.z {
        let t = texture(&sd.lower, false).or_else(|| {
            (sloped(FLOOR) && !is_sky(&sec.floor_flat) && !is_sky(&oth.floor_flat))
                .then(|| Surface::Flat(sec.floor_flat.clone()))
        });
        if let Some(t) = t {
            let ceiling = if both_sky { oth.ceiling } else { sec.ceiling };
            let back = oth.floor;
            let textop = move |_: f64| if pegged_bottom { ceiling } else { back };
            let m = |mat: &Mat| mapping(LOWER, &textop, mat);
            emit(b, ctx, between(ff, high), a, z, t, LOWER, &m, &light, 1.0, group);
        }
    }

    // 3. The middle texture, drawn once between the texture's top and
    //    bottom (unless it wraps) within the opening.
    if let Some(t) = texture(&sd.middle, true) {
        let wrap = l.flags & ML_WRAPMIDTEX != 0;
        let (lo, hi) = (sec.floor.max(oth.floor), sec.ceiling.min(oth.ceiling));
        let textop = move |h: f64| if pegged_bottom { lo + h } else { hi };
        let map_fn = |mat: &Mat| mapping(MIDDLE, &textop, mat);
        // The texture's extent needs its size, so take the material first.
        let alpha = l.alpha.clamp(0.0, 1.0);
        if alpha > 0.0 {
            let mat = b.material(&t, None, alpha < 0.999);
            let m = map_fn(&mat);
            let has_upper = texture(&sd.upper, false).is_some();
            let has_lower = texture(&sd.lower, false).is_some();
            let top = if s == o {
                Edge::flat(m.top)
            } else if !has_upper {
                if both_sky && !wrap { Edge::flat(m.top) } else { Edge { a: fc.a.max(bc.a), z: fc.z.max(bc.z) } }
            } else {
                Edge { a: fc.a.min(bc.a), z: fc.z.min(bc.z) }
            };
            let bottom = if s == o {
                Edge::flat(m.top - m.height)
            } else if !has_lower {
                Edge { a: ff.a.min(bf.a), z: ff.z.min(bf.z) }
            } else {
                Edge { a: ff.a.max(bf.a), z: ff.z.max(bf.z) }
            };
            let mut poly = between(bottom, top);
            if !wrap {
                let (t0, t1) = (m.top, m.top - m.height);
                poly = clip(&poly, |q| t0 - q[1]);
                poly = clip(&poly, |q| q[1] - t1);
            }
            emit(b, ctx, poly, a, z, t, MIDDLE, &map_fn, &light, alpha, group);
        }
    }

    // 4. 3D floors' sides, seen from this side.
    if !ctx.xf.floors[o].is_empty() || !ctx.xf.floors[s].is_empty() {
        ffloor_blocks(b, ctx, line, sd, a, z, s, o, [fc, ff, bc, bf], &light, group);
    }
}

/// The sides of the 3D floors in the sector across (`o`), between the
/// two sectors' opening, and of this sector's own 3D floors seen from
/// inside: GZDoom's `DoFFloorBlocks`.
#[allow(clippy::too_many_arguments)]
fn ffloor_blocks(
    b: &mut Builder,
    ctx: &Ctx,
    line: usize,
    sd: &SideDef,
    a: [f64; 2],
    z: [f64; 2],
    s: usize,
    o: usize,
    [fc, ff, bc, bf]: [Edge; 4],
    light: &SideLight,
    group: Group,
) {
    let mut top = if fc.a < bc.a && fc.z < bc.z { fc } else { bc };
    let bottom = if ff.a > bf.a && ff.z > bf.z { ff } else { bf };
    let at = |p: &Plane| Edge { a: p.z_at(a[0], a[1]), z: p.z_at(z[0], z[1]) };
    let above = |x: Edge, y: Edge| x.a > y.a && x.z > y.z;
    let mut rendered = false;
    let _ = line;
    for x in &ctx.xf.floors[o] {
        if x.flags & FF_EXISTS == 0 || x.flags & FF_RENDERSIDES == 0 || x.flags & xfloors::FF_INVERTSIDES != 0 {
            continue;
        }
        let mut ft = at(&x.top.plane);
        let fb = at(&x.bottom.plane);
        if above(fb, top) && !rendered {
            continue;
        }
        if above(ft, top) {
            ft = top;
        }
        if above(top, ft) {
            inverse_floors(b, ctx, sd, a, z, s, top, ft, light, group);
        }
        if x.flags & (xfloors::FF_SWIMMABLE | xfloors::FF_TRANSLUCENT) != 0 {
            clip_floors(b, ctx, sd, a, z, s, x, ft, fb, light, group);
        } else {
            block(b, ctx, sd, a, z, x, ft, fb, light, group);
        }
        top = fb;
        rendered = true;
        if top.a <= bottom.a && top.z <= bottom.z {
            return;
        }
    }
    if !ctx.xf.floors[s].is_empty() && (top.a > bottom.a || top.z > bottom.z) {
        inverse_floors(b, ctx, sd, a, z, s, top, bottom, light, group);
    }
}

/// This sector's own 3D floors that are drawn from inside (water), between
/// `top` and `bottom`.
#[allow(clippy::too_many_arguments)]
fn inverse_floors(
    b: &mut Builder,
    ctx: &Ctx,
    sd: &SideDef,
    a: [f64; 2],
    z: [f64; 2],
    s: usize,
    mut top: Edge,
    bottom: Edge,
    light: &SideLight,
    group: Group,
) {
    let at = |p: &Plane| Edge { a: p.z_at(a[0], a[1]), z: p.z_at(z[0], z[1]) };
    for x in &ctx.xf.floors[s] {
        if x.flags & FF_EXISTS == 0 || x.flags & FF_RENDERSIDES == 0 {
            continue;
        }
        if x.flags & (xfloors::FF_INVERTSIDES | xfloors::FF_ALLSIDES) == 0 {
            continue;
        }
        let (mut ft, mut fb) = (at(&x.top.plane), at(&x.bottom.plane));
        if fb.a > top.a && fb.z > top.z {
            continue;
        }
        if ft.a > top.a && ft.z > top.z {
            ft = top;
        }
        if fb.a < bottom.a && fb.z < bottom.z {
            fb = bottom;
        }
        if ft.a < fb.a || ft.z < fb.z {
            continue;
        }
        block(b, ctx, sd, a, z, x, ft, fb, light, group);
        top = fb;
        if top.a <= bottom.a && top.z <= bottom.z {
            return;
        }
    }
}

/// A liquid or translucent 3D floor's side, less the parts where this
/// sector has the same kind of 3D floor (water meeting water draws no
/// wall).
#[allow(clippy::too_many_arguments)]
fn clip_floors(
    b: &mut Builder,
    ctx: &Ctx,
    sd: &SideDef,
    a: [f64; 2],
    z: [f64; 2],
    s: usize,
    x: &XFloor,
    mut top: Edge,
    bottom: Edge,
    light: &SideLight,
    group: Group,
) {
    let at = |p: &Plane| Edge { a: p.z_at(a[0], a[1]), z: p.z_at(z[0], z[1]) };
    let kind = x.flags & (xfloors::FF_SWIMMABLE | xfloors::FF_TRANSLUCENT);
    for r in &ctx.xf.floors[s] {
        if r.flags & FF_EXISTS == 0 || r.flags & FF_RENDERSIDES == 0 {
            continue;
        }
        if r.flags & (xfloors::FF_SWIMMABLE | xfloors::FF_TRANSLUCENT) != kind {
            continue;
        }
        let ft = at(&r.top.plane);
        if ft.a < bottom.a && ft.z < bottom.z {
            break;
        }
        let fb = at(&r.bottom.plane);
        if fb.a > top.a && fb.z > top.z {
            continue;
        }
        if ft.a >= top.a && ft.z >= top.z {
            if fb.a <= bottom.a && fb.z <= bottom.z {
                return;
            } else if fb.a > bottom.a && fb.z > bottom.z {
                top = fb;
            }
        } else if ft.a <= top.a && ft.z <= top.z {
            block(b, ctx, sd, a, z, x, top, ft, light, group);
            if fb.a <= bottom.a && fb.z <= bottom.z {
                return;
            }
            top = fb;
        }
    }
    block(b, ctx, sd, a, z, x, top, bottom, light, group);
}

/// One piece of a 3D floor's side: the defining line's middle texture
/// (or this side's upper or lower texture, as the 3D floor's flags say),
/// aligned to the control sector's ceiling (`BuildFFBlock`).
#[allow(clippy::too_many_arguments)]
fn block(
    b: &mut Builder,
    ctx: &Ctx,
    sd: &SideDef,
    a: [f64; 2],
    z: [f64; 2],
    x: &XFloor,
    top: Edge,
    bottom: Edge,
    light: &SideLight,
    group: Group,
) {
    let map = ctx.map;
    let master = map.lines.get(x.line).and_then(|l| l.front).and_then(|i| map.sides.get(i));
    let Some(master) = master else { return };
    let own = x.flags & (xfloors::FF_UPPERTEXTURE | FF_LOWERTEXTURE) != 0;
    let (name, part, tex_side) = if x.flags & xfloors::FF_UPPERTEXTURE != 0 {
        (&sd.upper, UPPER, sd)
    } else if x.flags & FF_LOWERTEXTURE != 0 {
        (&sd.lower, LOWER, sd)
    } else {
        (&master.middle, MIDDLE, master)
    };
    if name.is_empty() || name == "-" {
        return;
    }
    let alpha = if x.translucent() { x.alpha as f32 / 255.0 } else { 1.0 };
    let model = &map.sectors[x.top.model];
    let model_top = if x.top.which == CEILING { model.ceiling } else { model.floor };
    let mapping = |mat: &Mat| {
        let ti = TexInfo::new(mat, tex_side.part_scales[part]);
        let (mx, my) = if own {
            (0.0, 0.0)
        } else {
            (master.x_offset + master.part_offsets[MIDDLE].0, master.y_offset + master.part_offsets[MIDDLE].1)
        };
        let (sx, sy) = (sd.x_offset + sd.part_offsets[MIDDLE].0, sd.y_offset + sd.part_offsets[MIDDLE].1);
        Mapping {
            u0: (ti.x_offset(mx) + ti.x_offset(sx)) / ti.width,
            du: 1.0 / ti.width,
            top: ti.y_offset(my) + ti.y_offset(sy) + model_top,
            height: ti.height,
        }
    };
    let surface = Surface::Wall(name.clone());
    emit(b, ctx, between(bottom, top), a, z, surface, MIDDLE, &mapping, light, alpha, group);
}

//endregion

/*┌─────────────────╮
| Sprites         |
└─────────────────╯*/
//region Sprites

/// The things as sprites turning to face the camera, standing on the floor
/// or hanging from the ceiling, in their sector's light.
fn sprites(b: &mut Builder, ctx: &Ctx, sector_at: &dyn Fn(f64, f64) -> Option<usize>) {
    let mut sprites: HashMap<String, Primitive> = HashMap::new();
    for t in &ctx.map.things {
        if t.flags & MTF_MULTIPLAYER != 0 {
            continue;
        }
        let Some((name, frame, hangs)) = things::sprite(t.kind) else { continue };
        let Some((picture, lump, left, top)) = b.gfx.sprite(name, frame) else { continue };
        let sector = sector_at(t.x, t.y);
        let p = match sprites.get_mut(&lump) {
            Some(p) => p,
            None => {
                let tex = b.push_texture(&picture, &lump);
                let m = Material {
                    name: lump.clone(),
                    base_color_texture: Some(texref(tex, b.smooth)),
                    alpha_mode: AlphaMode::Mask(0.5),
                    double_sided: true,
                    effect: Effect::Billboard,
                    fog: Some(scene_fog(None)),
                    ..base_material()
                };
                let material = b.push_material(m);
                sprites.entry(lump.clone()).or_insert(Primitive { material: Some(material), ..Default::default() })
            }
        };
        let color = match sector {
            Some(_) if things::full_bright(name) => Vec3::ONE,
            Some(s) => {
                let c = ctx.centers[s];
                let zone = xfloors::plane_zone(&ctx.xf.zones[s], c, ctx.z(FLOOR, s, [t.x, t.y]) + 1.0, false);
                let (level, from) = ctx.zone_light(s, zone, None);
                ctx.lighting.color(ctx.lighting.brightness(level, 0), &ctx.colormap(s, from), [0.0; 3], [1.0; 3])
            }
            None => Vec3::ONE,
        };
        let zpos = match sector {
            Some(s) if hangs => ctx.z(CEILING, s, [t.x, t.y]) - top as f64,
            Some(s) => ctx.z(FLOOR, s, [t.x, t.y]),
            None => 0.0,
        };
        let anchor = to_world(t.x, t.y, zpos);
        let (l, r) = ((-left) as f32 * UNIT, (picture.width as i32 - left) as f32 * UNIT);
        let (bottom, up) = ((top - picture.height as i32) as f32, top as f32);
        let (bottom, up) = (bottom * UNIT * STRETCH, up * UNIT * STRETCH);
        let base = p.positions.len() as u32;
        for (corner, uv) in [
            (Vec2::new(l, bottom), Vec2::new(0.0, 1.0)),
            (Vec2::new(r, bottom), Vec2::new(1.0, 1.0)),
            (Vec2::new(r, up), Vec2::new(1.0, 0.0)),
            (Vec2::new(l, up), Vec2::new(0.0, 0.0)),
        ] {
            p.positions.push(anchor);
            p.normals.push(Vec3::Y);
            p.uv0.push(uv);
            p.uv1.push(corner);
            p.colors.push(color.extend(1.0));
        }
        p.indices.extend([0, 1, 2, 0, 2, 3].map(|i| base + i));
    }
    if !sprites.is_empty() {
        let mut primitives: Vec<Primitive> = sprites.into_values().collect();
        primitives.sort_by_key(|p| p.material);
        b.scene.add_mesh_node(Mesh { name: "Things".into(), primitives, target_names: Vec::new() });
    }
}

//endregion
