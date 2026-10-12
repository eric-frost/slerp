//! Decodes GPU texture containers (KTX2, KTX1, DDS, raw ASTC), Radiance HDR
//! and ordinary images (PNG, JPEG, WebP, TGA, BMP, PCX) on the CPU, plus the
//! science and film formats (FITS, XISF, DPX, Cineon, JPEG XR, PAM and
//! farbfeld), and turns any of them into a thumbnail.  Nothing here needs a
//! GPU.
//!
//! Block formats decode to RGBA: BC1-7 and BC6H, ETC1/ETC2/EAC, ASTC LDR and
//! HDR (2D footprints, and 3D ones as volume textures), PVRTC1, ATC, and
//! every Basis Universal codec (ETC1S, UASTC LDR and HDR, XUASTC, XUBC7).
//! Uncompressed formats cover Vulkan's 8/16/32/64-bit, packed, half and
//! float formats, depth, DXGI and legacy DDS layouts, and DDS's video
//! formats (AYUV, Y410, Y416, YUY2, UYVY, Y210, Y216, NV12, P010, P016,
//! NV11), converted from YUV as texconv does.
//!
//! Linear measurements (astronomy frames, XISF) decode to float values in
//! the file's own units, tagged `(stretch)`, because their light sits in the
//! darkest few percent and needs a display stretch to be seen: the
//! [`stretch`] module has the automatic one thumbnails use and the pieces
//! the viewer's controls adjust.  Film scans decode to what a print or
//! monitor would show (8-bit sRGB) or, when linear, to float.
//!
//! Every decoder treats its input as hostile: files come from downloads as
//! often as from game projects, so sizes are checked before anything is
//! allocated (see [`MAX_DECODED_BYTES`]) and a malformed file returns an
//! error rather than panicking.
//!
//! Asking for less is much cheaper than decoding everything: with
//! [`DecodeOptions::min_size`] or `max_size` set, containers decode only the
//! nearest mip level, JPEGs decode at 1/2, 1/4 or 1/8 scale, WebP stills
//! shrink inside libwebp, JPEG XR skips its finest detail, and large PNG,
//! Radiance, FITS, XISF, DPX and Cineon files are averaged down while they
//! are read.

mod assemble;
mod astc;
mod astc3d;
mod basis;
mod bayer;
mod blocks;
mod color;
mod dds;
mod film;
mod fits;
mod formats;
mod hdr;
mod images;
mod jxr;
mod ktx1;
mod ktx2;
mod pam;
mod reduce;
mod resize;
pub mod stretch;
mod thumb;
mod types;
mod util;
mod xisf;
mod yuv;

pub use types::*;
pub use util::{MAX_DECODED_BYTES, MAX_DIMENSION};

use std::path::Path;

/*┌─────────────────╮
| Public API      |
└─────────────────╯*/
//region Public API

/// Recognises a format from the first bytes of a file (32 are enough).
pub fn sniff(head: &[u8]) -> Option<Format> {
    const KTX2: &[u8] = b"\xabKTX 20\xbb\r\n\x1a\n";
    const KTX1: &[u8] = b"\xabKTX 11\xbb\r\n\x1a\n";
    let starts = |m: &[u8]| head.starts_with(m);
    if starts(KTX2) {
        Some(Format::Ktx2)
    } else if starts(KTX1) {
        Some(Format::Ktx1)
    } else if starts(b"DDS ") {
        Some(Format::Dds)
    } else if starts(&[0x13, 0xab, 0xa1, 0x5c]) {
        Some(Format::Astc)
    } else if starts(b"\x89PNG\r\n\x1a\n") {
        Some(Format::Png)
    } else if starts(&[0xff, 0xd8, 0xff]) {
        Some(Format::Jpeg)
    } else if starts(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        Some(Format::Webp)
    } else if hdr_magic(head) {
        Some(Format::Hdr)
    } else if fits::sniff(head) {
        Some(Format::Fits)
    } else if xisf::sniff(head) {
        Some(Format::Xisf)
    } else if jxr::sniff(head) {
        Some(Format::Jxr)
    } else if film::sniff_dpx(head) {
        Some(Format::Dpx)
    } else if film::sniff_cineon(head) {
        Some(Format::Cineon)
    } else if pam::sniff_pam(head) {
        Some(Format::Pam)
    } else if pam::sniff_farbfeld(head) {
        Some(Format::Farbfeld)
    } else if images::bmp_plausible(head) {
        Some(Format::Bmp)
    } else if images::pcx_plausible(head) {
        Some(Format::Pcx)
    } else if images::tga_plausible(head) {
        Some(Format::Tga)
    } else {
        None
    }
}

/// Decodes a whole file already in memory.
pub fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Texture, TextureError> {
    let format = sniff(bytes).ok_or_else(|| TextureError::Unsupported("unrecognised image format".into()))?;
    decode_as(format, bytes, opts)
}

