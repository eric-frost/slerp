//! Doom's resource files: WADs (IWAD and PWAD, a directory of named lumps)
//! and the PK3 and PK7 archives (zip and 7z) that GZDoom and other source
//! ports load, whose folders stand for a WAD's namespaces.
//!
//! - `load` makes a gallery: the TITLEPIC title screen first, then a 2D
//!   automap of each map drawn the way Doom's own automap colours lines.
//!   Thumbnails show it.
//! - `parts` lists the maps, and `load_part` builds one as a 3D level (see
//!   `level`).  A PWAD's or archive's maps borrow textures and flats from an
//!   IWAD found beside it or where games are installed: Steam and GOG
//!   libraries, GZDoom's search list, Doom's usual folders (see `iwad`).
//!
//! Maps are found by structure rather than by name: a marker lump followed
//! by THINGS (the classic Doom and Hexen formats) or TEXTMAP (UDMF), or a
//! WAD in an archive's `maps/` folder, named after its file.

mod animdefs;
mod archive;
mod automap;
mod gldefs;
mod graphics;
mod iwad;
mod level;
mod lighting;
mod map;
mod mapinfo;
mod nodes;
mod planes;
mod polygon;
mod resources;
mod script;
mod sky;
mod textures;
mod things;
mod xfloors;

pub(crate) use archive::{Source, is_7z, is_zip};

use crate::pixels::{self, DOOM_PALETTE};
use crate::read::{Bytes, Result, cap};
use crate::{decode_image, read::cstr};
use archive::{Archive, Namespace};
use resources::{MapLoc, ResFile, Resources};
use slerp_scene::{Limits, LoadError, NamedImage, Scene, Texture};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAX_LUMPS: usize = 1 << 20;

/// The longest side of an automap picture.
const AUTOMAP_SIZE: u32 = 1024;

/// The most texels the gallery holds (256 MB), so a megawad's thirty-odd
/// maps can't exhaust memory.
const MAX_GALLERY_TEXELS: usize = 64 << 20;

/// Whether the first bytes look like a Doom WAD header: a lump count and
/// directory offset that aren't negative.
pub fn plausible_header(head: &[u8]) -> bool {
    let b = Bytes::new(head);
    match (b.i32(4), b.i32(8)) {
        (Ok(n), Ok(dir)) => n >= 0 && dir >= 12,
        _ => true,
    }
}

/// Whether an archive holds Doom content: maps, Doom's text lumps, or its
/// picture folders.  Other zip-based formats (3MF, USDZ, OpenRaster) have
/// none of these.
pub fn archive_is_doom<'p>(paths: impl IntoIterator<Item = &'p str>) -> bool {
    const LUMPS: &[&str] = &[
        "MAPINFO", "ZMAPINFO", "UMAPINFO", "EMAPINFO", "TEXTURES", "TEXTURE1", "PNAMES", "PLAYPAL", "DECORATE",
        "ZSCRIPT", "GLDEFS", "ANIMDEFS", "SNDINFO", "GAMEINFO", "KEYCONF", "MENUDEF", "SBARINFO", "IWADINFO",
        "LANGUAGE", "LOCKDEFS", "TERRAIN", "MODELDEF", "TITLEPIC", "COLORMAP",
    ];
    paths.into_iter().any(|p| {
        let lower = p.to_ascii_lowercase();
        let (ns, name) = archive::classify(&lower);
        match ns {
            Namespace::Maps => lower.ends_with(".wad"),
            Namespace::Flats | Namespace::Patches | Namespace::Sprites => true,
            Namespace::Global => LUMPS.contains(&name.as_str()) || lower.ends_with(".wad"),
            _ => false,
        }
    })
}

/// Lists an archive's file names, for sniffing: `None` when it isn't a
/// readable zip or 7z.
pub fn archive_paths(source: Source) -> Option<Vec<String>> {
    let a = Archive::open(source).ok()?;
    Some(a.entries.into_iter().map(|e| e.path).collect())
}
/*┌─────────────────╮
| WAD directory   |
└─────────────────╯*/
//region WAD directory

pub(crate) struct Lump<'a> {
    pub name: String,
    pub data: &'a [u8],
}

pub(crate) struct Wad<'a> {
    pub iwad: bool,
    pub lumps: Vec<Lump<'a>>,
}

