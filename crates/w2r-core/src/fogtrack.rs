//! Per-player fog for replay viewing (docs/replay-research.md, "Viewing"). The game keeps three
//! 16 KB maps for the fog, all for the local player only: what is in sight now (0x91ad5c), what has
//! been explored (0x91ad60) and, per tile, which players are in the dark (0x91ad64). The renderer
//! picks fog edge sprites from the exact values in them, so they cannot be approximated.
//!
//! So the game's own code makes them: this module keeps a private copy of the three maps for every
//! other player and wraps the five small functions that change them (light a circle, the
//! periodic reset, the rebuild from the explored map, reset all, reveal all). Each time the game
//! calls one, it runs again against each player's private maps, through the pointer variables the
//! game's code reads them by, with the local player set to that player, then the real call runs
//! on the real maps as always. The real maps never see the extra calls, and drawing copies the
//! watched player's private maps into the real buffers.
//!
//! x86 only (`cdecl`).

use core::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

use crate::guard::guarded;
use crate::hooks::{self, Owner};
use crate::host::Log;
use crate::pe::Image;
use crate::symbols::Symbols;

/// `PlayerOwner` value of an empty slot.
const OWNER_EMPTY: u8 = 3;

type LightFn = unsafe extern "C" fn(i32, i32, i32, i32, u32);
type VoidFn = unsafe extern "C" fn();

static ORIG_LIGHT: AtomicUsize = AtomicUsize::new(0);
static ORIG_PERIODIC: AtomicUsize = AtomicUsize::new(0);
static ORIG_REBUILD: AtomicUsize = AtomicUsize::new(0);
static ORIG_RESET: AtomicUsize = AtomicUsize::new(0);
static ORIG_REVEAL: AtomicUsize = AtomicUsize::new(0);
static ADDRS: OnceLock<Addrs> = OnceLock::new();

struct Addrs {
    /// The local player byte.
    local: usize,
    /// The three pointer variables the game reads the maps through.
    current: usize,
    remembered: usize,
    dark: usize,
    owner: usize,
}

pub fn install(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
    let get = |name: &str| symbols.address(name);
    let (Some(light), Some(periodic), Some(rebuild), Some(reset), Some(reveal)) = (
        get("FogLightCircle"),
        get("FogPeriodicReset"),
        get("FogRebuildFromExplored"),
        get("FogResetAll"),
        get("FogRevealAll"),
    ) else {
        log.line(W2R_LOG_ERROR, "host", "fog tracking: symbols missing");
        return false;
    };
    let (Some(local), Some(current), Some(remembered), Some(dark), Some(owner)) = (
        get("LocalPlayer"),
        get("FogShadePtr"),
        get("FogExploredPtr"),
        get("FogVisibilityPtr"),
        get("PlayerOwner"),
    ) else {
        log.line(W2R_LOG_ERROR, "host", "fog tracking: symbols missing");
        return false;
    };
    let _ = ADDRS.set(Addrs {
        local,
        current,
        remembered,
        dark,
        owner,
    });
    let mut ok = true;
    // SAFETY: each detour matches its target's cdecl signature (Ghidra, build 2818) and calls the
    // original with the same arguments.
    unsafe {
        ok &= hook(
            image,
            light,
            light_detour as *mut c_void,
            "FogLightCircle",
            &ORIG_LIGHT,
            log,
        );
        ok &= hook(
            image,
            periodic,
            periodic_detour as *mut c_void,
            "FogPeriodicReset",
            &ORIG_PERIODIC,
            log,
        );
        ok &= hook(
            image,
            rebuild,
            rebuild_detour as *mut c_void,
            "FogRebuildFromExplored",
            &ORIG_REBUILD,
            log,
        );
        ok &= hook(
            image,
            reset,
            reset_detour as *mut c_void,
            "FogResetAll",
            &ORIG_RESET,
            log,
        );
        ok &= hook(
            image,
            reveal,
            reveal_detour as *mut c_void,
            "FogRevealAll",
            &ORIG_REVEAL,
            log,
        );
    }
    ok
}

