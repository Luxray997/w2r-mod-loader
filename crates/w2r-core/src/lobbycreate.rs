//! The hook on the game's Create Multiplayer Game content (`LobbyCreationContent`, `cdecl`
//! `char()`, drawn every frame of that screen inside the game's own window). It hands the frame to
//! the replacement screen ([`crate::createscreen`]) when that is on and usable, and otherwise
//! lets the game draw its own. x86 only. (Until slice 4 this also drew two lock buttons over the
//! stock screen; the locks now live on the replacement screen and the plugin screen only.)

use core::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

use crate::hooks::{self, Owner};
use crate::host::Log;
use crate::pe::Image;
use crate::symbols::Symbols;

pub const SYMBOL: &str = "LobbyCreationContent";

type ContentFn = unsafe extern "C" fn() -> i8;

static ORIGINAL: AtomicUsize = AtomicUsize::new(0);

pub fn install(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
    let Some(target) = symbols.address(SYMBOL) else {
        log.line(
            W2R_LOG_ERROR,
            "host",
            &format!("{SYMBOL} is not in the symbol table"),
        );
        return false;
    };
    // SAFETY: `detour` matches LobbyCreationContent's cdecl `char()` signature.
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
                &format!("hooked {SYMBOL} @ {target:#010x} (create screen)"),
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

unsafe extern "C" fn detour() -> i8 {
    // The replacement screen, when it is on and usable, is the whole screen and the stock content
    // is not drawn; otherwise the game draws its own, untouched.
    if crate::createscreen::try_draw() {
        return 0;
    }
    let original: ContentFn = unsafe { core::mem::transmute(ORIGINAL.load(Ordering::Acquire)) };
    unsafe { original() }
}