impl<'a> Wad<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Wad<'a>> {
        let b = Bytes::new(data);
        if !(b.starts_with(b"IWAD") || b.starts_with(b"PWAD")) {
            return Err(LoadError::NotMine);
        }
        let count = cap(b.index(4)?, MAX_LUMPS, "lumps")?;
        let dir = Bytes::new(b.array(b.index(8)?, count, 16)?);
        let mut lumps = Vec::with_capacity(count);
        for i in 0..count {
            let (ofs, size) = (dir.index(i * 16)?, dir.index(i * 16 + 4)?);
            // Markers have no data; a bad lump is kept empty so map
            // structure around it still reads.
            let data = if size == 0 { &[][..] } else { b.slice(ofs, size).unwrap_or(&[]) };
            lumps.push(Lump { name: cstr(dir.slice(i * 16 + 8, 8)?).to_ascii_uppercase(), data });
        }
        Ok(Wad { iwad: b.starts_with(b"IWAD"), lumps })
    }

    /// The last lump called `name`: later lumps replace earlier ones, as in
    /// Doom.
    pub fn last(&self, name: &str) -> Option<&'a [u8]> {
        self.lumps.iter().rev().find(|l| l.name == name && !l.data.is_empty()).map(|l| l.data)
    }

    /// The maps' marker lumps: (index, name), in order.
    pub fn maps(&self) -> Vec<(usize, &str)> {
        (0..self.lumps.len().saturating_sub(1))
            .filter(|&i| matches!(self.lumps[i + 1].name.as_str(), "THINGS" | "TEXTMAP"))
            .map(|i| (i, self.lumps[i].name.as_str()))
            .collect()
    }

    /// Reads the map whose marker is lump `i`.
    pub fn map(&self, i: usize) -> Option<map::MapData> {
        map::read(self.lumps.get(i + 1..)?)
    }
}

//endregion

/*┌─────────────────╮
| Gallery         |
└─────────────────╯*/
//region Gallery

pub fn load(data: &[u8], limits: &Limits) -> Result<Vec<NamedImage>> {
    let wad = Wad::parse(data)?;
    let palette: &[u8] = wad.last("PLAYPAL").filter(|p| p.len() >= 768).map_or(&DOOM_PALETTE[..], |p| &p[..768]);
    let size = limits.max_texture_size.map_or(AUTOMAP_SIZE, |m| m.min(AUTOMAP_SIZE)).max(16);
    let mut out = Vec::new();
    let mut texels = 0usize;
    let mut push = |out: &mut Vec<NamedImage>, name: String, tex: Texture| -> bool {
        texels += tex.width() as usize * tex.height() as usize;
        if texels > MAX_GALLERY_TEXELS {
            return false;
        }
        out.push(NamedImage { name, texture: Arc::new(tex) });
        true
    };

    // 1. The title screen.
    if let Some(l) = wad.last("TITLEPIC")
        && let Ok(t) = picture(l, palette, limits.max_texture_size)
    {
        push(&mut out, "TITLEPIC".into(), t);
    }

    // 2. Every map's automap.
    for (i, name) in wad.maps() {
        let Some(map) = wad.map(i) else { continue };
        if let Some(t) = automap::draw(&map, palette, size)
            && !push(&mut out, name.to_string(), t)
        {
            break;
        }
    }
    if out.is_empty() {
        return Err(LoadError::Unsupported("the WAD has no title picture or maps".into()));
    }
    Ok(out)
}

/// Decodes a title picture: a PNG (as source ports allow), a raw 320x200
/// screen (Heretic and Hexen), or Doom's column-based patch format.
fn picture(data: &[u8], palette: &[u8], max: Option<u32>) -> Result<Texture> {
    if data.starts_with(b"\x89PNG") {
        return decode_image(data, max).map_err(LoadError::Invalid);
    }
    if data.len() == 320 * 200 {
        let rgba = pixels::expand(data, 320, 200, palette, None);
        return Ok(pixels::texture(320, 200, rgba, max, "Doom WAD", "raw screen (320x200 8-bit)"));
    }
    let (w, h, rgba) = patch(data, palette)?;
    Ok(pixels::texture(w, h, rgba, max, "Doom WAD", &format!("patch ({w}x{h} 8-bit)")))
}

