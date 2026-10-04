//! Hang diagnostics. The host's hooks record the stage they are in; a background thread notices
//! when no stage has changed for a while (the game's main thread is stuck or the game stopped
//! drawing), logs the last stage and writes one minidump. It never touches game state, only
//! atomics, so it cannot make a hang worse. Reading the result: a stage ending in `:game` means
//! the stall is inside a call into the game; `:host` means it is in our code.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_WARN};

use crate::host::Log;
use crate::session::StartupLog;

pub const IDLE: u32 = 0;
pub const FRAME_GAME: u32 = 1;
pub const FRAME_HOST: u32 = 2;
pub const PACKET_GAME: u32 = 3;
pub const PACKET_HOST: u32 = 4;
pub const SEND_GAME: u32 = 10;
pub const BROADCAST_GAME: u32 = 11;
pub const PEERS_GAME: u32 = 12;
pub const KICK_GAME: u32 = 13;

fn name(stage: u32) -> &'static str {
    match stage {
        IDLE => "idle",
        FRAME_GAME => "BeginNamedWindow:game",
        FRAME_HOST => "BeginNamedWindow:host",
        PACKET_GAME => "GetNextPeerPacket:game",
        PACKET_HOST => "GetNextPeerPacket:host",
        SEND_GAME => "transport:send_to:game",
        BROADCAST_GAME => "transport:broadcast:game",
        PEERS_GAME => "transport:enum_peers:game",
        KICK_GAME => "transport:kick:game",
        _ => "unknown",
    }
}

static STAGE: AtomicU32 = AtomicU32::new(IDLE);
/// Bumped on every `mark`; the monitor compares it between polls.
static PROGRESS: AtomicU64 = AtomicU64::new(0);
static STARTED: AtomicBool = AtomicBool::new(false);

/// Records that the calling thread is entering `stage`. Two relaxed stores: cheap enough for
/// every hook call. Only the main thread's hooks mark.
pub fn mark(stage: u32) {
    STAGE.store(stage, Ordering::Relaxed);
    PROGRESS.fetch_add(1, Ordering::Relaxed);
}

const POLL: Duration = Duration::from_millis(250);
/// Logged once per stall.
const WARN_AFTER_MS: u64 = 3000;
/// One dump per process, when the stall is clearly not a loading screen.
const DUMP_AFTER_MS: u64 = 8000;

/// Starts the monitor thread (once). `dump_dir` receives `hang-<unix seconds>.dmp`.
pub fn start(dump_dir: std::path::PathBuf) {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("w2r-watchdog".into())
        .spawn(move || monitor(dump_dir));
}

fn monitor(dump_dir: std::path::PathBuf) {
    let log = StartupLog;
    let mut last = PROGRESS.load(Ordering::Relaxed);
    let mut stalled_ms = 0u64;
    let mut warned = false;
    let mut dumped = false;
    loop {
        std::thread::sleep(POLL);
        let now = PROGRESS.load(Ordering::Relaxed);
        if now != last {
            if warned {
                log.line(
                    W2R_LOG_WARN,
                    "watchdog",
                    &format!("progress resumed after {stalled_ms} ms"),
                );
            }
            last = now;
            stalled_ms = 0;
            warned = false;
            continue;
        }
        // Nothing has run since the first frame: loading, or the game has no frames yet.
        if crate::frame::main_thread_id() == 0 {
            continue;
        }
        stalled_ms += POLL.as_millis() as u64;
        let stage = STAGE.load(Ordering::Relaxed);
        if !warned && stalled_ms >= WARN_AFTER_MS {
            warned = true;
            log.line(
                W2R_LOG_WARN,
                "watchdog",
                &format!(
                    "no host hook ran for {stalled_ms} ms; last stage: {}",
                    name(stage)
                ),
            );
        }
        if !dumped && stalled_ms >= DUMP_AFTER_MS {
            dumped = true;
            match write_dump(&dump_dir) {
                Ok(path) => log.line(
                    W2R_LOG_ERROR,
                    "watchdog",
                    &format!(
                        "stalled {stalled_ms} ms in {}; minidump written to {}",
                        name(stage),
                        path.display()
                    ),
                ),
                Err(e) => log.line(
                    W2R_LOG_ERROR,
                    "watchdog",
                    &format!("stalled in {}; minidump failed: {e}", name(stage)),
                ),
            }
        }
    }
}

fn write_dump(dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Diagnostics::Debug::{MiniDumpNormal, MiniDumpWriteDump};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId};

    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("hang-{secs}.dmp"));
    let file = std::fs::File::create(&path)?;
    let ok = unsafe {
        MiniDumpWriteDump(
            GetCurrentProcess(),
            GetCurrentProcessId(),
            file.as_raw_handle(),
            MiniDumpNormal,
            core::ptr::null(),
            core::ptr::null(),
            core::ptr::null(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(path)
}
