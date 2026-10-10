//! `cargo run -p w2r-replay --example lobby -- <file.w2rr>`: prints the eight lobby slot records
//! (0x26 bytes each) of a recording's start state.

use w2r_replay::Replay;

fn main() {
    let path = std::env::args().nth(1).expect("usage: lobby <file.w2rr>");
    let raw = std::fs::read(&path).expect("read");
    let r = Replay::parse(&raw).expect("parse");
    for slot in 0..8usize {
        let rec = &r.header.lobby[slot * 0x26..(slot + 1) * 0x26];
        let hex: Vec<String> = rec.iter().map(|b| format!("{b:02x}")).collect();
        println!("slot {slot}: {}", hex.join(" "));
    }
}
