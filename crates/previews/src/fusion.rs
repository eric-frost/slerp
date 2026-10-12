//! Autodesk Fusion design archives (`.f3d`): a zip of the design's streams
//! with the picture Fusion saved at
//! `FusionAssetName[Active]/Previews/small.png` (256 pixels square) and, in
//! some files, `Previews/big.png` beside it.  Files saved since early 2026
//! compress every member with Zstandard (zip method 93), the pictures too.
//!
//! Other programs use `.f3d` as well: Bazis-Mebelshchik furniture designs
//! start with `BZ85` or `BZ80` and aren't zips, so they aren't claimed.

use crate::source::Source;
use crate::zip::{Entry, Zip};
use slerp_scene::textures::Texture;
use slerp_scene::{Limits, LoadError};

const MAX_PICTURE: u64 = 64 << 20;

pub fn preview(src: &mut Source, limits: &Limits) -> Result<Texture, LoadError> {
    let zip = Zip::open(src)?;
    // The large picture for the viewer, the small one for thumbnails; the
    // active design's before any other's.
    let small = limits.max_texture_size.is_some_and(|n| n <= 256);
    let names: &[&str] = if small {
        &["/Previews/small.png", "/Previews/big.png"]
    } else {
        &["/Previews/big.png", "/Previews/small.png"]
    };
    let find = |suffix: &str| -> Option<&Entry> {
        let mut matching = zip.entries.iter().filter(|e| e.name.ends_with(suffix));
        let first = matching.clone().next();
        matching.find(|e| e.name.contains("[Active]")).or(first)
    };
    for suffix in names {
        if let Some(entry) = find(suffix) {
            let png = zip.read(src, entry, MAX_PICTURE)?;
            return crate::decode_image(&png, limits);
        }
    }
    Err(crate::no_preview("Fusion design"))
}
