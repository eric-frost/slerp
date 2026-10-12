//! Drag-out on Windows, with OLE's `DoDragDrop`.
//!
//! OLE drags need two COM objects: an `IDropSource`, which only decides when
//! the drag ends (Escape or a second button cancels it, releasing the button
//! drops), and an `IDataObject`, which holds what is dragged.  Ours offers
//! `CF_HDROP`, the format every program that takes dropped files understands,
//! and makes the files when a drop target first asks for it, so a drag that is
//! cancelled, or that passes over windows that don't ask, writes nothing.  The
//! target reads the files from the drag's private folder in `%TEMP%`.  Explorer
//! asks as soon as the pointer enters one of its windows (the desktop counts),
//! to see what it could do with the files, so there the files are written on
//! arrival rather than at the drop, and a cancel afterwards deletes them.
//! Offering them as virtual files (`CFSTR_FILEDESCRIPTORW` and
//! `CFSTR_FILECONTENTSW`) would let Explorer wait for the drop, but their
//! contents are read after the drag has ended, while `Payload` gives up its
//! producers when it does, so that is left out.
//!
//! `DoDragDrop` blocks in a message loop of its own, on the calling thread,
//! until the drag is over.  [`start`] is called from inside winit's event
//! handler (egui draws its frame there), so that loop runs with the handler
//! already active.  winit 0.30 allows for it: while its handler is busy it
//! buffers every event for later, and a `WM_PAINT` that arrives meanwhile is
//! marked handled and requested again afterwards.  The window therefore
//! doesn't repaint during the drag and catches up afterwards.  OLE's loop
//! also takes the pointer, so the window never sees the button go up; when
//! the drag is over [`start`] posts that release to the window, which keeps
//! winit's mouse bookkeeping right and wakes the event loop for a frame in
//! which the interface learns how the drag ended.
//!
//! winit also registers a drop target on the window, and its `DragEnter`
//! reads `CF_HDROP` at once.  Since every drag begins over the window it
//! came from, that would write all the files at the very start.  The data
//! object therefore refuses to hand out files while the pointer is over a
//! window of this process, which also keeps a drag from being dropped back
//! into the viewer, where winit would open the temporary file.

mod bytes;

use super::payload::{ActiveGuard, Drag, DragError, DragIcon, Outcome, Payload, Session};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::rc::Rc;
use windows::Win32::Foundation::{
    COLORREF, DATA_S_SAMEFORMATETC, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, DV_E_DVASPECT,
    DV_E_FORMATETC, DV_E_LINDEX, DV_E_TYMED, E_FAIL, E_NOTIMPL, E_OUTOFMEMORY, E_POINTER, E_UNEXPECTED, GlobalFree,
    HGLOBAL, HWND, LPARAM, OLE_E_ADVISENOTSUPPORTED, POINT, RPC_E_CHANGED_MODE, S_OK, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, HBITMAP, ScreenToClient,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, DATADIR_GET, DVASPECT_CONTENT, FORMATETC, IAdviseSink, IDataObject,
    IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, IStream, STGMEDIUM, STGMEDIUM_0, STREAM_SEEK_SET, TYMED_HGLOBAL,
    TYMED_ISTREAM,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize,
    ReleaseStgMedium,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MK_MBUTTON, MK_RBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VIRTUAL_KEY, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON};
use windows::Win32::UI::Shell::{CLSID_DragDropHelper, IDragSourceHelper, SHCreateStdEnumFmtEtc, SHDRAGIMAGE};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, GetWindowThreadProcessId, PostMessageW, SM_SWAPBUTTON, WM_LBUTTONUP, WM_MBUTTONUP,
    WM_RBUTTONUP, WindowFromPoint,
};
use windows::core::{BOOL, HRESULT, Ref, Result, implement, w};

/// The largest memory block `SetData` accepts, so a broken caller can't make
/// us copy without limit.  The drag picture, the biggest thing stored, is
/// under 100 KB.
const MAX_STORED: usize = 16 << 20;

//region Entry points

thread_local! {
    /// Whether this thread has initialised OLE.  The count that
    /// `OleInitialize` keeps is never given back: the thread keeps OLE until
    /// it ends, as winit's own call does.
    static OLE_READY: Cell<bool> = const { Cell::new(false) };
}

