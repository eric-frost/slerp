//! HEIC (and other HEIF pictures that are not AV1) through the system's
//! libheif.
//!
//! HEIC pictures are HEVC video frames, and Slerp bundles no HEVC decoder:
//! the codec is patent-encumbered, and the usual open decoder, libde265, is
//! LGPL.  Where the user has installed libheif (Linux distributions ship it
//! as `libheif1`; Homebrew as `libheif`) it is loaded at run time with
//! `dlopen`, so Slerp neither links nor ships it, and the picture is decoded
//! through it.  Without the library the viewer says what to install.
//!
//! libheif is a large C++ library that reads whatever a file says, so the
//! decode runs isolated with limits ([`DECODE_JOB`], see `slerp-isolate`)
//! and comes back as plain pixels.

use libloading::Library;
use slerp_isolate::{Job, Pack, Unpack};
use std::ffi::{CStr, c_char, c_int, c_void};

/// libheif's `heif_error`: a code, a sub-code and a message, returned by
/// value.
#[repr(C)]
#[derive(Clone, Copy)]
struct HeifError {
    code: c_int,
    subcode: c_int,
    message: *const c_char,
}

const CHANNEL_INTERLEAVED: c_int = 10;
const COLORSPACE_RGB: c_int = 1;
const CHROMA_RGBA: c_int = 11;
const CHROMA_RRGGBBAA_LE: c_int = 15;

/// What to tell the user when the library isn't there.
pub fn missing_message() -> String {
    if cfg!(target_os = "macos") {
        "HEIC needs the libheif library; install it with Homebrew (brew install libheif)".into()
    } else if cfg!(windows) {
        "HEIC needs the libheif library (heif.dll), which Slerp doesn't ship; put a copy next to slerp.exe".into()
    } else {
        "HEIC needs the libheif library; install libheif1 (sudo apt install libheif1)".into()
    }
}

const CANDIDATES: &[&str] = if cfg!(target_os = "macos") {
    &["libheif.1.dylib", "libheif.dylib", "/opt/homebrew/lib/libheif.dylib", "/usr/local/lib/libheif.dylib"]
} else if cfg!(windows) {
    &["heif.dll", "libheif.dll"]
} else {
    &["libheif.so.1", "libheif.so"]
};

/// Opens libheif, or says it isn't installed.
fn open_library() -> Result<Library, String> {
    for name in CANDIDATES {
        // SAFETY: libheif's initialisers are the ordinary ones of a shared
        // library, and this runs in a child process (or on Windows, in the
        // thumbnail helper), not in the middle of something fragile.
        if let Ok(lib) = unsafe { Library::new(name) } {
            return Ok(lib);
        }
    }
    Err(missing_message())
}

/// Whether libheif can be loaded here, for the tests.
#[cfg(test)]
pub fn available() -> bool {
    open_library().is_ok()
}

/// A picture libheif decoded.
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub has_alpha: bool,
    pub bit_depth: u8,
    /// RGBA, 8-bit or 16-bit little-endian samples as `wide` says.
    pub wide: bool,
    pub pixels: Vec<u8>,
}

/// Decodes the primary picture of a HEIF file.  `wide` asks for 16-bit
/// samples (for HDR curves); `max_pixels` refuses larger pictures before
/// libheif is asked.
pub fn decode(file: &[u8], wide: bool, max_pixels: u64) -> Result<Decoded, String> {
    // Output size is unknown until libheif reads the header, so the limits
    // allow for the biggest picture accepted.
    let limits = crate::limits_for_pixels((max_pixels as usize).saturating_mul(if wide { 8 } else { 4 }));
    let input = Pack::new().bool(wide).u64(max_pixels).bytes(file).done();
    let wire = slerp_isolate::run(&DECODE_JOB, &input, limits)?;
    parse_wire(&wire, max_pixels)
}

