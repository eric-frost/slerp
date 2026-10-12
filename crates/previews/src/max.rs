//! 3ds Max scenes: the thumbnail Max stores in the `\x05SummaryInformation`
//! stream, the standard OLE property set every Windows document had.  The
//! thumbnail property (`PIDSI_THUMBNAIL`, number 17) is clipboard data:
//! older Max versions and newer ones alike write a Windows metafile picture
//! (`CF_METAFILEPICT`) whose one drawing record stretches a DIB onto the
//! page.  The DIB is all we need: a BMP file without its 14-byte header.

use crate::source::Source;
use slerp_scene::textures::Texture;
use slerp_scene::{Limits, LoadError};

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

pub fn preview(src: &mut Source, limits: &Limits) -> Result<Texture, LoadError> {
    let clip = crate::summary::thumbnail_clip(src)?.ok_or_else(|| crate::no_preview("3ds Max scene"))?;
    let bmp = dib_in(&clip).ok_or_else(|| LoadError::invalid("3ds Max thumbnail: no picture in it"))?;
    crate::decode_image(&bmp, limits)
}

/// Finds the DIB in clipboard data (a bare `CF_DIB`, or a metafile picture
/// that draws one) and turns it into a BMP file.
fn dib_in(clip: &[u8]) -> Option<Vec<u8>> {
    // Windows' own formats are tagged -1 and a format number; anything
    // else (Mac pictures, named formats) is searched as it is.
    let data = if u32_at(clip, 0)? == 0xFFFF_FFFF { clip.get(8..)? } else { clip };
    // A BITMAPINFOHEADER (or its larger V4 and V5 forms) is distinctive
    // enough to find by its fields, which spares parsing every metafile
    // record kind that could carry it.
    let mut i = 0;
    while i + 40 <= data.len() {
        let size = u32_at(data, i)?;
        if matches!(size, 40 | 108 | 124) {
            let w = u32_at(data, i + 4)? as i32;
            let h = u32_at(data, i + 8)? as i32;
            let planes = u16_at(data, i + 12)?;
            let bits = u16_at(data, i + 14)?;
            let compression = u32_at(data, i + 16)?;
            if planes == 1
                && (1..=8192).contains(&w)
                && h != 0
                && h.unsigned_abs() <= 8192
                && matches!(bits, 1 | 4 | 8 | 16 | 24 | 32)
                && compression <= 3
            {
                return Some(bmp_file(&data[i..], size, bits, compression));
            }
        }
        i += 2;
    }
    None
}

/// Puts a BMP file header in front of a DIB, pointing past the header,
/// colour masks and palette to the pixels.
pub(crate) fn bmp_file(dib: &[u8], header: u32, bits: u16, compression: u32) -> Vec<u8> {
    let used = u32_at(dib, 32).unwrap_or(0);
    let palette = if bits <= 8 { if used == 0 { 1u32 << bits } else { used.min(256) } } else { used.min(256) };
    let masks = if header == 40 && compression == 3 { 12 } else { 0 };
    let offset = 14 + header + masks + palette * 4;
    let total = 14 + dib.len() as u32;
    let mut out = Vec::with_capacity(total as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(dib);
    out
}