/// Makes this thread a single-threaded apartment with OLE, which
/// `DoDragDrop` requires.  winit does it too when it makes a window (it
/// registers a drop target), so this normally finds OLE ready: `OleInitialize`
/// then reports `S_FALSE`, which is success.
fn ensure_ole() -> std::result::Result<(), DragError> {
    OLE_READY.with(|ready| {
        if ready.get() {
            return Ok(());
        }
        match unsafe { OleInitialize(None) } {
            Ok(()) => {
                ready.set(true);
                Ok(())
            }
            Err(e) if e.code() == RPC_E_CHANGED_MODE => Err(DragError::Platform(
                "this thread already uses COM in the multithreaded mode, which OLE drag and drop can't use".into(),
            )),
            Err(e) => Err(DragError::Platform(format!("OleInitialize failed: {e}"))),
        }
    })
}

pub fn prepare(_: RawWindowHandle, _: RawDisplayHandle) -> std::result::Result<(), DragError> {
    ensure_ole()
}

/// Posts the release of `button` (an `MK_` flag) to `window`, at the
/// pointer's place in the window's client area.
fn post_button_up(window: HWND, button: u32) {
    let message = match button {
        b if b == MK_RBUTTON.0 => WM_RBUTTONUP,
        b if b == MK_MBUTTON.0 => WM_MBUTTONUP,
        _ => WM_LBUTTONUP,
    };
    let mut point = POINT::default();
    unsafe {
        if GetCursorPos(&mut point).is_ok() {
            let _ = ScreenToClient(window, &mut point);
        }
        // The client coordinates travel as two signed 16-bit halves.
        let position = ((point.y as i16 as u16 as u32) << 16 | point.x as i16 as u16 as u32) as i32 as isize;
        let _ = PostMessageW(Some(window), message, WPARAM(0), LPARAM(position));
    }
}

/// Which mouse button is held, as the `MK_` flag `DoDragDrop` reports it
/// with, or `None` when none is: a drag started without one would drop at
/// once.  `GetAsyncKeyState` reports physical buttons and the flags are
/// logical, so swapped buttons are mapped across.
fn held_button() -> Option<u32> {
    let down = |key: VIRTUAL_KEY| unsafe { GetAsyncKeyState(key.0 as i32) } < 0;
    let swapped = unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0;
    let (left, right) = if swapped { (MK_RBUTTON, MK_LBUTTON) } else { (MK_LBUTTON, MK_RBUTTON) };
    if down(VK_LBUTTON) {
        Some(left.0)
    } else if down(VK_RBUTTON) {
        Some(right.0)
    } else if down(VK_MBUTTON) {
        Some(MK_MBUTTON.0)
    } else {
        None
    }
}

pub fn start(win: RawWindowHandle, _: RawDisplayHandle, drag: Drag) -> std::result::Result<(), DragError> {
    // 1. The window, OLE and the one-drag-at-a-time rule.
    let RawWindowHandle::Win32(handle) = win else {
        return Err(DragError::Unsupported("this isn't a Win32 window".into()));
    };
    let window = HWND(handle.hwnd.get() as *mut c_void);
    ensure_ole()?;
    let guard = ActiveGuard::acquire()?;
    let button = held_button().ok_or(DragError::NoGesture)?;

    // 2. The objects OLE drives the drag with.  Nothing is written yet.
    let Drag { items, icon } = drag;
    let shared = Rc::new(RefCell::new(Shared { session: Some(Session::new(Payload::new(items), guard)), paths: None }));
    let data: IDataObject = DataObject {
        shared: shared.clone(),
        drop_effect_format: unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) } as u16,
        stored: RefCell::new(Vec::new()),
    }
    .into();
    let source: IDropSource = DropSource { button }.into();
    if let Some(icon) = icon.filter(DragIcon::is_valid)
        && let Err(e) = set_picture(&data, &icon)
    {
        eprintln!("slerp dragout: no drag picture: {e}");
    }

    // 3. The modal loop.  Only copying is allowed: the files are ours, and a
    //    move would let the target delete them while we still own them.
    let mut effect = DROPEFFECT_NONE;
    let result = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
    drop(data);

    // 4. A target that took the drop returned a copy.  Any other end,
    //    including a drop where nothing accepted it, is a cancel and the
    //    files go.  A target that copies on in the background gets the
    //    session's ten minutes of grace.
    let session = shared.borrow_mut().session.take();
    let dropped = result == DRAGDROP_S_DROP && effect != DROPEFFECT_NONE;
    let outcome = match session {
        Some(mut session) if dropped => session.finished(),
        Some(mut session) => session.cancelled(),
        None => Outcome::Cancelled,
    };
    if result.is_err() {
        return Err(DragError::Platform(format!("DoDragDrop failed: {}", windows::core::Error::from(result))));
    }

    // 5. The window never saw the button go up, because OLE's loop took the
    //    pointer.  winit counts presses to know when to release its mouse
    //    capture, and egui would think the button is still down, so tell the
    //    window it went up.  The posted message is handled once the current
    //    event is, and, being an event, also wakes the event loop for the
    //    frame in which the interface learns how the drag ended.
    post_button_up(window, button);
    super::report(outcome);
    Ok(())
}

