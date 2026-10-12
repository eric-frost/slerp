//! Conversions from the game formats an archive holds to files other
//! programs read: Doom pictures, flats, sprites and Quake pictures and
//! textures to RGBA pictures (with the offsets Doom stores), palettes and
//! colour maps to swatch pictures, DMX sounds to WAV, and MUS music to
//! MIDI.  The archive browser's Extract button and `slerp contents extract`
//! use them; the pixel decoders are the ones the loaders use.

use crate::miptex::{self, Palette};
use crate::pixels::{self, DOOM_PALETTE, QUAKE_PALETTE};
use crate::read::{Bytes, Result};
use slerp_scene::LoadError;
use slerp_scene::textures::Pixels;

pub use crate::doom::{CompositeInfo, Composites, ResourceFile, texture_defs, with_composites, zdoom_texture_defs};

/// A decoded picture in straight RGBA, with the offset Doom stores for
/// patches and sprites (where the picture's origin sits relative to its
/// top left corner; `None` for pictures without one).
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub offset: Option<(i32, i32)>,
}

/// Freedoom's palette, which stands in for Doom's when a file has no
/// PLAYPAL.
pub fn default_doom_palette() -> &'static [u8; 768] {
    DOOM_PALETTE
}

/// Quake's palette.
pub fn default_quake_palette() -> &'static [u8; 768] {
    QUAKE_PALETTE
}

/*┌─────────────────╮
| Pictures        |
└─────────────────╯*/
//region Pictures

/// A Doom picture (a patch, a sprite, a menu graphic) in `palette`, with its
/// offsets.  A lump of exactly 320x200 bytes is a raw screen (Heretic and
/// Hexen's), which has no offsets.
pub fn doom_picture(data: &[u8], palette: &[u8]) -> Result<Image> {
    check_palette(palette)?;
    if data.len() == 320 * 200 && slerp_archive::doom_picture_header(data, data.len()).is_none() {
        let rgba = pixels::expand(data, 320, 200, palette, None);
        return Ok(Image { width: 320, height: 200, rgba, offset: None });
    }
    let (w, h, rgba) = crate::doom::patch(data, palette)?;
    let b = Bytes::new(data);
    let offset = Some((b.i16(4)? as i32, b.i16(6)? as i32));
    Ok(Image { width: w, height: h, rgba, offset })
}

/// A Doom flat: 64 wide and as tall as the lump says, or a square for the
/// larger sizes source ports allow.
pub fn doom_flat(data: &[u8], palette: &[u8]) -> Result<Image> {
    check_palette(palette)?;
    let (w, h) = match data.len() {
        4096 => (64, 64),
        4160 => (64, 65),
        8192 => (64, 128),
        n if n > 0 && (n as f64).sqrt().fract() == 0.0 => ((n as f64).sqrt() as u32, (n as f64).sqrt() as u32),
        _ => return Err(LoadError::invalid(format!("{} bytes is not a flat's size", data.len()))),
    };
    let rgba = pixels::expand(data, w, h, palette, None);
    Ok(Image { width: w, height: h, rgba, offset: None })
}

/// A Quake picture (`.lmp`, or a WAD2 status-bar lump): width, height, then
/// palette indices, with index 255 transparent.
pub fn quake_picture(data: &[u8], palette: &[u8]) -> Result<Image> {
    check_palette(palette)?;
    let b = Bytes::new(data);
    let (w, h) = (b.u32(0)?, b.u32(4)?);
    let n = pixels::check_size(w, h)?;
    let rgba = pixels::expand(b.slice(8, n)?, w, h, palette, Some(255));
    Ok(Image { width: w, height: h, rgba, offset: None })
}

