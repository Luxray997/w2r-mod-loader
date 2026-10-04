//! `chat_send` (capability `chat.write`, docs/abi.md): at most 200 bytes of printable ASCII,
//! at most 4 lines per 5 s per plugin, only while the game's chat is available. The game side
//! is behind [`Chat`] so the rules are testable without the game; in the lobby the real
//! implementation writes the game's chat input and presses send, as wc2r-plus does.

pub const MAX_PER_WINDOW: usize = 4;
pub const WINDOW_MS: u64 = 5_000;

/// What the host can do with the game's chat right now.
pub trait Chat {
    fn available(&self) -> bool;
    /// Sends one validated line; false when the game refused.
    fn send(&self, text: &str) -> bool;
}

/// No game (tests, tooling): chat is never available.
pub struct NoChat;

impl Chat for NoChat {
    fn available(&self) -> bool {
        false
    }
    fn send(&self, _: &str) -> bool {
        false
    }
}

/// 1 to 200 bytes, every one printable ASCII (0x20..=0x7E).
pub fn valid_text(bytes: &[u8]) -> bool {
    (1..=w2r_abi::W2R_MAX_CHAT_BYTES as usize).contains(&bytes.len())
        && bytes.iter().all(|b| (0x20..=0x7E).contains(b))
}

/// Milliseconds from an arbitrary origin; only differences matter.
#[cfg(windows)]
pub fn now_ms() -> u64 {
    unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() }
}

#[cfg(not(windows))]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The real thing: the multiplayer lobby's chat box. x86 only, like the game.
#[cfg(all(windows, target_arch = "x86"))]
pub mod game {
    use super::*;
    use crate::guard::guarded;
    use crate::lobby::Lobby;
    use crate::symbols::Symbols;
    use core::ffi::{c_char, c_void};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// MSVC `std::string::assign(const char*, size_t)`; `StringAssign` in the symbol table.
    type StringAssignFn =
        unsafe extern "thiscall" fn(*mut c_void, *const c_char, usize) -> *mut c_void;
    /// The lobby's send-chat handler; reads and clears the input box.
    type SendChatFn = unsafe extern "C" fn();

    pub struct LobbyChat {
        input: *mut c_void,
        assign: StringAssignFn,
        send: SendChatFn,
        lobby: Rc<RefCell<Lobby>>,
    }

    impl LobbyChat {
        pub fn new(symbols: &Symbols, lobby: Rc<RefCell<Lobby>>) -> Option<LobbyChat> {
            let input = symbols.address("MpLobbyChatInput")? as *mut c_void;
            let assign: StringAssignFn =
                unsafe { core::mem::transmute(symbols.address("StringAssign")?) };
            let send: SendChatFn =
                unsafe { core::mem::transmute(symbols.address("MpLobbySendChat")?) };
            Some(LobbyChat {
                input,
                assign,
                send,
                lobby,
            })
        }
    }

    impl Chat for LobbyChat {
        fn available(&self) -> bool {
            self.lobby.borrow().in_lobby(now_ms())
        }

        fn send(&self, text: &str) -> bool {
            // Game code under the guard: a fault here is logged as a refused send, not a crash.
            guarded(|| unsafe {
                (self.assign)(self.input, text.as_ptr().cast(), text.len());
                (self.send)();
            })
            .is_ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_rules() {
        assert!(valid_text(b"hello"));
        assert!(valid_text(b" ~"));
        assert!(valid_text(&[b'x'; 200]));
        assert!(!valid_text(&[b'x'; 201]));
        assert!(!valid_text(b""));
        assert!(!valid_text(b"tab\there"));
        assert!(!valid_text("caf\u{e9}".as_bytes()));
        assert!(!valid_text(b"nl\n"));
        assert!(!valid_text(&[0x7F]));
    }

    /// The host side of `chat_send`, against a fake game chat.
    #[test]
    fn chat_send_enforces_capability_text_availability_and_rate() {
        use crate::dispatch::Registry;
        use crate::host::{Log, Services, caps};
        use std::cell::{Cell, RefCell};
        use std::rc::Rc;
        use w2r_abi::*;

        #[derive(Default)]
        struct Fake {
            available: Cell<bool>,
            sent: RefCell<Vec<String>>,
        }
        impl Chat for Fake {
            fn available(&self) -> bool {
                self.available.get()
            }
            fn send(&self, text: &str) -> bool {
                self.sent.borrow_mut().push(text.to_owned());
                true
            }
        }
        struct Quiet;
        impl Log for Quiet {
            fn line(&self, _: u32, _: &str, _: &str) {}
        }
        unsafe extern "C" fn on_load(_: *const W2rHost) -> W2rStatus {
            W2R_OK
        }
        static T: W2rPlugin = W2rPlugin {
            struct_size: W2R_PLUGIN_V1_SIZE,
            on_load: Some(on_load),
            on_event: None,
            on_ui: None,
            on_unload: None,
        };
        unsafe extern "C" fn entry(_: *const W2rHost) -> *const W2rPlugin {
            &T
        }
        let s = |t: &str| W2rStr {
            ptr: t.as_ptr().cast(),
            len: t.len() as u32,
        };

        let fake = Rc::new(Fake::default());
        let services = Services {
            chat: fake.clone(),
            ..Services::none()
        };
        let mut reg = Registry::with(Rc::new(Quiet), None, services);
        let denied = reg.load_entry("nocap", entry).unwrap();
        let i = reg
            .load_entry_with_caps("p", entry, &[caps::CHAT_WRITE])
            .unwrap();
        let h = unsafe { &*reg.slot(i).host() };
        let hd = unsafe { &*reg.slot(denied).host() };
        unsafe {
            assert_eq!((hd.chat_send)(hd.token, s("hi")), W2R_E_DENIED);
            assert_eq!(
                (h.chat_send)(h.token, s("hi")),
                W2R_E_STATE,
                "chat not available"
            );
            fake.available.set(true);
            assert_eq!((h.chat_send)(h.token, s("")), W2R_E_INVALID);
            assert_eq!((h.chat_send)(h.token, s("caf\u{e9}")), W2R_E_INVALID);
            assert_eq!((h.chat_send)(h.token, s(&"x".repeat(201))), W2R_E_INVALID);
            for n in 0..MAX_PER_WINDOW {
                assert_eq!((h.chat_send)(h.token, s(&format!("line {n}"))), W2R_OK);
            }
            assert_eq!((h.chat_send)(h.token, s("one too many")), W2R_E_RATE);
        }
        assert_eq!(fake.sent.borrow().len(), MAX_PER_WINDOW);
        assert_eq!(fake.sent.borrow()[0], "line 0");
    }
}
