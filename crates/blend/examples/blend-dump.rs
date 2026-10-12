//! Prints a `.blend` file's structure: `blend-dump FILE [summary | struct
//! NAME | ids XX | id NAME [DEPTH]]`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: blend-dump FILE [summary | struct NAME | ids XX | id NAME [DEPTH]]");
        std::process::exit(2);
    };
    let bytes = std::fs::read(path).expect("read");
    match slerp_blend::dump(&bytes, &args[1..].join(" ")) {
        Ok(s) => print!("{s}"),
        Err(e) => eprintln!("{path}: {e}"),
    }
}