/// Decodes a patch: a size, then for each column an offset to its "posts",
/// runs of pixels starting at a row.  Gaps stay transparent.
pub fn patch(data: &[u8], palette: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let b = Bytes::new(data);
    let (w, h) = (b.u16(0)? as u32, b.u16(2)? as u32);
    let n = pixels::check_size(w, h)?;
    if w > 4096 || h > 4096 {
        return Err(LoadError::invalid("patch is too large"));
    }
    let mut rgba = vec![0u8; n * 4];
    for x in 0..w as usize {
        let mut at = b.u32(8 + 4 * x)? as usize;
        let mut top: i64 = -1;
        // A column has at most one post per row, plus the end marker.
        for _ in 0..=h.max(256) {
            let delta = b.u8(at)?;
            if delta == 255 {
                break;
            }
            // Tall patches (DeePsea's extension) store offsets relative to
            // the previous post once rows pass 254.
            top = if (delta as i64) <= top { top + delta as i64 } else { delta as i64 };
            let len = b.u8(at + 1)? as usize;
            let run = b.slice(at + 3, len)?;
            for (i, &c) in run.iter().enumerate() {
                let y = top as usize + i;
                if y < h as usize {
                    let p = (y * w as usize + x) * 4;
                    let rgb = &palette[c as usize * 3..c as usize * 3 + 3];
                    rgba[p..p + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
                }
            }
            at += len + 4;
        }
    }
    Ok((w, h, rgba))
}

//endregion

/*┌─────────────────╮
| Archives        |
└─────────────────╯*/
//region Archives

/// The gallery of a PK3 or PK7: its title picture, then its maps'
/// automaps.  Thumbnails (which ask for small textures) stop after the
/// title, or the first map when there is none, since unpacking every map
/// of a megawad takes seconds.
pub fn load_archive(source: Source, limits: &Limits) -> Result<Vec<NamedImage>> {
    let archive = Archive::open(source)?;
    with_stack(None, Main::Archive(&archive), false, |res, _| {
        let palette = res.palette().unwrap_or_else(|| DOOM_PALETTE.to_vec());
        let size = limits.max_texture_size.map_or(AUTOMAP_SIZE, |m| m.min(AUTOMAP_SIZE)).max(16);
        let quick = limits.max_texture_size.is_some();
        let mut out = Vec::new();
        let mut texels = 0usize;
        let title = res.last(Namespace::Graphics, "TITLEPIC").or_else(|| res.last(Namespace::Global, "TITLEPIC"));
        if let Some(d) = title.and_then(|id| res.data(id))
            && let Ok(t) = picture(&d, &palette, limits.max_texture_size)
        {
            texels += t.width() as usize * t.height() as usize;
            out.push(NamedImage { name: "TITLEPIC".into(), texture: Arc::new(t) });
        }
        if !quick || out.is_empty() {
            for name in map_order(res) {
                let Some(t) = with_map(res, &name, |map| automap::draw(map, &palette, size)).ok().flatten() else {
                    continue;
                };
                texels += t.width() as usize * t.height() as usize;
                if texels > MAX_GALLERY_TEXELS {
                    break;
                }
                out.push(NamedImage { name, texture: Arc::new(t) });
                if quick {
                    break;
                }
            }
        }
        if out.is_empty() {
            return Err(LoadError::Unsupported("the archive has no title picture or maps".into()));
        }
        Ok(out)
    })
}

/// The maps of a PK3 or PK7, in MAPINFO's order, then the others by name.
pub fn archive_parts(source: Source) -> Vec<String> {
    let Ok(archive) = Archive::open(source) else { return Vec::new() };
    with_stack(None, Main::Archive(&archive), false, |res, _| Ok(map_order(res))).unwrap_or_default()
}

/// Builds map `name` of a PK3 or PK7 as a 3D level.
pub fn load_archive_part(path: &Path, source: Source, limits: &Limits, name: &str) -> Result<Scene> {
    let archive = Archive::open(source)?;
    level_from(path, Main::Archive(&archive), limits, name)
}

/// Map names in MAPINFO's order (the order a mod plays them), then those
/// MAPINFO doesn't name, sorted.
fn map_order(res: &Resources) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for (n, _) in res.maps() {
        if !names.contains(&n) {
            names.push(n);
        }
    }
    let infos = mapinfos(res);
    let mut ordered: Vec<String> =
        infos.maps.iter().map(|(n, _)| n.clone()).filter(|n| names.contains(n)).fold(Vec::new(), |mut v, n| {
            if !v.contains(&n) {
                v.push(n);
            }
            v
        });
    let mut rest: Vec<String> = names.into_iter().filter(|n| !ordered.contains(n)).collect();
    rest.sort();
    ordered.extend(rest);
    ordered
}

