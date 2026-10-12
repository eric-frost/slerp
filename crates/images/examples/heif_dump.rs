//! Lists the items, properties and references of an AVIF or HEIF file:
//! `heif_dump <file>`.

fn main() {
    let path = std::env::args().nth(1).expect("usage: heif_dump <file>");
    let bytes = std::fs::read(path).unwrap();
    match slerp_images::HeifForTest::parse(&bytes) {
        Ok(h) => print!("{}", h.describe()),
        Err(e) => eprintln!("{e}"),
    }
}
