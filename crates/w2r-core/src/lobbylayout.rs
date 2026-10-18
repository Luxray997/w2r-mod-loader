//! The geometry and the small pure rules of the replacement multiplayer lobby screen
//! (docs/lobby-screen.md), ported from wc2r-plus `features/ui/multiplayer_lobby_screen.cpp`. No
//! game access here, so all of it is tested on any platform; [`crate::lobbyscreen`] draws with it.

use core::ffi::CStr;

use crate::screenkit::{
    CAPTION_H, FOOTER_BOTTOM_GAP, FOOTER_H, FRAME_PAD, GAP, NAME_H, ROW_H, Rect,
};
use crate::screenkit::{fl, footer_button_width, lobby_frame};

pub const SLOT_COUNT: usize = 8;

const SLOTS_FRACTION: f32 = 0.58;
const SLOT_SPACER_FRACTION: f32 = 0.08;
const LEFT_COLUMN_FRACTION: f32 = 0.58;
const COLUMN_GUTTER_FRACTION: f32 = 0.03;
const SLOT_NAME_SHARE: f32 = 0.56;
const SLOT_RACE_SHARE: f32 = 0.30;
const SLOT_TEAM_SHARE: f32 = 0.14;
const SLOT_EDGE_PIXELS: f32 = 10.0;
/// Space between the chat history and the input row.
pub const CHAT_TO_INPUT_GAP: f32 = 10.0;
/// The buttons ignore clicks this long after the lobby opens (`MpLobbyEnterMs`).
pub const ENTRY_DEAD_MS: i64 = 1500;

const SWATCH_FRACTION: f32 = 0.38;
const CROWN_SIZE_MULTIPLE: f32 = 2.0;
const CROWN_OVERHANG: f32 = 12.0;
const MARK_GAP: f32 = 6.0;

/// Longest slot, map or game name shown; the game's own buffers are shorter.
pub const MAX_TEXT: usize = 64;

/// Where each part of the screen goes, from the game's window rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub frame: Rect,
    pub slots: Rect,
    pub chat_history: Rect,
    pub chat_input: Rect,
    /// The right column: map name, preview and info rows.
    pub right: Rect,
    pub footer_y: f32,
}

pub fn layout(window: Rect) -> Layout {
    let frame = lobby_frame(window);
    let content_x = frame.x + FRAME_PAD;
    let content_right = frame.x + frame.w - FRAME_PAD;
    let content_w = content_right - content_x;
    let content_y = frame.y + FRAME_PAD;
    let footer_y = window.h - FOOTER_BOTTOM_GAP;
    let content_h = (footer_y - GAP) - content_y;

    let left_w = fl(content_w * LEFT_COLUMN_FRACTION);
    let gutter = fl(content_w * COLUMN_GUTTER_FRACTION);
    let right_x = content_x + left_w + gutter;
    let right_w = content_right - right_x;

    let slots_h = fl(content_h * SLOTS_FRACTION);
    let chat_y = fl(content_y + slots_h + GAP);
    let chat_h = (content_y + content_h) - chat_y;
    Layout {
        frame,
        slots: Rect::new(content_x, content_y, left_w, slots_h),
        chat_history: Rect::new(
            content_x,
            chat_y,
            left_w,
            fl(chat_h - ROW_H - CHAT_TO_INPUT_GAP),
        ),
        chat_input: Rect::new(content_x, fl(content_y + content_h - ROW_H), left_w, ROW_H),
        right: Rect::new(right_x, content_y, right_w, content_h),
        footer_y,
    }
}

/// Sizes of the marks drawn beside a slot row: the host's crown, the mod dot, the start swatch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkSizes {
    pub crown: f32,
    pub swatch: f32,
    /// Width reserved at the left of the rows for the marks.
    pub gutter: f32,
}

pub fn mark_sizes(row_h: f32) -> MarkSizes {
    let swatch = fl(row_h * SWATCH_FRACTION);
    let crown = fl(swatch * CROWN_SIZE_MULTIPLE);
    MarkSizes {
        crown,
        swatch,
        gutter: fl((crown - CROWN_OVERHANG) + MARK_GAP * 2.0 + swatch),
    }
}

/// The Players panel: a header row then eight slot rows, in a list that starts after the gutter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SlotGeometry {
    pub header_h: f32,
    pub row_h: f32,
    pub spacer: f32,
    pub marks: MarkSizes,
    /// The list itself (the panel without the gutter).
    pub rows: Rect,
    /// Name, edge, race, edge, team: fractions of the row.
    pub ratios: [f32; 5],
}

pub fn slot_geometry(panel: Rect) -> SlotGeometry {
    let header_h = CAPTION_H;
    let pitch = fl(((panel.h - header_h) / SLOT_COUNT as f32).max(0.0));
    let spacer = fl(pitch * SLOT_SPACER_FRACTION);
    let row_h = pitch - spacer;
    let marks = mark_sizes(row_h);
    let rows = Rect::new(
        fl(panel.x + marks.gutter),
        panel.y,
        fl(panel.w - marks.gutter),
        panel.h,
    );
    let edge = if rows.w > 0.0 {
        SLOT_EDGE_PIXELS / rows.w
    } else {
        0.0
    };
    let inner = 1.0 - edge * 2.0;
    SlotGeometry {
        header_h,
        row_h,
        spacer,
        marks,
        rows,
        ratios: [
            inner * SLOT_NAME_SHARE,
            edge,
            inner * SLOT_RACE_SHARE,
            edge,
            inner * SLOT_TEAM_SHARE,
        ],
    }
}

/// The chat input row: the edit field and the Send button after it.
pub fn chat_input_cells(r: Rect) -> (Rect, Rect) {
    let send_w = fl(360.0f32.min(r.w * 0.22));
    let edit_w = fl(r.w - send_w - GAP);
    (
        Rect::new(r.x, r.y, edit_w, r.h),
        Rect::new(r.x + edit_w + GAP, r.y, send_w, r.h),
    )
}

/// Leave, and for the host Start beside it, centred under the screen.
pub fn footer_buttons(window: Rect, footer_y: f32, host: bool) -> (Rect, Option<Rect>) {
    let w = footer_button_width(window);
    let total = if host { w * 2.0 } else { w };
    let x = fl((window.w - total) * 0.5);
    (
        Rect::new(x, footer_y, w, FOOTER_H),
        host.then_some(Rect::new(x + w, footer_y, w, FOOTER_H)),
    )
}

/// True while the lobby is younger than [`ENTRY_DEAD_MS`].
pub fn entry_dead(now_ms: i64, entered_ms: i64) -> bool {
    now_ms.saturating_sub(entered_ms) < ENTRY_DEAD_MS
}