/// The isolated side of [`decode`].
pub(crate) static DECODE_JOB: Job = Job::once("heif.decode", "libheif", |input| {
    let mut u = Unpack::new(input);
    let (wide, max_pixels) = (u.bool()?, u.u64()?);
    decode_here(u.bytes()?, wide, max_pixels)
});

fn text_of(e: &HeifError) -> String {
    if e.message.is_null() {
        return format!("error {}.{}", e.code, e.subcode);
    }
    // SAFETY: libheif's messages are static NUL-terminated strings.
    unsafe { CStr::from_ptr(e.message) }.to_string_lossy().into_owned()
}

/// Says what a libheif error means for the user, where it is a known cause.
fn explain(e: &HeifError) -> String {
    let text = text_of(e);
    // Unsupported feature (3) with an unsupported codec subcode: libheif
    // is here but its HEVC plugin is not.
    if e.code == 3 || text.to_ascii_lowercase().contains("unsupported codec") || text.contains("No decoder") {
        return format!(
            "libheif can't decode this picture ({text}); its HEVC plugin may be missing (on Linux: libheif-plugin-libde265)"
        );
    }
    format!("libheif: {text}")
}

fn decode_here(file: &[u8], wide: bool, max_pixels: u64) -> Result<Vec<u8>, String> {
    let lib = open_library()?;
    // SAFETY (for every call below): the signatures are those of libheif's
    // public C API (heif.h), handles are used only between the call that
    // makes them and the one that releases them, and the file's bytes
    // outlive the context.
    unsafe {
        type Alloc = unsafe extern "C" fn() -> *mut c_void;
        type Free = unsafe extern "C" fn(*mut c_void);
        type Read = unsafe extern "C" fn(*mut c_void, *const c_void, usize, *const c_void) -> HeifError;
        type Primary = unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> HeifError;
        type Int = unsafe extern "C" fn(*const c_void) -> c_int;
        type Decode = unsafe extern "C" fn(*const c_void, *mut *mut c_void, c_int, c_int, *const c_void) -> HeifError;
        type Plane = unsafe extern "C" fn(*const c_void, c_int, *mut c_int) -> *const u8;
        type Init = unsafe extern "C" fn(*const c_void) -> HeifError;
        macro_rules! sym {
            ($name:literal, $ty:ty) => {
                *lib.get::<$ty>(concat!($name, "\0").as_bytes())
                    .map_err(|e| format!("libheif is missing {}: {e}", $name))?
            };
        }
        let f_alloc: Alloc = sym!("heif_context_alloc", Alloc);
        let f_free: Free = sym!("heif_context_free", Free);
        let f_read: Read = sym!("heif_context_read_from_memory_without_copy", Read);
        let f_primary: Primary = sym!("heif_context_get_primary_image_handle", Primary);
        let f_width: Int = sym!("heif_image_handle_get_width", Int);
        let f_height: Int = sym!("heif_image_handle_get_height", Int);
        let f_alpha: Int = sym!("heif_image_handle_has_alpha_channel", Int);
        let f_bits: Int = sym!("heif_image_handle_get_luma_bits_per_pixel", Int);
        let f_release_handle: Free = sym!("heif_image_handle_release", Free);
        let f_decode: Decode = sym!("heif_decode_image", Decode);
        let f_plane: Plane = sym!("heif_image_get_plane_readonly", Plane);
        let f_release_image: Free = sym!("heif_image_release", Free);
        // Newer libheif wants heif_init() before use; older has no such function.
        if let Ok(init) = lib.get::<Init>(b"heif_init\0") {
            init(std::ptr::null());
        }
        let ctx = f_alloc();
        if ctx.is_null() {
            return Err("libheif couldn't start".into());
        }
        let result = (|| -> Result<Vec<u8>, String> {
            let e = f_read(ctx, file.as_ptr().cast(), file.len(), std::ptr::null());
            if e.code != 0 {
                return Err(explain(&e));
            }
            let mut handle = std::ptr::null_mut();
            let e = f_primary(ctx, &mut handle);
            if e.code != 0 || handle.is_null() {
                return Err(explain(&e));
            }
            let (w, h) = (f_width(handle) as i64, f_height(handle) as i64);
            let (alpha, bits) = (f_alpha(handle) != 0, f_bits(handle));
            let mut image = std::ptr::null_mut();
            let outcome = (|| {
                if w <= 0 || h <= 0 || (w * h) as u64 > max_pixels {
                    return Err(format!("{w} × {h} pixels is more than is opened ({} megapixels)", max_pixels >> 20));
                }
                let chroma = if wide { CHROMA_RRGGBBAA_LE } else { CHROMA_RGBA };
                let e = f_decode(handle, &mut image, COLORSPACE_RGB, chroma, std::ptr::null());
                if e.code != 0 || image.is_null() {
                    return Err(explain(&e));
                }
                let mut stride: c_int = 0;
                let plane = f_plane(image, CHANNEL_INTERLEAVED, &mut stride);
                if plane.is_null() || stride <= 0 {
                    return Err("libheif returned no pixels".into());
                }
                let row = w as usize * 4 * if wide { 2 } else { 1 };
                let mut wire = Vec::with_capacity(16 + row * h as usize);
                wire.extend((w as u32).to_le_bytes());
                wire.extend((h as u32).to_le_bytes());
                wire.extend([alpha as u8, bits.clamp(0, 255) as u8, wide as u8, 0]);
                for y in 0..h as usize {
                    wire.extend_from_slice(std::slice::from_raw_parts(plane.add(y * stride as usize), row));
                }
                Ok(wire)
            })();
            if !image.is_null() {
                f_release_image(image);
            }
            f_release_handle(handle);
            outcome
        })();
        f_free(ctx);
        result
    }
}

