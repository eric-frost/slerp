//! A small window for trying drag-out by hand: it lists a generated PNG, a
//! text file and a 20 MB random file, the three together, and 2000 tiny files
//! (a long list, to see that hundreds of items go through), and pressing on
//! one and dragging it out of the window drops it as an ordinary file into
//! Dolphin, Explorer, Finder or any program that takes files.
//!
//! ```text
//! cargo run --release -p slerp-viewer --example dragout_demo
//! ```
//!
//! The terminal prints `materialized <name>` the moment a drop target
//! really asks for the data: nothing is written before that, and a drag
//! that is cancelled writes nothing.  On Wayland that moment comes when the
//! pointer first enters a window that reads the file list (Dolphin does so
//! while hovering), on Windows when the target reads the dropped files, on
//! macOS when Finder asks for each file.
//!
//! `--class <id>` sets the window's Wayland app id (for the headless tests).

use eframe::egui;
use slerp_viewer::dragout::{self, Drag, DragIcon, DragItem, Outcome};
use std::path::Path;
use std::time::Instant;

/// What a row offers.
#[derive(Clone, Copy)]
enum Kind {
    Png,
    Text,
    Random,
}

const ROWS: [(&str, Kind, &str); 3] = [
    ("demo picture.png", Kind::Png, "a generated 256 x 256 picture, about 200 KB"),
    ("notes #1 (50%).txt", Kind::Text, "a text file whose name has a space, '#' and '%'"),
    ("random 20 MB.bin", Kind::Random, "20 MB of pseudo-random bytes, made only when a target asks"),
];

struct Demo {
    prepared: bool,
    buttons_down: bool,
    status: String,
    started: Instant,
}

impl Demo {
    fn item(&self, name: &'static str, kind: Kind) -> DragItem {
        let started = self.started;
        DragItem::from_write(name, move |dest| {
            eprintln!("materialized {name} at {:.2} s -> {}", started.elapsed().as_secs_f64(), dest.display());
            write_sample(dest, kind)
        })
    }
}

impl eframe::App for Demo {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if !self.prepared {
            self.prepared = true;
            if let Err(e) = dragout::prepare(frame, ui.ctx()) {
                self.status = e.to_string();
            }
        }
        if let Some(outcome) = dragout::take_outcome() {
            self.status = match outcome {
                Outcome::Dropped => "The drop was accepted.".into(),
                Outcome::Cancelled => "The drag was cancelled.".into(),
            };
            eprintln!("drag ended: {outcome:?}");
        }

        // After a drag, egui must not think a button is still down.
        let down = ui.input(|i| i.pointer.any_down());
        if down != self.buttons_down {
            self.buttons_down = down;
            eprintln!("buttons down: {down}");
        }

        let mut drag: Option<(Vec<DragItem>, [u8; 3])> = None;
        ui.heading("Slerp drag-out demo");
        ui.label("Press on a row and drag it out of this window into a folder.");
        ui.add_space(8.0);
        for (i, (name, kind, note)) in ROWS.iter().enumerate() {
            let response = row(ui, name, note, i);
            if response.drag_started() {
                drag = Some((vec![self.item(name, *kind)], COLORS[i]));
            }
        }
        let response = row(ui, "all three at once", "three files in one drag", 3);
        if response.drag_started() {
            let items = ROWS.iter().map(|(name, kind, _)| self.item(name, *kind)).collect();
            drag = Some((items, COLORS[3]));
        }
        let response = row(ui, "2000 tiny files", "a long list: each file is a few bytes", 4);
        if response.drag_started() {
            let announced = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let items = (1..=2000)
                .map(|n| {
                    let announced = announced.clone();
                    DragItem::from_write(format!("tiny {n:04}.txt"), move |dest| {
                        if !announced.swap(true, std::sync::atomic::Ordering::SeqCst) {
                            eprintln!("materialized 2000 tiny files (first: {})", dest.display());
                        }
                        std::fs::write(dest, format!("{n}\n"))
                    })
                })
                .collect();
            drag = Some((items, COLORS[4]));
        }
        ui.add_space(8.0);
        ui.label(&self.status);

        if let Some((items, color)) = drag {
            let count = items.len();
            match dragout::start(frame, Drag::new(items).with_icon(icon(color, count))) {
                Ok(()) => {
                    self.status = "Dragging...".into();
                    eprintln!("drag started ({count} item{})", if count == 1 { "" } else { "s" });
                }
                Err(e) => {
                    self.status = e.to_string();
                    eprintln!("drag failed: {e}");
                }
            }
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        raw_input.events.extend(dragout::take_pointer_release(ctx));
    }
}

