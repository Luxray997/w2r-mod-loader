//! `DllMain` and host startup.
//!
//! `DllMain` does only what the loader lock allows: disable thread notifications, wire the
//! `version.dll` forwarders, and start a thread. That thread opens the log and runs the build
//! guard. Plugins are not loaded here: every plugin callback must run on the game's main thread
//! (docs/abi.md), so that happens from the first main-thread hook, which is a later Phase 2 step.

use core::ffi::c_void;
use std::path::PathBuf;
use std::sync::OnceLock;

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO, W2R_LOG_WARN};
use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::{DisableThreadLibraryCalls, GetModuleHandleW};

use crate::build_guard;
use crate::host::Log;
use crate::log::file::FileLog;
use crate::paths;
use crate::pe::Image;
use crate::symbols::Symbols;

const DLL_PROCESS_ATTACH: u32 = 1;

/// What startup established; the main-thread session reads it.
pub struct Startup {
    pub log: FileLog,
    /// The host root under Saved Games (docs/layout.md).
    pub root: PathBuf,
    pub game_base: usize,
    pub build_ok: bool,
    /// The game module's headers, when the build guard passed.
    pub image: Option<Image>,
}

static STARTUP: OnceLock<Startup> = OnceLock::new();

pub fn startup() -> Option<&'static Startup> {
    STARTUP.get()
}

// The signature is fixed by Windows; `instance` is only handed back to the loader, never read.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[unsafe(no_mangle)]
pub extern "system" fn DllMain(instance: HMODULE, reason: u32, _reserved: *mut c_void) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        unsafe { DisableThreadLibraryCalls(instance) };
        #[cfg(all(target_arch = "x86", target_env = "msvc"))]
        let proxy = crate::proxy::init();
        #[cfg(not(all(target_arch = "x86", target_env = "msvc")))]
        let proxy: Result<(), Vec<&'static str>> = Ok(());
        // Everything else runs outside the loader lock. A panic here must not reach the game,
        // and neither must a failure to create the thread (`thread::spawn` would panic).
        let _ = std::thread::Builder::new()
            .name("w2r-startup".into())
            .spawn(move || {
                let _ = std::panic::catch_unwind(move || run_startup(proxy));
            });
    }
    1
}

/// The only channel left when the log cannot be opened: a debugger or DebugView sees it.
fn debug_print(msg: &str) {
    use windows_sys::Win32::System::Diagnostics::Debug::OutputDebugStringW;
    let wide: Vec<u16> = format!("w2rpm host: {msg}\n")
        .encode_utf16()
        .chain(core::iter::once(0))
        .collect();
    unsafe { OutputDebugStringW(wide.as_ptr()) };
}

fn run_startup(proxy: Result<(), Vec<&'static str>>) {
    let Some(paths::Root { path: root, origin }) = paths::root() else {
        debug_print("no usable root folder (Saved Games unavailable?); inactive");
        return;
    };
    let logs = root.join("logs");
    // Another process may hold `w2r.log` exclusively; a per-process file beats silence.
    let (log, log_note) = match crate::log::file::open(&logs) {
        Ok(log) => (log, None),
        Err(e) => {
            let name = format!("w2r.{}.log", std::process::id());
            match crate::log::file::open_named(&logs, &name) {
                Ok(log) => (
                    log,
                    Some(format!("w2r.log is not writable ({e}); using {name}")),
                ),
                Err(e2) => {
                    debug_print(&format!(
                        "cannot open a log under {}: {e2}; inactive",
                        logs.display()
                    ));
                    return;
                }
            }
        }
    };
    let game_base = unsafe { GetModuleHandleW(core::ptr::null()) } as usize;

    log.line(
        W2R_LOG_INFO,
        "host",
        &format!(
            "=== w2rpm host {} attached, game base {game_base:#010x}, pid {} ===",
            env!("CARGO_PKG_VERSION"),
            std::process::id()
        ),
    );
    if let Some(note) = log_note {
        log.line(W2R_LOG_WARN, "host", &note);
    }
    let origin_text = match origin {
        paths::Origin::SavedGames => "Saved Games",
        paths::Origin::Override => "W2R_ROOT",
        paths::Origin::RelativeOverrideIgnored => "Saved Games; W2R_ROOT ignored, not absolute",
    };
    log.line(
        if origin == paths::Origin::RelativeOverrideIgnored {
            W2R_LOG_WARN
        } else {
            W2R_LOG_INFO
        },
        "host",
        &format!("root {} ({origin_text})", root.display()),
    );
    if let Err(missing) = proxy {
        log.line(
            W2R_LOG_WARN,
            "host",
            &format!(
                "version.dll proxy: could not resolve {} (calls to them would crash)",
                missing.join(", ")
            ),
        );
    }

    let report = build_guard::verify(game_base, &log);
    if !report.passed() {
        log.line(
            W2R_LOG_ERROR,
            "host",
            "build guard failed; the host is inactive this session",
        );
        let _ = STARTUP.set(Startup {
            log,
            root,
            game_base,
            build_ok: false,
            image: None,
        });
        return;
    }
    let image = Image::from_module(game_base);
    if image.is_none() {
        log.line(
            W2R_LOG_ERROR,
            "host",
            "cannot read the game's PE headers; inactive",
        );
    }
    let _ = STARTUP.set(Startup {
        log,
        root,
        game_base,
        build_ok: true,
        image,
    });

    // The frame hook is the host's way onto the main thread; everything plugin-related follows
    // from there (later steps).
    let startup = STARTUP.get().expect("just set");
    crate::watchdog::start(startup.root.join("logs"));
    if let Some(image) = &startup.image {
        crate::frame::install(image, &Symbols::new(game_base), &startup.log);
    }
}
