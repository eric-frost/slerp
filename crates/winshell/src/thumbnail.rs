//! The thumbnail handler Explorer creates for each file: `IThumbnailProvider`
//! plus the three ways the shell can hand it the file.
//!
//! What Explorer does, as tested on Windows 11 24H2:
//!
//! - By default it loads thumbnail handlers into an isolated surrogate
//!   (`dllhost.exe /Processid:{AB8902B4-...}`) and initialises them only
//!   with `IInitializeWithStream`.  The stream's `Stat` gives the bare file
//!   name, never its folder, so glTF's `.bin`, MaterialX's textures, a
//!   BSP's `.lit` and an MD2's skin are out of reach.
//! - With `DisableProcessIsolation` set on the class, the handler runs in
//!   Explorer itself.  The shell still offers the stream first, but when
//!   `IInitializeWithStream::Initialize` fails it falls back to
//!   `IInitializeWithItem`, which carries the real path.
//!
//! So Slerp registers its handler in-process and refuses streams there,
//! and accepts them only in the isolated surrogate, where nothing else is
//! offered (`.png` stays isolated, see `registry.rs`).  In Explorer the
//! handler draws nothing itself: it sends the path to the thumbnail helper
//! process (`pipe.rs`), so a crashing decoder or GPU driver can't take
//! Explorer down.  In the isolated surrogate, which exists to absorb
//! crashes, it renders in-process.

use crate::log::log;
use crate::protocol::Reply;
use crate::source::{Input, Source, in_isolated_host, is_data_png, read_head};
use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use windows::Win32::Foundation::{E_FAIL, E_NOTIMPL, E_POINTER, E_UNEXPECTED, ERROR_ALREADY_INITIALIZED};
use windows::Win32::Graphics::Gdi::HBITMAP;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, IStream, STGM_READ, STREAM_SEEK_SET,
};
use windows::Win32::UI::Shell::PropertiesSystem::{
    IInitializeWithFile, IInitializeWithFile_Impl, IInitializeWithStream, IInitializeWithStream_Impl,
};
use windows::Win32::UI::Shell::{
    BHID_Stream, IInitializeWithItem, IInitializeWithItem_Impl, IShellItem, IThumbnailProvider,
    IThumbnailProvider_Impl, SHCreateItemFromParsingName, SIGDN_FILESYSPATH, WTS_ALPHATYPE, WTSAT_ARGB, WTSAT_RGB,
    WTSAT_UNKNOWN,
};
use windows::core::{GUID, HRESULT, HSTRING, Interface, PCWSTR, Ref, Result, implement};

/// One thumbnail request.  The shell creates a handler per file,
/// initialises it once and asks for one or more sizes.
#[implement(IThumbnailProvider, IInitializeWithStream, IInitializeWithItem, IInitializeWithFile)]
pub struct Thumbnailer {
    /// COM calls a handler registered with the apartment threading model
    /// on one thread only, so a `RefCell` is enough.
    source: RefCell<Option<Source>>,
}

impl Thumbnailer {
    pub fn new() -> Self {
        Thumbnailer { source: RefCell::new(None) }
    }

    fn set_source(&self, source: Source, via: &str) -> Result<()> {
        let mut slot = self.source.borrow_mut();
        if slot.is_some() {
            return Err(HRESULT::from_win32(ERROR_ALREADY_INITIALIZED.0).into());
        }
        match &source {
            Source::File(p) => log!("{via}: {}", p.display()),
            Source::Stream(_) => log!("{via}: a stream"),
        }
        *slot = Some(source);
        Ok(())
    }
}

/*┌─────────────────╮
| Initialisation  |
└─────────────────╯*/
//region Initialisation

impl IInitializeWithStream_Impl for Thumbnailer_Impl {
    fn Initialize(&self, stream: Ref<IStream>, _mode: u32) -> Result<()> {
        // Outside the isolated process the shell falls back to an item,
        // which says where the file is.
        if !in_isolated_host() {
            return Err(E_NOTIMPL.into());
        }
        let stream = stream.ok()?.clone();
        self.set_source(Source::Stream(stream), "IInitializeWithStream")
    }
}

