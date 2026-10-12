//! Renders a DNA file: its thumbnail exactly as `slerp thumbnail` makes it,
//! or the character posed by RigLogic.
//!
//! ```text
//! render <file.dna> <out.png> [--size N] [--thumbnail] [--lod N]
//!        [--arkit name=w,...] [--control name=w,...] [--yaw D] [--pitch D]
//!        [--distance M] [--target x,y,z]
//! ```

use slerp_core::{OrbitState, ThumbRenderer, ViewState};
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: render <file.dna> <out.png> [options]");
        std::process::exit(2);
    }
    let (input, output) = (Path::new(&args[1]), Path::new(&args[2]));
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let size: u32 = flag("--size").and_then(|s| s.parse().ok()).unwrap_or(512);
    let config = slerp_core::Config::load();
    let mut renderer = ThumbRenderer::new(&config.gpu);
    let start = std::time::Instant::now();

    // 1. The thumbnail, through core as the helper makes it.
    if args.iter().any(|a| a == "--thumbnail") {
        let image = match slerp_core::prepare_thumbnail(input, size).unwrap() {
            slerp_core::Prepared::Done(i) => i,
            slerp_core::Prepared::Render(scene) => renderer.render_thumbnail(&scene, size).unwrap(),
        };
        std::fs::write(output, slerp_core::encode_png(&image)).unwrap();
        println!("thumbnail in {:.0} ms", start.elapsed().as_secs_f64() * 1e3);
        return;
    }

    // 2. Any other file (chalk's baked GLBs, for comparison) through core,
    //    with morph weights by name.
    let orbit_flags = |orbit: &mut OrbitState| {
        if let Some(v) = flag("--yaw").and_then(|s| s.parse().ok()) {
            orbit.yaw = v;
        }
        if let Some(v) = flag("--pitch").and_then(|s| s.parse().ok()) {
            orbit.pitch = v;
        }
        orbit.distance = flag("--distance").and_then(|s| s.parse().ok());
        orbit.target = flag("--target").map(|t| {
            let v: Vec<f32> = t.split(',').filter_map(|x| x.parse().ok()).collect();
            [v[0], v[1], v[2]]
        });
    };
    if input.extension().is_some_and(|e| e != "dna") {
        let mut orbit = OrbitState { yaw: 20.0, pitch: 5.0, ..Default::default() };
        orbit_flags(&mut orbit);
        let mut state = ViewState { orbit: Some(orbit), debug: flag("--debug"), ..Default::default() };
        state.hidden = flag("--hide").map(|h| h.split(',').map(str::to_string).collect()).unwrap_or_default();
        for (name, w) in flag("--morph")
            .map(|l| {
                l.split(',')
                    .filter_map(|p| p.split_once('='))
                    .map(|(n, w)| (n.to_string(), w.parse().unwrap_or(1.0)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
        {
            state.morphs.insert(name, w);
        }
        let image = renderer.render_file(input, size, &state).unwrap();
        std::fs::write(output, slerp_core::encode_png(&image)).unwrap();
        return;
    }

    // 3. Posed: the rig evaluated with the controls given, its pose baked
    //    into the scene's rest pose so the offscreen renderer draws it.
    let mut opts = slerp_dna::Options::viewer();
    opts.lod = flag("--lod").and_then(|s| s.parse().ok()).unwrap_or(0);
    opts.textures = !args.iter().any(|a| a == "--clay");
    opts.body = !args.iter().any(|a| a == "--head");
    let mut c = slerp_dna::open(input, &opts).unwrap();
    println!("opened in {:.0} ms: {:?}", start.elapsed().as_secs_f64() * 1e3, c.timings);
    let pairs = |list: Option<String>| -> Vec<(String, f32)> {
        list.map(|l| {
            l.split(',')
                .filter_map(|p| p.split_once('='))
                .map(|(n, w)| (n.to_string(), w.parse().unwrap_or(1.0)))
                .collect()
        })
        .unwrap_or_default()
    };
    if let Some(rig) = &mut c.head_rig {
        for (name, w) in pairs(flag("--arkit")) {
            for control in slerp_dna::arkit_controls(&name).unwrap_or(&[]) {
                if let Some(i) = rig.control(control) {
                    rig.values[i] = w;
                }
            }
        }
        for (name, w) in pairs(flag("--control")) {
            match rig.control(&name) {
                Some(i) => rig.values[i] = w,
                None => eprintln!("no control {name}"),
            }
        }
    }
    let mut pose = slerp_core::scene::Pose::rest(&c.scene);
    let t = std::time::Instant::now();
    c.apply(&mut pose).unwrap();
    println!("rig evaluated in {:.2} ms", t.elapsed().as_secs_f64() * 1e3);
    let mut scene = std::mem::take(&mut c.scene);
    for (i, node) in scene.nodes.iter_mut().enumerate() {
        node.transform = pose.local[i];
        node.weights = pose.weights[i].clone();
    }
    let mut orbit = OrbitState { yaw: 20.0, pitch: 5.0, ..Default::default() };
    orbit_flags(&mut orbit);
    let state = ViewState {
        orbit: Some(orbit),
        background: flag("--background"),
        debug: flag("--debug"),
        ..Default::default()
    };
    let image = renderer.render_state(&scene, &state, size, size).unwrap();
    std::fs::write(output, slerp_core::encode_png(&image)).unwrap();
    println!("rendered in {:.0} ms in all", start.elapsed().as_secs_f64() * 1e3);
}