/// A Quake miptex texture's largest level, in `palette` (Quake's global
/// palette), or a Half-Life miptex or picture (`half_life`) in the palette
/// stored with it.
pub fn miptex(data: &[u8], palette: &[u8], half_life: bool) -> Result<Image> {
    let source = if half_life { Palette::Embedded } else { Palette::Global(palette) };
    if !half_life {
        check_palette(palette)?;
    }
    let decoded = miptex::decode(Bytes::new(data), 0, source, None, "miptex")?;
    let t = decoded.base;
    match t.base() {
        Pixels::Rgba8(v) => Ok(Image { width: t.width(), height: t.height(), rgba: v.clone(), offset: None }),
        _ => Err(LoadError::invalid("the texture didn't decode to 8-bit colour")),
    }
}

/// A Source engine VTF texture's largest level.
pub fn vtf_image(data: &[u8]) -> Result<Image> {
    let t = crate::vtf::decode(data, None)?;
    match t.base() {
        Pixels::Rgba8(v) => Ok(Image { width: t.width(), height: t.height(), rgba: v.clone(), offset: None }),
        _ => Err(LoadError::invalid("the texture is high dynamic range, which PNG can't hold")),
    }
}

/// A Half-Life picture: width, height, indices, then its own palette (a
/// count and RGB triples).
pub fn wad3_picture(data: &[u8]) -> Result<Image> {
    let b = Bytes::new(data);
    let (w, h) = (b.u32(0)?, b.u32(4)?);
    let n = pixels::check_size(w, h)?;
    let at = 8 + n;
    let count = (b.u16(at)? as usize).min(256);
    let mut palette = [0u8; 768];
    palette[..count * 3].copy_from_slice(b.slice(at + 2, count * 3)?);
    let rgba = pixels::expand(b.slice(8, n)?, w, h, &palette, None);
    Ok(Image { width: w, height: h, rgba, offset: None })
}

fn check_palette(palette: &[u8]) -> Result<()> {
    if palette.len() < 768 {
        return Err(LoadError::invalid("a palette needs 768 bytes"));
    }
    Ok(())
}

//endregion

/*┌─────────────────╮
| Palettes        |
└─────────────────╯*/
//region Palettes

fn fill(rgba: &mut [u8], stride: u32, x0: u32, y0: u32, w: u32, h: u32, color: [u8; 3]) {
    for y in y0..y0 + h {
        for x in x0..x0 + w {
            let p = ((y * stride + x) * 4) as usize;
            rgba[p..p + 4].copy_from_slice(&[color[0], color[1], color[2], 255]);
        }
    }
}

/// A palette (768 bytes, 256 RGB triples) as a 16x16 grid of swatches in
/// palette order, 16 pixels each.
pub fn palette_image(palette: &[u8]) -> Result<Image> {
    check_palette(palette)?;
    let cell = 16;
    let side = 16 * cell;
    let mut rgba = vec![0u8; (side * side * 4) as usize];
    for i in 0..256u32 {
        let c = [palette[i as usize * 3], palette[i as usize * 3 + 1], palette[i as usize * 3 + 2]];
        fill(&mut rgba, side, (i % 16) * cell, (i / 16) * cell, cell, cell, c);
    }
    Ok(Image { width: side, height: side, rgba, offset: None })
}

/// PLAYPAL's palettes (Doom's has 14: normal, damage reds, pickup golds and
/// the radiation suit's green) side by side, seven to a row, each as the
/// 16x16 grid of 8-pixel swatches, with a gap between them.
pub fn playpal_image(playpal: &[u8]) -> Result<Image> {
    let count = (playpal.len() / 768).min(64);
    if count == 0 {
        return Err(LoadError::invalid("PLAYPAL holds no palette"));
    }
    let (cell, gap) = (8u32, 8u32);
    let side = 16 * cell;
    let columns = count.min(7) as u32;
    let rows = count.div_ceil(7) as u32;
    let (w, h) = (columns * (side + gap) - gap, rows * (side + gap) - gap);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for k in 0..count as u32 {
        let (ox, oy) = ((k % 7) * (side + gap), (k / 7) * (side + gap));
        for i in 0..256u32 {
            let at = k as usize * 768 + i as usize * 3;
            let c = [playpal[at], playpal[at + 1], playpal[at + 2]];
            fill(&mut rgba, w, ox + (i % 16) * cell, oy + (i / 16) * cell, cell, cell, c);
        }
    }
    Ok(Image { width: w, height: h, rgba, offset: None })
}