//endregion

//region Drop source

/// Ends the drag as the user expects: Escape or a second button cancels it,
/// and releasing the button that started it drops.
#[implement(IDropSource)]
struct DropSource {
    /// The `MK_` flag of the button that started the drag.
    button: u32,
}

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, escape_pressed: BOOL, key_state: MODIFIERKEYS_FLAGS) -> HRESULT {
        let buttons = key_state.0 & (MK_LBUTTON.0 | MK_RBUTTON.0 | MK_MBUTTON.0);
        if escape_pressed.as_bool() || buttons & !self.button != 0 {
            DRAGDROP_S_CANCEL
        } else if buttons & self.button == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

//endregion

//region Data object

/// What the data object and [`start`] both reach: the drag's session while it
/// is under way, and the files once a target has asked for them.
struct Shared {
    /// `None` once the drag is over.
    session: Option<Session>,
    /// The files, kept for a target that asks again, or only after the drag
    /// ended because it copies on a background thread.
    paths: Option<Vec<PathBuf>>,
}

/// A format somebody stored with `SetData`: the bytes of a global memory
/// block, or a stream.
enum Stored {
    Memory(Vec<u8>),
    Stream(IStream),
}

impl Stored {
    fn tymed(&self) -> u32 {
        match self {
            Stored::Memory(_) => TYMED_HGLOBAL.0 as u32,
            Stored::Stream(_) => TYMED_ISTREAM.0 as u32,
        }
    }

    /// Copies a medium a caller passed to `SetData`.
    fn from_medium(medium: &STGMEDIUM) -> Result<Stored> {
        unsafe {
            if medium.tymed == TYMED_HGLOBAL.0 as u32 {
                read_hglobal(medium.u.hGlobal).map(Stored::Memory).ok_or_else(|| E_OUTOFMEMORY.into())
            } else if medium.tymed == TYMED_ISTREAM.0 as u32 {
                (*medium.u.pstm).clone().map(Stored::Stream).ok_or_else(|| E_POINTER.into())
            } else {
                Err(DV_E_TYMED.into())
            }
        }
    }

    /// A fresh medium for a caller of `GetData`, which releases it.  A stream
    /// is cloned so that each reader has a seek position of its own.
    fn medium(&self) -> Result<STGMEDIUM> {
        match self {
            Stored::Memory(bytes) => hglobal_medium(bytes),
            Stored::Stream(stream) => unsafe {
                let stream = stream.Clone()?;
                stream.Seek(0, STREAM_SEEK_SET, None)?;
                Ok(STGMEDIUM {
                    tymed: TYMED_ISTREAM.0 as u32,
                    u: STGMEDIUM_0 { pstm: ManuallyDrop::new(Some(stream)) },
                    pUnkForRelease: ManuallyDrop::new(None),
                })
            },
        }
    }
}

/// The dragged files as `CF_HDROP`, plus whatever formats other parties store
/// in it: the shell's drag-picture helper keeps the picture here, and a drop
/// target reports back the effect it carried out.  COM calls the object on
/// the thread that made it, so plain `Rc` and `RefCell` are enough.
#[implement(IDataObject)]
struct DataObject {
    shared: Rc<RefCell<Shared>>,
    /// The registered number of the "Preferred DropEffect" format.
    drop_effect_format: u16,
    /// The formats stored with `SetData`.
    stored: RefCell<Vec<(u16, Stored)>>,
}

/// Whether the pointer is over a window of this process.
fn pointer_over_own_window() -> bool {
    unsafe {
        let mut point = POINT::default();
        if GetCursorPos(&mut point).is_err() {
            return false;
        }
        let window = WindowFromPoint(point);
        if window.0.is_null() {
            return false;
        }
        let mut process = 0;
        GetWindowThreadProcessId(window, Some(&mut process));
        process == GetCurrentProcessId()
    }
}

fn format(cf: u16, tymed: u32) -> FORMATETC {
    FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed }
}

