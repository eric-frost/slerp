//! The four schematic layouts, each read into the same `Build`: regions of
//! cells holding indices into the region's palette of block states.
//!
//! - Sponge `.schem` (WorldEdit 7, versions 1 to 3): a palette of block
//!   state strings and the cells as variable-length integers.
//! - Litematica `.litematic`: named regions, each with a palette of
//!   compounds and the cells packed into 64-bit words at the smallest bit
//!   width the palette needs (at least 2), values spanning word boundaries.
//! - Vanilla structure files `.nbt` (structure blocks, data packs): a size,
//!   a palette and a list of blocks with their positions.
//! - MCEdit and Schematica `.schematic`: numeric block ids and data values
//!   from before Minecraft 1.13, named through `legacy`.
//!
//! Cells are in Minecraft's order, x fastest, then z, then y.

use crate::blocks::BlockState;
use crate::legacy;
use crate::nbt::{self, Compound, Tag};
use slerp_scene::LoadError;
use std::collections::HashMap;

type Result<T> = std::result::Result<T, LoadError>;

/// The largest region side believed.  Minecraft's worlds are 30 million
/// blocks wide, but a schematic's data must fit in memory, which this and
/// the data length checks keep it to.
const MAX_SIDE: i64 = 1 << 16;
/// Palette entries kept per region; cells store 16-bit global ids.
const MAX_PALETTE: usize = 1 << 15;

/// What a schematic file holds.
pub struct Build<'a> {
    pub regions: Vec<Region<'a>>,
    /// Facts for the inspector: the build's name, author, game version.
    pub details: Vec<(String, String)>,
    /// The layout's name, such as "Sponge schematic version 3".
    pub format: String,
}

pub struct Region<'a> {
    pub name: String,
    /// Where the region's lowest corner sits in the build, in blocks.
    pub origin: [i64; 3],
    pub size: [usize; 3],
    pub palette: Vec<BlockState>,
    pub cells: Cells<'a>,
}

