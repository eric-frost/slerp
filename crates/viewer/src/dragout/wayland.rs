//! Drag-out on Wayland, with the core protocol's data device.
//!
//! A drag starts with `wl_data_device.start_drag`, which needs the serial of
//! the pointer button press that began it.  winit receives that press and
//! doesn't pass the serial on, so a second `wl_pointer` object is made on a
//! second event queue of the same connection.  Compositors send input events
//! to every pointer object of a client, so it sees the same presses.  The
//! connection is winit's own: `raw-window-handle` gives the `wl_display`
//! pointer, and `Backend::from_foreign_display` wraps it for an event queue
//! of ours, the same arrangement `smithay-clipboard` uses.  libwayland lets
//! several threads read the socket: whichever thread wakes first reads, and
//! each event lands in the queue of the object it was addressed to, so
//! winit's events stay winit's.
//!
//! A thread owns that queue.  It sleeps in `poll` on the socket and an
//! eventfd (zero cost while idle), tracks button presses, and when asked to
//! start a drag creates the data source, offers the types, calls
//! `start_drag` and then answers the compositor's requests: `send` (a drop
//! target wants the data: the files are made now, and written to the pipe on
//! a short thread so a slow reader can't stall the queue), `cancelled`,
//! `dnd_drop_performed` and `dnd_finished`.

use super::payload::{ActiveGuard, Drag, DragError, DragIcon, OFFERED, Outcome, Payload, Session};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use rustix::event::{EventfdFlags, PollFd, PollFlags, eventfd};
use std::ffi::c_void;
use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use wayland_client::backend::{Backend, ObjectId, WaylandError};
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_compositor::WlCompositor,
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::{DndAction, WlDataDeviceManager},
    wl_data_offer::WlDataOffer,
    wl_data_source::{self, WlDataSource},
    wl_pointer::{self, WlPointer},
    wl_registry::WlRegistry,
    wl_seat::{self, WlSeat},
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, event_created_child};

/// How long to wait for a drop target to say it is finished after the drop
/// before giving up on it, so a target that never answers doesn't block
/// every later drag.
const FINISH_TIMEOUT: Duration = Duration::from_secs(30);

//region Handle

/// What the caller holds: a way to talk to the thread.
struct Handle {
    display: usize,
    commands: Sender<Command>,
    wake: OwnedFd,
    thread: Option<std::thread::JoinHandle<()>>,
}

enum Slot {
    Empty,
    Ready(Handle),
    /// Setting up failed for a reason that won't change (no data device in
    /// this compositor), remembered so every drag doesn't retry.
    Unsupported {
        display: usize,
        why: String,
    },
}

static SLOT: Mutex<Slot> = Mutex::new(Slot::Empty);

enum Command {
    Start { origin: usize, drag: Drag, guard: ActiveGuard, reply: Sender<Result<(), DragError>> },
    Exit,
}

impl Handle {
    fn send(&self, command: Command) -> Result<(), DragError> {
        self.commands.send(command).map_err(|_| DragError::Platform("the drag thread has stopped".into()))?;
        let _ = rustix::io::write(&self.wake, &1u64.to_ne_bytes());
        Ok(())
    }