/// Drops control characters and cuts to [`MAX_TEXT`] characters: names come from other players
/// and files, and must be harmless in the game's label functions.
pub fn clean_text(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEXT)
        .collect()
}

/// The digit shown for the team option at `index` in place of the game's own text. The ninth
/// option is the observer team, added only when the observer plugin is loaded.
pub fn team_digit(index: usize) -> Option<&'static CStr> {
    const DIGITS: [&CStr; 9] = [c"1", c"2", c"3", c"4", c"5", c"6", c"7", c"8", c"OBS"];
    DIGITS.get(index).copied()
}

pub const INFO_ROW_COUNT: usize = 8;
const LOCK_BUTTON_SHARE: f32 = 0.34;
const RESOURCE_MASK: u32 = 0x2_3000;
const TILESET_MASK: u32 = 0x1_c000;
/// The "peasants only" bit in the match flags.
pub const ONE_PEON_BIT: u32 = 0x200;

/// The right column: the map name, the square preview and the info rows at the bottom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RightColumn {
    pub name: Rect,
    pub preview: Rect,
    pub info: Rect,
    pub row_h: f32,
}

pub fn right_column(column: Rect) -> RightColumn {
    let info_h = CAPTION_H * INFO_ROW_COUNT as f32;
    let available = column.h - NAME_H - GAP - info_h - GAP;
    let side = fl(available.min(column.w).max(0.0));
    let info = Rect::new(column.x, fl(column.y + column.h - info_h), column.w, info_h);
    RightColumn {
        name: Rect::new(column.x, column.y, column.w, NAME_H),
        preview: Rect::new(
            fl(column.x + (column.w - side) * 0.5),
            fl(column.y + NAME_H + GAP),
            side,
            side,
        ),
        info,
        row_h: fl(info.h / INFO_ROW_COUNT as f32),
    }
}

/// Row `i` of the info column.
pub fn info_row(info: Rect, row_h: f32, i: usize) -> Rect {
    Rect::new(info.x, info.y + row_h * i as f32, info.w, row_h)
}

/// The host's flip button at the right end of a lock row.
pub fn lock_button(row: Rect) -> Rect {
    let w = fl(row.w * LOCK_BUTTON_SHARE);
    Rect::new(fl(row.x + row.w - w), row.y, w, row.h)
}

/// Which entry of the game's resources option list the match flags select.
pub fn resources_index(flags: u32) -> usize {
    match flags & RESOURCE_MASK {
        0x1000 => 2,
        0x2000 => 3,
        0x3000 => 4,
        0x2_0000 => 1,
        _ => 0,
    }
}

/// Which entry of the game's tileset option list the match flags select.
pub fn tileset_index(flags: u32) -> usize {
    match flags & TILESET_MASK {
        0x4000 => 1,
        0x8000 => 2,
        0xc000 => 3,
        0x1_c000 => 4,
        _ => 0,
    }
}

/// The name of a game speed setting (0 slowest to 8 fastest); out-of-range values show as numbers.
pub fn speed_name(speed: u8) -> String {
    speed_name_c(speed).map_or_else(|| speed.to_string(), |n| n.to_string_lossy().into_owned())
}

/// [`speed_name`] as a C string, for the game's own text calls; `None` outside 0..=8.
pub fn speed_name_c(speed: u8) -> Option<&'static core::ffi::CStr> {
    const NAMES: [&core::ffi::CStr; 9] = [
        c"Slowest",
        c"Even Slower",
        c"Slower",
        c"Slow",
        c"Normal",
        c"Fast",
        c"Faster",
        c"Even Faster",
        c"Fastest",
    ];
    NAMES.get(usize::from(speed)).copied()
}

/// A caption with exactly one trailing colon and no trailing blanks; empty stays empty.
pub fn with_colon(text: &str) -> String {
    let t = text.trim_end_matches([' ', '\t']);
    if t.is_empty() {
        String::new()
    } else if t.ends_with(':') {
        t.to_owned()
    } else {
        format!("{t}:")
    }
}

/// What the password row says: unknown until the host's message arrived, none when empty.
pub fn password_text(known: Option<&[u8]>) -> String {
    match known {
        None => "Unknown".to_owned(),
        Some([]) => "None".to_owned(),
        Some(p) => clean_text(&String::from_utf8_lossy(p)),
    }
}

/// The crown picture: 50 by 50 RGBA, from wc2r-plus `crown_image.h` (MIT).
pub const CROWN_SIZE: usize = 50;
pub const CROWN_RGBA: &[u8; CROWN_SIZE * CROWN_SIZE * 4] = include_bytes!("../assets/crown.rgba");

/// Game colours are packed `0xAABBGGRR`.
pub const fn pack(r: u8, g: u8, b: u8, a: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16) | ((a as u32) << 24)
}

/// The eight player colours, in slot order (wc2r-plus `team_colours.h`), for the fixed-start swatch.
pub const TEAM_COLOURS: [u32; SLOT_COUNT] = [
    pack(196, 40, 40, 255),
    pack(48, 72, 200, 255),
    pack(0, 152, 152, 255),
    pack(128, 48, 168, 255),
    pack(232, 132, 24, 255),
    pack(32, 32, 32, 255),
    pack(236, 236, 236, 255),
    pack(232, 212, 48, 255),
];
pub const CROWN_COLOUR: u32 = pack(240, 206, 74, 255);
pub const MOD_BADGE_COLOUR: u32 = pack(96, 200, 120, 255);
/// The dot for a player running the mod with different simulation plugins.
pub const MOD_BADGE_DIFFERS_COLOUR: u32 = pack(236, 170, 40, 255);
pub const MOD_BADGE_RING: u32 = pack(18, 36, 22, 255);
pub const MOD_BADGE_RING_WIDTH: f32 = 2.0;
const MOD_BADGE_ALONE: f32 = 0.55;
const MOD_BADGE_WITH_CROWN: f32 = 0.42;
const CROWN_BAND_FRACTION: f32 = 0.42;
const CROWN_PRONG_WIDTH: f32 = 0.24;
const CROWN_MIDDLE_TOP: f32 = 0.22;

/// The crown's square beside a slot, from the slot's gutter rectangle. It hangs a little left of
/// the gutter, centred on the row.
pub fn crown_rect(gutter: Rect, marks: MarkSizes) -> Rect {
    Rect::new(
        fl(gutter.x - CROWN_OVERHANG),
        fl(gutter.y + (gutter.h - marks.crown) * 0.5),
        marks.crown,
        marks.crown,
    )
}

