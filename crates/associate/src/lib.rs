//! Slerp's file association manager: for each format in the format table
//! (`slerp_formats`), whether Slerp makes the file manager's thumbnails
//! and whether Slerp opens it by default, as the user chose
//! (`slerp_core::choices`), applied to the desktop and undone exactly.
//!
//! - Linux and other freedesktop desktops ([`freedesktop`]): per-user
//!   thumbnailer files, Dolphin's preview plugin list, `mimeapps.list`.
//! - Windows ([`windows`]): Explorer's thumbnail handler and "Open with"
//!   per extension in the user's registry.  Windows protects the default
//!   app with a hash only its own settings can write, so Slerp offers
//!   itself and opens the Default apps page.
//! - macOS ([`macos`]): the Quick Look extension declines the formats
//!   switched off; LaunchServices sets default apps.
//!
//! Every change outside Slerp's own settings goes into a [`Record`] in the
//! state folder, so [`reset`] can put back what was there, other programs'
//! defaults included.  Callers use [`apply`], [`reset`] and [`status`];
//! [`Context::system`] finds the files.

pub mod ini;
pub mod record;

#[cfg(not(any(windows, target_os = "macos")))]
pub mod freedesktop;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod windows;

pub use record::Record;
use slerp_core::choices::Choices;
use slerp_formats::Format;
use std::path::PathBuf;

/// What applying or resetting did (or, for a dry run, would do).
#[derive(Debug, Default)]
pub struct Report {
    /// One line per change, for the user.
    pub changes: Vec<String>,
    /// Things the user should know: what Slerp couldn't do, what to
    /// restart, choices given up because another program changed them.
    pub notes: Vec<String>,
    /// Whether the choices changed (and were saved) while applying.
    pub choices_changed: bool,
}

impl Report {
    pub(crate) fn change(&mut self, line: impl Into<String>) {
        self.changes.push(line.into());
    }

    pub(crate) fn note(&mut self, line: impl Into<String>) {
        let line = line.into();
        if !self.notes.contains(&line) {
            self.notes.push(line);
        }
    }
}

/// One format's choices and what handles it on this system now.
#[derive(Debug)]
pub struct Status {
    pub format: &'static Format,
    pub thumbnails: bool,
    pub open_by_default: bool,
    pub replace_kde_previews: bool,
    /// KDE thumbnail plugins that claim the format and win over Slerp in
    /// Dolphin.
    pub kde_rivals: Vec<String>,
    /// What makes its thumbnails here now ("Slerp", "KDE's imagethumbnail"),
    /// if anything.
    pub thumbnails_by: Option<String>,
    /// What opens it by default now, if anything.
    pub opens_with: Option<String>,
    pub notes: Vec<String>,
}

/// Where the manager reads and writes.
#[derive(Clone, Debug)]
pub struct Context {
    /// The user's choices, `file-types.jsonc` in the settings folder.
    pub choices_path: PathBuf,
    /// The record of changes, in the state folder.
    pub record_path: PathBuf,
    #[cfg(not(any(windows, target_os = "macos")))]
    pub env: freedesktop::Env,
    #[cfg(windows)]
    pub install: self::windows::Install,
}

impl Context {
    /// This user's files and this system's desktop.
    pub fn system() -> Result<Context, String> {
        let choices_path = Choices::path().ok_or("no settings folder")?;
        let record_path = slerp_core::config::state_dir().ok_or("no state folder")?.join("file-types-applied.json");
        Ok(Context {
            choices_path,
            record_path,
            #[cfg(not(any(windows, target_os = "macos")))]
            env: freedesktop::Env::from_system(),
            #[cfg(windows)]
            install: self::windows::Install::beside_this_program(),
        })
    }

    pub fn load_choices(&self) -> Choices {
        Choices::load_from(&self.choices_path)
    }
}

/// Makes the desktop follow `choices`, records what that changed, and saves
/// `choices` when applying had to change them.  A dry run reports and
/// changes nothing.
pub fn apply(ctx: &Context, choices: &mut Choices, dry_run: bool) -> Result<Report, String> {
    let mut record = Record::load(&ctx.record_path);
    #[cfg(not(any(windows, target_os = "macos")))]
    let report = freedesktop::apply(&ctx.env, choices, &mut record, dry_run);
    #[cfg(windows)]
    let report = self::windows::apply(&ctx.install, choices, &mut record, dry_run);
    #[cfg(target_os = "macos")]
    let report = macos::apply(choices, &mut record, dry_run);
    if !dry_run {
        record.save(&ctx.record_path).map_err(|e| format!("{}: {e}", ctx.record_path.display()))?;
        if report.choices_changed {
            choices.save_to(&ctx.choices_path).map_err(|e| format!("{}: {e}", ctx.choices_path.display()))?;
        }
    }
    Ok(report)
}

/// Undoes every change the manager made, from its record: files and
/// registry values return to what they held, other apps' defaults
/// included.  The user's choices stay, so `apply` brings them back.
pub fn reset(ctx: &Context, dry_run: bool) -> Result<Report, String> {
    let mut record = Record::load(&ctx.record_path);
    #[cfg(not(any(windows, target_os = "macos")))]
    let report = freedesktop::reset(&ctx.env, &mut record, dry_run);
    #[cfg(windows)]
    let report = self::windows::reset(&ctx.install, &mut record, dry_run);
    #[cfg(target_os = "macos")]
    let report = macos::reset(&mut record, dry_run);
    if !dry_run {
        record.save(&ctx.record_path).map_err(|e| format!("{}: {e}", ctx.record_path.display()))?;
    }
    Ok(report)
}

/// Every format's choices and what handles it now.
pub fn status(ctx: &Context, choices: &Choices) -> Vec<Status> {
    #[cfg(not(any(windows, target_os = "macos")))]
    return freedesktop::status(&ctx.env, choices);
    #[cfg(windows)]
    return self::windows::status(&ctx.install, choices);
    #[cfg(target_os = "macos")]
    {
        let _ = ctx;
        macos::status(choices)
    }
}

/// What the platform can't do, for the settings page and `--list`.
pub fn limits() -> &'static [&'static str] {
    #[cfg(not(any(windows, target_os = "macos")))]
    return &[
        "Dolphin uses KDE's own thumbnail plugins for the types they claim (DDS, Blender files), whatever Slerp's switch says, unless KDE's plugin is replaced for that format.",
        "File managers read their thumbnailer lists when they start: restart Dolphin to see a change.",
    ];
    #[cfg(windows)]
    return &[
        "Windows protects the default app with a hash only its Settings can write, so Slerp opens Settings > Default apps for you to choose it.",
        "Explorer keeps thumbnails it has made; a type switched off shows its old thumbnails until Disk Cleanup clears the thumbnail cache.",
    ];
    #[cfg(target_os = "macos")]
    return &[
        "Quick Look keeps the types macOS declares itself (glTF, USD, DDS, Radiance HDR, PNG and other pictures) for Apple's own thumbnailers.",
        "macOS may ask you to confirm a new default app.",
    ];
}
