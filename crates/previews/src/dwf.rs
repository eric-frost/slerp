//! Autodesk Design Web Format files: DWF 6 (`.dwf`, a zip after a 12-byte
//! `(DWF V06.00)` banner) and DWFx (`.dwfx`, the same content in an Open
//! Packaging zip that XPS viewers can also read).  The package's
//! `manifest.xml` lists its sections, one per sheet of a drawing or per 3D
//! model, and each section's resources; the one whose role is `thumbnail` is
//! a PNG or JPEG of that sheet or model.  Thumbnails show the first
//! section's, and the viewer offers every section's as a part.  A DWFx also
//! names its thumbnail in the package relationships (`_rels/.rels`), which
//! is used when the manifest names none.
//!
//! DWF files before version 6 (`(DWF V00.55)` and older) are bare W2D vector
//! streams with no picture, and some DWF 6 sheets have none either.  The
//! zips' entries often carry data descriptors, which the zip reader handles
//! by taking sizes from the central directory.  cad-preview
//! (github.com/cinderblock/cad-preview, MIT) found the banner and the
//! per-sheet pictures.

use crate::source::Source;
use crate::zip::Zip;
use slerp_scene::textures::Texture;
use slerp_scene::{Limits, LoadError};

/// The largest manifest and picture read.
const MAX_MANIFEST: u64 = 16 << 20;
const MAX_PICTURE: u64 = 64 << 20;

/// Most sections listed.
const MAX_SECTIONS: usize = 4096;

/// Is this the start of a DWF file?
pub fn looks_like_dwf(head: &[u8]) -> bool {
    head.starts_with(b"(DWF V")
}

/// A section with a picture: its title and the zip member holding it.
pub struct Sheet {
    pub title: String,
    pub member: String,
}

/// Every section's picture, in the manifest's order.
pub fn sheets(src: &mut Source) -> Result<Vec<Sheet>, LoadError> {
    let zip = open(src)?;
    sheets_in(&zip, src)
}

/// The zip, after the banner of a DWF file.  Files before DWF 6 have no zip.
fn open(src: &mut Source) -> Result<Zip, LoadError> {
    let head = src.head(16)?;
    if looks_like_dwf(&head) && !head.get(12..).is_some_and(|h| h.starts_with(b"PK\x03\x04")) {
        let banner = String::from_utf8_lossy(&head[..head.len().min(12)]).into_owned();
        return Err(LoadError::Unsupported(format!("{banner} files hold only vector drawings, without a picture")));
    }
    Zip::open(src)
}

fn sheets_in(zip: &Zip, src: &mut Source) -> Result<Vec<Sheet>, LoadError> {
    let mut out = Vec::new();
    // 1. Each manifest's sections (a DWFx can hold several documents).
    let manifests: Vec<String> = zip
        .entries
        .iter()
        .filter(|e| {
            let n = e.name.to_ascii_lowercase();
            n == "manifest.xml" || (n.starts_with("dwf/documents/") && n.ends_with("/manifest.xml"))
        })
        .map(|e| e.name.clone())
        .take(64)
        .collect();
    for name in manifests {
        let Some(xml) = zip.read_named(src, &name, MAX_MANIFEST)? else { continue };
        // AutoCAD 2004's packager ends the manifest with a NUL.
        let text = String::from_utf8_lossy(&xml);
        let Ok(doc) = roxmltree::Document::parse(text.trim_end_matches(['\0', ' ', '\r', '\n'])) else { continue };
        for section in doc.descendants().filter(|n| n.tag_name().name() == "Section").take(MAX_SECTIONS) {
            let Some(picture) = section.descendants().find(|n| {
                n.tag_name().name() == "Resource" && matches!(attr(*n, "role"), Some("thumbnail" | "preview"))
            }) else {
                continue;
            };
            let Some(member) = attr(picture, "href").and_then(|href| find_member(zip, href)) else { continue };
            let title = attr(section, "title").or_else(|| attr(section, "name")).unwrap_or("Sheet");
            out.push(Sheet { title: unique(&out, title), member });
        }
    }
    // 2. A DWFx's package thumbnail.
    if out.is_empty()
        && let Some(rels) = zip.read_named(src, "_rels/.rels", MAX_MANIFEST)?
    {
        let text = String::from_utf8_lossy(&rels);
        if let Ok(doc) = roxmltree::Document::parse(&text) {
            let target = doc.descendants().find_map(|n| {
                let ty = n.attribute("Type")?;
                ty.ends_with("/metadata/thumbnail").then(|| n.attribute("Target")).flatten()
            });
            if let Some(member) = target.and_then(|t| find_member(zip, t)) {
                out.push(Sheet { title: "Thumbnail".into(), member });
            }
        }
    }
    Ok(out)
}

/// An attribute by its name without the namespace (DWF writers differ on
/// `role` and `dwf:role`).
fn attr<'a>(n: roxmltree::Node<'a, '_>, key: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == key).map(|a| a.value())
}

/// The zip member a manifest's link names: DWF links use backslashes and
/// DWFx links start at the package root.
fn find_member(zip: &Zip, href: &str) -> Option<String> {
    let want = href.trim_start_matches('/').replace('\\', "/");
    zip.entries.iter().find(|e| e.name.replace('\\', "/").eq_ignore_ascii_case(&want)).map(|e| e.name.clone())
}

/// A title not yet used, so parts can be asked for by name.
fn unique(sheets: &[Sheet], title: &str) -> String {
    let mut name = title.to_string();
    let mut n = 2;
    while sheets.iter().any(|s| s.title == name) {
        name = format!("{title} ({n})");
        n += 1;
    }
    name
}

/// The picture of the section called `title`, or of the first section.
pub fn preview(src: &mut Source, limits: &Limits, title: Option<&str>) -> Result<Texture, LoadError> {
    let zip = open(src)?;
    let sheets = sheets_in(&zip, src)?;
    let sheet = match title {
        Some(t) => sheets.iter().find(|s| s.title == t).ok_or_else(|| LoadError::invalid("no such sheet"))?,
        None => sheets.first().ok_or_else(|| crate::no_preview("DWF file"))?,
    };
    let bytes =
        zip.read_named(src, &sheet.member, MAX_PICTURE)?.ok_or_else(|| LoadError::invalid("DWF: missing picture"))?;
    let mut texture = crate::decode_image(&bytes, limits)?;
    if sheets.len() > 1 {
        texture.source.header.push(("Sheet".into(), sheet.title.clone()));
    }
    Ok(texture)
}
