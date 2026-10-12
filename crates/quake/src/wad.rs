//! Quake's WAD2 and Half-Life's WAD3 texture packs: a directory of named
//! lumps holding miptex textures, status-bar pictures ("qpic"), fonts and
//! a palette.  Loaded on their own they become a gallery; BSP maps also
//! look textures up in them by name.

use crate::miptex::{self, Palette};
use crate::pixels::{self, QUAKE_PALETTE};
use crate::read::{Bytes, Result, cap};
use crate::{find_in, find_relative, read_neighbour};
use slerp_scene::{Limits, LoadError, NamedImage, Texture};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Directory entries beyond this are ignored; real WADs hold a few
/// thousand at most.
const MAX_ENTRIES: usize = 65_536;

/// The most texels a gallery decodes (256 MB of RGBA), so a huge pack can't
/// exhaust memory.
const MAX_GALLERY_TEXELS: usize = 64 << 20;

/// One directory entry.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub kind: u8,
    pub offset: usize,
    pub size: usize,
    pub compressed: bool,
}

pub struct Wad<'a> {
    pub bytes: Bytes<'a>,
    /// True for WAD3, whose pictures carry their own palettes.
    pub half_life: bool,
    pub entries: Vec<Entry>,
}

impl<'a> Wad<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Wad<'a>> {
        let b = Bytes::new(data);
        let half_life = match b.slice(0, 4)? {
            b"WAD2" => false,
            b"WAD3" => true,
            _ => return Err(LoadError::NotMine),
        };
        let count = cap(b.index(4)?, MAX_ENTRIES, "WAD entries")?;
        let dir = b.index(8)?;
        let table = b.array(dir, count, 32)?;
        let t = Bytes::new(table);
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let at = i * 32;
            entries.push(Entry {
                offset: t.index(at)?,
                size: t.index(at + 8)?,
                kind: t.u8(at + 12)?,
                compressed: t.u8(at + 13)? != 0,
                name: t.name(at + 16, 16)?,
            });
        }
        Ok(Wad { bytes: b, half_life, entries })
    }

    /// The pack's own palette lump, which some Quake mods ship to recolour
    /// their textures.
    pub fn palette(&self) -> Option<&'a [u8]> {
        let e = self.entries.iter().find(|e| e.kind == b'@' || e.name.eq_ignore_ascii_case("palette"))?;
        if e.compressed || e.size < 768 {
            return None;
        }
        self.bytes.slice(e.offset, 768).ok()
    }

    /// Decodes an entry into a texture (and an emissive one for Quake
    /// fullbrights), or `None` for lumps that aren't pictures.
    pub fn decode(&self, e: &Entry, palette: &[u8], max: Option<u32>) -> Result<Option<miptex::Decoded>> {
        if e.compressed {
            return Err(LoadError::Unsupported(format!("{} is compressed", e.name)));
        }
        let lump = self.bytes.sub(e.offset, e.size)?;
        let container = if self.half_life { "Half-Life WAD3" } else { "Quake WAD2" };
        match (self.half_life, e.kind) {
            // Miptex.  Quake's conchars font has the miptex type but is a
            // bare 128x128 picture with no header.
            (false, b'D') if e.name.eq_ignore_ascii_case("conchars") && e.size == 128 * 128 => {
                let idx = lump.slice(0, 128 * 128)?;
                let rgba = pixels::expand(idx, 128, 128, palette, Some(0));
                Ok(Some(plain(pixels::texture(128, 128, rgba, max, container, "conchars (128x128 8-bit)"))))
            }
            (false, b'D') => miptex::decode(lump, 0, Palette::Global(palette), max, container).map(Some),
            (true, b'C') => miptex::decode(lump, 0, Palette::Embedded, max, container).map(Some),
            // Status bar and menu pictures: width, height, pixels.
            (false, b'B') => {
                let (w, h) = (lump.u32(0)?, lump.u32(4)?);
                let n = pixels::check_size(w, h)?;
                let rgba = pixels::expand(lump.slice(8, n)?, w, h, palette, Some(255));
                Ok(Some(plain(pixels::texture(w, h, rgba, max, container, &format!("picture ({w}x{h} 8-bit)")))))
            }
            (true, b'B') | (true, b'@') => {
                let (w, h) = (lump.u32(0)?, lump.u32(4)?);
                let n = pixels::check_size(w, h)?;
                let pal = embedded_palette(lump, 8 + n)?;
                // Decals (type '@') draw only where they are darker than
                // their last colour; show them as they are.
                let rgba = pixels::expand(lump.slice(8, n)?, w, h, &pal, None);
                Ok(Some(plain(pixels::texture(w, h, rgba, max, container, &format!("picture ({w}x{h} 8-bit)")))))
            }
            // Fonts: a 256-wide sheet after a table of 256 glyph extents.
            (true, b'F') => {
                let (w, h) = (lump.u32(0)?, lump.u32(4)?);
                let n = pixels::check_size(w, h)?;
                let pal = embedded_palette(lump, 16 + 1024 + n)?;
                let rgba = pixels::expand(lump.slice(16 + 1024, n)?, w, h, &pal, Some(255));
                Ok(Some(plain(pixels::texture(w, h, rgba, max, container, &format!("font ({w}x{h} 8-bit)")))))
            }
            _ => Ok(None),
        }
    }
}

