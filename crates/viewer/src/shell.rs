//! The viewer's window: winit's window with egui's input (`egui_winit`)
//! and painter (`egui_wgpu`), the parts eframe is made of, driven here
//! rather than by eframe.  eframe opens its window only once it has a GPU
//! device, which takes 150 to 800 ms to start.  This window opens at once,
//! shows the theme's colours until the device arrives (`placeholder`), and
//! then draws with egui like any eframe window.
//!
//! The GPU starts on a thread of its own (see `lib.rs`), which tells the
//! event loop when it is done through [`Wake`].  While it starts it must
//! change the process environment for a moment (`slerp_render::EnvKeeper`),
//! and the window system's libraries on this thread read the environment
//! without a lock, so it asks this thread to make the changes between their
//! calls ([`env_channel`]).
//!
//! The viewer has one window and one egui viewport; dialogs are egui
//! windows inside it.

use crate::ViewerError;
use crate::app::Viewer;
use crate::placeholder::{Placeholder, ViewInsets};
use egui::{ViewportId, ViewportInfo};
use egui_winit::{ActionRequested, accesskit_winit};
use raw_window_handle::{DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle};
use slerp_render::{EnvChanges, EnvKeeper, Gpu};
use slerp_theme::DesktopTheme;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::window::{Window, WindowId};

/*┌─────────────────╮
| Waking the loop |
└─────────────────╯*/
//region Waking the loop

/// What wakes the event loop.
#[derive(Debug)]
pub enum UserEvent {
    /// egui wants a frame at `when` (its repaint callback), unless frame
    /// `pass` has already been drawn since.
    Repaint { when: Instant, pass: u64 },
    /// Another thread has something for the window: the GPU, the theme, or
    /// a change to the environment.
    Wake,
    /// A screen reader's request.
    AccessKit(accesskit_winit::Event),
}

impl From<accesskit_winit::Event> for UserEvent {
    fn from(e: accesskit_winit::Event) -> Self {
        UserEvent::AccessKit(e)
    }
}

/// Wakes the event loop from other threads, also before the loop exists:
/// a wake sent earlier is delivered once it does.
#[derive(Clone, Default)]
pub struct Wake(Arc<Mutex<WakeState>>);

#[derive(Default)]
struct WakeState {
    proxy: Option<EventLoopProxy<UserEvent>>,
    missed: bool,
}

impl Wake {
    /// Wakes the loop with [`UserEvent::Wake`].
    pub fn wake(&self) {
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match &s.proxy {
            Some(p) => {
                let _ = p.send_event(UserEvent::Wake);
            }
            None => s.missed = true,
        }
    }

    /// Wakes `proxy`'s loop from now on, and now if a wake came before.
    pub fn connect(&self, proxy: EventLoopProxy<UserEvent>) {
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if std::mem::take(&mut s.missed) {
            let _ = proxy.send_event(UserEvent::Wake);
        }
        s.proxy = Some(proxy);
    }
}

/// A change to the environment and where to say it is made.
type EnvRequest = (EnvChanges, Sender<()>);

/// The GPU thread's side of [`env_channel`].
pub struct MainThreadEnv {
    requests: Sender<EnvRequest>,
    wake: Wake,
}

