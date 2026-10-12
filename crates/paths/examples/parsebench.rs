//! Times the stages of a G-code thumbnail: reading the toolpath, building
//! the mesh.  `parsebench <file>`.
use std::time::Instant;
fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("a file"));
    let t = Instant::now();
    let tp =
        slerp_paths::read_toolpath(slerp_paths::PathFormat::Gcode, &path, slerp_paths::gcode::Keep::Drawn, 20_000_000)
            .unwrap();
    let parse = t.elapsed();
    if std::env::var_os("PARSE_ONLY").is_some() {
        println!("points {} parse {:.0} ms", tp.points.len(), parse.as_secs_f64() * 1e3);
        return;
    }
    let t = Instant::now();
    let look = slerp_paths::ToolpathLook::outside(&tp);
    let scene = slerp_paths::toolpath_scene(&tp, &look, slerp_paths::Target::Thumbnail(256));
    let mesh = t.elapsed();
    let t = Instant::now();
    let viewer = slerp_paths::toolpath_scene(&tp, &slerp_paths::ToolpathLook::all(), slerp_paths::Target::Viewer);
    let vmesh = t.elapsed();
    println!(
        "points {} parse {:.0} ms, thumbnail mesh {:.0} ms ({} tris), viewer mesh {:.0} ms ({} tris)",
        tp.points.len(),
        parse.as_secs_f64() * 1e3,
        mesh.as_secs_f64() * 1e3,
        scene.triangle_count(),
        vmesh.as_secs_f64() * 1e3,
        viewer.triangle_count()
    );
}