/// A Half-Life palette stored at `at`: a colour count then that many RGB
/// triples.
fn embedded_palette(lump: Bytes, at: usize) -> Result<[u8; 768]> {
    let n = (lump.u16(at)? as usize).min(256);
    let mut pal = [0u8; 768];
    pal[..n * 3].copy_from_slice(lump.slice(at + 2, n * 3)?);
    Ok(pal)
}

fn plain(base: Texture) -> miptex::Decoded {
    miptex::Decoded { masked: base.has_alpha, base, emissive: None }
}

/// Loads every picture in a WAD2 or WAD3 as a named gallery, in directory
/// order.
pub fn load(bytes: &[u8], limits: &Limits) -> Result<Vec<NamedImage>> {
    let wad = Wad::parse(bytes)?;
    let palette: &[u8] = wad.palette().unwrap_or(QUAKE_PALETTE);
    let mut out = Vec::new();
    let mut texels = 0usize;
    for e in &wad.entries {
        // A bad lump costs only itself.
        let Ok(Some(d)) = wad.decode(e, palette, limits.max_texture_size) else { continue };
        texels += d.base.width() as usize * d.base.height() as usize;
        if texels > MAX_GALLERY_TEXELS {
            break;
        }
        out.push(NamedImage { name: e.name.clone(), texture: Arc::new(d.base) });
    }
    if out.is_empty() {
        return Err(LoadError::invalid("the WAD holds no pictures"));
    }
    Ok(out)
}

/*┌─────────────────╮
| Map textures    |
└─────────────────╯*/
//region Map textures

/// A texture found for a map, with its fullbright colours and whether it
/// has see-through texels.
#[derive(Clone)]
pub(crate) struct Found {
    pub base: Arc<Texture>,
    pub emissive: Option<Arc<Texture>>,
    pub masked: bool,
}

impl Found {
    pub fn from(d: miptex::Decoded) -> Found {
        Found { base: Arc::new(d.base), emissive: d.emissive.map(Arc::new), masked: d.masked }
    }
}

/// What `lookup` found.
#[derive(Default)]
pub(crate) struct Lookup {
    /// Textures by lowercase name.
    pub found: HashMap<String, Found>,
    /// True when any WAD read was Half-Life's WAD3.
    pub half_life: bool,
    /// The WAD files read.
    pub files: Vec<PathBuf>,
}

