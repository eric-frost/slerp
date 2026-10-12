//! Prints what Slerp reads from layered documents: format facts, warnings
//! and the layer list.  `cargo run -p slerp-paint --example paint_layers -- *.psd`
use slerp_paint::{load, sniff};
use slerp_scene::{Asset, Limits};
use std::path::Path;

fn main() {
    for arg in std::env::args().skip(1) {
        let path = Path::new(&arg);
        let head = std::fs::read(path).map(|b| b[..b.len().min(4096)].to_vec()).unwrap_or_default();
        let Some(format) = sniff(path, &head) else {
            println!("{arg}: not recognised");
            continue;
        };
        println!("== {arg}");
        match load(format, path, &Limits::viewer()) {
            Ok(Asset::Canvas(c)) => {
                let i = c.info();
                println!("{} {:?}", i.format, c.size());
                for (k, v) in &i.details {
                    println!("  {k}: {v}");
                }
                for w in &i.warnings {
                    println!("  warning: {w}");
                }
                for l in c.layers() {
                    let indent = "  ".repeat(l.depth as usize + 1);
                    println!(
                        "{indent}{}{} [{}]{}",
                        if l.group { "+ " } else { "" },
                        l.name,
                        l.note,
                        if l.visible { "" } else { " (hidden)" }
                    );
                }
            }
            Ok(Asset::Image(t)) => {
                println!("image {} {} {}x{}", t.source.container, t.source.format, t.width(), t.height());
                for (k, v) in &t.source.header {
                    println!("  {k}: {v}");
                }
            }
            Ok(_) => println!("other asset"),
            Err(e) => println!("error: {e}"),
        }
    }
}
