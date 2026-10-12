//! What to show for an item selected in the Contents panel.  Pictures,
//! flats, textures and palettes are converted to PNG; sounds are drawn as
//! waveforms; formats the viewer already opens (images, models, maps,
//! nested archives) are handed over as the files they are; text is shown as
//! text; the rest gets a card of facts.

use super::export::{export_entry, png_bytes};
use super::{Contents, ContentsError, ItemId, Piece, Result, Source};
use slerp_archive::{Archive, DataKind, Entry};
use slerp_quake::convert::{self, Image};

/// The largest item read to preview it.
const MAX_PREVIEW_BYTES: u64 = 256 << 20;
/// The most text shown.
const MAX_TEXT_BYTES: usize = 256 << 10;
/// The width of a cube map's panorama made for showing.
const PREVIEW_PANORAMA: u32 = 2048;

/// What the viewer should do with an item.
pub enum PreviewKind {
    /// Open this file: its name (with the right extension, so the viewer
    /// recognises it) and bytes.
    File { name: String, bytes: Vec<u8> },
    /// Show this text.
    Text(String),
    /// Show only the facts.
    Info,
}

pub struct Preview {
    pub kind: PreviewKind,
    /// Facts to list beside it: "Type", "Size", "Picture", "Sample rate".
    pub facts: Vec<(String, String)>,
}

impl Contents {
    /// Prepares an item for display.
    pub fn preview(&self, id: ItemId) -> Result<Preview> {
        let item = self.item(id).ok_or(ContentsError::NoSuchItem)?;
        let mut facts: Vec<(String, String)> = vec![("Item".into(), item.path.clone())];
        match &item.source {
            Source::Composite { name, .. } => {
                facts.push(("Type".into(), "Composite texture".into()));
                facts.push(("Made of".into(), item.detail.clone()));
                let image =
                    self.render_composites(&[name.as_str()])?.pop().flatten().ok_or_else(|| {
                        ContentsError::Convert(format!("{name} couldn't be composed from its patches"))
                    })?;
                Ok(Preview {
                    kind: PreviewKind::File { name: format!("{name}.png"), bytes: png_bytes(&image)? },
                    facts,
                })
            }
            Source::Entry(i) => {
                let archive = self.archive.as_ref().ok_or(ContentsError::NoSuchItem)?;
                let entry = archive.entry(*i).ok_or(ContentsError::NoSuchItem)?;
                self.preview_entry(archive, entry, facts)
            }
            Source::Piece(piece) => self.preview_piece(id, piece, facts),
        }
    }

    /// A piece's preview: a texture as the file it was stored as (or a
    /// PNG or EXR of it), anything else as its facts; the viewer shows most
    /// pieces in the loaded file itself (see [`Contents::show`]).
    fn preview_piece(&self, id: ItemId, piece: &Piece, mut facts: Vec<(String, String)>) -> Result<Preview> {
        let item = self.item(id).ok_or(ContentsError::NoSuchItem)?;
        facts.push(("Type".into(), capitalised(item.kind.label())));
        if !item.detail.is_empty() {
            facts.push(("About".into(), item.detail.clone()));
        }
        if let Piece::Panorama { layer } = piece {
            // A smaller one than saving makes, which is all a window shows.
            let t = self.held.picture.as_ref().ok_or(ContentsError::NoSuchItem)?;
            let pano = super::panorama::panorama(t, *layer, PREVIEW_PANORAMA)?;
            let format = item.resolve(super::Format::Auto);
            facts.push(("Saved as".into(), format.label().to_string()));
            let name = self.file_name(id, format).unwrap_or_else(|| "panorama".into());
            let bytes = super::save::picture_bytes(&pano, 0, 0, 0, format)?;
            return Ok(Preview { kind: PreviewKind::File { name, bytes }, facts });
        }
        if let Piece::Texture(_) | Piece::MaterialPreview | Piece::MaterialBall | Piece::SavedPicture = piece {
            let format = item.resolve(super::Format::Auto);
            facts.push((
                "Saved as".into(),
                if format == super::Format::Raw { "the file as stored".into() } else { format.label().to_string() },
            ));
            let (name, bytes) = self.export(id, format)?;
            return Ok(Preview { kind: PreviewKind::File { name, bytes }, facts });
        }
        if item.formats.is_empty() {
            facts
                .push(("Note".into(), "This only means something inside its file, so it can't be saved alone.".into()));
        }
        Ok(Preview { kind: PreviewKind::Info, facts })
    }

