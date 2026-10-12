//! Quake, Half-Life and Doom files, and the thumbnails Unreal stores in its
//! packages.
//!
//! - Maps (Quake BSP 29, BSP2 and 2PSB, Half-Life BSP 30) become levels with
//!   textures, lightmaps, sky and liquid surfaces, a spawn point and entity
//!   markers.
//! - Models (Quake MDL, Half-Life MDL, MD2, MD3) become scenes whose
//!   animation frames are morph targets or, for Half-Life, skeletal clips.
//!   Source engine models (MDL versions 44 to 49, with their `.vvd` and
//!   `.vtx` files and VMT materials) become skinned scenes with their
//!   sequences as clips, flexes as morph targets and skin families as parts.
//! - Texture packs (WAD2, WAD3), Doom WADs and sprites (Quake and Half-Life
//!   `.spr`, Quake 2 `.sp2`) become galleries.  A Doom
//!   WAD's maps are also its parts (`parts`), each of which `load_part`
//!   builds as a level: sectors, walls with Doom's texture alignment,
//!   sector light, sky, the player's start and things as markers, and
//!   GZDoom's slopes, 3D floors, coloured light and fog.
//! - PK3 and PK7 archives (GZDoom's zip and 7z resource files) are Doom
//!   files too.  They run to hundreds of megabytes, so `load_path`,
//!   `parts_path` and `load_part_path` read them from disk piece by piece.
//! - Unreal `.uasset` files give up the picture the editor saved in them.
//!
//! Every loader converts Z-up coordinates to Y-up and its units to metres
//! at one inch per unit, the scale id's and Valve's art assumes.  Doom's
//! heights are 1.2 inches per unit, for the tall pixels it was drawn with.

mod bsp;
pub mod convert;
mod doom;
mod editor;
mod entities;
mod hlmdl;
mod md2;
mod md3;
mod mdl;
mod miptex;
mod morph;
mod pixels;
mod read;
mod sprite;
mod studio;
#[cfg(test)]
mod testutil;
mod uasset;
mod vpk;
mod vtf;
mod wad;

use slerp_scene::glam::Vec3;
use slerp_scene::textures::{DecodeOptions, Texture};
use slerp_scene::{Asset, Limits, LoadError};
use std::path::{Path, PathBuf};

/*┌─────────────────╮
| Public API      |
└─────────────────╯*/
//region Public API

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuakeFormat {
    Bsp,
    QuakeMdl,
    HalfLifeMdl,
    /// A Source engine model (`.mdl` version 44 to 49, with `.vvd` and
    /// `.vtx` files beside it).
    SourceMdl,
    Md2,
    Md3,
    Wad2,
    Wad3,
    DoomWad,
    /// A PK3 or PK7: a zip or 7z archive holding Doom maps or resources.
    DoomArchive,
    UnrealAsset,
    /// A Quake-family map's source (`.map`, from TrenchBroom, J.A.C.K.,
    /// Radiant): brushes as planes, entities, patches.
    Map,
    /// A Source engine map's source (`.vmf`, from Hammer).
    Vmf,
    /// A Quake or Half-Life sprite (`.spr`), a gallery of frames.
    Sprite,
    /// A Quake 2 sprite (`.sp2`), whose frames are `.pcx` files.
    Sprite2,
}

