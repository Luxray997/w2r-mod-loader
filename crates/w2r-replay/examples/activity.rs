//! Per-slot activity in recorded replays: when each slot's packets start and stop, when it last
//! did something on purpose, and how much it did in each fifth of the game.
//! `cargo run -p w2r-replay --example activity -- <file.w2rr>...`

use w2r_replay::packet::player_actions;
use w2r_replay::reader::Replay;

fn main() {
    for path in std::env::args().skip(1) {
        let Ok(raw) = std::fs::read(&path) else {
            println!("{path}: unreadable");
            continue;
        };
        let Ok(r) = Replay::parse(&raw) else {
            println!("{path}: not a replay");
            continue;
        };
        let last = r.last_turn().max(1);
        println!(
            "== {} turns={} footer={:?} local={}",
            path.rsplit(['/', '\\']).next().unwrap_or(&path),
            last,
            r.footer.as_ref().map(|f| format!("{f:?}").chars().take(90).collect::<String>()),
            r.header.local
        );
        {
            let mut ids = std::collections::BTreeMap::<u8, (u32, u32)>::new();
            for t in &r.turns {
                for (slot, _) in &t.packets {
                    let e = ids.entry(*slot).or_insert((0, 0));
                    e.0 += 1;
                    e.1 = t.turn;
                }
            }
            println!("  packet slots (id: turns, last): {ids:?}  header.slots kinds/teams: {:?}", r.players().iter().map(|p| (p.slot, p.team)).collect::<Vec<_>>());
        }
        let humans: Vec<_> = r.players().into_iter().filter(|p| p.kind == w2r_stats::slot::Kind::Human).collect();
        for (id, p) in humans.iter().enumerate() {
            let s = id;
            let (mut first, mut final_seen, mut final_act) = (None, 0u32, 0u32);
            let mut turns_with = 0u32;
            let mut fifths = [0u32; 5];
            let mut total = 0u32;
            for t in &r.turns {
                let Some((_, data)) = t.packets.iter().find(|(slot, _)| usize::from(*slot) == s) else {
                    continue;
                };
                turns_with += 1;
                first.get_or_insert(t.turn);
                final_seen = t.turn;
                let n = player_actions(data) as u32;
                if n > 0 {
                    final_act = t.turn;
                    total += n;
                    fifths[((u64::from(t.turn) * 5) / (u64::from(last) + 1)) as usize] += n;
                }
            }
            println!(
                "  slot{} {:?} {:<24} team={} first={:?} last_packet={} last_action={} turns_with_packet={} actions={} fifths={:?}",
                p.slot, p.kind, p.tag.as_deref().unwrap_or(&p.name), p.team, first, final_seen, final_act, turns_with, total, fifths
            );
        }
    }
}
