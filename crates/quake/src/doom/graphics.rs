//! The pictures a map's surfaces wear, found the way GZDoom's texture
//! manager finds them across a stack of resource files (an IWAD, then
//! PWADs and PK3s whose definitions replace earlier ones):
//!
//! - TEXTURE1 and TEXTURE2 compose wall textures from patches named by
//!   index into PNAMES.  Each file's lists are read with that file's
//!   PNAMES (as ZDoom does), so a PWAD that adds textures can't scramble
//!   the IWAD's.
//! - Flats sit between F_START and F_END, or in a PK3's `flats/`.
//! - Pictures in `textures/` (or between TX_START and TX_END) are textures
//!   as they are, usable on walls and floors alike.
//! - The TEXTURES lump composes textures in text, with scales (a 128-texel
//!   texture at XScale 2 covers 64 map units), flipped, rotated, tinted and
//!   translucent patches, and `define`s high-resolution stand-ins.
//! - ANIMDEFS: an animation shows its first frame.
//! - Colours come from PLAYPAL, else Freedoom's palette.
//!
//! Pictures may be Doom's column-based patches, raw flats, PNG, JPEG, TGA
//! and the other image formats the textures crate reads.  A map's pictures
//! are read and decoded together, in parallel, before it is built.

use super::animdefs::Animdefs;
use super::archive::Namespace;
use super::patch;
use super::resources::Resources;
use super::textures::{self, Style, Use};
use crate::decode_image;
use crate::pixels::{self, DOOM_PALETTE};
use crate::read::{Bytes, cstr};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// The largest texture side accepted, far beyond any real WAD's.
const MAX_SIDE: u32 = 16384;

/// The most texels decoded pictures may hold together (256 MB), and the
/// most texel copies composing textures may cost.  Real maps use a few
/// million of each; a hostile WAD naming thousands of huge textures, or
/// stacking thousands of patches into one, stops here.
const MAX_TEXELS: usize = 64 << 20;
const MAX_COPIES: usize = 128 << 20;

/// A decoded picture: straight RGBA, alpha 0 where nothing was drawn.
#[derive(Clone)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Some texels are transparent, as in fences and grates.
    pub holes: bool,
}

/// A texture ready to wear: its picture and how big it is in the world.
pub struct Tex {
    pub picture: Arc<Picture>,
    /// Texels per map unit, across and down.
    pub scale: (f64, f64),
    /// Texture offsets count in map units rather than texels.
    pub world_panning: bool,
}

impl Tex {
    /// The size in map units.
    pub fn size(&self) -> (f64, f64) {
        (self.picture.width as f64 / self.scale.0, self.picture.height as f64 / self.scale.1)
    }
}

/// Where a name may be used, as GZDoom's texture types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Sprite,
    Patch,
    /// TEXTUREx and TEXTURES `WallTexture`.
    Wall,
    Flat,
    /// `textures/`, TX_START and TEXTURES `Texture`: walls and flats both.
    Any,
    Graphic,
}

struct Entry {
    name: String,
    kind: Kind,
    source: Source,
}

enum Source {
    Lump(u32),
    Composite(Box<Composite>),
    /// A `define`: a picture drawn at a given size in map units.
    Define {
        lump: u32,
        width: u32,
        height: u32,
    },
}

pub(super) struct Composite {
    pub(super) width: u32,
    pub(super) height: u32,
    scale: (f64, f64),
    world_panning: bool,
    null: bool,
    pub(super) patches: Vec<textures::PatchDef>,
    /// TEXTURE1's patches name lumps only; TEXTURES' may name textures.
    lumps_only: bool,
    /// The entry's position, so a patch named like its texture finds the
    /// picture defined before it, not itself.
    order: usize,
}

pub struct Graphics<'a> {
    res: &'a Resources<'a>,
    pub palette: Vec<u8>,
    entries: Vec<Entry>,
    by_name: HashMap<String, Vec<usize>>,
    /// High-resolution replacements (`hires/`), by name.
    hires: HashMap<String, u32>,
    animdefs: Animdefs,
    /// Decoded lumps, by lump id.
    decoded: HashMap<u32, Option<Arc<Picture>>>,
    /// Built textures, by entry.
    built: HashMap<usize, Option<Arc<Tex>>>,
    texels_left: usize,
    copies_left: usize,
    /// Pictures left out because the budgets ran out.
    pub over_budget: usize,
}