/// Recognises a file by its magic bytes: BSP version 29, "BSP2", "2PSB",
/// 30; "IDPO"; "IDST" version 10 (Half-Life) or 44 to 59 (Source, whose
/// versions past 49 load as a message saying they aren't supported);
/// "IDP2"; "IDP3"; "WAD2"; "WAD3";
/// "IWAD"/"PWAD"; the .uasset package tag.  `.mdl` files without these
/// magics are not claimed.  `head` should hold the first 128 bytes or more;
/// BSP versions 29 and 30 are plain numbers, so their lump table is checked
/// too when it is there.  Zip and 7z files are Doom archives when their
/// directory (read from `path`) lists Doom content; without the file, the
/// extension decides.
pub fn sniff(path: &Path, head: &[u8]) -> Option<QuakeFormat> {
    if doom::is_zip(head) || doom::is_7z(head) {
        return sniff_archive(path, head);
    }
    let magic = head.get(..4)?;
    let version = head.get(4..8).map(|v| i32::from_le_bytes([v[0], v[1], v[2], v[3]]));
    match magic {
        b"BSP2" | b"2PSB" => Some(QuakeFormat::Bsp),
        [29 | 30, 0, 0, 0] if bsp::plausible_header(head) => Some(QuakeFormat::Bsp),
        b"IDPO" if version == Some(6) => Some(QuakeFormat::QuakeMdl),
        b"IDST" if version == Some(10) => Some(QuakeFormat::HalfLifeMdl),
        b"IDST" if studio::sniff(head) => Some(QuakeFormat::SourceMdl),
        b"IDP2" if version == Some(8) => Some(QuakeFormat::Md2),
        b"IDP3" if version == Some(15) => Some(QuakeFormat::Md3),
        b"IDSP" if sprite::plausible_spr(head) => Some(QuakeFormat::Sprite),
        b"IDS2" if sprite::plausible_sp2(head) => Some(QuakeFormat::Sprite2),
        b"WAD2" => Some(QuakeFormat::Wad2),
        b"WAD3" => Some(QuakeFormat::Wad3),
        b"IWAD" | b"PWAD" if doom::plausible_header(head) => Some(QuakeFormat::DoomWad),
        _ if head.starts_with(&uasset::TAG) => Some(QuakeFormat::UnrealAsset),
        _ if editor::sniff_map(head) => Some(QuakeFormat::Map),
        _ if editor::sniff_vmf(head) => Some(QuakeFormat::Vmf),
        _ => None,
    }
}

/// Archive extensions source ports load.
const DOOM_ARCHIVES: &[&str] = &["pk3", "pk7", "ipk3", "ipk7", "pke", "pk4"];

fn sniff_archive(path: &Path, head: &[u8]) -> Option<QuakeFormat> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    // Substance archives are 7z files too, and the materials loader's.
    if ext == "sbsar" {
        return None;
    }
    let by_name = DOOM_ARCHIVES.contains(&ext.as_str());
    match doom::Source::open(path).ok().and_then(doom::archive_paths) {
        Some(paths) => doom::archive_is_doom(paths.iter().map(String::as_str)).then_some(QuakeFormat::DoomArchive),
        // No file to look into (or an unreadable one): trust the name.
        None => (by_name && head.len() >= 4).then_some(QuakeFormat::DoomArchive),
    }
}

/// Whether a format is read from its path rather than from bytes in
/// memory: PK3 and PK7 archives, which run to gigabytes.
pub fn reads_path(format: QuakeFormat) -> bool {
    format == QuakeFormat::DoomArchive
}

/// Loads a file whose format `reads_path`, reading only what it needs.
pub fn load_path(format: QuakeFormat, path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    match format {
        QuakeFormat::DoomArchive => {
            let source = doom::Source::open(path)?;
            doom::load_archive(source, limits).map(Asset::Gallery)
        }
        _ => load(format, path, &std::fs::read(path)?, limits),
    }
}

/// `parts` for a file read from its path.
pub fn parts_path(format: QuakeFormat, path: &Path) -> Vec<String> {
    match format {
        QuakeFormat::DoomArchive => doom::Source::open(path).map(doom::archive_parts).unwrap_or_default(),
        _ => std::fs::read(path).map(|b| parts(format, &b)).unwrap_or_default(),
    }
}

/// `load_part` for a file read from its path.
pub fn load_part_path(format: QuakeFormat, path: &Path, limits: &Limits, part: &str) -> Result<Asset, LoadError> {
    match format {
        QuakeFormat::DoomArchive => {
            let source = doom::Source::open(path)?;
            let (mut result, neighbours) = slerp_scene::collect_neighbours(|| {
                doom::load_archive_part(path, source, limits, part).map(Asset::Scene)
            });
            if let Ok(Asset::Scene(scene)) = &mut result {
                scene.info.neighbours = neighbours;
            }
            result
        }
        _ => load_part(format, path, &std::fs::read(path)?, limits, part),
    }
}

