//! The replay panel: speed, pause state, the hotkeys and a progress bar, with a scoreboard under
//! it (each player's name, resources, supply and unit counts), drawn on the in-match HUD while a
//! replay plays. The HUD builder itself belongs to the chat plugin (it replaces it whole and also
//! hooks the map-message functions), so the panel is a small window of its own that the host's
//! `BeginNamedWindow` hook opens just before the game begins `game_hud`. x86 only (`cdecl`).

use core::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

use crate::guard::guarded;
use crate::host::Log;
use crate::replayplay::{self, Pace};
use crate::viewer::{self, Member};
use crate::scoreboard::{self, Counts, History};
use crate::screenkit::game::{Kit, rgba};
use crate::screenkit::{ALIGN_CENTRE, ALIGN_LEFT, ALIGN_RIGHT, Rect};
use crate::symbols::Symbols;

type ExtentFn = unsafe extern "C" fn() -> f32;
/// The game's `BeginNamedWindow`.
pub type BeginFn =
    unsafe extern "C" fn(*mut c_void, *const core::ffi::c_char, f32, f32, f32, f32, u32) -> i32;

static PANEL: OnceLock<Panel> = OnceLock::new();
static HISTORY: Mutex<Option<History>> = Mutex::new(None);

/// Where the game keeps what the scoreboard shows (all indexed by player).
struct Game {
    gold: usize,
    lumber: usize,
    oil: usize,
    food_used: usize,
    food_free: usize,
    food_cap: usize,
    units: usize,
    unit_count: usize,
    type_flags: usize,
    kills: usize,
    razed: usize,
    lost: usize,
    buildings_lost: usize,
    mined_gold: usize,
    mined_lumber: usize,
    mined_oil: usize,
    /// `TeamColourReorder`: the colour index of each player (the game shuffles them at the start).
    colour_reorder: usize,
    /// `TeamColourRgba`: four floats per colour index.
    colour_rgba: usize,
}

struct Panel {
    kit: Kit,
    ui_width: ExtentFn,
    game: Game,
    /// For drawing the map picture under the heat; `None` and the heat is drawn on plain ground.
    preview: Option<crate::previewdraw::PreviewGame>,
}

// SAFETY: plain game function and data addresses, fixed for the session.
unsafe impl Send for Panel {}
unsafe impl Sync for Panel {}

/// The flags the HUD's own full-screen window is begun with.
const WINDOW_FLAGS: u32 = 0x120;
const WIDTH: f32 = 362.0;
const ROW_HEIGHT: f32 = 12.0;
const MARGIN_RIGHT: f32 = 24.0;
const TOP: f32 = 22.0;
const BAR_HEIGHT: f32 = 6.0;
/// The combat timeline replaces the progress bar when a recording has combat events.
const GRAPH_HEIGHT: f32 = 14.0;
const GRAPH_QUIET: u32 = rgba(90, 78, 56, 255);
const GRAPH_PLAYED: u32 = rgba(226, 124, 60, 255);
const GRAPH_AHEAD: u32 = rgba(150, 70, 48, 255);
const PLAYHEAD: u32 = rgba(255, 244, 200, 255);
/// The heat thumbnail's side in pixels, and the caption above it.
const THUMB: f32 = 150.0;
const THUMB_CAPTION: f32 = 24.0;
const THUMB_BACK: u32 = rgba(22, 18, 12, 235);
const MAX_UNITS: usize = 4096;

const BACKGROUND: u32 = rgba(12, 9, 6, 200);
const TRACK: u32 = rgba(70, 60, 44, 255);
const FILL: u32 = rgba(226, 184, 68, 255);
const FILL_PAUSED: u32 = rgba(190, 96, 72, 255);
const WATCHED_ROW: u32 = rgba(226, 184, 68, 70);
const KEY_COLOUR: u32 = rgba(255, 214, 64, 255);
const DIM: u32 = rgba(140, 130, 112, 255);
const GONE: u32 = rgba(110, 100, 88, 255);

/// The colour the game gives a player: a shuffled index per player, then four floats per index
/// (the same tables the player-colours plugin reads). Falls back to grey for an unlikely index.
///
/// # Safety
/// Reads the game's colour tables, which are at least 8 entries.
unsafe fn player_colour(g: &Game, player: usize) -> u32 {
    unsafe {
        let index = usize::from(core::ptr::read_unaligned((g.colour_reorder + player % 8) as *const u8));
        if index >= 8 {
            return rgba(150, 150, 150, 255);
        }
        let f = |k: usize| core::ptr::read_unaligned((g.colour_rgba + (index * 4 + k) * 4) as *const f32);
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        rgba(byte(f(0)), byte(f(1)), byte(f(2)), 255)
    }
}

