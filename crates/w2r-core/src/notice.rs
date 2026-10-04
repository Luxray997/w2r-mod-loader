//! A banner over the multiplayer lobby for the handshake's warnings (docs/load-policy.md,
//! "Policy": a mismatched player "sees a clear warning in the lobby"). The game draws the lobby
//! as one Nuklear window; a window of ours cannot nest inside it, so the host hooks
//! `EndCurrentWindow` and, right after the lobby window ends, begins and ends a small
//! non-interactive window of its own at the top of the screen. It is created after the lobby
//! window each frame, so it draws above it. x86 only, like `options.rs`.

use core::ffi::c_void;
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

use crate::guard::guarded;
use crate::hooks::{self, Owner};
use crate::host::Log;
use crate::options::{GameWidgets, Nk};
use crate::pe::Image;
use crate::symbols::Symbols;
use crate::ui::Widgets;

pub const SYMBOL: &str = "EndCurrentWindow";
const WINDOW_NO_SCROLLBAR: u32 = 0x20;
const WINDOW_NO_INPUT: u32 = 0x1000;
/// Layout in the game's 2160-high UI space.
const BANNER_W: f32 = 1700.0;
const BANNER_TOP: f32 = 12.0;
const PAD: f32 = 16.0;
const LINE_H: f32 = 70.0;

type EndWindowFn = unsafe extern "C" fn(*mut c_void);

static ORIGINAL: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// The context the lobby window was begun on this frame, or 0.
    static LOBBY_CTX: Cell<usize> = const { Cell::new(0) };
}

/// Called by the frame hook for every window the game begins: remembers whether the window
/// now open is the lobby.
pub fn note_begin(ctx: *mut c_void, name: Option<&str>) {
    let lobby = name == Some(crate::lobby::WINDOW);
    LOBBY_CTX.with(|c| c.set(if lobby { ctx as usize } else { 0 }));
}

pub fn install(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
    let Some(target) = symbols.address(SYMBOL) else {
        log.line(
            W2R_LOG_ERROR,
            "host",
            &format!("{SYMBOL} is not in the symbol table"),
        );
        return false;
    };
    // SAFETY: `detour` matches EndCurrentWindow's cdecl signature from the symbol table.
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
                &format!("hooked {SYMBOL} @ {target:#010x}"),
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

unsafe extern "C" fn detour(ctx: *mut c_void) {
    let original: EndWindowFn = unsafe { core::mem::transmute(ORIGINAL.load(Ordering::Acquire)) };
    unsafe { original(ctx) };
    // Reset before drawing: our own window's end lands here too.
    let lobby = LOBBY_CTX.with(|c| c.replace(0));
    if lobby != 0 && lobby == ctx as usize {
        if let Err(fault) = guarded(|| crate::session::draw_lobby_notice(ctx)) {
            crate::session::host_fault("lobby notice", &fault);
        }
    }
}

/// One label per line, centred at the top. Every game call runs under its own guard inside
/// [`GameWidgets`]; the window begin/end run under the detour's guard.
pub fn draw(nk: &Nk, ctx: *mut c_void, lines: &[String]) {
    if ctx.is_null() || lines.is_empty() {
        return;
    }
    let width = nk.ui_width();
    let w = BANNER_W.min(width - 2.0 * PAD).max(PAD);
    let x = ((width - w) / 2.0).floor();
    let h = PAD * 2.0 + LINE_H * lines.len() as f32;
    let prev_skin = nk.push_skin(nk.window_skin());
    if nk.begin_window(
        ctx,
        c"w2rpm_lobby_notice",
        x,
        BANNER_TOP,
        w,
        h,
        WINDOW_NO_SCROLLBAR | WINDOW_NO_INPUT,
    ) {
        let widgets = GameWidgets::new(nk, ctx);
        for line in lines {
            widgets.row(LINE_H, 1);
            widgets.label(line);
        }
    }
    nk.end_window(ctx);
    nk.push_skin(prev_skin);
}
