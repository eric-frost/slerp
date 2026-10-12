//! Linux and other freedesktop desktops.
//!
//! Thumbnails: file managers find thumbnailers as `.thumbnailer` files in
//! `$XDG_DATA_HOME/thumbnailers` and each `$XDG_DATA_DIRS/thumbnailers`.
//! The manager writes a per-user `slerp.thumbnailer` listing the chosen MIME
//! types.  KDE's KIO (`KFileUtils::findAllUniqueFiles`), Thunar's tumbler
//! and PCManFM-Qt's libfm-qt keep only the first file of each name, user's
//! folder first, so it hides the system-wide one.  GNOME's thumbnail factory
//! (`gnome-desktop-thumbnail.c`, also inside Nemo and Caja) reads every
//! file and gives each type to the first file that claims it, so the
//! system-wide file still claims what the user switched off; for those
//! types the manager writes per-user files that claim them first: a copy of
//! the thumbnailer that would otherwise come next (glycin's, say), or one
//! that makes no thumbnail.  It writes those only where a GNOME-family file
//! manager is installed, since KDE lists every `.thumbnailer` file in
//! Dolphin's preview settings.
//!
//! Dolphin uses only the plugins named in `[PreviewSettings] Plugins` of
//! `dolphinrc` (all of them but `textthumbnail` when the key is missing),
//! and KIO prefers KDE's compiled plugins for any type they claim exactly;
//! the manager adds `slerp` to the list and, only when asked per format,
//! takes out the KDE plugin that wins over Slerp.
//!
//! Default apps: `[Default Applications]` in `$XDG_CONFIG_HOME/mimeapps.list`,
//! which KDE and GNOME both read, set only for the formats the user chose.

pub mod apps;
mod cache;
pub mod kio;

use crate::record::{Edit, Record, plan_file, reset_file};
use crate::{Report, Status};
use apps::{MimeDb, Thumbnailer};
use slerp_core::choices::Choices;
use slerp_formats::{FORMATS, Format};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Where things are.  [`Env::from_system`] reads the XDG variables; tests
/// point every folder into a temporary one.
#[derive(Clone, Debug)]
pub struct Env {
    pub home: PathBuf,
    pub config_home: PathBuf,
    pub config_dirs: Vec<PathBuf>,
    pub data_home: PathBuf,
    pub data_dirs: Vec<PathBuf>,
    pub cache_home: PathBuf,
    /// `XDG_CURRENT_DESKTOP`'s names, lower case (`kde`, `gnome`).
    pub desktops: Vec<String>,
    /// Where Qt plugins live, for KDE's thumbnail plugins.
    pub qt_plugin_dirs: Vec<PathBuf>,
    /// The program thumbnailer files run (`slerp`, or a full path).
    pub program: String,
    /// Whether to write the per-user files GNOME's thumbnail factory needs.
    pub gnome: bool,
    /// Whether to rebuild desktop caches (KDE's ksycoca) after a change.
    pub refresh: bool,
}

/// The desktop entry the defaults name.
pub const DESKTOP_ID: &str = "slerp.desktop";
/// Slerp's thumbnailer's file name, which is also its plugin id in Dolphin.
const THUMBNAILER: &str = "slerp";

impl Env {
    pub fn from_system() -> Env {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
        let home = var("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
        let dirs = |name: &str, default: &str| -> Vec<PathBuf> {
            let v = var(name).unwrap_or_else(|| default.into());
            std::env::split_paths(&v).filter(|p| p.is_absolute()).collect()
        };
        let gnome = match std::env::var("SLERP_GNOME_THUMBNAILERS").as_deref() {
            Ok("1") => true,
            Ok("0") => false,
            _ => ["nautilus", "nemo", "caja"].iter().any(|p| apps::on_path(p)),
        };
        Env {
            config_home: var("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config")),
            config_dirs: dirs("XDG_CONFIG_DIRS", "/etc/xdg"),
            data_home: var("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share")),
            data_dirs: dirs("XDG_DATA_DIRS", "/usr/local/share:/usr/share"),
            cache_home: var("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".cache")),
            desktops: std::env::var("XDG_CURRENT_DESKTOP")
                .unwrap_or_default()
                .split(':')
                .filter(|s| !s.is_empty())
                .map(str::to_ascii_lowercase)
                .collect(),
            qt_plugin_dirs: kio::plugin_dirs(),
            program: "slerp".into(),
            gnome,
            refresh: true,
            home,
        }
    }