    fn preview_entry(&self, archive: &Archive, entry: &Entry, mut facts: Vec<(String, String)>) -> Result<Preview> {
        facts.push(("Type".into(), describe(entry.data).into()));
        facts.push(("Size".into(), format!("{} bytes", entry.size)));
        if entry.stored_size != entry.size {
            facts.push(("Stored as".into(), format!("{} bytes", entry.stored_size)));
        }
        if entry.locked {
            facts.push(("Note".into(), "This can't be read: it is encrypted or lies outside the file.".into()));
            return Ok(Preview { kind: PreviewKind::Info, facts });
        }
        if entry.size > MAX_PREVIEW_BYTES {
            facts.push(("Note".into(), "Too large to preview here; Extract it instead.".into()));
            return Ok(Preview { kind: PreviewKind::Info, facts });
        }
        let name = entry.name();
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
            | DataKind::Palette768 => {
                let bytes = archive.read(entry)?;
                let image = super::export::image_of(entry.data, &bytes, self.palettes())?;
                facts.push(("Picture".into(), format!("{} x {} pixels", image.width, image.height)));
                if let Some((x, y)) = image.offset {
                    facts.push(("Offset".into(), format!("{x}, {y}")));
                }
                let (file, _) = export_entry(archive, entry, super::Format::Png, self.palettes())
                    .unwrap_or_else(|_| (format!("{name}.png"), Vec::new()));
                Ok(Preview { kind: PreviewKind::File { name: file, bytes: png_bytes(&image)? }, facts })
            }
            DataKind::DmxSound => {
                let bytes = archive.read(entry)?;
                let (info, range) = convert::dmx_info(&bytes).map_err(|e| ContentsError::Convert(e.to_string()))?;
                facts.push(("Sample rate".into(), format!("{} Hz", info.sample_rate)));
                facts.push(("Length".into(), seconds(info.samples as f64 / info.sample_rate as f64)));
                let peaks = peaks_u8(&bytes[range]);
                Ok(Preview {
                    kind: PreviewKind::File { name: format!("{name}.png"), bytes: waveform_png(&peaks)? },
                    facts,
                })
            }
            DataKind::Wav => {
                let bytes = archive.read(entry)?;
                match wav_peaks(&bytes, &mut facts) {
                    Some(peaks) => Ok(Preview {
                        kind: PreviewKind::File { name: format!("{name}.png"), bytes: waveform_png(&peaks)? },
                        facts,
                    }),
                    None => Ok(Preview { kind: PreviewKind::Info, facts }),
                }
            }
            DataKind::Text | DataKind::ZdoomTextures => {
                let head = archive.read_head(entry, MAX_TEXT_BYTES)?;
                Ok(Preview { kind: PreviewKind::Text(String::from_utf8_lossy(&head).into_owned()), facts })
            }
            DataKind::Png
            | DataKind::Jpeg
            | DataKind::Gif
            | DataKind::Bmp
            | DataKind::Tga
            | DataKind::Dds
            | DataKind::Ktx2
            | DataKind::Pcx
            | DataKind::Webp
            | DataKind::Mdl
            | DataKind::Md2
            | DataKind::Md3
            | DataKind::Bsp
            | DataKind::Sprite2
            | DataKind::Gltf
            | DataKind::Archive => {
                let bytes = archive.read(entry)?;
                let (file, _) = (super::export::raw_file_name(entry), ());
                Ok(Preview { kind: PreviewKind::File { name: file, bytes }, facts })
            }
            DataKind::Mus
            | DataKind::Midi
            | DataKind::Ogg
            | DataKind::Mp3
            | DataKind::Flac
            | DataKind::SpeakerSound => {
                if entry.data == DataKind::Mus {
                    facts.push(("Note".into(), "Save it as MIDI to play it.".into()));
                }
                Ok(Preview { kind: PreviewKind::Info, facts })
            }
            DataKind::MapLump | DataKind::Marker | DataKind::Empty => Ok(Preview { kind: PreviewKind::Info, facts }),
            // Anything else: a file the viewer may know by its name and
            // content, else text, else a hex dump of the start.
            _ => {
                let head = archive.read_head(entry, 4096)?;
                if crate::sniff(std::path::Path::new(name), &head).is_some() {
                    let bytes = archive.read(entry)?;
                    return Ok(Preview {
                        kind: PreviewKind::File { name: super::export::raw_file_name(entry), bytes },
                        facts,
                    });
                }
                if slerp_archive::looks_like_text(&head) {
                    let text = archive.read_head(entry, MAX_TEXT_BYTES)?;
                    return Ok(Preview { kind: PreviewKind::Text(String::from_utf8_lossy(&text).into_owned()), facts });
                }
                let hex: Vec<String> = head.iter().take(32).map(|b| format!("{b:02x}")).collect();
                facts.push(("First bytes".into(), hex.join(" ")));
                Ok(Preview { kind: PreviewKind::Info, facts })
            }
        }
    }
}

