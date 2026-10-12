//! macOS.
//!
//! Thumbnails: Quick Look asks Slerp's thumbnail extension for every type
//! it declares in `QLSupportedContentTypes` (`data/macos/Thumbnails-Info.plist`,
//! all the types Slerp imports), and nothing can change that list per user
//! without re-signing the app.  So the extension reads the user's choices
//! itself (`file-types.jsonc`, which its sandbox may read through a
//! temporary-exception entitlement for `~/Library/Application
//! Support/Slerp/`) and declines the formats switched off, leaving the file
//! its generic icon.  When every format is off, the manager switches the
//! extension off altogether with `pluginkit`.
//!
//! Default apps: LaunchServices' default role handler per type identifier.
//! macOS keeps one handler per type, so the previous handler is recorded and
//! set again when Slerp's default is switched off or reset.

use crate::{Record, Report, Status};
use slerp_core::choices::Choices;
use slerp_formats::FORMATS;
use std::ffi::{CStr, c_char, c_void};

/// The app's bundle id, which LaunchServices names handlers by.
pub const BUNDLE_ID: &str = "com.slerp3d.Slerp";
/// The Quick Look extension's bundle id.
pub const EXTENSION_ID: &str = "com.slerp3d.Slerp.Thumbnails";

/// Makes LaunchServices and Quick Look follow `choices`.
pub fn apply(choices: &mut Choices, record: &mut Record, dry_run: bool) -> Report {
    let mut report = Report::default();

    // 1. Default apps, type by type.
    for f in FORMATS {
        let wanted = choices.open_by_default(f);
        for uti in f.utis() {
            let now = default_handler(uti);
            let ours = now.as_deref().is_some_and(|b| b.eq_ignore_ascii_case(BUNDLE_ID));
            if wanted && !ours {
                report.change(format!("{uti}: open with Slerp (was {})", now.as_deref().unwrap_or("nothing")));
                if !dry_run {
                    record.mac_defaults.entry(uti.to_owned()).or_insert(now);
                    if let Err(code) = set_default_handler(uti, BUNDLE_ID) {
                        report.note(format!("LaunchServices refused Slerp as the default for {uti} (error {code})"));
                    }
                }
            } else if !wanted && let Some(previous) = record.mac_defaults.get(uti).cloned() {
                if ours && let Some(app) = &previous {
                    report.change(format!("{uti}: open with {app} again"));
                    if !dry_run {
                        let _ = set_default_handler(uti, app);
                    }
                }
                if !dry_run {
                    record.mac_defaults.remove(uti);
                }
            }
        }
    }

    // 2. Quick Look: the extension declines what is off; with nothing on,
    //    it is switched off.
    let any_on = FORMATS.iter().any(|f| choices.thumbnails(f) && !f.imported_utis().is_empty());
    if !any_on && !record.mac_extension_ignored {
        report.change("Quick Look extension switched off");
        if !dry_run && pluginkit("ignore") {
            record.mac_extension_ignored = true;
        }
    } else if any_on && record.mac_extension_ignored {
        report.change("Quick Look extension switched on");
        if !dry_run && pluginkit("use") {
            record.mac_extension_ignored = false;
        }
    }
    let types: Vec<String> = choices.thumbnail_types().into_iter().map(str::to_owned).collect();
    if types != record.thumbnail_types && !dry_run {
        if !record.thumbnail_types.is_empty() || choices.thumbnails != Default::default() {
            // Finder keeps thumbnails until the file changes; clear them.
            let _ = std::process::Command::new("qlmanage").args(["-r", "cache"]).output();
            report.change("cleared Quick Look's thumbnail cache");
        }
        record.thumbnail_types = types;
    }
    report
}

/// Puts back the default apps Slerp replaced and switches the extension
/// back on if the manager switched it off.
pub fn reset(record: &mut Record, dry_run: bool) -> Report {
    let mut report = Report::default();
    for (uti, previous) in std::mem::take(&mut record.mac_defaults) {
        let ours = default_handler(&uti).is_some_and(|b| b.eq_ignore_ascii_case(BUNDLE_ID));
        match previous {
            Some(app) if ours => {
                report.change(format!("{uti}: open with {app} again"));
                if !dry_run {
                    let _ = set_default_handler(&uti, &app);
                }
            }
            None if ours => {
                report.note(format!("{uti} had no default app before; macOS keeps Slerp until another app is chosen"))
            }
            _ => {}
        }
    }
    if record.mac_extension_ignored {
        report.change("Quick Look extension switched on again");
        if !dry_run && pluginkit("use") {
            record.mac_extension_ignored = false;
        }
    }
    // A dry run's record isn't saved, so taking its entries changes nothing.
    if !dry_run {
        record.thumbnail_types.clear();
    }
    report
}

