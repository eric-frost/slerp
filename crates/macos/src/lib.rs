//! The C interface of Slerp's macOS Quick Look thumbnail extension.
//!
//! Finder gets thumbnails from Quick Look, which asks app extensions: small
//! sandboxed processes inside an app's bundle that answer
//! `QLThumbnailProvider` requests.  Those classes are Objective-C, so the
//! extension itself is a page of Swift (`swift/ThumbnailProvider.swift`)
//! that links this library statically and calls it for the pixels.  The C
//! declarations are in `include/slerp.h`.
//!
//! Thumbnails are made exactly as `slerp thumbnail` makes them on
//! Linux, without the resident helper: Quick Look keeps the extension's
//! process alive between requests, so the process itself keeps one GPU
//! renderer, started by the first file that needs it.  Requests can arrive
//! on several threads at once; files load in parallel and only drawing
//! waits for the GPU.
//!
//! The sandbox grants the extension the requested file and nothing beside
//! it, so formats that read neighbouring files (a `.gltf` with separate
//! buffers, an OBJ's `.mtl`, a material's textures, a BSP's `.lit`) show
//! what the file alone holds, as under GNOME's thumbnail sandbox on Linux.
//!
//! Unwinding into Swift would be undefined behaviour, so every entry point
//! catches panics and reports them as a status.

use slerp_core::scene::{Image8, Scene};
use slerp_core::textures::Format as TextureFormat;
use slerp_core::{Config, Kind, Prepared, ThumbError, ThumbRenderer};
use std::ffi::{CStr, c_char};
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

/*┌─────────────────╮
| C interface     |
└─────────────────╯*/
//region C interface

/// A thumbnail handed to the caller: `width * height` pixels of 8-bit RGBA
/// with straight alpha, sRGB, top row first.  Free it with
/// [`slerp_free`].
#[repr(C)]
#[derive(Debug)]
pub struct SlerpImage {
    pub rgba: *mut u8,
    pub width: u32,
    pub height: u32,
}

impl SlerpImage {
    const EMPTY: SlerpImage = SlerpImage { rgba: std::ptr::null_mut(), width: 0, height: 0 };
}

/// The thumbnail is in `out`.
pub const SLERP_OK: i32 = 0;
/// Not a file Slerp makes thumbnails of: an ordinary 8-bit PNG, or a
/// file sharing its extension with an unrelated format.  The caller should
/// let the system draw it.
pub const SLERP_NOT_MINE: i32 = 1;
/// The file couldn't be read, decoded or drawn; the message says why.
pub const SLERP_FAILED: i32 = 2;
/// A bug: Slerp panicked.  The process stays usable, and the GPU
/// renderer starts afresh for the next file.
pub const SLERP_PANICKED: i32 = 3;
/// A null pointer, or an empty path.
pub const SLERP_BAD_ARGUMENT: i32 = 4;

/// Makes the thumbnail of the file at `path`, no larger than `max_size`
/// pixels on its longer side, and returns one of the `SLERP_*`
/// statuses.  On success `out` holds the picture, which the caller frees
/// with [`slerp_free`]; otherwise `out` is empty, and when `message`
/// isn't null an explanation is written there, NUL-terminated and cut to
/// `message_capacity` bytes.
///
/// # Safety
///
/// `path` must be null or a NUL-terminated string, `out` null or valid for
/// writing one `SlerpImage`, and `message` null or valid for writing
/// `message_capacity` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slerp_thumbnail(
    path: *const c_char,
    max_size: u32,
    out: *mut SlerpImage,
    message: *mut c_char,
    message_capacity: usize,
) -> i32 {
    let fail = |status: i32, text: &str| {
        // SAFETY: the caller guarantees `message` is null or writable for
        // `message_capacity` bytes.
        unsafe { write_message(message, message_capacity, text) };
        status
    };
    if out.is_null() {
        return fail(SLERP_BAD_ARGUMENT, "no place for the thumbnail");
    }
    // SAFETY: `out` is non-null and the caller guarantees it is writable.
    unsafe { out.write(SlerpImage::EMPTY) };
    if path.is_null() {
        return fail(SLERP_BAD_ARGUMENT, "no path");
    }
    // SAFETY: the caller guarantees a NUL-terminated string.
    let Some(path) = to_path(unsafe { CStr::from_ptr(path) }) else {
        return fail(SLERP_BAD_ARGUMENT, "empty path");
    };
    match guarded(|| thumbnail(&path, max_size)) {
        Ok(image) => {
            let (width, height) = (image.width, image.height);
            let rgba = Box::into_raw(image.pixels.into_boxed_slice()).cast::<u8>();
            // SAFETY: as above.
            unsafe { out.write(SlerpImage { rgba, width, height }) };
            SLERP_OK
        }
        Err(e) => fail(e.status(), &e.to_string()),
    }
}

