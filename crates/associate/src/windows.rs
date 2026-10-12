//! Windows: registering Slerp with Explorer for the current user, without
//! administrator rights: the thumbnail handler's COM classes, the handler
//! key for each extension whose thumbnails the user wants from Slerp, a
//! ProgID that puts the viewer in "Open with", and the capabilities that
//! list Slerp in Settings > Default apps.
//!
//! Everything lives under `HKEY_CURRENT_USER\Software`, which Windows merges
//! over the machine-wide classes, so the user's entries win and removing
//! them brings back whatever the machine has.  A per-user handler another
//! program registered for an extension is recorded before Slerp replaces
//! it, and put back when Slerp's thumbnails for it are switched off.
//!
//! Windows 10 and 11 keep the user's default app for an extension in a
//! `UserChoice` key protected by a hash that only Windows' own settings
//! write, so no program may set a default silently: Slerp offers itself
//! in "Open with" and opens the Default apps page for the user to choose.
//!
//! The registry changes are worked out as plain data ([`writes`],
//! [`removals`]) so they can be checked on any platform; the rest of the
//! module makes them on Windows.  `slerp.exe register`,
//! `DllRegisterServer`, `slerp associate` and the viewer's File Types page
//! all come through here.

use slerp_core::choices::Choices;
use slerp_formats::FORMATS;

/// The thumbnail handler's class id.  It runs in Explorer's own process
/// (`DisableProcessIsolation`), the only way Explorer hands a handler the
/// file's path rather than a stream of its bytes, which models with
/// separate `.bin`, texture, `.lit` or skin files need.  It draws nothing
/// there: the work happens in the thumbnail helper process.
pub const THUMBNAIL_CLSID: &str = "{1F742344-7356-4426-940A-FEDDD3F29297}";

/// The same handler under a class id that stays in Explorer's isolated
/// thumbnail process, for `.png`: ordinary PNGs are handed to Windows'
/// own decoder, which belongs in the isolated process as before.
pub const ISOLATED_CLSID: &str = "{7D3362EB-7B17-4B74-879A-444C3B2B23A8}";

/// The shell's id for thumbnail handlers (the `IThumbnailProvider`
/// interface id): an extension's `ShellEx\{this}` key names its handler.
pub const THUMBNAIL_HANDLER_KEY: &str = "{e357fccd-a995-4576-b01f-234630154e96}";

/// The ProgID that offers the viewer in "Open with".
pub const PROG_ID: &str = "Slerp.Asset";

/// Where the classes live inside `HKEY_CURRENT_USER`.
const CLASSES: &str = r"Software\Classes";

/// Slerp's capabilities, which Settings > Default apps lists.
const CAPABILITIES: &str = r"Software\Slerp\Capabilities";

/// The list of applications that have capabilities.
const REGISTERED_APPLICATIONS: &str = r"Software\RegisteredApplications";

/// Slerp's name in [`REGISTERED_APPLICATIONS`] and in Settings.
pub const APP_NAME: &str = "Slerp";

/*┌─────────────────╮
| Plan            |
└─────────────────╯*/
//region Plan

/// The extensions to register: those with Slerp's thumbnails, and those
/// offering the viewer in "Open with".  Lower case, without dots, each
/// once; only single-part extensions, since Windows matches the last part
/// alone (`.nii.gz` would mean every `.gz`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub thumbnails: Vec<&'static str>,
    pub open_with: Vec<&'static str>,
}

impl Selection {
    /// What `choices` ask for.  `.png` gets Slerp's (isolated) handler when
    /// 16-bit greyscale heightmaps are on: Explorer can't tell a heightmap
    /// from a photo by name, so the handler looks inside and hands
    /// ordinary PNGs to Windows' own thumbnailer.
    pub fn from_choices(choices: &Choices) -> Selection {
        let mut sel = Selection::default();
        for f in FORMATS {
            if choices.thumbnails(f) {
                let exts = if f.id == "png-16bit" { vec!["png"] } else { f.simple_extensions() };
                for e in exts {
                    if !sel.thumbnails.contains(&e) {
                        sel.thumbnails.push(e);
                    }
                }
            }
            if f.offered_on(slerp_formats::WINDOWS) || choices.open_by_default(f) {
                for e in f.simple_extensions() {
                    if !sel.open_with.contains(&e) {
                        sel.open_with.push(e);
                    }
                }
            }
        }
        sel
    }

