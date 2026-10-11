//! The host's in-match kick panel, without the game: which player is picked and the two-press
//! confirmation. `ingamekick.rs` reads the keys and the roster and calls the game; this module
//! only decides, so it is tested everywhere.

/// How long the first Delete stays armed for the second.
pub const CONFIRM_MS: u64 = 5000;

/// A player the host could remove: a connected person other than the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub net_slot: u8,
    pub name: String,
}

/// The keys that went down since the last look (edges, not levels).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keys {
    pub toggle: bool,
    pub up: bool,
    pub down: bool,
    pub kick: bool,
    pub close: bool,
}

impl Keys {
    pub fn any(&self) -> bool {
        self.toggle || self.up || self.down || self.kick || self.close
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    /// Drop this net slot now.
    Kick(u8),
}

/// Two presses on the same slot within [`CONFIRM_MS`]: the first arms, the second confirms.
#[derive(Debug, Default)]
pub struct Confirm {
    armed: Option<(u8, u64)>,
}

impl Confirm {
    pub const fn new() -> Confirm {
        Confirm { armed: None }
    }

    /// The slot waiting for its second press, while that is still valid.
    pub fn armed(&self, now: u64) -> Option<u8> {
        self.armed
            .filter(|&(_, at)| now.saturating_sub(at) <= CONFIRM_MS)
            .map(|(slot, _)| slot)
    }

    pub fn clear(&mut self) {
        self.armed = None;
    }

