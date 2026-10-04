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

type ContentFn = unsafe extern "C" fn() -> i8;

static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
/// `SkinLobbyCreation`: the create screen's own skin, which the stock content builds under.
static SKIN: AtomicUsize = AtomicUsize::new(0);
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
    SKIN.store(
        symbols.address("SkinLobbyCreation").unwrap_or(0),
        Ordering::Release,
    );
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
    // The replacement screen, when it is on and usable, is the whole screen; the stock content
    // is not drawn and the 4a buttons are part of ours.
    if crate::createscreen::try_draw() {
        return 0;
    }
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

/// Offsets in the game's Nuklear context (wc2r-plus `struct_offsets.h`).
const CTX_CURRENT_WINDOW: usize = 0x3d28;
const WINDOW_PANEL: usize = 0x8c;
const PANEL_CLIP: usize = 0x3c;
/// The footer row of the stock screen: bottom gap and button height (wc2r-plus `screen_kit.h`).
const FOOTER_BOTTOM_GAP: f32 = 230.0;
const FOOTER_H: f32 = 135.0;
const FRAME_PAD: f32 = 100.0;
const BUTTON_W: f32 = 560.0;
const GAP: f32 = 30.0;

/// The rectangle `(x, y, w, h)` of the window being built, from its panel's clip.
unsafe fn window_rect(ctx: *const u8) -> Option<(f32, f32, f32, f32)> {
    unsafe {
        let win = ctx
            .add(CTX_CURRENT_WINDOW)
            .cast::<*const u8>()
            .read_unaligned();
        if win.is_null() {
            return None;
        }
        let panel = win.add(WINDOW_PANEL).cast::<*const u8>().read_unaligned();
        if panel.is_null() {
            return None;
        }
        let clip = panel.add(PANEL_CLIP).cast::<[f32; 4]>().read_unaligned();
        (clip[2] > 100.0 && clip[3] > 100.0).then_some((clip[0], clip[1], clip[2], clip[3]))
    }
}

fn draw_buttons(nk: &Nk) {
    let Some((teams, slots)) = crate::session::lock_choice() else {
        return;
    };
    let ctx = nk.ctx();
    let Some((wx, wy, ww, wh)) = (!ctx.is_null())
        .then(|| unsafe { window_rect(ctx as *const u8) })
        .flatten()
    else {
        return;
    };
    let _ = (wx, wy, ww);
    let y = wh - FOOTER_BOTTOM_GAP;
    let skin = SKIN.load(Ordering::Acquire) as *const u32;
    let previous = (!skin.is_null()).then(|| nk.push_skin(unsafe { *skin }));
    nk.in_window(|w, place| {
        place(FRAME_PAD, y, BUTTON_W, FOOTER_H);
        let flip_teams = w.button(&format!("Teams: {}", label(teams)));
        place(FRAME_PAD + BUTTON_W + GAP, y, BUTTON_W, FOOTER_H);
        let flip_slots = w.button(&format!("Slots: {}", label(slots)));
        if flip_teams || flip_slots {
            crate::session::set_lock_choice(teams ^ flip_teams, slots ^ flip_slots);
        }
    });
    if let Some(previous) = previous {
        nk.push_skin(previous);
    }
    if !ANNOUNCED.swap(true, Ordering::Relaxed) {
        crate::session::log_info(
            "lobbypolicy",
            &format!(
                "lock buttons drawn on the create screen (window {ww:.0}x{wh:.0} at {wx:.0},{wy:.0})"
            ),
        );
    }
}