/// The start-location swatch at the right end of the gutter.
pub fn swatch_rect(gutter: Rect, marks: MarkSizes) -> Rect {
    Rect::new(
        fl(gutter.x + gutter.w - MARK_GAP - marks.swatch),
        fl(gutter.y + (gutter.h - marks.swatch) * 0.5),
        marks.swatch,
        marks.swatch,
    )
}

/// The green mod dot: in the crown's corner when the player is the host, else where the crown
/// would be. `None` when it would be too small to show its ring.
pub fn mod_badge_rect(crown: Rect, with_crown: bool) -> Option<Rect> {
    let size = fl(crown.w
        * if with_crown {
            MOD_BADGE_WITH_CROWN
        } else {
            MOD_BADGE_ALONE
        });
    if size <= MOD_BADGE_RING_WIDTH * 2.0 {
        return None;
    }
    let (x, y) = if with_crown {
        (fl(crown.x + crown.w - size), fl(crown.y + crown.h - size))
    } else {
        (
            fl(crown.x + (crown.w - size) * 0.5),
            fl(crown.y + (crown.h - size) * 0.5),
        )
    };
    Some(Rect::new(x, y, size, size))
}

/// The crown drawn from rectangles when its texture cannot be made: a band and three prongs.
pub fn crown_fallback(r: Rect) -> [Rect; 4] {
    let band_h = fl(r.h * CROWN_BAND_FRACTION);
    let band_y = fl(r.y + r.h - band_h);
    let prong_w = fl(r.w * CROWN_PRONG_WIDTH);
    let middle_top = fl(r.y + r.h * CROWN_MIDDLE_TOP);
    [
        Rect::new(r.x, band_y, r.w, band_h),
        Rect::new(r.x, r.y, prong_w, band_y - r.y),
        Rect::new(
            fl(r.x + (r.w - prong_w) * 0.5),
            middle_top,
            prong_w,
            band_y - middle_top,
        ),
        Rect::new(fl(r.x + r.w - prong_w), r.y, prong_w, band_y - r.y),
    ]
}

/// What the mod dot says about a seat.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ModMark {
    /// Not running the mod, or not heard from.
    #[default]
    None,
    /// Running the mod with the same simulation plugins as us.
    Match,
    /// Running the mod with different simulation plugins.
    Differs,
}

/// Which lobby slots hold a player running the mod, and whether their simulation plugins match
/// ours: `peer` gives `Some(matches)` for a net slot that announced itself in the handshake.
/// Ourselves always match. `lobby_of_net` is the game's net slot to lobby slot table.
pub fn mod_slots(
    lobby_of_net: &[Option<u8>; SLOT_COUNT],
    peer: impl Fn(u32) -> Option<bool>,
    self_net_slot: Option<u32>,
) -> [ModMark; SLOT_COUNT] {
    let mut out = [ModMark::None; SLOT_COUNT];
    for (net, lobby) in lobby_of_net.iter().enumerate() {
        let status = if self_net_slot == Some(net as u32) {
            Some(true)
        } else {
            peer(net as u32)
        };
        if let (Some(matches), Some(l)) = (status, lobby)
            && (*l as usize) < SLOT_COUNT
        {
            out[*l as usize] = if matches {
                ModMark::Match
            } else {
                ModMark::Differs
            };
        }
    }
    out
}

/// The host's lobby slot: the seat of net slot 0 when it is present, else the present slot whose
/// name equals the host's name (case-insensitive).
pub fn host_slot(
    seat_of_net0: Option<u8>,
    present: &[bool; SLOT_COUNT],
    names: &[String; SLOT_COUNT],
    host_name: &str,
) -> Option<usize> {
    if let Some(s) = seat_of_net0
        && (s as usize) < SLOT_COUNT
        && present[s as usize]
    {
        return Some(s as usize);
    }
    if host_name.is_empty() {
        return None;
    }
    (0..SLOT_COUNT).find(|&i| present[i] && names[i].eq_ignore_ascii_case(host_name))
}

// ---- download visuals (slice 4) -------------------------------------------------------------

/// How long a finished download's full bar stays on screen.
pub const DONE_SHOWN_MS: u64 = 1000;
const BAR_THICKNESS: f32 = 0.07;
const BAR_MIN_THICKNESS: f32 = 4.0;
const BAR_SIDE_INSET: f32 = 0.14;
const BAR_BOTTOM_INSET: f32 = 0.10;
const BAR_SWEEP_SHARE: f32 = 0.25;
pub const BAR_SWEEP_MS: u64 = 1400;
pub const BAR_TRACK: u32 = pack(0, 0, 0, 160);
pub const BAR_FILL: u32 = pack(240, 206, 74, 255);
pub const BAR_DONE: u32 = pack(96, 200, 120, 255);
pub const BAR_NO_MAP: u32 = pack(200, 64, 52, 255);
pub const PANEL_BACK: u32 = pack(0, 0, 0, 170);
pub const PANEL_ROUNDING: f32 = 12.0;
const PANEL_PAD: f32 = 0.06;
pub const PANEL_BAR_H: f32 = 24.0;
pub const WAIT_DOT_MS: u64 = 400;
/// Notices in the chat history are this colour.
pub const NOTICE_COLOUR: u32 = pack(232, 196, 96, 255);
/// The game's "<name> joined" and "<name> left" lines are this colour.
pub const PRESENCE_COLOUR: u32 = pack(160, 160, 160, 255);

/// Whether a line of the game's chat history is a join or leave line, "<name> Joined" or
/// "<name> Left", with no `:` (a player's own chat line is "name: text").
pub fn is_presence_line(text: &str) -> bool {
    !text.contains(':') && (text.ends_with(" Joined") || text.ends_with(" Left"))
}

/// Most notices kept; the oldest go first.
pub const MAX_NOTICES: usize = 32;
/// The lobby window has to go undrawn this long before notices are forgotten.
pub const NOTICE_GAP_MS: u64 = 2000;

/// What a slot's bar shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarKind {
    None,
    /// The player has been asked and has not answered: a sweeping segment.
    Deciding,
    Downloading(u8),
    Done,
    NoMap,
}

/// Maps the host's per-slot state onto a bar. `done_for_ms` is how long the slot has been `Done`.
pub fn bar_kind(state: crate::mapwire::SlotState, percent: u8, done_for_ms: u64) -> BarKind {
    use crate::mapwire::SlotState as S;
    match state {
        S::Offered => BarKind::Deciding,
        S::Uploading => BarKind::Downloading(percent.min(100)),
        S::Done if done_for_ms < DONE_SHOWN_MS => BarKind::Done,
        S::NoMap => BarKind::NoMap,
        _ => BarKind::None,
    }
}