/// Frees a thumbnail from [`slerp_thumbnail`] and empties `image`.
/// Freeing an empty image, or a null pointer, does nothing.
///
/// # Safety
///
/// `image` must be null, or point to a `SlerpImage` that is empty or
/// was filled by `slerp_thumbnail` and not changed since.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn slerp_free(image: *mut SlerpImage) {
    if image.is_null() {
        return;
    }
    // SAFETY: the caller guarantees `image` is valid.
    let image = unsafe { &mut *image };
    if !image.rgba.is_null() {
        let len = image.width as usize * image.height as usize * 4;
        // SAFETY: `rgba` came from `Box::into_raw` on a boxed slice of
        // exactly this length (`thumbnail` checks the length).
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(image.rgba, len)) });
    }
    *image = SlerpImage::EMPTY;
}

/// Copies as much of `text` as fits, whole characters only, and a NUL.
///
/// # Safety
///
/// `dst` must be null or valid for writing `capacity` bytes.
unsafe fn write_message(dst: *mut c_char, capacity: usize, text: &str) {
    if dst.is_null() || capacity == 0 {
        return;
    }
    let mut n = text.len().min(capacity - 1);
    while !text.is_char_boundary(n) {
        n -= 1;
    }
    // SAFETY: `n + 1 <= capacity` bytes are written.
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), dst.cast::<u8>(), n);
        *dst.add(n) = 0;
    }
}

#[cfg(unix)]
fn to_path(s: &CStr) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = s.to_bytes();
    (!bytes.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

#[cfg(not(unix))]
fn to_path(s: &CStr) -> Option<PathBuf> {
    s.to_str().ok().filter(|s| !s.is_empty()).map(PathBuf::from)
}

//endregion

/*┌─────────────────╮
| Thumbnails      |
└─────────────────╯*/
//region Thumbnails

/// Why there is no thumbnail.
#[derive(Debug)]
enum Failure {
    NotMine(String),
    Failed(String),
    Panicked(String),
}

impl Failure {
    fn status(&self) -> i32 {
        match self {
            Failure::NotMine(_) => SLERP_NOT_MINE,
            Failure::Failed(_) => SLERP_FAILED,
            Failure::Panicked(_) => SLERP_PANICKED,
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::NotMine(s) | Failure::Failed(s) => f.write_str(s),
            Failure::Panicked(s) => write!(f, "Slerp panicked: {s}"),
        }
    }
}

/// Runs `f`, turning a panic into [`Failure::Panicked`].
fn guarded<T>(f: impl FnOnce() -> Result<T, Failure>) -> Result<T, Failure> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => {
            let text = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            Err(Failure::Panicked(text))
        }
    }
}