/// The totals page: left edge, width, heading.
const TOTAL_COLUMNS: [(f32, f32, &str); 7] = [
    (112.0, 34.0, "Kill"),
    (148.0, 34.0, "Lost"),
    (184.0, 34.0, "Raze"),
    (220.0, 36.0, "BLost"),
    (258.0, 36.0, "Gold+"),
    (296.0, 32.0, "Wood+"),
    (330.0, 28.0, "Oil+"),
];

/// The rates page: left edge, width, heading.
const RATE_COLUMNS: [(f32, f32, &str); 4] = [
    (112.0, 58.0, "Gold/m"),
    (172.0, 58.0, "Wood/m"),
    (232.0, 58.0, "Oil/m"),
    (292.0, 44.0, "APM"),
];

/// The current page: left edge, width, heading.
const COLUMNS: [(f32, f32, &str); 7] = [
    (112.0, 40.0, "Gold"),
    (154.0, 40.0, "Wood"),
    (196.0, 36.0, "Oil"),
    (234.0, 46.0, "Food"),
    (286.0, 22.0, "Arm"),
    (314.0, 22.0, "Wrk"),
    (342.0, 18.0, "Bld"),
];

pub fn install(symbols: &Symbols, log: &dyn Log) -> bool {
    let Some(width) = symbols.address("HudUiWidth") else {
        log.line(W2R_LOG_ERROR, "host", "replay panel: symbols missing");
        return false;
    };
    let Some(kit) = Kit::new_in(symbols, "HudCtx") else {
        log.line(W2R_LOG_ERROR, "host", "replay panel: screen-kit symbols missing");
        return false;
    };
    let get = |n: &str| symbols.address(n);
    let (
        Some(gold),
        Some(lumber),
        Some(oil),
        Some(food_used),
        Some(food_free),
        Some(food_cap),
        Some(units),
        Some(unit_count),
        Some(type_flags),
        Some(kills),
        Some(razed),
        Some(lost),
        Some(buildings_lost),
        Some(mined_gold),
        Some(mined_lumber),
        Some(mined_oil),
        Some(colour_reorder),
        Some(colour_rgba),
    ) = (
        get("PlayerGold"),
        get("PlayerLumber"),
        get("PlayerOil"),
        get("FoodA"),
        get("FoodB"),
        get("FoodCap"),
        get("UnitArrayPtr"),
        get("UnitCount"),
        get("UnitTypeFlags"),
        get("ScoreUnitsKilled"),
        get("ScoreBuildingsRazed"),
        get("ScoreUnitsLost"),
        get("ScoreBuildingsLost"),
        get("ScoreGoldMined"),
        get("ScoreLumberMined"),
        get("ScoreOilMined"),
        get("TeamColourReorder"),
        get("TeamColourRgba"),
    )
    else {
        log.line(W2R_LOG_ERROR, "host", "replay scoreboard: symbols missing");
        return false;
    };
    // SAFETY: `HudUiWidth` takes nothing and returns a float (Ghidra, build 2818).
    let ui_width = unsafe { core::mem::transmute::<usize, ExtentFn>(width) };
    let game = Game {
        gold,
        lumber,
        oil,
        food_used,
        food_free,
        food_cap,
        units,
        unit_count,
        type_flags,
        kills,
        razed,
        lost,
        buildings_lost,
        mined_gold,
        mined_lumber,
        mined_oil,
        colour_reorder,
        colour_rgba,
    };
    let _ = PANEL.set(Panel {
        kit,
        ui_width,
        game,
        preview: crate::previewdraw::PreviewGame::new(symbols).ok(),
    });
    log.line(W2R_LOG_INFO, "host", "replay panel ready");
    true
}