//endregion

/*┌─────────────────╮
| Composites      |
└─────────────────╯*/
//region Composites

/// A composite texture's name and size, as TEXTURE1 or TEXTURES defines it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompositeInfo {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// How many patches it is made of.
    pub patches: usize,
}

/// The textures a TEXTURE1, TEXTURE2 or TEXTURE3 lump defines, with the
/// file's PNAMES lump naming their patches.
pub fn texture_defs(lump: &[u8], pnames: &[u8]) -> Vec<CompositeInfo> {
    let names = graphics::read_pnames(pnames);
    graphics::read_textures(lump, &names)
        .into_iter()
        .map(|(name, c)| CompositeInfo { name, width: c.width, height: c.height, patches: c.patches.len() })
        .collect()
}

/// The textures a ZDoom TEXTURES lump defines (`#include`s aren't followed).
pub fn zdoom_texture_defs(text: &[u8]) -> Vec<CompositeInfo> {
    let mut parsed = textures::Parsed::default();
    textures::parse(text, &mut |_| None, &mut parsed);
    parsed
        .textures
        .into_iter()
        .map(|t| CompositeInfo { name: t.name, width: t.width, height: t.height, patches: t.patches.len() })
        .collect()
}

/// A Doom resource file to read textures from.
#[derive(Clone, Copy)]
pub enum ResourceFile<'a> {
    /// A WAD held in memory.
    Wad(&'a [u8]),
    /// A PK3 or PK7 on disk.
    Archive(&'a Path),
}

/// Composes a file's textures, over the resource stack a source port would
/// build for it: an IWAD found beside it or where games are installed (for a
/// PWAD or PK3 whose patches live there), then the file itself.
pub struct Composites<'a> {
    graphics: graphics::Graphics<'a>,
}

impl Composites<'_> {
    /// The picture of the texture (or flat) called `name`, or `None` when
    /// the stack has none or its patches can't be read.
    pub fn render(&mut self, name: &str) -> Option<crate::convert::Image> {
        let tex = self.graphics.wall(name).or_else(|| self.graphics.flat(name))?;
        let p = &tex.picture;
        Some(crate::convert::Image { width: p.width, height: p.height, rgba: p.rgba.clone(), offset: None })
    }
}

/// Runs `f` with the composer for `file`.  `near` is the file's path, for
/// finding an IWAD beside it; `None` looks nowhere.
pub fn with_composites<T>(file: ResourceFile, near: Option<&Path>, f: impl FnOnce(&mut Composites) -> T) -> Result<T> {
    let run = |main: Main, map: &str, f: &mut dyn FnMut(&mut Composites)| {
        with_stack(near.map(|p| (p, map)), main, near.is_some(), |res, _| {
            f(&mut Composites { graphics: graphics::Graphics::new(res) });
            Ok(())
        })
    };
    let mut f = Some(f);
    let mut out = None;
    let mut call = |c: &mut Composites| {
        if let Some(f) = f.take() {
            out = Some(f(c));
        }
    };
    match file {
        ResourceFile::Wad(bytes) => {
            let wad = Wad::parse(bytes)?;
            let map = wad.maps().first().map_or("MAP01".to_string(), |(_, n)| n.to_string());
            run(Main::Wad(&wad), &map, &mut call)?;
        }
        ResourceFile::Archive(path) => {
            let archive = Archive::open(Source::File(path.to_path_buf()))?;
            run(Main::Archive(&archive), "MAP01", &mut call)?;
        }
    }
    out.ok_or_else(|| LoadError::invalid("the textures couldn't be composed"))
}

//endregion

/*┌─────────────────╮
| Levels          |
└─────────────────╯*/
//region Levels

/// The names of the WAD's maps in order, each once.
pub fn parts(data: &[u8]) -> Vec<String> {
    let Ok(wad) = Wad::parse(data) else { return Vec::new() };
    let mut names: Vec<String> = Vec::new();
    for (_, name) in wad.maps() {
        if !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    }
    names
}