    fn stop(mut self) {
        let _ = self.send(Command::Exit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn lock() -> MutexGuard<'static, Slot> {
    SLOT.lock().unwrap_or_else(|e| e.into_inner())
}

/// The `wl_display` of winit's connection, or why this isn't Wayland.
fn display_of(win: RawWindowHandle, display: RawDisplayHandle) -> Result<(usize, usize), DragError> {
    match (win, display) {
        (RawWindowHandle::Wayland(w), RawDisplayHandle::Wayland(d)) => {
            Ok((d.display.as_ptr() as usize, w.surface.as_ptr() as usize))
        }
        (RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_), _) => {
            Err(DragError::Unsupported("this is an X11 window; dragging out needs Wayland".into()))
        }
        _ => Err(DragError::Unsupported("this window isn't a Wayland window".into())),
    }
}

pub fn prepare(win: RawWindowHandle, display: RawDisplayHandle) -> Result<(), DragError> {
    let (display, _) = display_of(win, display)?;
    let mut slot = lock();
    ensure(&mut slot, display).map(|_| ())
}

/// Starts the thread for `display` if there isn't one, and returns it.
fn ensure(slot: &mut Slot, display: usize) -> Result<&Handle, DragError> {
    match slot {
        Slot::Ready(h) if h.display == display && h.thread.as_ref().is_some_and(|t| !t.is_finished()) => {}
        Slot::Unsupported { display: d, why } if *d == display => {
            return Err(DragError::Unsupported(why.clone()));
        }
        _ => {
            if let Slot::Ready(old) = std::mem::replace(slot, Slot::Empty) {
                old.stop();
            }
            match spawn(display) {
                Ok(handle) => *slot = Slot::Ready(handle),
                Err(DragError::Unsupported(why)) => {
                    *slot = Slot::Unsupported { display, why: why.clone() };
                    return Err(DragError::Unsupported(why));
                }
                Err(e) => return Err(e),
            }
        }
    }
    match slot {
        Slot::Ready(h) => Ok(h),
        _ => unreachable!(),
    }
}

fn spawn(display: usize) -> Result<Handle, DragError> {
    let (commands, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    let wake = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)
        .map_err(|e| DragError::Platform(format!("eventfd: {e}")))?;
    let thread_wake = wake.try_clone().map_err(|e| DragError::Platform(e.to_string()))?;
    let thread = std::thread::Builder::new()
        .name("dragout".into())
        .spawn(move || run(display, rx, thread_wake, ready_tx))
        .map_err(|e| DragError::Platform(e.to_string()))?;
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(Handle { display, commands, wake, thread: Some(thread) }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => Err(DragError::Platform("the Wayland connection didn't answer".into())),
    }
}

pub fn start(win: RawWindowHandle, display: RawDisplayHandle, drag: Drag) -> Result<(), DragError> {
    let (display, origin) = display_of(win, display)?;
    let guard = ActiveGuard::acquire()?;
    let (reply, answer) = mpsc::channel();
    {
        let mut slot = lock();
        let handle = ensure(&mut slot, display)?;
        handle.send(Command::Start { origin, drag, guard, reply })?;
    }
    answer
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| Err(DragError::Platform("the drag thread didn't answer".into())))
}

/// Stops the thread.  Call when the event loop is over, before winit closes
/// the connection; a drag cannot start afterwards unless [`prepare`] is
/// called again.
pub fn shutdown() {
    if let Slot::Ready(handle) = std::mem::replace(&mut *lock(), Slot::Empty) {
        handle.stop();
    }
}

//endregion

//region Thread

fn run(display: usize, commands: Receiver<Command>, wake: OwnedFd, ready: Sender<Result<(), DragError>>) {
    // SAFETY: the pointer is winit's `wl_display`, which lives as long as its
    // event loop; `shutdown` or a new `prepare` stops this thread before
    // that ends, and the process exiting ends it anyway.
    let backend = unsafe { Backend::from_foreign_display(display as *mut c_void as *mut _) };
    let conn = Connection::from_backend(backend);
    let (mut state, mut queue) = match State::connect(&conn) {
        Ok(pair) => pair,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    drop(ready);

    loop {
        // 1. Events first, so a button press that arrived just before a
        //    start command is known when the command runs.
        if queue.dispatch_pending(&mut state).is_err() {
            break;
        }
        while let Ok(command) = commands.try_recv() {
            match command {
                Command::Exit => return,
                Command::Start { origin, drag, guard, reply } => {
                    let result = state.start(&conn, &queue.handle(), origin, drag, guard);
                    let _ = reply.send(result);
                }
            }
        }
        state.check_finish_timeout();
        if conn.flush().is_err() {
            break;
        }

        // 2. Sleep until the compositor or a command wakes us.  The read
        //    guard is held while sleeping, as winit's own loop does, so
        //    whichever thread wakes first reads for both.
        let Some(guard) = conn.prepare_read() else { continue };
        let timeout = state
            .wait_limit()
            .map(|d| rustix::event::Timespec { tv_sec: d.as_secs() as _, tv_nsec: d.subsec_nanos() as _ });
        let polled = {
            let fd = guard.connection_fd();
            let mut fds = [PollFd::new(&fd, PollFlags::IN | PollFlags::ERR), PollFd::new(&wake, PollFlags::IN)];
            rustix::event::poll(&mut fds, timeout.as_ref())
                .map(|_| (!fds[0].revents().is_empty(), !fds[1].revents().is_empty()))
        };
        let (display_ready, wake_ready) = match polled {
            Ok(ready) => ready,
            Err(rustix::io::Errno::INTR) => {
                drop(guard);
                continue;
            }
            Err(_) => break,
        };
        if display_ready {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => break,
            }
        } else {
            drop(guard);
        }
        if wake_ready {
            let mut buf = [0u8; 8];
            let _ = rustix::io::read(&wake, &mut buf);
        }
    }
    // The connection is gone or broken: nothing can start until the next
    // `prepare` makes a new thread.
    state.end_drag(Outcome::Cancelled);
}

