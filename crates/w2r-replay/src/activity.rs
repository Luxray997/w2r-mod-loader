//! Who is still in the game and what they are doing, kept turn by turn from the packets the
//! recorder sees. A player who leaves stops sending packets; one who is still connected but has
//! nothing left to do keeps sending idle packets and gives no orders (see the `activity` example
//! for what real games look like). The stats result report carries the summary.
//!
//! Packet ids are the humans in slot order, not lobby slots: computers send nothing, so the first
//! human's packets are id 0, the next human's id 1, and so on.

use w2r_stats::report::PlayerActivity;
use w2r_stats::slot::{Kind, Player};

use crate::packet::player_actions;

pub const IDS: usize = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub seen: bool,
    pub last_seen: u32,
    pub last_action: u32,
    pub actions: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Activity {
    rows: [Row; IDS],
}

impl Activity {
    pub fn new() -> Activity {
        Activity::default()
    }

    /// One packet of `id` for `turn`.
    pub fn note(&mut self, turn: u32, id: u8, packet: &[u8]) {
        let Some(row) = self.rows.get_mut(usize::from(id)) else {
            return;
        };
        row.seen = true;
        row.last_seen = row.last_seen.max(turn);
        let n = player_actions(packet) as u32;
        if n > 0 {
            row.last_action = row.last_action.max(turn);
            row.actions = row.actions.saturating_add(n);
        }
    }

    /// True once any packet has been noted.
    pub fn any(&self) -> bool {
        self.rows.iter().any(|r| r.seen)
    }

    /// The summary for the humans in `players`, with each packet id matched to the human of that
    /// rank in slot order. Humans whose packets were never seen are left out.
    pub fn report(&self, players: &[Player]) -> Vec<PlayerActivity> {
        let mut humans: Vec<&Player> = players.iter().filter(|p| p.kind == Kind::Human).collect();
        humans.sort_by_key(|p| p.slot);
        humans
            .iter()
            .zip(self.rows.iter())
            .filter(|(_, r)| r.seen)
            .map(|(p, r)| PlayerActivity {
                slot: p.slot,
                last_seen: r.last_seen,
                last_action: r.last_action,
                actions: r.actions,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn human(slot: u8) -> Player {
        Player {
            slot,
            kind: Kind::Human,
            name: format!("P{slot}"),
            tag: None,
            team: 1,
            race: 0,
        }
    }

    // 0x0f is a three-byte order record; 0x05 is the idle record every packet may carry.
    const ORDER: [u8; 3] = [0x0f, 1, 2];
    const IDLE: [u8; 1] = [0x05];

    #[test]
    fn a_leaver_stops_and_an_idle_player_keeps_sending() {
        let mut a = Activity::new();
        for turn in 0..100 {
            if turn < 20 {
                a.note(turn, 0, &ORDER);
            } else {
                a.note(turn, 0, &IDLE);
            }
            if turn < 40 {
                a.note(turn, 1, &ORDER);
            }
        }
        let players = [human(1), human(4)];
        let r = a.report(&players);
        assert_eq!(r.len(), 2);
        assert_eq!(
            (r[0].slot, r[0].last_seen, r[0].last_action, r[0].actions),
            (1, 99, 19, 20)
        );
        assert_eq!((r[1].slot, r[1].last_seen, r[1].actions), (4, 39, 40));
    }

    #[test]
    fn ids_follow_humans_in_slot_order_and_skip_computers() {
        let mut a = Activity::new();
        a.note(5, 0, &ORDER);
        a.note(5, 1, &ORDER);
        let mut computer = human(2);
        computer.kind = Kind::Computer;
        // listed out of order on purpose
        let r = a.report(&[human(6), computer, human(0)]);
        assert_eq!(r.iter().map(|x| x.slot).collect::<Vec<_>>(), vec![0, 6]);
    }

    #[test]
    fn nothing_noted_means_no_summary() {
        let a = Activity::new();
        assert!(!a.any());
        assert!(a.report(&[human(0)]).is_empty());
    }
}
