//! Preview pictures that 3D and creative applications store inside their
//! own files, for formats whose content Slerp can't read: closed formats,
//! or ones only their application can evaluate.  Each becomes `Asset::Image` of
//! its preview, which thumbnails show and the viewer opens as a picture.
//!
//! - **3ds Max** (`.max`): an OLE compound document.  Max writes a
//!   thumbnail into the standard summary information property set, as a
//!   Windows metafile wrapping a DIB, the way Windows Explorer once read
//!   it.
//! - **SketchUp** (`.skp`): versions 6 to 2020 store a PNG near the start
//!   of the file; 2021 and later keep the file in a zip whose first entry
//!   is `meta/model_thumbnail.png`.
//! - **Cinema 4D** (`.c4d`): R13 and later store a JPEG of the document
//!   right after the file's first header.
//! - **ZBrush** (`.ztl` tools, `.zpr` projects): a small run-length
//!   compressed picture is the first block after the header.
//!
//! Maya files (`.ma`, `.mb`) carry no preview picture, so they aren't
//! claimed.
//!
//! The same goes for creative apps' documents, which Slerp shows by the
//! preview picture the app saved in them:
//!
//! - **Procreate** (`.procreate`), **Sketch** (`.sketch`), **Figma** local
//!   copies (`.fig`), **Adobe XD** (`.xd`) and **FreeCAD** (`.FCStd`) are
//!   zips with a PNG member; older Figma copies are bare `fig-kiwi` files
//!   ending in a PNG chunk.
//! - **Affinity** Photo, Designer and Publisher (`.afphoto`, `.afdesign`,
//!   `.afpub`, and the single `.af` format) keep a PNG in their own
//!   container.
//! - **Clip Studio Paint** (`.clip`) keeps a PNG in an SQLite database
//!   inside its own chunked file.
//! - **Paint.NET** (`.pdn`) has a base64 PNG in its XML header.
//! - **InDesign** (`.indd`) has a base64 JPEG in its XMP metadata.
//! - **CorelDRAW** (`.cdr`, `.cdt`, `.cmx`): versions 7 to 13 are RIFF files
//!   whose `DISP` chunk holds a 96x96 DIB; X4 to X6 are zips with a BMP and
//!   X7 and later zips with a PNG (see `corel`).
//! - **Visio** (`.vsdx` and the other Open Packaging forms, `.vsd`, `.vss`,
//!   `.vst`): a small Enhanced or Windows metafile in `docProps/thumbnail.emf`
//!   or in the summary information of an OLE file.  Only `slerp-metafile` can
//!   draw it, so these come back as [`Stored::Metafile`] for the caller.
//!
//! And CAD programs' and more creative apps' files (each module says
//! where its picture lives):
//!
//! - **AutoCAD** `.dwg` (`dwg`), **DWF** and **DWFx** (`dwf`, one picture
//!   per sheet, offered as [`parts`]).
//! - **SolidWorks**, **Inventor**, **Revit** and **Solid Edge** (`cad`):
//!   OLE streams and property sets, and SolidWorks 2015's own records.
//! - **Fusion** `.f3d` (`fusion`): a zip member, Zstandard compressed in
//!   newer files.
//! - **Keynote**, **Pages** and **Numbers** (`iwork`), **Pixelmator Pro**
//!   (`zipped`): pictures in zips.
//! - **Paint Shop Pro** (`psp`): the composite image bank.
//! - **PaintTool SAI** (`sai`): an encrypted page file system (SAI 1) and
//!   a canvas with a JSSF thumbnail and a merged picture (SAI 2).
//!
//! A file's own picture is always what thumbnails show; the viewer takes
//! the largest one a file keeps (a merged canvas over a thumbnail).
//!
//! This crate also reads colour palettes (see [`palette`]), which share
//! its seeking reader and zip reader.
//!
//! The files can be gigabytes, and the pictures sit near their start (or
//! behind a short chain of OLE sectors), so the readers seek to what they
//! need instead of reading the whole file.  Everything they read is
//! bounds-checked and capped; damaged files are errors, never panics.