/// Builds map `name` as a 3D level.  A PWAD's missing textures come from
/// an IWAD found by `find_iwad`, when `limits.neighbours` allows.
pub fn load_part(path: &Path, data: &[u8], limits: &Limits, name: &str) -> Result<Scene> {
    let wad = Wad::parse(data)?;
    level_from(path, Main::Wad(&wad), limits, name)
}

/// The file a level is opened from.
#[derive(Clone, Copy)]
enum Main<'a> {
    Wad(&'a Wad<'a>),
    Archive(&'a Archive),
}

/// The largest WAD inside an archive that is read.
const MAX_INNER_WAD: u64 = 512 << 20;

/// Runs `f` over the resource stack a file makes: its IWAD (when
/// `iwad_from` names the path to search beside), the file, and the WADs
/// at an archive's root.
fn with_stack<T>(
    iwad_from: Option<(&Path, &str)>,
    main: Main,
    neighbours: bool,
    f: impl FnOnce(&Resources, Option<&Path>) -> Result<T>,
) -> Result<T> {
    // 1. WADs inside an archive's root.
    let inner: Vec<Vec<u8>> = match main {
        Main::Archive(a) => {
            resources::embedded_wads(a).into_iter().filter_map(|i| a.read(i, MAX_INNER_WAD).ok()).collect()
        }
        Main::Wad(_) => Vec::new(),
    };
    let inner: Vec<Wad> = inner.iter().filter_map(|b| Wad::parse(b).ok()).collect();

    // 2. The IWAD, unless the file is one.
    let is_iwad = matches!(main, Main::Wad(w) if w.iwad);
    let main_file = match main {
        Main::Wad(w) => ResFile::Wad(w),
        Main::Archive(a) => ResFile::Archive(a),
    };
    let mut iwad_file = None;
    if let Some((path, map)) = iwad_from
        && !is_iwad
        && neighbours
    {
        // GAMEINFO names the IWAD an archive was made for.
        let probe = Resources::new(std::iter::once(main_file).chain(inner.iter().map(ResFile::Wad)).collect());
        let wanted = probe.texts("GAMEINFO").iter().rev().find_map(|t| gameinfo_iwad(t));
        let hexen = probe.maps().iter().any(|(n, loc)| n.eq_ignore_ascii_case(map) && is_hexen_loc(&probe, *loc));
        iwad_file = find_iwad(path, map, hexen, wanted.as_deref());
    }
    let iwad = iwad_file.as_ref().and_then(|(_, bytes)| Wad::parse(bytes).ok());
    let mut files: Vec<ResFile> = Vec::new();
    if let Some(w) = &iwad {
        files.push(ResFile::Wad(w));
    }
    files.push(main_file);
    files.extend(inner.iter().map(ResFile::Wad));
    let res = Resources::new(files);
    f(&res, iwad.as_ref().and(iwad_file.as_ref()).map(|(p, _)| p.as_path()))
}

/// Whether a map is in Hexen's binary format (it has scripts).
fn is_hexen_loc(res: &Resources, loc: MapLoc) -> bool {
    match loc {
        MapLoc::Marker { file, index } => match res.files[file as usize] {
            ResFile::Wad(w) => w.lumps.get(index as usize + 1..).is_some_and(is_hexen_map),
            ResFile::Archive(_) => false,
        },
        MapLoc::Nested { .. } => false,
    }
}

/// GAMEINFO's `IWAD = "DOOM2.WAD"`.
fn gameinfo_iwad(text: &[u8]) -> Option<String> {
    let mut sc = script::Scanner::new(text);
    while let Some(t) = sc.next() {
        if t.is("iwad") && sc.eat(b'=') {
            return sc.text().map(|s| s.to_ascii_lowercase());
        }
    }
    None
}