/// A region's cells as stored.
pub enum Cells<'a> {
    /// Sponge's variable-length integers.
    Varints(&'a [u8]),
    /// Litematica's bit-packed big-endian longs.
    Packed { longs: &'a [u8], bits: u32 },
    /// MCEdit's numbers: one palette index per (id, data) pair, looked up in
    /// `lut` by `id << 4 | data`.
    Legacy { blocks: &'a [u8], add: Option<&'a [u8]>, data: Option<&'a [u8]>, lut: Vec<u32> },
    /// A structure file's listed blocks: position and palette index.
    Listed(Vec<([u32; 3], u32)>),
}

impl Region<'_> {
    pub fn volume(&self) -> u64 {
        self.size.iter().map(|&s| s as u64).product()
    }

    /// Calls `f(x, y, z, palette index)` for every stored cell, in order.
    /// Cells past the end of damaged data are left out.
    pub fn for_each(&self, mut f: impl FnMut(u32, u32, u32, u32)) {
        let [sx, sy, sz] = self.size.map(|s| s as u64);
        let volume = sx * sy * sz;
        // The position steps along with the index, x fastest, then z, then
        // y, which costs less than dividing the index for every cell.
        let (mut x, mut y, mut z) = (0u32, 0u32, 0u32);
        let (wx, wz) = (sx as u32, sz as u32);
        match &self.cells {
            Cells::Varints(data) => {
                let mut at = 0usize;
                for _ in 0..volume {
                    let Some(&first) = data.get(at) else { return };
                    at += 1;
                    let mut value = (first & 0x7f) as u32;
                    if first & 0x80 != 0 {
                        let mut shift = 7;
                        loop {
                            let Some(&b) = data.get(at) else { return };
                            at += 1;
                            if shift < 32 {
                                value |= ((b & 0x7f) as u32) << shift;
                            }
                            shift += 7;
                            if b & 0x80 == 0 {
                                break;
                            }
                        }
                    }
                    f(x, y, z, value);
                    advance(&mut x, &mut y, &mut z, wx, wz);
                }
            }
            Cells::Packed { longs, bits } => {
                let words = longs.len() / 8;
                let mask = (1u64 << bits) - 1;
                for i in 0..volume {
                    let start = i * *bits as u64;
                    let word = (start >> 6) as usize;
                    let end_word = ((start + *bits as u64 - 1) >> 6) as usize;
                    if end_word >= words {
                        return;
                    }
                    let offset = (start & 63) as u32;
                    let mut v = nbt::long_at(longs, word) >> offset;
                    if end_word != word {
                        v |= nbt::long_at(longs, end_word) << (64 - offset);
                    }
                    f(x, y, z, (v & mask) as u32);
                    advance(&mut x, &mut y, &mut z, wx, wz);
                }
            }
            Cells::Legacy { blocks, add, data, lut } => {
                for i in 0..volume.min(blocks.len() as u64) {
                    let (id, meta) = legacy_cell(blocks, *add, *data, i as usize);
                    f(x, y, z, lut[(id as usize) << 4 | meta as usize]);
                    advance(&mut x, &mut y, &mut z, wx, wz);
                }
            }
            Cells::Listed(list) => {
                for &([x, y, z], s) in list {
                    f(x, y, z, s);
                }
            }
        }
    }
}

/// The next cell's position: x fastest, then z, then y.
#[inline]
fn advance(x: &mut u32, y: &mut u32, z: &mut u32, wx: u32, wz: u32) {
    *x += 1;
    if *x == wx {
        *x = 0;
        *z += 1;
        if *z == wz {
            *z = 0;
            *y += 1;
        }
    }
}

/// Cell `i`'s block id and data value: the id's low byte, then the high
/// nibble from `AddBlocks` (two cells per byte, the first in the high
/// nibble), and the data value's low nibble.
fn legacy_cell(blocks: &[u8], add: Option<&[u8]>, data: Option<&[u8]>, i: usize) -> (u16, u8) {
    let mut id = blocks[i] as u16;
    if let Some(add) = add
        && let Some(&b) = add.get(i >> 1)
    {
        let nibble = if i & 1 == 0 { b >> 4 } else { b & 15 };
        id |= (nibble as u16) << 8;
    }
    (id, data.and_then(|d| d.get(i)).map_or(0, |d| d & 15))
}

/// Keys left out when parsing: entities, block entities, ticks and biomes,
/// which can hold most of a file's values and nothing that's drawn.
pub const SKIP: &[&str] = &[
    "Entities",
    "TileEntities",
    "BlockEntities",
    "entities",
    "nbt",
    "PendingBlockTicks",
    "PendingFluidTicks",
    "PendingTicks",
    "TileTicks",
    "Biomes",
    "BiomeData",
    "BiomePalette",
    "PreviewImageData",
];

/// Reads whichever layout `root` has, by its keys.
pub fn read<'a>(root: &Compound<'a>) -> Result<Build<'a>> {
    if root.has("Regions") && root.has("Metadata") {
        return litematica(root);
    }
    if let Some(inner) = root.compound("Schematic")
        && inner.has("Blocks")
    {
        return sponge(inner);
    }
    if root.has("Palette") && root.has("BlockData") || root.compound("Blocks").is_some() {
        return sponge(root);
    }
    if root.bytes("Blocks").is_some() && root.has("Width") {
        return mcedit(root);
    }
    if root.has("size") && (root.has("palette") || root.has("palettes")) && root.has("blocks") {
        return structure(root);
    }
    Err(LoadError::NotMine)
}

/// Whether a root holds a vanilla structure: `size`, a palette and
/// `blocks`, since many other files are NBT too.
pub fn is_structure(root: &Compound) -> bool {
    root.has("size") && (root.has("palette") || root.has("palettes")) && root.has("blocks")
}

/// A side length, which must be positive and believable.
fn side(v: Option<i64>, what: &str) -> Result<usize> {
    let v = v.ok_or_else(|| LoadError::invalid(format!("the schematic has no {what}")))?;
    if !(0..=MAX_SIDE).contains(&v) {
        return Err(LoadError::invalid(format!("{what} of {v} blocks")));
    }
    Ok(v as usize)
}

/// A 16-bit size, which Sponge and MCEdit store as signed shorts although
/// they are unsigned.
fn short_side(c: &Compound, key: &str) -> Result<usize> {
    let v = match c.get(key) {
        Some(Tag::Short(s)) => *s as u16 as i64,
        Some(t) => t.as_int().unwrap_or(-1),
        None => -1,
    };
    side(Some(v), key)
}

/// The facts a schematic's `Metadata` names.
fn metadata(c: Option<&Compound>, out: &mut Vec<(String, String)>) {
    let Some(c) = c else { return };
    for (key, label) in [("Name", "Name"), ("Author", "Author"), ("Description", "Description")] {
        if let Some(v) = c.str(key).filter(|v| !v.trim().is_empty()) {
            out.push((label.into(), v.trim().to_string()));
        }
    }
}

fn data_version(v: Option<i64>, out: &mut Vec<(String, String)>) {
    if let Some(v) = v.filter(|&v| v > 0) {
        out.push(("Game version".into(), format!("{} (data version {v})", minecraft_version(v))));
    }
}

