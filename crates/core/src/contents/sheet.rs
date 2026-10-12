//! A contact sheet of the pictures in an archive that has no loader of its
//! own (a Quake PAK, a Godot PCK, a Valve VPK, a Build GRP, a Unity
//! package's stored previews): the thumbnail, and the gallery the viewer
//! opens such a file on.  Reading pictures costs
//! time, so a thumbnail takes the first few within a short budget and stops.

use super::{Contents, ItemKind, PreviewKind, Source};
use slerp_scene::{Limits, LoadError, NamedImage};
use slerp_textures::DecodeOptions;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A thumbnail's budget: pictures to take and time to spend.
const THUMB_PICTURES: usize = 9;
const THUMB_TIME: Duration = Duration::from_millis(250);
/// The viewer's.
const VIEW_PICTURES: usize = 200;
const VIEW_TIME: Duration = Duration::from_secs(4);
/// Pictures larger than this (in the archive) are skipped by thumbnails.
const THUMB_MAX_BYTES: u64 = 4 << 20;
const VIEW_MAX_BYTES: u64 = 32 << 20;

/// The pictures of the archive at `path`, in archive order, each a texture
/// shrunk to the thumbnail size when `limits` ask for one.  An archive with
/// none gives `Unsupported`.
pub fn gallery(path: &Path, limits: &Limits) -> Result<Vec<NamedImage>, LoadError> {
    let started = Instant::now();
    let thumb = limits.max_texture_size.is_some();
    let (max_pictures, budget, max_bytes) =
        if thumb { (THUMB_PICTURES, THUMB_TIME, THUMB_MAX_BYTES) } else { (VIEW_PICTURES, VIEW_TIME, VIEW_MAX_BYTES) };
    let options = match limits.max_texture_size {
        Some(n) => DecodeOptions::preview(n),
        None => DecodeOptions::full(),
    };
    if crate::sniff_file(path)? == Some(crate::Kind::Archive(slerp_archive::ArchiveKind::UnityPackage)) {
        return unity_gallery(path, max_pictures, budget, &options);
    }
    let contents = Contents::open(path).map_err(|e| LoadError::invalid(e.to_string()))?;
    let mut out = Vec::new();
    for item in contents.items() {
        if out.len() >= max_pictures || started.elapsed() > budget {
            break;
        }
        let picture = matches!(item.kind, ItemKind::Picture | ItemKind::Sprite | ItemKind::Texture);
        if !picture || item.locked || item.size > max_bytes || !matches!(item.source, Source::Entry(_)) {
            continue;
        }
        let Ok(preview) = contents.preview(item.id) else { continue };
        let PreviewKind::File { bytes, .. } = preview.kind else { continue };
        let decoded =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| slerp_textures::decode(&bytes, &options)));
        if let Ok(Ok(texture)) = decoded {
            out.push(NamedImage { name: item.name().to_string(), texture: Arc::new(texture) });
        }
    }
    if out.is_empty() {
        return Err(LoadError::Unsupported("the archive holds no pictures to show".into()));
    }
    Ok(out)
}

/// A Unity package's pictures: the previews the Unity editor stored for its
/// assets (or its first picture asset when it has none), found by inflating
/// only as much of the package as it takes, under the assets' paths.
fn unity_gallery(
    path: &Path,
    max_pictures: usize,
    budget: Duration,
    options: &DecodeOptions,
) -> Result<Vec<NamedImage>, LoadError> {
    let pictures =
        slerp_archive::unity::pictures(path, max_pictures, budget).map_err(|e| LoadError::invalid(e.to_string()))?;
    let out: Vec<NamedImage> = pictures
        .into_iter()
        .filter_map(|p| {
            let decoded =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| slerp_textures::decode(&p.bytes, options)));
            let texture = decoded.ok()?.ok()?;
            Some(NamedImage { name: p.path, texture: Arc::new(texture) })
        })
        .collect();
    if out.is_empty() {
        return Err(LoadError::Unsupported("the package holds no preview pictures".into()));
    }
    Ok(out)
}
