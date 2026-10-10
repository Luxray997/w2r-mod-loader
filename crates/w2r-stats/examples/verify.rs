//! `cargo run -p w2r-stats --example verify -- <report.json>...`: checks each queued report's
//! signature and prints what it says. A stand-in for the server's ingest until there is one.

use w2r_stats::envelope::Envelope;
use w2r_stats::hex;

fn main() {
    let mut bad = false;
    for path in std::env::args().skip(1) {
        let raw = match std::fs::read(&path) {
            Ok(r) => r,
            Err(e) => {
                println!("{path}: unreadable ({e})");
                bad = true;
                continue;
            }
        };
        let env = match Envelope::parse(&raw) {
            Ok(e) => e,
            Err(e) => {
                println!("{path}: not a report ({e})");
                bad = true;
                continue;
            }
        };
        match env.report() {
            Ok(r) => println!(
                "{path}: OK key={}.. phase={} me={} players={} result={:?}",
                &hex::encode(&env.key)[..12],
                r.phase.as_str(),
                r.me,
                r.players.len(),
                r.result
            ),
            Err(e) => {
                println!("{path}: REJECTED ({e})");
                bad = true;
            }
        }
    }
    if bad {
        std::process::exit(1);
    }
}
