//! Prints a DICOM file's elements as the viewer's inspector lists them,
//! sequences indented, plus the reader's warnings.  Person names and other
//! identifying values are shown as `<hidden>` unless `--all` is given.
//!
//! `cargo run -p slerp-dicom --example dicom-dump -- [--all] <file>`

use slerp_dicom::text::Row;

fn print(rows: &[Row], depth: usize, all: bool) {
    for r in rows {
        let value = if r.identifying && !all { "<hidden>".to_string() } else { r.value.clone() };
        println!("{}{} {} {:<40} {}", "  ".repeat(depth), r.tag_text, r.vr, r.name, value);
        for (i, item) in r.items.iter().enumerate().take(50) {
            println!("{}  Item {}", "  ".repeat(depth), i + 1);
            print(item, depth + 2, all);
        }
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let all = args.iter().any(|a| a == "--all");
    args.retain(|a| a != "--all");
    for path in args {
        match slerp_dicom::Dicom::open(std::path::Path::new(&path)) {
            Ok(d) => {
                println!("== {path}: {} ({} frames)", d.parsed.transfer.name(), d.frame_count());
                print(&slerp_dicom::text::rows(d.meta()), 0, all);
                print(&slerp_dicom::text::rows(d.data()), 0, all);
                for w in &d.parsed.warnings {
                    println!("warning: {w}");
                }
                if let Some(e) = &d.image_error {
                    println!("image: {e}");
                }
            }
            Err(e) => println!("== {path}: {e}"),
        }
    }
}
