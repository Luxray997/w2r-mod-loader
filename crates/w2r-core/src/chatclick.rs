//! Mouse use of the lobby chat history: right-click copies a line, Ctrl+click opens the first
//! web link in it. Finding the link is plain text work; the clipboard and the browser are Windows.

/// The first `http://`, `https://` or `www.` link in `text`, as a URL a browser can open
/// (`www.` gets `https://`). Trailing punctuation is not part of the link.
pub fn first_url(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let mut best: Option<usize> = None;
    for prefix in ["http://", "https://", "www."] {
        let mut from = 0;
        while let Some(at) = lower[from..].find(prefix) {
            let at = from + at;
            let boundary = at == 0
                || !lower.as_bytes()[at - 1].is_ascii_alphanumeric();
            if boundary {
                best = Some(best.map_or(at, |b| b.min(at)));
                break;
            }
            from = at + prefix.len();
        }
    }
    let start = best?;
    let rest = &text[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c.is_control())
        .unwrap_or(rest.len());
    let link = rest[..end].trim_end_matches(|c: char| ".,;:!?)]}'\"".contains(c));
    let host_part = link
        .split_once("://")
        .map_or(link, |(_, after)| after);
    if host_part.len() < 4 || !host_part.contains('.') {
        return None;
    }
    Some(if link.to_ascii_lowercase().starts_with("www.") {
        format!("https://{link}")
    } else {
        link.to_owned()
    })
}

#[cfg(all(windows, target_arch = "x86"))]
pub use win::{copy_text, left_clicked, open_url, right_clicked, ctrl_down};

#[cfg(all(windows, target_arch = "x86"))]
mod win {
    use core::sync::atomic::{AtomicBool, Ordering};
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_LBUTTON, VK_RBUTTON,
    };
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    const CF_UNICODETEXT: u32 = 13;

    fn down(vk: u16) -> bool {
        // SAFETY: a plain key-state query.
        (unsafe { GetAsyncKeyState(vk as i32) }) as u16 & 0x8000 != 0
    }

    pub fn ctrl_down() -> bool {
        down(VK_CONTROL)
    }

    /// True once per press, while the game window has the focus.
    fn pressed(vk: u16, was_down: &AtomicBool) -> bool {
        let now = crate::replayplay::game_has_focus() && down(vk);
        let before = was_down.swap(now, Ordering::AcqRel);
        now && !before
    }

    /// Call once per frame; true on the frame the left button went down.
    pub fn left_clicked() -> bool {
        static WAS: AtomicBool = AtomicBool::new(false);
        pressed(VK_LBUTTON, &WAS)
    }

    /// Call once per frame; true on the frame the right button went down.
    pub fn right_clicked() -> bool {
        static WAS: AtomicBool = AtomicBool::new(false);
        pressed(VK_RBUTTON, &WAS)
    }

    /// Puts `text` on the clipboard; false when Windows refused.
    pub fn copy_text(text: &str) -> bool {
        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let bytes = wide.len() * 2;
        // SAFETY: the clipboard calls follow their documented order; the block is freed here
        // unless the clipboard took it.
        unsafe {
            if OpenClipboard(core::ptr::null_mut()) == 0 {
                return false;
            }
            EmptyClipboard();
            let mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
            let mut ok = false;
            if !mem.is_null() {
                let p = GlobalLock(mem).cast::<u16>();
                if !p.is_null() {
                    core::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
                    GlobalUnlock(mem);
                    ok = !SetClipboardData(CF_UNICODETEXT, mem).is_null();
                }
                if !ok {
                    GlobalFree(mem);
                }
            }
            CloseClipboard();
            ok
        }
    }

    /// Opens an `http(s)` link in the default browser; false for anything else.
    pub fn open_url(url: &str) -> bool {
        let lower = url.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return false;
        }
        let wide: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
        let verb: Vec<u16> = "open\0".encode_utf16().collect();
        // SAFETY: both strings are NUL-terminated and outlive the call.
        let r = unsafe {
            ShellExecuteW(
                core::ptr::null_mut(),
                verb.as_ptr(),
                wide.as_ptr(),
                core::ptr::null(),
                core::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        r as usize > 32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_links() {
        assert_eq!(
            first_url("see https://example.com/a?b=1, ok").as_deref(),
            Some("https://example.com/a?b=1")
        );
        assert_eq!(
            first_url("[x] (http://a.org/path).").as_deref(),
            Some("http://a.org/path")
        );
        assert_eq!(
            first_url("go to www.example.com now").as_deref(),
            Some("https://www.example.com")
        );
        assert_eq!(
            first_url("HTTPS://Example.com").as_deref(),
            Some("HTTPS://Example.com")
        );
    }

    #[test]
    fn ignores_non_links() {
        assert_eq!(first_url("no link here"), None);
        assert_eq!(first_url("https://"), None);
        assert_eq!(first_url("ftp://example.com"), None);
        assert_eq!(first_url("awww.example.com"), None);
        assert_eq!(first_url("http://localhost"), None);
    }

    #[test]
    fn earliest_link_wins() {
        assert_eq!(
            first_url("www.a.com then https://b.com").as_deref(),
            Some("https://www.a.com")
        );
    }
}
