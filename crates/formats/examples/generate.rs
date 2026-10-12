//! Writes the files generated from the format table into the source tree,
//! like `slerp formats --write` but without building the whole program.
fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    match slerp_formats::generate::write_all(&root) {
        Ok(paths) if paths.is_empty() => println!("all generated files are current"),
        Ok(paths) => paths.iter().for_each(|p| println!("wrote {p}")),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
