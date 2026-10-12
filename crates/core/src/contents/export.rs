//! Turning an item into the bytes of a file: as stored, or converted with
//! the decoders the loaders use.

use super::{Contents, ContentsError, Format, Item, ItemId, Result, Source};
use slerp_archive::{Archive, DataKind, Entry, GroupKind};
use slerp_quake::convert::{self, Image, ResourceFile};

/// The formats an archive entry can be saved as, from what it is: its raw
/// bytes always, and for Doom and Quake pictures, flats, textures and
/// palettes a PNG, for DMX sounds a WAV, and for MUS music a MIDI file.
/// Images, sounds and music that are already in a normal format (PNG, JPEG,
/// TGA, DDS, KTX2, GIF, WAV, OGG, MIDI) copy unchanged and offer only raw.
pub fn convert_options(entry: &Entry) -> Vec<Format> {
    let mut formats = vec![Format::Raw];
    match entry.data {
        DataKind::DoomPicture
        | DataKind::DoomFlat
        | DataKind::QuakePicture
        | DataKind::Miptex
        | DataKind::Wad3Miptex
        | DataKind::Wad3Picture
        | DataKind::Vtf
        | DataKind::Playpal
        | DataKind::Colormap
        | DataKind::Palette768 => formats.push(Format::Png),
        DataKind::DmxSound => formats.push(Format::Wav),
        DataKind::Mus => formats.push(Format::Mid),
        _ => {}
    }
    formats
}

/// Saves one archive entry as `format`: its file name and bytes.  Pictures
/// are drawn with the archive's own palette (PLAYPAL for Doom, `palette.lmp`
/// for Quake), else with Freedoom's or Quake's.  [`Contents::export`] is the
/// same with the palettes found once, and handles derived items.
pub fn export(archive: &Archive, entry: &Entry, format: Format) -> Result<(String, Vec<u8>)> {
    let palettes = super::Palettes {
        doom: super::find_palette(archive, &["PLAYPAL"], DataKind::Playpal, convert::default_doom_palette()),
        quake: super::find_palette(
            archive,
            &["gfx/palette.lmp", "palette"],
            DataKind::Palette768,
            convert::default_quake_palette(),
        ),
    };
    export_entry(archive, entry, format, &palettes)
}

pub(super) fn export_entry(
    archive: &Archive,
    entry: &Entry,
    format: Format,
    palettes: &super::Palettes,
) -> Result<(String, Vec<u8>)> {
    let name = entry.name();
    match format {
        Format::Raw | Format::Auto => Ok((raw_name(name, entry.data), archive.read(entry)?)),
        Format::Png | Format::Wav | Format::Mid => {
            let bytes = archive.read(entry)?;
            let out = convert_bytes(entry.data, &bytes, format, palettes)?;
            Ok((converted_name(name, format), out))
        }
        Format::Wad => map_wad(archive, entry),
        Format::Exr | Format::Glb => {
            Err(ContentsError::Unsupported(format!("{name} can't be saved as {}", format.name())))
        }
    }
}

/// Converts the bytes of a `data` entry to `format`.
pub(super) fn convert_bytes(
    data: DataKind,
    bytes: &[u8],
    format: Format,
    palettes: &super::Palettes,
) -> Result<Vec<u8>> {
    let failed = |e: slerp_scene::LoadError| ContentsError::Convert(e.to_string());
    match (format, data) {
        (Format::Png, _) => png_bytes(&image_of(data, bytes, palettes)?),
        (Format::Wav, DataKind::DmxSound) => convert::dmx_to_wav(bytes).map_err(failed),
        (Format::Mid, DataKind::Mus) => convert::mus_to_midi(bytes).map_err(failed),
        _ => Err(ContentsError::Unsupported(format!("this can't be saved as {}", format.name()))),
    }
}

