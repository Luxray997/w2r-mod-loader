//! The in-game connection table (docs/net-panel.md): one row per other person in the match with
//! their ping, jitter and loss and a plain-language reading of the link (`netquality::assess`), so
//! nobody judges a connection by the ping number alone. It reads the figures the game's own
//! transport already keeps (the same ones the lobby bars use), so it works against players who do
//! not run the mod. The "Connections" button at the top right or Ctrl+Shift+N shows or hides it; `net_panel` in `host.json` sets whether a match
//! starts with it up.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::hostcfg::HostConfig;
use crate::netquality::Metrics;

static VISIBLE: AtomicBool = AtomicBool::new(false);

/// Takes the settings (at startup and whenever the user changes one; they apply at once).
pub fn configure(cfg: &HostConfig) {
    VISIBLE.store(cfg.net_panel, Ordering::Release);
}

/// A time in whole milliseconds, or a dash when it is not known yet.
pub fn ms_cell(v: Option<f32>) -> String {
    match v {
        Some(v) if v >= 1000.0 => "999+".to_owned(),
        Some(v) => format!("{}", v.round().max(0.0) as u32),
        None => "-".to_owned(),
    }
}

pub fn loss_cell(m: &Metrics) -> String {
    match m.loss_pct {
        Some(l) => format!("{}%", l.round() as u32),
        None => "-".to_owned(),
    }
}

#[cfg(target_arch = "x86")]
pub use live::{before_hud, install};

#[cfg(target_arch = "x86")]
mod live {
    use core::ffi::c_void;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

    use super::{VISIBLE, loss_cell, ms_cell};
    use crate::guard::guarded;
    use crate::host::Log;
    use crate::hudpanel::BeginFn;
    use crate::netquality::{self, Metrics, game::Reader};
    use crate::screenkit::game::{Kit, rgba};
    use crate::screenkit::{ALIGN_LEFT, ALIGN_RIGHT, Rect};
    use crate::symbols::Symbols;

    type ExtentFn = unsafe extern "C" fn() -> f32;

    /// No scrollbar, background, and no input (see `apm`): the panel is only labels.
    const WINDOW_FLAGS: u32 = 0x120 | 0x400;
    const WIDTH: f32 = 470.0;
    const TOP: f32 = 60.0;
    const MARGIN_RIGHT: f32 = 24.0;
    const ROW_HEIGHT: f32 = 14.0;
    const HEADER_HEIGHT: f32 = 32.0;
    const FOOTER_HEIGHT: f32 = 32.0;
    const BACKGROUND: u32 = rgba(12, 9, 6, 200);
    const HEAD: u32 = rgba(140, 130, 112, 255);
    const DIM_TEXT: u32 = HEAD;

    /// Columns (left edge, width), window-local.
    const NAME: (f32, f32) = (16.0, 92.0);
    const PING: (f32, f32) = (108.0, 44.0);
    const JITTER: (f32, f32) = (156.0, 44.0);
    const LOSS: (f32, f32) = (204.0, 40.0);
    const VERDICT: (f32, f32) = (256.0, 208.0);

    /// The button's window takes input (so a click is the button's and does not reach the map);
    /// the table below it does not (see `WINDOW_FLAGS`).
    const BUTTON_FLAGS: u32 = 0x120;
    const BUTTON_WIDTH: f32 = 24.0;
    const BUTTON_HEIGHT: f32 = 16.0;
    /// Just inside the game area, in the top strip right of the sidebar's Menu and Pause
    /// buttons (the sidebar is 176 px wide; docs/ingame-ui-research.md on the ingame-ui branch).
    const BUTTON_LEFT: f32 = 180.0;
    const BUTTON_TOP: f32 = 0.0;

    /// The tooltip beside the button, like the player-colours plugin's: one line on a dark plate
    /// the height of the top strip. The font has no width query, so the width assumes 7 px a character.
    const TIP_TEXT: &str = "Connections (Ctrl+Shift+N)";
    const TIP_PAD: f32 = 4.0;
    const TIP_GAP: f32 = 4.0;
    const TIP_BACKGROUND: u32 = rgba(16, 16, 16, 240);

