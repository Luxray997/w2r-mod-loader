//! The chat plugin's pure state: settings, the message backlog, fade and colour rules. No game
//! access, so it is unit-tested on any target. Constants are wc2r-plus `chat.cpp`'s.

use std::collections::VecDeque;
use std::ffi::CString;

pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16) | ((a as u32) << 24)
}

pub const HIGHLIGHT: u32 = rgba(255, 204, 0, 255);
pub const TEXT_GREY: u32 = rgba(0x92, 0x92, 0x92, 255);
pub const CURSOR_COLOUR: u32 = rgba(166, 141, 105, 235);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    System,
    All,
    Team,
    Allies,
    Enemies,
    Private,
}

impl Channel {
    pub fn colour(self) -> u32 {
        match self {
            Channel::System => HIGHLIGHT,
            Channel::Team => rgba(0x25, 0xe6, 0x5f, 255),
            Channel::Allies => rgba(0x69, 0xa7, 0xff, 255),
            Channel::All | Channel::Enemies | Channel::Private => TEXT_GREY,
        }
    }
}

/// A numeric setting: key, default, inclusive range (wc2r-plus `RegisterChatSettings`).
pub struct Spec {
    pub key: &'static str,
    pub default: i64,
    pub min: i64,
    pub max: i64,
}

pub const ENABLED: Spec = Spec {
    key: "enabled",
    default: 1,
    min: 0,
    max: 1,
};
pub const LINES: Spec = Spec {
    key: "lines",
    default: 8,
    min: 2,
    max: 20,
};
pub const FADE: Spec = Spec {
    key: "fade",
    default: 7,
    min: 0,
    max: 60,
};
pub const BACKLOG: Spec = Spec {
    key: "backlog",
    default: 100,
    min: 25,
    max: 500,
};
pub const BG: Spec = Spec {
    key: "bg",
    default: 100,
    min: 0,
    max: 255,
};
pub const TIMESTAMPS: Spec = Spec {
    key: "timestamps",
    default: 0,
    min: 0,
    max: 1,
};
pub const LOG: Spec = Spec {
    key: "log",
    default: 0,
    min: 0,
    max: 1,
};

impl Spec {
    pub fn clamp(&self, v: i64) -> i64 {
        v.clamp(self.min, self.max)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub lines: i32,
    pub fade_seconds: i32,
    pub backlog: usize,
    pub bg_alpha: i32,
    pub timestamps: bool,
    pub log: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: ENABLED.default != 0,
            lines: LINES.default as i32,
            fade_seconds: FADE.default as i32,
            backlog: BACKLOG.default as usize,
            bg_alpha: BG.default as i32,
            timestamps: TIMESTAMPS.default != 0,
            log: LOG.default != 0,
        }
    }
}

impl Settings {
    /// Applies one stored value, clamped to its range; an unknown key is ignored.
    pub fn apply(&mut self, key: &str, value: i64) {
        match key {
            "enabled" => self.enabled = ENABLED.clamp(value) != 0,
            "lines" => self.lines = LINES.clamp(value) as i32,
            "fade" => self.fade_seconds = FADE.clamp(value) as i32,
            "backlog" => self.backlog = BACKLOG.clamp(value) as usize,
            "bg" => self.bg_alpha = BG.clamp(value) as i32,
            "timestamps" => self.timestamps = TIMESTAMPS.clamp(value) != 0,
            "log" => self.log = LOG.clamp(value) != 0,
            _ => {}
        }
    }
}

#[derive(Clone, Debug)]
pub struct Message {
    /// NUL-terminated for the game's C-string widgets.
    pub text: CString,
    pub highlight: bool,
    pub arrived_ms: u64,
    pub channel: Channel,
}

impl Message {
    pub fn new(text: &[u8], highlight: bool, channel: Channel, now_ms: u64) -> Option<Message> {
        // The game's text, byte for byte; a NUL would end the line early in its widgets, so cut
        // there.
        let bytes: Vec<u8> = text.iter().copied().take_while(|&b| b != 0).collect();
        if bytes.is_empty() {
            return None;
        }
        Some(Message {
            text: CString::new(bytes).expect("no NUL"),
            highlight,
            arrived_ms: now_ms,
            channel,
        })
    }

    pub fn colour(&self) -> u32 {
        if self.highlight {
            HIGHLIGHT
        } else {
            self.channel.colour()
        }
    }
}

/// The newest `cap` messages, oldest dropped first.
#[derive(Default)]
pub struct Backlog {
    items: VecDeque<Message>,
    scroll_to_bottom: bool,
}