impl<'a> Graphics<'a> {
    /// Indexes the pictures of a resource stack, base first: later files
    /// win.
    pub fn new(res: &'a Resources<'a>) -> Graphics<'a> {
        let mut g = Graphics {
            res,
            palette: res.palette().unwrap_or_else(|| DOOM_PALETTE.to_vec()),
            entries: Vec::new(),
            by_name: HashMap::new(),
            hires: HashMap::new(),
            animdefs: Animdefs::default(),
            decoded: HashMap::new(),
            built: HashMap::new(),
            texels_left: MAX_TEXELS,
            copies_left: MAX_COPIES,
            over_budget: 0,
        };
        let mut pnames: Vec<String> = Vec::new();
        for fi in 0..res.files.len() as u32 {
            let in_file = |ns: Namespace| res.in_namespace(ns).filter(move |(_, l)| l.file == fi);
            // 1. Sprites and patches.
            for (id, l) in in_file(Namespace::Sprites) {
                g.push(l.name.clone(), Kind::Sprite, Source::Lump(id));
            }
            for (id, l) in in_file(Namespace::Patches) {
                g.push(l.name.clone(), Kind::Patch, Source::Lump(id));
            }
            // 2. TEXTURE1 and TEXTURE2 with this file's PNAMES.
            let global = |name: &str| {
                res.all(Namespace::Global, name).iter().rev().find(|&&id| res.file_of(id) == Some(fi)).copied()
            };
            if let Some(d) = global("PNAMES").and_then(|id| res.data(id)) {
                pnames = read_pnames(&d);
            }
            for t in ["TEXTURE1", "TEXTURE2"] {
                if let Some(d) = global(t).and_then(|id| res.data(id)) {
                    for (name, c) in read_textures(&d, &pnames) {
                        g.push_composite(name, Kind::Wall, c);
                    }
                }
            }
            // 3. Flats, then pictures used as they are.
            for (id, l) in in_file(Namespace::Flats) {
                g.push(l.name.clone(), Kind::Flat, Source::Lump(id));
            }
            for (id, l) in in_file(Namespace::Textures) {
                g.push(l.name.clone(), Kind::Any, Source::Lump(id));
            }
            for (id, l) in in_file(Namespace::Graphics) {
                g.push(l.name.clone(), Kind::Graphic, Source::Lump(id));
            }
            for (id, l) in in_file(Namespace::Hires) {
                g.hires.insert(l.name.clone(), id);
            }
            // 4. TEXTURES lumps.
            let mut parsed = textures::Parsed::default();
            for &id in res.all(Namespace::Global, "TEXTURES") {
                if res.file_of(id) != Some(fi) {
                    continue;
                }
                if let Some(d) = res.data(id) {
                    textures::parse(&d, &mut |n| res.include(n), &mut parsed);
                }
            }
            for t in parsed.textures {
                let kind = match t.use_as {
                    Use::Any => Kind::Any,
                    Use::Wall => Kind::Wall,
                    Use::Flat => Kind::Flat,
                    Use::Sprite => Kind::Sprite,
                    Use::Graphic => Kind::Graphic,
                };
                let c = Composite {
                    width: t.width,
                    height: t.height,
                    scale: (t.x_scale, t.y_scale),
                    world_panning: t.world_panning,
                    null: t.null,
                    patches: t.patches,
                    lumps_only: false,
                    order: 0,
                };
                g.push_composite(t.name, kind, c);
            }
            for d in parsed.defines {
                let lump = res.by_path(&d.path).or_else(|| res.last(Namespace::Patches, &d.name));
                if let Some(lump) = lump.or_else(|| res.last(Namespace::Graphics, &d.name)) {
                    g.push(d.name, Kind::Any, Source::Define { lump, width: d.width, height: d.height });
                }
            }
        }
        // 5. Animations, from every file in order.
        for text in res.texts("ANIMDEFS") {
            g.animdefs.parse(&text, &mut |n| res.include(n));
        }
        g
    }

    fn push(&mut self, name: String, kind: Kind, source: Source) {
        let i = self.entries.len();
        self.by_name.entry(name.clone()).or_default().push(i);
        self.entries.push(Entry { name, kind, source });
    }

    fn push_composite(&mut self, name: String, kind: Kind, mut c: Composite) {
        c.order = self.entries.len();
        self.push(name, kind, Source::Composite(Box::new(c)));
    }

    /// The entry a name stands for, used as `want`: the latest of a
    /// matching kind, else the latest of any kind, as GZDoom's
    /// `CheckForTexture` with `TryAny`.  Names with a slash are archive
    /// paths.
    fn find(&self, name: &str, want: &[Kind], before: usize) -> Option<usize> {
        let list = self.by_name.get(name)?;
        let ok = |&&i: &&usize| i < before;
        list.iter()
            .rev()
            .filter(ok)
            .find(|&&i| want.contains(&self.entries[i].kind))
            .or_else(|| list.iter().rev().find(ok))
            .copied()
    }

    /// The lump a path names, for long texture names and TEXTURES patches
    /// given as files.
    fn path_lump(&self, name: &str) -> Option<u32> {
        if name.contains('/') || name.contains('.') { self.res.by_path(name) } else { None }
    }

    /// Whether a texture name is a camera or canvas texture (ANIMDEFS),
    /// which has no picture of its own.
    pub fn is_camera(&self, name: &str) -> bool {
        self.animdefs.cameras.contains(&name.to_ascii_uppercase())
    }

    /// A wall texture, cached.
    pub fn wall(&mut self, name: &str) -> Option<Arc<Tex>> {
        self.texture(name, false)
    }

    /// A flat, cached.  Wall textures stand in for missing flats, as
    /// ZDoom allows.
    pub fn flat(&mut self, name: &str) -> Option<Arc<Tex>> {
        self.texture(name, true)
    }

    fn resolve(&self, name: &str, flat: bool) -> Option<Resolved> {
        let upper = name.to_ascii_uppercase();
        if upper.is_empty() || upper == "-" {
            return None;
        }
        let upper = self.animdefs.first_frame.get(&(flat, upper.clone())).cloned().unwrap_or(upper);
        let want: &[Kind] = if flat { &[Kind::Flat, Kind::Any] } else { &[Kind::Wall, Kind::Any] };
        if let Some(i) = self.find(&upper, want, usize::MAX) {
            return Some(Resolved::Entry(i));
        }
        self.path_lump(name).map(Resolved::Lump)
    }

    fn texture(&mut self, name: &str, flat: bool) -> Option<Arc<Tex>> {
        match self.resolve(name, flat)? {
            Resolved::Entry(i) => self.build(i),
            Resolved::Lump(id) => {
                let picture = self.lump_picture(id)?;
                Some(Arc::new(Tex { picture, scale: (1.0, 1.0), world_panning: false }))
            }
        }
    }

    fn build(&mut self, i: usize) -> Option<Arc<Tex>> {
        if let Some(t) = self.built.get(&i) {
            return t.clone();
        }
        // Guard against definitions that use themselves.
        self.built.insert(i, None);
        let made = self.build_uncached(i).map(Arc::new);
        self.built.insert(i, made.clone());
        made
    }

    fn build_uncached(&mut self, i: usize) -> Option<Tex> {
        let name = self.entries[i].name.clone();
        let mut tex = match &self.entries[i].source {
            Source::Lump(id) => {
                let id = *id;
                Tex { picture: self.lump_picture(id)?, scale: (1.0, 1.0), world_panning: false }
            }
            Source::Define { lump, width, height } => {
                let (lump, w, h) = (*lump, *width, *height);
                let picture = self.lump_picture(lump)?;
                let scale = (picture.width as f64 / w.max(1) as f64, picture.height as f64 / h.max(1) as f64);
                Tex { picture, scale, world_panning: true }
            }
            Source::Composite(_) => {
                let Source::Composite(c) = std::mem::replace(&mut self.entries[i].source, Source::Lump(u32::MAX))
                else {
                    unreachable!()
                };
                let made = if c.null { None } else { self.compose(&c) };
                let scale = c.scale;
                let world_panning = c.world_panning;
                self.entries[i].source = Source::Composite(c);
                Tex { picture: made?, scale, world_panning }
            }
        };
        // 2. A high-resolution replacement keeps the size in the world.
        if let Some(&h) = self.hires.get(&name)
            && let Some(p) = self.lump_picture(h)
        {
            let (w, ht) = tex.size();
            tex = Tex {
                scale: (p.width as f64 / w.max(1e-3), p.height as f64 / ht.max(1e-3)),
                picture: p,
                world_panning: true,
            };
        }
        Some(tex)
    }

    /// Takes `texels` from the memory budget, or counts a picture left
    /// out.
    fn reserve(&mut self, texels: usize) -> bool {
        if texels > self.texels_left {
            self.over_budget += 1;
            return false;
        }
        self.texels_left -= texels;
        true
    }

    /// The picture a patch name stands for.
    fn patch_picture(&mut self, c: &Composite, name: &str) -> Option<Arc<Picture>> {
        let upper = name.to_ascii_uppercase();
        if let Some(id) = self.path_lump(name) {
            return self.lump_picture(id);
        }
        if c.lumps_only {
            let id = self.res.last(Namespace::Patches, &upper).or_else(|| self.res.last(Namespace::Global, &upper));
            return id.and_then(|id| self.lump_picture(id));
        }
        // A texture, patch, sprite or graphic defined before this one.
        if let Some(i) = self.find(&upper, &[Kind::Patch], c.order) {
            return self.build(i).map(|t| t.picture.clone());
        }
        let id = self.res.last(Namespace::Patches, &upper).or_else(|| self.res.last(Namespace::Global, &upper))?;
        self.lump_picture(id)
    }

    fn compose(&mut self, c: &Composite) -> Option<Arc<Picture>> {
        let (w, h) = (c.width, c.height);
        if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
            return None;
        }
        // 1. Most textures of high-resolution mods are one picture given a
        //    scale: that picture serves as it is.
        if let [p] = c.patches.as_slice()
            && plain(p)
            && (p.x, p.y, p.rotate, p.flip_x, p.flip_y) == (0, 0, 0, false, false)
            && let Some(src) = self.patch_picture(c, &p.name)
            && (src.width, src.height) == (w, h)
            && !src.rgba.chunks_exact(4).any(|t| t[3] != 0 && t[3] != 255)
        {
            return Some(src);
        }
        if !self.reserve(w as usize * h as usize) {
            return None;
        }
        let mut rgba = vec![0u8; w as usize * h as usize * 4];
        for p in &c.patches {
            let Some(src) = self.patch_picture(c, &p.name) else { continue };
            let src = transform(&src, p);
            let (ox, oy) = (p.x, p.y);
            // Only the part of the patch that lands on the texture.
            let (x0, x1) = ((-ox).max(0), (w as i32 - ox).min(src.width as i32));
            let (y0, y1) = ((-oy).max(0), (h as i32 - oy).min(src.height as i32));
            if x0 >= x1 || y0 >= y1 {
                continue;
            }
            let work = (x1 - x0) as usize * (y1 - y0) as usize;
            if work > self.copies_left {
                self.over_budget += 1;
                break;
            }
            self.copies_left -= work;
            let fast = plain(p);
            for y in y0..y1 {
                let s0 = (y as usize * src.width as usize + x0 as usize) * 4;
                let d0 = ((oy + y) as usize * w as usize + (ox + x0) as usize) * 4;
                let n = (x1 - x0) as usize * 4;
                let (from, to) = (&src.rgba[s0..s0 + n], &mut rgba[d0..d0 + n]);
                if fast {
                    for (t, f) in to.chunks_exact_mut(4).zip(from.chunks_exact(4)) {
                        if f[3] != 0 {
                            t.copy_from_slice(&[f[0], f[1], f[2], 255]);
                        }
                    }
                } else {
                    for (t, f) in to.chunks_exact_mut(4).zip(from.chunks_exact(4)) {
                        draw(t, [f[0], f[1], f[2], f[3]], p);
                    }
                }
            }
        }
        let holes = rgba.chunks_exact(4).any(|p| p[3] == 0);
        if holes {
            pixels::bleed(&mut rgba, w, h);
        }
        Some(Arc::new(Picture { width: w, height: h, rgba, holes }))
    }

    /// A lump decoded as a picture, cached.
    fn lump_picture(&mut self, id: u32) -> Option<Arc<Picture>> {
        if let Some(p) = self.decoded.get(&id) {
            return p.clone();
        }
        let data = self.res.data(id);
        let made = data.and_then(|d| self.decode_and_charge(id, &d));
        self.decoded.insert(id, made.clone());
        made
    }

    fn decode_and_charge(&mut self, id: u32, d: &[u8]) -> Option<Arc<Picture>> {
        let p = decode_lump(d, &self.lump_hint(id), &self.palette)?;
        self.reserve(p.width as usize * p.height as usize).then(|| Arc::new(p))
    }

    /// How a lump's bytes should be read: its namespace and extension.
    fn lump_hint(&self, id: u32) -> Hint {
        let l = &self.res.lumps[id as usize];
        Hint { flat: l.ns == Namespace::Flats, tga: l.path.ends_with(".tga") }
    }

    /// Reads and decodes, in parallel, every picture the named walls and
    /// flats need, so building the map later finds them decoded.
    pub fn prefetch(&mut self, walls: &[String], flats: &[String]) {
        // 1. Every lump the textures need, composites followed down to
        //    their patches.
        let mut lumps: Vec<u32> = Vec::new();
        let mut seen: HashSet<usize> = HashSet::new();
        let mut stack: Vec<Resolved> = Vec::new();
        for (names, flat) in [(walls, false), (flats, true)] {
            for n in names {
                stack.extend(self.resolve(n, flat));
            }
        }
        while let Some(r) = stack.pop() {
            let i = match r {
                Resolved::Lump(id) => {
                    lumps.push(id);
                    continue;
                }
                Resolved::Entry(i) => i,
            };
            if !seen.insert(i) {
                continue;
            }
            if let Some(&h) = self.hires.get(&self.entries[i].name) {
                lumps.push(h);
            }
            match &self.entries[i].source {
                Source::Lump(id) | Source::Define { lump: id, .. } => lumps.push(*id),
                Source::Composite(c) => {
                    for p in &c.patches {
                        let upper = p.name.to_ascii_uppercase();
                        if let Some(id) = self.path_lump(&p.name) {
                            lumps.push(id);
                        } else if !c.lumps_only
                            && let Some(j) = self.find(&upper, &[Kind::Patch], c.order)
                        {
                            stack.push(Resolved::Entry(j));
                        } else if let Some(id) = self
                            .res
                            .last(Namespace::Patches, &upper)
                            .or_else(|| self.res.last(Namespace::Global, &upper))
                        {
                            lumps.push(id);
                        }
                    }
                }
            }
        }
        lumps.sort_unstable();
        lumps.dedup();
        lumps.retain(|id| !self.decoded.contains_key(id));

        // 2. Read and decode in batches, charging the budget between them
        //    so a hostile file can't decode far past it.
        for batch in lumps.chunks(64) {
            if self.texels_left == 0 {
                break;
            }
            let hints: Vec<Hint> = batch.iter().map(|&id| self.lump_hint(id)).collect();
            let datas = self.res.data_many(batch);
            let palette = &self.palette;
            let pictures: Vec<Option<Picture>> = datas
                .par_iter()
                .zip(hints.par_iter())
                .map(|(d, hint)| d.as_ref().and_then(|d| decode_lump(d, hint, palette)))
                .collect();
            for (&id, p) in batch.iter().zip(pictures) {
                let p = p.filter(|p| self.reserve(p.width as usize * p.height as usize)).map(Arc::new);
                self.decoded.insert(id, p);
            }
        }
    }

    /// The sprite `name` (four letters) shows in `frame`, seen from the
    /// front: its picture, lump name, and offsets (how far its anchor, the
    /// thing's foot, lies right of its left edge and below its top).
    /// Frames drawn alike from every side end in 0, front views in 1; a
    /// sprite without either shows its first view.
    pub fn sprite(&mut self, name: &str, frame: u8) -> Option<(Arc<Picture>, String, i32, i32)> {
        let prefix = format!("{name}{}", frame as char);
        let res = self.res;
        let lump_of = |n: &str| res.last(Namespace::Sprites, n).or_else(|| res.last(Namespace::Global, n));
        let lump =
            [format!("{prefix}0"), format!("{prefix}1")].into_iter().find(|n| lump_of(n).is_some()).or_else(|| {
                let mut views: Vec<&str> = res
                    .in_namespace(Namespace::Sprites)
                    .map(|(_, l)| l.name.as_str())
                    .filter(|k| k.starts_with(&prefix))
                    .collect();
                views.sort();
                views.first().map(|s| s.to_string())
            })?;
        let id = lump_of(&lump)?;
        let picture = self.lump_picture(id)?;
        let data = self.res.data(id)?;
        let (left, top) = if data.len() >= 8 && !data.starts_with(b"\x89PNG") && !self.lump_hint(id).tga {
            (i16::from_le_bytes([data[4], data[5]]) as i32, i16::from_le_bytes([data[6], data[7]]) as i32)
        } else {
            png_offsets(&data).unwrap_or((picture.width as i32 / 2, picture.height as i32))
        };
        Some((picture, lump, left, top))
    }
}