//endregion

//region State

/// The Wayland objects and the drag in progress, owned by the thread.
struct State {
    conn: Connection,
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    seat: WlSeat,
    pointer: Option<WlPointer>,
    manager: WlDataDeviceManager,
    manager_version: u32,
    /// Buttons held in our windows with the serial of each press, oldest
    /// first.  Cleared when the pointer leaves: a drag takes the pointer
    /// without a release.
    pressed: Vec<(u32, u32)>,
    /// Data offers seen on the current data device, kept so they can be
    /// destroyed when the compositor moves on.
    offers: Vec<WlDataOffer>,
    drag: Option<ActiveDrag>,
    next_id: u64,
}

/// A drag in progress: the protocol objects and the data to give.
struct ActiveDrag {
    id: u64,
    source: WlDataSource,
    device: WlDataDevice,
    icon: Option<IconSurface>,
    session: Session,
    dropped_at: Option<Instant>,
}

struct IconSurface {
    surface: WlSurface,
    buffer: WlBuffer,
}

impl State {
    fn connect(conn: &Connection) -> Result<(State, EventQueue<State>), DragError> {
        let (globals, mut queue) = registry_queue_init::<State>(conn)
            .map_err(|e| DragError::Platform(format!("reading the Wayland globals: {e}")))?;
        let qh = queue.handle();
        let manager_global = globals_version(&globals, "wl_data_device_manager");
        if manager_global.is_none() {
            return Err(DragError::Unsupported("the compositor has no wl_data_device_manager".into()));
        }
        let manager: WlDataDeviceManager =
            globals.bind(&qh, 1..=3, ()).map_err(|e| DragError::Unsupported(format!("wl_data_device_manager: {e}")))?;
        let seat: WlSeat = globals.bind(&qh, 1..=7, ()).map_err(|e| DragError::Unsupported(format!("wl_seat: {e}")))?;
        let mut state = State {
            conn: conn.clone(),
            compositor: globals.bind(&qh, 1..=4, ()).ok(),
            shm: globals.bind(&qh, 1..=1, ()).ok(),
            seat,
            pointer: None,
            manager,
            manager_version: manager_global.unwrap_or(1).min(3),
            pressed: Vec::new(),
            offers: Vec::new(),
            drag: None,
            next_id: 1,
        };
        // The seat reports its pointer in an event after it is bound.
        queue.roundtrip(&mut state).map_err(|e| DragError::Platform(format!("roundtrip: {e}")))?;
        Ok((state, queue))
    }

    /// How long the thread may sleep: until a dropped drag's finish timeout.
    fn wait_limit(&self) -> Option<Duration> {
        let dropped = self.drag.as_ref()?.dropped_at?;
        Some((dropped + FINISH_TIMEOUT).saturating_duration_since(Instant::now()).max(Duration::from_millis(1)))
    }

    fn check_finish_timeout(&mut self) {
        if let Some(drag) = &self.drag
            && drag.dropped_at.is_some_and(|t| t.elapsed() >= FINISH_TIMEOUT)
        {
            eprintln!("slerp dragout: the drop target never said it was finished; treating the drop as done");
            self.end_drag(Outcome::Dropped);
        }
    }

