//! Times how long the GPU's shader compiler takes to turn each compute entry
//! point of a WGSL file into a pipeline, for finding shapes of code that one
//! backend's compiler handles badly (Direct3D's FXC unrolls some loops for
//! tens of seconds).  Usage: wgsl_probe <file.wgsl> <entry>...
fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: wgsl_probe <file.wgsl> <entry>...");
        std::process::exit(2);
    };
    let source = std::fs::read_to_string(&path).expect("read the WGSL file");
    let gpu = slerp_render::open_gpu(&slerp_render::GpuPolicy::Auto).expect("a GPU");
    println!("{} via {}", gpu.info.name, gpu.chosen_by);
    let device = &gpu.device;
    let module = device.create_shader_module(slerp_render::wgpu::ShaderModuleDescriptor {
        label: Some("probe"),
        source: slerp_render::wgpu::ShaderSource::Wgsl(source.into()),
    });
    for entry in args {
        let start = std::time::Instant::now();
        let _pipeline = device.create_compute_pipeline(&slerp_render::wgpu::ComputePipelineDescriptor {
            label: Some(&entry),
            layout: None,
            module: &module,
            entry_point: Some(&entry),
            compilation_options: slerp_render::wgpu::PipelineCompilationOptions {
                zero_initialize_workgroup_memory: std::env::var_os("PROBE_NO_ZERO_INIT").is_none(),
                ..Default::default()
            },
            cache: None,
        });
        println!("{entry}: {:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
    }
}
