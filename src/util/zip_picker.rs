//! Asking the system for a ZIP of the game files, when the platform has a
//! picker.
//!
//! The startup-failure card's own button needs the platform's document picker;
//! a build without one installs no hook, so the request reports `false` and the
//! card says so. The answer comes back separately: Android copies the chosen
//! document to a real file and calls `app::game_import`'s entry point, which is
//! why this module carries no answer slot — unlike `folder_picker`, whose tree
//! identifier has to reach the Rust side before anything can read it.
//!
//! ## Dependency rules
//! - Part of util/: no dependencies on game modules.

use std::sync::OnceLock;

/// Opens the platform's ZIP picker. `true` means it opened.
static HOOK: OnceLock<fn() -> bool> = OnceLock::new();

/// Install the platform hook. The first install wins.
pub fn install(request: fn() -> bool) -> bool {
    HOOK.set(request).is_ok()
}

/// Ask the platform to show its ZIP picker.
///
/// `true` means the picker opened and its answer will arrive through the
/// platform's own path; `false` means this build has no picker (or the platform
/// refused to start one).
pub fn request() -> bool {
    HOOK.get().is_some_and(|request| (request)())
}
