//! "Create Multiplayer Game" lock buttons (docs/lobby-policy.md, slice 4a). The game's own
//! screen is drawn by `LobbyCreationContent`, inside the game's own window; the host calls it
//! unchanged and then places two buttons, Teams and Slots, by rectangle in that same window
//! (wc2r-plus's way; a second named window began from here nests badly). They flip
//! `lock_team_changes` and `lock_slot_swaps` in `host.json`. x86 only (`cdecl`).

use core::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

use crate::guard::guarded;
use crate::hooks::{self, Owner};
use crate::host::Log;
use crate::options::Nk;
use crate::pe::Image;
use crate::symbols::Symbols;
use crate::ui::Widgets;

pub const SYMBOL: &str = "LobbyCreationContent";
/// Bottom-left of the 2160-high UI space, clear of the centred Cancel and Create buttons.
const MARGIN: f32 = 40.0;
const WINDOW_W: f32 = 760.0;
const ROW_H: f32 = 100.0;

type ContentFn = unsafe extern "C" fn() -> i8;

static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static ANNOUNCED: AtomicBool = AtomicBool::new(false);

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
                &format!("hooked {SYMBOL} @ {target:#010x} (lock buttons)"),
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
    let original: ContentFn = unsafe { core::mem::transmute(ORIGINAL.load(Ordering::Acquire)) };
    let result = unsafe { original() };
    if let Err(fault) = guarded(|| crate::session::with_nk(draw_buttons)) {
        crate::session::host_fault("create screen locks", &fault);
    }
    result
}

fn label(locked: bool) -> &'static str {
    if locked { "Locked" } else { "Unlocked" }
}

fn draw_buttons(nk: &Nk) {
    let Some((teams, slots)) = crate::session::lock_choice() else {
        return;
    };
    let (_, height) = nk.ui_size();
    let (x, y) = (MARGIN, height - ROW_H - MARGIN);
    let half = (WINDOW_W - MARGIN) / 2.0;
    nk.in_window(|w, place| {
        place(x, y, half, ROW_H);
        let flip_teams = w.button(&format!("Teams: {}", label(teams)));
        place(x + half + MARGIN, y, half, ROW_H);
        let flip_slots = w.button(&format!("Slots: {}", label(slots)));
        if flip_teams || flip_slots {
            crate::session::set_lock_choice(teams ^ flip_teams, slots ^ flip_slots);
        }
    });
    if !ANNOUNCED.swap(true, Ordering::Relaxed) {
        crate::session::log_info("lobbypolicy", "lock buttons drawn on the create screen");
    }
}