/// Decodes the bytes of a picture-like entry.
pub(super) fn image_of(data: DataKind, bytes: &[u8], palettes: &super::Palettes) -> Result<Image> {
    let failed = |e: slerp_scene::LoadError| ContentsError::Convert(e.to_string());
    match data {
        DataKind::DoomPicture => convert::doom_picture(bytes, &palettes.doom).map_err(failed),
        DataKind::DoomFlat => convert::doom_flat(bytes, &palettes.doom).map_err(failed),
        DataKind::QuakePicture => convert::quake_picture(bytes, &palettes.quake).map_err(failed),
        DataKind::Miptex => convert::miptex(bytes, &palettes.quake, false).map_err(failed),
        DataKind::Wad3Miptex => convert::miptex(bytes, &palettes.quake, true).map_err(failed),
        DataKind::Wad3Picture => convert::wad3_picture(bytes).map_err(failed),
        DataKind::Vtf => convert::vtf_image(bytes).map_err(failed),
        DataKind::Playpal => convert::playpal_image(bytes).map_err(failed),
        DataKind::Colormap => convert::colormap_image(bytes, &palettes.doom).map_err(failed),
        DataKind::Palette768 => convert::palette_image(bytes).map_err(failed),
        _ => Err(ContentsError::Unsupported("this can't be saved as a PNG".into())),
    }
}

/// An image as a PNG file.  Doom's offsets go in a `grAb` chunk, as SLADE
/// and GZDoom write them.
pub fn png_bytes(image: &Image) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(image.rgba.len() / 2 + 128);
    let mut encoder = png::Encoder::new(&mut out, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let bad = |e: png::EncodingError| ContentsError::Convert(e.to_string());
    let mut writer = encoder.write_header().map_err(bad)?;
    if let Some((x, y)) = image.offset {
        let mut chunk = [0u8; 8];
        chunk[..4].copy_from_slice(&x.to_be_bytes());
        chunk[4..].copy_from_slice(&y.to_be_bytes());
        writer.write_chunk(png::chunk::ChunkType(*b"grAb"), &chunk).map_err(bad)?;
    }
    writer.write_image_data(&image.rgba).map_err(bad)?;
    writer.finish().map_err(bad)?;
    Ok(out)
}

/// The offsets in a PNG's `grAb` chunk, if it has one (for tests and for
/// reading back what was written).
pub fn png_offsets(png: &[u8]) -> Option<(i32, i32)> {
    let mut at = 8;
    while at + 8 <= png.len() {
        let len = u32::from_be_bytes(png[at..at + 4].try_into().ok()?) as usize;
        let kind = &png[at + 4..at + 8];
        if kind == b"grAb" && len == 8 {
            let d = png.get(at + 8..at + 16)?;
            return Some((i32::from_be_bytes(d[..4].try_into().ok()?), i32::from_be_bytes(d[4..].try_into().ok()?)));
        }
        if kind == b"IDAT" {
            return None;
        }
        at += 12 + len;
    }
    None
}

/// Whether a name already has a file extension.
fn has_extension(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && !ext.is_empty() && ext.len() <= 8 && ext.chars().all(|c| c.is_ascii_alphanumeric())
    })
}

/// The file name for an entry saved as stored: lumps that have no
/// extension get the one their format uses.
pub(super) fn raw_file_name(entry: &Entry) -> String {
    raw_name(entry.name(), entry.data)
}

fn raw_name(name: &str, data: DataKind) -> String {
    if has_extension(name) { name.to_string() } else { format!("{name}.{}", data.extension()) }
}

/// The file name for a converted entry: the name without a `.lmp` or similar
/// extension, and the new one.
fn converted_name(name: &str, format: Format) -> String {
    let stem = match name.rsplit_once('.') {
        Some((stem, ext)) if matches!(ext.to_ascii_lowercase().as_str(), "lmp" | "mus" | "raw" | "dmx" | "flat") => {
            stem
        }
        _ => name,
    };
    format!("{stem}.{}", format.extension())
}