/// Called by the window hook just before the game starts its `game_hud` window: while a replay
/// plays, the panel is a small window of its own, begun and ended here (the chat plugin owns
/// every HUD function, so nothing is hooked inside the HUD). `begin` is the game's own
/// `BeginNamedWindow`, so this does not come back through the hook.
///
/// # Safety
/// `ctx` is the context the game is about to begin `game_hud` in.
pub unsafe fn before_hud(ctx: *mut c_void, begin: BeginFn) {
    let Some(pace) = replayplay::pace() else { return };
    let Some(panel) = PANEL.get() else { return };
    let result = guarded(|| unsafe {
        let mode = viewer::board_mode();
        // Read every frame, whatever page is up, so income has a history when its page opens.
        let rows = read_board(&panel.game);
        let mut mined = [[0u32; 3]; 8];
        for row in &rows {
            mined[usize::from(row.member.slot) % 8] = row.mined;
        }
        if let Ok(mut h) = HISTORY.lock() {
            h.get_or_insert_with(History::default)
                .record(pace.game_ms, mined);
        }
        let board = (mode < 3).then_some(rows);
        let header = header_height(&pace);
        let heat = replayplay::heat_view();
        let thumb = if heat.is_some() { THUMB_CAPTION + THUMB + 8.0 } else { 0.0 };
        let height = thumb
            + match &board {
                Some(rows) => header + ROW_HEIGHT * (rows.len() as f32 + 1.0) + 4.0,
                None => header,
            };
        let x = ((panel.ui_width)() - MARGIN_RIGHT - WIDTH).max(0.0);
        if begin(ctx, c"replay_panel".as_ptr(), x, TOP, WIDTH, height, WINDOW_FLAGS) != 0 {
            panel.kit.space_begin();
            draw(&pace, board.as_deref(), mode, x, height, heat.as_ref());
            panel.kit.space_end();
            panel.kit.end_window();
        }
    });
    if let Err(fault) = result {
        crate::session::host_fault("replay panel", &fault);
    }
}

/// Rows of key hints under the header: an observer's two, or in a replay one row of the keys
/// used most, or all three while the key list is shown (`?`).
fn hint_rows(pace: &Pace) -> usize {
    if pace.live {
        2
    } else if pace.help {
        3
    } else {
        1
    }
}

/// Where the hints start, window-local: under the timeline graph when there is one, otherwise
/// under the progress bar.
fn hints_top(pace: &Pace) -> f32 {
    if pace.live || pace.graph.is_empty() { 28.0 } else { 36.0 }
}

/// The header's height: the text line, the timeline or progress bar and the hint rows.
fn header_height(pace: &Pace) -> f32 {
    hints_top(pace) + ROW_HEIGHT.max(14.0) * hint_rows(pace) as f32 + 4.0
}

/// The combat timeline: one bar per slice of the recording, taller where more was lost, the part
/// already played in a warm colour and the rest darker, with a line at the playhead. `x` is the
/// window's left edge in screen coordinates.
fn draw_graph(kit: &Kit, pace: &Pace, x: f32) {
    let n = pace.graph.len();
    if n == 0 {
        return;
    }
    let area = Rect::new(x + 6.0, TOP + 18.0, WIDTH - 12.0, GRAPH_HEIGHT);
    kit.fill_rect(area, TRACK);
    let fraction = if pace.total == 0 { 0.0 } else { pace.turn as f32 / pace.total as f32 };
    let slot_w = area.w / n as f32;
    for (i, &h) in pace.graph.iter().enumerate() {
        let played = ((i + 1) as f32 / n as f32) <= fraction;
        let colour = match (h, played) {
            (0, _) => GRAPH_QUIET,
            (_, true) => GRAPH_PLAYED,
            (_, false) => GRAPH_AHEAD,
        };
        // quiet stretches get a hairline so the whole length of the game reads as a track
        let bar_h = if h == 0 { 2.0 } else { 2.0 + (GRAPH_HEIGHT - 2.0) * f32::from(h) / 255.0 };
        let left = area.x + i as f32 * slot_w;
        kit.fill_rect(Rect::new(left, area.y + GRAPH_HEIGHT - bar_h, (slot_w - 1.0).max(1.0), bar_h), colour);
    }
    kit.fill_rect(
        Rect::new((area.x + area.w * fraction - 1.0).max(area.x), area.y - 2.0, 2.0, GRAPH_HEIGHT + 4.0),
        PLAYHEAD,
    );
}