/// Finds a WAD that a map names, the way the tools that wrote the name
/// meant it: as a path relative to the map, an absolute path, or (since
/// most names are absolute paths on the mapper's own computer) by its file
/// name, or its last folder and file name, next to the map or in the
/// folders above it, where Half-Life keeps `halflife.wad`
/// (`valve/maps/x.bsp` beside `valve/halflife.wad`).
pub(crate) fn find_wad(map: &Path, stored: &str) -> Option<PathBuf> {
    let dir = map.parent()?;
    let unix = stored.replace('\\', "/");
    let parts: Vec<&str> = unix.split('/').filter(|p| !p.is_empty()).collect();
    let windows_drive = unix.as_bytes().get(1) == Some(&b':');
    if !windows_drive && !unix.is_empty() {
        let direct = if unix.starts_with('/') { PathBuf::from(&unix) } else { dir.join(&unix) };
        if direct.is_file() {
            return Some(direct);
        }
    }
    let base = *parts.last()?;
    let tail = parts.len().checked_sub(2).map(|i| parts[i..].join("/"));
    dir.ancestors().take(4).find_map(|a| find_in(a, base).or_else(|| tail.as_ref().and_then(|t| find_relative(a, t))))
}

/// Looks up `names` (lowercase) in the WADs a map lists (`list` is the
/// worldspawn's `wad` key: paths separated by semicolons), decoding each
/// texture once, from the first WAD that has it.  Missing WADs and WADs
/// that aren't WADs are reported in `warnings`.
pub(crate) fn lookup(
    list: &str,
    map: &Path,
    names: &HashSet<String>,
    limits: &Limits,
    warnings: &mut Vec<String>,
) -> Lookup {
    let mut out = Lookup::default();
    if !limits.neighbours {
        warnings.push("textures are in WAD files, which weren't read".into());
        return out;
    }
    for stored in list.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        if out.found.len() == names.len() {
            break;
        }
        let base = crate::base_name(stored);
        let Some(file) = find_wad(map, stored) else {
            warnings.push(format!("WAD not found: {base}"));
            continue;
        };
        if out.files.contains(&file) {
            continue;
        }
        let Some(bytes) = read_neighbour(&file) else { continue };
        out.files.push(file);
        let Ok(wad) = Wad::parse(&bytes) else {
            warnings.push(format!("{base} is not a WAD"));
            continue;
        };
        out.half_life |= wad.half_life;
        let palette = wad.palette().unwrap_or(QUAKE_PALETTE);
        for e in &wad.entries {
            let key = e.name.to_ascii_lowercase();
            if !names.contains(&key) || out.found.contains_key(&key) {
                continue;
            }
            if let Ok(Some(d)) = wad.decode(e, palette, limits.max_texture_size) {
                out.found.insert(key, Found::from(d));
            }
        }
    }
    out
}

