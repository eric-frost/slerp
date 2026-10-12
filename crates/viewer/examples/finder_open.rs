//! Checks on a Mac that files Finder opens reach the viewer, without a
//! window: the viewer's `macos` module under a background-only event loop,
//! which prints each file it is asked to open and quits after 20 seconds.
//! Bundle the binary as an app (any `Info.plist` with `LSBackgroundOnly`
//! set) and send it files with `open -g -a <app> <file>`, at launch and
//! while it runs.

#[cfg(target_os = "macos")]
#[path = "../src/macos.rs"]
#[allow(dead_code)]
mod macos;

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("finder_open is for macOS");
}

#[cfg(target_os = "macos")]
fn main() {
    use std::time::{Duration, Instant};
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
    use winit::window::WindowId;

    struct Listener {
        until: Instant,
    }

    impl ApplicationHandler for Listener {
        fn resumed(&mut self, _: &ActiveEventLoop) {
            println!("launched");
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            if let Some(path) = macos::take_pending() {
                println!("asked to open {}", path.display());
            }
            if Instant::now() >= self.until {
                event_loop.exit();
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(100)));
        }
    }

    macos::listen();
    let event_loop = EventLoop::builder()
        .with_activation_policy(ActivationPolicy::Prohibited)
        .with_activate_ignoring_other_apps(false)
        .with_default_menu(false)
        .build()
        .expect("event loop");
    let mut listener = Listener { until: Instant::now() + Duration::from_secs(20) };
    event_loop.run_app(&mut listener).expect("event loop");
}