/// A word for what an entry is.
fn describe(data: DataKind) -> &'static str {
    match data {
        DataKind::Unknown => "Unknown data",
        DataKind::DoomPicture => "Doom picture",
        DataKind::DoomFlat => "Doom flat",
        DataKind::QuakePicture => "Quake picture",
        DataKind::Miptex => "Quake texture",
        DataKind::Wad3Miptex => "Half-Life texture",
        DataKind::Wad3Picture => "Half-Life picture",
        DataKind::Wad3Font => "Half-Life font",
        DataKind::Png => "PNG picture",
        DataKind::Jpeg => "JPEG picture",
        DataKind::Gif => "GIF picture",
        DataKind::Bmp => "BMP picture",
        DataKind::Tga => "TGA picture",
        DataKind::Dds => "DDS texture",
        DataKind::Ktx2 => "KTX2 texture",
        DataKind::Pcx => "PCX picture",
        DataKind::Webp => "WebP picture",
        DataKind::Vtf => "Source texture (VTF)",
        DataKind::DmxSound => "Doom sound (DMX)",
        DataKind::SpeakerSound => "Doom PC speaker sound",
        DataKind::Mus => "Doom music (MUS)",
        DataKind::Midi => "MIDI music",
        DataKind::Wav => "WAV sound",
        DataKind::Ogg => "Ogg audio",
        DataKind::Mp3 => "MP3 audio",
        DataKind::Flac => "FLAC audio",
        DataKind::Playpal => "Doom palettes (PLAYPAL)",
        DataKind::Colormap => "Doom colour map",
        DataKind::Palette768 => "Palette",
        DataKind::Pnames => "Patch names (PNAMES)",
        DataKind::TextureDefs => "Composite texture definitions",
        DataKind::ZdoomTextures => "ZDoom TEXTURES",
        DataKind::Text => "Text",
        DataKind::Mdl => "Model (MDL)",
        DataKind::Md2 => "Model (MD2)",
        DataKind::Md3 => "Model (MD3)",
        DataKind::Bsp => "Quake or Half-Life level (BSP)",
        DataKind::Sprite2 => "Sprite",
        DataKind::Gltf => "glTF model",
        DataKind::MapLump => "Map data",
        DataKind::Marker => "Marker",
        DataKind::Archive => "Another archive",
        DataKind::Empty => "Empty",
    }
}

/// "blend shape" as "Blend shape".
fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn seconds(s: f64) -> String {
    if s < 1.0 { format!("{:.0} ms", s * 1000.0) } else { format!("{s:.2} s") }
}

/*┌─────────────────╮
| Waveforms       |
└─────────────────╯*/
//region Waveforms