    /// `$XDG_DATA_HOME`, then `$XDG_DATA_DIRS` without repeats.
    pub fn data_dirs_all(&self) -> Vec<PathBuf> {
        let mut out = vec![self.data_home.clone()];
        for d in &self.data_dirs {
            let d = PathBuf::from(d.to_string_lossy().trim_end_matches('/'));
            if !out.contains(&d) {
                out.push(d);
            }
        }
        out
    }

    fn thumbnailer_dir(&self) -> PathBuf {
        self.data_home.join("thumbnailers")
    }

    fn dolphinrc(&self) -> PathBuf {
        self.config_home.join("dolphinrc")
    }

    fn mimeapps(&self) -> PathBuf {
        self.config_home.join("mimeapps.list")
    }

    fn is_kde(&self) -> bool {
        self.desktops.iter().any(|d| d == "kde")
    }

    /// A path for messages, with `~` for the home folder.
    pub fn show(&self, path: &Path) -> String {
        match path.strip_prefix(&self.home) {
            Ok(rest) if self.home != Path::new("/") => format!("~/{}", rest.display()),
            _ => path.display().to_string(),
        }
    }
}

/*┌─────────────────╮
| Thumbnails      |
└─────────────────╯*/
//region Thumbnails

/// Everything the manager knows about this system's thumbnailers.
struct Scene {
    thumbnailers: Vec<Thumbnailer>,
    kde: Vec<kio::KdePlugin>,
    db: MimeDb,
}

impl Scene {
    fn load(env: &Env) -> Scene {
        Scene { thumbnailers: apps::thumbnailers(env), kde: kio::plugins(&env.qt_plugin_dirs), db: MimeDb::load(env) }
    }

    /// The MIME types the system-wide Slerp thumbnailers claim: what file
    /// managers used before the manager wrote anything.
    fn system_types(&self, env: &Env) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for t in &self.thumbnailers {
            if t.id == THUMBNAILER && !t.path.starts_with(&env.data_home) {
                for m in &t.mime_types {
                    if !out.contains(m) {
                        out.push(m.clone());
                    }
                }
            }
        }
        out
    }

    /// Dolphin's preview plugin list: `dolphinrc`'s, or KIO's default (every
    /// plugin but `textthumbnail`) when it names none.
    fn dolphin_plugins(&self, env: &Env) -> (Option<Vec<String>>, Vec<String>) {
        let text = std::fs::read_to_string(env.dolphinrc()).unwrap_or_default();
        let set = crate::ini::Ini::parse(&text).get("PreviewSettings", "Plugins").map(|v| crate::ini::list(&v, ','));
        let default: Vec<String> = self
            .kde
            .iter()
            .map(|p| p.id.clone())
            .chain(apps::by_name_first(&self.thumbnailers).into_iter().map(|t| t.id.clone()))
            .filter(|id| id != "textthumbnail")
            .fold(Vec::new(), |mut acc, id| {
                if !acc.contains(&id) {
                    acc.push(id);
                }
                acc
            });
        (set, default)
    }

    /// The KDE plugins (only the `enabled` ones, when given) that claim
    /// exactly one of `format`'s types, and so win over Slerp in Dolphin.
    fn kde_rivals(&self, enabled: Option<&[String]>, format: &Format) -> Vec<&kio::KdePlugin> {
        let types: Vec<&str> = format.listed_mime().map(|m| self.db.canonical(m)).collect();
        self.kde
            .iter()
            .filter(|p| enabled.is_none_or(|e| e.contains(&p.id)))
            .filter(|p| p.mime_types.iter().any(|m| types.contains(&self.db.canonical(m))))
            .collect()
    }

    /// Who makes Dolphin's thumbnails of `mime` now: KIO's rule, exact type
    /// before parents before `type/*`, compiled plugins before thumbnailer
    /// files.
    fn kde_thumbnailer(&self, enabled: &[String], mime: &str) -> Option<String> {
        let files = apps::by_name_first(&self.thumbnailers);
        let mut candidates = vec![self.db.canonical(mime).to_owned()];
        candidates.extend(self.db.ancestors(mime));
        if let Some((major, _)) = mime.split_once('/') {
            candidates.push(format!("{major}/*"));
        }
        for t in candidates {
            if t == "application/octet-stream" {
                break;
            }
            if let Some(p) = self.kde.iter().find(|p| enabled.contains(&p.id) && p.mime_types.contains(&t)) {
                return Some(format!("KDE's {}", p.id));
            }
            if let Some(f) = files.iter().find(|f| enabled.contains(&f.id) && f.mime_types.contains(&t)) {
                return Some(if f.id == THUMBNAILER { "Slerp".to_owned() } else { f.id.clone() });
            }
        }
        None
    }
}

