#![cfg_attr(not(target_arch = "x86"), allow(dead_code))]
//! The observer's auto-director: picks the player worth watching from how busy they are. A player's
//! score blends their actions per minute with how much fighting they were in (kills, losses and
//! razed buildings since the last look), smoothed over a few seconds. The view moves to a new
//! player only when they are clearly busier than the one on screen and the last move was a while
//! ago, so the picture does not flick between players. Pure; the game side is in `replayplay.rs`.

/// How often the scores are looked at.
pub const EVAL_MS: u64 = 1_000;
/// The shortest time on one player.
pub const HOLD_MS: u64 = 8_000;
/// What one fighting event in a second is worth, in actions per minute.
const COMBAT_WEIGHT: f32 = 90.0;
/// The share of the old score kept at each look.
const KEEP: f32 = 0.7;
/// The challenger must beat the player on screen by this factor and margin.
const FACTOR: f32 = 1.4;
const MARGIN: f32 = 30.0;

/// What is known about one player at a look.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub player: u8,
    /// Actions per minute, if there is a figure yet.
    pub apm: Option<u32>,
    /// Kills + losses + razed buildings so far in the match.
    pub combat: u32,
}

#[derive(Debug, Default)]
pub struct Director {
    score: [f32; 8],
    last_combat: [Option<u32>; 8],
    last_eval: u64,
    last_switch: u64,
}

impl Director {
    pub fn new() -> Director {
        Director::default()
    }

    /// Looks at the players if it is time. Returns the player to switch the view to, if any.
    pub fn tick(&mut self, now_ms: u64, current: Option<u8>, samples: &[Sample]) -> Option<u8> {
        if self.last_eval != 0 && now_ms < self.last_eval + EVAL_MS {
            return None;
        }
        self.last_eval = now_ms.max(1);
        for s in samples.iter().filter(|s| s.player < 8) {
            let i = usize::from(s.player);
            let fights = self.last_combat[i].map_or(0, |before| s.combat.saturating_sub(before));
            self.last_combat[i] = Some(s.combat);
            let now = s.apm.unwrap_or(0) as f32 + COMBAT_WEIGHT * fights as f32;
            self.score[i] = KEEP * self.score[i] + (1.0 - KEEP) * now;
        }
        let best = samples
            .iter()
            .filter(|s| s.player < 8)
            .max_by(|a, b| self.score[usize::from(a.player)].total_cmp(&self.score[usize::from(b.player)]))?
            .player;
        let on_screen = current.filter(|&c| samples.iter().any(|s| s.player == c));
        let Some(on_screen) = on_screen else {
            self.last_switch = now_ms;
            return Some(best);
        };
        if best == on_screen || now_ms < self.last_switch + HOLD_MS {
            return None;
        }
        let (b, c) = (self.score[usize::from(best)], self.score[usize::from(on_screen)]);
        if b > c * FACTOR + MARGIN {
            self.last_switch = now_ms;
            return Some(best);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet(p: u8, apm: u32) -> Sample {
        Sample { player: p, apm: Some(apm), combat: 0 }
    }

    #[test]
    fn nobody_on_screen_picks_the_busiest() {
        let mut d = Director::new();
        assert_eq!(d.tick(1_000, None, &[quiet(0, 20), quiet(1, 90)]), Some(1));
    }

    #[test]
    fn it_waits_between_looks_and_holds_a_choice() {
        let mut d = Director::new();
        d.tick(1_000, None, &[quiet(0, 100), quiet(1, 10)]);
        assert_eq!(d.tick(1_500, Some(0), &[quiet(0, 10), quiet(1, 200)]), None, "too soon to look");
        // Player 1 is far busier but the hold has not passed.
        for t in 1..7 {
            assert_eq!(d.tick(1_000 + t * 1_000, Some(0), &[quiet(0, 10), quiet(1, 300)]), None);
        }
        assert_eq!(d.tick(9_000, Some(0), &[quiet(0, 10), quiet(1, 300)]), Some(1));
    }

    #[test]
    fn a_small_lead_does_not_move_the_view() {
        let mut d = Director::new();
        d.tick(1_000, None, &[quiet(0, 100), quiet(1, 100)]);
        for t in 1..20 {
            let s = [quiet(0, 100), quiet(1, 120)];
            assert_eq!(d.tick(1_000 + t * 1_000, Some(0), &s), None);
        }
    }

    #[test]
    fn a_fight_draws_the_view() {
        let mut d = Director::new();
        d.tick(1_000, None, &[quiet(0, 60), quiet(1, 60)]);
        let mut moved = None;
        for t in 1..16u64 {
            let fighting = Sample { player: 1, apm: Some(60), combat: (t as u32) * 3 };
            if let Some(p) = d.tick(1_000 + t * 1_000, Some(0), &[quiet(0, 60), fighting]) {
                moved = Some(p);
                break;
            }
        }
        assert_eq!(moved, Some(1));
    }

    #[test]
    fn a_player_who_left_the_list_is_replaced() {
        let mut d = Director::new();
        assert_eq!(d.tick(1_000, Some(3), &[quiet(0, 50), quiet(1, 80)]), Some(1));
    }
}