/// A global memory block holding `bytes`, in the form the caller of `GetData`
/// frees with `ReleaseStgMedium`.
fn hglobal_medium(bytes: &[u8]) -> Result<STGMEDIUM> {
    unsafe {
        let block = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
        let at = GlobalLock(block);
        if at.is_null() {
            let _ = GlobalFree(Some(block));
            return Err(E_OUTOFMEMORY.into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), at.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(block);
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: block },
            pUnkForRelease: ManuallyDrop::new(None),
        })
    }
}

/// The bytes of a global memory block, or `None` if it is too large to keep.
fn read_hglobal(block: HGLOBAL) -> Option<Vec<u8>> {
    unsafe {
        let size = GlobalSize(block);
        if size > MAX_STORED {
            return None;
        }
        let at = GlobalLock(block);
        if at.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(at.cast::<u8>(), size).to_vec();
        let _ = GlobalUnlock(block);
        Some(bytes)
    }
}

impl DataObject {
    /// Whether files can still be had at all: while the drag is under way,
    /// and afterwards if a target got them before.  This is what the list of
    /// formats says, and it stays the same wherever the pointer is, because
    /// a target may be handed a copy of that list instead of asking anew.
    fn has_files(&self) -> bool {
        match self.shared.try_borrow() {
            Ok(shared) => shared.session.is_some() || shared.paths.is_some(),
            Err(_) => false,
        }
    }

    /// Whether files are handed out right now: not to our own windows.
    fn hands_out_files(&self) -> bool {
        match self.shared.try_borrow() {
            Ok(shared) if shared.session.is_some() => !pointer_over_own_window(),
            Ok(shared) => shared.paths.is_some(),
            Err(_) => false,
        }
    }

    /// The `HRESULT` for a request of `wanted`, as `QueryGetData` and
    /// `GetData` both give it.
    fn check(&self, wanted: &FORMATETC) -> HRESULT {
        let tymed = if wanted.cfFormat == CF_HDROP.0 {
            self.hands_out_files().then_some(TYMED_HGLOBAL.0 as u32)
        } else if wanted.cfFormat == self.drop_effect_format {
            Some(TYMED_HGLOBAL.0 as u32)
        } else {
            self.stored.borrow().iter().find(|(cf, _)| *cf == wanted.cfFormat).map(|(_, stored)| stored.tymed())
        };
        match tymed {
            None => DV_E_FORMATETC,
            Some(_) if wanted.dwAspect != DVASPECT_CONTENT.0 => DV_E_DVASPECT,
            Some(_) if wanted.lindex != -1 => DV_E_LINDEX,
            Some(tymed) if wanted.tymed & tymed == 0 => DV_E_TYMED,
            Some(_) => S_OK,
        }
    }

    /// The dragged files, making them the first time anyone asks.
    fn files(&self) -> Result<Vec<PathBuf>> {
        let mut shared = self.shared.try_borrow_mut().map_err(|_| E_UNEXPECTED)?;
        if shared.paths.is_none() {
            let session = shared.session.as_mut().ok_or(E_UNEXPECTED)?;
            let paths = session.payload().paths();
            shared.paths = Some(paths);
        }
        match &shared.paths {
            Some(paths) if !paths.is_empty() => Ok(paths.clone()),
            // Every item failed to produce its file (each said why on stderr).
            _ => Err(E_FAIL.into()),
        }
    }
}