mod affinity;
mod c4d;
mod cad;
mod clip;
mod corel;
mod dwf;
mod dwg;
mod fusion;
mod indd;
mod iwork;
mod max;
mod ole;
pub mod palette;
mod pdn;
mod psp;
mod sai;
mod skp;
mod source;
mod sqlite;
mod summary;
mod visio;
mod zbrush;
mod zip;
mod zipped;

#[cfg(test)]
mod synthetic;
#[cfg(test)]
mod synthetic_cad;
#[cfg(test)]
mod synthetic_paint;
#[cfg(test)]
mod tests;

use slerp_scene::textures::{DecodeOptions, Texture};
use slerp_scene::{Asset, Limits, LoadError};
use std::path::Path;
use std::sync::Arc;

pub use source::Source;

/// The formats whose previews this crate reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewFormat {
    /// A 3ds Max scene (`.max`).
    Max,
    /// A SketchUp model (`.skp`).
    SketchUp,
    /// A Cinema 4D scene (`.c4d`).
    Cinema4d,
    /// A ZBrush tool, project or document (`.ztl`, `.zpr`, `.zbr`).
    ZBrush,
    /// A Procreate artwork (`.procreate`).
    Procreate,
    /// An Affinity Photo, Designer or Publisher document (`.afphoto`,
    /// `.afdesign`, `.afpub`, `.af`).
    Affinity,
    /// A Sketch document (`.sketch`).
    Sketch,
    /// A Figma file saved locally (`.fig`).
    Figma,
    /// A Clip Studio Paint illustration (`.clip`).
    ClipStudio,
    /// A Paint.NET image (`.pdn`).
    PaintNet,
    /// An InDesign document (`.indd`).
    InDesign,
    /// An Adobe XD document (`.xd`).
    AdobeXd,
    /// A FreeCAD document (`.FCStd`).
    FreeCad,
    /// A CorelDRAW drawing or template (`.cdr`, `.cdt`) or Presentation
    /// Exchange file (`.cmx`).
    CorelDraw,
    /// A Visio drawing, template or stencil (`.vsdx`, `.vsdm`, `.vstx`,
    /// `.vssx`, `.vsd`, `.vss`, `.vst`).
    Visio,
    /// An AutoCAD drawing (`.dwg`).
    Dwg,
    /// An Autodesk Design Web Format file (`.dwf`, `.dwfx`).
    Dwf,
    /// A SolidWorks part, assembly or drawing (`.sldprt`, `.sldasm`,
    /// `.slddrw`).
    SolidWorks,
    /// An Autodesk Inventor part, assembly, drawing or presentation (`.ipt`,
    /// `.iam`, `.idw`, `.ipn`).
    Inventor,
    /// An Autodesk Revit project, family or template (`.rvt`, `.rfa`,
    /// `.rte`).
    Revit,
    /// A Solid Edge part, sheet metal part, assembly or draft (`.par`,
    /// `.psm`, `.asm`, `.dft`).
    SolidEdge,
    /// An Autodesk Fusion design (`.f3d`).
    Fusion,
    /// A Pixelmator Pro document (`.pxd`).
    Pixelmator,
    /// A PaintTool SAI document (`.sai`, `.sai2`).
    Sai,
    /// A Paint Shop Pro image, frame or picture tube (`.psp`, `.pspimage`,
    /// `.pspframe`, `.psptube`, `.tub`).
    PaintShopPro,
    /// An Apple Keynote, Pages or Numbers document (`.key`, `.pages`,
    /// `.numbers`).
    Iwork,
}