/// The Minecraft release a data version belongs to, roughly.
fn minecraft_version(v: i64) -> &'static str {
    const VERSIONS: &[(i64, &str)] = &[
        (4325, "1.21.5 or later"),
        (4189, "1.21.4"),
        (4080, "1.21.2"),
        (3953, "1.21"),
        (3837, "1.20.5"),
        (3698, "1.20.3"),
        (3578, "1.20.2"),
        (3463, "1.20"),
        (3337, "1.19.4"),
        (3218, "1.19.3"),
        (3105, "1.19"),
        (2975, "1.18.2"),
        (2860, "1.18"),
        (2724, "1.17"),
        (2566, "1.16"),
        (2225, "1.15"),
        (1952, "1.14"),
        (1519, "1.13"),
        (1139, "1.12"),
        (819, "1.11"),
        (169, "1.9 or 1.10"),
    ];
    VERSIONS.iter().find(|(d, _)| v >= *d).map_or("before 1.9", |(_, n)| n)
}

/*┌─────────────────╮
| Sponge          |
└─────────────────╯*/
//region Sponge

fn sponge<'a>(c: &Compound<'a>) -> Result<Build<'a>> {
    let version = c.int("Version").unwrap_or(1);
    let size = [short_side(c, "Width")?, short_side(c, "Height")?, short_side(c, "Length")?];
    // Version 3 keeps the palette and data under `Blocks`.
    let (palette_tag, data) = match c.compound("Blocks") {
        Some(b) => (b.compound("Palette"), b.bytes("Data")),
        None => (c.compound("Palette"), c.bytes("BlockData")),
    };
    let palette_tag = palette_tag.ok_or_else(|| LoadError::invalid("the schematic has no palette"))?;
    let data = data.ok_or_else(|| LoadError::invalid("the schematic has no block data"))?;
    if palette_tag.0.len() > MAX_PALETTE {
        return Err(LoadError::invalid("the schematic's palette is too large"));
    }
    // Palette entries map a state string to its index; indices may skip.
    let mut palette = vec![BlockState::air(); palette_tag.0.len()];
    for (state, index) in &palette_tag.0 {
        let Some(i) = index.as_int().and_then(|i| usize::try_from(i).ok()) else { continue };
        if i >= MAX_PALETTE {
            continue;
        }
        if i >= palette.len() {
            palette.resize(i + 1, BlockState::air());
        }
        palette[i] = BlockState::parse(state);
    }
    let volume = size.iter().map(|&s| s as u64).product::<u64>();
    if (data.len() as u64) < volume {
        return Err(LoadError::invalid("the schematic's block data is shorter than its size"));
    }
    let offset = c.get("Offset").and_then(Tag::as_ivec3).unwrap_or([0; 3]);
    let mut details = Vec::new();
    metadata(c.compound("Metadata"), &mut details);
    data_version(c.int("DataVersion"), &mut details);
    Ok(Build {
        regions: vec![Region { name: String::new(), origin: offset, size, palette, cells: Cells::Varints(data) }],
        details,
        format: format!("Sponge schematic version {version}"),
    })
}

//endregion

/*┌─────────────────╮
| Litematica      |
└─────────────────╯*/
//region Litematica

fn litematica<'a>(c: &Compound<'a>) -> Result<Build<'a>> {
    let regions_tag = c.compound("Regions").ok_or_else(|| LoadError::invalid("no regions"))?;
    let xyz = |c: Option<&Compound>| -> Option<[i64; 3]> {
        let c = c?;
        Some([c.int("x")?, c.int("y")?, c.int("z")?])
    };
    let mut regions = Vec::new();
    for (name, tag) in regions_tag.0.iter().take(4096) {
        let Some(r) = tag.as_compound() else { continue };
        let pos = xyz(r.compound("Position")).ok_or_else(|| LoadError::invalid("a region has no position"))?;
        let raw = xyz(r.compound("Size")).ok_or_else(|| LoadError::invalid("a region has no size"))?;
        let mut size = [0usize; 3];
        let mut origin = [0i64; 3];
        for a in 0..3 {
            size[a] = side(Some(raw[a].abs()), "region size")?;
            // A negative size runs from the position back.
            origin[a] = if raw[a] < 0 { pos[a] + raw[a] + 1 } else { pos[a] };
        }
        let list = r.list("BlockStatePalette").ok_or_else(|| LoadError::invalid("a region has no palette"))?;
        if list.len() > MAX_PALETTE {
            return Err(LoadError::invalid("a region's palette is too large"));
        }
        let palette: Vec<BlockState> = list
            .iter()
            .map(|t| t.as_compound().and_then(BlockState::from_nbt).unwrap_or_else(BlockState::air))
            .collect();
        let Some(Tag::LongArray(longs)) = r.get("BlockStates") else {
            return Err(LoadError::invalid("a region has no block states"));
        };
        let bits = bit_width(palette.len());
        let volume = size.iter().map(|&s| s as u64).product::<u64>();
        let needed = (volume * bits as u64).div_ceil(64);
        if (longs.len() / 8) < needed as usize {
            return Err(LoadError::invalid("a region's block states are shorter than its size"));
        }
        regions.push(Region { name: name.to_string(), origin, size, palette, cells: Cells::Packed { longs, bits } });
    }
    let mut details = Vec::new();
    metadata(c.compound("Metadata"), &mut details);
    data_version(c.int("MinecraftDataVersion"), &mut details);
    if regions.len() > 1 {
        details.push(("Regions".into(), regions.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", ")));
    }
    let version = c.int("Version").unwrap_or(0);
    Ok(Build { regions, details, format: format!("Litematica schematic version {version}") })
}