#[derive(Clone, Copy)]
enum Resolved {
    Entry(usize),
    Lump(u32),
}

/// What a lump's location says about its format.
#[derive(Clone, Copy, Default)]
struct Hint {
    flat: bool,
    tga: bool,
}

/// A PNG's `grAb` offsets, which ZDoom ports read as a patch's.
fn png_offsets(d: &[u8]) -> Option<(i32, i32)> {
    let mut at = 8;
    while at + 12 <= d.len() {
        let len = u32::from_be_bytes(d[at..at + 4].try_into().ok()?) as usize;
        let kind = &d[at + 4..at + 8];
        if kind == b"grAb" && len >= 8 && at + 16 <= d.len() {
            let x = i32::from_be_bytes(d[at + 8..at + 12].try_into().ok()?);
            let y = i32::from_be_bytes(d[at + 12..at + 16].try_into().ok()?);
            return Some((x, y));
        }
        if kind == b"IDAT" || kind == b"IEND" {
            return None;
        }
        at = at.checked_add(12 + len)?;
    }
    None
}

/// A patch flipped and rotated as a TEXTURES patch asks.
fn transform<'p>(src: &'p Picture, p: &textures::PatchDef) -> std::borrow::Cow<'p, Picture> {
    if !p.flip_x && !p.flip_y && p.rotate == 0 {
        return std::borrow::Cow::Borrowed(src);
    }
    let (sw, sh) = (src.width as usize, src.height as usize);
    let (w, h) = if p.rotate % 2 == 1 { (sh, sw) } else { (sw, sh) };
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            // Undo the rotation (clockwise quarter turns), then the flips.
            let (mut sx, mut sy) = match p.rotate {
                1 => (y, sh - 1 - x),
                2 => (sw - 1 - x, sh - 1 - y),
                3 => (sw - 1 - y, x),
                _ => (x, y),
            };
            if p.flip_x {
                sx = sw - 1 - sx;
            }
            if p.flip_y {
                sy = sh - 1 - sy;
            }
            let s = (sy * sw + sx) * 4;
            let d = (y * w + x) * 4;
            rgba[d..d + 4].copy_from_slice(&src.rgba[s..s + 4]);
        }
    }
    std::borrow::Cow::Owned(Picture { width: w as u32, height: h as u32, rgba, holes: src.holes })
}