impl PreviewFormat {
    pub fn name(self) -> &'static str {
        match self {
            PreviewFormat::Max => "3ds Max scene",
            PreviewFormat::SketchUp => "SketchUp model",
            PreviewFormat::Cinema4d => "Cinema 4D scene",
            PreviewFormat::ZBrush => "ZBrush file",
            PreviewFormat::Procreate => "Procreate artwork",
            PreviewFormat::Affinity => "Affinity document",
            PreviewFormat::Sketch => "Sketch document",
            PreviewFormat::Figma => "Figma file",
            PreviewFormat::ClipStudio => "Clip Studio Paint file",
            PreviewFormat::PaintNet => "Paint.NET image",
            PreviewFormat::InDesign => "InDesign document",
            PreviewFormat::AdobeXd => "Adobe XD document",
            PreviewFormat::FreeCad => "FreeCAD document",
            PreviewFormat::CorelDraw => "CorelDRAW drawing",
            PreviewFormat::Visio => "Visio drawing",
            PreviewFormat::Dwg => "AutoCAD drawing",
            PreviewFormat::Dwf => "DWF file",
            PreviewFormat::SolidWorks => "SolidWorks file",
            PreviewFormat::Inventor => "Inventor file",
            PreviewFormat::Revit => "Revit file",
            PreviewFormat::SolidEdge => "Solid Edge file",
            PreviewFormat::Fusion => "Fusion design",
            PreviewFormat::PaintShopPro => "Paint Shop Pro image",
            PreviewFormat::Pixelmator => "Pixelmator Pro document",
            PreviewFormat::Sai => "PaintTool SAI document",
            PreviewFormat::Iwork => "iWork document",
        }
    }
}

impl PreviewFormat {
    fn cad(program: cad::Program) -> PreviewFormat {
        match program {
            cad::Program::SolidWorks => PreviewFormat::SolidWorks,
            cad::Program::Inventor => PreviewFormat::Inventor,
            cad::Program::Revit => PreviewFormat::Revit,
            cad::Program::SolidEdge => PreviewFormat::SolidEdge,
        }
    }

    fn program(self) -> cad::Program {
        match self {
            PreviewFormat::Inventor => cad::Program::Inventor,
            PreviewFormat::Revit => cad::Program::Revit,
            PreviewFormat::SolidEdge => cad::Program::SolidEdge,
            _ => cad::Program::SolidWorks,
        }
    }
}

/// Recognises a file by its first bytes, and where the container is a
/// common one (OLE for 3ds Max, zip for the others) by its extension too.
pub fn sniff(path: &Path, head: &[u8]) -> Option<PreviewFormat> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    if head.starts_with(ole::MAGIC) {
        return match ext.as_str() {
            "max" => Some(PreviewFormat::Max),
            "vsd" | "vss" | "vst" => Some(PreviewFormat::Visio),
            e => cad::Program::of_extension(e).or_else(|| cad::solid_edge_by_class(head)).map(PreviewFormat::cad),
        };
    }
    if cad::Program::of_extension(&ext) == Some(cad::Program::SolidWorks) && cad::looks_like_modern_solidworks(head) {
        return Some(PreviewFormat::SolidWorks);
    }
    if corel::looks_like_riff(head) {
        return Some(PreviewFormat::CorelDraw);
    }
    if zip::is_zip(head) {
        return match ext.as_str() {
            "cdr" | "cdt" => Some(PreviewFormat::CorelDraw),
            "vsdx" | "vsdm" | "vstx" | "vstm" | "vssx" | "vssm" => Some(PreviewFormat::Visio),
            "procreate" => Some(PreviewFormat::Procreate),
            "sketch" => Some(PreviewFormat::Sketch),
            "fig" => Some(PreviewFormat::Figma),
            "xd" => Some(PreviewFormat::AdobeXd),
            "fcstd" => Some(PreviewFormat::FreeCad),
            e if iwork::is_iwork_extension(e) => Some(PreviewFormat::Iwork),
            "dwfx" => Some(PreviewFormat::Dwf),
            "f3d" => Some(PreviewFormat::Fusion),
            "pxd" => Some(PreviewFormat::Pixelmator),
            // Under another name, as the MIME database knows them: by the
            // first member.
            _ => match zip::first_name(head) {
                Some(n) if n.starts_with(b"FusionAssetName[") => Some(PreviewFormat::Fusion),
                Some(b"metadata.info") => Some(PreviewFormat::Pixelmator),
                _ => None,
            },
        };
    }
    if head.starts_with(zipped::FIG_KIWI) {
        return Some(PreviewFormat::Figma);
    }
    if sai::looks_like_sai2(head) || sai::looks_like_sai1(head) {
        return Some(PreviewFormat::Sai);
    }
    if psp::looks_like_psp(head) {
        return Some(PreviewFormat::PaintShopPro);
    }
    if dwf::looks_like_dwf(head) {
        return Some(PreviewFormat::Dwf);
    }
    if dwg::looks_like_dwg(head) {
        return Some(PreviewFormat::Dwg);
    }
    if skp::looks_like_skp(head) {
        return Some(PreviewFormat::SketchUp);
    }
    if c4d::looks_like_c4d(head) {
        return Some(PreviewFormat::Cinema4d);
    }
    if zbrush::looks_like_zbrush(head) {
        return Some(PreviewFormat::ZBrush);
    }
    if affinity::looks_like_affinity(head) {
        return Some(PreviewFormat::Affinity);
    }
    if clip::looks_like_clip(head) {
        return Some(PreviewFormat::ClipStudio);
    }
    if pdn::looks_like_pdn(head) {
        return Some(PreviewFormat::PaintNet);
    }
    if indd::looks_like_indd(head) {
        return Some(PreviewFormat::InDesign);
    }
    None
}