/// Decodes a file in memory whose format the caller already knows, such as
/// a TGA (which has no magic number) named by its extension.
pub fn decode_as(format: Format, bytes: &[u8], opts: &DecodeOptions) -> Result<Texture, TextureError> {
    decode_as_for(format, bytes, opts, GpuBlocks::default())
}

/// Decodes a whole file in memory for a GPU that samples the block
/// formats in `gpu`: Basis Universal textures (ETC1S and UASTC) transcode
/// to BC7 (BC5 for two-channel normal maps) or ASTC blocks instead of RGBA,
/// which takes less time and a quarter of the memory, and uploads as it
/// is.  That needs the whole texture with every mip level stored and a
/// size in whole 4x4 blocks; anything else decodes as [`decode`] does.
/// Use it only where the texels go straight to the GPU: thumbnails and
/// anything that reads texels on the CPU need [`decompress`] first.
pub fn decode_for_gpu(bytes: &[u8], opts: &DecodeOptions, gpu: GpuBlocks) -> Result<Texture, TextureError> {
    let format = sniff(bytes).ok_or_else(|| TextureError::Unsupported("unrecognised image format".into()))?;
    decode_as_for(format, bytes, opts, gpu)
}

fn decode_as_for(format: Format, bytes: &[u8], opts: &DecodeOptions, gpu: GpuBlocks) -> Result<Texture, TextureError> {
    guard(|| {
        let tex = match format {
            Format::Png | Format::Jpeg | Format::Tga | Format::Bmp | Format::Webp | Format::Pcx => {
                images::decode(format, bytes, opts)?
            }
            Format::Ktx2 => ktx2::decode(bytes, opts, gpu)?,
            Format::Ktx1 => ktx1::decode(bytes, opts)?,
            Format::Dds => dds::decode(bytes, opts)?,
            Format::Astc => astc::decode(bytes, opts)?,
            Format::Hdr => hdr::decode(bytes, opts)?,
            Format::Fits => fits::decode(&mut std::io::Cursor::new(bytes), bytes.len() as u64, opts, None)?,
            Format::Xisf => xisf::decode(&mut std::io::Cursor::new(bytes), bytes.len() as u64, opts, None)?,
            Format::Pam => pam::decode_pam(bytes)?,
            Format::Farbfeld => pam::decode_farbfeld(bytes)?,
            Format::Dpx => film::decode_dpx(&mut std::io::Cursor::new(bytes), bytes.len() as u64, opts)?,
            Format::Cineon => film::decode_cineon(&mut std::io::Cursor::new(bytes), bytes.len() as u64, opts)?,
            Format::Jxr => jxr::decode(bytes, opts)?,
        };
        Ok(limit_size(tex, opts))
    })
}

/// Decodes a file from disk.  Large Radiance HDR files, astronomy frames
/// and film scans are streamed and downscaled while reading instead of
/// being held whole.  Files without a recognisable signature are tried as
/// TGA when their name ends in `.tga`.
pub fn decode_file(path: &Path, opts: &DecodeOptions) -> Result<Texture, TextureError> {
    decode_file_part(path, None, opts)
}

/// The names of the pictures in a file that holds several (a FITS file with
/// image extensions), in file order; empty for files that hold one.  A name
/// from this list picks a picture in [`decode_file_part`].
pub fn parts_file(path: &Path) -> Vec<String> {
    let Ok(mut file) = std::fs::File::open(path) else { return Vec::new() };
    let Ok(len) = file.metadata().map(|m| m.len()) else { return Vec::new() };
    let mut head = [0u8; 32];
    let n = util::read_fully(&mut file, &mut head).unwrap_or(0);
    match sniff(&head[..n]) {
        Some(Format::Fits) => {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fits::parts(&mut file, len))).unwrap_or_default()
        }
        Some(Format::Xisf) => {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| xisf::parts(&mut file, len))).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

/// Decodes one of the pictures [`parts_file`] lists, or the first picture
/// when `part` is `None`.
pub fn decode_file_part(path: &Path, part: Option<&str>, opts: &DecodeOptions) -> Result<Texture, TextureError> {
    use std::io::{Read, Seek};
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut head = [0u8; 32];
    let n = util::read_fully(&mut file, &mut head)?;
    let format = match sniff(&head[..n]) {
        Some(f) => f,
        None if has_extension(path, "tga") => Format::Tga,
        None => return Err(TextureError::Unsupported("unrecognised image format".into())),
    };
    file.seek(std::io::SeekFrom::Start(0))?;
    if format == Format::Hdr {
        return guard(|| Ok(limit_size(hdr::decode_reader(&mut file, len, opts)?, opts)));
    }
    if format == Format::Fits {
        return guard(|| Ok(limit_size(fits::decode(&mut file, len, opts, part)?, opts)));
    }
    if format == Format::Xisf {
        return guard(|| Ok(limit_size(xisf::decode(&mut file, len, opts, part)?, opts)));
    }
    if format == Format::Dpx {
        return guard(|| Ok(limit_size(film::decode_dpx(&mut file, len, opts)?, opts)));
    }
    if format == Format::Cineon {
        return guard(|| Ok(limit_size(film::decode_cineon(&mut file, len, opts)?, opts)));
    }
    util::check_bytes(len)?;
    let mut bytes = Vec::with_capacity(len as usize);
    file.read_to_end(&mut bytes)?;
    decode_as(format, &bytes, opts)
}

