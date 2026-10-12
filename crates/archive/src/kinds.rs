//! What an entry probably is, guessed from where it sits, what it is called
//! and its first bytes.  The guess decides the icon the panel shows and the
//! formats an entry can be saved as; the decoders still check the bytes they
//! are given, so a wrong guess costs a failed conversion, never a crash.

/// A coarse class of entry, for icons, colours and filters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// A picture of any format: a Doom patch, a Quake status-bar picture, a
    /// PNG, a TGA.
    Picture,
    /// A Doom flat: a raw 64x64 (or larger) square of palette indices.
    Flat,
    Sprite,
    Sound,
    Music,
    /// A wall texture (Quake and Half-Life miptex) or the lists that define
    /// Doom's composite textures.
    Texture,
    /// A map's marker or one of the lumps that make it up.
    Map,
    /// A colour palette or a colour map.
    Palette,
    Text,
    /// A 3D model, or a sprite model.
    Model,
    /// A namespace marker such as `S_START`, which holds no data.
    Marker,
    /// Another archive.
    Archive,
    Other,
}

impl EntryKind {
    /// A word for lists and the command line.
    pub fn label(self) -> &'static str {
        match self {
            EntryKind::Picture => "picture",
            EntryKind::Flat => "flat",
            EntryKind::Sprite => "sprite",
            EntryKind::Sound => "sound",
            EntryKind::Music => "music",
            EntryKind::Texture => "texture",
            EntryKind::Map => "map",
            EntryKind::Palette => "palette",
            EntryKind::Text => "text",
            EntryKind::Model => "model",
            EntryKind::Marker => "marker",
            EntryKind::Archive => "archive",
            EntryKind::Other => "other",
        }
    }
}

/// The format of an entry's bytes, as far as the guess goes.  The converters
/// read it to know which decoder to try.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DataKind {
    Unknown,
    /// Doom's column-and-post picture format (patches, sprites, menu and
    /// status-bar graphics).
    DoomPicture,
    /// Raw palette indices, 64 wide (or square when larger).
    DoomFlat,
    /// Quake's status-bar and menu pictures: width, height, indices.
    QuakePicture,
    /// A Quake miptex texture (indices in four mip levels, palette outside).
    Miptex,
    /// A Half-Life miptex texture, which carries its own palette.
    Wad3Miptex,
    /// A Half-Life picture with its own palette.
    Wad3Picture,
    Wad3Font,
    Png,
    Jpeg,
    Gif,
    Bmp,
    Tga,
    Dds,
    Ktx2,
    Pcx,
    Webp,
    /// Valve's texture format.
    Vtf,
    /// Doom's DMX digital sound.
    DmxSound,
    /// Doom's PC speaker sound.
    SpeakerSound,
    Mus,
    Midi,
    Wav,
    Ogg,
    Mp3,
    Flac,
    /// Doom's PLAYPAL: fourteen 768-byte palettes.
    Playpal,
    /// Doom's COLORMAP: 34 light levels of 256 palette indices.
    Colormap,
    /// A bare 768-byte palette (Quake's `palette.lmp`, a WAD2 `@` lump).
    Palette768,
    /// PNAMES, the patch names composite textures refer to.
    Pnames,
    /// TEXTURE1, TEXTURE2 and TEXTURE3: composite texture definitions.
    TextureDefs,
    /// ZDoom's TEXTURES lump.
    ZdoomTextures,
    Text,
    Mdl,
    Md2,
    Md3,
    Bsp,
    Sprite2,
    Gltf,
    /// A Doom map's marker lump, or a lump that belongs to one.
    MapLump,
    /// A namespace marker.
    Marker,
    /// Another archive.
    Archive,
    Empty,
}

