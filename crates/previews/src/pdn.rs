//! Paint.NET images.  A `.pdn` file starts with `PDN3`, a 24-bit
//! little-endian length and that many bytes of XML describing the image,
//! whose `thumb` element carries the thumbnail as a base64 PNG in its `png`
//! attribute.  The layers follow, serialised .NET objects that aren't
//! read.

use crate::source::Source;
use base64::Engine;
use slerp_scene::textures::Texture;
use slerp_scene::{Limits, LoadError};

pub const MAGIC: &[u8] = b"PDN3";

pub fn looks_like_pdn(head: &[u8]) -> bool {
    head.starts_with(MAGIC) && head.get(7) == Some(&b'<')
}

pub fn preview(src: &mut Source, limits: &Limits) -> Result<Texture, LoadError> {
    let head = src.exact(0, 7)?;
    if !head.starts_with(MAGIC) {
        return Err(LoadError::invalid("not a Paint.NET image"));
    }
    // A 24-bit length caps the header at 16 MB.
    let len = u32::from_le_bytes([head[4], head[5], head[6], 0]) as u64;
    let xml = src.exact(7, len)?;
    let text = String::from_utf8_lossy(&xml);
    let Some(png) = thumb_attribute(&text) else { return Err(crate::no_preview("Paint.NET image")) };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(png.trim())
        .map_err(|e| LoadError::invalid(format!("Paint.NET thumbnail: {e}")))?;
    crate::decode_image(&bytes, limits)
}

/// The `png` attribute of the `thumb` element.  The header is a short,
/// machine-written document, so a string search does; an XML parser would
/// only add escapes that base64 never contains.
fn thumb_attribute(xml: &str) -> Option<&str> {
    let at = xml.find("<thumb")?;
    let rest = &xml[at..];
    let rest = &rest[rest.find("png=")? + 4..];
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let rest = &rest[1..];
    Some(&rest[..rest.find(quote)?])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_thumbnail_attribute() {
        let xml = r#"<pdnImage width="2" height="2"><custom><thumb png="iVBORw0KGgo=" /></custom></pdnImage>"#;
        assert_eq!(thumb_attribute(xml), Some("iVBORw0KGgo="));
        assert_eq!(thumb_attribute("<pdnImage><thumb/></pdnImage>"), None);
        assert_eq!(thumb_attribute("<thumb png=\"unterminated"), None);
    }
}
