//! Automatic upload of match reports (docs/stats-service.md). When the user has turned
//! `share_match_data` on, a background thread sends the unsent reports in `outbox\` to the stats
//! service after each result report is queued, and once shortly after startup for anything left
//! over. The thread never touches game state: it reads files, talks WinHTTP (the system's own
//! TLS and proxy settings, so no TLS stack is linked into the game). It does not use the host log
//! (that is for the main thread); it appends one line per upload to `logs\share.log`.
//!
//! One upload runs at a time; a request that arrives meanwhile is folded into the running one by
//! a second pass. Every failure is logged and ignored, since the files stay for the next try.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use w2r_abi::W2R_LOG_WARN;
use w2r_stats::identity;
use w2r_stats::share::{self, Error, Poster, Settings};

use crate::host::Log;
use crate::session::StartupLog;

/// The host waits this long after startup before the first upload, so the game finishes loading
/// without competing for the network.
const STARTUP_DELAY: Duration = Duration::from_secs(20);

/// `share_match_data`, cached for the plugin screen, which asks every frame.
static ENABLED: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);
static AGAIN: AtomicBool = AtomicBool::new(false);

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Reads `sharing.json` once at startup so the screen shows the real setting.
pub fn init(root: &Path) {
    if let Ok(s) = Settings::load(root) {
        ENABLED.store(s.share_match_data, Ordering::Relaxed);
    }
}

/// The user flipped the setting: save it and, when turned on, send what is waiting.
pub fn set_enabled(root: &Path, on: bool) -> Result<(), Error> {
    let mut settings = Settings::load(root)?;
    settings.share_match_data = on;
    settings.save(root)?;
    ENABLED.store(on, Ordering::Relaxed);
    if on {
        kick(root, false);
    }
    Ok(())
}

/// Asks for an upload. Cheap and non-blocking; does nothing when sharing is off.
pub fn kick(root: &Path, delay: bool) {
    // Skip the thread altogether for the common case of sharing being off.
    match Settings::load(root) {
        Ok(s) if s.share_match_data => {}
        Ok(_) => return,
        Err(e) => {
            StartupLog.line(W2R_LOG_WARN, "share", &format!("sharing.json unusable: {e}"));
            return;
        }
    }
    if RUNNING.swap(true, Ordering::AcqRel) {
        AGAIN.store(true, Ordering::Release);
        return;
    }
    let root: PathBuf = root.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("w2r-share".into())
        .spawn(move || {
            if delay {
                std::thread::sleep(STARTUP_DELAY);
            }
            loop {
                AGAIN.store(false, Ordering::Release);
                run_once(&root);
                if !AGAIN.load(Ordering::Acquire) {
                    break;
                }
            }
            RUNNING.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        RUNNING.store(false, Ordering::Release);
    }
}

fn run_once(root: &Path) {
    // Read again: the user may have turned sharing off since the kick.
    let Ok(settings) = Settings::load(root) else {
        return;
    };
    if !settings.share_match_data {
        return;
    }
    match share::upload(root, &settings, &WinHttp) {
        Ok(s) if s.sent + s.rejected + s.skipped == 0 => {}
        Ok(s) => note(
            root,
            &format!(
                "reports: {} sent, {} refused, {} skipped",
                s.sent, s.rejected, s.skipped
            ),
        ),
        Err(e) => note(root, &format!("reports: upload failed, will retry: {e}")),
    }
    // A replay still being recorded is not finished; `replayrec::end` asks again when it is.
    if recording() {
        return;
    }
    let identity = identity::load_or_create(&root.join("config").join("identity.json"), || None);
    let Ok(identity) = identity else { return };
    match w2r_replay::upload::upload(root, &settings, &identity, &WinHttp) {
        Ok(s) if s.sent + s.rejected + s.skipped == 0 => {}
        Ok(s) => note(
            root,
            &format!(
                "replays: {} sent, {} refused, {} skipped, {} waiting",
                s.sent, s.rejected, s.skipped, s.waiting
            ),
        ),
        Err(e) => note(root, &format!("replays: upload failed, will retry: {e}")),
    }
}

#[cfg(all(windows, target_arch = "x86"))]
fn recording() -> bool {
    crate::replayrec::recording()
}

#[cfg(not(all(windows, target_arch = "x86")))]
fn recording() -> bool {
    false
}

/// One line in `logs\share.log`.
fn note(root: &Path, line: &str) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let path = root.join("logs").join("share.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{secs} {line}");
    }
}

struct WinHttp;

impl Poster for WinHttp {
    fn post_json(&self, url: &str, body: &[u8]) -> Result<Vec<u8>, Error> {
        let (status, answer) = winhttp::request("POST", url, &[], body)?;
        if status != 200 {
            return Err(Error::new("network", format!("service answered {status}")));
        }
        Ok(answer)
    }