/// Whether a patch is copied as it is: no tint, translucency or style.
fn plain(p: &textures::PatchDef) -> bool {
    p.style == Style::Copy && p.alpha >= 1.0 && p.blend.is_none() && !p.desaturate
}

/// Draws one texel of a patch over the texture's, with its tint and
/// style.
fn draw(dst: &mut [u8], src: [u8; 4], p: &textures::PatchDef) {
    if src[3] == 0 {
        return;
    }
    let mut c = [src[0] as f32, src[1] as f32, src[2] as f32];
    if p.desaturate {
        let l = c[0] * 0.3 + c[1] * 0.56 + c[2] * 0.14;
        c = [l; 3];
    }
    if let Some([r, g, b, amount]) = p.blend {
        let tint = [r * 255.0, g * 255.0, b * 255.0];
        if amount >= 1.0 {
            // Without an amount, ZDoom colourises: brightness times colour.
            let l = (c[0] * 0.3 + c[1] * 0.56 + c[2] * 0.14) / 255.0;
            c = tint.map(|t| t * l);
        } else {
            for k in 0..3 {
                c[k] += (tint[k] - c[k]) * amount;
            }
        }
    }
    let a = p.alpha * src[3] as f32 / 255.0;
    let old = [dst[0] as f32, dst[1] as f32, dst[2] as f32];
    let out = match p.style {
        Style::Translucent => [0, 1, 2].map(|k| old[k] + (c[k] - old[k]) * a),
        Style::Add => [0, 1, 2].map(|k| old[k] + c[k] * a),
        Style::Subtract => [0, 1, 2].map(|k| old[k] - c[k] * a),
        Style::ReverseSubtract => [0, 1, 2].map(|k| c[k] * a - old[k]),
        Style::Modulate => [0, 1, 2].map(|k| old[k] * c[k] / 255.0),
        _ => c,
    };
    for k in 0..3 {
        dst[k] = out[k].round().clamp(0.0, 255.0) as u8;
    }
    if matches!(p.style, Style::Copy | Style::CopyAlpha | Style::CopyNewAlpha | Style::Overlay) || dst[3] == 0 {
        dst[3] = 255;
    }
}

