//! The thumbnail in the standard OLE summary information property set
//! (`\x05SummaryInformation`), which Windows Explorer once showed for
//! Office-era documents.  The property (number 17) is clipboard data: a
//! format tag, then the picture in that format.

use crate::ole::Ole;
use crate::source::Source;
use slerp_scene::LoadError;

/// The summary stream holds a few hundred kilobytes at most.
const MAX_STREAM: u64 = 16 << 20;

const PIDSI_THUMBNAIL: u32 = 17;
const VT_CF: u32 = 71;

/// Clipboard formats found in thumbnails.
pub const CF_METAFILEPICT: u32 = 3;
pub const CF_DIB: u32 = 8;
pub const CF_ENHMETAFILE: u32 = 14;

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i.checked_add(4)?)?.try_into().ok()?))
}

/// The clipboard data of the thumbnail property, from its format tag on,
/// or `None` for files without a thumbnail.
pub fn thumbnail_clip(src: &mut Source) -> Result<Option<Vec<u8>>, LoadError> {
    let mut ole = Ole::open(src)?;
    let Some((entry, root)) = ole.find("\u{5}SummaryInformation")? else { return Ok(None) };
    let stream = ole.read(&entry, &root, MAX_STREAM)?;
    Ok(thumbnail_property(&stream).map(<[u8]>::to_vec))
}

/// Finds the thumbnail property in a property set stream.
pub fn thumbnail_property(s: &[u8]) -> Option<&[u8]> {
    clip_property(s, Some(PIDSI_THUMBNAIL))
}

/// The clipboard data of property `id` in the first property set of a
/// stream, or of the first clipboard property when `id` is `None`
/// (applications' own property sets put their thumbnails under their own
/// numbers).  The data runs from the format tag on.
pub fn clip_property(s: &[u8], id: Option<u32>) -> Option<&[u8]> {
    // 1. The stream header names its property sets and where they start;
    //    the summary information is the first.
    if s.get(..2)? != [0xFE, 0xFF] {
        return None;
    }
    let sets = u32_at(s, 24)?;
    if sets == 0 {
        return None;
    }
    let set = u32_at(s, 44)? as usize;
    // 2. The set's table of property numbers and offsets.
    let count = u32_at(s, set.checked_add(4)?)? as usize;
    for i in 0..count.min(4096) {
        let pid = u32_at(s, set + 8 + i * 8)?;
        if id.is_some_and(|id| id != pid) {
            continue;
        }
        let at = set.checked_add(u32_at(s, set + 12 + i * 8)? as usize)?;
        if u32_at(s, at)? & 0xFFFF != VT_CF {
            if id.is_some() {
                return None;
            }
            continue;
        }
        // 3. Clipboard data: its size (counting what follows it), then the
        //    format, then the data.
        let size = u32_at(s, at + 4)? as usize;
        let end = (at + 8).checked_add(size.max(4))?.min(s.len());
        return s.get(at + 8..end);
    }
    None
}

/// Splits clipboard data into the Windows clipboard format number and the
/// picture bytes after it.  Other format tags (Macintosh, named formats)
/// give `None`.
pub fn windows_format(clip: &[u8]) -> Option<(u32, &[u8])> {
    (u32_at(clip, 0)? == 0xFFFF_FFFF).then_some(())?;
    Some((u32_at(clip, 4)?, clip.get(8..)?))
}