/// Makes a thumbnail no larger than `size` on its longer side: alpha
/// ignored, cubemaps laid out as a cross, the first layer and slice of
/// arrays and 3D textures, and HDR mapped with automatic exposure and
/// Khronos PBR Neutral tone mapping.
pub fn thumbnail(tex: &Texture, size: u32) -> Image8 {
    if is_compressed(tex) {
        // The level nearest the size is all a thumbnail reads.
        let level = tex.levels.iter().rposition(|l| l.width.max(l.height) >= size).unwrap_or(0);
        let mut small = tex.clone();
        small.levels = vec![decompress_level(&tex.levels[level], tex.color_space)];
        return thumb::thumbnail(&small, size);
    }
    thumb::thumbnail(tex, size)
}

/// Whether any of the texture's surfaces are GPU blocks
/// ([`decode_for_gpu`]).
pub fn is_compressed(tex: &Texture) -> bool {
    tex.levels.iter().any(|l| l.surfaces.iter().any(|s| matches!(s, Pixels::Blocks(..))))
}

/// The texture with any GPU blocks decoded to 8-bit RGBA, for code that
/// reads texels: two-channel BC5 gives red, green, 0, 255, as two-channel
/// textures decode everywhere else.  Blocks that don't decode come out
/// black.
pub fn decompress(tex: &Texture) -> Texture {
    if !is_compressed(tex) {
        return tex.clone();
    }
    let mut out = tex.clone();
    out.levels = tex.levels.iter().map(|l| decompress_level(l, tex.color_space)).collect();
    out
}

fn decompress_level(level: &Level, color_space: ColorSpace) -> Level {
    use rayon::prelude::*;
    let surfaces = level
        .surfaces
        .par_iter()
        .map(|s| match s {
            Pixels::Blocks(format, data) => {
                blocks::decode_gpu_blocks(*format, level.width, level.height, data, color_space == ColorSpace::Srgb)
                    .unwrap_or_else(|_| Pixels::Rgba8(vec![0; level.width as usize * level.height as usize * 4]))
            }
            other => other.clone(),
        })
        .collect();
    Level { surfaces, ..level.clone() }
}

//endregion

/*┌─────────────────╮
| Shared steps    |
└─────────────────╯*/
//region Shared steps

/// Turns a panic inside a decoder into an error.  Decoders are written not
/// to panic, but a thumbnailer runs on every file a user browses past, so a
/// missed case must cost one thumbnail, not the process.
fn guard(f: impl FnOnce() -> Result<Texture, TextureError>) -> Result<Texture, TextureError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(TextureError::Invalid("internal decoder error".into())))
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// Radiance files start with `#?` and the writing program's name, usually
/// RADIANCE or RGBE.
fn hdr_magic(head: &[u8]) -> bool {
    let Some(rest) = head.strip_prefix(b"#?") else { return false };
    let name: Vec<u8> = rest.iter().copied().take_while(|c| c.is_ascii_alphanumeric() || *c == b'_').collect();
    !name.is_empty() && matches!(rest.get(name.len()), Some(b'\n' | b'\r' | b' ') | None)
}

/// Downscales every decoded level so its longer side is at most
/// `opts.max_size`, dropping levels that would then repeat a size.
fn limit_size(mut tex: Texture, opts: &DecodeOptions) -> Texture {
    let Some(max) = opts.max_size else { return tex };
    let first = &tex.levels[0];
    if first.width.max(first.height) <= max {
        return tex;
    }
    let srgb = tex.color_space != ColorSpace::Linear;
    let (dw, dh) = resize::fit(first.width, first.height, max);
    let surfaces = first
        .surfaces
        .iter()
        .map(|s| resize::resize(s, first.width, first.height, first.depth, dw, dh, srgb))
        .collect();
    let base = Level { width: dw, height: dh, depth: first.depth, surfaces };
    let rest: Vec<Level> = tex.levels.drain(1..).filter(|l| l.width.max(l.height) < dw.max(dh)).collect();
    tex.levels = std::iter::once(base).chain(rest).collect();
    tex
}

//endregion