unsafe fn hook(
    image: &Image,
    target: usize,
    detour: *mut c_void,
    name: &str,
    original: &'static AtomicUsize,
    log: &dyn Log,
) -> bool {
    let result = unsafe {
        hooks::global().install(
            Owner::Host,
            image,
            target,
            detour,
            &format!("host:{name}"),
            |o| original.store(o as usize, Ordering::Release),
        )
    };
    match result {
        Ok(_) => {
            log.line(
                W2R_LOG_INFO,
                "host",
                &format!("hooked {name} @ {target:#010x} (replay fog)"),
            );
            true
        }
        Err(e) => {
            log.line(
                W2R_LOG_ERROR,
                "host",
                &format!("could not hook {name}: {e:?}"),
            );
            false
        }
    }
}

/// Puts one player's private maps and local-player identity in place for the duration of `f`.
struct Swapped<'a> {
    addrs: &'a Addrs,
    saved: [usize; 3],
    local: u8,
}

impl<'a> Swapped<'a> {
    /// # Safety
    /// The pointers must stay valid until the guard drops; addresses are the game's globals.
    unsafe fn new(addrs: &'a Addrs, player: u8, maps: [usize; 3]) -> Swapped<'a> {
        unsafe {
            let saved = [
                core::ptr::read_unaligned(addrs.current as *const usize),
                core::ptr::read_unaligned(addrs.remembered as *const usize),
                core::ptr::read_unaligned(addrs.dark as *const usize),
            ];
            let local = *(addrs.local as *const u8);
            core::ptr::write_unaligned(addrs.current as *mut usize, maps[0]);
            core::ptr::write_unaligned(addrs.remembered as *mut usize, maps[1]);
            core::ptr::write_unaligned(addrs.dark as *mut usize, maps[2]);
            core::ptr::write_volatile(addrs.local as *mut u8, player);
            Swapped {
                addrs,
                saved,
                local,
            }
        }
    }
}

impl Drop for Swapped<'_> {
    fn drop(&mut self) {
        // SAFETY: restores what `new` read from the same addresses.
        unsafe {
            core::ptr::write_unaligned(self.addrs.current as *mut usize, self.saved[0]);
            core::ptr::write_unaligned(self.addrs.remembered as *mut usize, self.saved[1]);
            core::ptr::write_unaligned(self.addrs.dark as *mut usize, self.saved[2]);
            core::ptr::write_volatile(self.addrs.local as *mut u8, self.local);
        }
    }
}

/// Runs `call` once for each other player in the match, against that player's private maps.
fn for_each_private(call: impl Fn()) {
    let Some(a) = ADDRS.get() else { return };
    // The recorded player owns the real maps; before it is known, so does the local player.
    // SAFETY: the owner table and the local player byte are in the game's data section.
    let local = match crate::viewer::real() {
        real if real < 8 => real,
        _ => unsafe { *(a.local as *const u8) },
    };
    // The addresses are taken under the maps' lock and used after it is released: the vectors are
    // never resized while armed, and the game's functions called from here must not find the
    // lock held.
    let targets = crate::fogmaps::targets(local, |p| {
        // SAFETY: as above.
        unsafe { *((a.owner + p) as *const u8) == OWNER_EMPTY }
    });
    for (player, maps) in targets {
        // SAFETY: the vectors outlive the guard (see above).
        let _swapped = unsafe { Swapped::new(a, player, maps) };
        call();
    }
}

fn private_pass(f: impl Fn()) {
    let _ = guarded(|| for_each_private(f));
}

unsafe extern "C" fn light_detour(x: i32, y: i32, owner: i32, radius: i32, table: u32) {
    let original: LightFn = unsafe { core::mem::transmute(ORIG_LIGHT.load(Ordering::Acquire)) };
    private_pass(|| unsafe { original(x, y, owner, radius, table) });
    unsafe { original(x, y, owner, radius, table) };
}

macro_rules! void_detour {
    ($name:ident, $orig:ident) => {
        unsafe extern "C" fn $name() {
            let original: VoidFn = unsafe { core::mem::transmute($orig.load(Ordering::Acquire)) };
            private_pass(|| unsafe { original() });
            unsafe { original() };
        }
    };
}

void_detour!(periodic_detour, ORIG_PERIODIC);
void_detour!(rebuild_detour, ORIG_REBUILD);
void_detour!(reset_detour, ORIG_RESET);
void_detour!(reveal_detour, ORIG_REVEAL);