/// Min and max of unsigned 8-bit samples (128 is silence), in -1..1, for
/// each of the picture's columns.
fn peaks_u8(samples: &[u8]) -> Vec<(f32, f32)> {
    peaks(samples.len(), |i| (samples[i] as f32 - 128.0) / 128.0)
}

fn peaks(n: usize, sample: impl Fn(usize) -> f32) -> Vec<(f32, f32)> {
    const COLUMNS: usize = 1024;
    if n == 0 {
        return vec![(0.0, 0.0); COLUMNS];
    }
    (0..COLUMNS)
        .map(|c| {
            let (from, to) = (c * n / COLUMNS, ((c + 1) * n / COLUMNS).max(c * n / COLUMNS + 1).min(n));
            let mut lo = f32::MAX;
            let mut hi = f32::MIN;
            for i in from..to {
                let v = sample(i);
                lo = lo.min(v);
                hi = hi.max(v);
            }
            (lo.min(hi), hi.max(lo))
        })
        .collect()
}

/// Peaks of a PCM WAV file's first channel, adding its format to `facts`;
/// `None` for anything but 8, 16, 24 or 32-bit PCM.
fn wav_peaks(bytes: &[u8], facts: &mut Vec<(String, String)>) -> Option<Vec<(f32, f32)>> {
    if bytes.get(0..4)? != b"RIFF" || bytes.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut at = 12;
    let (mut format, mut channels, mut rate, mut bits) = (0u16, 0u16, 0u32, 0u16);
    let mut data: Option<&[u8]> = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        let body = bytes.get(at + 8..(at + 8).saturating_add(len).min(bytes.len()))?;
        if id == b"fmt " && body.len() >= 16 {
            format = u16::from_le_bytes([body[0], body[1]]);
            channels = u16::from_le_bytes([body[2], body[3]]);
            rate = u32::from_le_bytes(body[4..8].try_into().ok()?);
            bits = u16::from_le_bytes([body[14], body[15]]);
        } else if id == b"data" {
            data = Some(body);
        }
        at = (at + 8).saturating_add(len).saturating_add(len & 1);
    }
    facts.push(("Sample rate".into(), format!("{rate} Hz")));
    facts.push(("Channels".into(), channels.to_string()));
    facts.push(("Bits".into(), bits.to_string()));
    let data = data?;
    let pcm = format == 1 || format == 0xFFFE;
    let width = (bits / 8) as usize;
    if !pcm || channels == 0 || !(1..=4).contains(&width) {
        return None;
    }
    let frame = width * channels as usize;
    let n = data.len() / frame;
    if rate > 0 {
        facts.push(("Length".into(), seconds(n as f64 / rate as f64)));
    }
    Some(peaks(n, |i| {
        let s = &data[i * frame..i * frame + width];
        match width {
            1 => (s[0] as f32 - 128.0) / 128.0,
            2 => i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0,
            3 => (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0,
            _ => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
        }
    }))
}

/// A waveform as a 1024 x 256 PNG: bright on dark, so it shows on either
/// theme.
fn waveform_png(peaks: &[(f32, f32)]) -> Result<Vec<u8>> {
    let (w, h) = (peaks.len().max(1) as u32, 256u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for p in rgba.chunks_exact_mut(4) {
        p.copy_from_slice(&[0x1b, 0x1f, 0x24, 255]);
    }
    let mid = h as f32 / 2.0;
    for x in 0..w {
        let put = |rgba: &mut [u8], y: u32, c: [u8; 3]| {
            let p = ((y.min(h - 1) * w + x) * 4) as usize;
            rgba[p..p + 3].copy_from_slice(&c);
        };
        put(&mut rgba, h / 2, [0x3a, 0x42, 0x4c]);
        let (lo, hi) = peaks[x as usize];
        let top = (mid - hi.clamp(-1.0, 1.0) * (mid - 1.0)) as u32;
        let bottom = (mid - lo.clamp(-1.0, 1.0) * (mid - 1.0)) as u32;
        for y in top..=bottom.max(top) {
            put(&mut rgba, y, [0x6c, 0xc3, 0xf0]);
        }
    }
    png_bytes(&Image { width: w, height: h, rgba, offset: None })
}

//endregion