    /// Every extension Slerp could register, for undoing all of it.
    pub fn everything() -> Selection {
        let mut sel = Selection::default();
        for f in FORMATS {
            for e in f.simple_extensions().into_iter().chain(if f.id == "png-16bit" { Some("png") } else { None }) {
                if !sel.thumbnails.contains(&e) {
                    sel.thumbnails.push(e);
                }
                if !sel.open_with.contains(&e) {
                    sel.open_with.push(e);
                }
            }
        }
        sel
    }
}

/// A registry value's data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Data {
    /// `REG_SZ`.
    String(String),
    /// `REG_DWORD`.
    Dword(u32),
    /// `REG_NONE` with no bytes, the form `OpenWithProgids` entries take.
    Empty,
}

/// Sets one value, creating its key as needed.  Keys are relative to
/// `HKEY_CURRENT_USER`; an empty `name` is the key's default value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Write {
    pub key: String,
    pub name: String,
    pub data: Data,
}

/// One step of unregistering, relative to `HKEY_CURRENT_USER`.  Steps run
/// in order, so a key's values go before the key itself is tested for
/// emptiness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Removal {
    /// Deletes a key Slerp owns, with everything under it.
    Tree(String),
    /// Deletes a value when it still holds `equals` (any data when `None`),
    /// so a handler another program registered since is left alone.
    Value { key: String, name: String, equals: Option<String> },
    /// Deletes a key when nothing is left in it, so keys Slerp created
    /// for its own values don't linger, while shared ones stay.
    KeyIfEmpty(String),
}

/// The key naming an extension's thumbnail handler.
pub fn handler_key(ext: &str) -> String {
    format!(r"{CLASSES}\.{ext}\ShellEx\{THUMBNAIL_HANDLER_KEY}")
}

/// The registry values that register Slerp for `sel`.  `dll` is the full
/// path of `slerp_shell.dll`; `viewer` the program that opens files,
/// usually `slerpw.exe` beside it, which is run as `"<viewer>" view "%1"`.
pub fn writes(dll: &str, viewer: &str, sel: &Selection) -> Vec<Write> {
    let mut out = Vec::new();
    let mut set = |key: String, name: &str, data: Data| out.push(Write { key, name: name.to_owned(), data });
    let text = |s: &str| Data::String(s.to_owned());

    // 1. The COM classes, both served by the DLL.
    for (id, name) in [(THUMBNAIL_CLSID, "Slerp thumbnail handler"), (ISOLATED_CLSID, "Slerp PNG heightmaps")] {
        let clsid = format!(r"{CLASSES}\CLSID\{id}");
        set(clsid.clone(), "", text(name));
        set(format!(r"{clsid}\InprocServer32"), "", text(dll));
        set(format!(r"{clsid}\InprocServer32"), "ThreadingModel", text("Apartment"));
        if id == THUMBNAIL_CLSID {
            set(clsid, "DisableProcessIsolation", Data::Dword(1));
        }
    }

    // 2. The ProgID behind "Open with".  Thumbnails come from the
    //    extensions' own keys, which Explorer still reads when Slerp
    //    is made the default program.
    let prog = format!(r"{CLASSES}\{PROG_ID}");
    set(prog.clone(), "", text("Game or 3D asset"));
    set(prog.clone(), "FriendlyTypeName", text("Game or 3D asset"));
    // No badge on thumbnails: without it Explorer stamps the associated
    // program's icon in the corner, a blank window for these types.
    set(prog.clone(), "TypeOverlay", text(""));
    set(format!(r"{prog}\Application"), "ApplicationName", text("Slerp"));
    set(format!(r"{prog}\shell\open"), "FriendlyAppName", text("Slerp"));
    set(format!(r"{prog}\shell\open\command"), "", Data::String(format!("\"{viewer}\" view \"%1\"")));
    // "Open with" names a program by its FriendlyAppName here when the
    // executable carries no version information.
    if let Some(exe) = file_name(viewer) {
        set(format!(r"{CLASSES}\Applications\{exe}"), "FriendlyAppName", text("Slerp"));
    }

    // 3. Each extension: the thumbnail handler, and the "Open with" entry.
    for ext in &sel.thumbnails {
        set(handler_key(ext), "", text(handler_for(ext)));
    }
    for ext in &sel.open_with {
        set(format!(r"{CLASSES}\.{ext}\OpenWithProgids"), PROG_ID, Data::Empty);
    }

    // 4. Settings > Default apps: Slerp's capabilities and the types it
    //    can be chosen for.
    set(CAPABILITIES.into(), "ApplicationName", text(APP_NAME));
    set(CAPABILITIES.into(), "ApplicationDescription", text("Thumbnails and a viewer for game and 3D asset files"));
    for ext in &sel.open_with {
        set(format!(r"{CAPABILITIES}\FileAssociations"), &format!(".{ext}"), text(PROG_ID));
    }
    set(REGISTERED_APPLICATIONS.into(), APP_NAME, text(CAPABILITIES));
    out
}

