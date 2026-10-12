//! `slerp associate` (which formats get Slerp's thumbnails and open in
//! Slerp by default) and `slerp formats` (the format table and the files
//! generated from it).

use slerp_associate::{Context, Report, Status};
use slerp_core::formats::{self, Category, FORMATS, Format};
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct AssociateArgs {
    /// Lists every format: its category and extensions, the choices, and
    /// what makes its thumbnails and opens it on this system now.
    #[arg(long)]
    list: bool,
    /// Switches Slerp's thumbnails on or off for formats, categories
    /// (`model`, `texture`, `retro`, ...) or `all`, then applies the choice.
    #[arg(long, num_args = 2.., value_names = ["on|off", "FORMAT"])]
    thumbnails: Option<Vec<String>>,
    /// Makes Slerp the default app for formats, categories or `all` (on),
    /// or stops (off), then applies the choice.
    #[arg(long, num_args = 2.., value_names = ["on|off", "FORMAT"])]
    default: Option<Vec<String>>,
    /// KDE only: switches off, in Dolphin, the KDE thumbnail plugin that
    /// wins over Slerp for these formats (on), or switches it back (off).
    /// That plugin then previews none of its other types either.
    #[arg(long, num_args = 2.., value_names = ["on|off", "FORMAT"])]
    replace_kde: Option<Vec<String>>,
    /// Applies the saved choices again (after an update).
    #[arg(long)]
    apply: bool,
    /// Undoes everything the manager changed outside Slerp, other apps'
    /// defaults included.  The choices are kept; --apply brings them back.
    #[arg(long)]
    reset: bool,
    /// Shows what would change, and changes nothing.
    #[arg(long)]
    dry_run: bool,
    /// With --list: one tab-separated line per format, for scripts.
    #[arg(long)]
    tsv: bool,
}

pub fn associate(args: AssociateArgs) -> Result<(), String> {
    let ctx = Context::system()?;
    if args.reset {
        let report = slerp_associate::reset(&ctx, args.dry_run)?;
        print_report(&report, args.dry_run);
        return Ok(());
    }
    let mut choices = ctx.load_choices();
    let mut changed = false;
    let mut default_on = false;
    if let Some(v) = &args.thumbnails {
        let (on, list) = switch(v)?;
        for f in list {
            choices.set_thumbnails(f, on);
        }
        changed = true;
    }
    if let Some(v) = &args.default {
        let (on, list) = switch(v)?;
        for f in list {
            choices.set_open_by_default(f, on);
        }
        default_on = on;
        changed = true;
    }
    if let Some(v) = &args.replace_kde {
        let (on, list) = switch(v)?;
        for f in list {
            choices.set_replace_kde_previews(f, on);
        }
        changed = true;
    }
    if changed && !args.dry_run {
        choices.save_to(&ctx.choices_path).map_err(|e| format!("{}: {e}", ctx.choices_path.display()))?;
    }
    if changed || args.apply {
        let report = slerp_associate::apply(&ctx, &mut choices, args.dry_run)?;
        print_report(&report, args.dry_run);
        #[cfg(windows)]
        if default_on && !args.dry_run {
            slerp_associate::windows::open_default_apps_settings();
        }
        let _ = default_on;
        return Ok(());
    }
    let statuses = slerp_associate::status(&ctx, &choices);
    if args.tsv {
        print_tsv(&statuses);
    } else {
        print_list(&statuses);
    }
    Ok(())
}

/// `on|off` and the formats it applies to.
fn switch(args: &[String]) -> Result<(bool, Vec<&'static Format>), String> {
    let on = match args[0].to_ascii_lowercase().as_str() {
        "on" | "yes" | "true" => true,
        "off" | "no" | "false" => false,
        other => return Err(format!("expected on or off, not {other:?}")),
    };
    let mut out: Vec<&'static Format> = Vec::new();
    for name in &args[1..] {
        let found = resolve(name)
            .ok_or_else(|| format!("no format or category {name:?} (`slerp associate --list` shows them)"))?;
        for f in found {
            if !out.iter().any(|o| o.id == f.id) {
                out.push(f);
            }
        }
    }
    Ok((on, out))
}

/// A format id, category id, `all`, an extension (`.glb`) or a MIME type.
fn resolve(name: &str) -> Option<Vec<&'static Format>> {
    let lower = name.to_ascii_lowercase();
    if lower == "all" {
        return Some(FORMATS.iter().collect());
    }
    if let Some(f) = formats::by_id(&lower) {
        return Some(vec![f]);
    }
    if let Some(c) = Category::from_id(&lower) {
        return Some(formats::in_category(c).collect());
    }
    if let Some(f) = formats::by_mime(&lower) {
        return Some(vec![f]);
    }
    let by_ext = formats::by_file_name(&format!("x.{}", lower.trim_start_matches('.')));
    (!by_ext.is_empty()).then_some(by_ext)
}