/// Reads PNAMES: a count, then that many 8-byte names.
pub(super) fn read_pnames(d: &[u8]) -> Vec<String> {
    let b = Bytes::new(d);
    let n = b.index(0).unwrap_or(0).min(d.len().saturating_sub(4) / 8);
    (0..n).filter_map(|i| b.slice(4 + i * 8, 8).ok()).map(|s| cstr(s).trim().to_ascii_uppercase()).collect()
}

/// Reads TEXTURE1 or TEXTURE2: a count, offsets, then for each texture its
/// name, size and patch list.  Strife's variant drops two obsolete fields;
/// it is recognised by its records' sizes.
pub(super) fn read_textures(d: &[u8], pnames: &[String]) -> Vec<(String, Composite)> {
    let b = Bytes::new(d);
    let Ok(n) = b.index(0) else { return Vec::new() };
    let n = n.min(d.len() / 4);
    let offsets: Vec<usize> = (0..n).filter_map(|i| b.index(4 + i * 4).ok()).collect();
    // Doom: name 8, masked 4, width 2, height 2, column directory 4,
    // patch count 2, then 10 bytes per patch.  Strife: no column
    // directory, 6 bytes per patch.
    let fits = |at: usize, header: usize, stride: usize| -> bool {
        b.u16(at + header - 2).is_ok_and(|count| b.slice(at + header, count as usize * stride).is_ok())
    };
    let strife = !offsets.is_empty()
        && offsets.iter().take(16).all(|&at| fits(at, 18, 6))
        && !offsets.iter().take(16).all(|&at| fits(at, 22, 10));
    let (header, stride) = if strife { (18, 6) } else { (22, 10) };
    let mut out = Vec::new();
    for at in offsets {
        let (Ok(name), Ok(w), Ok(h), Ok(count)) =
            (b.slice(at, 8), b.u16(at + 12), b.u16(at + 14), b.u16(at + header - 2))
        else {
            continue;
        };
        let (w, h) = (w as u32, h as u32);
        if w == 0 || h == 0 || w > 4096 || h > 4096 {
            continue;
        }
        let mut patches = Vec::new();
        for k in 0..count as usize {
            let p = at + header + k * stride;
            let (Ok(x), Ok(y), Ok(i)) = (b.i16(p), b.i16(p + 2), b.u16(p + 4)) else { break };
            if let Some(pname) = pnames.get(i as usize) {
                patches.push(textures::PatchDef {
                    name: pname.clone(),
                    x: x as i32,
                    y: y as i32,
                    flip_x: false,
                    flip_y: false,
                    rotate: 0,
                    alpha: 1.0,
                    style: Style::Copy,
                    blend: None,
                    desaturate: false,
                });
            }
        }
        let c = Composite {
            width: w,
            height: h,
            scale: (1.0, 1.0),
            world_panning: false,
            null: false,
            patches,
            lumps_only: true,
            order: 0,
        };
        out.push((cstr(name).trim().to_ascii_uppercase(), c));
    }
    out
}

