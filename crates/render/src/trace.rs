//! Optional timing of the renderer's steps, printed to stderr when
//! `SLERP_TRACE` is set.  Each step waits for the GPU to finish, so
//! tracing slows things down a little but shows where the time goes.

use std::sync::OnceLock;
use std::time::Instant;

pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SLERP_TRACE").is_some_and(|v| !v.is_empty() && v != "0"))
}

/// Times one step; `done` waits for the GPU when tracing.
pub(crate) struct Step {
    name: &'static str,
    detail: &'static str,
    start: Option<Instant>,
}

impl Step {
    pub fn new(name: &'static str) -> Step {
        Step::with(name, "")
    }

    /// A step named by two parts, such as "compile" and a shader's entry
    /// point.
    pub fn with(name: &'static str, detail: &'static str) -> Step {
        Step { name, detail, start: enabled().then(Instant::now) }
    }

    pub fn done(self, device: &wgpu::Device) {
        if let Some(start) = self.start {
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            let space = if self.detail.is_empty() { "" } else { " " };
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            eprintln!("slerp-render: {}{space}{} {ms:.1} ms", self.name, self.detail);
        }
    }
}

/// Prints how long after `start` a step finished, when tracing.  For steps
/// before there is a device to wait for.
pub(crate) fn note(name: &str, start: Instant) {
    if enabled() {
        eprintln!("slerp-render: {name} ready at {:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
    }
}
