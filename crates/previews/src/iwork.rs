//! Apple Keynote, Pages and Numbers documents, shown by the picture of the
//! first slide, page or sheet that iWork saves in them.
//!
//! - iWork 2013 and later write a zip holding `preview.jpg` (up to 1024
//!   pixels across), `preview-web.jpg` (about 200) and `preview-micro.jpg`
//!   (about 50) next to the document's `Index/` protocol buffers.
//! - iWork '09 wrote a zip with `QuickLook/Thumbnail.jpg` (512 pixels
//!   across; Pages adds `QuickLook/Preview.pdf`), and Keynote '08 a slide
//!   thumbnail `thumbs/st0.jpg`.
//! - A document saved as a package folder and then zipped (as Finder does
//!   when it sends one) keeps the same members one folder down.
//!
//! Package folders themselves are directories, which file managers never
//! hand to a thumbnailer (they show folders by their own means), so they
//! aren't read.  Keynote 1 to 3 kept TIFF slide thumbnails, and
//! password-protected documents keep no picture; neither has a preview
//! here.

use crate::source::Source;
use crate::zip::Zip;
use slerp_scene::textures::Texture;
use slerp_scene::{Limits, LoadError};

/// The largest picture inflated.
const MAX_PICTURE: u64 = 32 << 20;

/// The members that hold the picture, best first: the large preview, then
/// the medium one, which beats '09's for small thumbnails only.
const MEMBERS: [&str; 6] = [
    "preview.jpg",
    "QuickLook/Thumbnail.jpg",
    "QuickLook/Thumbnail.png",
    "preview-web.jpg",
    "thumbs/st0.jpg",
    "preview-micro.jpg",
];

/// The extensions of the three apps' documents and templates.
pub fn is_iwork_extension(ext: &str) -> bool {
    matches!(ext, "key" | "pages" | "numbers" | "kth" | "template" | "nmbtemplate")
}

pub fn preview(src: &mut Source, limits: &Limits) -> Result<Texture, LoadError> {
    let zip = Zip::open(src)?;
    for name in MEMBERS {
        // The member itself, or the same one inside a zipped package
        // folder (`Presentation.key/preview.jpg`).
        let entry = zip.find(name).or_else(|| {
            zip.entries.iter().find(|e| {
                e.name.split_once('/').is_some_and(|(folder, rest)| {
                    !folder.is_empty() && !rest.contains("../") && rest.eq_ignore_ascii_case(name)
                })
            })
        });
        if let Some(entry) = entry {
            let bytes = zip.read(src, entry, MAX_PICTURE)?;
            return crate::decode_image(&bytes, limits);
        }
    }
    Err(crate::no_preview("iWork document"))
}
