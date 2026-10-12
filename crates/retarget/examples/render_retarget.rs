//! Renders retargeted clips at a few moments into contact sheets, to check
//! by eye.  Each sheet has one row per character and one column per moment.
//!
//! Usage: render_retarget [OUT_DIR]
//!
//! Writes, into OUT_DIR (default `local/artifacts`):
//! - `retarget-metahuman-walk.png`: the Universal Animation Library's walk
//!   on chalk's MetaHuman, retargeted by Slerp (top) and as chalk's own
//!   pipeline baked it (bottom).
//! - `retarget-vrm-walk.png`: the same walk on VRoid's AvatarSample_B
//!   (VRM 1.0, top) and AvatarSample_A (VRM 0.x, bottom).
//! - `retarget-vrma.png`: VRM animation files on the MetaHuman: `VRMA_02`
//!   (top) and `idle_loop` (bottom).
//!
//! Renders on the integrated GPU unless `SLERP_GPU` says otherwise.

use slerp_render::wgpu;
use slerp_render::{Background, GpuPolicy, RenderSettings, Renderer, RendererConfig, open_gpu};
use slerp_scene::{Aabb, Asset, Image8, Limits, OrbitCamera, Pose, Scene};
use std::path::{Path, PathBuf};

/// A path in a checkout of chalk (a game whose files some tests read) in the
/// folder beside this repository's; tests skip the files that aren't there.
macro_rules! chalk {
    ($path:literal) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../../chalk/", $path)
    };
}

const UAL1: &str =
    chalk!("tools/humans/vendor/ual/ual1/Universal Animation Library[Standard]/Unreal-Godot/UAL1_Standard.glb");
const METAHUMAN: &str = chalk!("html/demos/humans/tate-tee-lod3.glb");
const FRAME: (u32, u32) = (360, 540);

fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets")
}

fn load(path: &Path) -> Scene {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    match slerp_gltf::load(path, &bytes, &Limits::viewer()) {
        Ok(Asset::Scene(mut s)) => {
            // chalk's eye-occlusion and tear meshes need chalk's shaders; as
            // plain materials they are white shells over the eyes.
            for i in 0..s.nodes.len() {
                let mesh =
                    s.nodes[i].mesh.and_then(|m| s.meshes.get(m)).map(|m| m.name.to_lowercase()).unwrap_or_default();
                let name = s.nodes[i].name.to_lowercase();
                if [mesh, name].iter().any(|n| n.contains("eyeshell") || n.contains("eyeedge")) {
                    s.nodes[i].visible = false;
                }
            }
            s
        }
        Ok(_) => panic!("{} isn't a scene", path.display()),
        Err(e) => panic!("{}: {e}", path.display()),
    }
}

/// A character and the clip to show, by name.
struct Row {
    scene: Scene,
    clip: usize,
}

/// `target` with `library`'s clips added; shows the added clip that was
/// called `name` in the library.
fn retargeted(target: &Path, library: &Path, name: &str) -> Row {
    let (mut scene, library) = (load(target), load(library));
    let before = scene.animations.len();
    let added = slerp_retarget::add_animations(&mut scene, &library).expect("retarget");
    let clip = library.animations.iter().position(|a| a.name == name).expect("clip");
    println!("{}: {} clips added, showing {}", target.display(), added.len(), added[clip]);
    Row { clip: before + clip, scene }
}

fn baked(target: &Path, name: &str) -> Row {
    let scene = load(target);
    let clip = scene.animations.iter().position(|a| a.name == name).expect("clip");
    Row { scene, clip }
}

/// Renders each row at `fractions` of its clip and lays the frames out in
/// a grid.
fn sheet(renderer: &mut Renderer, gpu: &slerp_render::Gpu, rows: &[Row], fractions: &[f32]) -> Image8 {
    let (w, h) = FRAME;
    let mut out = Image8::new(w * fractions.len() as u32, h * rows.len() as u32);
    for (r, row) in rows.iter().enumerate() {
        let scene = &row.scene;
        let duration = scene.animations[row.clip].duration;
        let poses: Vec<Pose> = fractions
            .iter()
            .map(|f| {
                let mut pose = Pose::rest(scene);
                pose.sample(scene, row.clip, duration * f);
                pose.update_world(scene);
                pose
            })
            .collect();
        // One camera for the row, framing the skeleton at every moment
        // with room for the flesh around it.
        let joints: Vec<usize> = scene.skins.iter().flat_map(|s| s.joints.iter().copied()).collect();
        let mut bounds = Aabb::EMPTY;
        for pose in &poses {
            for &j in &joints {
                bounds.add(pose.world[j].w_axis.truncate());
            }
        }
        let pad = (bounds.max - bounds.min).max_element() * 0.08;
        bounds.min -= slerp_scene::glam::Vec3::splat(pad);
        bounds.max += slerp_scene::glam::Vec3::splat(pad);
        let mut cam = OrbitCamera::framing(&bounds, w as f32 / h as f32);
        cam.yaw = 55f32.to_radians();
        cam.pitch = 8f32.to_radians();
        cam.frame(&bounds, w as f32 / h as f32);
        // Framing fits a sphere; a standing figure is much narrower.
        cam.distance *= 0.7;
        let view = cam.view(w as f32 / h as f32);
        let mut gs = renderer.prepare(gpu, scene);
        let mut settings = RenderSettings::viewer();
        settings.background = Background::Color([0.18, 0.18, 0.19, 1.0]);
        for (c, pose) in poses.iter().enumerate() {
            renderer.update_pose(gpu, &mut gs, scene, pose);
            let image = renderer.render_image(gpu, &gs, &view, &settings, w, h);
            for y in 0..h {
                let from = (y * w * 4) as usize;
                let to = (((r as u32 * h + y) * out.width + c as u32 * w) * 4) as usize;
                out.pixels[to..to + (w * 4) as usize].copy_from_slice(&image.pixels[from..from + (w * 4) as usize]);
            }
        }
    }
    out
}

fn save(image: &Image8, path: &Path) {
    let file = std::fs::File::create(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), image.width, image.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&image.pixels).unwrap();
    println!("wrote {}", path.display());
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../local/artifacts")), PathBuf::from);
    std::fs::create_dir_all(&out).expect("output folder");
    let gpu = open_gpu(&GpuPolicy::Integrated).expect("no GPU");
    println!("rendering on {}", gpu.info.name);
    let mut renderer = Renderer::with_config(&gpu, wgpu::TextureFormat::Rgba8UnormSrgb, RendererConfig::viewer());
    let (ual, mh) = (Path::new(UAL1), Path::new(METAHUMAN));
    let walk = [0.0, 0.25, 0.5];

    let rows = [retargeted(mh, ual, "Walk_Loop"), baked(mh, "Walk_Loop")];
    save(&sheet(&mut renderer, &gpu, &rows, &walk), &out.join("retarget-metahuman-walk.png"));

    let vrm = assets().join("vrm");
    let rows = [
        retargeted(&vrm.join("AvatarSample_B.vrm"), ual, "Walk_Loop"),
        retargeted(&vrm.join("AvatarSample_A.vrm"), ual, "Walk_Loop"),
    ];
    save(&sheet(&mut renderer, &gpu, &rows, &walk), &out.join("retarget-vrm-walk.png"));

    let anim = vrm.join("anim");
    let rows = [
        retargeted(mh, &anim.join("VRMA_02.vrma"), "animation"),
        retargeted(mh, &anim.join("idle_loop.vrma"), "animation"),
    ];
    save(&sheet(&mut renderer, &gpu, &rows, &[0.2, 0.5, 0.8]), &out.join("retarget-vrma.png"));
}