impl EnvKeeper for MainThreadEnv {
    /// Hands the change to the main thread and waits until it is made.  If
    /// the main thread has stopped serving (its window closed first), the
    /// change is skipped: the instance then loads every driver, which is
    /// only slower.
    fn change(&mut self, changes: EnvChanges) {
        let asked = Instant::now();
        let (done, made) = mpsc::channel();
        if self.requests.send((changes, done)).is_ok() {
            self.wake.wake();
            let _ = made.recv();
        }
        if crate::env::is_set("SLERP_TIMING") {
            eprintln!(
                "slerp view: environment changed by the main thread in {:.1} ms",
                asked.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
}

/// The main thread's side of [`env_channel`].
pub struct EnvRequests(Receiver<EnvRequest>);

impl EnvRequests {
    /// Makes the changes asked for so far.  The asking thread waits
    /// meanwhile, and the loader's and theme's threads read the environment
    /// only through Rust's own (locked) accessors.
    pub fn serve(&self) {
        while let Ok((changes, done)) = self.0.try_recv() {
            slerp_render::apply_env(changes);
            let _ = done.send(());
        }
    }
}

/// A keeper for `slerp_render::open_gpu_with` on another thread, whose
/// changes this thread makes when its event loop serves [`EnvRequests`].
pub fn env_channel(wake: Wake) -> (MainThreadEnv, EnvRequests) {
    let (tx, rx) = mpsc::channel();
    (MainThreadEnv { requests: tx, wake }, EnvRequests(rx))
}

//endregion

/*┌─────────────────╮
| Frame           |
└─────────────────╯*/
//region Frame

/// What the viewer's frames get of the window: its handles, which file
/// dialogs and drag-out need.
pub struct Frame {
    window: Arc<Window>,
}

impl Frame {
    pub fn winit_window(&self) -> Option<&Window> {
        Some(&self.window)
    }
}

impl HasWindowHandle for Frame {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        self.window.window_handle()
    }
}

impl HasDisplayHandle for Frame {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        self.window.display_handle()
    }
}

//endregion

/*┌─────────────────╮
| Window          |
└─────────────────╯*/
//region Window

/// The GPU a window draws with: there already (a warm viewer), or still
/// starting on its thread.
pub enum GpuSource {
    Ready(Arc<Gpu>),
    Coming(Receiver<Result<Arc<Gpu>, String>>),
}

/// What the viewer is made from once the GPU is there.
pub struct Parts<'a> {
    pub ctx: &'a egui::Context,
    pub render_state: &'a egui_wgpu::RenderState,
    pub gpu: Arc<Gpu>,
    pub theme: DesktopTheme,
}

/// Makes the viewer for a window.
pub type MakeViewer<'a> = Box<dyn FnOnce(Parts<'_>) -> Viewer + 'a>;

/// Everything a window opens with.
pub struct Opening<'a> {
    pub builder: egui::ViewportBuilder,
    pub gpu: GpuSource,
    pub theme: Receiver<DesktopTheme>,
    /// The GPU thread's changes to the environment, for the first window.
    pub env: Option<EnvRequests>,
    /// Where the last window had its view, for the placeholder.
    pub view_insets: Option<ViewInsets>,
    pub surface: egui_wgpu::SurfaceConfig,
    pub make: MakeViewer<'a>,
    /// When the command asked for the window, and whether to print times.
    pub started: Instant,
    pub timing: bool,
}

/// The window from the moment it opens: until the GPU arrives it shows a
/// placeholder, then the viewer.
pub struct Shell<'a> {
    ctx: egui::Context,
    proxy: EventLoopProxy<UserEvent>,
    builder: egui::ViewportBuilder,
    gpu: Option<GpuSource>,
    theme_rx: Receiver<DesktopTheme>,
    theme: Option<DesktopTheme>,
    env: Option<EnvRequests>,
    view_insets: Option<ViewInsets>,
    surface: egui_wgpu::SurfaceConfig,
    make: Option<MakeViewer<'a>>,
    started: Instant,
    timing: bool,

    window: Option<Arc<Window>>,
    input: Option<egui_winit::State>,
    info: ViewportInfo,
    shown: bool,
    placeholder: Option<Placeholder>,
    /// egui's painter on the window, from when the GPU is there until the
    /// viewer is made.
    painter: Option<(egui_wgpu::winit::Painter, Arc<Gpu>)>,
    running: Option<Running>,
    /// When egui next wants a frame.
    repaint_at: Option<Instant>,
    /// Screenshots and clipboard actions egui asked for, for the next paint.
    actions: Vec<ActionRequested>,
    /// Whether the window is in a live resize (macOS presents differently
    /// meanwhile).
    resizing: bool,
    /// The viewer asked to close; the window goes at the loop's next turn.
    closing: bool,
    result: Result<(), ViewerError>,
}

/// The viewer and the painter that draws its frames.
struct Running {
    viewer: Box<Viewer>,
    painter: egui_wgpu::winit::Painter,
    frame: Frame,
}