    fn latest_press(&self) -> Option<u32> {
        self.pressed.last().map(|&(_, serial)| serial)
    }

    fn start(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<State>,
        origin: usize,
        drag: Drag,
        guard: ActiveGuard,
    ) -> Result<(), DragError> {
        if self.drag.is_some() {
            return Err(DragError::Busy);
        }
        // 1. Wayland refuses a drag that doesn't come from a button press.
        let serial = self.latest_press().ok_or(DragError::NoGesture)?;

        // 2. The window's surface, as an object of this queue's connection.
        let origin = {
            let interface = WlSurface::interface();
            // SAFETY: the pointer is the `wl_surface` winit gave
            // `raw-window-handle` for a window that is being used right now.
            let id = unsafe { ObjectId::from_ptr(interface, origin as *mut c_void as *mut _) }
                .map_err(|_| DragError::Platform("the window's surface isn't a wl_surface".into()))?;
            WlSurface::from_id(conn, id).map_err(|_| DragError::Platform("invalid window surface".into()))?
        };

        // 3. The source, offering every type we can answer.
        let id = self.next_id;
        self.next_id += 1;
        let device = self.manager.get_data_device(&self.seat, qh, ());
        let source = self.manager.create_data_source(qh, id);
        for mime in OFFERED {
            source.offer(mime.to_string());
        }
        if self.manager_version >= 3 {
            // Copying is all an extracted file can mean.
            source.set_actions(DndAction::Copy);
        }
        let icon = drag.icon.as_ref().filter(|i| i.is_valid()).and_then(|i| self.make_icon(i, qh));

        // 4. Hand the pointer to the compositor.
        device.start_drag(Some(&source), &origin, icon.as_ref().map(|i| &i.surface), serial);
        if let Some(icon) = &icon {
            icon.surface.commit();
        }
        conn.flush().map_err(|e| DragError::Platform(e.to_string()))?;

        let session = Session::new(Payload::new(drag.items), guard);
        self.drag = Some(ActiveDrag { id, source, device, icon, session, dropped_at: None });
        // The press that began it is spent.
        self.pressed.clear();
        Ok(())
    }

    /// The drag picture as a shared-memory buffer on its own surface, with
    /// the pointer at its top-left corner; `None` if the compositor lacks
    /// the pieces (the drag then just has no picture).
    fn make_icon(&self, icon: &DragIcon, qh: &QueueHandle<State>) -> Option<IconSurface> {
        let (compositor, shm) = (self.compositor.as_ref()?, self.shm.as_ref()?);
        let (w, h) = (icon.width as i32, icon.height as i32);
        let size = icon.width as usize * icon.height as usize * 4;
        let fd = rustix::fs::memfd_create("slerp-drag-icon", rustix::fs::MemfdFlags::CLOEXEC).ok()?;
        // wl_shm ARGB8888 is premultiplied, with blue first in memory on a
        // little-endian machine.
        let mut argb = Vec::with_capacity(size);
        for px in icon.rgba.chunks_exact(4) {
            let a = px[3] as u32;
            let mul = |c: u8| ((c as u32 * a + 127) / 255) as u8;
            argb.extend_from_slice(&[mul(px[2]), mul(px[1]), mul(px[0]), px[3]]);
        }
        let mut file = std::fs::File::from(fd);
        file.write_all(&argb).ok()?;
        let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
        let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, qh, ());
        pool.destroy();
        let surface = compositor.create_surface(qh, ());
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, w, h);
        Some(IconSurface { surface, buffer })
    }

    /// Ends the drag in progress, if any, and tells the caller how.
    fn end_drag(&mut self, how: Outcome) {
        let Some(mut drag) = self.drag.take() else { return };
        let outcome = match how {
            Outcome::Dropped => drag.session.finished(),
            Outcome::Cancelled => drag.session.cancelled(),
        };
        drag.source.destroy();
        if drag.device.version() >= 2 {
            drag.device.release();
        }
        if let Some(icon) = drag.icon.take() {
            icon.surface.destroy();
            icon.buffer.destroy();
        }
        let _ = self.conn.flush();
        super::report(outcome);
    }

    fn drag_with_id(&mut self, id: u64) -> Option<&mut ActiveDrag> {
        self.drag.as_mut().filter(|d| d.id == id)
    }
}