    const STATE_IN_MATCH: u16 = 3;
    const VK_CONTROL: i32 = 0x11;
    const VK_SHIFT: i32 = 0x10;
    const VK_N: i32 = 0x4E;

    fn log(text: &str) {
        crate::session::StartupLog.line(W2R_LOG_INFO, "netpanel", text);
    }

    struct Game {
        kit: Kit,
        ui_width: ExtentFn,
        reader: Reader,
        player_by_net: usize,
        names: usize,
        owner: usize,
        result: usize,
        state: usize,
    }

    // SAFETY: plain game function and data addresses, fixed for the session.
    unsafe impl Send for Game {}
    unsafe impl Sync for Game {}

    static GAME: OnceLock<Game> = OnceLock::new();
    static KEY_WAS_DOWN: AtomicBool = AtomicBool::new(false);

    const NAME_STRIDE: usize = 0x38;
    const NAME_MAX: usize = 0x19;

    pub fn install(symbols: &Symbols, log: &dyn Log) -> bool {
        let get = |n: &str| symbols.address(n);
        let (
            Some(ui_width),
            Some(player_by_net),
            Some(names),
            Some(owner),
            Some(result),
            Some(state),
            Some(reader),
            Some(kit),
        ) = (
            get("HudUiWidth"),
            get("NetSlotPlayer"),
            get("PlayerNames"),
            get("PlayerOwner"),
            get("PlayerMatchResult"),
            get("GameStateCurrent"),
            Reader::new(symbols),
            Kit::new_in(symbols, "HudCtx"),
        )
        else {
            log.line(W2R_LOG_ERROR, "host", "connection table: symbols missing");
            return false;
        };
        // SAFETY: `HudUiWidth` takes nothing and returns a float (Ghidra, build 2818).
        let ui_width = unsafe { core::mem::transmute::<usize, ExtentFn>(ui_width) };
        let _ = GAME.set(Game {
            kit,
            ui_width,
            reader,
            player_by_net,
            names,
            owner,
            result,
            state,
        });
        log.line(
            W2R_LOG_INFO,
            "host",
            "connection table ready (Ctrl+Shift+N in a match)",
        );
        true
    }

    impl Game {
        fn name(&self, player: u8) -> String {
            // SAFETY: the player records are a fixed table in the game's data section.
            let bytes = unsafe {
                core::slice::from_raw_parts(
                    (self.names + usize::from(player) * NAME_STRIDE) as *const u8,
                    NAME_MAX,
                )
            };
            let n = bytes.iter().position(|&b| b == 0).unwrap_or(NAME_MAX);
            String::from_utf8_lossy(&bytes[..n]).into_owned()
        }

        /// The people still in the match whose link we measure, as (name, metrics).
        fn rows(&self) -> Vec<(String, Metrics)> {
            // Reads the records and keeps the trackers fed whether or not the table is up, so the
            // figures are ready when it is opened.
            self.reader.poll();
            let by_net = netquality::game::latest_by_net();
            by_net
                .iter()
                .enumerate()
                .filter_map(|(net, m)| {
                    let m = m.clone()?;
                    // SAFETY: per-net-slot table of u32 and per-player byte tables of at least 8
                    // entries in the game's data section; the caller wraps this in `guarded`.
                    let player = unsafe {
                        (core::ptr::read_unaligned((self.player_by_net + net * 4) as *const u32)
                            & 0xff) as u8
                    };
                    if player >= 8 {
                        return None;
                    }
                    let (owner, result) = unsafe {
                        (
                            *((self.owner + usize::from(player)) as *const u8),
                            *((self.result + usize::from(player)) as *const u8),
                        )
                    };
                    // A person (owner 0) who has not left or been beaten.
                    (owner == 0 && result == 0).then(|| (self.name(player), m))
                })
                .collect()
        }
    }

    /// Ctrl+Shift+N, once per press, only while the game has focus.
    fn toggle_key_pressed() -> bool {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
        if !crate::replayplay::game_has_focus() {
            KEY_WAS_DOWN.store(false, Ordering::Release);
            return false;
        }
        let held = |vk: i32| (unsafe { GetAsyncKeyState(vk) } as u16) & 0x8000 != 0;
        let down = held(VK_N) && held(VK_CONTROL) && held(VK_SHIFT);
        // Swap first: the flag must follow the key on release too, or only the first press counts.
        let was_down = KEY_WAS_DOWN.swap(down, Ordering::AcqRel);
        down && !was_down
    }

