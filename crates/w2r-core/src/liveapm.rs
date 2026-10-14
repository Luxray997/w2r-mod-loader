#![cfg_attr(not(target_arch = "x86"), allow(dead_code))]
//! Live actions per minute for every player, for an observer's scoreboard. Every machine in a
//! match gets every player's command packet each turn, so the observer counts them the same way
//! the player's own counter does (`apm::Tracker`): per-player orders over the last 20 seconds
//! played, with pauses left out. Only observers run it.

use std::sync::Mutex;

use crate::apm::Tracker;

#[derive(Default)]
struct Table {
    trackers: [Tracker; 8],
}

static TABLE: Mutex<Option<Table>> = Mutex::new(None);

/// A new match: every figure starts over.
pub fn reset() {
    if let Ok(mut t) = TABLE.lock() {
        *t = None;
    }
}

/// One turn completed at `wall_ms`; `actions[p]` is how many orders player `p` gave in it and
/// `present` has a bit for each player that was in the match.
pub fn turn(wall_ms: u64, actions: [u32; 8], present: u8) {
    if let Ok(mut t) = TABLE.lock() {
        let table = t.get_or_insert_with(Table::default);
        for p in 0..8 {
            if present & (1 << p) != 0 {
                table.trackers[p].turn(wall_ms, actions[p]);
            }
        }
    }
}

/// Player `slot`'s actions per minute, once there is enough play to measure.
pub fn apm(slot: u8) -> Option<u32> {
    let t = TABLE.lock().ok()?;
    t.as_ref()?.trackers.get(usize::from(slot))?.apm()
}

/// The turn that just ran, from the packets the net layer holds. Call inside `guarded`.
#[cfg(target_arch = "x86")]
pub fn on_turn() {
    if !crate::observer::local_is_observer() || crate::replayplay::is_active() {
        return;
    }
    if crate::apm::paused() {
        return;
    }
    let counted = crate::replayrec::with_turn_packets(|packets| {
        let mut actions = [0u32; 8];
        let mut present = 0u8;
        for (slot, data) in packets {
            if let Some(n) = actions.get_mut(usize::from(*slot)) {
                *n += w2r_replay::packet::player_actions(data) as u32;
                present |= 1 << slot;
            }
        }
        (actions, present)
    });
    if let Some((actions, present)) = counted {
        turn(crate::chat::now_ms(), actions, present);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_player_has_their_own_figure() {
        reset();
        let mut now = 1_000;
        while now < 12_000 {
            now += 100;
            // Player 1 gives an order every turn; player 2 never does.
            turn(now, [0, 1, 0, 0, 0, 0, 0, 0], 0b0000_0110);
        }
        assert!(apm(1).unwrap() > 400);
        assert_eq!(apm(2), Some(0));
        assert_eq!(apm(3), None, "never present");
        reset();
        assert_eq!(apm(1), None);
    }
}