impl<'a> Shell<'a> {
    pub fn new(opening: Opening<'a>, proxy: EventLoopProxy<UserEvent>) -> Shell<'a> {
        let ctx = egui::Context::default();
        // One window: anything that would be a window of its own (none
        // is) shows as an egui window inside it.
        ctx.set_embed_viewports(true);
        ctx.options_mut(|o| o.max_passes = std::num::NonZeroUsize::new(2).expect("2 is not zero"));
        let repaint = proxy.clone();
        ctx.set_request_repaint_callback(move |info| {
            let when = Instant::now() + info.delay;
            let _ = repaint.send_event(UserEvent::Repaint { when, pass: info.current_cumulative_pass_nr });
        });
        Shell {
            ctx,
            proxy,
            builder: opening.builder,
            gpu: Some(opening.gpu),
            theme_rx: opening.theme,
            theme: None,
            env: opening.env,
            view_insets: opening.view_insets,
            surface: opening.surface,
            make: Some(opening.make),
            started: opening.started,
            timing: opening.timing,
            window: None,
            input: None,
            info: ViewportInfo::default(),
            shown: false,
            placeholder: None,
            painter: None,
            running: None,
            repaint_at: None,
            actions: Vec::new(),
            resizing: false,
            closing: false,
            result: Ok(()),
        }
    }

    /// How the window ended: closed, or failed to open or to start the GPU.
    pub fn finish(self) -> Result<(), ViewerError> {
        self.result
    }

    fn ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }

    /// Opens the window, hidden until it has a picture: accesskit's adapter
    /// must exist before a window is first shown.
    fn open(&mut self, event_loop: &ActiveEventLoop) -> Result<(), ViewerError> {
        let builder = self.builder.clone().with_visible(false);
        let window = egui_winit::create_window(&self.ctx, event_loop, &builder)
            .map_err(|e| ViewerError::Window(e.to_string()))?;
        let window = Arc::new(window);
        let mut input = egui_winit::State::new(
            self.ctx.clone(),
            ViewportId::ROOT,
            event_loop,
            Some(window.scale_factor() as f32),
            event_loop.system_theme(),
            None,
        );
        input.init_accesskit(event_loop, &window, self.proxy.clone());
        // egui knows the scale before the viewer is made, as with eframe.
        egui_winit::update_viewport_info(&mut self.info, &self.ctx, &window, true);
        let ppp = egui_winit::pixels_per_point(&self.ctx, &window);
        self.ctx.input_mut(|i| {
            i.raw.viewports.insert(ViewportId::ROOT, self.info.clone());
            i.pixels_per_point = ppp;
        });
        // macOS draws its own background in a new window, so it shows at
        // once; elsewhere the placeholder's first picture shows it.
        if cfg!(target_os = "macos") {
            self.show(&window);
        }
        if self.timing {
            eprintln!("slerp view: window made at {:.0} ms", self.ms());
        }
        window.request_redraw();
        self.window = Some(window);
        self.input = Some(input);
        Ok(())
    }

    fn show(&mut self, window: &Window) {
        if !self.shown {
            self.shown = true;
            window.set_visible(true);
        }
    }

    /// Serves the GPU thread's changes to the environment.
    fn serve_env(&mut self) {
        if let Some(env) = &self.env {
            env.serve();
        }
    }

    /// Takes what other threads have finished: the theme and the GPU.  The
    /// painter starts on the window as soon as the GPU is there, and the
    /// viewer once the theme is too.
    fn progress(&mut self, event_loop: &ActiveEventLoop) {
        self.serve_env();
        let Some(window) = self.window.clone() else { return };
        if self.theme.is_none()
            && let Ok(theme) = self.theme_rx.try_recv()
        {
            self.theme = Some(theme);
            if self.timing {
                eprintln!("slerp view: theme ready at {:.0} ms", self.ms());
            }
            window.request_redraw();
        }
        if self.running.is_some() {
            return;
        }
        if self.painter.is_none() {
            let gpu = match self.gpu.take() {
                Some(GpuSource::Ready(gpu)) => gpu,
                Some(GpuSource::Coming(rx)) => match rx.try_recv() {
                    Ok(Ok(gpu)) => gpu,
                    Ok(Err(e)) => return self.fail(event_loop, ViewerError::Gpu(e)),
                    Err(mpsc::TryRecvError::Empty) => {
                        self.gpu = Some(GpuSource::Coming(rx));
                        return;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return self.fail(event_loop, ViewerError::Gpu("the GPU thread stopped".into()));
                    }
                },
                None => return,
            };
            match self.painter_for(&window, &gpu) {
                Ok(painter) => self.painter = Some((painter, gpu)),
                Err(e) => return self.fail(event_loop, e),
            }
        }
        if self.theme.is_some()
            && let Some((painter, gpu)) = self.painter.take()
            && let Err(e) = self.start(window, painter, gpu)
        {
            self.fail(event_loop, e);
        }
    }

