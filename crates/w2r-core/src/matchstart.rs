//! `W2R_EVT_MATCH_START`. The game clears its map-message slots when a match begins, and
//! wc2r-plus hooks that call as its match-start signal (`core/match_start.cpp`); so do we. Like
//! wc2r-plus the handlers run before the game's own function. x86 only (`fastcall`).
//!
//! `W2R_EVT_MATCH_END` has no proven source in wc2r-plus and is not fired yet (docs/abi.md).

use core::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

use crate::guard::guarded;
use crate::hooks::{self, Owner};
use crate::host::Log;
use crate::pe::Image;
use crate::symbols::Symbols;

pub const SYMBOL: &str = "ResetMapMessageSlots";

type ResetFn = unsafe extern "fastcall" fn(*mut c_void);

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
    // SAFETY: `detour` matches ResetMapMessageSlots' fastcall signature from the symbol table.
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
                &format!("hooked {SYMBOL} @ {target:#010x} (match start)"),
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

unsafe extern "fastcall" fn detour(unused: *mut c_void) {
    if let Err(fault) = guarded(crate::session::on_match_start) {
        crate::session::host_fault("match start", &fault);
    }
    let original: ResetFn = unsafe { core::mem::transmute(ORIGINAL.load(Ordering::Acquire)) };
    unsafe { original(unused) };
}
