//! Substance archives (`.sbsar`): 7z archives holding a compiled material
//! graph (`.sbsasm`), its description (`.xml`) and sometimes an icon.  The
//! graph needs Substance's own engine to run, so the thumbnail is the icon:
//! an image file in the archive, preferring names with "icon", "thumb" or
//! "preview" in them, or an image the description names in an `icon`
//! attribute.  Archives without one get no thumbnail.

use slerp_archive::{Archive, ArchiveError};
use slerp_scene::{Asset, Limits, LoadError};
use std::sync::Arc;

/// Largest block the archive may say it unpacks to, and largest icon or
/// description read from it.  Real archives are tens of megabytes; these
/// stop a small file from claiming gigabytes.
const MAX_BLOCK_BYTES: u64 = 1 << 30;
const MAX_ENTRY_BYTES: u64 = 64 << 20;
/// Largest LZMA2 dictionary accepted: real archives' are far smaller, and a
/// damaged header's would only waste time allocating.
const MAX_DICT_BYTES: u64 = 256 << 20;

fn invalid(e: ArchiveError) -> LoadError {
    match e {
        ArchiveError::TooLarge(m) => LoadError::TooLarge(m),
        e => LoadError::invalid(format!("7z: {e}")),
    }
}

pub(crate) fn load(bytes: &[u8], limits: &Limits) -> Result<Asset, LoadError> {
    // 1. Open the archive, refusing ones that claim to unpack to more than
    //    a real one would.
    let caps = slerp_archive::Limits {
        max_block_bytes: MAX_BLOCK_BYTES,
        max_dictionary_bytes: MAX_DICT_BYTES,
        max_read: MAX_ENTRY_BYTES,
        ..Default::default()
    };
    let archive = Archive::from_bytes_with(bytes, caps).map_err(invalid)?;

    // 2. Find the icon: by file name, or named in the description.
    let entries: Vec<(String, u64)> = archive.entries().iter().map(|e| (e.path.clone(), e.size)).collect();
    let read = |name: &str| -> Result<Vec<u8>, LoadError> {
        let at = *archive.find(name).first().ok_or_else(|| LoadError::invalid(format!("7z: {name} is missing")))?;
        let entry = archive.entry(at).ok_or_else(|| LoadError::invalid("7z: no such entry"))?;
        archive.read(entry).map_err(invalid)
    };
    let mut icon = entries
        .iter()
        .filter(|(name, size)| is_image(name) && *size <= MAX_ENTRY_BYTES)
        .max_by_key(|(name, _)| score(name))
        .map(|(name, _)| name.clone());
    if icon.is_none()
        && let Some((xml, _)) =
            entries.iter().find(|(n, s)| n.to_ascii_lowercase().ends_with(".xml") && *s <= MAX_ENTRY_BYTES)
    {
        let text = read(xml)?;
        icon = named_icon(&String::from_utf8_lossy(&text), &entries);
    }
    let Some(icon) = icon else {
        return Err(LoadError::Unsupported("Substance archive without an icon".into()));
    };

    // 3. Decode it.
    let data = read(&icon)?;
    let texture =
        crate::realize::decode_bytes(&data, limits).map_err(|e| LoadError::invalid(format!("{icon}: {e}")))?;
    Ok(Asset::Image(Arc::new(texture)))
}

fn is_image(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".webp", ".tga", ".bmp"].iter().any(|e| lower.ends_with(e))
}

fn score(name: &str) -> u32 {
    let lower = name.to_ascii_lowercase();
    let file = lower.rsplit('/').next().unwrap_or(&lower);
    if file.contains("icon") {
        3
    } else if file.contains("thumb") || file.contains("preview") {
        2
    } else {
        1
    }
}

/// An archive entry the description names in an `icon="..."` attribute.
fn named_icon(xml: &str, entries: &[(String, u64)]) -> Option<String> {
    let mut rest = xml;
    while let Some(i) = rest.find("icon=\"") {
        rest = &rest[i + 6..];
        let value = &rest[..rest.find('"')?];
        let file = value.rsplit(['/', '\\']).next().unwrap_or(value);
        if let Some((name, _)) =
            entries.iter().find(|(n, s)| n.rsplit('/').next() == Some(file) && *s <= MAX_ENTRY_BYTES)
        {
            return Some(name.clone());
        }
    }
    None
}