/// The work of [`slerp_thumbnail`], as `slerp thumbnail` does it.
fn thumbnail(path: &Path, size: u32) -> Result<Image8, Failure> {
    let size = size.clamp(16, 2048);
    // 1. Recognise the file.  Of PNGs, Slerp draws only the 16-bit
    //    greyscale ones (heightmaps), as on Linux.  Quick Look doesn't send
    //    PNGs to the extension today (see `data/macos/Info.plist`), but a
    //    misnamed file can be one.
    let mut head = Vec::with_capacity(slerp_core::SNIFF_BYTES);
    std::fs::File::open(path)
        .and_then(|f| f.take(slerp_core::SNIFF_BYTES as u64).read_to_end(&mut head))
        .map_err(|e| Failure::Failed(e.to_string()))?;
    let kind = slerp_core::sniff(path, &head).ok_or_else(|| Failure::NotMine("not a format Slerp reads".into()))?;
    if kind == Kind::Texture(TextureFormat::Png) && !is_data_png(&head) {
        return Err(Failure::NotMine("an ordinary PNG".into()));
    }
    // Quick Look sends every type the extension lists; the user may have
    // switched some off in Slerp's File Types.
    if !slerp_core::choices::thumbnails_wanted(kind, path, choices_file().as_deref()) {
        return Err(Failure::NotMine("thumbnails of this format are switched off in Slerp".into()));
    }

    // 2. Load, and draw on the GPU when the file needs it.
    let image = match slerp_core::prepare_thumbnail(path, size) {
        Ok(Prepared::Done(image)) => image,
        Ok(Prepared::Render(scene)) => render(&scene, size)?,
        Err(ThumbError::Unknown) => return Err(Failure::NotMine("not a format Slerp reads".into())),
        Err(e) => return Err(Failure::Failed(e.to_string())),
    };

    // 3. `slerp_free` relies on the exact length.
    if image.width == 0 || image.height == 0 || image.pixels.len() != image.width as usize * image.height as usize * 4 {
        return Err(Failure::Failed(format!("bad thumbnail size {image:?}")));
    }
    Ok(image)
}

/// The user's choices of formats, `file-types.jsonc` in Slerp's settings
/// folder.  Inside the extension's sandbox `HOME` is the extension's
/// container (`~/Library/Containers/<id>/Data`), so the real home is the
/// part before it; the extension's entitlements let it read Slerp's
/// settings folder there and nothing else.
fn choices_file() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let text = home.to_string_lossy().into_owned();
    let real = match text.find("/Library/Containers/") {
        Some(i) => PathBuf::from(&text[..i]),
        None => home,
    };
    if cfg!(target_os = "macos") {
        Some(real.join("Library/Application Support/Slerp/file-types.jsonc"))
    } else {
        slerp_core::choices::Choices::path()
    }
}

/// True for a PNG of 16-bit grey, with or without alpha: nearly always a
/// heightmap or other data whose values fill a sliver of the range, which
/// Slerp stretches to be visible.  The same test as
/// `image/x-png-16bit-grey` in `data/slerp.xml`.
fn is_data_png(head: &[u8]) -> bool {
    head.get(12..16) == Some(b"IHDR") && head.get(24) == Some(&16) && matches!(head.get(25), Some(0 | 4))
}

/// The GPU renderer, kept for the life of the extension's process.  `None`
/// until a file needs it, and again after a panic while drawing, which may
/// have left it half-updated.
static RENDERER: Mutex<Option<ThumbRenderer>> = Mutex::new(None);

/// Locks `mutex`, emptying it if a panic poisoned it.
fn lock_or_reset<T>(mutex: &Mutex<Option<T>>) -> MutexGuard<'_, Option<T>> {
    mutex.lock().unwrap_or_else(|poisoned| {
        let mut guard = poisoned.into_inner();
        *guard = None;
        mutex.clear_poison();
        guard
    })
}