/// Loads a file that `sniff` recognised.  `path` is used to find files next
/// to it (WADs, skins, `.lit` lightmaps, Half-Life texture and sequence
/// files) when `limits.neighbours` allows.
pub fn load(format: QuakeFormat, path: &Path, bytes: &[u8], limits: &Limits) -> Result<Asset, LoadError> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{} bytes", bytes.len())));
    }
    // Neighbours are read through `read_neighbour`, which records them.
    let (mut result, neighbours) = slerp_scene::collect_neighbours(|| match format {
        QuakeFormat::Bsp => bsp::load(path, bytes, limits).map(Asset::Scene),
        QuakeFormat::QuakeMdl => mdl::load(path, bytes, limits).map(Asset::Scene),
        QuakeFormat::HalfLifeMdl => hlmdl::load(path, bytes, limits),
        QuakeFormat::SourceMdl => studio::load(path, bytes, limits, 0).map(Asset::Scene),
        QuakeFormat::Md2 => md2::load(path, bytes, limits).map(Asset::Scene),
        QuakeFormat::Md3 => md3::load(path, bytes, limits).map(Asset::Scene),
        QuakeFormat::Wad2 | QuakeFormat::Wad3 => wad::load(bytes, limits).map(Asset::Gallery),
        QuakeFormat::DoomWad => doom::load(bytes, limits).map(Asset::Gallery),
        QuakeFormat::DoomArchive => doom::load_archive(doom::Source::Memory(bytes), limits).map(Asset::Gallery),
        QuakeFormat::UnrealAsset => uasset::load(bytes, limits).map(Asset::Image),
        QuakeFormat::Map => editor::load_map(path, bytes, limits).map(Asset::Scene),
        QuakeFormat::Vmf => editor::load_vmf(path, bytes, limits).map(Asset::Scene),
        QuakeFormat::Sprite => sprite::load_spr(bytes, limits).map(Asset::Gallery),
        QuakeFormat::Sprite2 => sprite::load_sp2(path, bytes, limits).map(Asset::Gallery),
    });
    if let Ok(Asset::Scene(scene)) = &mut result {
        scene.info.neighbours = neighbours;
    }
    result
}

/// The names of a file's parts that load one at a time, in file order: a
/// Doom WAD's maps (MAP01, E1M1, ...), a Source model's skin families
/// ("Skin 1", ...).  Empty for files that are one thing, which is every
/// other format here.
pub fn parts(format: QuakeFormat, bytes: &[u8]) -> Vec<String> {
    match format {
        QuakeFormat::DoomWad => doom::parts(bytes),
        QuakeFormat::SourceMdl => studio::parts(bytes),
        QuakeFormat::DoomArchive => doom::archive_parts(doom::Source::Memory(bytes)),
        _ => Vec::new(),
    }
}

/// Loads one part that `parts` listed: a Doom WAD's map becomes a 3D
/// level, whereas `load` gives the WAD's gallery of title screen and
/// automaps; a Source model's skin family dresses the model in that skin.
pub fn load_part(
    format: QuakeFormat,
    path: &Path,
    bytes: &[u8],
    limits: &Limits,
    part: &str,
) -> Result<Asset, LoadError> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{} bytes", bytes.len())));
    }
    let (mut result, neighbours) = slerp_scene::collect_neighbours(|| match format {
        QuakeFormat::DoomWad => doom::load_part(path, bytes, limits, part).map(Asset::Scene),
        QuakeFormat::DoomArchive => {
            doom::load_archive_part(path, doom::Source::Memory(bytes), limits, part).map(Asset::Scene)
        }
        QuakeFormat::SourceMdl => match studio::family_of(part) {
            Some(family) => studio::load(path, bytes, limits, family).map(Asset::Scene),
            None => Err(LoadError::Unsupported(format!("no part called {part}"))),
        },
        _ => Err(LoadError::Unsupported("this file has no parts".into())),
    });
    if let Ok(Asset::Scene(scene)) = &mut result {
        scene.info.neighbours = neighbours;
    }
    result
}

//endregion

/*┌─────────────────╮
| Shared helpers  |
└─────────────────╯*/
//region Shared helpers

/// Metres per Quake unit: id and Valve built their worlds at about an inch
/// per unit.
pub(crate) const UNIT: f32 = 0.0254;

/// Converts a Quake-space point (Z up, units) to Slerp's (Y up, metres).
pub(crate) fn to_y_up(v: Vec3) -> Vec3 {
    Vec3::new(v.x, v.z, -v.y) * UNIT
}

/// Converts a direction from Quake space without scaling it.
pub(crate) fn dir_to_y_up(v: Vec3) -> Vec3 {
    Vec3::new(v.x, v.z, -v.y)
}

/// `to_y_up` as a transform (a quarter turn about X and the inch scale),
/// for skeletons whose joints stay in Quake space under a converting root.
pub(crate) fn y_up_root() -> slerp_scene::Transform {
    slerp_scene::Transform {
        translation: Vec3::ZERO,
        rotation: slerp_scene::glam::Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
        scale: Vec3::splat(UNIT),
    }
}

