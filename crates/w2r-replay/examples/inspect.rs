//! `cargo run -p w2r-replay --example inspect -- <file.w2rr>`: prints a replay's header, per-slot
//! packet and action counts, the opcode histogram and any bytes the scanner could not split.

use std::collections::BTreeMap;

use w2r_replay::Replay;
use w2r_replay::packet;

fn main() {
    let path = std::env::args().nth(1).expect("usage: inspect <file.w2rr>");
    let raw = std::fs::read(&path).expect("read");
    let r = Replay::parse(&raw).expect("parse");
    let h = &r.header;
    println!(
        "{} build {} {} map {:?} local {} fp {:02x?} seed {} bytes",
        path,
        h.build,
        h.client,
        h.map,
        h.local,
        h.fingerprint,
        h.seed.len()
    );
    println!(
        "  game seed {:08x?} rng before first turn {:08x?}",
        h.game_seed(),
        h.rng_at_start()
    );
    for p in r.players() {
        println!("  player {p:?}");
    }
    println!(
        "turns {} (first {}, last {}), complete {}, footer {:?}",
        r.turns.len(),
        r.turns.first().map_or(0, |t| t.turn),
        r.last_turn(),
        r.complete(),
        r.footer
    );
    let mut per_slot: BTreeMap<u8, (usize, usize, usize)> = BTreeMap::new();
    let mut ops: BTreeMap<u8, usize> = BTreeMap::new();
    let mut leftovers = 0;
    let mut gaps = 0;
    let mut prev = None;
    let mut sample_left: Vec<(u32, u8, Vec<u8>)> = Vec::new();
    for t in &r.turns {
        if prev.is_some_and(|p| t.turn != p + 1) {
            gaps += 1;
        }
        prev = Some(t.turn);
        for (slot, data) in &t.packets {
            let e = per_slot.entry(*slot).or_default();
            e.0 += 1;
            e.1 += data.len();
            let (cmds, rest) = packet::commands(data);
            e.2 += cmds.iter().filter(|c| c.0 != 0x05).count();
            for (op, _) in cmds {
                *ops.entry(op).or_default() += 1;
            }
            if rest > 0 {
                leftovers += 1;
                if sample_left.len() < 5 {
                    sample_left.push((t.turn, *slot, data.clone()));
                }
            }
        }
    }
    println!("turn-number gaps: {gaps}, packets with unparsed bytes: {leftovers}");
    for (slot, (n, bytes, actions)) in per_slot {
        println!("  slot {slot}: {n} packets, {bytes} bytes, {actions} non-idle commands");
    }
    println!("opcodes: {ops:02x?}");
    println!("chat lines: {}", r.chats.len());
    for c in &r.chats {
        println!(
            "  turn {} player {} mode {} ({} ms): {}",
            c.turn,
            c.who,
            c.mode,
            c.duration_ms,
            String::from_utf8_lossy(&c.text)
        );
    }
    for (t, s, d) in sample_left {
        println!("  unparsed sample turn {t} slot {s}: {d:02x?}");
    }
}
