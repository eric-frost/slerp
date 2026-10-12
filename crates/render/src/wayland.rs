//! Asks the Wayland compositor which GPU it renders on: the "main device"
//! of `zwp_linux_dmabuf_v1`'s default feedback (protocol version 4).  A
//! window must be drawn on that GPU, and on hybrid laptops it is often not
//! the boot display (KWin may composite on the NVIDIA card while the AMD
//! iGPU is the boot display).

use std::time::{Duration, Instant};
use wayland_client::protocol::{wl_callback, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::{self, ZwpLinuxDmabufFeedbackV1};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;

#[derive(Default)]
struct State {
    dmabuf: Option<ZwpLinuxDmabufV1>,
    synced: bool,
    main_device: Option<u64>,
    feedback_done: bool,
}

/// The compositor's main device as a `dev_t`, or `None` when there is no
/// Wayland session, the compositor lacks dmabuf feedback, or it doesn't
/// answer within `timeout`.
pub fn main_device(timeout: Duration) -> Option<u64> {
    let display = std::env::var_os("WAYLAND_DISPLAY")?;
    // Without a runtime directory (GNOME's thumbnail sandbox) libwayland
    // would print an error to stderr before failing, so don't ask it.
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from);
    if !std::path::Path::new(&display).is_absolute() && !runtime.is_some_and(|r| r.is_dir()) {
        return None;
    }
    let deadline = Instant::now() + timeout;
    let conn = Connection::connect_to_env().ok()?;
    let mut queue: EventQueue<State> = conn.new_event_queue();
    let qh = queue.handle();
    let display = conn.display();
    let mut state = State::default();

    // 1. List the globals; the sync callback marks the end of the list.
    let _registry = display.get_registry(&qh, ());
    let _sync = display.sync(&qh, ());
    wait(&conn, &mut queue, &mut state, deadline, |s| s.synced)?;

    // 2. Ask for the default feedback and wait for its first "done".
    let dmabuf = state.dmabuf.clone()?;
    let feedback = dmabuf.get_default_feedback(&qh, ());
    wait(&conn, &mut queue, &mut state, deadline, |s| s.feedback_done)?;
    feedback.destroy();
    dmabuf.destroy();
    let _ = conn.flush();
    state.main_device
}

/// Reads and dispatches events until `done` holds or the deadline passes.
fn wait(
    conn: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    deadline: Instant,
    done: impl Fn(&State) -> bool,
) -> Option<()> {
    loop {
        queue.dispatch_pending(state).ok()?;
        if done(state) {
            return Some(());
        }
        conn.flush().ok()?;
        let Some(guard) = queue.prepare_read() else { continue };
        let left = deadline.checked_duration_since(Instant::now())?;
        let ready = {
            let fd = guard.connection_fd();
            let mut fds = [rustix::event::PollFd::new(&fd, rustix::event::PollFlags::IN)];
            let ts = rustix::event::Timespec { tv_sec: left.as_secs() as i64, tv_nsec: left.subsec_nanos() as i64 };
            rustix::event::poll(&mut fds, Some(&ts)).ok()?
        };
        if ready == 0 {
            return None;
        }
        guard.read().ok()?;
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event
            && interface == ZwpLinuxDmabufV1::interface().name
            && version >= 4
            && state.dmabuf.is_none()
        {
            state.dmabuf = Some(registry.bind::<ZwpLinuxDmabufV1, _, _>(name, 4, qh, ()));
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.synced = true;
        }
    }
}

impl Dispatch<ZwpLinuxDmabufFeedbackV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpLinuxDmabufFeedbackV1,
        event: zwp_linux_dmabuf_feedback_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // The array holds a dev_t in native byte order: 8 bytes on
            // 64-bit Linux, 4 on some 32-bit systems.
            zwp_linux_dmabuf_feedback_v1::Event::MainDevice { device } => {
                state.main_device = match device.len() {
                    8 => Some(u64::from_ne_bytes(device[..8].try_into().unwrap())),
                    4 => Some(u32::from_ne_bytes(device[..4].try_into().unwrap()) as u64),
                    _ => None,
                };
            }
            zwp_linux_dmabuf_feedback_v1::Event::Done => state.feedback_done = true,
            _ => {}
        }
    }
}

delegate_noop!(State: ignore ZwpLinuxDmabufV1);
