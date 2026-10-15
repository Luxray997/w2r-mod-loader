//! Combat events for replays (docs/replay-research.md, "Combat events"). The game credits every
//! kill through one function, `KillCredit` (0x4def00, cdecl `(killer player byte, victim unit*)`),
//! which its only caller, the lethal-damage code, calls once per unit or building lost to another
//! player. Hooking it gives where and when each loss happened without touching the simulation:
//! the hook only reads the victim unit and then runs the original. Used for the timeline, the
//! jump-to-battle keys and the heat map. x86 only.
//!
//! Unit layout (Ghidra, build 2818): stride 0x98, map pixel x and y as two i16 at +0 and +2 (32
//! pixels per tile), unit type id at +0x27, owner at +0x2c.

use core::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

use crate::guard::guarded;
use crate::hooks::{self, Owner};
use crate::host::Log;
use crate::pe::Image;
use crate::symbols::Symbols;

type KillFn = unsafe extern "C" fn(u32, *const u8);

static ORIG: AtomicUsize = AtomicUsize::new(0);

pub fn install(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
    let Some(target) = symbols.address("KillCredit") else {
        log.line(W2R_LOG_ERROR, "host", "combat recording: symbol missing");
        return false;
    };
    // SAFETY: the detour matches the target's cdecl signature (Ghidra, build 2818) and calls the
    // original with the same arguments.
    let result = unsafe {
        hooks::global().install(
            Owner::Host,
            image,
            target,
            detour as *mut c_void,
            "host:KillCredit",
            |o| ORIG.store(o as usize, Ordering::Release),
        )
    };
    match result {
        Ok(_) => {
            log.line(
                W2R_LOG_INFO,
                "host",
                &format!("hooked KillCredit @ {target:#010x} (replay combat events)"),
            );
            true
        }
        Err(e) => {
            log.line(W2R_LOG_ERROR, "host", &format!("could not hook KillCredit: {e:?}"));
            false
        }
    }
}

unsafe extern "C" fn detour(killer: u32, victim: *const u8) {
    // Read the unit before the original runs; the unit is not freed by it, but this keeps the
    // event independent of whatever the game does to the struct next.
    let _ = guarded(|| {
        if victim.is_null() || !crate::replayrec::recording() {
            return;
        }
        // SAFETY: the game passes a live unit struct; the fields are within its 0x98 bytes.
        let (owner, unit_type, px, py) = unsafe {
            (
                *victim.add(0x2c),
                *victim.add(0x27),
                core::ptr::read_unaligned(victim as *const i16),
                core::ptr::read_unaligned(victim.add(2) as *const i16),
            )
        };
        if let Some(ev) = w2r_replay::heat::kill_event(killer as u8, owner, unit_type, px, py) {
            crate::replayrec::note_event(&ev);
        }
    });
    let original: KillFn = unsafe { core::mem::transmute(ORIG.load(Ordering::Acquire)) };
    unsafe { original(killer, victim) };
}
