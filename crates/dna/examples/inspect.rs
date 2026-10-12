//! Prints what a DNA file holds and how long it takes to load:
//! `cargo run -p slerp-dna --example inspect <file.dna> [viewer|thumb SIZE]`.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("usage: inspect <file.dna> [viewer | thumb SIZE]");
        std::process::exit(2);
    };
    let opts = match args.get(2).map(String::as_str) {
        Some("thumb") => slerp_dna::Options::thumbnail(args.get(3).and_then(|s| s.parse().ok()).unwrap_or(256)),
        _ => slerp_dna::Options::viewer(),
    };
    let start = std::time::Instant::now();
    let c = match slerp_dna::open(std::path::Path::new(path), &opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    println!("loaded in {:.0} ms: {:?}", start.elapsed().as_secs_f64() * 1e3, c.timings);
    let s = &c.scene;
    println!(
        "{} ({}), {} nodes, {} meshes, {} triangles, {} vertices",
        s.name,
        s.info.format,
        s.nodes.len(),
        s.meshes.len(),
        s.triangle_count(),
        s.vertex_count()
    );
    for (k, v) in &s.info.details {
        println!("  {k}: {v}");
    }
    for w in &s.info.warnings {
        println!("  warning: {w}");
    }
    for m in &s.meshes {
        let targets = m.primitives.iter().map(|p| p.targets.len()).max().unwrap_or(0);
        let stored: usize = m.primitives.iter().flat_map(|p| &p.targets).map(|t| t.positions.len()).sum();
        let bends: usize = m.primitives.iter().flat_map(|p| &p.targets).map(|t| t.normals.len()).sum();
        println!(
            "  mesh {}: {} primitives, {} targets, {} stored offsets and {} normal changes ({:.1} MB)",
            m.name,
            m.primitives.len(),
            targets,
            stored,
            bends,
            (stored + bends) as f64 * 12.0 / 1e6
        );
    }
    if let Some(r) = &c.head_rig {
        println!("head rig: {} controls, {} lods", r.controls.len(), r.lod_count());
    }
    if let Some(r) = &c.body_rig {
        println!("body rig: {} controls, {} lods", r.controls.len(), r.lod_count());
    }
}