impl DataKind {
    /// The coarse class, given the namespace the entry sits in.
    pub fn kind(self, namespace: Option<Namespace>) -> EntryKind {
        use DataKind::*;
        match self {
            DoomPicture | Png | Jpeg | Gif | Bmp | Tga | Dds | Ktx2 | Pcx | Webp | Vtf | QuakePicture | Wad3Picture
            | Wad3Font => match namespace {
                Some(Namespace::Sprites) => EntryKind::Sprite,
                _ => EntryKind::Picture,
            },
            DoomFlat => EntryKind::Flat,
            Miptex | Wad3Miptex | TextureDefs | ZdoomTextures | Pnames => EntryKind::Texture,
            DmxSound | SpeakerSound | Wav | Ogg | Mp3 | Flac => EntryKind::Sound,
            Mus | Midi => EntryKind::Music,
            Playpal | Colormap | Palette768 => EntryKind::Palette,
            Text => EntryKind::Text,
            Mdl | Md2 | Md3 | Sprite2 | Gltf => EntryKind::Model,
            Bsp | MapLump => EntryKind::Map,
            Marker => EntryKind::Marker,
            Archive => EntryKind::Archive,
            Unknown | Empty => EntryKind::Other,
        }
    }

    /// The extension a raw export gets when the entry's name has none.
    pub fn extension(self) -> &'static str {
        use DataKind::*;
        match self {
            Png => "png",
            Jpeg => "jpg",
            Gif => "gif",
            Bmp => "bmp",
            Tga => "tga",
            Dds => "dds",
            Ktx2 => "ktx2",
            Pcx => "pcx",
            Webp => "webp",
            Vtf => "vtf",
            Mus => "mus",
            Midi => "mid",
            Wav => "wav",
            Ogg => "ogg",
            Mp3 => "mp3",
            Flac => "flac",
            Text | ZdoomTextures => "txt",
            Mdl => "mdl",
            Md2 => "md2",
            Md3 => "md3",
            Bsp => "bsp",
            Sprite2 => "spr",
            Gltf => "glb",
            Archive => "wad",
            _ => "lmp",
        }
    }
}

/// Doom's lump namespaces, which a WAD marks with `X_START` and `X_END`
/// lumps and an archive with its top folders.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Namespace {
    Sprites,
    Flats,
    Patches,
    /// `TX_START`: pictures used as textures as they are.
    Textures,
    Colormaps,
    Acs,
    Voxels,
    /// High-resolution replacements.
    Hires,
}

impl Namespace {
    /// The folder the namespace shows as.
    pub fn folder(self) -> &'static str {
        match self {
            Namespace::Sprites => "Sprites",
            Namespace::Flats => "Flats",
            Namespace::Patches => "Patches",
            Namespace::Textures => "Textures",
            Namespace::Colormaps => "Colormaps",
            Namespace::Acs => "ACS",
            Namespace::Voxels => "Voxels",
            Namespace::Hires => "Hires",
        }
    }

    /// The namespace a PK3 or PK7 top-level folder stands for.
    pub fn of_folder(folder: &str) -> Option<Namespace> {
        Some(match folder.to_ascii_lowercase().as_str() {
            "sprites" => Namespace::Sprites,
            "flats" => Namespace::Flats,
            "patches" => Namespace::Patches,
            "textures" => Namespace::Textures,
            "colormaps" => Namespace::Colormaps,
            "acs" => Namespace::Acs,
            "voxels" => Namespace::Voxels,
            "hires" => Namespace::Hires,
            _ => return None,
        })
    }
}

/*┌─────────────────╮
| Magic           |
└─────────────────╯*/
//region Magic

/// What the first bytes say, for formats that have a signature.
pub fn sniff_magic(head: &[u8]) -> DataKind {
    let starts = |m: &[u8]| head.starts_with(m);
    if head.is_empty() {
        return DataKind::Empty;
    }
    if starts(b"\x89PNG\r\n\x1a\n") {
        DataKind::Png
    } else if starts(b"\xFF\xD8\xFF") {
        DataKind::Jpeg
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        DataKind::Gif
    } else if starts(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        DataKind::Webp
    } else if starts(b"DDS ") {
        DataKind::Dds
    } else if starts(b"VTF\0") {
        DataKind::Vtf
    } else if starts(b"\xABKTX 20\xBB\r\n\x1A\n") {
        DataKind::Ktx2
    } else if starts(b"RIFF") && head.get(8..12) == Some(b"WAVE") {
        DataKind::Wav
    } else if starts(b"OggS") {
        DataKind::Ogg
    } else if starts(b"fLaC") {
        DataKind::Flac
    } else if starts(b"ID3") {
        DataKind::Mp3
    } else if starts(b"MThd") {
        DataKind::Midi
    } else if starts(b"MUS\x1A") {
        DataKind::Mus
    } else if starts(b"IDPO") || starts(b"IDST") {
        DataKind::Mdl
    } else if starts(b"IDP2") {
        DataKind::Md2
    } else if starts(b"IDP3") {
        DataKind::Md3
    } else if starts(b"IDSP") {
        DataKind::Sprite2
    } else if starts(b"glTF") {
        DataKind::Gltf
    } else if starts(b"BSP2") || starts(b"2PSB") || starts(b"IBSP") || starts(b"VBSP") {
        DataKind::Bsp
    } else if matches!(head, [29 | 30, 0, 0, 0, ..]) {
        // Quake and Half-Life BSP versions.
        DataKind::Bsp
    } else if crate::sniff(head).is_some() {
        DataKind::Archive
    } else {
        DataKind::Unknown
    }
}

