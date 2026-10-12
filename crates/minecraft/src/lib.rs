//! Minecraft schematics as block scenes: Sponge `.schem` (WorldEdit,
//! versions 1 to 3), Litematica `.litematic` (every region in place),
//! vanilla structure files `.nbt` and MCEdit/Schematica `.schematic`.
//!
//! Files are NBT (`nbt`), read into regions of palette indices
//! (`layouts`), and drawn as blocks (`mesh`): full cubes greedy-meshed with
//! hidden faces left out, slabs, stairs, fences, walls, panes, plants,
//! carpets, doors and a few others approximated with boxes and crossed
//! quads, everything else a cube.  Colours come from a built-in table
//! (`colors`) with small procedural textures (`patterns`), or from the
//! block textures of a Minecraft jar or resource pack already on the
//! computer (`textures`), read at run time and never copied anywhere.
//! Builds of millions of blocks are thinned for thumbnails.  Litematica
//! files may carry a preview picture taken in the game, which
//! `stored_preview` returns for thumbnails to show instead of a render.
//!
//! Minecraft is Y-up and right-handed like the scene model; a block is a
//! metre.

mod blocks;
mod colors;
mod layouts;
mod legacy;
mod mesh;
mod nbt;
mod patterns;
mod textures;

#[cfg(test)]
mod tests;

pub use textures::{Source as TextureSource, installed_jar};

use slerp_scene::{Limits, LoadError, Scene};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MinecraftFormat {
    /// Sponge `.schem`, as WorldEdit writes it.
    Sponge,
    /// Litematica `.litematic`.
    Litematica,
    /// A vanilla structure (`.nbt`), as structure blocks save them.
    Structure,
    /// MCEdit or Schematica `.schematic`, from before Minecraft 1.13.
    McEdit,
}

/// Structure files larger than this aren't looked into when sniffing.
const MAX_SNIFF_FILE: u64 = 64 << 20;

/// Recognises a schematic by its extension and NBT content: gzip, zlib or
/// a bare compound.  `.nbt` is used for many other kinds of data, so a file
/// of that name is only claimed when its root holds `size`, a palette and
/// `blocks`, which is checked in the start of the file (`head`) and, when
/// that isn't enough, in the whole file at `path`.
pub fn sniff(path: &Path, head: &[u8]) -> Option<MinecraftFormat> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let format = match ext.as_str() {
        "schem" => MinecraftFormat::Sponge,
        "litematic" => MinecraftFormat::Litematica,
        "schematic" => MinecraftFormat::McEdit,
        "nbt" => MinecraftFormat::Structure,
        _ => return None,
    };
    let nbt_like = head.starts_with(&[0x1f, 0x8b]) || nbt::is_zlib(head) || head.first() == Some(&10);
    if !nbt_like {
        return None;
    }
    // The root compound must open the data.
    let start = nbt::decompress_head(head, 1 << 16);
    if start.first() != Some(&10) {
        return None;
    }
    if format != MinecraftFormat::Structure {
        return Some(format);
    }
    let structure = |keys: &[String]| {
        let has = |k: &str| keys.iter().any(|x| x == k);
        has("size") && (has("palette") || has("palettes")) && has("blocks")
    };
    let (keys, complete) = nbt::root_keys(&start);
    if structure(&keys) {
        return Some(format);
    }
    if complete {
        return None;
    }
    let size = std::fs::metadata(path).ok()?.len();
    if size > MAX_SNIFF_FILE {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let data = nbt::decompress(&bytes, 256 << 20).ok()?;
    structure(&nbt::root_keys(&data).0).then_some(format)
}

/// Loads a schematic.  `thumbnail` keeps the grid small enough to mesh in
/// a thumbnail's time, thinning large builds; `textures` says where block
/// textures may come from (looked for only when `limits.neighbours`).
pub fn load(
    format: MinecraftFormat,
    path: &Path,
    bytes: &[u8],
    limits: &Limits,
    textures: &TextureSource,
) -> Result<Scene, LoadError> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{} bytes", bytes.len())));
    }
    // Thumbnails load no animations; that is how they are told apart.
    let thumbnail = !limits.animations;
    let max = if thumbnail { 256 << 20 } else { 1536 << 20 };
    let data = nbt::decompress(bytes, max)?;
    let (_, root) = nbt::parse(&data, layouts::SKIP)?;
    if format == MinecraftFormat::Structure && !layouts::is_structure(&root) {
        return Err(LoadError::NotMine);
    }
    let build = layouts::read(&root)?;
    let pack = if limits.neighbours { textures::Pack::find(path, textures) } else { None };
    let mut scene = mesh::scene(&build, limits, pack.as_ref(), thumbnail)?;
    scene.name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("schematic").to_string();
    Ok(scene)
}

/// The preview picture a schematic carries, if any: Litematica's
/// `Metadata.PreviewImageData`, a square of ARGB pixels taken in the game
/// when the schematic was saved (or later from its browser).  No other
/// layout stores one.  Thumbnails show it rather than drawing the blocks.
pub fn stored_preview(format: MinecraftFormat, bytes: &[u8]) -> Option<slerp_scene::Texture> {
    if format != MinecraftFormat::Litematica {
        return None;
    }
    // Litematica writes its metadata first: the start of the file usually
    // answers, and only otherwise is the whole file inflated.
    let head = nbt::decompress_head(bytes, 2 << 20);
    let full;
    let px = match nbt::root_int_array(&head, "Metadata", "PreviewImageData") {
        Ok(px) => px?,
        Err(()) => {
            full = nbt::decompress(bytes, 256 << 20).ok()?;
            nbt::root_int_array(&full, "Metadata", "PreviewImageData").ok()??
        }
    };
    let count = px.len() / 4;
    let size = (count as f64).sqrt().round() as usize;
    if size == 0 || size > 4096 || size * size != count {
        return None;
    }
    let mut rgba = Vec::with_capacity(count * 4);
    for i in 0..count {
        let [a, r, g, b] = (nbt::int_at(px, i) as u32).to_be_bytes();
        rgba.extend_from_slice(&[r, g, b, a]);
    }
    let mut t =
        slerp_scene::Texture::from_rgba8(size as u32, size as u32, rgba, slerp_scene::textures::ColorSpace::Srgb);
    t.source.container = "Litematica preview".into();
    Some(t)
}