    fn put_bytes(
        &self,
        url: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> Result<(u16, Vec<u8>), Error> {
        winhttp::request("PUT", url, headers, body)
    }
}

/// `https://host[:port]/path` into its parts.
fn split_url(url: &str) -> Option<(&str, u16, &str)> {
    let rest = url.strip_prefix("https://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (authority, 443),
    };
    (!host.is_empty()).then_some((host, port, path))
}

#[cfg(windows)]
mod winhttp {
    use core::ffi::c_void;
    use core::ptr::{null, null_mut};

    use windows_sys::Win32::Networking::WinHttp::*;

    use super::{Error, split_url};

    const MAX_RESPONSE: usize = 64 * 1024;

    struct Handle(*mut c_void);

    impl Handle {
        fn new(raw: *mut c_void, what: &str) -> Result<Handle, Error> {
            if raw.is_null() {
                Err(Error::new("network", format!("{what} failed")))
            } else {
                Ok(Handle(raw))
            }
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// One request; any HTTP status is returned, only transport failures are errors.
    pub fn request(
        verb: &str,
        url: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> Result<(u16, Vec<u8>), Error> {
        let (host, port, path) =
            split_url(url).ok_or_else(|| Error::new("bad_url", url.to_owned()))?;
        let len = u32::try_from(body.len()).map_err(|_| Error::new("too_large", "body"))?;
        unsafe {
            let agent = wide(concat!("w2r-host/", env!("CARGO_PKG_VERSION")));
            let session = Handle::new(
                WinHttpOpen(
                    agent.as_ptr(),
                    WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
                    null(),
                    null(),
                    0,
                ),
                "WinHttpOpen",
            )?;
            // resolve, connect, send, receive: milliseconds
            WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 20_000);
            let host_w = wide(host);
            let connection = Handle::new(
                WinHttpConnect(session.0, host_w.as_ptr(), port, 0),
                "WinHttpConnect",
            )?;
            let verb = wide(verb);
            let path_w = wide(path);
            let request = Handle::new(
                WinHttpOpenRequest(
                    connection.0,
                    verb.as_ptr(),
                    path_w.as_ptr(),
                    null(),
                    null(),
                    null(),
                    WINHTTP_FLAG_SECURE,
                ),
                "WinHttpOpenRequest",
            )?;
            let mut header_text = String::from("content-type: application/json\r\n");
            for (name, value) in headers {
                // Names and values are ours (hex and numbers); drop anything that would end a line.
                if !name.contains(['\r', '\n', ':']) && !value.contains(['\r', '\n']) {
                    header_text.push_str(&format!("{name}: {value}\r\n"));
                }
            }
            let headers = wide(&header_text);
            if WinHttpSendRequest(
                request.0,
                headers.as_ptr(),
                u32::MAX,
                body.as_ptr().cast(),
                len,
                len,
                0,
            ) == 0
            {
                return Err(Error::new("network", "WinHttpSendRequest failed"));
            }
            if WinHttpReceiveResponse(request.0, null_mut()) == 0 {
                return Err(Error::new("network", "no response"));
            }
            let mut status: u32 = 0;
            let mut size = core::mem::size_of::<u32>() as u32;
            if WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                null(),
                (&raw mut status).cast(),
                &mut size,
                null_mut(),
            ) == 0
            {
                return Err(Error::new("network", "no status"));
            }
            let mut out = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let mut got: u32 = 0;
                if WinHttpReadData(request.0, chunk.as_mut_ptr().cast(), chunk.len() as u32, &mut got)
                    == 0
                {
                    return Err(Error::new("network", "read failed"));
                }
                if got == 0 {
                    break;
                }
                out.extend_from_slice(&chunk[..got as usize]);
                if out.len() > MAX_RESPONSE {
                    return Err(Error::new("network", "response too large"));
                }
            }
            Ok((status as u16, out))
        }
    }
}

#[cfg(not(windows))]
mod winhttp {
    use super::Error;

    pub fn request(
        _verb: &str,
        _url: &str,
        _headers: &[(&str, String)],
        _body: &[u8],
    ) -> Result<(u16, Vec<u8>), Error> {
        Err(Error::new("network", "no transport on this platform"))
    }
}

#[cfg(test)]
mod tests {
    use super::split_url;

    #[test]
    fn urls_split_into_host_port_and_path() {
        assert_eq!(
            split_url("https://a.example.workers.dev/v1/reports"),
            Some(("a.example.workers.dev", 443, "/v1/reports"))
        );
        assert_eq!(split_url("https://h:8443"), Some(("h", 8443, "/")));
        assert_eq!(split_url("http://h/x"), None);
        assert_eq!(split_url("https:///x"), None);
        assert_eq!(split_url("https://h:notaport/x"), None);
    }
}