/// The bar's track along the bottom of a slot's name cell; `None` when the cell is too small.
pub fn bar_track(cell: Rect) -> Option<Rect> {
    if cell.w <= 0.0 || cell.h <= 0.0 {
        return None;
    }
    let thick = fl((cell.h * BAR_THICKNESS).max(BAR_MIN_THICKNESS));
    let side = fl(cell.h * BAR_SIDE_INSET);
    let w = fl(cell.w - side * 2.0);
    if w <= thick * 2.0 {
        return None;
    }
    Some(Rect::new(
        fl(cell.x + side),
        fl(cell.y + cell.h - cell.h * BAR_BOTTOM_INSET - thick),
        w,
        thick,
    ))
}

/// The coloured part of a bar inside its track, and its colour. `None` when nothing shows.
pub fn bar_fill(track: Rect, kind: BarKind, clock_ms: u64) -> Option<(Rect, u32)> {
    let thick = track.h;
    match kind {
        BarKind::None => None,
        BarKind::Deciding => {
            let seg = fl(track.w * BAR_SWEEP_SHARE);
            let t = (clock_ms % BAR_SWEEP_MS) as f32 / BAR_SWEEP_MS as f32;
            Some((
                Rect::new(fl(track.x + (track.w - seg) * t), track.y, seg, thick),
                BAR_FILL,
            ))
        }
        BarKind::Downloading(percent) => {
            let fw = fl(track.w * f32::from(percent.min(100)) / 100.0);
            (fw > 0.0).then(|| (Rect::new(track.x, track.y, fw.max(thick), thick), BAR_FILL))
        }
        BarKind::Done => Some((track, BAR_DONE)),
        BarKind::NoMap => Some((track, BAR_NO_MAP)),
    }
}

/// Inside of the panel that covers the preview.
pub fn panel_inner(panel: Rect) -> Rect {
    let pad = fl(panel.w * PANEL_PAD);
    Rect::new(fl(panel.x + pad), panel.y, fl(panel.w - pad * 2.0), panel.h)
}

/// "Looking for map" with one to three dots that cycle.
pub fn waiting_text(clock_ms: u64) -> String {
    const DOTS: [&str; 4] = ["", ".", "..", "..."];
    format!(
        "Looking for map{}",
        DOTS[((clock_ms / WAIT_DOT_MS) % 4) as usize]
    )
}

/// The parts of the "download this map?" panel: map name, size, seconds left and the button.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AskPanel {
    pub name: Rect,
    pub size: Rect,
    pub remaining: Rect,
    pub button: Rect,
}

pub fn ask_panel(panel: Rect) -> AskPanel {
    let inner = panel_inner(panel);
    let total = CAPTION_H * 3.0 + ROW_H + GAP * 2.0;
    let top = fl(panel.y + ((panel.h - total) * 0.5).max(0.0));
    AskPanel {
        name: Rect::new(inner.x, top, inner.w, CAPTION_H),
        size: Rect::new(inner.x, top + CAPTION_H, inner.w, CAPTION_H),
        remaining: Rect::new(inner.x, top + CAPTION_H * 2.0, inner.w, CAPTION_H),
        button: Rect::new(inner.x, top + CAPTION_H * 3.0 + GAP * 2.0, inner.w, ROW_H),
    }
}

pub fn ask_texts(map_name: &str, size_bytes: u32, ms_left: u64) -> (String, String, String) {
    (
        format!("Download {}?", clean_text(map_name)),
        format!("{} KB", size_bytes.div_ceil(1024)),
        format!("{} seconds remaining", ms_left.div_ceil(1000)),
    )
}

/// The tooltip over the Start button while players are still downloading.
pub fn pending_text(names: &[String]) -> String {
    let mut list = String::new();
    for n in names {
        let who = if n.is_empty() { "a player" } else { n };
        if !list.is_empty() {
            list.push_str(", ");
        }
        list.push_str(who);
        if list.len() >= 220 {
            break;
        }
    }
    format!("Pending map download from: {list}")
}

/// The loader's name and number from the version a player announced. Builds before the rename
/// announce "w2rpm x.y.z"; today's say "W2R ML x.y.z" (the field holds 16 bytes). The number is
/// `None` when the text does not carry one.
pub fn loader_name(announced: &str) -> (String, Option<String>) {
    let text = clean_text(announced);
    for prefix in ["W2R ML", "W2R Mod Loader", "w2rpm"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            let number = rest.trim();
            return if number.is_empty() {
                ("W2R Mod Loader".to_owned(), None)
            } else {
                (format!("W2R Mod Loader {number}"), Some(number.to_owned()))
            };
        }
    }
    if text.is_empty() {
        ("W2R Mod Loader".to_owned(), None)
    } else {
        (text, None)
    }
}

/// The tooltip over a seat's mod dot: which loader the player runs, whether the plugins match
/// yours, and, when the loader versions differ, both numbers. `None` for a seat without a dot.
/// `own` is the version this machine announces.
pub fn mod_tip(mark: ModMark, version: &str, own: &str) -> Option<String> {
    let (name, theirs) = loader_name(version);
    let mut tip = match mark {
        ModMark::None => return None,
        ModMark::Match => format!("{name}. Same plugins as yours."),
        ModMark::Differs => {
            format!("{name}. Different plugins from yours: the game may desync.")
        }
    };
    if let (Some(theirs), (_, Some(mine))) = (theirs, loader_name(own))
        && theirs != mine
    {
        tip.push_str(&format!(" You have {mine}, they have {theirs}."));
    }
    Some(tip)
}

/// Where a tooltip of `size` goes for a mouse at `(mx, my)` inside `window`: up and to the right
/// of the cursor, pulled back inside the window.
pub fn tooltip_origin(mouse: (f32, f32), size: (f32, f32), window: Rect) -> (f32, f32) {
    const OFFSET: f32 = 44.0;
    let mut x = mouse.0 + OFFSET;
    let mut y = mouse.1 - OFFSET - size.1;
    if x + size.0 > window.w {
        x = window.w - size.0;
    }
    x = x.max(0.0);
    y = y.max(0.0);
    (x, y)
}

pub fn contains(r: Rect, p: (f32, f32)) -> bool {
    r.w > 0.0 && p.0 >= r.x && p.0 < r.x + r.w && p.1 >= r.y && p.1 < r.y + r.h
}

