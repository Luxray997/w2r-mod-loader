//! The egui manager. `jobs` and `settings` are UI-free and tested; `app` is the window.

pub mod app;
pub mod jobs;
pub mod settings;
pub mod theme;

/// Read by `mgrupdate::inspect` in a downloaded exe to learn its version; `#[used]` keeps it in
/// the binary.
#[used]
pub static VERSION_MARKER: &str = concat!("W2R-LOADER-VERSION:", env!("CARGO_PKG_VERSION"), ";");