/// Bits per cell in Litematica's packed arrays: enough for the palette's
/// largest index, at least 2.
pub fn bit_width(palette: usize) -> u32 {
    let largest = palette.saturating_sub(1) as u64;
    (64 - largest.leading_zeros()).max(2)
}

//endregion

/*┌─────────────────╮
| Structures      |
└─────────────────╯*/
//region Structures

fn structure<'a>(c: &Compound<'a>) -> Result<Build<'a>> {
    let raw = c.get("size").and_then(Tag::as_ivec3).ok_or_else(|| LoadError::invalid("the structure has no size"))?;
    let size = [side(Some(raw[0]), "width")?, side(Some(raw[1]), "height")?, side(Some(raw[2]), "length")?];
    // Structures with random variants (shipwrecks) list several palettes;
    // the first is shown.
    let list = c
        .list("palette")
        .or_else(|| {
            c.list("palettes")
                .and_then(|p| p.first())
                .and_then(|t| if let Tag::List(l) = t { Some(&l[..]) } else { None })
        })
        .ok_or_else(|| LoadError::invalid("the structure has no palette"))?;
    if list.len() > MAX_PALETTE {
        return Err(LoadError::invalid("the structure's palette is too large"));
    }
    let palette: Vec<BlockState> =
        list.iter().map(|t| t.as_compound().and_then(BlockState::from_nbt).unwrap_or_else(BlockState::air)).collect();
    let mut cells = Vec::new();
    for b in c.list("blocks").unwrap_or(&[]) {
        let Some(b) = b.as_compound() else { continue };
        let (Some(state), Some(pos)) = (b.int("state"), b.get("pos").and_then(Tag::as_ivec3)) else { continue };
        let inside = (0..3).all(|a| pos[a] >= 0 && (pos[a] as u64) < size[a] as u64);
        if inside && state >= 0 && (state as usize) < palette.len() {
            cells.push(([pos[0] as u32, pos[1] as u32, pos[2] as u32], state as u32));
        }
    }
    let mut details = Vec::new();
    data_version(c.int("DataVersion"), &mut details);
    if let Some(author) = c.str("author") {
        details.push(("Author".into(), author.to_string()));
    }
    Ok(Build {
        regions: vec![Region { name: String::new(), origin: [0; 3], size, palette, cells: Cells::Listed(cells) }],
        details,
        format: "Minecraft structure".into(),
    })
}

//endregion

/*┌─────────────────╮
| MCEdit          |
└─────────────────╯*/
//region MCEdit