fn print_report(report: &Report, dry_run: bool) {
    let verb = if dry_run { "would change" } else { "changed" };
    if report.changes.is_empty() {
        println!("Nothing to change.");
    } else {
        println!("{} {verb}:", if dry_run { "This" } else { "Slerp" });
        for c in &report.changes {
            println!("  {c}");
        }
    }
    for n in &report.notes {
        println!("Note: {n}");
    }
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

/// The extensions of a format, cut short for the table.
fn short_extensions(f: &Format) -> String {
    let all: Vec<String> = f
        .extensions()
        .into_iter()
        .chain(f.by_content.iter().copied())
        .map(|e| format!(".{e}"))
        .chain(f.mime.iter().flat_map(|m| m.patterns.iter().map(|p| p.to_string())))
        .collect();
    if all.len() > 4 { format!("{} +{}", all[..3].join(" "), all.len() - 3) } else { all.join(" ") }
}

fn print_list(statuses: &[Status]) {
    let rows: Vec<[String; 6]> = statuses
        .iter()
        .map(|s| {
            [
                s.format.id.to_owned(),
                short_extensions(s.format),
                if s.format.can_thumbnail() { on_off(s.thumbnails).to_owned() } else { "-".to_owned() },
                if s.open_by_default { "on".to_owned() } else { "off".to_owned() },
                s.thumbnails_by.clone().unwrap_or_else(|| "-".into()),
                s.opens_with.clone().unwrap_or_else(|| "-".into()),
            ]
        })
        .collect();
    let headers = ["FORMAT", "EXTENSIONS", "THUMBS", "DEFAULT", "THUMBNAILS BY", "OPENS WITH"];
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for r in &rows {
        for (w, cell) in widths.iter_mut().zip(r) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cells: &[&str]| {
        let mut out = String::new();
        for (i, (cell, w)) in cells.iter().zip(&widths).enumerate() {
            if i + 1 == cells.len() {
                out.push_str(cell);
            } else {
                out.push_str(&format!("{cell:<w$}  "));
            }
        }
        println!("{}", out.trim_end());
    };
    for category in Category::ALL {
        let in_cat: Vec<usize> = (0..statuses.len()).filter(|&i| statuses[i].format.category == category).collect();
        if in_cat.is_empty() {
            continue;
        }
        println!("\n{} ({})", category.name(), category.id());
        line(&headers);
        for i in in_cat {
            let r: Vec<&str> = rows[i].iter().map(String::as_str).collect();
            line(&r);
            if statuses[i].replace_kde_previews {
                println!("    KDE's previews replaced: {}", statuses[i].kde_rivals.join(", "));
            }
            for n in &statuses[i].notes {
                println!("    {n}");
            }
        }
    }
    println!();
    for l in slerp_associate::limits() {
        println!("Note: {l}");
    }
    println!("Change with: slerp associate --thumbnails on|off FORMAT...  or  --default on|off FORMAT...");
}

fn print_tsv(statuses: &[Status]) {
    println!("format\tcategory\textensions\tthumbnails\tdefault\tthumbnails_by\topens_with\tmime");
    for s in statuses {
        let f = s.format;
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            f.id,
            f.category.id(),
            f.extensions().join(","),
            if f.can_thumbnail() { on_off(s.thumbnails) } else { "never" },
            on_off(s.open_by_default),
            s.thumbnails_by.as_deref().unwrap_or(""),
            s.opens_with.as_deref().unwrap_or(""),
            f.listed_mime().collect::<Vec<_>>().join(",")
        );
    }
}

/*┌─────────────────╮
| slerp formats   |
└─────────────────╯*/
//region slerp formats

#[derive(clap::Args)]
pub struct FormatsArgs {
    /// Writes the files generated from the table (the thumbnailer and
    /// desktop entry, the macOS Info.plists, the Qt plugin's metadata) into
    /// the source tree at this folder (default: the current one).
    #[arg(long, value_name = "ROOT", num_args = 0..=1, default_missing_value = ".")]
    write: Option<PathBuf>,
    /// Fails when a generated file in the source tree is out of date.
    #[arg(long, value_name = "ROOT", num_args = 0..=1, default_missing_value = ".")]
    check: Option<PathBuf>,
}

pub fn formats(args: FormatsArgs) -> Result<(), String> {
    let root_ok = |root: &Path| {
        if root.join("data/slerp.xml").is_file() {
            Ok(())
        } else {
            Err(format!("{} isn't Slerp's source tree (no data/slerp.xml)", root.display()))
        }
    };
    if let Some(root) = &args.write {
        root_ok(root)?;
        let written = formats::generate::write_all(root).map_err(|e| e.to_string())?;
        if written.is_empty() {
            println!("All generated files are current.");
        }
        for p in written {
            println!("wrote {p}");
        }
        return Ok(());
    }
    if let Some(root) = &args.check {
        root_ok(root)?;
        let stale: Vec<&str> = formats::generate::all()
            .into_iter()
            .filter(|f| std::fs::read_to_string(root.join(f.path)).ok().as_deref() != Some(f.text.as_str()))
            .map(|f| f.path)
            .collect();
        return if stale.is_empty() {
            Ok(())
        } else {
            Err(format!("out of date, run `slerp formats --write`: {}", stale.join(", ")))
        };
    }
    println!("id\tcategory\tthumbnails\topen_with\tqt\tmime\textensions");
    for f in FORMATS {
        println!(
            "{}\t{}\t{:?}\t{}\t{:?}\t{}\t{}",
            f.id,
            f.category.id(),
            f.thumbnails,
            f.open_with,
            f.qt,
            f.mime_names().collect::<Vec<_>>().join(","),
            f.extensions().join(",")
        );
    }
    Ok(())
}

//endregion