    /// Called by the window hook just before the game starts its `game_hud` window.
    ///
    /// # Safety
    /// `ctx` is the context the game is about to begin `game_hud` in; `begin` is the game's own
    /// `BeginNamedWindow`.
    pub unsafe fn before_hud(ctx: *mut c_void, begin: BeginFn) {
        let Some(g) = GAME.get() else { return };
        if crate::replayplay::is_active() {
            return;
        }
        let result = guarded(|| unsafe {
            if core::ptr::read_unaligned(g.state as *const u16) != STATE_IN_MATCH {
                return;
            }
            let rows = g.rows();
            if toggle_key_pressed() {
                VISIBLE.fetch_xor(true, Ordering::AcqRel);
                log(&format!(
                    "connection table {} (Ctrl+Shift+N)",
                    if VISIBLE.load(Ordering::Acquire) { "shown" } else { "hidden" }
                ));
            }
            // Alone in the match (single-player) there is nothing to show.
            if rows.is_empty() {
                return;
            }
            let x = ((g.ui_width)() - MARGIN_RIGHT - WIDTH).max(0.0);
            // The button, for players who do not know the hotkey.
            if begin(
                ctx,
                c"net_button".as_ptr(),
                BUTTON_LEFT,
                BUTTON_TOP,
                BUTTON_WIDTH,
                BUTTON_HEIGHT,
                BUTTON_FLAGS,
            ) != 0
            {
                g.kit.space_begin();
                if g.kit.button_at(Rect::new(0.0, 0.0, BUTTON_WIDTH, BUTTON_HEIGHT), "", false) {
                    VISIBLE.fetch_xor(true, Ordering::AcqRel);
                    log(&format!(
                        "connection table {} (button)",
                        if VISIBLE.load(Ordering::Acquire) { "shown" } else { "hidden" }
                    ));
                }
                // Signal bars in the colour of the worst link, so the icon says something even
                // while the table is closed.
                let worst = rows
                    .iter()
                    .map(|(_, m)| netquality::assess(m).quality)
                    .filter(|q| *q != netquality::Quality::NoData)
                    .min()
                    .unwrap_or(netquality::Quality::NoData);
                let colour = if VISIBLE.load(Ordering::Acquire) || worst == netquality::Quality::NoData {
                    HEAD
                } else {
                    worst.colour()
                };
                let bx = BUTTON_LEFT + 6.0;
                let by = BUTTON_TOP + BUTTON_HEIGHT - 3.0;
                for i in 0..4u32 {
                    let h = 3.0 + 2.5 * i as f32;
                    g.kit.fill_rect(Rect::new(bx + 4.0 * i as f32, by - h, 3.0, h), colour);
                }
                g.kit.space_end();
                g.kit.end_window();
            }
            let (mx, my) = g.kit.mouse();
            if mx >= BUTTON_LEFT
                && mx < BUTTON_LEFT + BUTTON_WIDTH
                && my >= BUTTON_TOP
                && my < BUTTON_TOP + BUTTON_HEIGHT
            {
                let tip_x = BUTTON_LEFT + BUTTON_WIDTH + TIP_GAP;
                let tip_w = TIP_TEXT.len() as f32 * 7.0 + TIP_PAD * 2.0;
                if begin(
                    ctx,
                    c"net_tip".as_ptr(),
                    tip_x,
                    BUTTON_TOP,
                    tip_w,
                    BUTTON_HEIGHT,
                    WINDOW_FLAGS,
                ) != 0
                {
                    g.kit.space_begin();
                    g.kit.fill_round(
                        Rect::new(tip_x, BUTTON_TOP, tip_w, BUTTON_HEIGHT),
                        2.0,
                        TIP_BACKGROUND,
                    );
                    g.kit.label_at(
                        Rect::new(TIP_PAD, 1.0, tip_w - TIP_PAD, 14.0),
                        TIP_TEXT,
                        ALIGN_LEFT,
                    );
                    g.kit.space_end();
                    g.kit.end_window();
                }
            }
            if !VISIBLE.load(Ordering::Acquire) {
                return;
            }
            let height = HEADER_HEIGHT + ROW_HEIGHT * rows.len() as f32 + FOOTER_HEIGHT;
            if begin(ctx, c"net_panel".as_ptr(), x, TOP, WIDTH, height, WINDOW_FLAGS) != 0 {
                g.kit.space_begin();
                draw(g, &rows, x, height);
                g.kit.space_end();
                g.kit.end_window();
            }
        });
        if let Err(fault) = result {
            crate::session::host_fault("connection table", &fault);
        }
    }

