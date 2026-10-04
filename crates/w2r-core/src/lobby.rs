//! Lobby presence, the way wc2r-plus detects it: the game draws the window named
//! `multiplayer_lobby_screen` every frame while the local player is in a lobby. Seeing it means
//! "in a lobby"; not seeing it for [`GAP_MS`] means "left". Pure logic; the frame hook feeds it
//! window names and a millisecond clock.

pub const WINDOW: &str = "multiplayer_lobby_screen";
/// How long the lobby window may go undrawn before the player counts as having left.
pub const GAP_MS: u64 = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Enter,
    Leave,
}

#[derive(Debug, Default)]
pub struct Lobby {
    last_seen: Option<u64>,
    inside: bool,
}

impl Lobby {
    pub fn new() -> Self {
        Lobby::default()
    }

    /// Call for every window the game begins. Returns `Enter` the first time the lobby window
    /// shows up after an absence.
    pub fn on_window(&mut self, name: &str, now: u64) -> Option<Event> {
        if name != WINDOW {
            return self.on_frame(now);
        }
        self.last_seen = Some(now);
        if self.inside {
            None
        } else {
            self.inside = true;
            Some(Event::Enter)
        }
    }

    /// Call at least once per frame (any window will do). Returns `Leave` once the lobby window
    /// has been missing for `GAP_MS`.
    pub fn on_frame(&mut self, now: u64) -> Option<Event> {
        match self.last_seen {
            Some(seen) if self.inside && now.saturating_sub(seen) > GAP_MS => {
                self.inside = false;
                Some(Event::Leave)
            }
            _ => None,
        }
    }

    /// True while the lobby window is being drawn (within `GAP_MS`).
    pub fn in_lobby(&self, now: u64) -> bool {
        self.inside
            && self
                .last_seen
                .is_some_and(|seen| now.saturating_sub(seen) <= GAP_MS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_once_then_leave_after_the_gap() {
        let mut l = Lobby::new();
        assert_eq!(l.on_window("main_menu", 0), None);
        assert!(!l.in_lobby(0));
        assert_eq!(l.on_window(WINDOW, 100), Some(Event::Enter));
        assert_eq!(l.on_window(WINDOW, 116), None, "no repeat while drawn");
        assert!(l.in_lobby(116));
        assert!(l.in_lobby(616), "within the gap");
        assert_eq!(l.on_window("other", 616), None);
        assert!(!l.in_lobby(617));
        assert_eq!(l.on_window("other", 617), Some(Event::Leave));
        assert_eq!(l.on_frame(1000), None, "leave fires once");
        assert_eq!(l.on_window(WINDOW, 2000), Some(Event::Enter), "re-entering");
    }

    #[test]
    fn a_frame_without_windows_still_detects_leaving() {
        let mut l = Lobby::new();
        l.on_window(WINDOW, 10);
        assert_eq!(l.on_frame(510), None);
        assert_eq!(l.on_frame(511), Some(Event::Leave));
        assert!(!l.in_lobby(511));
    }

    #[test]
    fn clock_going_backwards_does_not_panic() {
        let mut l = Lobby::new();
        l.on_window(WINDOW, 1000);
        assert_eq!(l.on_frame(0), None);
        assert!(l.in_lobby(0));
    }
}
