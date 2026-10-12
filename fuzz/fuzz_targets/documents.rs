//! PDF, Illustrator, EPS and PostScript (`crates/documents`): the saved
//! picture (XMP, page `/Thumb`, EPS previews), the page list and the first
//! page.  hayro draws in a forked child and Ghostscript is a separate
//! program; their faults are kept apart.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_documents::Picture;
use slerp_fuzz::{Target, finish_texture, head, note, split};
use std::sync::LazyLock;

static T: LazyLock<Target> =
    LazyLock::new(|| Target { name: "documents", exts: vec!["pdf", "ai", "eps", "epsi", "ps"] });

fn show(p: Picture, size: u32, limits: &slerp_scene::Limits) {
    match p {
        Picture::Image(tex) => finish_texture(&tex, size),
        Picture::Metafile { bytes, .. } => {
            if let Ok(tex) = slerp_metafile::load_sniffed(&bytes, limits) {
                finish_texture(&tex, size);
            }
        }
    }
}

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_documents::sniff(&path, head(input.body)) else { return };
    let limits = input.limits();
    let size = input.flags.size();
    let saved = slerp_documents::saved_picture(format, &path);
    note(data, &saved);
    if let Ok(Some(p)) = saved {
        show(p, size, &limits);
    }
    // Drawing a page starts hayro's child or Ghostscript: one input in
    // eight, picked by the flags, so the readers before it get the time.
    if input.flags.0 & 0x38 == 0x38 {
        let _ = slerp_documents::page_names(format, &path);
        let page = slerp_documents::load_path(format, &path, &limits);
        note(data, &page);
        if let Ok(p) = page {
            show(p, size, &limits);
        }
    }
});