/// A `.thumbnailer` file's text.
fn thumbnailer_text(types: &[&str], try_exec: Option<&str>, exec: &str, why: &str) -> String {
    let mut out = format!("# {why}\n[Thumbnailer Entry]\n");
    if let Some(t) = try_exec {
        out.push_str(&format!("TryExec={t}\n"));
    }
    out.push_str(&format!("Exec={exec}\nMimeType="));
    for t in types {
        out.push_str(t);
        out.push(';');
    }
    out.push('\n');
    out
}

/// The files the manager writes in the user's thumbnailer folder.
fn own_thumbnailers(env: &Env, scene: &Scene, types: &[&'static str]) -> Vec<(PathBuf, String)> {
    let dir = env.thumbnailer_dir();
    let p = &env.program;
    let mut out = vec![(
        dir.join(format!("{THUMBNAILER}.thumbnailer")),
        thumbnailer_text(
            types,
            Some(p),
            &format!("{p} thumbnail %s %i %o"),
            "Slerp's thumbnailer with the file types chosen in `slerp associate` or Slerp's Settings > File Types; it hides the system-wide one.",
        ),
    )];
    if !env.gnome {
        return out;
    }
    // GNOME: claim first what the system-wide file claims and the user
    // switched off, handing each type to the thumbnailer next in line.
    let others: Vec<Thumbnailer> =
        scene.thumbnailers.iter().filter(|t| !t.id.starts_with(THUMBNAILER)).cloned().collect();
    let mut handoff: BTreeMap<String, (Thumbnailer, Vec<String>)> = BTreeMap::new();
    let mut none: Vec<String> = Vec::new();
    for t in scene.system_types(env) {
        if types.contains(&t.as_str()) {
            continue;
        }
        match apps::gnome_choice(&others, &t) {
            Some(next) => handoff.entry(next.id.clone()).or_insert_with(|| (next.clone(), Vec::new())).1.push(t),
            None => none.push(t),
        }
    }
    for (id, (next, list)) in handoff {
        let list: Vec<&str> = list.iter().map(String::as_str).collect();
        out.push((
            dir.join(format!("{THUMBNAILER}-handoff-{id}.thumbnailer")),
            thumbnailer_text(
                &list,
                next.try_exec.as_deref(),
                &next.exec,
                &format!(
                    "For GNOME's thumbnail factory: these types are switched off in Slerp, so {} makes their thumbnails.  Written by `slerp associate`.",
                    next.path.display()
                ),
            ),
        ));
    }
    if !none.is_empty() {
        let list: Vec<&str> = none.iter().map(String::as_str).collect();
        out.push((
            dir.join(format!("{THUMBNAILER}-off.thumbnailer")),
            thumbnailer_text(
                &list,
                None,
                // `false` makes no picture; GNOME refuses a command that
                // doesn't name the input.
                "false %i %o",
                "For GNOME's thumbnail factory: these types are switched off in Slerp and nothing else thumbnails them.  Written by `slerp associate`.",
            ),
        ));
    }
    out
}

//endregion

/*┌─────────────────╮
| Applying        |
└─────────────────╯*/
//region Applying

/// Makes the desktop follow `choices`, starting from what `record` says the
/// manager did before, which it updates.  Entries someone changed since
/// are given up and the matching choice switched off (`choices` then
/// changes and the report says so).
pub fn apply(env: &Env, choices: &mut Choices, record: &mut Record, dry_run: bool) -> Report {
    let scene = Scene::load(env);
    let mut report = Report::default();
    let types = choices.thumbnail_types();

    // 1. Slerp's own thumbnailer files.
    let wanted = own_thumbnailers(env, &scene, &types);
    sync_own_files(env, &wanted, record, dry_run, &mut report);

    // 2. Dolphin's plugin list.
    let (_, default) = scene.dolphin_plugins(env);
    // KDE plugins claiming a format whose previews the user wants from
    // Slerp, whether or not they are enabled now (the manager may have
    // switched them off before; removing an absent one changes nothing).
    let mut remove: Vec<String> = Vec::new();
    for f in FORMATS.iter().filter(|f| choices.replace_kde_previews(f)) {
        for p in scene.kde_rivals(None, f) {
            if !remove.contains(&p.id) {
                remove.push(p.id.clone());
            }
        }
    }
    // Someone who took Slerp out of Dolphin's list keeps it out, until
    // they change Slerp's own choice of formats.
    let thumbnails_changed = !record.thumbnail_types.is_empty()
        && record.thumbnail_types.iter().map(String::as_str).ne(types.iter().copied());
    if thumbnails_changed && !dry_run {
        record.dolphin_declined = false;
    }
    let add_slerp = !types.is_empty() && (!record.dolphin_declined || thumbnails_changed);
    let dolphin_edits = if scene.kde.is_empty() && !env.dolphinrc().exists() {
        Vec::new()
    } else {
        vec![Edit::List {
            section: "PreviewSettings".into(),
            key: "Plugins".into(),
            sep: ',',
            trailing: false,
            add: if add_slerp { vec![THUMBNAILER.into()] } else { Vec::new() },
            remove: remove.clone(),
            default: Some(default),
        }]
    };
    let lost = sync_edited_file(env, &env.dolphinrc(), &dolphin_edits, record, dry_run, &mut report);
    for (_, _, value) in lost {
        let without_slerp = value.as_deref().is_none_or(|v| !crate::ini::list(v, ',').iter().any(|p| p == THUMBNAILER));
        if without_slerp && !dry_run {
            record.dolphin_declined = true;
        }
        report.note(format!(
            "Dolphin's preview plugins were changed outside Slerp ({}), so Slerp leaves them as they are until its file types change.",
            value.unwrap_or_else(|| "removed".into())
        ));
    }
    for id in &remove {
        let Some(p) = scene.kde.iter().find(|p| &p.id == id) else { continue };
        let lost: Vec<&str> = p
            .mime_types
            .iter()
            .map(String::as_str)
            .filter(|m| !types.iter().any(|t| scene.db.canonical(t) == scene.db.canonical(m)))
            .collect();
        if !lost.is_empty() {
            report.note(format!(
                "KDE's {id} is off in Dolphin, so Dolphin no longer previews {} unless Slerp's thumbnails are on for them.",
                summarise(&lost)
            ));
        }
    }

    // 3. Default apps.
    let mut edits = Vec::new();
    for f in FORMATS.iter().filter(|f| choices.open_by_default(f)) {
        for m in f.listed_mime().filter(|m| scene.db.exists(m)) {
            edits.push(Edit::Value {
                section: "Default Applications".into(),
                key: m.into(),
                value: format!("{DESKTOP_ID};"),
            });
            if !f.offered_on(slerp_formats::LINUX) {
                edits.push(Edit::List {
                    section: "Added Associations".into(),
                    key: m.into(),
                    sep: ';',
                    trailing: true,
                    add: vec![DESKTOP_ID.into()],
                    remove: Vec::new(),
                    default: None,
                });
            }
        }
    }
    let lost = sync_edited_file(env, &env.mimeapps(), &edits, record, dry_run, &mut report);
    for (section, mime, value) in lost {
        if section != "Default Applications" {
            continue;
        }
        if let Some(f) = slerp_formats::by_mime(&mime)
            && choices.open_by_default(f)
        {
            choices.set_open_by_default(f, false);
            report.choices_changed = true;
            report.note(format!(
                "The default app for {mime} was changed outside Slerp (to {}), so Slerp no longer sets itself as the default for {}.",
                value.as_deref().unwrap_or("the desktop's choice"),
                f.name
            ));
        }
    }
    if !edits.is_empty() {
        let apps = apps::apps(env);
        if !apps.iter().any(|a| a.id == DESKTOP_ID) {
            report.note(format!("{DESKTOP_ID} isn't installed, so the defaults set for Slerp have no effect yet."));
        }
        for f in FORMATS.iter().filter(|f| choices.open_by_default(f)) {
            if let Some(h) = apps::default_app(env, &scene.db, &apps, f.main_mime())
                && h.app != DESKTOP_ID
                && h.as_parent.is_none()
            {
                report.note(format!(
                    "{} still opens with {} because {} names it first.",
                    f.name,
                    h.name,
                    h.file.as_deref().map(|p| env.show(p)).unwrap_or_else(|| "the desktop".into())
                ));
            }
        }
    }

    // 4. Cached thumbnails of the types whose thumbnailer changed.
    let before: BTreeSet<String> = if record.thumbnail_types.is_empty() {
        scene.system_types(env).into_iter().collect()
    } else {
        record.thumbnail_types.iter().cloned().collect()
    };
    let after: BTreeSet<String> = types.iter().map(|t| t.to_string()).collect();
    let changed: BTreeSet<String> = before.symmetric_difference(&after).cloned().collect();
    if !dry_run {
        record.thumbnail_types = types.iter().map(|t| t.to_string()).collect();
        if !changed.is_empty() {
            let n = cache::clear(&env.cache_home, &changed);
            report.change(format!(
                "cleared {n} cached thumbnails and failure markers of the {} changed types",
                changed.len()
            ));
        }
    }
    if !changed.is_empty() {
        report.note(
            "Restart Dolphin (or other file managers) to see the change: they read their plugin lists when they start.",
        );
    }
    refresh(env, &report, dry_run);
    report
}

/// Undoes everything in `record`.
pub fn reset(env: &Env, record: &mut Record, dry_run: bool) -> Report {
    let mut report = Report::default();
    sync_own_files(env, &[], record, dry_run, &mut report);
    let paths: Vec<String> = record.edited.keys().cloned().collect();
    for p in paths {
        sync_edited_file(env, Path::new(&p), &[], record, dry_run, &mut report);
    }
    if !dry_run {
        let scene = Scene::load(env);
        let now: BTreeSet<String> = scene.system_types(env).into_iter().collect();
        let before: BTreeSet<String> = record.thumbnail_types.iter().cloned().collect();
        let changed: BTreeSet<String> = before.symmetric_difference(&now).cloned().collect();
        if !changed.is_empty() {
            let n = cache::clear(&env.cache_home, &changed);
            report.change(format!("cleared {n} cached thumbnails of {} types", changed.len()));
        }
        record.thumbnail_types.clear();
        record.dolphin_declined = false;
    }
    refresh(env, &report, dry_run);
    report
}

/// Writes the wanted own files and restores those no longer wanted.
fn sync_own_files(env: &Env, wanted: &[(PathBuf, String)], record: &mut Record, dry_run: bool, report: &mut Report) {
    for (path, text) in wanted {
        let key = path.to_string_lossy().into_owned();
        let current = std::fs::read_to_string(path).ok();
        if current.as_deref() == Some(text.as_str()) {
            continue;
        }
        let types = text.lines().find_map(|l| l.strip_prefix("MimeType=")).map_or(0, |l| l.matches(';').count());
        report.change(format!("write {} ({types} types)", env.show(path)));
        if dry_run {
            continue;
        }
        if let Some(dir) = path.parent()
            && !dir.exists()
        {
            let mut d = dir.to_path_buf();
            // Note every folder created, outermost last.
            let mut made = Vec::new();
            while !d.exists() {
                made.push(d.to_string_lossy().into_owned());
                if !d.pop() {
                    break;
                }
            }
            if std::fs::create_dir_all(dir).is_ok() {
                record.created_dirs.extend(made);
            }
        }
        record.created.entry(key).or_insert(current);
        if let Err(e) = write_through(path, text) {
            report.note(format!("couldn't write {}: {e}", env.show(path)));
        }
    }
    let stale: Vec<String> = record
        .created
        .keys()
        .filter(|k| !wanted.iter().any(|(p, _)| p.to_string_lossy() == k.as_str()))
        .cloned()
        .collect();
    for key in stale {
        let path = PathBuf::from(&key);
        let before = record.created.get(&key).cloned().flatten();
        report.change(match &before {
            Some(_) => format!("restore {}", env.show(&path)),
            None => format!("remove {}", env.show(&path)),
        });
        if dry_run {
            continue;
        }
        let result = match &before {
            Some(text) => write_through(&path, text),
            None => match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
        };
        match result {
            Ok(()) => {
                record.created.remove(&key);
            }
            Err(e) => report.note(format!("couldn't restore {}: {e}", env.show(&path))),
        }
    }
    if wanted.is_empty() && !dry_run {
        // Folders the manager made, innermost first, if empty.
        let mut dirs = std::mem::take(&mut record.created_dirs);
        dirs.sort_by_key(|d| std::cmp::Reverse(d.len()));
        for d in dirs {
            if std::fs::remove_dir(&d).is_err() && Path::new(&d).exists() {
                // Not empty: someone else's files are in it now.
                continue;
            }
        }
    }
}

/// Brings one edited file to the state `edits` asks for, returning entries
/// someone else changed since.
fn sync_edited_file(
    env: &Env,
    path: &Path,
    edits: &[Edit],
    record: &mut Record,
    dry_run: bool,
    report: &mut Report,
) -> Vec<(String, String, Option<String>)> {
    let key = path.to_string_lossy().into_owned();
    let current = std::fs::read_to_string(path).ok();
    let previous = record.edited.get(&key);
    let outcome = if edits.is_empty() && previous.is_none() {
        return Vec::new();
    } else if edits.is_empty() {
        crate::record::FileOutcome {
            text: reset_file(current.as_deref(), previous.unwrap()),
            record: None,
            overridden: Vec::new(),
        }
    } else {
        plan_file(current.as_deref(), edits, previous)
    };
    if outcome.text != current {
        for line in describe(previous, outcome.record.as_ref()) {
            report.change(format!("{}: {line}", env.show(path)));
        }
        if !dry_run {
            let result = match &outcome.text {
                Some(t) => write_through(path, t),
                None => std::fs::remove_file(path),
            };
            if let Err(e) = result {
                report.note(format!("couldn't write {}: {e}", env.show(path)));
                return outcome.overridden;
            }
        }
    }
    if !dry_run {
        match outcome.record {
            Some(r) => {
                record.edited.insert(key, r);
            }
            None => {
                record.edited.remove(&key);
            }
        }
    }
    outcome.overridden
}

/// One line per entry the change makes or undoes.
fn describe(before: Option<&crate::record::FileRecord>, after: Option<&crate::record::FileRecord>) -> Vec<String> {
    let mut out = Vec::new();
    let empty = Vec::new();
    let old = before.map(|r| &r.entries).unwrap_or(&empty);
    let new = after.map(|r| &r.entries).unwrap_or(&empty);
    for e in new {
        if !old.contains(e) {
            out.push(format!(
                "[{}] {} = {} (was {})",
                e.section,
                e.key,
                e.after.as_deref().unwrap_or("nothing"),
                e.before.as_deref().unwrap_or("unset")
            ));
        }
    }
    for e in old {
        if !new.iter().any(|n| n.section == e.section && n.key == e.key) {
            out.push(format!("[{}] {} back to {}", e.section, e.key, e.before.as_deref().unwrap_or("unset")));
        }
    }
    out
}

/// Writes through a symbolic link to the file it names, as the link's
/// owner would expect, rather than replacing the link.
fn write_through(path: &Path, text: &str) -> std::io::Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    slerp_core::write_atomic(&target, text.as_bytes())
}

