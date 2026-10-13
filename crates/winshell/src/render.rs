//! Making a thumbnail from a file, exactly as `slerp thumbnail` does,
//! with one GPU renderer shared by everything in the process.  The
//! thumbnail helper runs this for Explorer; Explorer's isolated thumbnail
//! process runs it itself.

use crate::protocol::Reply;
use slerp_core::{Config, Prepared, ThumbError, ThumbRenderer};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Mutex;

/// Thumbnails larger than this aren't thumbnails; the CLI clamps the same.
pub const MAX_SIZE: u32 = 2048;

/// The GPU renderer, created on the first 3D thumbnail and kept, so only
/// the first one in the process pays for starting the GPU.
static RENDERER: Mutex<Option<ThumbRenderer>> = Mutex::new(None);

/// Loads `path` and makes its thumbnail.  Loading runs in parallel on the
/// caller's threads; only drawing takes the renderer.  Never panics: a
/// panic anywhere becomes a failure reply.
pub fn thumbnail(path: &Path, size: u32) -> Reply {
    use_slerp_exe();
    let size = size.clamp(16, MAX_SIZE);
    match catch_unwind(AssertUnwindSafe(|| make(path, size))) {
        Ok(reply) => reply,
        Err(panic) => Reply::Failed(format!("panicked: {}", panic_message(&panic))),
    }
}

/// Has the decoders that run isolated run in the `slerp.exe` beside this
/// DLL.  The DLL carries its own copy of Slerp's crates, so what
/// `slerp.exe`'s `main` set up doesn't reach it, even inside `slerp.exe
/// helper`; and in Explorer's isolated thumbnail process there is no
/// `slerp.exe` at all.  Without the program the decoders run on a thread,
/// where a crash would end this process.
fn use_slerp_exe() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Ok(exe) = crate::this_dll().map(|dll| dll.with_file_name("slerp.exe")) else { return };
        if exe.is_file() {
            slerp_isolate::use_program(exe, slerp_core::ISOLATED_JOBS);
        } else {
            crate::log::log!("{} is missing, so decoders run unisolated", exe.display());
        }
    });
}

fn make(path: &Path, size: u32) -> Reply {
    // Explorer asks for every file of a registered extension; formats the
    // user switched off (one of two sharing `.art`, say) are declined, so
    // Explorer shows its icon.
    if !slerp_core::choices::thumbnails_wanted_for(path, None) {
        return Reply::Unknown;
    }
    let scene = match slerp_core::prepare_thumbnail(path, size) {
        Ok(Prepared::Done(image)) => return image_reply(image),
        Ok(Prepared::Render(scene)) => scene,
        Err(ThumbError::Unknown) => return Reply::Unknown,
        Err(e) => return Reply::Failed(e.to_string()),
    };
    let mut guard = RENDERER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let renderer = guard.get_or_insert_with(|| ThumbRenderer::new(&Config::load().gpu));
    match catch_unwind(AssertUnwindSafe(|| renderer.render_thumbnail(&scene, size))) {
        Ok(Ok(image)) => image_reply(image),
        Ok(Err(e)) => Reply::Failed(e.to_string()),
        Err(panic) => {
            // The renderer may have stopped half-way through a frame; the
            // next thumbnail starts a fresh one.
            *guard = None;
            Reply::Failed(format!("renderer panicked: {}", panic_message(&panic)))
        }
    }
}

fn image_reply(image: slerp_core::scene::Image8) -> Reply {
    Reply::Image { width: image.width, height: image.height, rgba: image.pixels }
}

pub fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    match (panic.downcast_ref::<&str>(), panic.downcast_ref::<String>()) {
        (Some(s), _) => (*s).to_owned(),
        (_, Some(s)) => s.clone(),
        _ => "unknown panic".into(),
    }
}
