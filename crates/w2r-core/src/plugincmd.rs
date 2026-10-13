//! The `/plugins` chat command: a player types it to tell the room which plugins they run. Nothing
//! is requested or sent without them doing so (most players will not run the same plugins, so the
//! host does not check). The text of the command is replaced by the list and goes out as an
//! ordinary chat line, in the lobby (`MpLobbySendChat`) and in a match (`ChatSendLocal`, see
//! `chatrec.rs`). x86 only for the hook; the formatting is plain.

/// The lobby's chat line is at most this long (`chat::valid_text`).
pub const MAX_LINE: usize = w2r_abi::W2R_MAX_CHAT_BYTES as usize;

/// Developer-only plugins (marked `dev_only` in the w2r-plugins repository, which does not publish
/// them). They are never named in chat: they are research tools, and a player who has one
/// installed should not have it announced. The manifest cannot carry the flag (its schema is
/// strict), so the host keeps the ids.
pub const DEV_ONLY: &[&str] = &["w2r-lobby-log", "w2r-sdk-probe"];

pub fn is_dev_only(id: &str) -> bool {
    DEV_ONLY.contains(&id)
}

/// True for `/plugins` on its own (any case, surrounding spaces ignored).
pub fn is_command(text: &[u8]) -> bool {
    text.trim_ascii().eq_ignore_ascii_case(b"/plugins")
}

/// "Plugins: a 1.0.0, b 0.2.0", cut to `MAX_LINE` bytes with "+N more" when
/// the list is long. `entries` are "id version" strings of the plugins that are running.
pub fn line(entries: &[String]) -> String {
    let head = "Plugins: ".to_owned();
    if entries.is_empty() {
        return format!("{head}none");
    }
    let mut out = head;
    for (i, entry) in entries.iter().enumerate() {
        let rest = entries.len() - i - 1;
        let more = if rest > 0 {
            format!(", +{rest} more")
        } else {
            String::new()
        };
        let sep = if i == 0 { "" } else { ", " };
        if out.len() + sep.len() + entry.len() + more.len() > MAX_LINE {
            out.push_str(&format!("{sep}+{} more", entries.len() - i));
            break;
        }
        out.push_str(sep);
        out.push_str(entry);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_is_recognised_alone_only() {
        assert!(is_command(b"/plugins"));
        assert!(is_command(b"  /Plugins "));
        assert!(!is_command(b"/plugins now"));
        assert!(!is_command(b"plugins"));
        assert!(!is_command(b""));
    }

    #[test]
    fn developer_plugins_are_known_by_id() {
        assert!(is_dev_only("w2r-lobby-log") && is_dev_only("w2r-sdk-probe"));
        assert!(!is_dev_only("w2r-chat"));
    }

    #[test]
    fn the_line_lists_plugins_and_cuts_a_long_list() {
        assert_eq!(line(&[]), "Plugins: none");
        let one = vec!["w2r-chat 1.0.0".to_owned(), "w2r-observer 0.1.0".to_owned()];
        assert_eq!(
            line(&one),
            "Plugins: w2r-chat 1.0.0, w2r-observer 0.1.0"
        );
        let many: Vec<String> = (0..40).map(|i| format!("plugin-number-{i} 1.0.0")).collect();
        let l = line(&many);
        assert!(l.len() <= MAX_LINE, "{}", l.len());
        assert!(l.ends_with(" more"), "{l}");
        assert!(crate::chat::valid_text(l.as_bytes()));
    }
}

#[cfg(all(windows, target_arch = "x86"))]
pub use game::install;

#[cfg(all(windows, target_arch = "x86"))]
mod game {
    use core::ffi::{c_char, c_void};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

    use crate::guard::guarded;
    use crate::hooks::{self, Owner};
    use crate::host::Log;
    use crate::pe::Image;
    use crate::symbols::Symbols;

    const STR_SIZE: usize = 0x10;
    const STR_CAPACITY: usize = 0x14;
    const STR_SSO_MAX: u32 = 15;

    type StringAssignFn =
        unsafe extern "thiscall" fn(*mut c_void, *const c_char, usize) -> *mut c_void;
    type SendChatFn = unsafe extern "C" fn();

    static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
    static INPUT: AtomicUsize = AtomicUsize::new(0);
    static ASSIGN: AtomicUsize = AtomicUsize::new(0);

    /// Hooks the lobby's send-chat handler so `/plugins` is swapped for the list.
    pub fn install(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
        let (Some(target), Some(input), Some(assign)) = (
            symbols.address("MpLobbySendChat"),
            symbols.address("MpLobbyChatInput"),
            symbols.address("StringAssign"),
        ) else {
            log.line(W2R_LOG_ERROR, "host", "/plugins: symbols missing");
            return false;
        };
        INPUT.store(input, Ordering::Release);
        ASSIGN.store(assign, Ordering::Release);
        // SAFETY: `detour` matches MpLobbySendChat's cdecl `void()` signature.
        let result = unsafe {
            hooks::global().install(
                Owner::Host,
                image,
                target,
                detour as *mut c_void,
                "host:MpLobbySendChat",
                |o| ORIGINAL.store(o as usize, Ordering::Release),
            )
        };
        match result {
            Ok(_) => {
                log.line(
                    W2R_LOG_INFO,
                    "host",
                    &format!("hooked MpLobbySendChat @ {target:#010x} (/plugins)"),
                );
                true
            }
            Err(e) => {
                log.line(
                    W2R_LOG_ERROR,
                    "host",
                    &format!("could not hook MpLobbySendChat: {e:?}"),
                );
                false
            }
        }
    }

    unsafe extern "C" fn detour() {
        let original: SendChatFn = unsafe { core::mem::transmute(ORIGINAL.load(Ordering::Acquire)) };
        let _ = guarded(|| unsafe { replace_command() });
        unsafe { original() };
    }

    /// If the chat box holds `/plugins`, writes the list into it.
    ///
    /// # Safety
    /// Reads and writes the game's chat input string through its symbol addresses.
    unsafe fn replace_command() {
        let input = INPUT.load(Ordering::Acquire) as *mut u8;
        let size = unsafe { input.add(STR_SIZE).cast::<u32>().read_unaligned() } as usize;
        if size == 0 || size > 64 {
            return;
        }
        let capacity = unsafe { input.add(STR_CAPACITY).cast::<u32>().read_unaligned() };
        let data = if capacity > STR_SSO_MAX {
            unsafe { input.cast::<*const u8>().read_unaligned() }
        } else {
            input as *const u8
        };
        if !super::is_command(unsafe { core::slice::from_raw_parts(data, size) }) {
            return;
        }
        let Some(text) = crate::session::plugins_chat_line() else {
            return;
        };
        let assign: StringAssignFn =
            unsafe { core::mem::transmute(ASSIGN.load(Ordering::Acquire)) };
        unsafe { assign(input.cast(), text.as_ptr().cast(), text.len()) };
    }
}