/// Tells KDE that associations changed.  (Other desktops read the files
/// again by themselves.)
fn refresh(env: &Env, report: &Report, dry_run: bool) {
    if dry_run || !env.refresh || !env.is_kde() || !report.changes.iter().any(|c| c.contains("mimeapps")) {
        return;
    }
    if apps::on_path("kbuildsycoca6") {
        let _ = std::process::Command::new("kbuildsycoca6")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// A few items of a list, for messages.
fn summarise(items: &[&str]) -> String {
    match items.len() {
        0 => "nothing".into(),
        1..=6 => items.join(", "),
        n => format!("{} and {} more types", items[..5].join(", "), n - 5),
    }
}

//endregion

/*┌─────────────────╮
| Status          |
└─────────────────╯*/
//region Status

/// Every format's choices and what handles it here now.
pub fn status(env: &Env, choices: &Choices) -> Vec<Status> {
    let scene = Scene::load(env);
    let apps = apps::apps(env);
    let (set, default) = scene.dolphin_plugins(env);
    let enabled = set.unwrap_or(default);
    let all_files = &scene.thumbnailers;
    FORMATS
        .iter()
        .map(|f| {
            let mime = f.main_mime();
            let mut notes = Vec::new();
            let thumbnails_by = if env.is_kde() || !env.gnome {
                scene.kde_thumbnailer(&enabled, mime)
            } else {
                apps::gnome_choice(all_files, mime).map(|t| if t.id == THUMBNAILER { "Slerp".into() } else { t.id.clone() })
            };
            let rivals = scene.kde_rivals(Some(&enabled), f);
            if choices.thumbnails(f) && !rivals.is_empty() && !choices.replace_kde_previews(f) {
                let ids: Vec<&str> = rivals.iter().map(|p| p.id.as_str()).collect();
                notes.push(format!(
                    "Dolphin uses KDE's {} for this type: KDE's own plugins win over thumbnailer files.  Replacing KDE's previews switches {} off in Dolphin for every type it handles.",
                    ids.join(" and "),
                    if ids.len() == 1 { "it" } else { "them" }
                ));
            }
            if choices.thumbnails(f) && !enabled.iter().any(|p| p == THUMBNAILER) && (env.is_kde() || !scene.kde.is_empty()) {
                notes.push("Slerp isn't enabled in Dolphin's preview settings; `slerp associate --apply` adds it.".into());
            }
            let handler = apps::default_app(env, &scene.db, &apps, mime);
            let opens_with = handler.as_ref().map(|h| match &h.as_parent {
                Some(p) => format!("{} (as {p})", h.name),
                None if h.app == DESKTOP_ID => "Slerp".into(),
                None => h.name.clone(),
            });
            Status {
                format: f,
                thumbnails: choices.thumbnails(f),
                open_by_default: choices.open_by_default(f),
                replace_kde_previews: choices.replace_kde_previews(f),
                kde_rivals: rivals.iter().map(|p| p.id.clone()).collect(),
                thumbnails_by,
                opens_with,
                notes,
            }
        })
        .collect()
}

//endregion

#[cfg(test)]
mod tests;