/// A line the lobby shows in the chat history that the game did not put there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    /// The number of chat lines the game had when it arrived: it shows before the next one.
    pub anchor: usize,
}

/// Keeps the notices, newest last, at most [`MAX_NOTICES`].
#[derive(Debug, Default)]
pub struct Notices(Vec<Notice>);

impl Notices {
    pub const fn new() -> Notices {
        Notices(Vec::new())
    }

    pub fn push(&mut self, text: String, anchor: usize) {
        self.0.push(Notice { text, anchor });
        if self.0.len() > MAX_NOTICES {
            self.0.remove(0);
        }
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn all(&self) -> &[Notice] {
        &self.0
    }
}

/// One line of the history as drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub text: String,
    pub notice: bool,
}

/// Interleaves notices into the game's chat lines (each with its index in the game's list): a
/// notice appears before the first line whose index is at least its anchor, the rest at the end.
pub fn merge_notices(lines: &[(usize, String)], notices: &[Notice]) -> Vec<Row> {
    let mut out = Vec::with_capacity(lines.len() + notices.len());
    let mut next = 0;
    for (index, text) in lines {
        while next < notices.len() && notices[next].anchor <= *index {
            out.push(Row {
                text: notices[next].text.clone(),
                notice: true,
            });
            next += 1;
        }
        out.push(Row {
            text: text.clone(),
            notice: false,
        });
    }
    for n in &notices[next..] {
        out.push(Row {
            text: n.text.clone(),
            notice: true,
        });
    }
    out
}

/// The notice for a lock change the host made.
pub fn lock_notice(teams: bool, locked: bool) -> &'static str {
    match (teams, locked) {
        (true, true) => "Teams locked by host",
        (true, false) => "Teams unlocked by host",
        (false, true) => "Slots locked by host",
        (false, false) => "Slots unlocked by host",
    }
}

/// Keeps the chat history at the bottom when it was there and grew, and notices when something
/// cleared the frame mid-way (wc2r-plus `lobby_chat.cpp`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollTracker {
    pub scroll: f32,
    max_offset: f32,
    content_h: f32,
    last_seq: u32,
    seq_valid: bool,
}

impl ScrollTracker {
    pub const fn new() -> ScrollTracker {
        ScrollTracker {
            scroll: 0.0,
            max_offset: 0.0,
            content_h: 0.0,
            last_seq: 0,
            seq_valid: false,
        }
    }

    /// True when the context's sequence number jumped by more than one since the last call: the
    /// game cleared the frame's scroll state and the saved offset must be put back.
    pub fn wiped(&mut self, seq: u32) -> bool {
        let wiped = self.seq_valid && seq.wrapping_sub(self.last_seq) > 1;
        self.last_seq = seq;
        self.seq_valid = true;
        wiped
    }

