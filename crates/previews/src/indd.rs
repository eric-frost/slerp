//! InDesign documents.  A `.indd` file is a database of 4 KB pages: two
//! master pages (the newer one, by its sequence number, is current, and
//! says how many pages the database has), the database pages, and then
//! "contiguous objects", each framed by a header and a trailer that start
//! with fixed GUIDs.  The XMP metadata is one of those objects, usually
//! the first, and its `xmpGImg:image` element holds a JPEG thumbnail of the
//! first spread in base64.  Only the master pages and the object headers
//! up to the XMP packet are read.  Database pages often hold stale copies
//! of earlier XMP packets, so searching the file for the first packet
//! would find an old thumbnail.

use crate::source::Source;
use base64::Engine;
use slerp_scene::textures::Texture;
use slerp_scene::{Limits, LoadError};

/// The master pages' GUID, followed by `DOCUMENT`.
const MASTER: &[u8] = b"\x06\x06\xED\xF5\xD8\x1D\x46\xE5\xBD\x31\xEF\xE7\xFE\x74\xB7\x1D";

/// The GUID starting every contiguous object's header.
const OBJECT: &[u8] = b"\xDE\x39\x39\x79\x51\x88\x4B\x6C\x8E\x63\xEE\xF8\xAE\xE0\xDD\x38";

const PAGE: u64 = 4096;

/// The most objects passed over looking for the XMP packet.
const MAX_OBJECTS: usize = 4096;

/// The largest XMP packet read.
const MAX_XMP: u64 = 64 << 20;

pub fn looks_like_indd(head: &[u8]) -> bool {
    head.starts_with(MASTER) && head.get(16..24) == Some(b"DOCUMENT")
}

pub fn preview(src: &mut Source, limits: &Limits) -> Result<Texture, LoadError> {
    let xmp = find_xmp(src)?.ok_or_else(|| crate::no_preview("InDesign document"))?;
    let jpeg = xmp_thumbnail(&xmp).ok_or_else(|| crate::no_preview("InDesign document"))?;
    crate::decode_image(&jpeg, limits)
}

/// The XMP packet, from the contiguous objects after the database pages.
fn find_xmp(src: &mut Source) -> Result<Option<Vec<u8>>, LoadError> {
    // 1. The current master page and its page count, little-endian
    //    whatever byte order the document's streams use.
    let m = src.exact(0, 2 * PAGE)?;
    let le = |b: &[u8]| u64::from_le_bytes(b.try_into().unwrap_or([0; 8]));
    let current = if le(&m[PAGE as usize + 264..PAGE as usize + 272]) > le(&m[264..272]) { PAGE as usize } else { 0 };
    let pages = u32::from_le_bytes(m[current + 280..current + 284].try_into().unwrap_or([0; 4])) as u64;
    // 2. The objects, one header at a time.
    let mut at = pages.checked_mul(PAGE).ok_or_else(|| LoadError::invalid("InDesign: bad page count"))?;
    for _ in 0..MAX_OBJECTS {
        let Ok(h) = src.exact(at, 32) else { return Ok(None) };
        if &h[..16] != OBJECT {
            return Ok(None);
        }
        let len = u32::from_le_bytes([h[24], h[25], h[26], h[27]]) as u64;
        // The XMP stream starts with its own length, then the packet.
        if len > 20 && src.exact(at + 32 + 4, 15).is_ok_and(|s| s == b"<?xpacket begin") {
            if len > MAX_XMP {
                return Err(LoadError::TooLarge("InDesign XMP packet".into()));
            }
            return src.exact(at + 32 + 4, len - 4).map(Some);
        }
        at = at.checked_add(32 + len + 32).ok_or_else(|| LoadError::invalid("InDesign: bad object length"))?;
    }
    Ok(None)
}

/// The largest thumbnail in an XMP packet: an `xmpGImg:image` element (or
/// attribute) of base64 text, whose line breaks XMP writers escape as
/// `&#xA;`.
fn xmp_thumbnail(xmp: &[u8]) -> Option<Vec<u8>> {
    let text = String::from_utf8_lossy(xmp);
    let mut best: Option<Vec<u8>> = None;
    let mut rest: &str = &text;
    while let Some(i) = rest.find("xmpGImg:image") {
        rest = &rest[i + "xmpGImg:image".len()..];
        // `<xmpGImg:image>...</` or `xmpGImg:image="..."`.
        let body = if let Some(r) = rest.strip_prefix('>') {
            &r[..r.find('<').unwrap_or(r.len())]
        } else if let Some(r) = rest.strip_prefix("=\"") {
            &r[..r.find('"').unwrap_or(r.len())]
        } else {
            continue;
        };
        let clean: String =
            body.replace("&#xA;", "").replace("&#xD;", "").chars().filter(|c| !c.is_whitespace()).collect();
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(clean.as_bytes())
            && best.as_ref().is_none_or(|b| bytes.len() > b.len())
        {
            best = Some(bytes);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_thumbnails_from_xmp() {
        let xmp = br#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><rdf:li><xmpGImg:format>JPEG</xmpGImg:format><xmpGImg:image>/9j/4AAQ&#xA;SkZJ</xmpGImg:image></rdf:li>"#;
        assert_eq!(xmp_thumbnail(xmp).unwrap(), [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49]);
        let attr = br#"<rdf:li xmpGImg:width="2" xmpGImg:image="/9j/4AAQ"/>"#;
        assert_eq!(xmp_thumbnail(attr).unwrap(), [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]);
        assert!(xmp_thumbnail(b"<x:xmpmeta/>").is_none());
    }
}