impl Backlog {
    pub fn push(&mut self, m: Message, cap: usize) {
        self.items.push_back(m);
        while self.items.len() > cap {
            self.items.pop_front();
        }
        self.scroll_to_bottom = true;
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.scroll_to_bottom = false;
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn snapshot(&self) -> Vec<Message> {
        self.items.iter().cloned().collect()
    }

    /// Newest first, for the recent-lines view.
    pub fn recent_first(&self) -> impl Iterator<Item = &Message> {
        self.items.iter().rev()
    }

    pub fn request_scroll(&mut self) {
        self.scroll_to_bottom = true;
    }

    /// True once after a push or reveal.
    pub fn take_scroll(&mut self) -> bool {
        std::mem::replace(&mut self.scroll_to_bottom, false)
    }
}

pub const FADE_OUT_MS: u64 = 1000;

/// 1.0 while the message is young, then a linear fade over a second to 0.
pub fn visibility(arrived_ms: u64, now_ms: u64, hold_ms: u64) -> f32 {
    let age = now_ms.saturating_sub(arrived_ms);
    if age < hold_ms {
        1.0
    } else if age >= hold_ms + FADE_OUT_MS {
        0.0
    } else {
        1.0 - (age - hold_ms) as f32 / FADE_OUT_MS as f32
    }
}

/// Scales the alpha byte of an `rgba` colour.
pub fn faded(colour: u32, alpha: f32) -> u32 {
    if alpha >= 1.0 {
        return colour;
    }
    let a = ((colour >> 24) as f32 * alpha) as u32;
    (colour & 0x00ff_ffff) | (a << 24)
}

/// The game multiplies text colour channels by a style factor; alpha is untouched.
pub fn apply_colour_factor(colour: u32, factor: f32) -> u32 {
    if factor == 1.0 {
        return colour;
    }
    let ch = |shift: u32| (((colour >> shift) & 0xff) as f32 * factor) as i32 as u8 as u32;
    (colour & 0xff00_0000) | (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// Whether the full history (not just recent lines) is shown: fade is off, the player is
/// typing, or the game is paused.
pub fn revealed(fade_seconds: i32, chat_target_mode: u8, pause_flags: u8) -> bool {
    fade_seconds <= 0 || chat_target_mode != 0 || pause_flags & 1 != 0
}

/// Local wall-clock time, as the game machine shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// `[HH:MM] `, the prefix "Show Timestamps" puts on a message.
pub fn timestamp_prefix(t: &LocalTime) -> String {
    format!("[{:02}:{:02}] ", t.hour, t.minute)
}

/// One line of the disk log: `[YYYY-MM-DD HH:MM:SS] text`.
pub fn log_line(t: &LocalTime, text: &str) -> String {
    format!(
        "[{:04}-{:02}-{:02} {:02}:{:02}:{:02}] {text}\n",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    )
}

/// The per-launch log file name; fits the host's data-file rule.
pub fn log_file_name(t: &LocalTime) -> String {
    format!(
        "chat-{:04}-{:02}-{:02}_{:02}-{:02}-{:02}.log",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    )
}

pub const LOG_MATCH_MARKER: &str = "\n=== new match ===\n";

/// The game's player name slot: NUL-terminated within the slot. A name that fills the slot (no
/// terminator) or is empty is not trusted.
pub fn player_name(slot: &[u8]) -> Option<String> {
    let n = slot.iter().position(|&b| b == 0)?;
    (n > 0).then(|| String::from_utf8_lossy(&slot[..n]).into_owned())
}

/// "X paused the game." (with the remaining pauses in a networked match) or "X resumed the game."
pub fn pause_line(name: &str, paused: bool, remaining: Option<u8>) -> String {
    if !paused {
        return format!("{name} resumed the game.");
    }
    match remaining {
        Some(n) => format!(
            "{name} paused the game. ({n} pause{} remaining)",
            if n == 1 { "" } else { "s" }
        ),
        None => format!("{name} paused the game."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(text: &str, at: u64) -> Message {
        Message::new(text.as_bytes(), false, Channel::All, at).unwrap()
    }

    #[test]
    fn colours_pack_like_wc2r_plus() {
        assert_eq!(rgba(255, 204, 0, 255), 0xff00_ccff);
        assert_eq!(HIGHLIGHT, 0xff00_ccff);
        assert_eq!(Channel::Team.colour(), rgba(0x25, 0xe6, 0x5f, 255));
        assert_eq!(Channel::All.colour(), TEXT_GREY);
        assert_eq!(Channel::System.colour(), HIGHLIGHT);
    }

    #[test]
    fn settings_default_clamp_and_ignore_the_unknown() {
        let mut s = Settings::default();
        assert_eq!(
            (s.lines, s.fade_seconds, s.backlog, s.bg_alpha),
            (8, 7, 100, 100)
        );
        assert!(s.enabled);
        s.apply("lines", 99);
        s.apply("fade", -5);
        s.apply("backlog", 1);
        s.apply("bg", 300);
        s.apply("enabled", 0);
        s.apply("nonsense", 1);
        assert_eq!(
            (s.lines, s.fade_seconds, s.backlog, s.bg_alpha),
            (20, 0, 25, 255)
        );
        assert!(!s.enabled);
    }

    #[test]
    fn the_backlog_keeps_the_newest_messages_up_to_the_cap() {
        let mut b = Backlog::default();
        for i in 0..30 {
            b.push(msg(&format!("m{i}"), i), 25);
        }
        assert_eq!(b.len(), 25);
        let snap = b.snapshot();
        assert_eq!(snap.first().unwrap().text.to_str().unwrap(), "m5");
        assert_eq!(snap.last().unwrap().text.to_str().unwrap(), "m29");
        assert_eq!(
            b.recent_first().next().unwrap().text.to_str().unwrap(),
            "m29"
        );
        // Lowering the cap drops from the front on the next push.
        b.push(msg("x", 99), 3);
        assert_eq!(b.len(), 3);
        assert!(b.take_scroll());
        assert!(!b.take_scroll(), "only once");
        b.clear();
        assert!(b.is_empty() && !b.take_scroll());
    }

    #[test]
    fn empty_and_nul_text_make_no_message() {
        assert!(Message::new(b"", false, Channel::All, 0).is_none());
        assert!(Message::new(b"\0abc", false, Channel::All, 0).is_none());
        let m = Message::new(b"ab\0cd", false, Channel::All, 0).unwrap();
        assert_eq!(m.text.to_str().unwrap(), "ab");
    }

    #[test]
    fn highlight_wins_over_the_channel_colour() {
        let mut m = msg("x", 0);
        assert_eq!(m.colour(), TEXT_GREY);
        m.highlight = true;
        assert_eq!(m.colour(), HIGHLIGHT);
    }

    #[test]
    fn visibility_holds_then_fades_over_a_second() {
        assert_eq!(visibility(1000, 1000, 7000), 1.0);
        assert_eq!(visibility(1000, 7999, 7000), 1.0);
        assert!((visibility(1000, 8500, 7000) - 0.5).abs() < 1e-6);
        assert_eq!(visibility(1000, 9000, 7000), 0.0);
        assert_eq!(
            visibility(5000, 100, 7000),
            1.0,
            "a clock that went backwards"
        );
    }

    #[test]
    fn alpha_fading_and_the_colour_factor() {
        assert_eq!(faded(0xff10_2030, 1.0), 0xff10_2030);
        assert_eq!(faded(0xff10_2030, 0.5), 0x7f10_2030);
        assert_eq!(faded(0xff10_2030, 0.0), 0x0010_2030);
        assert_eq!(apply_colour_factor(0xff10_2030, 1.0), 0xff10_2030);
        assert_eq!(apply_colour_factor(0xff10_2030, 0.5), 0xff08_1018);
    }

    #[test]
    fn the_history_is_revealed_when_fade_is_off_typing_or_paused() {
        assert!(!revealed(7, 0, 0));
        assert!(revealed(0, 0, 0));
        assert!(revealed(7, 2, 0));
        assert!(revealed(7, 0, 1));
        assert!(!revealed(7, 0, 0b10), "only bit 0 is the pause flag");
    }

    fn t() -> LocalTime {
        LocalTime {
            year: 2026,
            month: 10,
            day: 4,
            hour: 9,
            minute: 5,
            second: 7,
        }
    }

    #[test]
    fn timestamps_log_lines_and_file_names_follow_wc2r_plus() {
        assert_eq!(timestamp_prefix(&t()), "[09:05] ");
        assert_eq!(log_line(&t(), "hi"), "[2026-10-04 09:05:07] hi\n");
        assert_eq!(log_file_name(&t()), "chat-2026-10-04_09-05-07.log");
        assert_eq!(LOG_MATCH_MARKER, "\n=== new match ===\n");
    }

    #[test]
    fn pause_lines_pluralise_and_skip_the_count_offline() {
        assert_eq!(
            pause_line("Ann", true, Some(2)),
            "Ann paused the game. (2 pauses remaining)"
        );
        assert_eq!(
            pause_line("Ann", true, Some(1)),
            "Ann paused the game. (1 pause remaining)"
        );
        assert_eq!(
            pause_line("Ann", true, Some(0)),
            "Ann paused the game. (0 pauses remaining)"
        );
        assert_eq!(pause_line("Ann", true, None), "Ann paused the game.");
        assert_eq!(pause_line("Ann", false, Some(2)), "Ann resumed the game.");
    }

    #[test]
    fn player_names_need_a_terminator_and_text() {
        assert_eq!(player_name(b"Ann\0junk"), Some("Ann".into()));
        assert_eq!(player_name(b"\0abc"), None);
        assert_eq!(
            player_name(b"nonul"),
            None,
            "fills the slot with no terminator"
        );
    }

    #[test]
    fn the_new_settings_default_off_and_clamp() {
        let mut s = Settings::default();
        assert!(!s.timestamps && !s.log);
        s.apply("timestamps", 1);
        s.apply("log", 7);
        assert!(s.timestamps && s.log);
        s.apply("log", -3);
        assert!(!s.log);
    }
}