/// A preview as stored: a picture decoded here, or a Windows metafile that
/// only the metafile renderer can draw (Visio's thumbnails).
pub enum Stored {
    Image(Texture),
    /// The bytes of an EMF or WMF.
    Metafile(Vec<u8>),
}

/// Reads the preview picture of the file at `path`.  Files without one
/// are `LoadError::Unsupported`.  Previews that are metafiles are errors
/// here; `read_stored_path` gives them to a caller that can draw them.
pub fn load_path(format: PreviewFormat, path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let mut src = Source::open(path)?;
    let texture = load_source(format, &mut src, limits)?;
    Ok(Asset::Image(Arc::new(texture)))
}

/// Reads the preview of the file at `path` as it is stored.
pub fn read_stored_path(format: PreviewFormat, path: &Path, limits: &Limits) -> Result<Stored, LoadError> {
    read_stored(format, &mut Source::open(path)?, limits)
}

/// The names of a file's parts, when it holds several pictures shown one
/// at a time: the sheets and models of a DWF.  Empty for files with one
/// picture, and for unreadable ones.
pub fn parts(format: PreviewFormat, path: &Path) -> Vec<String> {
    let sheets = match format {
        PreviewFormat::Dwf => Source::open(path).and_then(|mut src| dwf::sheets(&mut src)),
        _ => return Vec::new(),
    };
    match sheets {
        Ok(sheets) if sheets.len() > 1 => sheets.into_iter().map(|s| s.title).collect(),
        _ => Vec::new(),
    }
}

/// The picture of the part called `name`, one of those `parts` lists.
pub fn load_part(format: PreviewFormat, path: &Path, limits: &Limits, name: &str) -> Result<Asset, LoadError> {
    let mut src = Source::open(path)?;
    let mut texture = match format {
        PreviewFormat::Dwf => dwf::preview(&mut src, limits, Some(name))?,
        _ => return Err(LoadError::Unsupported("this file has no parts".into())),
    };
    texture.source.container = format.name().to_string();
    texture.source.file_bytes = src.len();
    Ok(Asset::Image(Arc::new(texture)))
}

/// The preview of a file already opened, as a texture.
pub fn load_source(format: PreviewFormat, src: &mut Source, limits: &Limits) -> Result<Texture, LoadError> {
    match read_stored(format, src, limits)? {
        Stored::Image(t) => Ok(t),
        Stored::Metafile(_) => Err(LoadError::Unsupported(format!("the picture in a {} is a metafile", format.name()))),
    }
}