/// A map saved as a standalone PWAD: its marker and the lumps after it.
fn map_wad(archive: &Archive, marker: &Entry) -> Result<(String, Vec<u8>)> {
    let group = archive
        .groups()
        .iter()
        .find(|g| g.kind == GroupKind::Map && g.first == marker.index)
        .ok_or_else(|| ContentsError::Unsupported("this isn't a map's marker".into()))?;
    let lumps: Vec<&Entry> = archive.entries()[group.first..=group.last].iter().collect();
    let mut out = Vec::new();
    out.extend_from_slice(b"PWAD");
    out.extend_from_slice(&(lumps.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    let mut table = Vec::with_capacity(lumps.len() * 16);
    for e in &lumps {
        let data = archive.read(e)?;
        table.extend_from_slice(&(if data.is_empty() { 0 } else { out.len() as u32 }).to_le_bytes());
        table.extend_from_slice(&(data.len() as u32).to_le_bytes());
        let mut n = [0u8; 8];
        let name = e.name().as_bytes();
        n[..name.len().min(8)].copy_from_slice(&name[..name.len().min(8)]);
        table.extend_from_slice(&n);
        out.extend_from_slice(&data);
    }
    let dir = out.len() as u32;
    out[8..12].copy_from_slice(&dir.to_le_bytes());
    out.extend_from_slice(&table);
    Ok((format!("{}.wad", marker.name()), out))
}

impl Contents {
    /// Saves an item as `format` ([`Item::resolve`] decides what a request
    /// the item can't meet, or `Auto`, means): the file name and bytes.
    pub fn export(&self, id: ItemId, format: Format) -> Result<(String, Vec<u8>)> {
        self.export_with(id, &super::ExtractOptions { format, ..Default::default() })
    }

    /// As [`export`](Self::export), with the options GLB files take: a
    /// mesh or node moved to the origin, a clip with its skeleton alone.
    pub fn export_with(&self, id: ItemId, options: &super::ExtractOptions) -> Result<(String, Vec<u8>)> {
        let item = self.item(id).ok_or(ContentsError::NoSuchItem)?;
        let format = item.resolve(options.format);
        match &item.source {
            Source::Entry(i) => {
                let archive = self.archive.as_ref().ok_or(ContentsError::NoSuchItem)?;
                let entry = archive.entry(*i).ok_or(ContentsError::NoSuchItem)?;
                export_entry(archive, entry, format, self.palettes())
            }
            Source::Composite { name, .. } => {
                let image = self.render_composites(&[name.as_str()])?.pop().flatten();
                let image = image
                    .ok_or_else(|| ContentsError::Convert(format!("{name} couldn't be composed from its patches")))?;
                Ok((format!("{name}.png"), png_bytes(&image)?))
            }
            Source::Piece(piece) => super::save::export(self, item, piece, format, options),
        }
    }

    /// The name an item gets when saved as `format`, without reading it.
    pub fn file_name(&self, id: ItemId, format: Format) -> Option<String> {
        let item = self.item(id)?;
        let format = item.resolve(format);
        Some(match (&item.source, format) {
            (Source::Piece(p), f) => super::save::file_name(self, item, p, f),
            (Source::Entry(i), Format::Raw | Format::Auto) => {
                raw_name(item.name(), self.archive.as_ref()?.entry(*i)?.data)
            }
            (Source::Entry(_), Format::Wad) => format!("{}.wad", item.name()),
            (_, f) => converted_name(item.name(), f),
        })
    }

    /// Composes textures by name over the file's resource stack, all in one
    /// pass (building the stack costs more than drawing a texture).  An
    /// entry is `None` for a texture that can't be composed.
    pub fn render_composites(&self, names: &[&str]) -> Result<Vec<Option<Image>>> {
        let near = self.path();
        let render = |file: ResourceFile| {
            convert::with_composites(file, near, |c| names.iter().map(|n| c.render(n)).collect::<Vec<_>>())
                .map_err(|e| ContentsError::Convert(e.to_string()))
        };
        let archive = self.archive.as_ref().ok_or(ContentsError::NoSuchItem)?;
        match archive.kind() {
            slerp_archive::ArchiveKind::DoomWad => {
                let bytes = self.wad_bytes()?;
                render(ResourceFile::Wad(bytes))
            }
            slerp_archive::ArchiveKind::Zip | slerp_archive::ArchiveKind::SevenZip => match near {
                Some(path) => render(ResourceFile::Archive(path)),
                None => Err(ContentsError::Unsupported("composite textures need the archive on disk".into())),
            },
            _ => Err(ContentsError::Unsupported("this archive has no composite textures".into())),
        }
    }

    /// The whole WAD in memory, read once.
    fn wad_bytes(&self) -> Result<&[u8]> {
        if let Some(b) = self.wad.get() {
            return Ok(b);
        }
        let bytes = self.archive.as_ref().ok_or(ContentsError::NoSuchItem)?.source_bytes()?;
        Ok(self.wad.get_or_init(|| bytes))
    }
}

impl Item {
    /// Whether this item is the marker of a map folder.
    pub fn is_map(&self) -> bool {
        self.formats.contains(&Format::Wad)
    }
}