/// Every format's choices and its default app now.
pub fn status(choices: &Choices) -> Vec<Status> {
    FORMATS
        .iter()
        .map(|f| {
            let utis = f.utis();
            let thumbnails_by = if !f.imported_utis().is_empty() && choices.thumbnails(f) {
                Some("Slerp".to_owned())
            } else if matches!(f.mac, slerp_formats::Mac::System(_)) {
                Some("macOS".to_owned())
            } else {
                None
            };
            let opens_with = utis
                .first()
                .and_then(|u| default_handler(u))
                .map(|b| if b.eq_ignore_ascii_case(BUNDLE_ID) { "Slerp".to_owned() } else { b });
            let mut notes = Vec::new();
            if matches!(f.mac, slerp_formats::Mac::System(_)) && choices.thumbnails(f) {
                notes.push(
                    "macOS declares this type itself, so Quick Look keeps it for Apple's thumbnailers.".to_owned(),
                );
            }
            Status {
                format: f,
                thumbnails: choices.thumbnails(f),
                open_by_default: choices.open_by_default(f),
                replace_kde_previews: false,
                kde_rivals: Vec::new(),
                thumbnails_by,
                opens_with,
                notes,
            }
        })
        .collect()
}

fn pluginkit(mode: &str) -> bool {
    std::process::Command::new("pluginkit")
        .args(["-e", mode, "-i", EXTENSION_ID])
        .output()
        .is_ok_and(|o| o.status.success())
}

/*┌─────────────────╮
| LaunchServices  |
└─────────────────╯*/
//region LaunchServices

type CFStringRef = *const c_void;
const UTF8: u32 = 0x0800_0100;
/// Every role: viewer, editor, shell.
const ROLES_ALL: u32 = 0xFFFF_FFFF;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithBytes(
        alloc: *const c_void,
        bytes: *const u8,
        len: isize,
        encoding: u32,
        bom: u8,
    ) -> CFStringRef;
    fn CFStringGetCString(s: CFStringRef, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFRelease(cf: *const c_void);
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn LSCopyDefaultRoleHandlerForContentType(content_type: CFStringRef, role: u32) -> CFStringRef;
    fn LSSetDefaultRoleHandlerForContentType(content_type: CFStringRef, role: u32, bundle: CFStringRef) -> i32;
}

/// A CoreFoundation string, released when dropped.
struct CfString(CFStringRef);

impl CfString {
    fn new(s: &str) -> Option<CfString> {
        // SAFETY: the bytes are valid for their length; a null result is
        // checked.
        let r = unsafe { CFStringCreateWithBytes(std::ptr::null(), s.as_ptr(), s.len() as isize, UTF8, 0) };
        (!r.is_null()).then_some(CfString(r))
    }

    fn to_string(r: CFStringRef) -> Option<String> {
        if r.is_null() {
            return None;
        }
        let mut buf = vec![0 as c_char; 512];
        // SAFETY: the buffer's length is passed; the result is NUL-terminated
        // on success.
        let ok = unsafe { CFStringGetCString(r, buf.as_mut_ptr(), buf.len() as isize, UTF8) };
        // SAFETY: as above.
        (ok != 0).then(|| unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned())
    }
}

impl Drop for CfString {
    fn drop(&mut self) {
        // SAFETY: created by a Create function, released once.
        unsafe { CFRelease(self.0) };
    }
}

/// The bundle id of the app that opens `uti` now.
pub fn default_handler(uti: &str) -> Option<String> {
    let t = CfString::new(uti)?;
    // SAFETY: a valid string; the result follows the Copy rule.
    let r = unsafe { LSCopyDefaultRoleHandlerForContentType(t.0, ROLES_ALL) };
    let out = CfString::to_string(r);
    if !r.is_null() {
        drop(CfString(r));
    }
    out
}

/// Makes `bundle` the app that opens `uti`, or returns LaunchServices'
/// error code.
pub fn set_default_handler(uti: &str, bundle: &str) -> Result<(), i32> {
    let (Some(t), Some(b)) = (CfString::new(uti), CfString::new(bundle)) else { return Err(-1) };
    // SAFETY: valid strings.
    match unsafe { LSSetDefaultRoleHandlerForContentType(t.0, ROLES_ALL, b.0) } {
        0 => Ok(()),
        code => Err(code),
    }
}

//endregion
