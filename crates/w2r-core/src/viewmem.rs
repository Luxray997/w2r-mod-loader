//! The game-facing half of `viewmemory.rs`: what the game remembers about buildings it has seen.
//! Two places hold it, both written by the draw pass for whichever player the screen shows:
//! each unit's remembered appearance (two bytes at +0x86 and +0x87 of its 0x98-byte slot, 0xff
//! at +0x86 = nothing remembered) and the 25-slot array of ghost copies of buildings that left
//! sight. x86 only.

use std::sync::{Mutex, OnceLock};

use crate::guard::guarded;
use crate::symbols::Symbols;
use crate::viewmemory::ViewMemory;

const UNIT_STRIDE: usize = 0x98;
const SEEN_AT: usize = 0x86;
const GHOSTS_LEN: usize = 25 * UNIT_STRIDE;
/// More than the game ever has: a bad count never makes us read far.
const MAX_UNITS: usize = 4096;

struct Addrs {
    /// Pointer variable to the unit slots, and the count of slots in use.
    units: usize,
    count: usize,
    ghosts: usize,
}

/// What is remembered: each unit slot's two bytes, and the ghost array.
pub struct Memory {
    units: Vec<[u8; 2]>,
    ghosts: Vec<u8>,
}

static ADDRS: OnceLock<Addrs> = OnceLock::new();
static STATE: Mutex<Option<ViewMemory<Memory>>> = Mutex::new(None);

pub fn init(symbols: &Symbols) -> bool {
    let (Some(units), Some(count), Some(ghosts)) = (
        symbols.address("UnitArrayPtr"),
        symbols.address("UnitCount"),
        symbols.address("GhostUnits"),
    ) else {
        return false;
    };
    let _ = ADDRS.set(Addrs {
        units,
        count,
        ghosts,
    });
    true
}

/// The unit slots in use: base address and count.
fn units(a: &Addrs) -> (usize, usize) {
    // SAFETY: a pointer variable and a count in the game's data section.
    let (base, n) = unsafe {
        (
            core::ptr::read_unaligned(a.units as *const usize),
            core::ptr::read_unaligned(a.count as *const u32) as usize,
        )
    };
    (base, if base == 0 { 0 } else { n.min(MAX_UNITS) })
}

fn read(a: &Addrs) -> Memory {
    let (base, n) = units(a);
    // SAFETY: `n` slots of 0x98 bytes from the game's unit array; the ghost array is fixed.
    unsafe {
        Memory {
            units: (0..n)
                .map(|i| {
                    let at = base + i * UNIT_STRIDE + SEEN_AT;
                    [*(at as *const u8), *((at + 1) as *const u8)]
                })
                .collect(),
            ghosts: core::slice::from_raw_parts(a.ghosts as *const u8, GHOSTS_LEN).to_vec(),
        }
    }
}

fn write(a: &Addrs, m: &Memory) {
    let (base, n) = units(a);
    // SAFETY: as in `read`; only slots that exist now are written.
    unsafe {
        for (i, seen) in m.units.iter().take(n).enumerate() {
            let at = base + i * UNIT_STRIDE + SEEN_AT;
            *(at as *mut u8) = seen[0];
            *((at + 1) as *mut u8) = seen[1];
        }
        let n = m.ghosts.len().min(GHOSTS_LEN);
        core::ptr::copy_nonoverlapping(m.ghosts.as_ptr(), a.ghosts as *mut u8, n);
    }
}

/// Offset of the "hidden from" bits in a unit slot (and in a ghost copy of one).
const HIDDEN_AT: usize = 0x28;

/// The hidden-from byte of every unit slot in use, then of the 25 ghost slots.
pub fn save_hidden() -> Vec<u8> {
    let Some(a) = ADDRS.get() else {
        return Vec::new();
    };
    let (base, n) = units(a);
    // SAFETY: `n` slots of the game's unit array and its fixed ghost array.
    unsafe {
        (0..n)
            .map(|i| *((base + i * UNIT_STRIDE + HIDDEN_AT) as *const u8))
            .chain((0..25).map(|i| *((a.ghosts + i * UNIT_STRIDE + HIDDEN_AT) as *const u8)))
            .collect()
    }
}

/// Puts back what `save_hidden` read (only slots that still exist are written).
pub fn restore_hidden(saved: &[u8]) {
    let Some(a) = ADDRS.get() else { return };
    let (base, n) = units(a);
    if saved.len() < n + 25 {
        return;
    }
    // SAFETY: as in `save_hidden`.
    unsafe {
        for (i, &b) in saved.iter().take(n).enumerate() {
            *((base + i * UNIT_STRIDE + HIDDEN_AT) as *mut u8) = b;
        }
        for i in 0..25 {
            *((a.ghosts + i * UNIT_STRIDE + HIDDEN_AT) as *mut u8) = saved[n + i];
        }
    }
}

/// A view that has seen nothing: no unit remembered, no ghosts.
fn blank(a: &Addrs) -> Memory {
    let (_, n) = units(a);
    Memory {
        units: vec![[0xff, 0]; n],
        ghosts: vec![0; GHOSTS_LEN],
    }
}

/// Shows `slot`'s memory (0..=7 a player, 8 full vision) instead of the recorded player's.
pub fn enter(slot: usize) {
    let Some(a) = ADDRS.get() else { return };
    let _ = guarded(|| {
        if let Ok(mut state) = STATE.lock() {
            state.get_or_insert_with(ViewMemory::default).enter(
                slot,
                || read(a),
                |m| write(a, m),
                || blank(a),
            );
        }
    });
}

/// Puts the recorded player's memory back (before the simulation continues).
pub fn leave() {
    let Some(a) = ADDRS.get() else { return };
    let _ = guarded(|| {
        if let Ok(mut state) = STATE.lock() {
            if let Some(m) = state.as_mut() {
                m.leave(|| read(a), |m| write(a, m));
            }
        }
    });
}

/// The replay ended: forget every view.
pub fn reset() {
    leave();
    if let Ok(mut state) = STATE.lock() {
        *state = None;
    }
}