//endregion

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::miptex::tests::build;

    /// Builds a WAD from (name, type, data) lumps.
    pub fn build_wad(magic: &[u8; 4], lumps: &[(&str, u8, Vec<u8>)]) -> Vec<u8> {
        let mut out = magic.to_vec();
        out.extend_from_slice(&(lumps.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        let mut dir = Vec::new();
        for (name, kind, data) in lumps {
            let off = out.len() as u32;
            out.extend_from_slice(data);
            dir.extend_from_slice(&off.to_le_bytes());
            dir.extend_from_slice(&(data.len() as u32).to_le_bytes());
            dir.extend_from_slice(&(data.len() as u32).to_le_bytes());
            dir.extend_from_slice(&[*kind, 0, 0, 0]);
            let mut n = [0u8; 16];
            n[..name.len()].copy_from_slice(name.as_bytes());
            dir.extend_from_slice(&n);
        }
        let dir_at = out.len() as u32;
        out[8..12].copy_from_slice(&dir_at.to_le_bytes());
        out.extend_from_slice(&dir);
        out
    }

    #[test]
    fn wad2_and_wad3_galleries() {
        let mut pic = Vec::new();
        pic.extend_from_slice(&4u32.to_le_bytes());
        pic.extend_from_slice(&2u32.to_le_bytes());
        pic.extend_from_slice(&[1; 8]);
        let w2 = build_wad(
            b"WAD2",
            &[
                ("brick", b'D', build("brick", 16, 16, |_, _| 3, None)),
                ("num_0", b'B', pic),
                ("sound", b'C', vec![1, 2, 3]),
            ],
        );
        let g = load(&w2, &Limits::viewer()).unwrap();
        assert_eq!(g.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(), ["brick", "num_0"]);

        let pal = [9u8; 768];
        let w3 =
            build_wad(b"WAD3", &[("{grate", 0x43, build("{grate", 32, 32, |x, _| (x % 2 * 255) as u8, Some(&pal)))]);
        let g = load(&w3, &Limits::thumbnail()).unwrap();
        assert!(g[0].texture.has_alpha);
    }

    #[test]
    fn corrupt_wads_fail_cleanly() {
        let w2 = build_wad(b"WAD2", &[("brick", b'D', build("brick", 16, 16, |_, _| 3, None))]);
        for cut in [0, 4, 11, 12, 40, w2.len() - 1] {
            assert!(load(&w2[..cut], &Limits::viewer()).is_err(), "cut at {cut}");
        }
        let mut bad = w2.clone();
        bad[4..8].copy_from_slice(&0x7fff_ffffu32.to_le_bytes());
        assert!(load(&bad, &Limits::viewer()).is_err());
        let mut bad = w2.clone();
        let dir = u32::from_le_bytes(bad[8..12].try_into().unwrap()) as usize;
        bad[dir..dir + 4].copy_from_slice(&0xffff_0000u32.to_le_bytes());
        assert!(load(&bad, &Limits::viewer()).is_err());
    }

    #[test]
    fn real_wads() {
        let mut paths = Vec::new();
        for p in [crate::testutil::chalk!("local/qbsp/chalk.wad"), crate::testutil::chalk!("local/calib/chalk.wad")] {
            paths.extend(crate::testutil::external(p));
        }
        for p in [
            "quake-extra/wads/lq_medieval.wad",
            "quake-extra/wads/lq_liquidsky.wad",
            "quake-extra/wads/lq_utility.wad",
            "quake-extra/libquake/gfx.wad",
        ] {
            paths.extend(crate::testutil::asset(p));
        }
        for p in paths {
            let bytes = std::fs::read(&p).unwrap();
            let t = std::time::Instant::now();
            let g = load(&bytes, &Limits::thumbnail()).unwrap();
            let thumb = t.elapsed();
            let t = std::time::Instant::now();
            let full = load(&bytes, &Limits::viewer()).unwrap();
            eprintln!(
                "{}: {} pictures, thumbnail {thumb:?}, viewer {:?}, first {:?}",
                p.display(),
                full.len(),
                t.elapsed(),
                g.iter().take(4).map(|n| &n.name).collect::<Vec<_>>()
            );
            assert_eq!(g.len(), full.len());
        }
    }

    /// Writes the first pictures of a WAD side by side, for checking by eye.
    #[test]
    fn debug_contact_sheet() {
        let Some(p) = crate::testutil::asset("quake-extra/wads/lq_medieval.wad") else { return };
        let g = load(&std::fs::read(&p).unwrap(), &Limits { max_texture_size: Some(64), ..Limits::viewer() }).unwrap();
        let (cols, cell) = (8usize, 64usize);
        let rows = g.len().min(32).div_ceil(cols);
        let mut sheet = vec![0u8; cols * cell * rows * cell * 4];
        for (i, n) in g.iter().take(32).enumerate() {
            let t = &n.texture;
            let slerp_scene::textures::Pixels::Rgba8(px) = t.base() else { continue };
            let (ox, oy) = ((i % cols) * cell, (i / cols) * cell);
            for y in 0..(t.height() as usize).min(cell) {
                for x in 0..(t.width() as usize).min(cell) {
                    let s = (y * t.width() as usize + x) * 4;
                    let d = ((oy + y) * cols * cell + ox + x) * 4;
                    sheet[d..d + 4].copy_from_slice(&px[s..s + 4]);
                }
            }
        }
        crate::testutil::write_png(
            std::path::Path::new("/tmp/slerp-quake/lq_medieval.png"),
            (cols * cell) as u32,
            (rows * cell) as u32,
            &sheet,
        );
    }
}