/// What a file name's extension says.
pub fn from_extension(name: &str) -> DataKind {
    let lower = name.to_ascii_lowercase();
    let Some((_, ext)) = lower.rsplit_once('.') else { return DataKind::Unknown };
    match ext {
        "png" => DataKind::Png,
        "jpg" | "jpeg" => DataKind::Jpeg,
        "gif" => DataKind::Gif,
        "bmp" => DataKind::Bmp,
        "tga" => DataKind::Tga,
        "dds" => DataKind::Dds,
        "ktx2" => DataKind::Ktx2,
        "pcx" => DataKind::Pcx,
        "webp" => DataKind::Webp,
        "vtf" => DataKind::Vtf,
        "wav" => DataKind::Wav,
        "ogg" | "oga" | "opus" => DataKind::Ogg,
        "mp3" => DataKind::Mp3,
        "flac" => DataKind::Flac,
        "mid" | "midi" => DataKind::Midi,
        "mus" => DataKind::Mus,
        "mdl" => DataKind::Mdl,
        "md2" => DataKind::Md2,
        "md3" => DataKind::Md3,
        "bsp" => DataKind::Bsp,
        "spr" | "sp2" => DataKind::Sprite2,
        "glb" | "gltf" => DataKind::Gltf,
        "wad" | "pk3" | "pk7" | "zip" | "pak" | "vpk" | "pck" | "grp" | "7z" => DataKind::Archive,
        "txt" | "cfg" | "rc" | "ini" | "md" | "json" | "xml" | "yml" | "yaml" | "toml" | "csv" | "log" | "lua"
        | "js" | "py" | "sh" | "bat" | "c" | "h" | "cpp" | "cs" | "gd" | "tscn" | "tres" | "godot" | "import"
        | "shader" | "gdshader" | "glsl" | "vmt" | "vmf" | "map" | "qc" | "cmd" | "script" | "acs" | "dec" | "zs"
        | "zsc" | "decorate" | "zscript" | "gldefs" | "mapinfo" | "sndinfo" | "animdefs" | "texinfo" | "menudef"
        | "language" | "skin" | "def" | "arena" | "bot" | "cam" | "ent" | "inc" | "remap" | "cube" | "mtlx" | "mtl"
        | "obj" | "desc" | "tsv" | "html" | "htm" | "css" => DataKind::Text,
        _ => DataKind::Unknown,
    }
}

/// Whether the bytes look like text: no control characters but tabs and
/// line breaks in the first kilobyte, and either valid UTF-8 or mostly
/// ASCII.
pub fn looks_like_text(head: &[u8]) -> bool {
    let head = &head[..head.len().min(1024)];
    if head.is_empty() {
        return false;
    }
    let odd = head.iter().filter(|&&b| b < 32 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0c)).count();
    odd == 0 && (is_utf8_prefix(head) || head.iter().filter(|&&b| b >= 0x80).count() * 20 < head.len())
}

/// Valid UTF-8, allowing the cut at the end to fall inside a character.
fn is_utf8_prefix(head: &[u8]) -> bool {
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    }
}

//endregion

/*┌─────────────────╮
| Doom lumps      |
└─────────────────╯*/
//region Doom lumps