/// The largest neighbouring file read (a WAD, skin or sequence file).
const MAX_NEIGHBOUR_BYTES: u64 = 256 << 20;

/// Finds `name` in `dir`, ignoring case, since Quake-era files were made on
/// case-insensitive systems and their names rarely match the disk's.
pub(crate) fn find_in(dir: &Path, name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains(['/', '\\']) {
        return None;
    }
    let exact = dir.join(name);
    if exact.is_file() {
        return Some(exact);
    }
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_str().is_some_and(|n| n.eq_ignore_ascii_case(name)))
        .map(|e| e.path())
        .filter(|p| p.is_file())
}

/// Finds a relative path such as `models/monsters/tank/skin.pcx` below
/// `dir`, matching each part without regard to case.
pub(crate) fn find_relative(dir: &Path, relative: &str) -> Option<PathBuf> {
    let mut at = dir.to_path_buf();
    let parts: Vec<&str> = relative.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".").collect();
    let (last, dirs) = parts.split_last()?;
    for part in dirs {
        if *part == ".." {
            return None;
        }
        let exact = at.join(part);
        at = if exact.is_dir() {
            exact
        } else {
            std::fs::read_dir(&at)
                .ok()?
                .flatten()
                .find(|e| e.file_name().to_str().is_some_and(|n| n.eq_ignore_ascii_case(part)))
                .map(|e| e.path())
                .filter(|p| p.is_dir())?
        };
    }
    find_in(&at, last)
}

/// Reads a neighbouring file, refusing huge ones, and records it in the
/// scene's `Info::neighbours`.
pub(crate) fn read_neighbour(path: &Path) -> Option<Vec<u8>> {
    slerp_scene::record_neighbour(path);
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_NEIGHBOUR_BYTES {
        return None;
    }
    std::fs::read(path).ok()
}

/// Decodes a PNG, JPEG, TGA, PCX or BMP from another file through the
/// textures crate.  A decoder panic is caught and reported like any other
/// failure, because a broken skin must never cost the whole model.
pub(crate) fn decode_image(bytes: &[u8], max: Option<u32>) -> Result<Texture, String> {
    let opts = DecodeOptions { min_size: max, max_size: max, all_mips: false, first_layer_only: true };
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| slerp_scene::textures::decode(bytes, &opts)));
    match run {
        Ok(Ok(t)) => Ok(t),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("the image decoder failed".to_string()),
    }
}

/// The file name of a path stored by a Windows or Unix tool, without its
/// folders.
pub(crate) fn base_name(stored: &str) -> &str {
    stored.rsplit(['/', '\\']).next().unwrap_or(stored)
}

/// Image extensions tried for skins: mods often replace a model's PCX or
/// TGA with a PNG of the same name.
const IMAGE_EXTENSIONS: &[&str] = &["pcx", "png", "tga", "jpg", "jpeg", "bmp"];

/// Finds the image a model names (`models/monsters/tank/skin.pcx`, or a
/// Quake 3 shader name): the path below the model's folder or any folder
/// above it, then the bare file name next to the model, each also with the
/// other image extensions.
pub(crate) fn find_skin(model: &Path, stored: &str) -> Option<PathBuf> {
    let dir = model.parent()?;
    let stored = stored.trim().trim_start_matches(['/', '\\']);
    if stored.is_empty() {
        return None;
    }
    let stem = stored.rsplit_once('.').map_or(stored, |(s, _)| s);
    let mut names = vec![stored.to_string()];
    names.extend(IMAGE_EXTENSIONS.iter().map(|e| format!("{stem}.{e}")));
    for name in &names {
        if let Some(p) = find_in(dir, base_name(name)) {
            return Some(p);
        }
        if let Some(p) = dir.ancestors().take(6).find_map(|a| find_relative(a, name)) {
            return Some(p);
        }
    }
    None
}

/// Guesses the skin of a model that names none: an image with the model's
/// name, then any image whose name starts with it, then Quake 2's
/// `skin.pcx`.
pub(crate) fn guess_skin(model: &Path) -> Option<PathBuf> {
    let dir = model.parent()?;
    let stem = model.file_stem()?.to_str()?.to_ascii_lowercase();
    for e in IMAGE_EXTENSIONS {
        if let Some(p) = find_in(dir, &format!("{stem}.{e}")) {
            return Some(p);
        }
    }
    let mut prefixed: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            name.starts_with(&stem) && IMAGE_EXTENSIONS.contains(&ext.as_str())
        })
        .collect();
    prefixed.sort();
    prefixed.into_iter().next().or_else(|| find_in(dir, "skin.pcx"))
}

