//! The in-flight marker: while a match runs the host keeps its start report and the latest turn
//! count in one small file outside the outbox. A match that ends normally deletes it. If the
//! player quits from the results screen, the game crashes or the machine dies, the file is still
//! there at the next launch and [`recover`] turns it into the `left` result report that the
//! lobby re-entry would have produced. File: `{"v":1,"turns":n,"sync_mismatch":n,"start":{report}}`.

use std::fs;
use std::io;
use std::path::Path;

use w2r_formats::json::{self, Number, Value};

use crate::report::{Outcome, Phase, Report, ResultInfo};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inflight {
    /// The start report that was queued for this match.
    pub start: Report,
    pub turns: u32,
    pub sync_mismatch: u32,
}

fn num(n: u32) -> Value {
    Value::Number(Number {
        text: n.to_string(),
    })
}

impl Inflight {
    pub fn to_json(&self) -> String {
        Value::Object(vec![
            ("v".into(), num(1)),
            ("turns".into(), num(self.turns)),
            ("sync_mismatch".into(), num(self.sync_mismatch)),
            ("start".into(), self.start.to_value()),
        ])
        .to_json()
    }

    pub fn parse(raw: &[u8]) -> Result<Inflight, &'static str> {
        let v = json::parse(raw).map_err(|_| "not json")?;
        let uint = |k: &str| {
            v.get(k)
                .and_then(Value::as_number)
                .and_then(Number::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .ok_or("bad number")
        };
        if uint("v")? != 1 {
            return Err("unsupported version");
        }
        let start = Report::from_value(v.get("start").ok_or("missing start")?)?;
        if start.phase != Phase::Start {
            return Err("not a start report");
        }
        Ok(Inflight {
            start,
            turns: uint("turns")?,
            sync_mismatch: uint("sync_mismatch")?,
        })
    }
}

/// Replaces the marker (through a temp file and a rename).
pub fn write(path: &Path, marker: &Inflight) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, marker.to_json())?;
    fs::rename(&tmp, path)
}

/// The marker left by an earlier run, if any. A file that cannot be read is removed: it can
/// only describe a match we can no longer report, and leaving it would block every launch.
pub fn read(path: &Path) -> Option<Inflight> {
    let raw = match fs::read(path) {
        Ok(r) => r,
        Err(_) => return None,
    };
    match Inflight::parse(&raw) {
        Ok(m) => Some(m),
        Err(_) => {
            let _ = fs::remove_file(path);
            None
        }
    }
}

pub fn clear(path: &Path) {
    let _ = fs::remove_file(path);
}

/// The result report for a match that was cut off: the player's own entry never said anything,
/// so it is a `left` at the last turn seen. `None` when no turn had run.
pub fn recover(marker: &Inflight, rid: [u8; 8]) -> Option<Report> {
    let outcome = Outcome::from_code(0, marker.turns)?;
    let mut report = marker.start.clone();
    report.phase = Phase::Result;
    report.rid = rid;
    report.result = Some(ResultInfo {
        outcome,
        code: 0,
        turns: marker.turns,
        sync_mismatch: marker.sync_mismatch,
    });
    Some(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::setup_id;
    use crate::slot::{Kind, Player};

    fn marker(turns: u32) -> Inflight {
        let players = vec![Player {
            slot: 0,
            kind: Kind::Human,
            name: "Me".into(),
            tag: Some("Me#1".into()),
            team: 1,
            race: 0,
        }];
        Inflight {
            start: Report {
                phase: Phase::Start,
                rid: [1; 8],
                setup: setup_id("m", &players),
                started: 1_760_000_000,
                client: "t".into(),
                fingerprint: "00000000".into(),
                map: "m".into(),
                me: 0,
                players,
                result: None,
            },
            turns,
            sync_mismatch: 0,
        }
    }

    fn temp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("w2r-inflight-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn write_read_clear() {
        let d = temp("a");
        let p = d.join("inflight.json");
        assert_eq!(read(&p), None);
        write(&p, &marker(10)).unwrap();
        write(&p, &marker(250)).unwrap();
        assert_eq!(read(&p), Some(marker(250)));
        clear(&p);
        assert_eq!(read(&p), None);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn an_unreadable_marker_is_removed() {
        let d = temp("b");
        let p = d.join("inflight.json");
        fs::write(&p, b"garbage").unwrap();
        assert_eq!(read(&p), None);
        assert!(!p.exists());
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn recovery_makes_a_left_result_for_the_same_match() {
        let m = marker(777);
        let r = recover(&m, [9; 8]).unwrap();
        assert_eq!(r.phase, Phase::Result);
        assert_eq!(
            (r.setup, r.started, r.me),
            (m.start.setup, m.start.started, 0)
        );
        let info = r.result.unwrap();
        assert_eq!(
            (info.outcome, info.code, info.turns),
            (Outcome::Left, 0, 777)
        );
        assert_ne!(r.rid, m.start.rid);
    }

    #[test]
    fn nothing_to_recover_before_the_first_turn() {
        assert_eq!(recover(&marker(0), [9; 8]), None);
    }
}
