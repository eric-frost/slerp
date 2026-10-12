//! The viewer's testing hooks (`SLERP_SCREENSHOT`, `SLERP_INPUT` and the
//! rest, listed in the crate documentation) come from the environment of
//! the `slerp view` command that opened the window.  A warm viewer opens
//! windows for other commands, so for those windows the variables come from
//! the command's request instead of this process's own environment.

use std::sync::RwLock;

/// The variables of the command that opened the current window, when it
/// wasn't this process's own.
static WINDOW_ENV: RwLock<Option<Vec<(String, String)>>> = RwLock::new(None);

/// A hook variable as the window's command set it.
pub fn var(name: &str) -> Option<String> {
    match &*WINDOW_ENV.read().unwrap_or_else(|e| e.into_inner()) {
        Some(vars) => vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()),
        None => std::env::var(name).ok(),
    }
}

/// Whether a hook variable is set (to anything).
pub fn is_set(name: &str) -> bool {
    var(name).is_some()
}

/// Makes `vars` the variables of the windows opened from now on.
pub fn set_window_env(vars: Vec<(String, String)>) {
    *WINDOW_ENV.write().unwrap_or_else(|e| e.into_inner()) = Some(vars);
}

/// The variables a `slerp view` command passes to a warm viewer: the
/// hooks, whose names start with `SLERP_`.
pub fn hooks_of_this_process() -> Vec<(String, String)> {
    std::env::vars().filter(|(k, _)| k.starts_with("SLERP_")).collect()
}