/// The heat thumbnail under the scoreboard: a square for the map, a haze over every tile where
/// units or buildings were lost. `y` is the window-local top of the caption.
fn draw_heat(kit: &Kit, preview: Option<&crate::previewdraw::PreviewGame>, heat: &replayplay::HeatView, x: f32, y: f32) {
    use w2r_replay::heat::{SIDE, ramp};
    let caption = if heat.whole { "Where the fighting was" } else { "Where the fighting is so far" };
    kit.label_at(Rect::new(6.0, y, WIDTH - 12.0, 14.0), caption, ALIGN_CENTRE);
    let area = Rect::new(x + (WIDTH - THUMB) / 2.0, TOP + y + THUMB_CAPTION, THUMB, THUMB);
    // The map picture with the heat over it; if that cannot be drawn, the heat on a dark square.
    if let Some(game) = preview {
        let req = crate::previewdraw::Request {
            area,
            path: heat.map_path.clone(),
            tileset: None,
            heat: Some(heat.image.clone()),
        };
        if crate::previewdraw::draw(game, kit, &req) {
            return;
        }
    }
    kit.fill_rect(area, THUMB_BACK);
    let side = heat.side.min(SIDE);
    let cell = THUMB / side as f32;
    for ty in 0..side {
        for tx in 0..side {
            let v = heat.image[ty * SIDE + tx];
            if v == 0 {
                continue;
            }
            let [r, g, b, a] = ramp(v);
            kit.fill_rect(
                Rect::new(area.x + tx as f32 * cell, area.y + ty as f32 * cell, cell, cell),
                rgba(r, g, b, a),
            );
        }
    }
}

/// One scoreboard row, read from the game's tables.
struct Row {
    member: Member,
    gold: u32,
    lumber: u32,
    oil: u32,
    food_used: u16,
    food_cap: u16,
    counts: Counts,
    kills: u16,
    lost: u16,
    razed: u16,
    buildings_lost: u16,
    mined: [u32; 3],
    /// The player's colour in this game.
    colour: u32,
    /// Actions per minute; only for people, and only once there is something to measure.
    apm: Option<u32>,
}

/// # Safety
/// Reads the game's player tables and unit array; the caller wraps it in `guarded`.
unsafe fn read_board(g: &Game) -> Vec<Row> {
    let mut counts = [Counts::default(); 8];
    unsafe {
        let base = core::ptr::read_unaligned(g.units as *const usize);
        let n = core::ptr::read_unaligned(g.unit_count as *const u32) as usize;
        if base != 0 {
            let flags_of = |ty: u8| {
                core::ptr::read_unaligned((g.type_flags + usize::from(ty) * 4) as *const u32)
            };
            for i in 0..n.min(MAX_UNITS) {
                let slot = core::slice::from_raw_parts(
                    (base + i * scoreboard::STRIDE) as *const u8,
                    scoreboard::STRIDE,
                );
                scoreboard::count(slot, flags_of, &mut counts);
            }
        }
    }
    viewer::roster()
        .into_iter()
        .map(|member| {
            let p = usize::from(member.slot);
            // SAFETY: per-player tables of at least 8 entries in the game's data section.
            unsafe {
                let u32_at = |base: usize| core::ptr::read_unaligned((base + p * 4) as *const u32);
                let u16_at = |base: usize| core::ptr::read_unaligned((base + p * 2) as *const u16);
                Row {
                    gold: u32_at(g.gold),
                    lumber: u32_at(g.lumber),
                    oil: u32_at(g.oil),
                    food_used: u16_at(g.food_used).wrapping_sub(u16_at(g.food_free)),
                    food_cap: u16_at(g.food_cap).min(200),
                    counts: counts[p],
                    kills: u16_at(g.kills),
                    lost: u16_at(g.lost),
                    razed: u16_at(g.razed),
                    buildings_lost: u16_at(g.buildings_lost),
                    mined: [u32_at(g.mined_gold), u32_at(g.mined_lumber), u32_at(g.mined_oil)],
                    colour: player_colour(g, p),
                    apm: if member.human {
                        if replayplay::is_live() {
                            crate::liveapm::apm(member.slot)
                        } else {
                            replayplay::actions_per_minute(member.slot)
                        }
                    } else {
                        None
                    },
                    member,
                }
            }
        })
        .collect()
}

