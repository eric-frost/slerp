//! A drop target for testing drag-out on Windows: a plain window that
//! registers an OLE drop target and writes to a log file what a drag offers
//! and what it receives.  For each dropped file the log has the path, the size
//! and a checksum, so a test can tell the right bytes arrived; around each
//! request for the files it also lists drag-out's temporary folders, which
//! shows when the files were written.
//!
//! ```text
//! dragout_target.exe <log-file> [--at X Y WIDTH HEIGHT] [--eager] [--quit-after SECONDS]
//! ```
//!
//! `--eager` asks for the files as soon as the drag enters the window, as
//! some targets do while hovering; without it they are asked for at the drop.

#[cfg(not(windows))]
fn main() {
    eprintln!("dragout_target is for Windows");
}

#[cfg(windows)]
fn main() {
    target::run();
}

#[cfg(windows)]
mod target {
    use std::fmt::Display;
    use std::io::Write;
    use std::sync::Mutex;
    use std::time::Instant;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINTL, WPARAM};
    use windows::Win32::Graphics::Gdi::{COLOR_WINDOW, HBRUSH};
    use windows::Win32::System::Com::{DATADIR_GET, DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL};
    use windows::Win32::System::DataExchange::GetClipboardFormatNameW;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Ole::{
        CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl, OleInitialize,
        RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop,
    };
    use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    use windows::Win32::UI::WindowsAndMessaging::{
        CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, IDC_ARROW,
        KillTimer, LoadCursorW, MSG, PostQuitMessage, RegisterClassW, SW_SHOW, SetTimer, ShowWindow, WINDOW_EX_STYLE,
        WM_DESTROY, WM_TIMER, WNDCLASSW, WS_OVERLAPPEDWINDOW,
    };
    use windows::core::{PCWSTR, Ref, Result, implement, w};

    static LOG: Mutex<Option<(std::fs::File, Instant)>> = Mutex::new(None);

    fn log(message: impl Display) {
        if let Some((file, start)) = LOG.lock().unwrap().as_mut() {
            let _ = writeln!(file, "[{:7.3}] {message}", start.elapsed().as_secs_f64());
        }
    }

    /// The files in drag-out's folders of `%TEMP%`, with sizes.
    fn staging() -> String {
        let mut listing = Vec::new();
        if let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) {
            for dir in entries.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("slerp-drag-")) {
                let files: Vec<String> = std::fs::read_dir(dir.path())
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|f| {
                        format!(
                            "{} ({} bytes)",
                            f.file_name().to_string_lossy(),
                            f.metadata().map(|m| m.len()).unwrap_or(0)
                        )
                    })
                    .collect();
                listing.push(format!("{}: [{}]", dir.file_name().to_string_lossy(), files.join(", ")));
            }
        }
        if listing.is_empty() { "no drag folders".into() } else { listing.join("; ") }
    }

    /// The names of the formats a data object offers.
    fn formats(data: &IDataObject) -> String {
        let mut names = Vec::new();
        unsafe {
            if let Ok(list) = data.EnumFormatEtc(DATADIR_GET.0 as u32) {
                let mut format = [FORMATETC::default()];
                let mut fetched = 0;
                while list.Next(&mut format, Some(&mut fetched)).is_ok() && fetched == 1 {
                    let cf = format[0].cfFormat;
                    let mut name = [0u16; 128];
                    let length = GetClipboardFormatNameW(cf as u32, &mut name);
                    names.push(if cf == CF_HDROP.0 {
                        "CF_HDROP".to_string()
                    } else if length > 0 {
                        String::from_utf16_lossy(&name[..length as usize])
                    } else {
                        format!("#{cf}")
                    });
                    fetched = 0;
                }
            }
        }
        names.join(", ")
    }

    fn checksum(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
    }

    /// Asks for the files the way a shell target does and logs each.
    fn read_files(data: &IDataObject, label: &str) -> bool {
        let wanted = FORMATETC {
            cfFormat: CF_HDROP.0,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        };
        log(format_args!("{label}: before GetData(CF_HDROP): {}", staging()));
        let mut medium = match unsafe { data.GetData(&wanted) } {
            Ok(medium) => medium,
            Err(e) => {
                log(format_args!("{label}: GetData(CF_HDROP) failed: {e}"));
                return false;
            }
        };
        log(format_args!("{label}: after GetData(CF_HDROP): {}", staging()));
        let hdrop = HDROP(unsafe { medium.u.hGlobal }.0);
        let count = unsafe { DragQueryFileW(hdrop, u32::MAX, None) };
        log(format_args!("{label}: {count} file(s)"));
        for i in 0..count {
            let length = unsafe { DragQueryFileW(hdrop, i, None) } as usize;
            let mut path = vec![0u16; length + 1];
            unsafe { DragQueryFileW(hdrop, i, Some(&mut path)) };
            let path = String::from_utf16_lossy(&path[..length]);
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let head: Vec<String> = bytes.iter().take(16).map(|b| format!("{b:02x}")).collect();
                    log(format_args!(
                        "{label}: file {i}: {path} size={} fnv={:016x} head={}",
                        bytes.len(),
                        checksum(&bytes),
                        head.join(" ")
                    ));
                }
                Err(e) => log(format_args!("{label}: file {i}: {path} unreadable: {e}")),
            }
        }
        unsafe { ReleaseStgMedium(&mut medium) };
        true
    }

    #[implement(IDropTarget)]
    struct Target {
        eager: bool,
    }

    #[allow(non_snake_case)]
    impl IDropTarget_Impl for Target_Impl {
        fn DragEnter(
            &self,
            data: Ref<IDataObject>,
            _: MODIFIERKEYS_FLAGS,
            at: &POINTL,
            effect: *mut DROPEFFECT,
        ) -> Result<()> {
            let data = data.ok()?;
            log(format_args!("DragEnter at ({}, {}); formats: {}; {}", at.x, at.y, formats(data), staging()));
            let ok = !self.eager || read_files(data, "enter");
            unsafe { *effect = if ok { DROPEFFECT_COPY } else { DROPEFFECT_NONE } };
            Ok(())
        }

        fn DragOver(&self, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
            unsafe { *effect = DROPEFFECT_COPY };
            Ok(())
        }

        fn DragLeave(&self) -> Result<()> {
            log("DragLeave");
            Ok(())
        }

        fn Drop(
            &self,
            data: Ref<IDataObject>,
            _: MODIFIERKEYS_FLAGS,
            at: &POINTL,
            effect: *mut DROPEFFECT,
        ) -> Result<()> {
            let data = data.ok()?;
            log(format_args!("Drop at ({}, {})", at.x, at.y));
            let ok = read_files(data, "drop");
            unsafe { *effect = if ok { DROPEFFECT_COPY } else { DROPEFFECT_NONE } };
            log(format_args!("Drop done, {}", if ok { "accepted" } else { "rejected" }));
            Ok(())
        }
    }

    unsafe extern "system" fn window_proc(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        match message {
            WM_TIMER => {
                log("quitting");
                unsafe {
                    let _ = KillTimer(Some(window), 1);
                    let _ = RevokeDragDrop(window);
                    let _ = DestroyWindow(window);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(window, message, w, l) },
        }
    }

    pub fn run() {
        let mut args = std::env::args().skip(1);
        let Some(log_path) = args.next() else {
            eprintln!("usage: dragout_target <log-file> [--at X Y W H] [--eager] [--quit-after SECONDS]");
            return;
        };
        let (mut rect, mut eager, mut quit_after) = ([CW_USEDEFAULT, CW_USEDEFAULT, 480, 320], false, 0u32);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--at" => {
                    for slot in rect.iter_mut() {
                        *slot = args.next().and_then(|v| v.parse().ok()).unwrap_or(*slot);
                    }
                }
                "--eager" => eager = true,
                "--quit-after" => quit_after = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
                other => eprintln!("ignoring {other}"),
            }
        }
        *LOG.lock().unwrap() = std::fs::File::create(&log_path).ok().map(|f| (f, Instant::now()));
        log(format_args!("target starting, eager={eager}"));
        unsafe {
            OleInitialize(None).expect("OleInitialize");
            let instance = GetModuleHandleW(None).expect("module").into();
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as usize as *mut _),
                lpszClassName: w!("SlerpDragoutTarget"),
                ..Default::default()
            };
            RegisterClassW(&class);
            let window = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("SlerpDragoutTarget"),
                PCWSTR(w!("Slerp drag target").as_ptr()),
                WS_OVERLAPPEDWINDOW,
                rect[0],
                rect[1],
                rect[2],
                rect[3],
                None,
                None,
                Some(instance),
                None,
            )
            .expect("window");
            let target: IDropTarget = Target { eager }.into();
            RegisterDragDrop(window, &target).expect("RegisterDragDrop");
            let _ = ShowWindow(window, SW_SHOW);
            if quit_after > 0 {
                SetTimer(Some(window), 1, quit_after * 1000, None);
            }
            log("ready");
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {
                DispatchMessageW(&message);
            }
        }
        log("target ended");
    }
}
