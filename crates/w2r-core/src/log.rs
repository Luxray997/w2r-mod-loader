//! The host log: `logs\w2r.log` under the w2r root (docs/layout.md). One line per entry,
//! `[HH:MM:SS.mmm] LEVEL tag: message`. Shared by the host and, through [`Log`], every plugin.
//!
//! Limits, ported from wc2r-plus `core/log.cpp` (MIT): a session writes at most 20 MiB (one
//! notice, then silence until the next launch), and a file over 5 MiB at startup is rotated to
//! `w2r.1.log`. Control characters in messages become `?` so a plugin cannot forge lines.
//! Per-plugin throttling is a Phase 3 item.

use std::io::Write;
use std::sync::Mutex;

use w2r_abi::{W2R_LOG_DEBUG, W2R_LOG_ERROR, W2R_LOG_INFO, W2R_LOG_WARN};

use crate::host::Log;

pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
pub const MAX_SESSION_BYTES: usize = 20 * 1024 * 1024;
pub const FILE_NAME: &str = "w2r.log";
pub const ROTATED_NAME: &str = "w2r.1.log";
const CAP_NOTICE: &str =
    "*** log limit for this session reached; nothing more is written until the next launch ***\n";

pub fn level_name(level: u32) -> &'static str {
    match level {
        W2R_LOG_DEBUG => "DEBUG",
        W2R_LOG_INFO => "INFO ",
        W2R_LOG_WARN => "WARN ",
        W2R_LOG_ERROR => "ERROR",
        _ => "?????",
    }
}

struct State<W> {
    sink: W,
    written: usize,
    capped: bool,
}

/// A line writer with the session cap. `W` is a `File` in the game and a `Vec<u8>` in tests.
pub struct Writer<W: Write> {
    state: Mutex<State<W>>,
}

impl<W: Write> Writer<W> {
    pub fn new(sink: W) -> Self {
        Writer {
            state: Mutex::new(State {
                sink,
                written: 0,
                capped: false,
            }),
        }
    }

    /// Writes one formatted line; `clock` is the timestamp text.
    pub fn write_line(&self, clock: &str, level: u32, tag: &str, msg: &str) {
        let line = format!("[{clock}] {} {tag}: {}\n", level_name(level), sanitise(msg));
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.capped {
            return;
        }
        if s.written + line.len() > MAX_SESSION_BYTES {
            s.capped = true;
            let _ = s.sink.write_all(CAP_NOTICE.as_bytes());
            let _ = s.sink.flush();
            return;
        }
        s.written += line.len();
        let _ = s.sink.write_all(line.as_bytes());
        let _ = s.sink.flush();
    }

    pub fn into_inner(self) -> W {
        self.state
            .into_inner()
            .unwrap_or_else(|e| e.into_inner())
            .sink
    }
}

impl<W: Write> Log for Writer<W> {
    fn line(&self, level: u32, plugin: &str, msg: &str) {
        self.write_line(&clock(), level, plugin, msg);
    }
}

/// Control characters, line/paragraph separators and invisible format characters (bidi
/// overrides, zero-width joiners, BOM) become `?`: none of them belongs in a log line, and the
/// bidi ones can make a viewer display a plugin's text as part of a host line.
fn sanitise(msg: &str) -> String {
    msg.chars()
        .map(|c| if is_invisible(c) { '?' } else { c })
        .collect()
}

fn is_invisible(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{061C}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{E0000}'..='\u{E007F}'
        )
}

#[cfg(windows)]
fn clock() -> String {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    let mut t = unsafe { core::mem::zeroed::<windows_sys::Win32::Foundation::SYSTEMTIME>() };
    unsafe { GetLocalTime(&mut t) };
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

#[cfg(not(windows))]
fn clock() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let s = (ms / 1000) % 86_400;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        s / 3600,
        (s / 60) % 60,
        s % 60,
        ms % 1000
    )
}

/// The real log file, with rotation. Fully shared: a viewer can tail it, a second game instance
/// (the usual way to test a lobby on one machine) appends to the same file, and neither blocks
/// the other's rotation. Appends are atomic per line, so interleaved sessions stay readable.
#[cfg(windows)]
pub mod file {
    use super::*;
    use std::fs::File;
    use std::io;
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::Path;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    pub type FileLog = Writer<File>;

    const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