fn parse_wire(wire: &[u8], max_pixels: u64) -> Result<Decoded, String> {
    if wire.len() < 12 {
        return Err("libheif's answer was cut short".into());
    }
    let width = u32::from_le_bytes([wire[0], wire[1], wire[2], wire[3]]);
    let height = u32::from_le_bytes([wire[4], wire[5], wire[6], wire[7]]);
    let (has_alpha, bit_depth, wide) = (wire[8] != 0, wire[9], wire[10] != 0);
    let body = &wire[12..];
    let expected = (width as u64).checked_mul(height as u64).and_then(|n| n.checked_mul(if wide { 8 } else { 4 }));
    if width == 0 || height == 0 || expected != Some(body.len() as u64) || (width as u64 * height as u64) > max_pixels {
        return Err("libheif's answer has the wrong size".into());
    }
    Ok(Decoded { width, height, has_alpha, bit_depth, wide, pixels: body.to_vec() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_format_is_checked() {
        let mut wire = Vec::new();
        wire.extend(2u32.to_le_bytes());
        wire.extend(1u32.to_le_bytes());
        wire.extend([1, 8, 0, 0]);
        wire.extend([1, 2, 3, 4, 5, 6, 7, 8]);
        let d = parse_wire(&wire, 100).unwrap();
        assert_eq!((d.width, d.height, d.has_alpha, d.wide), (2, 1, true, false));
        assert!(parse_wire(&wire[..wire.len() - 1], 100).is_err());
        assert!(parse_wire(&wire, 1).is_err());
        assert!(parse_wire(&wire[..5], 100).is_err());
        wire[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_wire(&wire, u64::MAX).is_err());
    }

    #[test]
    fn the_message_names_the_package() {
        let m = missing_message();
        assert!(m.contains("libheif"));
        if cfg!(target_os = "linux") {
            assert!(m.contains("libheif1"));
        }
    }

    #[test]
    fn garbage_is_an_error_not_a_crash() {
        if !available() {
            return;
        }
        for junk in
            [&b""[..], b"not a heif file at all", &[0u8; 100], b"\0\0\0\x18ftypheic\0\0\0\0mif1heic\0\0\0\x08meta"]
        {
            assert!(decode(junk, false, 1 << 20).is_err());
        }
    }
}