/// The steps that undo `writes` for `sel`'s extensions (and, with
/// `everything`, the classes, ProgID and capabilities too).  Handlers
/// another program put back since are left alone.
pub fn removals(viewer: &str, sel: &Selection, everything: bool) -> Vec<Removal> {
    let mut out = Vec::new();
    for ext in &sel.thumbnails {
        let handler = handler_key(ext);
        for id in [THUMBNAIL_CLSID, ISOLATED_CLSID] {
            out.push(Removal::Value { key: handler.clone(), name: String::new(), equals: Some(id.into()) });
        }
        out.push(Removal::KeyIfEmpty(handler));
        out.push(Removal::KeyIfEmpty(format!(r"{CLASSES}\.{ext}\ShellEx")));
        out.push(Removal::KeyIfEmpty(format!(r"{CLASSES}\.{ext}")));
    }
    for ext in &sel.open_with {
        let open_with = format!(r"{CLASSES}\.{ext}\OpenWithProgids");
        out.push(Removal::Value { key: open_with.clone(), name: PROG_ID.into(), equals: None });
        out.push(Removal::KeyIfEmpty(open_with));
        out.push(Removal::Value {
            key: format!(r"{CAPABILITIES}\FileAssociations"),
            name: format!(".{ext}"),
            equals: None,
        });
        out.push(Removal::KeyIfEmpty(format!(r"{CLASSES}\.{ext}")));
    }
    if everything {
        if let Some(exe) = file_name(viewer) {
            let app = format!(r"{CLASSES}\Applications\{exe}");
            out.push(Removal::Value { key: app.clone(), name: "FriendlyAppName".into(), equals: None });
            out.push(Removal::KeyIfEmpty(app));
        }
        out.push(Removal::Tree(format!(r"{CLASSES}\{PROG_ID}")));
        out.push(Removal::Tree(format!(r"{CLASSES}\CLSID\{THUMBNAIL_CLSID}")));
        out.push(Removal::Tree(format!(r"{CLASSES}\CLSID\{ISOLATED_CLSID}")));
        out.push(Removal::Value { key: REGISTERED_APPLICATIONS.into(), name: APP_NAME.into(), equals: None });
        out.push(Removal::Tree(CAPABILITIES.into()));
        out.push(Removal::KeyIfEmpty(r"Software\Slerp".into()));
    }
    out
}

/// The class that makes thumbnails for `ext`.
pub fn handler_for(ext: &str) -> &'static str {
    if ext == "png" { ISOLATED_CLSID } else { THUMBNAIL_CLSID }
}

/// The last component of a Windows path.
fn file_name(path: &str) -> Option<&str> {
    path.rsplit(['\\', '/']).next().filter(|s| !s.is_empty())
}

/// The record's name for a value: its key and name joined by `|`.
pub fn value_id(key: &str, name: &str) -> String {
    format!("{key}|{name}")
}

//endregion

/*┌─────────────────╮
| Windows         |
└─────────────────╯*/
//region Windows