    /// One press on `slot`; true when it confirms (and spends) an armed press.
    pub fn press(&mut self, slot: u8, now: u64) -> bool {
        if self.armed(now) == Some(slot) {
            self.armed = None;
            true
        } else {
            self.armed = Some((slot, now));
            false
        }
    }
}

#[derive(Debug, Default)]
pub struct Panel {
    open: bool,
    selected: Option<u8>,
    armed: Confirm,
}

impl Panel {
    pub const fn new() -> Panel {
        Panel {
            open: false,
            selected: None,
            armed: Confirm::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn selected(&self) -> Option<u8> {
        self.selected
    }

    /// The slot waiting for its second Delete, while that is still valid.
    pub fn armed(&self, now: u64) -> Option<u8> {
        self.armed.armed(now)
    }

    pub fn close(&mut self) {
        self.open = false;
        self.armed.clear();
    }

    /// Applies one batch of key presses against the current list.
    pub fn press(&mut self, keys: Keys, list: &[Candidate], now: u64) -> Action {
        if keys.toggle {
            if self.open {
                self.close();
            } else {
                self.open = true;
                self.selected = list.first().map(|c| c.net_slot);
                self.armed.clear();
            }
            return Action::None;
        }
        if !self.open {
            return Action::None;
        }
        if keys.close {
            self.close();
            return Action::None;
        }
        // The list can change under us (someone left): keep the pick if it is still there.
        let at = self
            .selected
            .and_then(|s| list.iter().position(|c| c.net_slot == s));
        let at = match at {
            Some(i) => i,
            None => {
                self.armed.clear();
                self.selected = list.first().map(|c| c.net_slot);
                0
            }
        };
        if list.is_empty() {
            return Action::None;
        }
        if keys.up || keys.down {
            let n = list.len();
            let to = if keys.down {
                (at + 1) % n
            } else {
                (at + n - 1) % n
            };
            self.selected = Some(list[to].net_slot);
            self.armed.clear();
            return Action::None;
        }
        if keys.kick && self.armed.press(list[at].net_slot, now) {
            return Action::Kick(list[at].net_slot);
        }
        Action::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> Vec<Candidate> {
        vec![
            Candidate {
                net_slot: 2,
                name: "Ann".into(),
            },
            Candidate {
                net_slot: 5,
                name: "Bob".into(),
            },
        ]
    }

    fn key(f: impl Fn(&mut Keys)) -> Keys {
        let mut k = Keys::default();
        f(&mut k);
        k
    }

    fn open(p: &mut Panel, l: &[Candidate]) {
        p.press(key(|k| k.toggle = true), l, 0);
    }

    #[test]
    fn confirm_needs_two_presses_on_the_same_slot() {
        let mut c = Confirm::new();
        assert!(!c.press(3, 0));
        assert!(!c.press(4, 10), "another slot re-arms instead");
        assert_eq!(c.armed(10), Some(4));
        assert!(c.press(4, 20));
        assert_eq!(c.armed(20), None);
        assert!(!c.press(4, 30));
        assert!(!c.press(4, 30 + CONFIRM_MS + 1), "too late");
    }

    #[test]
    fn closed_panel_ignores_everything_but_the_toggle() {
        let mut p = Panel::default();
        let l = list();
        assert_eq!(p.press(key(|k| k.kick = true), &l, 0), Action::None);
        assert_eq!(p.press(key(|k| k.down = true), &l, 0), Action::None);
        assert!(!p.is_open());
        open(&mut p, &l);
        assert!(p.is_open());
        assert_eq!(p.selected(), Some(2));
    }

    #[test]
    fn the_second_delete_on_the_same_player_kicks() {
        let mut p = Panel::default();
        let l = list();
        open(&mut p, &l);
        let del = key(|k| k.kick = true);
        assert_eq!(p.press(del, &l, 100), Action::None);
        assert_eq!(p.armed(100), Some(2));
        assert_eq!(p.press(del, &l, 900), Action::Kick(2));
        assert_eq!(p.armed(900), None, "the confirmation is spent");
        assert_eq!(
            p.press(del, &l, 1000),
            Action::None,
            "a new press arms again"
        );
    }

    #[test]
    fn the_confirmation_runs_out() {
        let mut p = Panel::default();
        let l = list();
        open(&mut p, &l);
        let del = key(|k| k.kick = true);
        p.press(del, &l, 0);
        assert_eq!(p.armed(CONFIRM_MS), Some(2));
        assert_eq!(p.armed(CONFIRM_MS + 1), None);
        assert_eq!(
            p.press(del, &l, CONFIRM_MS + 1),
            Action::None,
            "too late: arms again"
        );
    }

    #[test]
    fn moving_the_pick_disarms_and_wraps() {
        let mut p = Panel::default();
        let l = list();
        open(&mut p, &l);
        p.press(key(|k| k.kick = true), &l, 0);
        p.press(key(|k| k.down = true), &l, 10);
        assert_eq!((p.selected(), p.armed(10)), (Some(5), None));
        p.press(key(|k| k.down = true), &l, 20);
        assert_eq!(p.selected(), Some(2), "wraps past the end");
        p.press(key(|k| k.up = true), &l, 30);
        assert_eq!(p.selected(), Some(5), "and back past the start");
        assert_eq!(p.press(key(|k| k.kick = true), &l, 40), Action::None);
        assert_eq!(p.press(key(|k| k.kick = true), &l, 50), Action::Kick(5));
    }

    #[test]
    fn a_player_who_left_moves_the_pick_and_never_gets_kicked() {
        let mut p = Panel::default();
        let l = list();
        open(&mut p, &l);
        p.press(key(|k| k.kick = true), &l, 0);
        let gone = vec![Candidate {
            net_slot: 5,
            name: "Bob".into(),
        }];
        // Ann left; the armed press must not carry over to Bob.
        assert_eq!(p.press(key(|k| k.kick = true), &gone, 10), Action::None);
        assert_eq!(p.selected(), Some(5));
        assert_eq!(p.armed(10), Some(5));
    }

    #[test]
    fn nobody_to_kick_does_nothing() {
        let mut p = Panel::default();
        open(&mut p, &[]);
        assert_eq!(p.press(key(|k| k.kick = true), &[], 0), Action::None);
        assert_eq!(p.selected(), None);
    }

    #[test]
    fn escape_and_the_toggle_close_and_disarm() {
        let mut p = Panel::default();
        let l = list();
        open(&mut p, &l);
        p.press(key(|k| k.kick = true), &l, 0);
        p.press(key(|k| k.close = true), &l, 1);
        assert!(!p.is_open());
        assert_eq!(p.armed(1), None);
        open(&mut p, &l);
        p.press(key(|k| k.toggle = true), &l, 2);
        assert!(!p.is_open());
    }
}