/// Reads and decodes a skin file, describing any failure.
pub(crate) fn load_skin(path: &Path, max: Option<u32>) -> Result<Texture, String> {
    let bytes = read_neighbour(path).ok_or_else(|| format!("can't read {}", path.display()))?;
    decode_image(&bytes, max).map_err(|e| format!("{}: {e}", path.display()))
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_magics() {
        let p = Path::new("x");
        let mut bsp29 = vec![29u8, 0, 0, 0];
        for i in 0..15u32 {
            bsp29.extend_from_slice(&(124 + i * 4).to_le_bytes());
            bsp29.extend_from_slice(&4u32.to_le_bytes());
        }
        assert_eq!(sniff(p, &bsp29), Some(QuakeFormat::Bsp));
        let mut not_bsp = bsp29.clone();
        not_bsp[8..12].copy_from_slice(&(-5i32).to_le_bytes());
        assert_eq!(sniff(p, &not_bsp), None);
        assert_eq!(sniff(p, b"BSP2...."), Some(QuakeFormat::Bsp));
        assert_eq!(sniff(p, b"IDPO\x06\0\0\0"), Some(QuakeFormat::QuakeMdl));
        assert_eq!(sniff(p, b"IDST\x0a\0\0\0"), Some(QuakeFormat::HalfLifeMdl));
        assert_eq!(sniff(p, b"IDST\x31\0\0\0"), Some(QuakeFormat::SourceMdl), "Source engine models");
        assert_eq!(sniff(p, b"IDST\x35\0\0\0"), Some(QuakeFormat::SourceMdl), "Titanfall's, refused when loaded");
        assert_eq!(sniff(p, b"IDST\x2b\0\0\0"), None, "older than Half-Life 2");
        assert_eq!(sniff(p, b"IDP2\x08\0\0\0"), Some(QuakeFormat::Md2));
        assert_eq!(sniff(p, b"IDP3\x0f\0\0\0"), Some(QuakeFormat::Md3));
        assert_eq!(sniff(p, b"WAD2\0\0\0\0"), Some(QuakeFormat::Wad2));
        assert_eq!(sniff(p, b"WAD3\0\0\0\0"), Some(QuakeFormat::Wad3));
        assert_eq!(sniff(p, b"PWAD\x01\0\0\0\x0c\0\0\0"), Some(QuakeFormat::DoomWad));
        assert_eq!(sniff(p, b"\xc1\x83\x2a\x9e\xf9\xff\xff\xff"), Some(QuakeFormat::UnrealAsset));
        assert_eq!(sniff(p, b"MDL5\0\0\0\0"), None);
        assert_eq!(sniff(p, b"ID"), None);
    }

    /// Every downloaded sample goes through `sniff` and `load` as the core
    /// crate will call them.
    #[test]
    fn dispatch_real_files() {
        let samples = [
            ("quake-extra/libquake/maps/start.bsp", QuakeFormat::Bsp),
            ("quake-extra/libquake/progs/player.mdl", QuakeFormat::QuakeMdl),
            ("quake-extra/hlmdl/man.mdl", QuakeFormat::HalfLifeMdl),
            ("quake-extra/hlmdl/manT.mdl", QuakeFormat::HalfLifeMdl),
            ("quake-extra/md2/faerie.md2", QuakeFormat::Md2),
            ("quake-extra/md3/watercan.md3", QuakeFormat::Md3),
            ("quake-extra/wads/lq_tech.wad", QuakeFormat::Wad2),
            ("doom/freedoom/freedoom2.wad", QuakeFormat::DoomWad),
            ("quake-extra/uasset/ue4/1M_Cube.uasset", QuakeFormat::UnrealAsset),
        ];
        for (rel, want) in samples {
            let Some(p) = testutil::asset(rel) else { continue };
            let bytes = std::fs::read(&p).unwrap();
            assert_eq!(sniff(&p, &bytes[..bytes.len().min(256)]), Some(want), "{rel}");
            let asset = load(want, &p, &bytes, &Limits::thumbnail()).unwrap_or_else(|e| panic!("{rel}: {e}"));
            let kind = match asset {
                Asset::Scene(s) => format!("scene of {} triangles", s.triangle_count()),
                Asset::Gallery(g) => format!("gallery of {}", g.len()),
                Asset::Image(t) => format!("{}x{} image", t.width(), t.height()),
                Asset::Material(_) => "material".into(),
                _ => "2D picture".into(),
            };
            eprintln!("{rel}: {kind}");
        }
        // Not ours: the sequence-group file of a Half-Life model, and a
        // Quake palette.
        for rel in ["quake-extra/hlmdl/man01.mdl", "quake-extra/libquake/gfx/palette.lmp"] {
            let Some(p) = testutil::asset(rel) else { continue };
            assert_eq!(sniff(&p, &std::fs::read(&p).unwrap()), None, "{rel}");
        }
        let tiny = Limits { max_file_bytes: 10, ..Limits::viewer() };
        assert!(matches!(
            load(QuakeFormat::Wad2, Path::new("x"), b"WAD2\0\0\0\0\0\0\0\0", &tiny),
            Err(LoadError::TooLarge(_))
        ));
    }

    /// Load times with thumbnail and viewer limits: the median of 15 runs
    /// per file.  Run with `cargo test --release -- --ignored --nocapture
    /// bench`.
    #[test]
    #[ignore]
    fn bench() {
        let samples = [
            ("quake-extra/libquake/maps/start.bsp", QuakeFormat::Bsp),
            ("quake-extra/libquake/maps/lq_e0m8.bsp", QuakeFormat::Bsp),
            ("quake-extra/libquake/progs/player.mdl", QuakeFormat::QuakeMdl),
            ("quake-extra/hlmdl/chrome_sphere.mdl", QuakeFormat::HalfLifeMdl),
            ("quake-extra/md2/faerie.md2", QuakeFormat::Md2),
            ("quake-extra/md3/q3root/models/mapobjects/kt_kubalwagon/european_fnt_v2.md3", QuakeFormat::Md3),
            ("quake-extra/wads/lq_medieval.wad", QuakeFormat::Wad2),
            ("doom/freedoom/freedoom2.wad", QuakeFormat::DoomWad),
            ("quake-extra/uasset/ue4/1M_Cube.uasset", QuakeFormat::UnrealAsset),
            ("quake/avix_qcastle/avix_qcastle.map", QuakeFormat::Map),
            ("quake/signati/signati.map", QuakeFormat::Map),
            ("quake/ericw-testmaps/hl_currents.map", QuakeFormat::Map),
            ("source/hl2sb-mapsrc/sb_wasteland.vmf", QuakeFormat::Vmf),
            ("source/valve/sdk_pl_goldrush.vmf", QuakeFormat::Vmf),
        ];
        let mut paths: Vec<(PathBuf, QuakeFormat)> =
            samples.iter().filter_map(|(rel, f)| testutil::asset(rel).map(|p| (p, *f))).collect();
        paths.extend(testutil::external(testutil::chalk!("local/calib/hall.bsp")).map(|p| (p, QuakeFormat::Bsp)));
        for (p, format) in paths {
            let bytes = std::fs::read(&p).unwrap();
            let time = |limits: &Limits| {
                let mut runs: Vec<f64> = (0..15)
                    .map(|_| {
                        let t = std::time::Instant::now();
                        load(format, &p, &bytes, limits).unwrap();
                        t.elapsed().as_secs_f64() * 1000.0
                    })
                    .collect();
                runs.sort_by(f64::total_cmp);
                runs[runs.len() / 2]
            };
            let viewer = time(&Limits::viewer());
            let thumb = time(&Limits::thumbnail());
            let name = p.file_name().unwrap().to_string_lossy();
            eprintln!("{name:32} {:>8} KB  thumbnail {thumb:7.2} ms  viewer {viewer:7.2} ms", bytes.len() / 1024);
        }
    }

    #[test]
    fn y_up_conversion() {
        // Quake +Z (up) becomes +Y; Quake +Y (left) becomes -Z.
        assert_eq!(to_y_up(Vec3::new(0.0, 0.0, 100.0)), Vec3::new(0.0, 2.54, 0.0));
        assert_eq!(dir_to_y_up(Vec3::Y), Vec3::new(0.0, 0.0, -1.0));
        let m = y_up_root().matrix();
        assert!(
            (m.transform_point3(Vec3::new(10.0, 20.0, 30.0)) - to_y_up(Vec3::new(10.0, 20.0, 30.0))).length() < 1e-5
        );
    }
}