/// COLORMAP's light levels (34 in Doom: 32 from bright to dark, the
/// invulnerability map and a black one) as rows of 256 swatches, each
/// colour looked up in `palette`.
pub fn colormap_image(colormap: &[u8], palette: &[u8]) -> Result<Image> {
    check_palette(palette)?;
    let rows = (colormap.len() / 256).min(64) as u32;
    if rows == 0 {
        return Err(LoadError::invalid("COLORMAP holds no light level"));
    }
    let (cw, ch) = (4u32, 8u32);
    let (w, h) = (256 * cw, rows * ch);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for r in 0..rows {
        for i in 0..256u32 {
            let index = colormap[(r * 256 + i) as usize] as usize;
            let c = [palette[index * 3], palette[index * 3 + 1], palette[index * 3 + 2]];
            fill(&mut rgba, w, i * cw, r * ch, cw, ch, c);
        }
    }
    Ok(Image { width: w, height: h, rgba, offset: None })
}

//endregion

/*┌─────────────────╮
| Sound           |
└─────────────────╯*/
//region Sound

/// What a DMX sound holds, besides its samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoundInfo {
    pub sample_rate: u32,
    pub samples: u32,
}

/// Reads a DMX sound's header and the range of its samples: format 3, the
/// sample rate, the sample count (which includes 16 padding bytes at each
/// end), then unsigned 8-bit samples.  Returns the info and the byte range
/// of the real samples.
pub fn dmx_info(data: &[u8]) -> Result<(SoundInfo, std::ops::Range<usize>)> {
    let b = Bytes::new(data);
    if b.u16(0)? != 3 {
        return Err(LoadError::invalid("not a DMX digital sound"));
    }
    let rate = b.u16(2)? as u32;
    let count = b.u32(4)? as usize;
    if rate == 0 || count == 0 || count > data.len().saturating_sub(8) {
        return Err(LoadError::invalid("the DMX sound's header doesn't fit its lump"));
    }
    // The padding is the first and last 16 bytes of the samples.
    let (start, end) = if count > 32 { (8 + 16, 8 + count - 16) } else { (8, 8 + count) };
    Ok((SoundInfo { sample_rate: rate, samples: (end - start) as u32 }, start..end))
}

/// A DMX sound as a WAV file: mono, 8 bits, at the sound's own rate, with
/// the padding bytes dropped.
pub fn dmx_to_wav(data: &[u8]) -> Result<Vec<u8>> {
    let (info, range) = dmx_info(data)?;
    Ok(wav_u8_mono(info.sample_rate, &data[range]))
}

/// Unsigned 8-bit mono PCM as a WAV file.
pub fn wav_u8_mono(rate: u32, samples: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(44 + samples.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + samples.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes()); // bytes per second
    out.extend_from_slice(&1u16.to_le_bytes()); // block align
    out.extend_from_slice(&8u16.to_le_bytes()); // bits
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    out.extend_from_slice(samples);
    out
}

//endregion

/*┌─────────────────╮
| Music           |
└─────────────────╯*/
//region Music

/// MIDI's ticks per quarter note for converted music.  MUS counts 140 ticks
/// a second; at a tempo of 500,000 microseconds per quarter note (two
/// quarters a second) that is 70 ticks per quarter note.
const MIDI_DIVISION: u16 = 70;
const MIDI_TEMPO: u32 = 500_000;
/// The most events one score may hold; a real song has a few tens of
/// thousands.
const MAX_MUS_EVENTS: usize = 4 << 20;