fn mcedit<'a>(c: &Compound<'a>) -> Result<Build<'a>> {
    let size = [short_side(c, "Width")?, short_side(c, "Height")?, short_side(c, "Length")?];
    let blocks = c.bytes("Blocks").ok_or_else(|| LoadError::invalid("the schematic has no blocks"))?;
    let volume = size.iter().map(|&s| s as u64).product::<u64>();
    if (blocks.len() as u64) < volume {
        return Err(LoadError::invalid("the schematic's blocks are fewer than its size"));
    }
    let add = c.bytes("AddBlocks").or_else(|| c.bytes("Add"));
    let data = c.bytes("Data");
    // Tools that knew mods saved how their ids map to names.
    let mut names: HashMap<u16, String> = HashMap::new();
    if let Some(m) = c.compound("SchematicaMapping") {
        for (name, id) in &m.0 {
            if let Some(id) = id.as_int().and_then(|i| u16::try_from(i).ok()) {
                names.insert(id, name.to_string());
            }
        }
    }
    if let Some(m) = c.compound("BlockIDs") {
        for (id, name) in &m.0 {
            if let (Ok(id), Tag::String(name)) = (id.parse::<u16>(), name) {
                names.insert(id, name.to_string());
            }
        }
    }
    // Every (id, data) pair present becomes a palette entry.
    let mut seen = vec![false; 1 << 16];
    for i in 0..volume as usize {
        let (id, meta) = legacy_cell(blocks, add, data, i);
        seen[(id as usize) << 4 | meta as usize] = true;
    }
    let mut lut = vec![0u32; 1 << 16];
    let mut palette = Vec::new();
    for (key, _) in seen.iter().enumerate().filter(|(_, s)| **s) {
        let (id, meta) = ((key >> 4) as u16, (key & 15) as u8);
        let state = match names.get(&id) {
            // A name the mapping gives that isn't Minecraft's own: its data
            // value means nothing to the table.
            Some(n) if !n.starts_with("minecraft:") => BlockState::parse(n),
            _ => legacy::state(id, meta),
        };
        lut[key] = palette.len() as u32;
        palette.push(state);
        if palette.len() > MAX_PALETTE {
            return Err(LoadError::invalid("the schematic's palette is too large"));
        }
    }
    let mut details = Vec::new();
    if let Some(m) = c.str("Materials") {
        details.push(("Materials".into(), m.to_string()));
    }
    let origin = ["WEOffsetX", "WEOffsetY", "WEOffsetZ"].map(|k| c.int(k).unwrap_or(0));
    Ok(Build {
        regions: vec![Region {
            name: String::new(),
            origin,
            size,
            palette,
            cells: Cells::Legacy { blocks, add, data, lut },
        }],
        details,
        format: "MCEdit schematic".into(),
    })
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    /// Packs values the way Litematica's bit array does: tightly, across
    /// word boundaries.
    pub fn pack(values: &[u32], bits: u32) -> Vec<u8> {
        let words = (values.len() as u64 * bits as u64).div_ceil(64) as usize;
        let mut longs = vec![0u64; words];
        for (i, &v) in values.iter().enumerate() {
            let start = i as u64 * bits as u64;
            let (w, o) = ((start >> 6) as usize, (start & 63) as u32);
            longs[w] |= (v as u64) << o;
            if o + bits > 64 {
                longs[w + 1] |= (v as u64) >> (64 - o);
            }
        }
        longs.iter().flat_map(|l| l.to_be_bytes()).collect()
    }

    #[test]
    fn packed_arrays_at_every_width() {
        for bits in 2..=16u32 {
            let palette = 1usize << bits;
            assert_eq!(bit_width(palette), bits, "a palette of {palette}");
            assert_eq!(bit_width(palette / 2 + 1), bits);
            // An odd volume, so values straddle words at every width.
            let size = [7usize, 3, 5];
            let values: Vec<u32> =
                (0..105u32).map(|i| i.wrapping_mul(2654435761u32.wrapping_mul(bits)) % palette as u32).collect();
            let bytes = pack(&values, bits);
            let region = Region {
                name: String::new(),
                origin: [0; 3],
                size,
                palette: Vec::new(),
                cells: Cells::Packed { longs: &bytes, bits },
            };
            let mut got = Vec::new();
            region.for_each(|x, y, z, v| got.push(((x, y, z), v)));
            assert_eq!(got.len(), 105, "{bits} bits");
            for (i, ((x, y, z), v)) in got.iter().enumerate() {
                assert_eq!(*v, values[i], "{bits} bits, cell {i}");
                assert_eq!((*y as usize * 5 + *z as usize) * 7 + *x as usize, i);
            }
        }
        assert_eq!(bit_width(1), 2);
        assert_eq!(bit_width(0), 2);
        assert_eq!(bit_width(5), 3);
    }

    #[test]
    fn varints_and_legacy_nibbles() {
        // 1, 300 (two bytes), 0, then the data runs out.
        let data = [1u8, 0xac, 0x02, 0];
        let r = Region {
            name: String::new(),
            origin: [0; 3],
            size: [2, 2, 1],
            palette: Vec::new(),
            cells: Cells::Varints(&data),
        };
        let mut got = Vec::new();
        r.for_each(|_, _, _, v| got.push(v));
        assert_eq!(got, [1, 300, 0]);
        // Ids 0x105 and 0x2ff: AddBlocks holds 1 in the high nibble for the
        // first cell and 2 in the low one for the second.
        assert_eq!(legacy_cell(&[5, 0xff], Some(&[0x12]), Some(&[3, 0x1f]), 0), (0x105, 3));
        assert_eq!(legacy_cell(&[5, 0xff], Some(&[0x12]), Some(&[3, 0x1f]), 1), (0x2ff, 15));
    }
}