    /// egui's painter on the window's surface, drawing with `gpu`.
    fn painter_for(&mut self, window: &Arc<Window>, gpu: &Gpu) -> Result<egui_wgpu::winit::Painter, ViewerError> {
        let config = egui_wgpu::WgpuConfiguration {
            surface: self.surface,
            wgpu_setup: egui_wgpu::WgpuSetup::Existing(egui_wgpu::WgpuSetupExisting {
                instance: gpu.instance.clone(),
                adapter: gpu.adapter.clone(),
                device: gpu.device.clone(),
                queue: gpu.queue.clone(),
            }),
            ..Default::default()
        };
        // eframe's defaults: no multisampling or depth, dithered colours.
        let options = egui_wgpu::RendererOptions { dithering: true, ..Default::default() };
        let mut painter = pollster::block_on(egui_wgpu::winit::Painter::new(self.ctx.clone(), config, false, options));
        pollster::block_on(painter.set_window(ViewportId::ROOT, Some(window.clone())))
            .map_err(|e| ViewerError::Window(e.to_string()))?;
        if let (Some(input), Some(side)) = (&mut self.input, painter.max_texture_side()) {
            input.set_max_texture_side(side);
        }
        if self.timing {
            eprintln!("slerp view: painter ready at {:.0} ms", self.ms());
        }
        Ok(painter)
    }

    /// The viewer, drawn by `painter` from the next frame on.
    fn start(
        &mut self,
        window: Arc<Window>,
        painter: egui_wgpu::winit::Painter,
        gpu: Arc<Gpu>,
    ) -> Result<(), ViewerError> {
        let render_state = painter.render_state().ok_or_else(|| ViewerError::Window("no render state".into()))?;
        let (Some(make), Some(theme)) = (self.make.take(), self.theme.take()) else {
            return Err(ViewerError::Window("started twice".into()));
        };
        let viewer = make(Parts { ctx: &self.ctx, render_state: &render_state, gpu, theme });
        self.running = Some(Running { viewer: Box::new(viewer), painter, frame: Frame { window: window.clone() } });
        window.request_redraw();
        Ok(())
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, e: ViewerError) {
        self.result = Err(e);
        self.close(event_loop);
    }

    /// Ends the window: the viewer (which sends the renderer home for a
    /// warm viewer), the painter and the window go now, inside the event
    /// loop, which destroys the window and tells the window system before
    /// it returns.  Dropped after the loop returned, a Wayland window would
    /// stay on screen, frozen, for as long as a warm viewer waits without
    /// turning the loop.  Not called while drawing (see `frame`).
    fn close(&mut self, event_loop: &ActiveEventLoop) {
        self.running = None;
        self.painter = None;
        self.placeholder = None;
        self.input = None;
        self.window = None;
        event_loop.exit();
    }

    /// The placeholder's picture, once the theme's colours are known, and
    /// unless the GPU's painter already has the window (a warm viewer's
    /// window, which shows with its first frame).
    fn paint_placeholder(&mut self) {
        if self.painter.is_some() {
            return;
        }
        let (Some(window), Some(theme)) = (self.window.clone(), &self.theme) else { return };
        if self.placeholder.is_none() {
            self.placeholder = Placeholder::new(&window, theme, self.view_insets);
        }
        if self.placeholder.is_none() {
            return;
        }
        // Shown first: X11 and Windows drop a picture drawn into a hidden
        // window.
        if !self.shown {
            self.show(&window);
            if self.timing {
                eprintln!("slerp view: window shown at {:.0} ms", self.ms());
            }
        }
        if let Some(placeholder) = &mut self.placeholder {
            placeholder.paint(&window);
        }
    }