fn midi_variable(out: &mut Vec<u8>, mut v: u32) {
    let mut bytes = [0u8; 5];
    let mut n = 0;
    loop {
        bytes[n] = (v & 0x7F) as u8 | if n > 0 { 0x80 } else { 0 };
        n += 1;
        v >>= 7;
        if v == 0 {
            break;
        }
    }
    out.extend(bytes[..n].iter().rev());
}

/// MUS (Doom's music format) as a standard MIDI file, format 0.
///
/// The score is a list of events, each a byte with the channel, the event
/// type and a "delay follows" flag; delays count 140 ticks a second.
/// Channel 15 (percussion) becomes MIDI channel 9 and channels 9 to 14 shift
/// up by one.  Controller changes, program changes, pitch bends, note
/// events and the system events (all sounds off and the others) map to the
/// MIDI messages of the same meaning.
pub fn mus_to_midi(data: &[u8]) -> Result<Vec<u8>> {
    let b = Bytes::new(data);
    if !data.starts_with(b"MUS\x1a") {
        return Err(LoadError::invalid("not a MUS file"));
    }
    let score_len = b.u16(4)? as usize;
    let score_start = b.u16(6)? as usize;
    // Some files declare a length past their end; read what is there.
    let end = score_start.saturating_add(score_len).min(data.len());
    let score = data.get(score_start..end).ok_or_else(|| LoadError::invalid("the MUS score lies outside the file"))?;

    let mut track: Vec<u8> = Vec::new();
    // Tempo, at time zero.
    track.extend_from_slice(&[0x00, 0xFF, 0x51, 0x03]);
    track.extend_from_slice(&MIDI_TEMPO.to_be_bytes()[1..]);
    let channel_of = |c: u8| -> u8 {
        match c {
            15 => 9,
            9..=14 => c + 1,
            c => c,
        }
    };
    let mut velocity = [127u8; 16];
    let mut pending: u32 = 0;
    let mut at = 0usize;
    let mut events = 0usize;
    let truncated = || LoadError::invalid("the MUS score is cut short");
    while at < score.len() {
        events += 1;
        if events > MAX_MUS_EVENTS {
            return Err(LoadError::TooLarge("a MUS score with millions of events".into()));
        }
        let head = score[at];
        at += 1;
        let (last, kind, ch) = (head & 0x80 != 0, (head >> 4) & 7, channel_of(head & 0x0F));
        let byte = |at: &mut usize| -> Result<u8> {
            let v = *score.get(*at).ok_or_else(truncated)?;
            *at += 1;
            Ok(v)
        };
        let mut message: Vec<u8> = Vec::with_capacity(3);
        match kind {
            // Release note.
            0 => message.extend_from_slice(&[0x80 | ch, byte(&mut at)? & 0x7F, 0]),
            // Play note, with a volume when the note's high bit is set.
            1 => {
                let n = byte(&mut at)?;
                if n & 0x80 != 0 {
                    velocity[(head & 0x0F) as usize] = byte(&mut at)? & 0x7F;
                }
                message.extend_from_slice(&[0x90 | ch, n & 0x7F, velocity[(head & 0x0F) as usize]]);
            }
            // Pitch bend: 0 to 255 with 128 in the middle, as 14 bits.
            2 => {
                let v = (byte(&mut at)? as u16) << 6;
                message.extend_from_slice(&[0xE0 | ch, (v & 0x7F) as u8, (v >> 7) as u8]);
            }
            // System event: a controller with no value.
            3 => {
                let cc = match byte(&mut at)? & 0x7F {
                    10 => 120,
                    11 => 123,
                    12 => 126,
                    13 => 127,
                    14 => 121,
                    _ => {
                        // Not a defined system event: skip it.
                        if last {
                            pending += read_delay(score, &mut at)?;
                        }
                        continue;
                    }
                };
                message.extend_from_slice(&[0xB0 | ch, cc, 0]);
            }
            // Controller: 0 is a program change.
            4 => {
                let (number, value) = (byte(&mut at)? & 0x7F, byte(&mut at)? & 0x7F);
                if number == 0 {
                    message.extend_from_slice(&[0xC0 | ch, value]);
                } else {
                    let cc = match number {
                        1 => 0,
                        2 => 1,
                        3 => 7,
                        4 => 10,
                        5 => 11,
                        6 => 91,
                        7 => 93,
                        8 => 64,
                        9 => 67,
                        _ => {
                            if last {
                                pending += read_delay(score, &mut at)?;
                            }
                            continue;
                        }
                    };
                    message.extend_from_slice(&[0xB0 | ch, cc, value]);
                }
            }
            // End of measure.
            5 => {}
            // Finish: the score ends.
            6 => break,
            // Unused, with one data byte.
            _ => {
                byte(&mut at)?;
            }
        }
        if !message.is_empty() {
            midi_variable(&mut track, pending);
            pending = 0;
            track.extend_from_slice(&message);
        }
        if last {
            pending += read_delay(score, &mut at)?;
        }
    }
    midi_variable(&mut track, pending);
    track.extend_from_slice(&[0xFF, 0x2F, 0x00]);

    let mut out = Vec::with_capacity(22 + track.len());
    out.extend_from_slice(b"MThd");
    out.extend_from_slice(&6u32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // format 0
    out.extend_from_slice(&1u16.to_be_bytes()); // one track
    out.extend_from_slice(&MIDI_DIVISION.to_be_bytes());
    out.extend_from_slice(b"MTrk");
    out.extend_from_slice(&(track.len() as u32).to_be_bytes());
    out.extend_from_slice(&track);
    Ok(out)
}

/// A MUS delay: 7 bits at a time, high bit set while more follow.
fn read_delay(score: &[u8], at: &mut usize) -> Result<u32> {
    let mut delay = 0u32;
    for _ in 0..5 {
        let b = *score.get(*at).ok_or_else(|| LoadError::invalid("the MUS score is cut short"))?;
        *at += 1;
        delay = (delay << 7) | (b & 0x7F) as u32;
        if b & 0x80 == 0 {
            return Ok(delay);
        }
    }
    Err(LoadError::invalid("a MUS delay is too long"))
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;
    use slerp_archive::testutil;

    /// A palette whose colour `i` is (i, 255 - i, i / 2).
    fn palette() -> Vec<u8> {
        (0..256u32).flat_map(|i| [i as u8, 255 - i as u8, (i / 2) as u8]).collect()
    }

    fn rgb(img: &Image, x: u32, y: u32) -> [u8; 4] {
        let p = ((y * img.width + x) * 4) as usize;
        img.rgba[p..p + 4].try_into().unwrap()
    }

    #[test]
    fn doom_pictures_keep_pixels_and_offsets() {
        let pal = palette();
        // 3 wide, 2 tall: columns hold the pixels top to bottom.
        let data = testutil::doom_picture(3, 2, -4, 7, &[1, 2, 3, 4, 5, 6]);
        let img = doom_picture(&data, &pal).unwrap();
        assert_eq!((img.width, img.height, img.offset), (3, 2, Some((-4, 7))));
        assert_eq!(rgb(&img, 0, 0), [1, 254, 0, 255]);
        assert_eq!(rgb(&img, 2, 0), [3, 252, 1, 255]);
        assert_eq!(rgb(&img, 0, 1), [4, 251, 2, 255]);
        assert_eq!(rgb(&img, 2, 1), [6, 249, 3, 255]);
        // Damage fails cleanly.
        for cut in 0..data.len() {
            let _ = doom_picture(&data[..cut], &pal);
        }
        assert!(doom_picture(&data, &pal[..100]).is_err());
    }

    #[test]
    fn a_patch_with_gaps_is_transparent_there() {
        // One column, 4 tall, with a single post covering rows 1 and 2.
        let mut data = vec![1, 0, 4, 0, 0, 0, 0, 0, 12, 0, 0, 0];
        data.extend_from_slice(&[1, 2, 0, 9, 8, 0, 0xFF]);
        let img = doom_picture(&data, &palette()).unwrap();
        assert_eq!(rgb(&img, 0, 0)[3], 0);
        assert_eq!(rgb(&img, 0, 1), [9, 246, 4, 255]);
        assert_eq!(rgb(&img, 0, 2), [8, 247, 4, 255]);
        assert_eq!(rgb(&img, 0, 3)[3], 0);
    }

    #[test]
    fn flats_are_64_wide() {
        let pal = palette();
        let mut flat = vec![0u8; 4096];
        flat[0] = 10;
        flat[63] = 11;
        flat[64] = 12;
        let img = doom_flat(&flat, &pal).unwrap();
        assert_eq!((img.width, img.height, img.offset), (64, 64, None));
        assert_eq!(rgb(&img, 0, 0)[0], 10);
        assert_eq!(rgb(&img, 63, 0)[0], 11);
        assert_eq!(rgb(&img, 0, 1)[0], 12);
        assert_eq!(doom_flat(&vec![0u8; 16384], &pal).unwrap().width, 128);
        assert!(doom_flat(&[0u8; 1000], &pal).is_err());
    }

    #[test]
    fn palettes_become_swatch_grids() {
        let pal = palette();
        let img = palette_image(&pal).unwrap();
        assert_eq!((img.width, img.height), (256, 256));
        // Colour 2 * 16 + 5 sits in column 5, row 2.
        assert_eq!(rgb(&img, 5 * 16 + 3, 2 * 16 + 9), [37, 218, 18, 255]);
        let mut playpal = Vec::new();
        for k in 0..14u8 {
            playpal.extend(pal.iter().map(|&b| b.wrapping_add(k)));
        }
        let img = playpal_image(&playpal).unwrap();
        assert_eq!((img.width, img.height), (7 * 136 - 8, 2 * 136 - 8));
        // Palette 8 is in the second row, second column.
        assert_eq!(rgb(&img, 136 + 2, 136 + 2), [8, 7, 8, 255]);
        assert!(playpal_image(&[]).is_err());
        // A colour map: row 1 maps everything to colour 7.
        let mut cmap = (0..=255u8).collect::<Vec<_>>();
        cmap.extend(std::iter::repeat_n(7u8, 256));
        let img = colormap_image(&cmap, &pal).unwrap();
        assert_eq!((img.width, img.height), (1024, 16));
        assert_eq!(rgb(&img, 100 * 4, 0)[0], 100);
        assert_eq!(rgb(&img, 100 * 4, 8)[0], 7);
    }

    #[test]
    fn dmx_sounds_become_wav() {
        let mut dmx = vec![3, 0];
        dmx.extend_from_slice(&11025u16.to_le_bytes());
        // 16 padding, 5 samples, 16 padding.
        dmx.extend_from_slice(&37u32.to_le_bytes());
        dmx.extend_from_slice(&[128; 16]);
        dmx.extend_from_slice(&[1, 2, 3, 4, 5]);
        dmx.extend_from_slice(&[128; 16]);
        let wav = dmx_to_wav(&dmx).unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 36 + 5);
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 11025);
        assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 8);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 5);
        assert_eq!(&wav[44..], [1, 2, 3, 4, 5]);
        // A count longer than the lump, a wrong format, and cuts.
        let mut bad = dmx.clone();
        bad[4..8].copy_from_slice(&5000u32.to_le_bytes());
        assert!(dmx_to_wav(&bad).is_err());
        bad = dmx.clone();
        bad[0] = 2;
        assert!(dmx_to_wav(&bad).is_err());
        for cut in 0..dmx.len() {
            let _ = dmx_to_wav(&dmx[..cut]);
        }
    }

    /// A MUS file with the given score.
    fn mus(score: &[u8]) -> Vec<u8> {
        let mut out = b"MUS\x1a".to_vec();
        out.extend_from_slice(&(score.len() as u16).to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0]);
        out.extend_from_slice(score);
        out
    }

    #[test]
    fn mus_events_become_midi_in_order() {
        let score = [
            0x40, // controller on channel 0 ...
            0x00,
            0x05, // ... 0 is a program change to 5
            0x90, // play note on channel 0, delay follows
            0xBC,
            100,  // note 60 with a volume byte of 100
            10,   // delay 10
            0x80, // release note, delay follows
            60,
            0,    // note 60, no delay
            0x9F, // play note on channel 15, delay follows
            36,
            5,        // note 36, delay 5
            0x20 | 3, // pitch bend on channel 3 ...
            128,      // ... centred
            0x60,     // finish
        ];
        let midi = mus_to_midi(&mus(&score)).unwrap();
        assert_eq!(&midi[0..4], b"MThd");
        assert_eq!(&midi[8..14], [0, 0, 0, 1, 0, 70]);
        assert_eq!(&midi[14..18], b"MTrk");
        let track = &midi[22..];
        assert_eq!(u32::from_be_bytes(midi[18..22].try_into().unwrap()) as usize, track.len());
        assert_eq!(
            track,
            [
                0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20, // tempo 500,000
                0x00, 0xC0, 0x05, // program change
                0x00, 0x90, 60, 100, // note on with its volume
                10, 0x80, 60, 0, // release after 10 ticks
                0x00, 0x99, 36, 127, // percussion is channel 9, at full velocity
                5, 0xE3, 0x00, 0x40, // pitch bend 128 << 6 = 8192
                0x00, 0xFF, 0x2F, 0x00, // end of track
            ]
        );
    }

    #[test]
    fn mus_controllers_and_channels() {
        let score = [
            0x40 | 9,
            3,
            90, // volume on MUS channel 9 -> MIDI channel 10
            0x30 | 15,
            11, // all notes off on channel 15 -> 9
            0x40 | 1,
            1,
            0, // bank select -> controller 0
            0x40 | 1,
            12,
            1, // an undefined controller number is skipped
            0x80 | 0x60,
            140,
            0x01, // finish with a delay that is never reached
        ];
        let midi = mus_to_midi(&mus(&score)).unwrap();
        assert_eq!(&midi[29..], [0x00, 0xBA, 7, 90, 0x00, 0xB9, 123, 0, 0x00, 0xB1, 0, 0, 0x00, 0xFF, 0x2F, 0x00]);
    }

    #[test]
    fn long_mus_delays_use_variable_lengths() {
        // Note on, then a delay of 300 ticks (two bytes: 0x82 0x2C), finish.
        let midi = mus_to_midi(&mus(&[0x90, 60, 0x82, 0x2C, 0x00, 60, 0x60])).unwrap();
        assert!(midi.windows(3).any(|w| w == [0x82, 0x2C, 0x80]), "{midi:02x?}");
    }

    #[test]
    fn damaged_mus_never_panics() {
        let good = mus(&[0x40, 0x00, 0x05, 0x90, 0xBC, 100, 10, 0x80, 60, 0, 0x60]);
        for cut in 0..good.len() {
            let _ = mus_to_midi(&good[..cut]);
        }
        for i in 0..good.len() {
            for v in [0u8, 0x7f, 0x80, 0xff] {
                let mut m = good.clone();
                m[i] = v;
                let _ = mus_to_midi(&m);
            }
        }
        assert!(mus_to_midi(b"MThd").is_err());
        // A delay that never ends.
        assert!(mus_to_midi(&mus(&[0x90, 60, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])).is_err());
    }

    #[test]
    fn quake_pictures_and_textures() {
        let pal = palette();
        let mut qpic = Vec::new();
        qpic.extend_from_slice(&2u32.to_le_bytes());
        qpic.extend_from_slice(&1u32.to_le_bytes());
        qpic.extend_from_slice(&[3, 255]);
        let img = quake_picture(&qpic, &pal).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(rgb(&img, 0, 0), [3, 252, 1, 255]);
        assert_eq!(rgb(&img, 1, 0)[3], 0);
        let tex = crate::miptex::tests::build("brick", 16, 16, |x, y| ((x + y) % 200) as u8, None);
        let img = miptex(&tex, &pal, false).unwrap();
        assert_eq!((img.width, img.height), (16, 16));
        assert_eq!(rgb(&img, 3, 4), [7, 248, 3, 255]);
        assert!(miptex(&tex[..30], &pal, false).is_err());
    }

    #[test]
    fn composite_textures_draw_their_patches() {
        // Palette: colour 1 red, 2 green.
        let mut pal = vec![0u8; 768];
        pal[3..6].copy_from_slice(&[255, 0, 0]);
        pal[6..9].copy_from_slice(&[0, 255, 0]);
        let red = testutil::doom_picture(2, 2, 0, 0, &[1; 4]);
        let green = testutil::doom_picture(2, 2, 0, 0, &[2; 4]);
        let pnames = {
            let mut p = 2u32.to_le_bytes().to_vec();
            p.extend_from_slice(b"RED\0\0\0\0\0GREEN\0\0\0");
            p
        };
        let texture1 = {
            let mut t = 1u32.to_le_bytes().to_vec();
            t.extend_from_slice(&8u32.to_le_bytes());
            t.extend_from_slice(b"WALL\0\0\0\0");
            t.extend_from_slice(&0u32.to_le_bytes());
            t.extend_from_slice(&4i16.to_le_bytes());
            t.extend_from_slice(&4i16.to_le_bytes());
            t.extend_from_slice(&0u32.to_le_bytes());
            t.extend_from_slice(&2i16.to_le_bytes());
            for (x, y, patch) in [(0i16, 0i16, 0u16), (2, 2, 1)] {
                t.extend_from_slice(&x.to_le_bytes());
                t.extend_from_slice(&y.to_le_bytes());
                t.extend_from_slice(&patch.to_le_bytes());
                t.extend_from_slice(&[1, 0, 0, 0]);
            }
            t
        };
        let defs = texture_defs(&texture1, &pnames);
        assert_eq!(defs, [CompositeInfo { name: "WALL".into(), width: 4, height: 4, patches: 2 }]);
        let wad = testutil::wad(
            true,
            &[
                ("PLAYPAL", &pal),
                ("PNAMES", &pnames),
                ("TEXTURE1", &texture1),
                ("P_START", b""),
                ("RED", &red),
                ("GREEN", &green),
                ("P_END", b""),
            ],
        );
        let img = with_composites(ResourceFile::Wad(&wad), None, |c| c.render("WALL")).unwrap().unwrap();
        assert_eq!((img.width, img.height), (4, 4));
        assert_eq!(rgb(&img, 0, 0), [255, 0, 0, 255]);
        assert_eq!(rgb(&img, 1, 1), [255, 0, 0, 255]);
        assert_eq!(rgb(&img, 2, 2), [0, 255, 0, 255]);
        assert_eq!(rgb(&img, 3, 3), [0, 255, 0, 255]);
        assert_eq!(rgb(&img, 3, 0)[3], 0);
        assert!(with_composites(ResourceFile::Wad(&wad), None, |c| c.render("NOPE")).unwrap().is_none());
        // ZDoom's TEXTURES lists the same way.
        let text = b"Texture \"BRICK\", 64, 32 { Patch \"RED\", 0, 0 }";
        assert_eq!(
            zdoom_texture_defs(text),
            [CompositeInfo { name: "BRICK".into(), width: 64, height: 32, patches: 1 }]
        );
    }
}