    /// Opens `<logs_dir>\w2r.log` for appending, rotating it first if it is over the size cap.
    pub fn open(logs_dir: &Path) -> io::Result<FileLog> {
        let path = logs_dir.join(FILE_NAME);
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > MAX_FILE_BYTES {
                // A viewer that holds the file without delete sharing makes the rename fail;
                // then truncate in place so the file still cannot grow without bound. If that
                // fails too (no write sharing either), appending is all that is left.
                if std::fs::rename(&path, logs_dir.join(ROTATED_NAME)).is_err() {
                    let _ = File::options()
                        .write(true)
                        .truncate(true)
                        .share_mode(SHARE_ALL)
                        .open(&path);
                }
            }
        }
        open_named(logs_dir, FILE_NAME)
    }

    /// Opens `<logs_dir>\<name>` for appending, without rotation.
    pub fn open_named(logs_dir: &Path, name: &str) -> io::Result<FileLog> {
        let file = File::options()
            .append(true)
            .create(true)
            .share_mode(SHARE_ALL)
            .open(logs_dir.join(name))?;
        Ok(Writer::new(file))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(w: Writer<Vec<u8>>) -> String {
        String::from_utf8(w.into_inner()).unwrap()
    }

    #[test]
    fn lines_have_clock_level_tag_and_message() {
        let w = Writer::new(Vec::new());
        w.write_line("12:34:56.789", W2R_LOG_INFO, "hello", "event 1");
        w.write_line(
            "12:34:56.790",
            W2R_LOG_ERROR,
            "crash",
            "disabled: on_event(1): access violation",
        );
        assert_eq!(
            text(w),
            "[12:34:56.789] INFO  hello: event 1\n\
             [12:34:56.790] ERROR crash: disabled: on_event(1): access violation\n"
        );
    }

    #[test]
    fn control_characters_cannot_forge_lines() {
        let w = Writer::new(Vec::new());
        w.write_line("t", W2R_LOG_INFO, "p", "a\nb\r\x1b[31mc\x7f");
        assert_eq!(text(w), "[t] INFO  p: a?b??[31mc?\n");
    }

    #[test]
    fn separators_and_bidi_controls_cannot_reorder_lines() {
        let w = Writer::new(Vec::new());
        w.write_line(
            "t",
            W2R_LOG_INFO,
            "p",
            "a\u{2028}b\u{202e}c\u{200b}d\u{feff}e\u{2066}f é",
        );
        assert_eq!(text(w), "[t] INFO  p: a?b?c?d?e?f é\n");
    }

    #[test]
    fn session_cap_writes_one_notice_then_nothing() {
        let w = Writer::new(Vec::new());
        let big = "x".repeat(1024 * 1024);
        for _ in 0..19 {
            w.write_line("t", W2R_LOG_INFO, "p", &big);
        }
        let before = w.state.lock().unwrap().written;
        assert!(before > 19 * 1024 * 1024);
        w.write_line("t", W2R_LOG_INFO, "p", &big); // this one does not fit
        w.write_line("t", W2R_LOG_INFO, "p", "after the cap");
        let out = text(w);
        assert_eq!(out.matches(CAP_NOTICE.trim_end()).count(), 1);
        assert!(out.ends_with(CAP_NOTICE));
        assert!(!out.contains("after the cap"));
    }

    #[test]
    fn the_log_trait_adds_a_clock() {
        let w = Writer::new(Vec::new());
        w.line(W2R_LOG_WARN, "p", "m");
        let out = text(w);
        assert!(out.starts_with('['), "{out}");
        assert!(out.ends_with("] WARN  p: m\n"), "{out}");
        assert_eq!(out.len(), "[00:00:00.000] WARN  p: m\n".len());
    }

    #[cfg(windows)]
    #[test]
    fn file_log_rotates_a_big_file_and_appends() {
        let dir = std::env::temp_dir().join(format!("w2r-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, vec![b'x'; MAX_FILE_BYTES as usize + 1]).unwrap();

        let log = file::open(&dir).unwrap();
        log.line(W2R_LOG_INFO, "host", "fresh");
        drop(log);
        assert_eq!(
            std::fs::metadata(dir.join(ROTATED_NAME)).unwrap().len(),
            MAX_FILE_BYTES + 1
        );
        let fresh = std::fs::read_to_string(&path).unwrap();
        assert!(fresh.ends_with("INFO  host: fresh\n"), "{fresh}");

        let log = file::open(&dir).unwrap();
        log.line(W2R_LOG_INFO, "host", "again");
        drop(log);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
