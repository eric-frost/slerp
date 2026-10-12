//! Draws a texture file the way the viewer's texture mode does, to PNG.
//!
//! Usage: texview_png [--gpu POLICY] [--size N] [--mip N] [--layer N]
//!                    [--face N] [--channels rgba] [--exposure STOPS]
//!                    [--zoom PX_PER_TEXEL] [--center X,Y] [--as-env]
//!                    TEXTURE OUT.png
//! With `--as-env`, the texture lights the material spheres instead, as
//! when the viewer uses an HDR or cubemap as lighting.

use slerp_render::wgpu;
use slerp_render::{Background, GpuPolicy, RenderSettings, Renderer, TextureDrawParams, open_gpu};
use slerp_scene::textures::{DecodeOptions, decode_file};
use slerp_scene::{OrbitCamera, Shape};
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut policy = GpuPolicy::Auto;
    let mut size = 512u32;
    let (mut mip, mut layer, mut face) = (None, 0, 0);
    let mut channels = [true; 4];
    let mut exposure = 0.0;
    let mut zoom = None;
    let mut center = None;
    let mut as_env = false;
    let mut positional = Vec::new();
    while let Some(a) = args.next() {
        let num = |args: &mut std::iter::Skip<std::env::Args>| args.next().and_then(|s| s.parse::<f32>().ok());
        match a.as_str() {
            "--gpu" => policy = GpuPolicy::parse(&args.next().unwrap_or_default()),
            "--size" => size = num(&mut args).unwrap_or(512.0) as u32,
            "--mip" => mip = num(&mut args).map(|v| v as u32),
            "--layer" => layer = num(&mut args).unwrap_or(0.0) as u32,
            "--face" => face = num(&mut args).unwrap_or(0.0) as u32,
            "--exposure" => exposure = num(&mut args).unwrap_or(0.0),
            "--zoom" => zoom = num(&mut args),
            "--center" => {
                center = args.next().and_then(|s| {
                    let (x, y) = s.split_once(',')?;
                    Some([x.parse().ok()?, y.parse().ok()?])
                })
            }
            "--channels" => {
                let c = args.next().unwrap_or_default();
                channels = ['r', 'g', 'b', 'a'].map(|ch| c.contains(ch));
            }
            "--as-env" => as_env = true,
            _ => positional.push(a),
        }
    }
    let [input, out] = positional.as_slice() else {
        eprintln!("usage: texview_png [options] TEXTURE OUT.png");
        std::process::exit(2);
    };
    let t0 = Instant::now();
    let tex = decode_file(std::path::Path::new(input), &DecodeOptions::full()).expect("decode");
    let t_decode = t0.elapsed();
    let gpu = open_gpu(&policy).expect("no GPU");
    let mut renderer = Renderer::new(&gpu, wgpu::TextureFormat::Rgba8UnormSrgb);
    let image = if as_env {
        renderer.set_environment(&gpu, &tex);
        let scene = slerp_scene::Scene::material_ball(
            &slerp_scene::MaterialAsset {
                name: "ball".into(),
                material: slerp_scene::Material { metallic: 1.0, roughness: 0.2, ..Default::default() },
                textures: vec![],
                preview: None,
                info: Default::default(),
            },
            Shape::Sphere,
            64,
        );
        let gs = renderer.prepare(&gpu, &scene);
        let view = OrbitCamera::framing(&gs.bounds, 1.0).view(1.0);
        let settings = RenderSettings { background: Background::Environment { blur: 0.0 }, ..RenderSettings::viewer() };
        renderer.render_image(&gpu, &gs, &view, &settings, size, size)
    } else {
        let gt = renderer.upload_texture(&gpu, &tex).expect("upload");
        let mut params = TextureDrawParams::fit(&gt, (size, size));
        params.mip = mip;
        params.layer = layer;
        params.face = face;
        params.channels = channels;
        params.exposure = exposure;
        if let Some(z) = zoom {
            params.zoom = z;
        }
        if let Some(c) = center {
            params.center = c;
        }
        println!(
            "{}x{}, {} layers, {} mips ({} in the file), hdr {}",
            gt.width(),
            gt.height(),
            gt.layers(),
            gt.mips(),
            gt.file_mips(),
            gt.is_hdr()
        );
        renderer.draw_texture_image(&gpu, &gt, &params, size, size)
    };
    let file = std::fs::File::create(out).expect("create output");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), image.width, image.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&image.pixels).unwrap();
    println!("decode {:.1} ms, total {:.1} ms", t_decode.as_secs_f64() * 1000.0, t0.elapsed().as_secs_f64() * 1000.0);
}