    /// After the rows are laid out: returns true when `scroll` moved and must be written back
    /// (the view was at the bottom and the content grew).
    pub fn follow(&mut self, content_h: f32, view_h: f32, line_h: f32) -> bool {
        let max_offset = (content_h - view_h).max(0.0);
        let at_bottom = self.scroll >= self.max_offset - line_h * 0.5;
        self.max_offset = max_offset;
        let grew = if self.content_h > 0.0 {
            content_h - self.content_h
        } else {
            0.0
        };
        self.content_h = content_h;
        if at_bottom && grew > 0.0 {
            self.scroll += grew;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_lines_are_told_from_chat() {
        assert!(is_presence_line("Bob Joined"));
        assert!(is_presence_line("Bob Left"));
        assert!(!is_presence_line("Bob: I Left"));
        assert!(!is_presence_line("Teams locked by host"));
    }

    const WINDOW: Rect = Rect::new(0.0, 0.0, 3840.0, 2160.0);

    #[test]
    fn the_columns_fill_the_frame_and_stay_apart() {
        let l = layout(WINDOW);
        assert_eq!(l.frame, lobby_frame(WINDOW));
        assert_eq!(l.slots.x, l.frame.x + FRAME_PAD);
        assert!(
            l.slots.x + l.slots.w < l.right.x,
            "a gutter between the columns"
        );
        assert_eq!(l.right.x + l.right.w, l.frame.x + l.frame.w - FRAME_PAD);
        assert_eq!(l.footer_y, 2160.0 - FOOTER_BOTTOM_GAP);
    }

    #[test]
    fn slots_chat_and_input_stack_in_the_left_column_without_overlap() {
        let l = layout(WINDOW);
        assert!(l.slots.y + l.slots.h < l.chat_history.y);
        assert!(l.chat_history.y + l.chat_history.h + CHAT_TO_INPUT_GAP <= l.chat_input.y);
        let content_bottom = l.frame.y + FRAME_PAD + (l.footer_y - GAP - (l.frame.y + FRAME_PAD));
        assert_eq!(l.chat_input.y + l.chat_input.h, content_bottom);
        assert_eq!(l.chat_history.w, l.slots.w);
        assert_eq!(l.chat_input.w, l.slots.w);
    }

    #[test]
    fn eight_rows_and_a_header_fit_the_slots_panel() {
        let g = slot_geometry(layout(WINDOW).slots);
        let used = g.header_h + (g.row_h + g.spacer) * SLOT_COUNT as f32;
        assert!(used <= layout(WINDOW).slots.h);
        assert!(g.row_h > 0.0 && g.spacer >= 0.0);
        assert!(g.rows.x > layout(WINDOW).slots.x, "the gutter is reserved");
    }

    #[test]
    fn the_column_ratios_add_up_to_one() {
        let g = slot_geometry(layout(WINDOW).slots);
        let sum: f32 = g.ratios.iter().sum();
        assert!((sum - 1.0).abs() < 1e-4, "{sum}");
    }

    #[test]
    fn a_tiny_panel_does_not_produce_nan_or_negative_sizes() {
        let g = slot_geometry(Rect::new(0.0, 0.0, 5.0, 10.0));
        assert!(g.ratios.iter().all(|r| r.is_finite()));
        assert!(g.marks.gutter >= 0.0);
        let g = slot_geometry(Rect::new(0.0, 0.0, 0.0, 0.0));
        assert!(g.ratios.iter().all(|r| r.is_finite()));
    }

    #[test]
    fn the_input_row_is_an_edit_field_then_send() {
        let r = Rect::new(100.0, 1500.0, 2000.0, ROW_H);
        let (edit, send) = chat_input_cells(r);
        assert_eq!(send.w, 360.0f32.min(2000.0 * 0.22));
        assert_eq!(edit.x, r.x);
        assert_eq!(send.x, edit.x + edit.w + GAP);
        assert_eq!(send.x + send.w, r.x + r.w);
        let (_, narrow_send) = chat_input_cells(Rect::new(0.0, 0.0, 1000.0, ROW_H));
        assert_eq!(narrow_send.w, 220.0);
    }

    #[test]
    fn the_host_gets_start_beside_leave_and_both_are_centred() {
        let (leave, start) = footer_buttons(WINDOW, 1930.0, true);
        let start = start.expect("host has Start");
        assert_eq!(start.x, leave.x + leave.w);
        assert_eq!((leave.x + leave.w + start.w + leave.x), 3840.0, "centred");
        let (alone, none) = footer_buttons(WINDOW, 1930.0, false);
        assert!(none.is_none());
        assert_eq!(
            alone.x,
            fl((3840.0 - alone.w) * 0.5),
            "centred, to the pixel below"
        );
    }

    #[test]
    fn buttons_are_dead_for_the_first_second_and_a_half() {
        assert!(entry_dead(1499, 0));
        assert!(!entry_dead(1500, 0));
        assert!(!entry_dead(5000, 0));
        assert!(entry_dead(10, 20), "a clock that went backwards stays dead");
    }

    #[test]
    fn hostile_text_loses_control_characters_and_is_cut() {
        assert_eq!(clean_text("Ann\u{7}\n\u{1b}[31m"), "Ann[31m");
        let long: String = "x".repeat(500);
        assert_eq!(clean_text(&long).len(), MAX_TEXT);
        assert_eq!(clean_text("plain"), "plain");
    }

    #[test]
    fn the_history_follows_new_lines_only_from_the_bottom() {
        let mut s = ScrollTracker::default();
        // First layout: nothing to compare with, so no jump.
        assert!(!s.follow(1000.0, 400.0, 40.0));
        // At the bottom (scroll tracks max offset 600) and 80 more pixels arrive: follow.
        s.scroll = 600.0;
        assert!(!s.follow(1000.0, 400.0, 40.0), "no growth, nothing to do");
        assert!(s.follow(1080.0, 400.0, 40.0));
        assert_eq!(s.scroll, 680.0);
        // The reader scrolled up; new lines must not drag the view down.
        s.scroll = 100.0;
        assert!(!s.follow(1160.0, 400.0, 40.0));
        assert_eq!(s.scroll, 100.0);
    }

    #[test]
    fn a_short_history_never_has_a_negative_range() {
        let mut s = ScrollTracker::default();
        assert!(!s.follow(100.0, 400.0, 40.0));
        assert!(
            s.follow(140.0, 400.0, 40.0),
            "at the bottom of nothing, still follows"
        );
        assert_eq!(s.scroll, 40.0);
    }

    #[test]
    fn a_jump_in_the_sequence_means_the_frame_was_cleared() {
        let mut s = ScrollTracker::default();
        assert!(!s.wiped(10), "the first sight proves nothing");
        assert!(!s.wiped(11));
        assert!(!s.wiped(11), "a repeat is not a wipe");
        assert!(s.wiped(15));
        assert!(!s.wiped(16));
        let mut w = ScrollTracker::default();
        assert!(!w.wiped(u32::MAX));
        assert!(!w.wiped(0), "wrapping by one is normal");
    }

    #[test]
    fn the_right_column_stacks_name_preview_and_seven_rows() {
        let l = layout(WINDOW);
        let c = right_column(l.right);
        assert_eq!(c.name.y, l.right.y);
        assert!(c.preview.y >= c.name.y + c.name.h);
        assert_eq!(c.preview.w, c.preview.h);
        assert!(
            c.preview.y + c.preview.h + GAP <= c.info.y,
            "the preview clears the info rows"
        );
        assert_eq!(c.info.y + c.info.h, l.right.y + l.right.h);
        let last = info_row(c.info, c.row_h, INFO_ROW_COUNT - 1);
        assert!(last.y + last.h <= c.info.y + c.info.h);
        assert!(c.preview.x >= l.right.x && c.preview.x + c.preview.w <= l.right.x + l.right.w);
        let tiny = right_column(Rect::new(0.0, 0.0, 100.0, 100.0));
        assert_eq!(tiny.preview.w, 0.0, "never negative");
    }

    #[test]
    fn the_lock_button_sits_at_the_right_end_of_its_row() {
        let row = Rect::new(100.0, 500.0, 1000.0, 90.0);
        let b = lock_button(row);
        assert_eq!(b.x + b.w, row.x + row.w);
        assert_eq!((b.y, b.h), (row.y, row.h));
        assert_eq!(b.w, 340.0);
    }

    #[test]
    fn the_match_flags_pick_the_resources_and_tileset_entries() {
        assert_eq!(resources_index(0), 0);
        assert_eq!(resources_index(0x1000), 2);
        assert_eq!(resources_index(0x2000), 3);
        assert_eq!(resources_index(0x3000), 4);
        assert_eq!(resources_index(0x2_0000), 1);
        assert_eq!(
            resources_index(0x1000 | 0x200 | 0x4000),
            2,
            "other bits are ignored"
        );
        assert_eq!(tileset_index(0), 0);
        assert_eq!(tileset_index(0x4000), 1);
        assert_eq!(tileset_index(0x8000), 2);
        assert_eq!(tileset_index(0xc000), 3);
        assert_eq!(tileset_index(0x1_c000), 4);
        assert_eq!(tileset_index(0x1000), 0);
    }

    #[test]
    fn captions_end_in_exactly_one_colon() {
        assert_eq!(with_colon("Game speed"), "Game speed:");
        assert_eq!(with_colon("Resources: "), "Resources:");
        assert_eq!(with_colon("Units:"), "Units:");
        assert_eq!(with_colon("  "), "");
        assert_eq!(with_colon(""), "");
    }

    #[test]
    fn the_password_row_distinguishes_unknown_none_and_a_password() {
        assert_eq!(password_text(None), "Unknown");
        assert_eq!(password_text(Some(b"")), "None");
        assert_eq!(password_text(Some(b"hunter2")), "hunter2");
        assert_eq!(
            password_text(Some(b"a\x07b")),
            "ab",
            "control characters are dropped"
        );
    }

    #[test]
    fn the_crown_art_is_the_size_it_says_and_not_blank() {
        assert_eq!(CROWN_RGBA.len(), CROWN_SIZE * CROWN_SIZE * 4);
        assert!(CROWN_RGBA.chunks(4).any(|p| p[3] > 0), "some opaque pixels");
        assert!(
            CROWN_RGBA.chunks(4).any(|p| p[3] == 0),
            "some transparent pixels"
        );
    }

    #[test]
    fn marks_sit_inside_or_just_left_of_the_gutter_and_centre_on_the_row() {
        let row_h = 100.0;
        let marks = mark_sizes(row_h);
        let gutter = Rect::new(600.0, 800.0, marks.gutter, row_h);
        let crown = crown_rect(gutter, marks);
        assert_eq!(crown.x, gutter.x - 12.0);
        assert_eq!(crown.w, crown.h);
        assert_eq!(crown.y + crown.h * 0.5, gutter.y + gutter.h * 0.5);
        let sw = swatch_rect(gutter, marks);
        assert_eq!(sw.x + sw.w + 6.0, gutter.x + gutter.w);
        assert!(
            sw.x >= crown.x + crown.w,
            "the swatch does not sit on the crown"
        );
    }

    #[test]
    fn the_mod_dot_is_a_corner_badge_on_a_crown_and_centred_without_one() {
        let crown = Rect::new(100.0, 100.0, 80.0, 80.0);
        let with = mod_badge_rect(crown, true).unwrap();
        assert_eq!((with.x + with.w, with.y + with.h), (180.0, 180.0));
        let alone = mod_badge_rect(crown, false).unwrap();
        assert_eq!(alone.x + alone.w * 0.5, crown.x + crown.w * 0.5);
        assert!(alone.w > with.w);
        assert_eq!(mod_badge_rect(Rect::new(0.0, 0.0, 8.0, 8.0), true), None);
    }

    #[test]
    fn the_fallback_crown_stays_inside_its_square() {
        let r = Rect::new(10.0, 20.0, 80.0, 80.0);
        for p in crown_fallback(r) {
            assert!(p.x >= r.x && p.y >= r.y && p.x + p.w <= r.x + r.w && p.y + p.h <= r.y + r.h);
            assert!(p.w > 0.0 && p.h >= 0.0);
        }
    }

    #[test]
    fn mod_slots_follow_announcements_and_ourselves() {
        // Net slots 0 to 3 sit in lobby slots 2, 0, 5 and 7; net slot 1 announced, we are net 0.
        let table = [Some(2), Some(0), Some(5), Some(7), None, None, None, None];
        use ModMark::{Differs, Match, None as No};
        let got = mod_slots(&table, |n| (n == 1).then_some(true), Some(0));
        assert_eq!(got, [Match, No, Match, No, No, No, No, No]);
        assert_eq!(mod_slots(&table, |_| None, None), [No; 8]);
        let differs = mod_slots(&table, |n| (n == 1).then_some(false), Some(0));
        assert_eq!(differs[0], Differs, "net slot 1 sits in lobby slot 0");
        assert_eq!(differs[2], Match, "ourselves always match");
        let bad = [Some(9), None, None, None, None, None, None, None];
        assert_eq!(
            mod_slots(&bad, |_| Some(true), None),
            [No; 8],
            "an out-of-range seat is ignored"
        );
    }

    #[test]
    fn the_host_is_net_slot_zero_or_found_by_name() {
        let mut present = [false; 8];
        present[1] = true;
        present[4] = true;
        let mut names: [String; 8] = Default::default();
        names[1] = "Ann".into();
        names[4] = "Bob".into();
        assert_eq!(host_slot(Some(4), &present, &names, "ann"), Some(4));
        assert_eq!(host_slot(Some(3), &present, &names, "ann"), Some(1));
        assert_eq!(host_slot(None, &present, &names, "BOB"), Some(4));
        assert_eq!(host_slot(None, &present, &names, ""), None);
        assert_eq!(host_slot(None, &present, &names, "Cy"), None);
        assert_eq!(host_slot(Some(200), &present, &names, "Cy"), None);
    }

    #[test]
    fn slot_states_map_onto_bars_as_in_wc2r_plus() {
        use crate::mapwire::SlotState as S;
        assert_eq!(bar_kind(S::None, 0, 0), BarKind::None);
        assert_eq!(bar_kind(S::Offered, 0, 0), BarKind::Deciding);
        assert_eq!(bar_kind(S::Uploading, 40, 0), BarKind::Downloading(40));
        assert_eq!(bar_kind(S::Uploading, 250, 0), BarKind::Downloading(100));
        assert_eq!(bar_kind(S::Done, 100, 999), BarKind::Done);
        assert_eq!(bar_kind(S::Done, 100, 1000), BarKind::None, "done fades");
        assert_eq!(bar_kind(S::NoMap, 0, 0), BarKind::NoMap);
        for s in [S::Stalled, S::LeftNoMap, S::NoAnswer] {
            assert_eq!(bar_kind(s, 0, 0), BarKind::None);
        }
    }

    #[test]
    fn the_bar_track_sits_at_the_bottom_of_the_cell_and_never_overflows_it() {
        let cell = Rect::new(100.0, 200.0, 900.0, 100.0);
        let t = bar_track(cell).unwrap();
        assert!(t.x >= cell.x && t.x + t.w <= cell.x + cell.w);
        assert!(t.y >= cell.y && t.y + t.h <= cell.y + cell.h);
        assert!(t.h >= 4.0);
        assert_eq!(
            bar_track(Rect::new(0.0, 0.0, 20.0, 100.0)),
            None,
            "too narrow"
        );
        assert_eq!(bar_track(Rect::default()), None);
    }

    #[test]
    fn bar_fills_follow_progress_and_stay_inside_the_track() {
        let track = Rect::new(0.0, 0.0, 400.0, 8.0);
        assert_eq!(bar_fill(track, BarKind::None, 0), None);
        assert_eq!(bar_fill(track, BarKind::Downloading(0), 0), None);
        let (half, c) = bar_fill(track, BarKind::Downloading(50), 0).unwrap();
        assert_eq!((half.w, c), (200.0, BAR_FILL));
        let (tiny, _) = bar_fill(track, BarKind::Downloading(1), 0).unwrap();
        assert_eq!(tiny.w, 8.0, "never thinner than the bar is tall");
        assert_eq!(bar_fill(track, BarKind::Done, 0), Some((track, BAR_DONE)));
        assert_eq!(
            bar_fill(track, BarKind::NoMap, 0),
            Some((track, BAR_NO_MAP))
        );
        for ms in [0, 350, 700, 1399, 99_999] {
            let (seg, _) = bar_fill(track, BarKind::Deciding, ms).unwrap();
            assert!(
                seg.x >= track.x && seg.x + seg.w <= track.x + track.w,
                "{ms}"
            );
        }
        let (a, _) = bar_fill(track, BarKind::Deciding, 0).unwrap();
        let (b, _) = bar_fill(track, BarKind::Deciding, 700).unwrap();
        assert!(b.x > a.x, "the segment sweeps");
    }

    #[test]
    fn the_waiting_text_cycles_its_dots() {
        assert_eq!(waiting_text(0), "Looking for map");
        assert_eq!(waiting_text(400), "Looking for map.");
        assert_eq!(waiting_text(1200), "Looking for map...");
        assert_eq!(waiting_text(1600), "Looking for map");
    }

    #[test]
    fn the_ask_panel_stacks_four_parts_inside_the_preview_square() {
        let panel = Rect::new(2000.0, 500.0, 900.0, 900.0);
        let a = ask_panel(panel);
        for r in [a.name, a.size, a.remaining, a.button] {
            assert!(r.x >= panel.x && r.x + r.w <= panel.x + panel.w);
            assert!(r.y >= panel.y && r.y + r.h <= panel.y + panel.h);
        }
        assert!(a.name.y + a.name.h <= a.size.y);
        assert!(a.size.y + a.size.h <= a.remaining.y);
        assert!(a.remaining.y + a.remaining.h <= a.button.y);
    }

    #[test]
    fn the_ask_texts_round_up_and_clean_the_name() {
        let (name, size, left) = ask_texts("Ridge\u{7}", 1025, 14_001);
        assert_eq!(name, "Download Ridge?");
        assert_eq!(size, "2 KB");
        assert_eq!(left, "15 seconds remaining");
        assert_eq!(ask_texts("m", 0, 0).1, "0 KB");
    }

    #[test]
    fn the_mod_dot_tooltip_names_the_version_and_the_match() {
        assert_eq!(mod_tip(ModMark::None, "W2R ML 1.1.2", "W2R ML 1.1.2"), None);
        let same = mod_tip(ModMark::Match, "W2R ML 1.1.2", "W2R ML 1.1.2").unwrap();
        assert_eq!(same, "W2R Mod Loader 1.1.2. Same plugins as yours.");
        let differs = mod_tip(ModMark::Differs, "", "W2R ML 1.1.2").unwrap();
        assert!(differs.starts_with("W2R Mod Loader. ") && differs.contains("Different plugins"));
    }

    #[test]
    fn builds_from_before_the_rename_show_under_the_new_name() {
        assert_eq!(
            loader_name("w2rpm 1.1.1"),
            ("W2R Mod Loader 1.1.1".to_owned(), Some("1.1.1".to_owned()))
        );
        assert_eq!(loader_name("W2R ML 1.1.2").1.as_deref(), Some("1.1.2"));
        assert_eq!(loader_name("w2rpm"), ("W2R Mod Loader".to_owned(), None));
        assert_eq!(loader_name("wc2r-plus 3").0, "wc2r-plus 3");
    }

    #[test]
    fn the_tooltip_says_when_the_loader_versions_differ() {
        let tip = mod_tip(ModMark::Match, "w2rpm 1.1.1", "W2R ML 1.1.2").unwrap();
        assert_eq!(
            tip,
            "W2R Mod Loader 1.1.1. Same plugins as yours. You have 1.1.2, they have 1.1.1."
        );
        let no_number = mod_tip(ModMark::Match, "w2rpm", "W2R ML 1.1.2").unwrap();
        assert!(!no_number.contains("You have"), "nothing to compare");
    }

    #[test]
    fn the_start_tooltip_lists_who_is_downloading() {
        assert_eq!(
            pending_text(&["Ann".into(), String::new(), "Bob".into()]),
            "Pending map download from: Ann, a player, Bob"
        );
        let many: Vec<String> = (0..50).map(|i| format!("Player{i}")).collect();
        assert!(pending_text(&many).len() < 260, "bounded");
    }

    #[test]
    fn a_tooltip_goes_above_right_of_the_cursor_and_stays_on_screen() {
        let w = Rect::new(0.0, 0.0, 3840.0, 2160.0);
        assert_eq!(
            tooltip_origin((1000.0, 1000.0), (500.0, 100.0), w),
            (1044.0, 856.0)
        );
        let (x, _) = tooltip_origin((3800.0, 1000.0), (500.0, 100.0), w);
        assert_eq!(x, 3340.0, "pulled back inside");
        let (_, y) = tooltip_origin((10.0, 20.0), (500.0, 100.0), w);
        assert_eq!(y, 0.0, "never above the window");
    }

    #[test]
    fn contains_is_half_open_and_ignores_empty_rects() {
        let r = Rect::new(10.0, 10.0, 20.0, 20.0);
        assert!(contains(r, (10.0, 10.0)));
        assert!(!contains(r, (30.0, 15.0)));
        assert!(!contains(Rect::default(), (0.0, 0.0)));
    }

    #[test]
    fn notices_appear_before_the_chat_line_they_precede_and_the_rest_at_the_end() {
        let lines: Vec<(usize, String)> = vec![(0, "a".into()), (1, "b".into()), (3, "d".into())];
        let notices = vec![
            Notice {
                text: "n1".into(),
                anchor: 1,
            },
            Notice {
                text: "n2".into(),
                anchor: 2,
            },
            Notice {
                text: "n3".into(),
                anchor: 9,
            },
        ];
        let rows: Vec<String> = merge_notices(&lines, &notices)
            .into_iter()
            .map(|r| {
                if r.notice {
                    format!("[{}]", r.text)
                } else {
                    r.text
                }
            })
            .collect();
        assert_eq!(rows, ["a", "[n1]", "b", "[n2]", "d", "[n3]"]);
        assert_eq!(merge_notices(&[], &notices).len(), 3);
        assert!(merge_notices(&[], &[]).is_empty());
    }

    #[test]
    fn only_the_newest_notices_are_kept() {
        let mut n = Notices::default();
        for i in 0..40 {
            n.push(format!("n{i}"), i);
        }
        assert_eq!(n.all().len(), MAX_NOTICES);
        assert_eq!(n.all()[0].text, "n8");
        n.clear();
        assert!(n.all().is_empty());
    }

    #[test]
    fn lock_notices_name_what_changed() {
        assert_eq!(lock_notice(true, true), "Teams locked by host");
        assert_eq!(lock_notice(true, false), "Teams unlocked by host");
        assert_eq!(lock_notice(false, true), "Slots locked by host");
        assert_eq!(lock_notice(false, false), "Slots unlocked by host");
    }

    #[test]
    fn team_digits_cover_nine_options() {
        assert_eq!(team_digit(0), Some(c"1"));
        assert_eq!(team_digit(8), Some(c"OBS"));
        assert_eq!(team_digit(9), None);
    }
}