/// Per player, kills + losses + razed buildings so far, for the auto-director.
pub fn combat_totals() -> Option<[u32; 8]> {
    let g = &PANEL.get()?.game;
    guarded(|| {
        let mut out = [0u32; 8];
        for (p, o) in out.iter_mut().enumerate() {
            // SAFETY: per-player u16 tables of at least 8 entries in the game's data section.
            let at = |base: usize| u32::from(unsafe { core::ptr::read_unaligned((base + p * 2) as *const u16) });
            *o = at(g.kills) + at(g.lost) + at(g.razed) + at(g.buildings_lost);
        }
        out
    })
    .ok()
}

/// One line of key hints, centred: each key in yellow followed by what it does. The font has
/// no width query, so the layout assumes `CHAR_WIDTH` per character.
fn hint_row(kit: &Kit, y: f32, items: &[(&str, &str)]) {
    const CHAR_WIDTH: f32 = 7.0;
    const GAP: usize = 3;
    let chars: usize = items
        .iter()
        .map(|(k, d)| k.len() + 1 + d.len())
        .sum::<usize>()
        + GAP * items.len().saturating_sub(1);
    let mut x = (WIDTH - chars as f32 * CHAR_WIDTH) / 2.0;
    for (key, what) in items {
        let key_w = key.len() as f32 * CHAR_WIDTH;
        kit.label_coloured_at(Rect::new(x, y, key_w * 2.0, 14.0), key, ALIGN_LEFT, KEY_COLOUR);
        let what_x = x + key_w + CHAR_WIDTH;
        let what_w = what.len() as f32 * CHAR_WIDTH * 2.0;
        kit.label_at(Rect::new(what_x, y, what_w, 14.0), what, ALIGN_LEFT);
        x = what_x + what.len() as f32 * CHAR_WIDTH + GAP as f32 * CHAR_WIDTH;
    }
}

