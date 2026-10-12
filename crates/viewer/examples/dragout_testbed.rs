//! Tools for testing drag-out on Wayland without a person, inside a nested
//! compositor (`tools/test-dragout.sh` starts a headless sway and runs
//! them): a drop target and a pointer driver.
//!
//! - `receiver [--reject] [--read-on-enter] [--seconds N] [--exit-after N]`
//!   opens a window that accepts `text/uri-list` drops and logs, to standard
//!   output, what it receives: each URI, the file's size and its first bytes.
//!   `--read-on-enter` reads the list as soon as the pointer enters, as
//!   Dolphin does while hovering; `--reject` refuses every drop.
//! - `drive [--extent=WxH] <step>...` moves a virtual pointer
//!   (`zwlr_virtual_pointer_v1` in sway, `org_kde_kwin_fake_input` in a
//!   nested KWin; neither touches a real pointer) over the compositor's
//!   output.  Steps: `at:X,Y` jumps there, `drag:X,Y` moves
//!   there in small steps, `press` and `release` use the left button,
//!   `wait:MS` sleeps, `key:CODE` presses a key by its evdev code (KWin only).
//!
//! - `shot FILE.png` saves the compositor's output as a picture
//!   (`wlr-screencopy`, so sway but not KWin), to see what a test did.
//!
//! None of this is useful outside the test.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("dragout_testbed is for Wayland on Linux");
}

#[cfg(target_os = "linux")]
fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("receiver") => receiver::run(args.collect()),
        Some("drive") => driver::run(args.collect()),
        Some("shot") => shot::run(args.collect()),
        _ => {
            eprintln!("usage: dragout_testbed receiver|drive ...");
            std::process::exit(2);
        }
    }
}

#[cfg(target_os = "linux")]
mod receiver {
    use std::io::Read;
    use std::os::fd::{AsFd, OwnedFd};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::{
        wl_buffer::WlBuffer,
        wl_compositor::WlCompositor,
        wl_data_device::{self, WlDataDevice},
        wl_data_device_manager::{DndAction, WlDataDeviceManager},
        wl_data_offer::{self, WlDataOffer},
        wl_registry::WlRegistry,
        wl_seat::WlSeat,
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    };
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
    use wayland_protocols::xdg::shell::client::{
        xdg_surface::{self, XdgSurface},
        xdg_toplevel::{self, XdgToplevel},
        xdg_wm_base::{self, XdgWmBase},
    };

    #[derive(Default)]
    struct OfferMimes(Mutex<Vec<String>>);

    struct State {
        conn: Connection,
        reject: bool,
        read_on_enter: bool,
        exit_after: usize,
        drops: usize,
        done: bool,
        surface: WlSurface,
        shm: WlShm,
        buffer: Option<WlBuffer>,
        current: Option<WlDataOffer>,
    }

    fn say(line: &str) {
        println!("{line}");
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }

