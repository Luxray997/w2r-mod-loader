//! The hook on the game's "Enter Password" dialog for joining a private game
//! (`JoinPasswordContent`, cdecl `void()`, drawn every frame while the dialog is up). The game
//! leaves the text field unfocused, so the player has to click it before typing; this gives it the
//! keyboard when the dialog opens. x86 only.

use core::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

use crate::hooks::{self, Owner};
use crate::host::Log;
use crate::pe::Image;
use crate::screenkit::game::Kit;
use crate::symbols::Symbols;

pub const SYMBOL: &str = "JoinPasswordContent";

/// A longer gap between frames than this means the dialog was closed and is opening again.
const REOPEN_GAP_MS: u64 = 250;

type ContentFn = unsafe extern "C" fn();

struct State {
    kit: Kit,
    buffer: usize,
}

static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static STATE: OnceLock<State> = OnceLock::new();
static LAST_FRAME_MS: AtomicU64 = AtomicU64::new(0);
/// The field has the keyboard for this opening; nothing more is done until the dialog reopens.
static FOCUSED: AtomicBool = AtomicBool::new(false);

pub fn install(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
    let (Some(target), Some(buffer), Some(kit)) = (
        symbols.address(SYMBOL),
        symbols.address("JoinPasswordBuffer"),
        Kit::new(symbols),
    ) else {
        log.line(
            W2R_LOG_ERROR,
            "host",
            "join password: a symbol is missing; the dialog stays as it is",
        );
        return false;
    };
    let _ = STATE.set(State { kit, buffer });
    // SAFETY: `detour` matches JoinPasswordContent's cdecl `void()` signature.
    let result = unsafe {
        hooks::global().install(
            Owner::Host,
            image,
            target,
            detour as *mut c_void,
            &format!("host:{SYMBOL}"),
            |original| ORIGINAL.store(original as usize, Ordering::Release),
        )
    };
    match result {
        Ok(_) => {
            log.line(
                W2R_LOG_INFO,
                "host",
                &format!("hooked {SYMBOL} @ {target:#010x} (focus the password field)"),
            );
            true
        }
        Err(e) => {
            log.line(
                W2R_LOG_ERROR,
                "host",
                &format!("could not hook {SYMBOL}: {e:?}"),
            );
            false
        }
    }
}

unsafe extern "C" fn detour() {
    let original: ContentFn = unsafe { core::mem::transmute(ORIGINAL.load(Ordering::Acquire)) };
    let Some(state) = STATE.get() else {
        return unsafe { original() };
    };
    let now = crate::chat::now_ms();
    let last = LAST_FRAME_MS.swap(now, Ordering::Relaxed);
    if last == 0 || now.saturating_sub(last) > REOPEN_GAP_MS {
        FOCUSED.store(false, Ordering::Relaxed);
    }
    if FOCUSED.load(Ordering::Relaxed) {
        return unsafe { original() };
    }
    // The dialog has one text field, so it gets the window's next edit id. Ask for the keyboard
    // before it is drawn and check afterwards that the game kept it.
    let id = state.kit.edit_seq();
    if id != u32::MAX {
        // SAFETY: the game's NUL-terminated password buffer.
        let len = unsafe { core::ffi::CStr::from_ptr(state.buffer as *const _) }
            .to_bytes()
            .len();
        state.kit.focus_edit(id, len);
    }
    unsafe { original() };
    if id != u32::MAX && state.kit.edit_focus() == Some(id) {
        FOCUSED.store(true, Ordering::Relaxed);
    }
}