fn render(scene: &Scene, size: u32) -> Result<Image8, Failure> {
    static CONFIG: OnceLock<Config> = OnceLock::new();
    let mut guard = lock_or_reset(&RENDERER);
    // The settings file is out of the sandbox's reach, so this reads the
    // defaults (or the copy in the extension's container): the system's
    // default GPU.
    let renderer = guard.get_or_insert_with(|| ThumbRenderer::new(&CONFIG.get_or_init(Config::load).gpu));
    renderer.render_thumbnail(scene, size).map_err(|e| Failure::Failed(e.to_string()))
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    /// A folder of test files for this process.
    fn temp(name: &str, data: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("slerp-macos-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, data).unwrap();
        path
    }

    fn png(width: u32, height: u32, color: png::ColorType, depth: png::BitDepth, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(color);
        enc.set_depth(depth);
        enc.write_header().unwrap().write_image_data(data).unwrap();
        out
    }

    /// Calls the C interface as Swift does.
    fn call(path: &Path, size: u32) -> (i32, SlerpImage, String) {
        let c = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let mut out = SlerpImage { rgba: std::ptr::dangling_mut(), width: 7, height: 7 };
        let mut message = [0 as c_char; 256];
        let status = unsafe { slerp_thumbnail(c.as_ptr(), size, &mut out, message.as_mut_ptr(), message.len()) };
        let text = unsafe { CStr::from_ptr(message.as_ptr()) }.to_string_lossy().into_owned();
        (status, out, text)
    }

    #[test]
    fn heightmap_png_is_stretched() {
        // 16-bit grey from 1000 to 1255: a sliver of the range.
        let (w, h) = (64u32, 32u32);
        let mut data = Vec::new();
        for _y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&(1000 + (x * 255 / (w - 1)) as u16).to_be_bytes());
            }
        }
        let path = temp("height.png", &png(w, h, png::ColorType::Grayscale, png::BitDepth::Sixteen, &data));
        let (status, mut out, text) = call(&path, 128);
        assert_eq!(status, SLERP_OK, "{text}");
        assert!(out.width > 0 && out.width <= 128 && out.width == out.height * 2, "{out:?}");
        let px = unsafe { std::slice::from_raw_parts(out.rgba, (out.width * out.height * 4) as usize) };
        let last = ((out.width - 1) * 4) as usize;
        assert!(px[0] < 30 && px[last] > 225, "not stretched: {} .. {}", px[0], px[last]);
        unsafe { slerp_free(&mut out) };
        assert!(out.rgba.is_null() && out.width == 0);
        // Freeing twice is harmless.
        unsafe { slerp_free(&mut out) };
    }

    #[test]
    fn ordinary_files_are_left_to_the_system() {
        let rgb = temp("photo.png", &png(4, 4, png::ColorType::Rgb, png::BitDepth::Eight, &[128; 48]));
        let grey8 = temp("grey8.png", &png(4, 4, png::ColorType::Grayscale, png::BitDepth::Eight, &[128; 16]));
        let text = temp("notes.mdl", b"Model {\n  Name \"simulink\"\n}\n");
        for path in [rgb, grey8, text] {
            let (status, out, message) = call(&path, 256);
            assert_eq!(status, SLERP_NOT_MINE, "{path:?}: {message}");
            assert!(out.rgba.is_null() && out.width == 0 && out.height == 0);
            assert!(!message.is_empty());
        }
    }

    #[test]
    fn bad_files_fail_without_panicking() {
        let missing = std::env::temp_dir().join("slerp-macos-no-such-file.glb");
        let (status, _, message) = call(&missing, 256);
        assert_eq!(status, SLERP_FAILED, "{message}");
        // A KTX2 header cut short, and a GLB claiming more than it holds.
        let ktx2 = temp("cut.ktx2", b"\xabKTX 20\xbb\r\n\x1a\n\x25\0\0\0\x01\0\0\0");
        let glb = temp("cut.glb", b"glTF\x02\0\0\0\xff\xff\xff\x7f\x10\0\0\0JSON{\"asset\":");
        for path in [ktx2, glb] {
            let (status, out, message) = call(&path, 256);
            assert_eq!(status, SLERP_FAILED, "{path:?}: {message}");
            assert!(out.rgba.is_null());
        }
    }

    #[test]
    fn bad_arguments() {
        let mut out = SlerpImage::EMPTY;
        let mut message = [0 as c_char; 64];
        let status = unsafe { slerp_thumbnail(std::ptr::null(), 256, &mut out, message.as_mut_ptr(), 64) };
        assert_eq!(status, SLERP_BAD_ARGUMENT);
        let empty = CString::new("").unwrap();
        let status = unsafe { slerp_thumbnail(empty.as_ptr(), 256, &mut out, std::ptr::null_mut(), 0) };
        assert_eq!(status, SLERP_BAD_ARGUMENT);
        let path = CString::new("/nonexistent").unwrap();
        let status = unsafe { slerp_thumbnail(path.as_ptr(), 256, std::ptr::null_mut(), std::ptr::null_mut(), 0) };
        assert_eq!(status, SLERP_BAD_ARGUMENT);
        unsafe { slerp_free(std::ptr::null_mut()) };
    }

    #[test]
    fn messages_are_cut_at_characters() {
        let mut buf = [0x55 as c_char; 6];
        unsafe { write_message(buf.as_mut_ptr(), buf.len(), "ab\u{e9}\u{e9}cd") };
        // "ab" plus one two-byte character fits in five bytes before the NUL.
        assert_eq!(unsafe { CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap(), "ab\u{e9}");
        let mut one = [0x55 as c_char; 1];
        unsafe { write_message(one.as_mut_ptr(), 1, "abc") };
        assert_eq!(one[0], 0);
        unsafe { write_message(std::ptr::null_mut(), 10, "abc") };
    }

    /// Swift sees the statuses through the C header, so it must agree.
    #[test]
    fn header_matches() {
        let header = include_str!("../include/slerp.h");
        let defined = |name: &str| -> i32 {
            let line = header.lines().find(|l| l.starts_with(&format!("#define {name} "))).expect(name);
            line.rsplit(' ').next().unwrap().parse().unwrap()
        };
        assert_eq!(defined("SLERP_OK"), SLERP_OK);
        assert_eq!(defined("SLERP_NOT_MINE"), SLERP_NOT_MINE);
        assert_eq!(defined("SLERP_FAILED"), SLERP_FAILED);
        assert_eq!(defined("SLERP_PANICKED"), SLERP_PANICKED);
        assert_eq!(defined("SLERP_BAD_ARGUMENT"), SLERP_BAD_ARGUMENT);
        assert!(header.contains("int32_t slerp_thumbnail(const char *path, uint32_t max_size, SlerpImage *out"));
        assert!(header.contains("void slerp_free(SlerpImage *image);"));
    }

    #[test]
    fn panics_become_a_status() {
        let result: Result<(), Failure> = guarded(|| panic!("boom {}", 42));
        let e = result.unwrap_err();
        assert_eq!(e.status(), SLERP_PANICKED);
        assert!(e.to_string().contains("boom 42"), "{e}");
        assert!(guarded(|| -> Result<(), Failure> { panic!("static") }).unwrap_err().to_string().contains("static"));
    }

    #[test]
    fn a_panic_while_drawing_resets_the_renderer() {
        static SLOT: Mutex<Option<u32>> = Mutex::new(None);
        *lock_or_reset(&SLOT) = Some(1);
        let _ = std::thread::spawn(|| {
            let _guard = lock_or_reset(&SLOT);
            panic!("while drawing");
        })
        .join();
        assert!(lock_or_reset(&SLOT).is_none());
        assert!(!SLOT.is_poisoned());
    }

    /// A model goes to the GPU; the second request reuses the renderer.
    /// Machines without a GPU (or lavapipe) report that instead.
    #[test]
    fn models_render_on_the_gpu() {
        let obj = temp("tri.obj", b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n");
        for _ in 0..2 {
            let (status, mut out, message) = call(&obj, 64);
            if status == SLERP_FAILED && message.contains("GPU") {
                eprintln!("skipped: {message}");
                return;
            }
            assert_eq!(status, SLERP_OK, "{message}");
            assert_eq!((out.width, out.height), (64, 64));
            unsafe { slerp_free(&mut out) };
        }
    }
}