    pub fn run(args: Vec<String>) {
        let mut seconds = 60u64;
        let mut state_args = (false, false, 1usize);
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--reject" => state_args.0 = true,
                "--read-on-enter" => state_args.1 = true,
                "--seconds" => seconds = it.next().and_then(|s| s.parse().ok()).unwrap_or(seconds),
                "--exit-after" => state_args.2 = it.next().and_then(|s| s.parse().ok()).unwrap_or(1),
                other => panic!("unknown option {other}"),
            }
        }
        let conn = Connection::connect_to_env().expect("no Wayland compositor");
        let (globals, mut queue) = registry_queue_init::<State>(&conn).expect("registry");
        let qh = queue.handle();
        let compositor: WlCompositor = globals.bind(&qh, 1..=4, ()).expect("wl_compositor");
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
        let wm_base: XdgWmBase = globals.bind(&qh, 1..=1, ()).expect("xdg_wm_base");
        let seat: WlSeat = globals.bind(&qh, 1..=5, ()).expect("wl_seat");
        let manager: WlDataDeviceManager = globals.bind(&qh, 1..=3, ()).expect("wl_data_device_manager");
        let _device = manager.get_data_device(&seat, &qh, ());

        let surface = compositor.create_surface(&qh, ());
        let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_title("dragout receiver".into());
        toplevel.set_app_id("dragout-receiver".into());
        surface.commit();

        let mut state = State {
            conn: conn.clone(),
            reject: state_args.0,
            read_on_enter: state_args.1,
            exit_after: state_args.2,
            drops: 0,
            done: false,
            surface,
            shm,
            buffer: None,
            current: None,
        };
        let until = Instant::now() + Duration::from_secs(seconds);
        while !state.done && Instant::now() < until {
            // A short timeout lets the deadline and `done` be noticed.
            let _ = conn.flush();
            if let Some(guard) = conn.prepare_read() {
                let fd = guard.connection_fd();
                let mut fds = [rustix::event::PollFd::new(&fd, rustix::event::PollFlags::IN)];
                let timeout = rustix::event::Timespec { tv_sec: 0, tv_nsec: 100_000_000 };
                let ready = rustix::event::poll(&mut fds, Some(&timeout)).map(|n| n > 0).unwrap_or(false);
                if ready {
                    let _ = guard.read();
                } else {
                    drop(guard);
                }
            }
            queue.dispatch_pending(&mut state).expect("dispatch");
        }
        say("receiver exiting");
    }

    /// Reads one type from the offer through a pipe.
    fn read_offer(state: &State, offer: &WlDataOffer, mime: &str) -> Option<String> {
        let (reader, writer) = std::io::pipe().ok()?;
        let writer: OwnedFd = writer.into();
        offer.receive(mime.to_string(), writer.as_fd());
        drop(writer);
        state.conn.flush().ok()?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut text = Vec::new();
            let mut reader = reader;
            let _ = reader.read_to_end(&mut text);
            let _ = tx.send(text);
        });
        let bytes = rx.recv_timeout(Duration::from_secs(20)).ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn percent_decode(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%'
                && i + 2 < b.len()
                && let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
            out.push(b[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// Reports a received list: how many files it names and whether each
    /// exists, with the first few in detail.
    fn describe(list: &str) {
        let lines: Vec<&str> = list.split("\r\n").filter(|l| !l.is_empty()).collect();
        say(&format!(
            "uri-list has {} entries, crlf-terminated: {}",
            lines.len(),
            list.is_empty() || list.ends_with("\r\n")
        ));
        let mut missing = 0;
        for (n, uri) in lines.iter().enumerate() {
            let Some(path) = uri.strip_prefix("file://") else {
                say(&format!("  not a file uri: {uri}"));
                continue;
            };
            let path = percent_decode(path);
            match std::fs::read(&path) {
                Ok(data) if n < 5 => {
                    let head: Vec<String> = data.iter().take(8).map(|b| format!("{b:02x}")).collect();
                    say(&format!("  file {path} size {} head {}", data.len(), head.join("")));
                }
                Ok(_) => {}
                Err(e) => {
                    missing += 1;
                    say(&format!("  file {path} unreadable: {e}"));
                }
            }
        }
        say(&format!("  {missing} of the files are missing"));
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
            match event {
                wl_data_device::Event::Enter { serial, x, y, id: Some(offer), .. } => {
                    let mimes = offer.data::<OfferMimes>().map(|m| m.0.lock().unwrap().clone()).unwrap_or_default();
                    say(&format!("enter at {x:.0},{y:.0} offering {mimes:?}"));
                    if state.reject {
                        offer.accept(serial, None);
                    } else {
                        offer.accept(serial, Some("text/uri-list".into()));
                        if offer.version() >= 3 {
                            offer.set_actions(DndAction::Copy, DndAction::Copy);
                        }
                        if state.read_on_enter
                            && let Some(list) = read_offer(state, &offer, "text/uri-list")
                        {
                            say("read on enter:");
                            describe(&list);
                        }
                    }
                    state.current = Some(offer);
                }
                wl_data_device::Event::Leave => {
                    say("leave");
                    if let Some(offer) = state.current.take() {
                        offer.destroy();
                    }
                }
                wl_data_device::Event::Drop => {
                    say("drop");
                    if let Some(offer) = state.current.take() {
                        if let Some(list) = read_offer(state, &offer, "text/uri-list") {
                            say("read on drop:");
                            describe(&list);
                        }
                        if let Some(text) = read_offer(state, &offer, "text/plain") {
                            say(&format!("plain text has {} lines", text.lines().count()));
                        }
                        if offer.version() >= 3 {
                            offer.finish();
                        }
                        offer.destroy();
                        state.drops += 1;
                        if state.drops >= state.exit_after {
                            let _ = state.conn.flush();
                            state.done = true;
                        }
                    }
                }
                _ => {}
            }
        }

        event_created_child!(State, WlDataDevice, [
            wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, OfferMimes::default()),
        ]);
    }

    impl Dispatch<WlDataOffer, OfferMimes> for State {
        fn event(
            _: &mut State,
            _: &WlDataOffer,
            event: wl_data_offer::Event,
            data: &OfferMimes,
            _: &Connection,
            _: &QueueHandle<State>,
        ) {
            if let wl_data_offer::Event::Offer { mime_type } = event {
                data.0.lock().unwrap().push(mime_type);
            }
        }
    }

    impl Dispatch<XdgWmBase, ()> for State {
        fn event(
            _: &mut State,
            base: &XdgWmBase,
            event: xdg_wm_base::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<State>,
        ) {
            if let xdg_wm_base::Event::Ping { serial } = event {
                base.pong(serial);
            }
        }
    }

    impl Dispatch<XdgSurface, ()> for State {
        fn event(
            state: &mut State,
            xdg: &XdgSurface,
            event: xdg_surface::Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<State>,
        ) {
            if let xdg_surface::Event::Configure { serial } = event {
                xdg.ack_configure(serial);
                if state.buffer.is_none() {
                    // A solid green window, 400 x 300.
                    let (w, h) = (400i32, 300i32);
                    let size = (w * h * 4) as usize;
                    let fd = rustix::fs::memfd_create("receiver", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
                    let mut file = std::fs::File::from(fd);
                    use std::io::Write;
                    file.write_all(&[0x60, 0xA0, 0x30, 0xFF].repeat(size / 4)).unwrap();
                    let pool = state.shm.create_pool(file.as_fd(), size as i32, qh, ());
                    let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, qh, ());
                    pool.destroy();
                    state.surface.attach(Some(&buffer), 0, 0);
                    state.surface.commit();
                    state.buffer = Some(buffer);
                    say("receiver ready");
                }
            }
        }
    }

    impl Dispatch<XdgToplevel, ()> for State {
        fn event(
            _: &mut State,
            _: &XdgToplevel,
            _: xdg_toplevel::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<State>,
        ) {
        }
    }

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

    macro_rules! ignore {
        ($($t:ty),*) => {$(
            impl Dispatch<$t, ()> for State {
                fn event(_: &mut State, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {}
            }
        )*};
    }
    ignore!(WlCompositor, WlShm, WlShmPool, WlBuffer, WlSurface, WlSeat, WlDataDeviceManager);
}

#[cfg(target_os = "linux")]
mod driver {
    use std::time::{Duration, Instant};
    use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
    use wayland_client::protocol::{wl_pointer::ButtonState, wl_registry::WlRegistry, wl_seat::WlSeat};
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
    use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input::OrgKdeKwinFakeInput;
    use wayland_protocols_wlr::virtual_pointer::v1::client::{
        zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1, zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    };

    struct State;

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
    macro_rules! ignore {
        ($($t:ty),*) => {$(
            impl Dispatch<$t, ()> for State {
                fn event(_: &mut State, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {}
            }
        )*};
    }
    ignore!(WlSeat, ZwlrVirtualPointerManagerV1, ZwlrVirtualPointerV1, OrgKdeKwinFakeInput);

    const BTN_LEFT: u32 = 0x110;

    /// A pointer that moves without any real device: wlroots compositors
    /// (sway) offer `zwlr_virtual_pointer_v1` to everyone, KWin offers
    /// `org_kde_kwin_fake_input` to clients it trusts (a nested KWin started
    /// with `KWIN_WAYLAND_NO_PERMISSION_CHECKS=1` trusts all).
    enum Pointer {
        Wlr(ZwlrVirtualPointerV1, (u32, u32)),
        Kde(OrgKdeKwinFakeInput),
    }

    impl Pointer {
        fn open(globals: &GlobalList, qh: &QueueHandle<State>, extent: (u32, u32)) -> Pointer {
            if let Ok(manager) = globals.bind::<ZwlrVirtualPointerManagerV1, _, _>(qh, 1..=2, ()) {
                let seat: WlSeat = globals.bind(qh, 1..=5, ()).expect("wl_seat");
                return Pointer::Wlr(manager.create_virtual_pointer(Some(&seat), qh, ()), extent);
            }
            let fake: OrgKdeKwinFakeInput = globals
                .bind(qh, 1..=5, ())
                .expect("neither zwlr_virtual_pointer_manager_v1 nor org_kde_kwin_fake_input: this compositor can't fake a pointer");
            fake.authenticate("dragout test".into(), "tests dragging files out of a window".into());
            Pointer::Kde(fake)
        }

        fn motion(&self, time: u32, p: (f64, f64)) {
            match self {
                Pointer::Wlr(pointer, extent) => {
                    pointer.motion_absolute(time, p.0 as u32, p.1 as u32, extent.0, extent.1);
                    pointer.frame();
                }
                Pointer::Kde(fake) => fake.pointer_motion_absolute(p.0, p.1),
            }
        }

        fn button(&self, time: u32, down: bool) {
            match self {
                Pointer::Wlr(pointer, _) => {
                    pointer.button(time, BTN_LEFT, if down { ButtonState::Pressed } else { ButtonState::Released });
                    pointer.frame();
                }
                Pointer::Kde(fake) => fake.button(BTN_LEFT, down as u32),
            }
        }

        /// A key press and release by its evdev code (KDE fake input only).
        fn key(&self, code: u32) {
            if let Pointer::Kde(fake) = self {
                fake.keyboard_key(code, 1);
                fake.keyboard_key(code, 0);
            }
        }

        fn close(&self) {
            match self {
                Pointer::Wlr(pointer, _) => pointer.destroy(),
                Pointer::Kde(fake) => fake.destroy(),
            }
        }
    }

    fn xy(s: &str) -> (f64, f64) {
        let (x, y) = s.split_once(',').expect("X,Y");
        (x.parse().expect("X"), y.parse().expect("Y"))
    }

    pub fn run(args: Vec<String>) {
        let mut extent = (1280u32, 720u32);
        let mut steps = Vec::new();
        for a in args {
            if let Some(e) = a.strip_prefix("--extent=") {
                let (w, h) = e.split_once('x').expect("WxH");
                extent = (w.parse().unwrap(), h.parse().unwrap());
            } else {
                steps.push(a);
            }
        }
        let conn = Connection::connect_to_env().expect("no Wayland compositor");
        let (globals, mut queue) = registry_queue_init::<State>(&conn).expect("registry");
        let qh = queue.handle();
        let pointer = Pointer::open(&globals, &qh, extent);
        let mut state = State;
        queue.roundtrip(&mut state).expect("roundtrip");
        let start = Instant::now();
        let time = || start.elapsed().as_millis() as u32;
        let mut at = (0.0f64, 0.0f64);
        for step in steps {
            let (name, arg) = step.split_once(':').unwrap_or((&step, ""));
            match name {
                "at" => {
                    at = xy(arg);
                    pointer.motion(time(), at);
                }
                "drag" => {
                    let to = xy(arg);
                    let distance = ((to.0 - at.0).powi(2) + (to.1 - at.1).powi(2)).sqrt();
                    let n = (distance / 6.0).ceil().max(1.0) as u32;
                    for i in 1..=n {
                        let t = i as f64 / n as f64;
                        pointer.motion(time(), (at.0 + (to.0 - at.0) * t, at.1 + (to.1 - at.1) * t));
                        let _ = conn.flush();
                        std::thread::sleep(Duration::from_millis(15));
                    }
                    at = to;
                }
                "press" => pointer.button(time(), true),
                "release" => pointer.button(time(), false),
                "key" => pointer.key(arg.parse().expect("an evdev key code")),
                "wait" => std::thread::sleep(Duration::from_millis(arg.parse().unwrap_or(100))),
                other => panic!("unknown step {other}"),
            }
            let _ = conn.flush();
            queue.roundtrip(&mut state).expect("roundtrip");
        }
        pointer.close();
        queue.roundtrip(&mut state).expect("roundtrip");
    }
}

#[cfg(target_os = "linux")]
mod shot {
    use std::os::fd::AsFd;
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::{
        wl_buffer::WlBuffer,
        wl_output::WlOutput,
        wl_registry::WlRegistry,
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
    };
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
    use wayland_protocols_wlr::screencopy::v1::client::{
        zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
        zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
    };

    #[derive(Default)]
    struct State {
        format: Option<(u32, u32, u32, u32)>,
        buffer_done: bool,
        ready: bool,
        failed: bool,
    }

    impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
        fn event(
            state: &mut State,
            _: &ZwlrScreencopyFrameV1,
            event: zwlr_screencopy_frame_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<State>,
        ) {
            match event {
                zwlr_screencopy_frame_v1::Event::Buffer { format: WEnum::Value(f), width, height, stride } => {
                    state.format = Some((f as u32, width, height, stride));
                }
                zwlr_screencopy_frame_v1::Event::BufferDone => state.buffer_done = true,
                zwlr_screencopy_frame_v1::Event::Ready { .. } => state.ready = true,
                zwlr_screencopy_frame_v1::Event::Failed => state.failed = true,
                _ => {}
            }
        }
    }
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
    macro_rules! ignore {
        ($($t:ty),*) => {$(
            impl Dispatch<$t, ()> for State {
                fn event(_: &mut State, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {}
            }
        )*};
    }
    ignore!(WlOutput, WlShm, WlShmPool, WlBuffer, ZwlrScreencopyManagerV1);

    pub fn run(args: Vec<String>) {
        let path = args.first().expect("shot FILE.png");
        let conn = Connection::connect_to_env().expect("no Wayland compositor");
        let (globals, mut queue) = registry_queue_init::<State>(&conn).expect("registry");
        let qh = queue.handle();
        let manager: ZwlrScreencopyManagerV1 =
            globals.bind(&qh, 1..=3, ()).expect("wlr-screencopy (sway has it, KWin doesn't)");
        let output: WlOutput = globals.bind(&qh, 1..=4, ()).expect("wl_output");
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
        let frame = manager.capture_output(1, &output, &qh, ());
        let mut state = State::default();
        while !state.buffer_done && !state.failed {
            queue.blocking_dispatch(&mut state).expect("dispatch");
        }
        let (format, w, h, stride) = state.format.expect("no buffer format");
        let size = (stride * h) as usize;
        let fd = rustix::fs::memfd_create("shot", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        rustix::fs::ftruncate(&fd, size as u64).unwrap();
        let pool = shm.create_pool(fd.as_fd(), size as i32, &qh, ());
        let buffer = pool.create_buffer(
            0,
            w as i32,
            h as i32,
            stride as i32,
            wl_shm::Format::try_from(format).unwrap(),
            &qh,
            (),
        );
        frame.copy(&buffer);
        while !state.ready && !state.failed {
            queue.blocking_dispatch(&mut state).expect("dispatch");
        }
        assert!(!state.failed, "the compositor refused the capture");
        let mut bytes = vec![0u8; size];
        rustix::io::pread(&fd, &mut bytes, 0).unwrap();
        // Xrgb8888 and Argb8888 are blue-first in memory.
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h as usize {
            for x in 0..w as usize {
                let p = y * stride as usize + x * 4;
                rgb.extend_from_slice(&[bytes[p + 2], bytes[p + 1], bytes[p]]);
            }
        }
        std::fs::write(path, png(w, h, &rgb)).unwrap();
        println!("saved {path} ({w}x{h})");
    }

    /// An uncompressed RGB PNG (stored deflate blocks).
    fn png(w: u32, h: u32, rgb: &[u8]) -> Vec<u8> {
        let mut raw = Vec::with_capacity(rgb.len() + h as usize);
        for row in rgb.chunks(w as usize * 3) {
            raw.push(0);
            raw.extend_from_slice(row);
        }
        let mut z = vec![0x78, 0x01];
        let mut chunks = raw.chunks(65535).peekable();
        while let Some(c) = chunks.next() {
            z.push(chunks.peek().is_none() as u8);
            z.extend_from_slice(&(c.len() as u16).to_le_bytes());
            z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
            z.extend_from_slice(c);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &v in &raw {
            a = (a + v as u32) % 65521;
            b = (b + a) % 65521;
        }
        z.extend_from_slice(&((b << 16) | a).to_be_bytes());
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        for (kind, data) in [(b"IHDR", ihdr), (b"IDAT", z), (b"IEND", Vec::new())] {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut body = kind.to_vec();
            body.extend_from_slice(&data);
            out.extend_from_slice(&body);
            out.extend_from_slice(&crc32(&body).to_be_bytes());
        }
        out
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &byte in data {
            crc ^= byte as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }
}
