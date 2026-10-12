//! Visio drawings.  The Open Packaging files (`.vsdx`, `.vsdm`, `.vstx`,
//! `.vssx`) are zips with the picture at `docProps/thumbnail.emf`, a tiny
//! Enhanced Metafile that usually just blits a bitmap of the first page.
//! The old binary files (`.vsd`, `.vss`, `.vst`) are OLE documents whose
//! summary information holds the thumbnail as clipboard data, again mostly an
//! Enhanced Metafile (`CF_ENHMETAFILE`), in Visio 5 a Windows metafile
//! picture (`CF_METAFILEPICT`).  Both need the metafile renderer.

use crate::Stored;
use crate::source::Source;
use crate::summary::{CF_DIB, CF_ENHMETAFILE, CF_METAFILEPICT};
use crate::zip::{self, Zip};
use slerp_scene::{Limits, LoadError};

const MAX_PICTURE: u64 = 64 << 20;

/// The thumbnail members of an Open Packaging file, best first: bitmaps need
/// no replay, so they come before metafiles.
const MEMBERS: [&str; 6] = [
    "docProps/thumbnail.png",
    "docProps/thumbnail.jpeg",
    "docProps/thumbnail.jpg",
    "docProps/thumbnail.emf",
    "docProps/thumbnail.wmf",
    "docProps/thumbnail.gif",
];

pub fn preview(src: &mut Source, limits: &Limits) -> Result<Stored, LoadError> {
    let head = src.head(8)?;
    if zip::is_zip(&head) {
        let zip = Zip::open(src)?;
        for name in MEMBERS {
            let Some(bytes) = zip.read_named(src, name, MAX_PICTURE)? else { continue };
            return if name.ends_with(".emf") || name.ends_with(".wmf") {
                Ok(Stored::Metafile(bytes))
            } else {
                crate::decode_image(&bytes, limits).map(Stored::Image)
            };
        }
        return Err(crate::no_preview("Visio drawing"));
    }
    let clip = crate::summary::thumbnail_clip(src)?.ok_or_else(|| crate::no_preview("Visio drawing"))?;
    let (format, data) = crate::summary::windows_format(&clip)
        .ok_or_else(|| LoadError::Unsupported("Visio thumbnail in an unknown clipboard format".into()))?;
    match format {
        CF_ENHMETAFILE => Ok(Stored::Metafile(data.to_vec())),
        // A metafile picture: mapping mode and size, then the metafile.
        CF_METAFILEPICT => Ok(Stored::Metafile(data.get(8..).unwrap_or_default().to_vec())),
        CF_DIB => {
            let bmp = crate::bmp_from_dib(data).ok_or_else(|| LoadError::invalid("Visio thumbnail: not a bitmap"))?;
            crate::decode_image(&bmp, limits).map(Stored::Image)
        }
        f => Err(LoadError::Unsupported(format!("Visio thumbnail in clipboard format {f}"))),
    }
}