/// The preview of a file already opened, as it is stored.
pub fn read_stored(format: PreviewFormat, src: &mut Source, limits: &Limits) -> Result<Stored, LoadError> {
    if src.len() > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{} bytes", src.len())));
    }
    let stored = match format {
        PreviewFormat::CorelDraw => corel::preview(src, limits)?,
        PreviewFormat::Visio => visio::preview(src, limits)?,
        PreviewFormat::Dwg => dwg::preview(src, limits)?,
        PreviewFormat::Dwf => Stored::Image(dwf::preview(src, limits, None)?),
        PreviewFormat::SolidWorks | PreviewFormat::Inventor | PreviewFormat::Revit | PreviewFormat::SolidEdge => {
            Stored::Image(cad::preview(format.program(), src, limits)?)
        }
        PreviewFormat::Fusion => Stored::Image(fusion::preview(src, limits)?),
        PreviewFormat::PaintShopPro => Stored::Image(psp::preview(src, limits)?),
        PreviewFormat::Sai => {
            let head = src.head(16)?;
            Stored::Image(if sai::looks_like_sai2(&head) {
                sai::sai2_preview(src, limits)?
            } else {
                sai::sai1_preview(src, limits)?
            })
        }
        PreviewFormat::Iwork => Stored::Image(iwork::preview(src, limits)?),
        PreviewFormat::Max => Stored::Image(max::preview(src, limits)?),
        PreviewFormat::SketchUp => Stored::Image(skp::preview(src, limits)?),
        PreviewFormat::Cinema4d => Stored::Image(c4d::preview(src, limits)?),
        PreviewFormat::ZBrush => Stored::Image(zbrush::preview(src)?),
        PreviewFormat::Procreate
        | PreviewFormat::Sketch
        | PreviewFormat::Figma
        | PreviewFormat::AdobeXd
        | PreviewFormat::FreeCad
        | PreviewFormat::Pixelmator => Stored::Image(zipped::preview(format, src, limits)?),
        PreviewFormat::Affinity => Stored::Image(affinity::preview(src, limits)?),
        PreviewFormat::ClipStudio => Stored::Image(clip::preview(src, limits)?),
        PreviewFormat::PaintNet => Stored::Image(pdn::preview(src, limits)?),
        PreviewFormat::InDesign => Stored::Image(indd::preview(src, limits)?),
    };
    Ok(match stored {
        Stored::Image(mut texture) => {
            texture.source.container = format.name().to_string();
            texture.source.file_bytes = src.len();
            Stored::Image(texture)
        }
        metafile => metafile,
    })
}

/// Decodes an embedded PNG, JPEG or BMP at the size the caller wants.
fn decode_image(bytes: &[u8], limits: &Limits) -> Result<Texture, LoadError> {
    let opts = limits.max_texture_size.map_or_else(DecodeOptions::full, DecodeOptions::preview);
    let mut t = slerp_scene::textures::decode(bytes, &opts)?;
    // The picture is what the file shows, not the file's own format.
    t.source.format = std::mem::take(&mut t.source.container);
    Ok(t)
}

/// Turns a device-independent bitmap (a BMP file without its 14-byte file
/// header, as the clipboard and many applications' files keep pictures) into
/// a BMP file, or `None` when it doesn't start with a bitmap header.
fn bmp_from_dib(dib: &[u8]) -> Option<Vec<u8>> {
    let u32_at = |i: usize| Some(u32::from_le_bytes(dib.get(i..i + 4)?.try_into().ok()?));
    let header = u32_at(0)?;
    if !matches!(header, 40 | 52 | 56 | 108 | 124) {
        return None;
    }
    let bits = u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?);
    Some(max::bmp_file(dib, header, bits, u32_at(16)?))
}

fn no_preview(what: &str) -> LoadError {
    LoadError::Unsupported(format!("{what} without a preview picture"))
}