/// Reads map `name` from the stack and runs `f` on it.
fn with_map<T>(res: &Resources, name: &str, f: impl FnOnce(&map::MapData) -> T) -> Result<T> {
    let (_, loc) = res
        .maps()
        .into_iter()
        .rev()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .ok_or_else(|| LoadError::invalid(format!("no map {name}")))?;
    match loc {
        MapLoc::Marker { file, index } => {
            let ResFile::Wad(w) = res.files[file as usize] else { return Err(LoadError::invalid("bad map")) };
            let map = w.map(index as usize).ok_or_else(|| LoadError::invalid(format!("map {name} can't be read")))?;
            Ok(f(&map))
        }
        MapLoc::Nested { lump } => {
            let bytes = res.data(lump).ok_or_else(|| LoadError::invalid(format!("map {name} can't be unpacked")))?;
            let wad = Wad::parse(&bytes)?;
            let (at, _) = wad
                .maps()
                .into_iter()
                .next()
                .ok_or_else(|| LoadError::invalid(format!("{name}'s WAD holds no map")))?;
            let map = wad.map(at).ok_or_else(|| LoadError::invalid(format!("map {name} can't be read")))?;
            Ok(f(&map))
        }
    }
}

/// Every MAPINFO-family lump of the stack, in load order: a file's
/// ZMAPINFO replaces its MAPINFO, as in GZDoom.
fn mapinfos(res: &Resources) -> mapinfo::Infos {
    let mut infos = mapinfo::Infos::default();
    for fi in 0..res.files.len() as u32 {
        let of = |name: &str| -> Vec<u32> {
            res.all(Namespace::Global, name).iter().copied().filter(|&id| res.file_of(id) == Some(fi)).collect()
        };
        let z = of("ZMAPINFO");
        let lumps = if z.is_empty() { of("MAPINFO") } else { z };
        for id in lumps.into_iter().chain(of("UMAPINFO")) {
            if let Some(d) = res.data(id) {
                infos.parse(&d, &mut |n| res.include(n));
            }
        }
    }
    infos
}

/// MAPINFO's entry for a map, ZMAPINFO's thing names and GLDEFS.
fn extras(res: &Resources, name: &str) -> level::Extras {
    let infos = mapinfos(res);
    let mut gldefs = gldefs::Gldefs::default();
    for text in res.texts("GLDEFS") {
        gldefs.parse(&text, &mut |n| res.include(n));
    }
    level::Extras { info: infos.map(name).cloned(), ednums: infos.ednums.clone(), gldefs }
}

fn level_from(path: &Path, main: Main, limits: &Limits, name: &str) -> Result<Scene> {
    let is_iwad = matches!(main, Main::Wad(w) if w.iwad);
    with_stack(Some((path, name)), main, limits.neighbours, |res, iwad| {
        let extras = extras(res, name);
        let mut gfx = graphics::Graphics::new(res);
        let upper = name.to_ascii_uppercase();
        let (mut scene, _) = with_map(res, name, |map| level::build(map, &upper, &mut gfx, limits, &extras))??;
        if !is_iwad {
            let from = iwad.map_or_else(|| "this file only (no IWAD found)".into(), |p| p.display().to_string());
            scene.info.details.push(("Textures from".into(), from));
        }
        Ok(scene)
    })
}

fn is_hexen_map(after: &[Lump]) -> bool {
    after.iter().take(12).any(|l| l.name == "BEHAVIOR")
}

/// Builds map `name` of `wad`, over `iwad`'s resources when given.
#[cfg(test)]
pub(crate) fn build_level(wad: &Wad, iwad: Option<&Wad>, name: &str, limits: &Limits) -> Result<Scene> {
    let mut files: Vec<ResFile> = iwad.into_iter().map(ResFile::Wad).collect();
    files.push(ResFile::Wad(wad));
    let res = Resources::new(files);
    let wanted = name.to_ascii_uppercase();
    if !wad.maps().iter().any(|(_, n)| *n == wanted) {
        return Err(LoadError::invalid(format!("the WAD has no map {name}")));
    }
    let extras = extras(&res, &wanted);
    let mut gfx = graphics::Graphics::new(&res);
    with_map(&res, &wanted, |map| level::build(map, &wanted, &mut gfx, limits, &extras).map(|(s, _)| s))?
}

/// Finds and reads an IWAD for a PWAD's map: (path, bytes).  `wanted` is
/// the IWAD a mod's GAMEINFO names, tried first.
fn find_iwad(pwad: &Path, map: &str, hexen: bool, wanted: Option<&str>) -> Option<(PathBuf, Vec<u8>)> {
    iwad::find(pwad, &iwad::names(map, hexen, wanted), &iwad::folders(pwad))
}

//endregion

#[cfg(test)]
mod tests;