/// Whether the start of a lump (`data`, of `total` bytes in all) is a Doom
/// picture: a header of size and offsets, then one offset per column, all
/// leading into the lump, to posts that end inside it.  Returns (width,
/// height, left offset, top offset).  A Doom picture has no signature, so
/// this checks the structure instead, as far as `data` reaches; random bytes
/// rarely pass.  `data` must hold at least the offset table.
pub fn doom_picture_header(data: &[u8], total: usize) -> Option<(u32, u32, i32, i32)> {
    let w = u16::from_le_bytes(data.get(0..2)?.try_into().ok()?) as usize;
    let h = u16::from_le_bytes(data.get(2..4)?.try_into().ok()?) as usize;
    let left = i16::from_le_bytes(data.get(4..6)?.try_into().ok()?) as i32;
    let top = i16::from_le_bytes(data.get(6..8)?.try_into().ok()?) as i32;
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return None;
    }
    let table_end = 8 + 4 * w;
    if total < table_end + 1 || data.len() < table_end {
        return None;
    }
    // Every column offset points past the table and into the lump.
    let mut offsets = Vec::with_capacity(w);
    for c in 0..w {
        let at = 8 + 4 * c;
        let o = u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize;
        if o < table_end || o >= total {
            return None;
        }
        offsets.push(o);
    }
    // The posts of the first columns walk to their 0xFF end inside the lump,
    // as far as the bytes at hand go.
    for &o in offsets.iter().take(4) {
        let mut at = o;
        while let Some(&top_delta) = data.get(at) {
            if top_delta == 0xFF {
                break;
            }
            let Some(&len) = data.get(at + 1) else { break };
            at += len as usize + 4;
            if at >= total {
                return None;
            }
        }
    }
    Some((w as u32, h as u32, left, top))
}

/// The width and height of a flat of `len` bytes, if it is a plausible
/// flat: Doom's 64x64, Heretic's 64x65 and 64x128 variants, and the larger
/// squares of source ports.
pub fn flat_size(len: usize) -> Option<(u32, u32)> {
    match len {
        4096 => Some((64, 64)),
        4160 => Some((64, 65)),
        8192 => Some((64, 128)),
        16384 => Some((128, 128)),
        65536 => Some((256, 256)),
        _ => None,
    }
}

/// Whether the start of a lump (`data`, of `total` bytes in all) is a DMX
/// digital sound: format 3, a sample rate, and a sample count that fits in
/// the lump.
pub fn is_dmx_sound(data: &[u8], total: usize) -> bool {
    let (Some(format), Some(rate), Some(count)) = (
        data.get(0..2).map(|b| u16::from_le_bytes([b[0], b[1]])),
        data.get(2..4).map(|b| u16::from_le_bytes([b[0], b[1]])),
        data.get(4..8).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
    ) else {
        return false;
    };
    format == 3 && rate >= 1000 && count >= 1 && count as usize <= total.saturating_sub(8)
}

/// Whether the start of a lump is a PC speaker sound: format 0, a tone count
/// that fits in the lump (`total` bytes).
pub fn is_speaker_sound(data: &[u8], total: usize) -> bool {
    let (Some(format), Some(count)) = (
        data.get(0..2).map(|b| u16::from_le_bytes([b[0], b[1]])),
        data.get(2..4).map(|b| u16::from_le_bytes([b[0], b[1]])),
    ) else {
        return false;
    };
    format == 0 && count >= 1 && count as usize + 4 <= total
}

/// The size of a Quake picture: two sizes, then at least that many indices.
pub fn quake_picture_size(data: &[u8]) -> Option<(u32, u32)> {
    let w = u32::from_le_bytes(data.get(0..4)?.try_into().ok()?);
    let h = u32::from_le_bytes(data.get(4..8)?.try_into().ok()?);
    (w > 0 && h > 0 && w <= 4096 && h <= 4096 && data.len() as u64 >= 8 + w as u64 * h as u64).then_some((w, h))
}