const COLORS: [[u8; 3]; 5] = [[70, 130, 220], [90, 170, 90], [200, 120, 60], [150, 90, 190], [190, 80, 100]];

/// One draggable row.
fn row(ui: &mut egui::Ui, name: &str, note: &str, index: usize) -> egui::Response {
    let size = egui::vec2(ui.available_width(), 56.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let fill =
        if response.hovered() { ui.visuals().widgets.hovered.bg_fill } else { ui.visuals().widgets.inactive.bg_fill };
    ui.painter().rect_filled(rect, 6.0, fill);
    let [r, g, b] = COLORS[index];
    ui.painter().rect_filled(
        egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 8.0), egui::vec2(40.0, 40.0)),
        6.0,
        egui::Color32::from_rgb(r, g, b),
    );
    let text = ui.visuals().text_color();
    ui.painter().text(
        rect.min + egui::vec2(60.0, 12.0),
        egui::Align2::LEFT_TOP,
        name,
        egui::FontId::proportional(16.0),
        text,
    );
    ui.painter().text(
        rect.min + egui::vec2(60.0, 33.0),
        egui::Align2::LEFT_TOP,
        note,
        egui::FontId::proportional(12.0),
        ui.visuals().weak_text_color(),
    );
    if index == 0 && ui.ctx().cumulative_frame_nr() == 1 {
        // The first row's place, for tests that drive the pointer.
        eprintln!("first row at {:?}", rect);
    }
    response
}

/// The picture dragged along with the pointer: a coloured tile, with a bar
/// for each file in the drag.
fn icon([r, g, b]: [u8; 3], count: usize) -> DragIcon {
    let (w, h) = (48u32, 48u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let inside = x >= 2 && x < w - 2 && y >= 2 && y < h - 2;
            let bar = (0..count.min(4)).any(|i| y >= 8 + 9 * i as u32 && y < 14 + 9 * i as u32 && x >= 8 && x < w - 8);
            let px = if bar {
                [255, 255, 255, 235]
            } else if inside {
                [r, g, b, 220]
            } else {
                [0, 0, 0, 0]
            };
            let at = ((y * w + x) * 4) as usize;
            rgba[at..at + 4].copy_from_slice(&px);
        }
    }
    DragIcon { width: w, height: h, rgba }
}

fn write_sample(dest: &Path, kind: Kind) -> std::io::Result<()> {
    match kind {
        Kind::Png => std::fs::write(dest, sample_png(256, 256)),
        Kind::Text => std::fs::write(dest, "Dropped out of the Slerp drag-out demo.\nHello from the window.\n"),
        Kind::Random => {
            use std::io::Write;
            let mut out = std::io::BufWriter::new(std::fs::File::create(dest)?);
            let mut x = 0x9E37_79B9_7F4A_7C15u64;
            let mut chunk = vec![0u8; 1 << 16];
            for _ in 0..(20 << 20) / chunk.len() {
                for b in chunk.chunks_exact_mut(8) {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    b.copy_from_slice(&x.to_le_bytes());
                }
                out.write_all(&chunk)?;
            }
            out.flush()
        }
    }
}

/// An uncompressed RGB PNG of a colour gradient (stored deflate blocks), so
/// the demo needs no image library.
fn sample_png(w: u32, h: u32) -> Vec<u8> {
    let mut raw = Vec::new();
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            raw.extend_from_slice(&[(x * 255 / w) as u8, (y * 255 / h) as u8, 160]);
        }
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
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    for (kind, data) in [(b"IHDR", ihdr), (b"IDAT", z), (b"IEND", Vec::new())] {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(&data);
        png.extend_from_slice(&body);
        png.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    png
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

fn main() -> eframe::Result {
    let mut args = std::env::args().skip(1);
    let mut app_id = "slerp-dragout-demo".to_string();
    while let Some(arg) = args.next() {
        if arg == "--class" {
            app_id = args.next().unwrap_or(app_id);
        }
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([460.0, 400.0]).with_app_id(app_id),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Slerp drag-out demo",
        options,
        Box::new(|_| {
            Ok(Box::new(Demo { prepared: false, buttons_down: false, status: String::new(), started: Instant::now() }))
        }),
    );
    dragout::shutdown();
    result
}