/// `x` is the window's left edge: fills use screen coordinates, labels the window's own.
fn draw(
    pace: &Pace,
    board: Option<&[Row]>,
    mode: u8,
    x: f32,
    height: f32,
    heat: Option<&replayplay::HeatView>,
) {
    let Some(panel) = PANEL.get() else { return };
    let kit = &panel.kit;
    kit.fill_round(Rect::new(x, TOP, WIDTH, height), 3.0, BACKGROUND);

    let fraction = if pace.total == 0 {
        0.0
    } else {
        pace.turn as f32 / pace.total as f32
    };
    draw_graph(kit, pace, x);
    // The timeline has its own playhead; a recording without events gets the plain bar.
    if !pace.live && pace.graph.is_empty() {
        let bar = Rect::new(x + 6.0, TOP + 19.0, WIDTH - 12.0, BAR_HEIGHT);
        kit.fill_rect(bar, TRACK);
        let colour = if pace.paused || pace.ended {
            FILL_PAUSED
        } else {
            FILL
        };
        kit.fill_rect(Rect::new(bar.x, bar.y, bar.w * fraction, bar.h), colour);
    }

    let state = if pace.live {
        "OBSERVING".to_string()
    } else if pace.seeking {
        "JUMPING TO BATTLE".to_string()
    } else if pace.ended {
        "FINISHED".to_string()
    } else if pace.paused {
        format!("PAUSED  {}x", pace.speed)
    } else {
        format!("PLAYING  {}x", pace.speed)
    };
    let progress = if pace.live {
        let secs = pace.game_ms / 1000;
        format!("{}:{:02}", secs / 60, secs % 60)
    } else {
        format!(
            "turn {} / {}   {}%",
            pace.turn,
            pace.total,
            (fraction * 100.0) as u32
        )
    };
    kit.label_at(Rect::new(6.0, 2.0, WIDTH * 0.5, 14.0), &state, ALIGN_LEFT);
    kit.label_at(
        Rect::new(WIDTH * 0.5, 2.0, WIDTH * 0.5 - 6.0, 14.0),
        &progress,
        ALIGN_RIGHT,
    );
    if pace.ended {
        kit.label_at(
            Rect::new(0.0, hints_top(pace), WIDTH, 14.0),
            "Open the menu to leave",
            ALIGN_CENTRE,
        );
    } else if pace.live {
        hint_row(kit, 28.0, &[("`", "page"), ("End", if pace.auto { "auto on" } else { "auto" })]);
        hint_row(
            kit,
            42.0,
            &[("[ ]", "player"), ("Home", "all"), ("Ins", "vision"), (";", "follow")],
        );
    } else {
        let top = hints_top(pace);
        let battles = !pace.graph.is_empty();
        if pace.help {
            hint_row(kit, top, &[("PgUp/PgDn", "speed"), ("End", "pause"), ("Del", "step"), ("`", "page")]);
            hint_row(
                kit,
                top + 14.0,
                &[("[ ]", "player"), ("Home", "own"), ("Ins", "vision"), (";", "follow")],
            );
            if battles {
                hint_row(kit, top + 28.0, &[(".", "next battle"), ("/", "heat map"), ("?", "hide keys")]);
            } else {
                hint_row(kit, top + 28.0, &[("?", "hide keys")]);
            }
        } else if battles {
            hint_row(kit, top, &[("PgUp/PgDn", "speed"), ("End", "pause"), (".", "battle"), ("?", "keys")]);
        } else {
            hint_row(kit, top, &[("PgUp/PgDn", "speed"), ("End", "pause"), ("?", "keys")]);
        }
    }

    if let Some(heat) = heat {
        let rows_h = board.map_or(0.0, |r| ROW_HEIGHT * (r.len() as f32 + 1.0) + 4.0);
        draw_heat(kit, PANEL.get().and_then(|p| p.preview.as_ref()), heat, x, header_height(pace) + rows_h);
    }

    let Some(rows) = board else { return };
    let watching = viewer::watching();
    let top = header_height(pace);
    let columns: &[(f32, f32, &str)] = match mode {
        1 => &TOTAL_COLUMNS,
        2 => &RATE_COLUMNS,
        _ => &COLUMNS,
    };
    for &(rect_x, rect_w, title) in columns {
        kit.label_coloured_at(
            Rect::new(rect_x, top, rect_w, ROW_HEIGHT),
            title,
            ALIGN_RIGHT,
            DIM,
        );
    }
    kit.label_coloured_at(
        Rect::new(18.0, top, 90.0, ROW_HEIGHT),
        "Player",
        ALIGN_LEFT,
        DIM,
    );
    for (n, row) in rows.iter().enumerate() {
        let y = top + ROW_HEIGHT * (n as f32 + 1.0);
        let gone = row.counts.is_empty();
        if watching == Some(row.member.slot) {
            kit.fill_rect(
                Rect::new(x + 2.0, TOP + y, WIDTH - 4.0, ROW_HEIGHT),
                WATCHED_ROW,
            );
        }
        let swatch = row.colour;
        kit.fill_rect(
            Rect::new(x + 6.0, TOP + y + 2.0, 8.0, ROW_HEIGHT - 4.0),
            swatch,
        );
        let text = |value: String, column: usize| {
            let (cx, cw, _) = columns[column];
            let r = Rect::new(cx, y, cw, ROW_HEIGHT);
            if gone {
                kit.label_coloured_at(r, &value, ALIGN_RIGHT, GONE);
            } else {
                kit.label_at(r, &value, ALIGN_RIGHT);
            }
        };
        let name = Rect::new(18.0, y, 92.0, ROW_HEIGHT);
        if gone {
            kit.label_coloured_at(name, &row.member.name, ALIGN_LEFT, GONE);
        } else {
            kit.label_at(name, &row.member.name, ALIGN_LEFT);
        }
        let values: Vec<String> = match mode {
            1 => vec![
                row.kills.to_string(),
                row.lost.to_string(),
                row.razed.to_string(),
                row.buildings_lost.to_string(),
                row.mined[0].to_string(),
                row.mined[1].to_string(),
                row.mined[2].to_string(),
            ],
            2 => {
                let slot = usize::from(row.member.slot);
                let history = HISTORY.lock().ok();
                let rate = |kind: usize| {
                    history
                        .as_ref()
                        .and_then(|h| h.as_ref())
                        .and_then(|h| h.per_minute(slot, kind))
                        .map_or_else(|| "-".to_string(), |v| v.to_string())
                };
                vec![
                    rate(0),
                    rate(1),
                    rate(2),
                    row.apm.map_or_else(|| "-".to_string(), |v| v.to_string()),
                ]
            }
            _ => vec![
                row.gold.to_string(),
                row.lumber.to_string(),
                row.oil.to_string(),
                format!("{}/{}", row.food_used, row.food_cap),
                row.counts.army.to_string(),
                row.counts.workers.to_string(),
                row.counts.buildings.to_string(),
            ],
        };
        for (column, value) in values.into_iter().enumerate() {
            text(value, column);
        }
    }
}