#[cfg(windows)]
pub use self::windows_impl::*;

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use crate::{Record, Report, Status};
    use std::path::{Path, PathBuf};
    use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
    use windows_registry::{CLASSES_ROOT, CURRENT_USER, Type};

    /// Where Slerp is installed: the thumbnail handler DLL and the viewer's
    /// launcher.
    #[derive(Clone, Debug)]
    pub struct Install {
        pub dll: PathBuf,
        pub viewer: PathBuf,
    }

    impl Install {
        /// The files beside the running program (`slerp.exe` and the DLL
        /// live together).
        pub fn beside_this_program() -> Install {
            let exe = std::env::current_exe().unwrap_or_default();
            Install::beside(&exe.with_file_name("slerp_shell.dll"))
        }

        /// The files beside the handler DLL at `dll`: the windowless
        /// launcher `slerpw.exe` when present, else `slerp.exe`.
        pub fn beside(dll: &Path) -> Install {
            let dir = dll.parent().unwrap_or(Path::new("."));
            let launcher = dir.join("slerpw.exe");
            let viewer = if launcher.is_file() { launcher } else { dir.join("slerp.exe") };
            Install { dll: dll.to_path_buf(), viewer }
        }
    }

    /// Registers what `choices` ask for and unregisters the rest, recording
    /// other programs' handlers it replaces.
    pub fn apply(install: &Install, choices: &mut Choices, record: &mut Record, dry_run: bool) -> Report {
        let mut report = Report::default();
        let (dll, viewer) = (install.dll.to_string_lossy(), install.viewer.to_string_lossy());
        let sel = Selection::from_choices(choices);
        let all = Selection::everything();

        // 1. Extensions no longer wanted: Slerp's handler and "Open with"
        //    entry go, and a handler Slerp replaced comes back.
        let gone = Selection {
            thumbnails: all.thumbnails.iter().copied().filter(|e| !sel.thumbnails.contains(e)).collect(),
            open_with: all.open_with.iter().copied().filter(|e| !sel.open_with.contains(e)).collect(),
        };
        for ext in &gone.thumbnails {
            let id = value_id(&handler_key(ext), "");
            if read_string(&handler_key(ext), "").is_some_and(|v| is_ours(&v)) {
                report.change(format!(".{ext}: Slerp's thumbnails off"));
            }
            if !dry_run && let Some(previous) = record.registry.remove(&id).flatten() {
                apply_removals(&removals(&viewer, &Selection { thumbnails: vec![ext], open_with: Vec::new() }, false));
                let _ =
                    apply_write(&Write { key: handler_key(ext), name: String::new(), data: Data::String(previous) });
            }
        }
        if !dry_run {
            apply_removals(&removals(&viewer, &gone, false));
        }

        // 2. Wanted extensions: note any other program's handler first.
        for ext in &sel.thumbnails {
            let key = handler_key(ext);
            let now = read_string(&key, "");
            if !now.as_deref().is_some_and(is_ours) {
                report.change(format!(".{ext}: Slerp's thumbnails on"));
                if !dry_run {
                    record.registry.entry(value_id(&key, "")).or_insert(now);
                }
            }
        }
        if !dry_run {
            for w in writes(&dll, &viewer, &sel) {
                if let Err(e) = apply_write(&w) {
                    report.note(format!("couldn't write {}\\{}: {e}", w.key, w.name));
                }
            }
            notify();
        }
        report.note(format!(
            "{} extensions have Slerp's thumbnails, {} offer Slerp in \"Open with\".",
            sel.thumbnails.len(),
            sel.open_with.len()
        ));
        if FORMATS.iter().any(|f| choices.open_by_default(f)) {
            report.note(
                "Windows lets only its own Settings choose default apps: pick Slerp for these types in Settings > Apps > Default apps > Slerp (`slerp associate --default on` opens it).",
            );
        }
        report
    }

    /// Unregisters everything and puts back the handlers Slerp replaced.
    pub fn reset(install: &Install, record: &mut Record, dry_run: bool) -> Report {
        let mut report = Report::default();
        report.change("unregistered Slerp's thumbnail handler, \"Open with\" entries and capabilities");
        if dry_run {
            return report;
        }
        apply_removals(&removals(&install.viewer.to_string_lossy(), &Selection::everything(), true));
        for (id, previous) in std::mem::take(&mut record.registry) {
            let Some((key, name)) = id.split_once('|') else { continue };
            if let Some(data) = previous {
                report.change(format!("restored {key}"));
                let _ = apply_write(&Write { key: key.into(), name: name.into(), data: Data::String(data) });
            }
        }
        notify();
        report
    }

    /// What handles each format's first extension now.
    pub fn status(_install: &Install, choices: &Choices) -> Vec<Status> {
        FORMATS
            .iter()
            .map(|f| {
                let ext = f.simple_extensions().first().copied();
                let thumbnails_by = ext.and_then(|e| {
                    let clsid = read_root_string(&format!(r".{e}\ShellEx\{THUMBNAIL_HANDLER_KEY}"), "")?;
                    if is_ours(&clsid) {
                        return Some("Slerp".into());
                    }
                    Some(read_root_string(&format!(r"CLSID\{clsid}"), "").unwrap_or(clsid))
                });
                let opens_with = ext.and_then(default_program);
                Status {
                    format: f,
                    thumbnails: choices.thumbnails(f),
                    open_by_default: choices.open_by_default(f),
                    replace_kde_previews: false,
                    kde_rivals: Vec::new(),
                    thumbnails_by,
                    opens_with,
                    notes: Vec::new(),
                }
            })
            .collect()
    }

    /// The program the user chose for `.ext`, or the machine's.
    fn default_program(ext: &str) -> Option<String> {
        let choice = format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.{ext}\UserChoice");
        let prog = CURRENT_USER
            .open(&choice)
            .ok()
            .and_then(|k| k.get_string("ProgId").ok())
            .or_else(|| read_root_string(&format!(".{ext}"), ""))?;
        if prog == PROG_ID {
            return Some("Slerp".into());
        }
        Some(read_root_string(&prog, "FriendlyTypeName").or_else(|| read_root_string(&prog, "")).unwrap_or(prog))
    }

    /// Opens Settings > Default apps on Slerp's page.
    pub fn open_default_apps_settings() {
        let _ = std::process::Command::new("explorer.exe")
            .arg(format!("ms-settings:defaultapps?registeredAppUser={APP_NAME}"))
            .spawn();
    }

    fn is_ours(clsid: &str) -> bool {
        clsid.eq_ignore_ascii_case(THUMBNAIL_CLSID) || clsid.eq_ignore_ascii_case(ISOLATED_CLSID)
    }

    fn read_string(key: &str, name: &str) -> Option<String> {
        CURRENT_USER.open(key).ok()?.get_string(name).ok()
    }

    fn read_root_string(key: &str, name: &str) -> Option<String> {
        CLASSES_ROOT.open(key).ok()?.get_string(name).ok()
    }

    fn notify() {
        // SAFETY: SHCNE_ASSOCCHANGED takes no item lists.
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    }

    fn apply_write(w: &Write) -> windows::core::Result<()> {
        let key = CURRENT_USER.create(&w.key)?;
        match &w.data {
            Data::String(s) => key.set_string(&w.name, s),
            Data::Dword(v) => key.set_u32(&w.name, *v),
            Data::Empty => key.set_bytes(&w.name, Type::Other(0), &[]),
        }
    }

    /// Runs removal steps, ignoring keys and values that are already gone.
    fn apply_removals(steps: &[Removal]) {
        for step in steps {
            let _ = apply_removal(step);
        }
    }

    fn apply_removal(step: &Removal) -> windows::core::Result<()> {
        match step {
            Removal::Tree(path) => {
                if CURRENT_USER.open(path).is_ok() {
                    CURRENT_USER.remove_tree(path)?;
                }
            }
            Removal::Value { key, name, equals } => {
                let Ok(k) = CURRENT_USER.options().read().write().open(key) else { return Ok(()) };
                let matches = match equals {
                    Some(want) => k.get_string(name).is_ok_and(|have| have.eq_ignore_ascii_case(want)),
                    None => k.get_type(name).is_ok(),
                };
                if matches {
                    k.remove_value(name)?;
                }
            }
            Removal::KeyIfEmpty(path) => {
                let Ok(k) = CURRENT_USER.open(path) else { return Ok(()) };
                let empty = k.keys()?.next().is_none() && k.values()?.next().is_none();
                drop(k);
                if empty {
                    CURRENT_USER.remove_tree(path)?;
                }
            }
        }
        Ok(())
    }

    /// Registers Slerp with the user's saved choices: what `slerp.exe
    /// register` and `regsvr32` do.
    pub fn register(install: &Install) -> Result<Report, String> {
        let ctx = crate::Context { install: install.clone(), ..crate::Context::system()? };
        let mut choices = ctx.load_choices();
        crate::apply(&ctx, &mut choices, false)
    }

    /// Undoes everything: what `slerp.exe unregister` and `regsvr32 /u` do.
    pub fn unregister(install: &Install) -> Result<Report, String> {
        let ctx = crate::Context { install: install.clone(), ..crate::Context::system()? };
        crate::reset(&ctx, false)
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    const DLL: &str = r"C:\Tools\Slerp\slerp_shell.dll";
    const VIEWER: &str = r"C:\Tools\Slerp\slerpw.exe";

    #[test]
    fn the_defaults_register_every_thumbnail_extension_and_open_with() {
        let sel = Selection::from_choices(&Choices::default());
        let writes = writes(DLL, VIEWER, &sel);
        for ext in ["glb", "ktx2", "dds", "png", "fbx", "wad", "pk3", "fits", "xnb", "dna", "3dm", "bsp", "map"] {
            let registered = writes.iter().any(|w| w.key == handler_key(ext));
            // .map is confirmed by content only, never claimed by name.
            assert_eq!(registered, ext != "map", "{ext}");
        }
        assert_eq!(handler_for("png"), ISOLATED_CLSID);
        // Ordinary pictures are offered in "Open with" but keep Windows'
        // thumbnails; CorelDRAW and Visio get thumbnails, not "Open with".
        assert!(!sel.thumbnails.contains(&"gif") && sel.open_with.contains(&"gif"));
        assert!(sel.thumbnails.contains(&"vsdx") && !sel.open_with.contains(&"vsdx"));
        assert!(!sel.thumbnails.iter().chain(&sel.open_with).any(|e| e.contains('.')));
        let open = r"Software\Classes\.glb\OpenWithProgids";
        assert!(writes.iter().any(|w| w.key == open && w.name == PROG_ID && w.data == Data::Empty));
        assert!(writes.iter().any(|w| w.key == r"Software\RegisteredApplications" && w.name == APP_NAME));
    }

    #[test]
    fn choices_change_the_selection() {
        let mut c = Choices::default();
        c.set_thumbnails(slerp_formats::by_id("glb").unwrap(), false);
        c.set_thumbnails(slerp_formats::by_id("gif").unwrap(), true);
        c.set_thumbnails(slerp_formats::by_id("png-16bit").unwrap(), false);
        let sel = Selection::from_choices(&c);
        assert!(
            !sel.thumbnails.contains(&"glb") && sel.thumbnails.contains(&"gif") && !sel.thumbnails.contains(&"png")
        );
        assert!(sel.open_with.contains(&"glb"));
    }

    #[test]
    fn classes_point_at_the_dll_in_an_apartment_and_only_one_leaves_isolation() {
        let writes = writes(DLL, VIEWER, &Selection::default());
        let value =
            |key: &str, name: &str| writes.iter().find(|w| w.key == key && w.name == name).map(|w| w.data.clone());
        for id in [THUMBNAIL_CLSID, ISOLATED_CLSID] {
            let server = format!(r"Software\Classes\CLSID\{id}\InprocServer32");
            assert_eq!(value(&server, ""), Some(Data::String(DLL.into())));
            assert_eq!(value(&server, "ThreadingModel"), Some(Data::String("Apartment".into())));
        }
        let isolation = |id: &str| value(&format!(r"Software\Classes\CLSID\{id}"), "DisableProcessIsolation");
        assert_eq!(isolation(THUMBNAIL_CLSID), Some(Data::Dword(1)));
        assert_eq!(isolation(ISOLATED_CLSID), None);
        let command = writes.iter().find(|w| w.key.ends_with(r"shell\open\command")).unwrap();
        assert_eq!(command.data, Data::String(format!("\"{VIEWER}\" view \"%1\"")));
        assert!(writes.iter().any(|w| w.key == r"Software\Classes\Applications\slerpw.exe"));
    }

    #[test]
    fn removals_undo_every_key_written() {
        let all = Selection::everything();
        let removals = removals(VIEWER, &all, true);
        for w in writes(DLL, VIEWER, &all) {
            let undone = removals.iter().any(|r| match r {
                Removal::Tree(path) => w.key == *path || w.key.starts_with(&format!(r"{path}\")),
                Removal::Value { key, name, .. } => *key == w.key && *name == w.name,
                Removal::KeyIfEmpty(_) => false,
            });
            assert!(undone, "{w:?} is never removed");
        }
        // Shared extension keys are only removed when empty, never as trees.
        assert!(!removals.iter().any(|r| matches!(r, Removal::Tree(p) if p.contains(r"Classes\."))));
    }
}