#[allow(non_snake_case)]
impl IDataObject_Impl for DataObject_Impl {
    fn GetData(&self, wanted: *const FORMATETC) -> Result<STGMEDIUM> {
        let wanted = unsafe { wanted.as_ref() }.ok_or(E_POINTER)?;
        self.check(wanted).ok()?;
        if wanted.cfFormat == CF_HDROP.0 {
            let paths: Vec<Vec<u16>> = self.files()?.iter().map(|p| p.as_os_str().encode_wide().collect()).collect();
            hglobal_medium(&bytes::drop_files(&paths))
        } else if wanted.cfFormat == self.drop_effect_format {
            hglobal_medium(&DROPEFFECT_COPY.0.to_le_bytes())
        } else {
            let stored = self.stored.borrow();
            let (_, stored) = stored.iter().find(|(cf, _)| *cf == wanted.cfFormat).ok_or(DV_E_FORMATETC)?;
            stored.medium()
        }
    }

    fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, wanted: *const FORMATETC) -> HRESULT {
        match unsafe { wanted.as_ref() } {
            Some(wanted) => self.check(wanted),
            None => E_POINTER,
        }
    }

    fn GetCanonicalFormatEtc(&self, wanted: *const FORMATETC, canonical: *mut FORMATETC) -> HRESULT {
        // Every format has one rendering, whatever the target device says.
        if let (Some(wanted), false) = (unsafe { wanted.as_ref() }, canonical.is_null()) {
            unsafe { *canonical = FORMATETC { ptd: std::ptr::null_mut(), ..*wanted } };
            DATA_S_SAMEFORMATETC
        } else {
            E_POINTER
        }
    }

    fn SetData(&self, wanted: *const FORMATETC, medium: *const STGMEDIUM, release: BOOL) -> Result<()> {
        let (Some(wanted), Some(copy)) = (unsafe { wanted.as_ref() }, unsafe { medium.as_ref() }) else {
            return Err(E_POINTER.into());
        };
        let copy = Stored::from_medium(copy)?;
        let mut stored = self.stored.borrow_mut();
        stored.retain(|(cf, _)| *cf != wanted.cfFormat);
        stored.push((wanted.cfFormat, copy));
        // The caller handed the medium over: our copy is the only one now.
        if release.as_bool() {
            unsafe { ReleaseStgMedium(medium as *mut STGMEDIUM) };
        }
        Ok(())
    }

    fn EnumFormatEtc(&self, direction: u32) -> Result<IEnumFORMATETC> {
        if direction != DATADIR_GET.0 as u32 {
            return Err(E_NOTIMPL.into());
        }
        let memory = TYMED_HGLOBAL.0 as u32;
        let mut formats = Vec::new();
        if self.has_files() {
            formats.push(format(CF_HDROP.0, memory));
        }
        formats.push(format(self.drop_effect_format, memory));
        formats.extend(self.stored.borrow().iter().map(|(cf, stored)| format(*cf, stored.tymed())));
        unsafe { SHCreateStdEnumFmtEtc(&formats) }
    }

    fn DAdvise(&self, _: *const FORMATETC, _: u32, _: Ref<IAdviseSink>) -> Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _: u32) -> Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

//endregion

//region Drag picture

/// A bitmap with an alpha channel of the picture, in the form the shell
/// wants: 32 bits per pixel, premultiplied, top row first.
fn picture_bitmap(icon: &DragIcon) -> Result<HBITMAP> {
    let (width, height) = (icon.width as i32, icon.height as i32);
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let pixels = bytes::premultiplied_bgra(&icon.rgba);
    unsafe {
        let bitmap = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u8>(), pixels.len());
        Ok(bitmap)
    }
}

/// Gives the drag a picture, with the shell's drag-image helper.  It keeps
/// the picture in the data object (as extra formats, through `SetData`),
/// where the drop target's own helper finds it, so only targets that show
/// drag pictures at all, such as Explorer, display it.  The pointer is held
/// at the picture's middle.
fn set_picture(data: &IDataObject, icon: &DragIcon) -> Result<()> {
    unsafe {
        let helper: IDragSourceHelper = CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER)?;
        let bitmap = picture_bitmap(icon)?;
        let (width, height) = (icon.width as i32, icon.height as i32);
        let image = SHDRAGIMAGE {
            sizeDragImage: SIZE { cx: width, cy: height },
            ptOffset: POINT { x: width / 2, y: height / 2 },
            hbmpDragImage: bitmap,
            crColorKey: COLORREF(0x00FF_00FF),
        };
        // The helper owns the bitmap once it has taken it.
        helper.InitializeFromBitmap(&image, data).inspect_err(|_| {
            let _ = DeleteObject(bitmap.into());
        })
    }
}

//endregion
