//! Writes `docs/formats.md` and the website's `site/formats.json` from
//! `slerp_formats::FORMATS`, so the lists readers see are the one the
//! thumbnailer, the desktop entry and the file association manager are
//! generated from.  `--check` fails instead when a file is out of date.

use slerp_formats::{Category, FORMATS, Format, Thumbs};
use std::fmt::Write;
use std::path::Path;

fn main() {
    let check = std::env::args().any(|a| a == "--check");
    let mut stale = false;
    for (name, text) in [("docs/formats.md", markdown()), ("site/formats.json", json())] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(name);
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if current == text {
            println!("{name} is up to date");
        } else if check {
            eprintln!("{name} is out of date: cargo run --manifest-path tools/format-list/Cargo.toml");
            stale = true;
        } else {
            std::fs::write(&path, text).unwrap_or_else(|e| panic!("can't write {name}: {e}"));
            println!("wrote {name}");
        }
    }
    if stale {
        std::process::exit(1);
    }
}

/// The whole page: an introduction, then a table per category in the order
/// the settings show them.
fn markdown() -> String {
    let mut out = String::new();
    let thumbnails = FORMATS.iter().filter(|f| f.thumbnails == Thumbs::On).count();
    out.push_str("# Formats\n\n");
    let _ = writeln!(
        out,
        "Slerp reads the {} formats below, in {} categories.  The list is generated from the format table \
         (`crates/formats/src/table.rs`) by `tools/format-list`; `slerp formats` prints the same table, and \
         `slerp associate --list` shows what handles each format on this computer.\n",
        FORMATS.len(),
        Category::ALL.len()
    );
    let _ = writeln!(
        out,
        "\"Thumbnails\" says whether Slerp makes file-manager thumbnails of the format unless the user changes it \
         ({thumbnails} formats).  They are off for ordinary pictures and documents the desktops already preview; \
         `slerp associate --thumbnails on <format>` or the viewer's Settings, File Types switches them on.  \
         Extensions in italics are only confirmed by looking inside the file, because unrelated files share \
         them.\n"
    );
    for category in Category::ALL {
        let formats: Vec<&Format> = slerp_formats::in_category(category).collect();
        if formats.is_empty() {
            continue;
        }
        let _ = writeln!(out, "## {}\n", category.name());
        out.push_str("| Format | Files | Thumbnails |\n|---|---|---|\n");
        for f in formats {
            let _ = writeln!(out, "| {} | {} | {} |", escape(f.name), files(f), thumbs(f.thumbnails));
        }
        out.push('\n');
    }
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

/// The same list for the website's format search: the categories in order,
/// each with its formats' names, the extensions and name patterns they
/// claim, those confirmed by content, and whether thumbnails are on.
fn json() -> String {
    let mut out = String::from("{\"categories\": [\n");
    let mut first_category = true;
    for category in Category::ALL {
        let formats: Vec<&Format> = slerp_formats::in_category(category).collect();
        if formats.is_empty() {
            continue;
        }
        if !first_category {
            out.push_str(",\n");
        }
        first_category = false;
        let _ = writeln!(out, "  {{\"name\": {}, \"formats\": [", quote(category.name()));
        let rows: Vec<String> = formats
            .iter()
            .map(|f| {
                let mut files: Vec<String> = f.extensions().iter().map(|e| quote(&format!(".{e}"))).collect();
                for m in f.mime {
                    files.extend(m.patterns.iter().map(|p| quote(p)));
                }
                let by_content: Vec<String> = f.by_content.iter().map(|e| quote(&format!(".{e}"))).collect();
                format!(
                    "    {{\"name\": {}, \"files\": [{}], \"byContent\": [{}], \"thumbnails\": {}}}",
                    quote(f.name),
                    files.join(", "),
                    by_content.join(", "),
                    quote(thumbs(f.thumbnails))
                )
            })
            .collect();
        out.push_str(&rows.join(",\n"));
        out.push_str("\n  ]}");
    }
    out.push_str("\n]}\n");
    out
}

/// A JSON string literal.
fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The extensions and other name patterns a format claims, then those it
/// confirms by content.
fn files(f: &Format) -> String {
    let mut parts: Vec<String> = f.extensions().iter().map(|e| format!("`.{e}`")).collect();
    for m in f.mime {
        parts.extend(m.patterns.iter().map(|p| format!("`{p}`")));
    }
    parts.extend(f.by_content.iter().map(|e| format!("*`.{e}`*")));
    if parts.is_empty() { "by content".to_string() } else { parts.join(" ") }
}

fn thumbs(t: Thumbs) -> &'static str {
    match t {
        Thumbs::On => "on",
        Thumbs::Off => "off",
        Thumbs::Never => "none",
    }
}

fn escape(text: &str) -> String {
    text.replace('|', "\\|")
}