/// The version the compositor advertises for a global, if it does.
fn globals_version(globals: &GlobalList, interface: &str) -> Option<u32> {
    globals.contents().with_list(|list| list.iter().find(|g| g.interface == interface).map(|g| g.version))
}

//endregion

//region Events

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut State,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(state: &mut State, seat: &WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<State>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            let has_pointer = caps.contains(wl_seat::Capability::Pointer);
            if has_pointer && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(qh, ()));
            } else if !has_pointer && let Some(pointer) = state.pointer.take() {
                if pointer.version() >= 3 {
                    pointer.release();
                }
                state.pressed.clear();
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        state: &mut State,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        match event {
            wl_pointer::Event::Button { serial, button, state: button_state, .. } => {
                state.pressed.retain(|&(b, _)| b != button);
                if matches!(button_state, WEnum::Value(wl_pointer::ButtonState::Pressed)) {
                    state.pressed.push((button, serial));
                }
            }
            wl_pointer::Event::Leave { .. } => state.pressed.clear(),
            _ => {}
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        state: &mut State,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        // This device exists only to start a drag, but the compositor still
        // introduces the clipboard's offer, and our own drag's, to it.  Keep
        // no more than the newest offers: the compositor never refers to
        // an older one again.
        match event {
            wl_data_device::Event::DataOffer { id } => {
                state.offers.push(id);
                while state.offers.len() > 4 {
                    state.offers.remove(0).destroy();
                }
            }
            wl_data_device::Event::Selection { .. } | wl_data_device::Event::Leave => {}
            _ => {}
        }
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for State {
    fn event(
        _: &mut State,
        _: &WlDataOffer,
        _: <WlDataOffer as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<WlDataSource, u64> for State {
    fn event(
        state: &mut State,
        _: &WlDataSource,
        event: wl_data_source::Event,
        id: &u64,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                let Some(drag) = state.drag_with_id(*id) else { return };
                // The files are made here, in order, the first time a target
                // asks; the pipe is written on a thread of its own so a
                // reader that is slow, or has gone, can't hold up the queue.
                if let Some(bytes) = drag.session.data(&mime_type) {
                    let _ = std::thread::Builder::new().name("dragout-write".into()).spawn(move || {
                        let mut file = std::fs::File::from(fd);
                        let _ = file.write_all(&bytes);
                    });
                }
            }
            wl_data_source::Event::Cancelled => {
                if state.drag_with_id(*id).is_some() {
                    state.end_drag(Outcome::Cancelled);
                }
            }
            wl_data_source::Event::DndDropPerformed => {
                if let Some(drag) = state.drag_with_id(*id) {
                    drag.session.drop_performed();
                    drag.dropped_at = Some(Instant::now());
                    // A source of version 2 or lower gets no `dnd_finished`.
                    if state.manager_version < 3 {
                        state.end_drag(Outcome::Dropped);
                    }
                }
            }
            wl_data_source::Event::DndFinished => {
                if state.drag_with_id(*id).is_some() {
                    state.end_drag(Outcome::Dropped);
                }
            }
            // The target and the action it chose change nothing: files are
            // copied.
            _ => {}
        }
    }
}

macro_rules! ignore_events {
    ($($interface:ty),*) => {$(
        impl Dispatch<$interface, ()> for State {
            fn event(_: &mut State, _: &$interface, _: <$interface as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {}
        }
    )*};
}

ignore_events!(WlCompositor, WlShm, WlShmPool, WlBuffer, WlSurface, WlDataDeviceManager);

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x11_windows_are_unsupported() {
        use raw_window_handle::{XlibDisplayHandle, XlibWindowHandle};
        let win = RawWindowHandle::Xlib(XlibWindowHandle::new(1));
        let display = RawDisplayHandle::Xlib(XlibDisplayHandle::new(None, 0));
        let err = prepare(win, display).unwrap_err();
        assert!(matches!(err, DragError::Unsupported(_)));
        assert!(err.to_string().contains("Ctrl+C"));
    }
}