impl IInitializeWithFile_Impl for Thumbnailer_Impl {
    fn Initialize(&self, path: &PCWSTR, _mode: u32) -> Result<()> {
        if path.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: the shell passes a valid NUL-terminated path.
        let path = PathBuf::from(unsafe { path.to_string() }?);
        self.set_source(Source::File(path), "IInitializeWithFile")
    }
}

impl IInitializeWithItem_Impl for Thumbnailer_Impl {
    fn Initialize(&self, item: Ref<IShellItem>, _mode: u32) -> Result<()> {
        let item = item.ok()?;
        // SAFETY: valid item; the returned string is freed right after.
        let source = match unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) } {
            Ok(name) => {
                // SAFETY: GetDisplayName returned a CoTaskMemAlloc'd string.
                let path = unsafe { name.to_string() };
                unsafe { CoTaskMemFree(Some(name.0.cast())) };
                Source::File(PathBuf::from(path?))
            }
            // Not a file on disk (inside a zip folder, on a phone): read it.
            // SAFETY: valid item; BHID_Stream yields an IStream.
            Err(_) => Source::Stream(unsafe { item.BindToHandler(None, &BHID_Stream)? }),
        };
        self.set_source(source, "IInitializeWithItem")
    }
}

//endregion

/*┌─────────────────╮
| Thumbnails      |
└─────────────────╯*/
//region Thumbnails

/// True until the first thumbnail in this process, for the log.
static FIRST: AtomicBool = AtomicBool::new(true);

impl IThumbnailProvider_Impl for Thumbnailer_Impl {
    fn GetThumbnail(&self, cx: u32, phbmp: *mut HBITMAP, pdwalpha: *mut WTS_ALPHATYPE) -> Result<()> {
        if phbmp.is_null() || pdwalpha.is_null() {
            return Err(E_POINTER.into());
        }
        let source = self.source.borrow();
        let Some(source) = source.as_ref() else { return Err(E_UNEXPECTED.into()) };
        let start = Instant::now();
        let first = FIRST.swap(false, Ordering::Relaxed);
        let size = cx.clamp(16, crate::render::MAX_SIZE);
        // Nothing may unwind into the host: a panic becomes E_FAIL.
        let made = catch_unwind(AssertUnwindSafe(|| make(source, size)))
            .unwrap_or_else(|panic| Err(windows::core::Error::new(E_FAIL, crate::render::panic_message(&panic))));
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        match made {
            Ok((bitmap, alpha, how)) => {
                if first && crate::log::enabled() {
                    let age = crate::log::process_age_ms();
                    log!("first thumbnail here, {cx} px, {how}: {ms:.1} ms; process started {age:.0} ms ago");
                } else {
                    log!("thumbnail {cx} px, {how}: {ms:.1} ms");
                }
                // SAFETY: both pointers were checked above.
                unsafe {
                    *phbmp = bitmap;
                    *pdwalpha = alpha;
                }
                Ok(())
            }
            Err(e) => {
                log!("no thumbnail ({cx} px, {ms:.1} ms): {}", e.message());
                Err(e)
            }
        }
    }
}

/// The largest thumbnails returned with transparency.  Explorer asks for
/// 768 pixels for its preview pane, and the icons it later scales down from
/// such a large thumbnail lose their alpha channel, which turns transparent
/// backgrounds black.  Larger thumbnails are therefore drawn over the
/// window colour instead.
const TRANSPARENT_UP_TO: u32 = 256;

/// Explorer's window background: white in light mode, its dark grey in
/// dark mode (`AppsUseLightTheme`).
fn window_colour() -> [u8; 3] {
    let light = windows_registry::CURRENT_USER
        .open(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_u32("AppsUseLightTheme"))
        .unwrap_or(1);
    if light == 0 { [25, 25, 25] } else { [255, 255, 255] }
}

