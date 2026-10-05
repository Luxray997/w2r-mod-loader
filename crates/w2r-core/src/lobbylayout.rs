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

/// The digit shown for the team option at `index` in place of the game's own text.
pub fn team_digit(index: usize) -> Option<&'static CStr> {
    const DIGITS: [&CStr; 9] = [c"1", c"2", c"3", c"4", c"5", c"6", c"7", c"8", c"9"];
    DIGITS.get(index).copied()
}

pub const INFO_ROW_COUNT: usize = 7;
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
    fn team_digits_cover_nine_options() {
        assert_eq!(team_digit(0), Some(c"1"));
        assert_eq!(team_digit(8), Some(c"9"));
        assert_eq!(team_digit(9), None);
    }
}