/// Decodes a lump as a picture: an image file (PNG, JPEG, TGA, ...), a
/// Doom patch, a raw flat or a raw 320x200 screen.
fn decode_lump(d: &[u8], hint: &Hint, palette: &[u8]) -> Option<Picture> {
    let image = |format: Option<slerp_scene::textures::Format>| -> Option<Picture> {
        let t = match format {
            Some(f) => {
                let opts = slerp_scene::textures::DecodeOptions {
                    min_size: None,
                    max_size: None,
                    all_mips: false,
                    first_layer_only: true,
                };
                let run = std::panic::catch_unwind(|| slerp_scene::textures::decode_as(f, d, &opts));
                run.ok()?.ok()?
            }
            None => decode_image(d, None).ok()?,
        };
        let px = match t.base() {
            slerp_scene::textures::Pixels::Rgba8(px) => px.clone(),
            _ => return None,
        };
        let (w, h) = (t.width(), t.height());
        if w > MAX_SIDE || h > MAX_SIDE || px.len() != w as usize * h as usize * 4 {
            return None;
        }
        let mut rgba = px;
        let holes = rgba.chunks_exact(4).any(|p| p[3] < 128);
        if holes {
            pixels::bleed(&mut rgba, w, h);
        }
        Some(Picture { width: w, height: h, rgba, holes })
    };
    use slerp_scene::textures::{Format, sniff};
    if let Some(Format::Png | Format::Jpeg | Format::Webp | Format::Bmp) = sniff(&d[..d.len().min(64)]) {
        return image(None);
    }
    if hint.tga
        && let Some(p) = image(Some(Format::Tga))
    {
        return Some(p);
    }
    if hint.flat
        && let Some(p) = decode_flat(d, palette)
    {
        return Some(p);
    }
    if let Ok((w, h, mut rgba)) = patch(d, palette) {
        let holes = rgba.chunks_exact(4).any(|p| p[3] == 0);
        if holes {
            pixels::bleed(&mut rgba, w, h);
        }
        return Some(Picture { width: w, height: h, rgba, holes });
    }
    if let Some(p) = decode_flat(d, palette) {
        return Some(p);
    }
    if d.len() == 64000 {
        let rgba = pixels::expand(d, 320, 200, palette, None);
        return Some(Picture { width: 320, height: 200, rgba, holes: false });
    }
    None
}

/// Decodes a flat: square raw palette indices (64x64, or the larger sizes
/// source ports allow; Heretic's 64x65 drops its extra row).
fn decode_flat(d: &[u8], palette: &[u8]) -> Option<Picture> {
    let side = match d.len() {
        4096 | 4160 => 64,
        16384 => 128,
        65536 => 256,
        262144 => 512,
        _ => return None,
    };
    let n = side * side;
    let rgba = pixels::expand(&d[..n], side as u32, side as u32, palette, None);
    Some(Picture { width: side as u32, height: side as u32, rgba, holes: false })
}