    /// `x` is the window's left edge: fills use screen coordinates, labels the window's own.
    fn draw(g: &Game, rows: &[(String, Metrics)], x: f32, height: f32) {
        let kit = &g.kit;
        kit.fill_round(Rect::new(x, TOP, WIDTH, height), 3.0, BACKGROUND);
        kit.label_at(Rect::new(6.0, 2.0, WIDTH * 0.5, 14.0), "Connections", ALIGN_LEFT);
        kit.label_coloured_at(
            Rect::new(WIDTH * 0.4, 2.0, WIDTH * 0.6 - 6.0, 14.0),
            "Ctrl+Shift+N or the button hides",
            ALIGN_RIGHT,
            DIM_TEXT,
        );
        let head = |col: (f32, f32), text: &str, align: i32| {
            kit.label_coloured_at(Rect::new(col.0, 16.0, col.1, 14.0), text, align, HEAD);
        };
        head(NAME, "Player", ALIGN_LEFT);
        head(PING, "Ping", ALIGN_RIGHT);
        head(JITTER, "Jitter", ALIGN_RIGHT);
        head(LOSS, "Loss", ALIGN_RIGHT);
        head(VERDICT, "Connection", ALIGN_LEFT);
        for (n, (name, m)) in rows.iter().enumerate() {
            let y = HEADER_HEIGHT + ROW_HEIGHT * n as f32;
            let a = netquality::assess(m);
            let colour = a.quality.colour();
            kit.fill_rect(Rect::new(x + 6.0, TOP + y + 3.0, 6.0, ROW_HEIGHT - 6.0), colour);
            let cell = |col: (f32, f32), text: &str, align: i32| {
                kit.label_at(Rect::new(col.0, y, col.1, ROW_HEIGHT), text, align);
            };
            cell(NAME, name, ALIGN_LEFT);
            cell(PING, &ms_cell(m.ping_ms), ALIGN_RIGHT);
            cell(JITTER, &ms_cell(m.jitter_ms), ALIGN_RIGHT);
            cell(LOSS, &loss_cell(m), ALIGN_RIGHT);
            kit.label_coloured_at(
                Rect::new(VERDICT.0, y, VERDICT.1, ROW_HEIGHT),
                a.summary,
                ALIGN_LEFT,
                colour,
            );
        }
        let foot = HEADER_HEIGHT + ROW_HEIGHT * rows.len() as f32 + 4.0;
        kit.label_coloured_at(
            Rect::new(6.0, foot, WIDTH - 12.0, 14.0),
            "Ping is delay (ms), jitter how much it swings, loss resent packets.",
            ALIGN_LEFT,
            DIM_TEXT,
        );
        kit.label_coloured_at(
            Rect::new(6.0, foot + 13.0, WIDTH - 12.0, 14.0),
            "Steady lag is playable; spikes and loss hurt more than a high ping.",
            ALIGN_LEFT,
            DIM_TEXT,
        );
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(ping: Option<f32>, loss: Option<f32>) -> Metrics {
        Metrics {
            ping_ms: ping,
            jitter_ms: None,
            loss_pct: loss,
            silent_ms: 0.0,
            waiting_ms: None,
            samples: 3,
        }
    }

    #[test]
    fn cells_round_and_cap() {
        assert_eq!(ms_cell(Some(41.6)), "42");
        assert_eq!(ms_cell(Some(2500.0)), "999+");
        assert_eq!(ms_cell(None), "-");
    }

    #[test]
    fn loss_is_a_percentage_or_a_dash() {
        assert_eq!(loss_cell(&m(Some(40.0), Some(4.4))), "4%");
        assert_eq!(loss_cell(&m(Some(40.0), None)), "-");
    }
}