    /// One egui frame of the viewer, painted.
    fn frame(&mut self, event_loop: &ActiveEventLoop) {
        if self.closing {
            return;
        }
        let (Some(running), Some(window), Some(input)) = (&mut self.running, &self.window, &mut self.input) else {
            return;
        };
        let window = window.clone();

        // 1. Input.
        egui_winit::update_viewport_info(&mut self.info, &self.ctx, &window, false);
        let mut raw = input.take_egui_input(&window);
        raw.viewports = std::iter::once((ViewportId::ROOT, self.info.clone())).collect();
        running.painter.handle_screenshots(&mut raw.events);
        running.viewer.raw_input_hook(&self.ctx, &mut raw);

        // 2. The viewer.
        let output = self.ctx.run_ui(raw, |ui| running.viewer.ui(ui, &mut running.frame));
        self.info.events.clear();
        input.handle_platform_output_with_event_loop(&window, event_loop, output.platform_output);

        // 3. What it asked of the window: title, full screen, close,
        //    screenshots.  A Wayland window can change size here.
        let old_size = window.inner_size();
        let commands = output.viewport_output.get(&ViewportId::ROOT).map(|o| o.commands.clone()).unwrap_or_default();
        egui_winit::process_viewport_commands(&self.ctx, &mut self.info, commands, &window, &mut self.actions);
        let close = self.info.events.iter().any(|e| matches!(e, egui::ViewportEvent::Close));
        let size = window.inner_size();
        if size != old_size
            && let (Some(w), Some(h)) = (std::num::NonZeroU32::new(size.width), std::num::NonZeroU32::new(size.height))
        {
            running.painter.on_window_resized(ViewportId::ROOT, w, h);
        }

        // 4. Paint, with any screenshot asked for.
        let mut shots = Vec::new();
        self.actions.retain(|a| match a {
            ActionRequested::Screenshot(data) => {
                shots.push(data.clone());
                false
            }
            _ => true,
        });
        let primitives = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        let clear = self.ctx.global_style().visuals.panel_fill.to_normalized_gamma_f32();
        running.painter.paint_and_update_textures(
            ViewportId::ROOT,
            output.pixels_per_point,
            clear,
            &primitives,
            &output.textures_delta,
            shots,
            &window,
        );
        // The GPU's picture has replaced the placeholder's.
        self.placeholder = None;
        if !self.shown {
            self.shown = true;
            window.set_visible(true);
        }
        for action in self.actions.drain(..) {
            let event = match action {
                ActionRequested::Cut => egui::Event::Cut,
                ActionRequested::Copy => egui::Event::Copy,
                ActionRequested::Paste => match input.clipboard_text().map(|t| t.replace("\r\n", "\n")) {
                    Some(text) if !text.is_empty() => egui::Event::Paste(text),
                    _ => continue,
                },
                ActionRequested::Screenshot(_) => continue,
            };
            input.egui_input_mut().events.push(event);
        }
        if close {
            // Closed at the start of the loop's next turn: winit destroys a
            // dropped Wayland window when it next handles its requests, and
            // for this window it already has in this turn, before this
            // redraw; were the loop to stop now, the window would stay.
            self.closing = true;
            let _ = self.proxy.send_event(UserEvent::Wake);
            return;
        }
        // A minimised window would otherwise spin (as eframe notes for
        // macOS and Windows).
        if window.is_minimized() == Some(true) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn accesskit(&mut self, event: accesskit_winit::Event) {
        let (Some(input), Some(window)) = (&mut self.input, &self.window) else { return };
        // An event for a window a warm viewer closed before this one.
        if event.window_id != window.id() {
            return;
        }
        match event.window_event {
            accesskit_winit::WindowEvent::InitialTreeRequested => {
                self.ctx.enable_accesskit();
                window.request_redraw();
            }
            accesskit_winit::WindowEvent::ActionRequested(request) => {
                input.on_accesskit_action_request(request);
                window.request_redraw();
            }
            accesskit_winit::WindowEvent::AccessibilityDeactivated => self.ctx.disable_accesskit(),
        }
    }
}

impl ApplicationHandler<UserEvent> for Shell<'_> {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, _: winit::event::StartCause) {
        if self.closing {
            return self.close(event_loop);
        }
        self.serve_env();
        if let Some(at) = self.repaint_at
            && Instant::now() >= at
        {
            self.repaint_at = None;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        if let Err(e) = self.open(event_loop) {
            return self.fail(event_loop, e);
        }
        // The theme and the GPU may be there already (a warm viewer).
        self.progress(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Wake => self.progress(event_loop),
            UserEvent::Repaint { when, pass } => {
                // A request from before the last frame is out of date.
                let now = self.ctx.cumulative_pass_nr_for(ViewportId::ROOT);
                if now == pass || now == pass + 1 {
                    self.repaint_at = Some(self.repaint_at.map_or(when, |t| t.min(when)));
                }
            }
            UserEvent::AccessKit(e) => self.accesskit(e),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(window) = self.window.clone() else { return };
        let keeps_resizing = matches!(event, WindowEvent::Resized(_))
            || (cfg!(target_os = "macos") && matches!(event, WindowEvent::Moved(_)));
        if self.resizing && !keeps_resizing {
            self.resizing = false;
            if let Some(r) = &mut self.running {
                r.painter.on_window_resize_state_change(ViewportId::ROOT, false);
            }
        }
        match &event {
            WindowEvent::RedrawRequested => {
                if self.running.is_some() {
                    self.frame(event_loop);
                } else {
                    self.paint_placeholder();
                }
                return;
            }
            WindowEvent::CloseRequested => {
                drop(window);
                return self.close(event_loop);
            }
            WindowEvent::Resized(size) => {
                if let Some(r) = &mut self.running
                    && let (Some(w), Some(h)) =
                        (std::num::NonZeroU32::new(size.width), std::num::NonZeroU32::new(size.height))
                {
                    if !self.resizing {
                        self.resizing = true;
                        r.painter.on_window_resize_state_change(ViewportId::ROOT, true);
                    }
                    r.painter.on_window_resized(ViewportId::ROOT, w, h);
                }
            }
            _ => {}
        }
        let Some(input) = &mut self.input else { return };
        let response = input.on_window_event(&window, &event);
        if matches!(event, WindowEvent::Resized(_)) && cfg!(windows) && self.running.is_some() {
            // Windows waits for a resized window's picture inside the
            // resize; one drawn later flickers.
            self.frame(event_loop);
        } else if response.repaint || matches!(event, WindowEvent::Resized(_)) {
            window.request_redraw();
        }
    }

    fn device_event(&mut self, _: &ActiveEventLoop, _: DeviceId, event: DeviceEvent) {
        // Mouse movement without a pointer position, for looking around in
        // first person with the pointer captured.
        let DeviceEvent::MouseMotion { delta } = event else { return };
        let (Some(window), Some(input)) = (&self.window, &mut self.input) else { return };
        if self.running.is_some()
            && (window.has_focus() || input.is_any_pointer_button_down())
            && input.on_mouse_motion(delta)
        {
            window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(match self.repaint_at {
            Some(at) => ControlFlow::WaitUntil(at),
            None => ControlFlow::Wait,
        });
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    /// The GPU thread's change waits until the main thread has made it, and
    /// a wake sent before the event loop exists isn't lost.
    #[test]
    fn environment_changes_are_made_by_the_serving_thread() {
        const NAME: &str = "SLERP_TEST_SHELL_ENV_CHANGE";
        let wake = Wake::default();
        let (mut keeper, requests) = env_channel(wake.clone());
        let asker = std::thread::spawn(move || {
            keeper.change(vec![(NAME, Some("1".into()))]);
            // Made by the time `change` returns.
            std::env::var(NAME).ok()
        });
        while !asker.is_finished() {
            requests.serve();
            std::thread::yield_now();
        }
        assert_eq!(asker.join().unwrap().as_deref(), Some("1"));
        assert!(wake.0.lock().unwrap().missed, "the wake waits for the loop");
        requests.serve();
        slerp_render::apply_env(vec![(NAME, None)]);
    }

    /// With the main thread gone, a change is skipped rather than waited
    /// for forever.
    #[test]
    fn a_change_nobody_serves_doesnt_hang() {
        let (mut keeper, requests) = env_channel(Wake::default());
        drop(requests);
        keeper.change(vec![("SLERP_TEST_SHELL_ENV_UNSERVED", Some("1".into()))]);
        assert!(std::env::var_os("SLERP_TEST_SHELL_ENV_UNSERVED").is_none());
    }
}
