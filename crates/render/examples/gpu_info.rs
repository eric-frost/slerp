//! Prints which GPU `open_gpu` picks for a policy and how long it took.
//! Usage: gpu_info [auto|integrated|discrete|software|<name>]
fn main() {
    let t = std::time::Instant::now();
    let policy = slerp_render::GpuPolicy::parse(&std::env::args().nth(1).unwrap_or_default());
    match slerp_render::open_gpu(&policy) {
        Ok(gpu) => println!(
            "{:?}: {} ({:?}, {}) via {} in {:.1} ms, pipeline cache {}",
            policy,
            gpu.info.name,
            gpu.info.device_type,
            gpu.info.driver,
            gpu.chosen_by,
            t.elapsed().as_secs_f64() * 1000.0,
            gpu.pipeline_cache.is_some()
        ),
        Err(e) => println!("{policy:?}: {e}"),
    }
}
