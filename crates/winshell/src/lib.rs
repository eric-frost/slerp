//! Windows Explorer integration for Slerp.
//!
//! The crate builds `slerp_shell.dll`, an in-process COM server whose
//! thumbnail handler draws Explorer's thumbnails through `slerp-core`,
//! exactly as `slerp thumbnail` does on Linux, and `slerpw.exe`, the
//! console-less launcher Explorer runs to open files in the viewer.
//! The association manager (`slerp_associate::windows`, re-exported as
//! [`registry`] on Windows) registers both for the current user, with the
//! user's choice of formats; `DllRegisterServer`, `slerp.exe register`,
//! `slerp associate` and the viewer's File Types page all use it.  On other
//! platforms only the protocol is compiled.

pub mod protocol;
#[cfg(windows)]
pub use slerp_associate::windows as registry;

#[cfg(windows)]
mod bitmap;
#[cfg(windows)]
mod log;
#[cfg(windows)]
mod pipe;
#[cfg(windows)]
mod render;
#[cfg(windows)]
mod source;
#[cfg(windows)]
mod thumbnail;

#[cfg(windows)]
pub use self::server::*;

#[cfg(windows)]
mod server {
    use crate::log::log;
    use crate::thumbnail::Thumbnailer;
    use std::ffi::c_void;
    use std::path::PathBuf;
    use windows::Win32::Foundation::{
        CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_POINTER, HMODULE, S_FALSE, S_OK,
    };
    use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
    use windows::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW,
        GetModuleHandleExW,
    };
    use windows::core::{BOOL, GUID, HRESULT, IUnknown, Interface, PCWSTR, Ref, implement};

    /// The thumbnail handler's class id, registered to run in Explorer's
    /// process ([`crate::registry::THUMBNAIL_CLSID`]).
    pub const THUMBNAIL_CLSID: GUID = GUID::from_u128(0x1f742344_7356_4426_940a_feddd3f29297);
    /// The same handler under the class id that stays in Explorer's
    /// isolated process, for `.png` ([`crate::registry::ISOLATED_CLSID`]).
    pub const ISOLATED_CLSID: GUID = GUID::from_u128(0x7d3362eb_7b17_4b74_879a_444c3b2b23a8);

    /// Creates thumbnail handlers for COM.
    #[implement(IClassFactory)]
    struct Factory;

    impl IClassFactory_Impl for Factory_Impl {
        fn CreateInstance(
            &self,
            outer: Ref<IUnknown>,
            riid: *const GUID,
            object: *mut *mut c_void,
        ) -> windows::core::Result<()> {
            if object.is_null() || riid.is_null() {
                return Err(E_POINTER.into());
            }
            // SAFETY: checked non-null; COM wants it cleared on failure.
            unsafe { *object = std::ptr::null_mut() };
            if outer.is_some() {
                return Err(CLASS_E_NOAGGREGATION.into());
            }
            let handler: IUnknown = Thumbnailer::new().into();
            // SAFETY: riid and object are valid, as checked above.
            unsafe { handler.query(riid, object).ok() }
        }

        fn LockServer(&self, _lock: BOOL) -> windows::core::Result<()> {
            Ok(())
        }
    }

    /// COM's entry point: hands out the class factory for Slerp's
    /// thumbnail handler, under either of its class ids.
    ///
    /// # Safety
    ///
    /// COM passes valid pointers, as its contract requires.
    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn DllGetClassObject(
        clsid: *const GUID,
        riid: *const GUID,
        out: *mut *mut c_void,
    ) -> HRESULT {
        if clsid.is_null() || riid.is_null() || out.is_null() {
            return E_POINTER;
        }
        install_panic_log();
        // SAFETY: checked non-null.
        unsafe { *out = std::ptr::null_mut() };
        let clsid = unsafe { *clsid };
        log!("DllGetClassObject {clsid:?} in {}", std::env::current_exe().unwrap_or_default().display());
        if clsid != THUMBNAIL_CLSID && clsid != ISOLATED_CLSID {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = Factory.into();
        // SAFETY: riid and out are valid, as checked above.
        unsafe { factory.query(riid, out) }
    }

    /// Never unload.  In the isolated thumbnail process the GPU renderer and
    /// its worker threads live as long as the process, and in Explorer the
    /// threads that ran the handler may keep Rust thread-local data whose
    /// clean-up code is in this DLL; unloading it would crash the host.  So
    /// replacing the DLL needs Explorer restarted.
    #[unsafe(no_mangle)]
    pub extern "system" fn DllCanUnloadNow() -> HRESULT {
        S_FALSE
    }

    /// Registers this DLL for the current user, with `slerpw.exe` (or
    /// `slerp.exe`) beside it as the viewer.  `regsvr32` and
    /// `slerp.exe register` call this.
    #[unsafe(no_mangle)]
    pub extern "system" fn DllRegisterServer() -> HRESULT {
        to_hresult(this_dll().and_then(|dll| {
            crate::registry::register(&crate::registry::Install::beside(&dll)).map(|_| ()).map_err(|e| {
                log!("registering: {e}");
                windows::core::Error::from(E_FAIL)
            })
        }))
    }

    /// Removes the current user's registration.
    #[unsafe(no_mangle)]
    pub extern "system" fn DllUnregisterServer() -> HRESULT {
        to_hresult(this_dll().and_then(|dll| {
            crate::registry::unregister(&crate::registry::Install::beside(&dll)).map(|_| ()).map_err(|e| {
                log!("unregistering: {e}");
                windows::core::Error::from(E_FAIL)
            })
        }))
    }

    /// Runs the thumbnail helper until it has been idle for the configured
    /// time, and returns the process exit code.  `slerp.exe helper`
    /// calls this; the handler in Explorer starts that on demand.
    #[unsafe(no_mangle)]
    pub extern "system" fn SlerpServe() -> i32 {
        install_panic_log();
        let idle = std::time::Duration::from_secs(slerp_core::Config::load().helper_idle_seconds);
        match std::panic::catch_unwind(|| crate::pipe::serve(idle)) {
            Ok(Ok(())) => 0,
            Ok(Err(e)) => {
                log!("thumbnail helper: {e}");
                1
            }
            Err(_) => 101,
        }
    }

    fn to_hresult(result: windows::core::Result<()>) -> HRESULT {
        match result {
            Ok(()) => S_OK,
            Err(e) => e.code(),
        }
    }

    /// The full path of this DLL.
    pub fn this_dll() -> windows::core::Result<PathBuf> {
        let mut module = HMODULE::default();
        // SAFETY: the address of a function in this DLL identifies it; the
        // reference count is left alone.
        unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                PCWSTR(DllRegisterServer as *const () as *const u16),
                &mut module,
            )?
        };
        let mut buf = vec![0u16; 1024];
        loop {
            // SAFETY: buf is valid for its length.
            let n = unsafe { GetModuleFileNameW(Some(module), &mut buf) } as usize;
            if n == 0 {
                return Err(E_FAIL.into());
            }
            if n < buf.len() {
                return Ok(PathBuf::from(String::from_utf16_lossy(&buf[..n])));
            }
            buf.resize(buf.len() * 2, 0);
        }
    }

    /// Logs panics with their location before they are caught, so the log
    /// says where a failing thumbnail went wrong.  The hook belongs to this
    /// DLL's copy of the Rust runtime, so the host process isn't affected.
    fn install_panic_log() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            if crate::log::enabled() {
                std::panic::set_hook(Box::new(|info| log!("{info}")));
            }
        });
    }
}