/// Makes the bitmap, and says how for the log.
fn make(source: &Source, size: u32) -> Result<(HBITMAP, WTS_ALPHATYPE, &'static str)> {
    // 1. A path the loaders can read.
    let input = Input::resolve(source)?;
    // 2. `.png` is claimed for 16-bit greyscale heightmaps only; every
    //    other PNG goes to Windows' own thumbnailer, unchanged.
    let head = read_head(input.path());
    if head.starts_with(b"\x89PNG") && !is_data_png(&head) {
        let (bitmap, alpha) = delegate(source, size)?;
        return Ok((bitmap, alpha, "an ordinary PNG, drawn by Windows"));
    }
    // 3. Load and draw: here in the isolated process, else in the helper.
    let (reply, how) = if in_isolated_host() {
        (crate::render::thumbnail(input.path(), size), "drawn in the isolated process")
    } else {
        let reply = crate::pipe::request(input.path(), size).map_err(|e| windows::core::Error::new(E_FAIL, e))?;
        (reply, "drawn by the helper")
    };
    match reply {
        Reply::Image { width, height, rgba } if size > TRANSPARENT_UP_TO => {
            let bitmap = crate::bitmap::dib_section(width, height, &rgba, Some(window_colour()))?;
            Ok((bitmap, WTSAT_RGB, how))
        }
        Reply::Image { width, height, rgba } => {
            let bitmap = crate::bitmap::dib_section(width, height, &rgba, None)?;
            Ok((bitmap, WTSAT_ARGB, how))
        }
        Reply::Unknown => Err(windows::core::Error::new(E_FAIL, "not a format Slerp reads")),
        Reply::Failed(message) => Err(windows::core::Error::new(E_FAIL, message)),
    }
}

//endregion

/*┌─────────────────╮
| Ordinary PNGs   |
└─────────────────╯*/
//region Ordinary PNGs

/// Windows' photo thumbnailer, which handles PNG when nothing overrides it.
const PHOTO_THUMBNAIL_PROVIDER: GUID = GUID::from_u128(0xc7657c4a_9f68_40fa_a4df_96bc08eb3551);

/// Asks the thumbnailer Windows itself registers for `.png` to make this
/// thumbnail, initialised the same way Slerp was.
fn delegate(source: &Source, size: u32) -> Result<(HBITMAP, WTS_ALPHATYPE)> {
    let clsid = system_handler("png").unwrap_or(PHOTO_THUMBNAIL_PROVIDER);
    // SAFETY: plain COM creation in this apartment.
    let provider: IThumbnailProvider = unsafe { CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER)? };
    match source {
        Source::Stream(stream) => {
            // SAFETY: rewinding the stream Slerp read, then handing it on.
            unsafe {
                stream.Seek(0, STREAM_SEEK_SET, None)?;
                provider.cast::<IInitializeWithStream>()?.Initialize(stream, STGM_READ.0)?;
            }
        }
        Source::File(path) => {
            let wide = HSTRING::from(path.as_os_str());
            if let Ok(init) = provider.cast::<IInitializeWithFile>() {
                // SAFETY: valid interface and path.
                unsafe { init.Initialize(&wide, STGM_READ.0)? };
            } else {
                // SAFETY: valid path; the item is released when dropped.
                unsafe {
                    let item: IShellItem = SHCreateItemFromParsingName(&wide, None)?;
                    provider.cast::<IInitializeWithItem>()?.Initialize(&item, STGM_READ.0)?;
                }
            }
        }
    }
    let mut bitmap = HBITMAP::default();
    let mut alpha = WTSAT_UNKNOWN;
    // SAFETY: both out pointers are valid.
    unsafe { provider.GetThumbnail(size, &mut bitmap, &mut alpha)? };
    Ok((bitmap, alpha))
}

/// The thumbnail handler the machine-wide registry names for `.ext`,
/// skipping Slerp's own.  Looks where Explorer does: the extension,
/// its system file association, then the perceived type's (`image` for
/// PNG on Windows 11).
fn system_handler(ext: &str) -> Option<GUID> {
    let key = crate::registry::THUMBNAIL_HANDLER_KEY;
    let candidates = [
        format!(r"Software\Classes\.{ext}\ShellEx\{key}"),
        format!(r"Software\Classes\SystemFileAssociations\.{ext}\ShellEx\{key}"),
        format!(r"Software\Classes\SystemFileAssociations\image\ShellEx\{key}"),
    ];
    let ours = [crate::THUMBNAIL_CLSID, crate::ISOLATED_CLSID];
    candidates.iter().find_map(|path| {
        let value = windows_registry::LOCAL_MACHINE.open(path).ok()?.get_string("").ok()?;
        GUID::try_from(value.trim().trim_matches(['{', '}'])).ok().filter(|g| !ours.contains(g))
    })
}

//endregion