/// Whether `data` is a TEXTURE1-style definition lump: a count and a table
/// of offsets that fall inside the lump.
pub fn looks_like_texture_defs(data: &[u8]) -> bool {
    let Some(count) = data.get(0..4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize) else {
        return false;
    };
    if count == 0 || count > 65_536 || data.len() < 4 + count * 4 {
        return false;
    }
    (0..count.min(8)).all(|i| {
        let at = 4 + i * 4;
        let o = u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as usize;
        o >= 4 + count * 4 && o + 22 <= data.len()
    })
}

/// A Doom lump's format from its name, namespace and first bytes (`head`)
/// and its whole `size`.  Names are compared without case.
pub fn doom_lump(name: &str, namespace: Option<Namespace>, head: &[u8], size: u64) -> DataKind {
    let upper = name.to_ascii_uppercase();
    if size == 0 {
        return DataKind::Empty;
    }
    // Names that fix the format.
    match upper.as_str() {
        "PLAYPAL" if size.is_multiple_of(768) => return DataKind::Playpal,
        "COLORMAP" | "COLORMAP2" if size.is_multiple_of(256) => return DataKind::Colormap,
        "PNAMES" => return DataKind::Pnames,
        "TEXTURE1" | "TEXTURE2" | "TEXTURE3" if looks_like_texture_defs(head) => return DataKind::TextureDefs,
        "TEXTURES" if looks_like_text(head) => return DataKind::ZdoomTextures,
        _ => {}
    }
    let magic = sniff_magic(head);
    if !matches!(magic, DataKind::Unknown | DataKind::Empty) {
        return magic;
    }
    match namespace {
        Some(Namespace::Flats) if flat_size(size as usize).is_some() => return DataKind::DoomFlat,
        Some(Namespace::Colormaps) => return DataKind::Colormap,
        Some(Namespace::Sprites | Namespace::Patches | Namespace::Textures | Namespace::Hires)
            if doom_picture_header(head, size as usize).is_some() =>
        {
            return DataKind::DoomPicture;
        }
        _ => {}
    }
    if is_dmx_sound(head, size as usize) {
        return DataKind::DmxSound;
    }
    if is_speaker_sound(head, size as usize) && (upper.starts_with("DP") || upper.starts_with("PC")) {
        return DataKind::SpeakerSound;
    }
    if doom_picture_header(head, size as usize).is_some() {
        return DataKind::DoomPicture;
    }
    if looks_like_text(head) {
        return DataKind::Text;
    }
    DataKind::Unknown
}

//endregion

/// A guess from a path alone, for archives whose entries aren't read when
/// they are listed: the extension first, then the name and namespace for
/// Doom's extension-less lumps.
pub fn from_name(path: &str, namespace: Option<Namespace>) -> DataKind {
    let name = path.rsplit('/').next().unwrap_or(path);
    let by_extension = from_extension(name);
    if by_extension != DataKind::Unknown {
        return by_extension;
    }
    let (stem, has_extension) = match name.rsplit_once('.') {
        Some((s, _)) => (s, true),
        None => (name, false),
    };
    match stem.to_ascii_uppercase().as_str() {
        "PLAYPAL" => return DataKind::Playpal,
        "COLORMAP" => return DataKind::Colormap,
        "PNAMES" => return DataKind::Pnames,
        "TEXTURE1" | "TEXTURE2" | "TEXTURE3" => return DataKind::TextureDefs,
        "TEXTURES" => return DataKind::ZdoomTextures,
        "TEXTMAP" | "MAPINFO" | "ZMAPINFO" | "UMAPINFO" | "SNDINFO" | "DECORATE" | "ZSCRIPT" | "GLDEFS"
        | "ANIMDEFS" | "KEYCONF" | "MENUDEF" | "GAMEINFO" | "LANGUAGE" | "LOCKDEFS" | "TERRAIN" | "MODELDEF"
        | "SBARINFO" | "IWADINFO" | "DMXGUS" => return DataKind::Text,
        _ => {}
    }
    if has_extension {
        return DataKind::Unknown;
    }
    match namespace {
        Some(Namespace::Sprites | Namespace::Patches | Namespace::Textures | Namespace::Hires) => DataKind::DoomPicture,
        Some(Namespace::Flats) => DataKind::DoomFlat,
        Some(Namespace::Colormaps) => DataKind::Colormap,
        _ => DataKind::Unknown,
    }
}
