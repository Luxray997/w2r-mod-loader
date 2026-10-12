//! The report schema, version 1. Two phases per match (docs/stats-reporting.md): a `start` report
//! when the match is under way and a `result` report when it ends for this player. Both carry the
//! roster so the server can group reports of one match without a shared id: `setup` is a hash of
//! the roster and map, and `started` (unix seconds on the reporter's clock) separates repeats of
//! the same setup.
//!
//! JSON, keys in this fixed order:
//! `{"v":1,"phase":"start"|"result","rid":hex8,"setup":hex32,"started":n,"client":s,"fp":hex8,
//!   "map":s,"me":slot,"players":[{"slot":n,"kind":"human"|"computer","name":s,"tag":s|null,
//!   "team":n,"race":n}],"result":null|{"outcome":s,"code":n,"turns":n,"sync_mismatch":n}}`

use sha2::{Digest, Sha256};
use w2r_formats::json::{Number, Value};

use crate::game::GameInfo;
use crate::hex;
use crate::slot::{Kind, Player};

pub const VERSION: u32 = 1;
const MAX_NAME: usize = 40;
const MAX_PLAYERS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Start,
    Result,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Start => "start",
            Phase::Result => "result",
        }
    }
}

/// What the reporter's own entry of `PlayerMatchResult` said (win 3, loss 2) or that it never
/// said anything after the match had run (surrender, leave, crash: all look alike from here).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Win,
    Loss,
    Left,
    /// A code this version has not seen (a draw is expected to be 5).
    Other(u8),
}

impl Outcome {
    /// `None` for a zero code with no turns played: no match happened.
    pub fn from_code(code: u8, turns: u32) -> Option<Outcome> {
        match code {
            3 => Some(Outcome::Win),
            2 => Some(Outcome::Loss),
            0 if turns > 0 => Some(Outcome::Left),
            0 => None,
            c => Some(Outcome::Other(c)),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Win => "win",
            Outcome::Loss => "loss",
            Outcome::Left => "left",
            Outcome::Other(_) => "other",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultInfo {
    pub outcome: Outcome,
    /// The raw `PlayerMatchResult` byte of the reporter.
    pub code: u8,
    pub turns: u32,
    /// The game's own lockstep desync flag at the end.
    pub sync_mismatch: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub phase: Phase,
    /// Random per report: lets a server drop a re-upload of the same report.
    pub rid: [u8; 8],
    pub setup: [u8; 32],
    pub started: u64,
    /// `w2rpm <version>`.
    pub client: String,
    /// The simulation fingerprint, 4 bytes hex.
    pub fingerprint: String,
    pub map: String,
    /// The reporter's own slot.
    pub me: u8,
    pub players: Vec<Player>,
    /// The settings the match was played with; absent from reports made by older clients.
    pub game: Option<GameInfo>,
    /// Present on `Result` reports only.
    pub result: Option<ResultInfo>,
}

/// Hash of what every participant sees identically: the map and each occupied slot's index, kind,
/// identity (BattleTag or name), team and race, in slot order.
pub fn setup_id(map: &str, players: &[Player]) -> [u8; 32] {
    let mut sorted: Vec<&Player> = players.iter().collect();
    sorted.sort_by_key(|p| p.slot);
    let mut h = Sha256::new();
    h.update(b"w2r-setup-1\n");
    h.update(map.as_bytes());
    h.update(b"\n");
    for p in sorted {
        h.update(
            format!(
                "{}|{}|{}|{}|{}\n",
                p.slot,
                p.kind.as_str(),
                p.identity(),
                p.team,
                p.race
            )
            .as_bytes(),
        );
    }
    h.finalize().into()
}

fn num(n: impl ToString) -> Value {
    Value::Number(Number {
        text: n.to_string(),
    })
}

fn text(s: &str) -> Value {
    Value::String(s.to_string())
}

fn clip(s: &str) -> String {
    s.chars().take(MAX_NAME).collect()
}

fn object(pairs: Vec<(&str, Value)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

impl Report {
    pub fn to_value(&self) -> Value {
        let players = self
            .players
            .iter()
            .take(MAX_PLAYERS)
            .map(|p| {
                object(vec![
                    ("slot", num(p.slot)),
                    ("kind", text(p.kind.as_str())),
                    ("name", Value::String(clip(&p.name))),
                    (
                        "tag",
                        p.tag
                            .as_deref()
                            .map_or(Value::Null, |t| Value::String(clip(t))),
                    ),
                    ("team", num(p.team)),
                    ("race", num(p.race)),
                ])
            })
            .collect();
        let result = match &self.result {
            None => Value::Null,
            Some(r) => object(vec![
                ("outcome", text(r.outcome.as_str())),
                ("code", num(r.code)),
                ("turns", num(r.turns)),
                ("sync_mismatch", num(r.sync_mismatch)),
            ]),
        };
        let mut fields = vec![
            ("v", num(VERSION)),
            ("phase", text(self.phase.as_str())),
            ("rid", Value::String(hex::encode(&self.rid))),
            ("setup", Value::String(hex::encode(&self.setup))),
            ("started", num(self.started)),
            ("client", Value::String(clip(&self.client))),
            ("fp", Value::String(self.fingerprint.clone())),
            ("map", Value::String(clip(&self.map))),
            ("me", num(self.me)),
            ("players", Value::Array(players)),
        ];
        // Older clients sent no `game` key; leave it out rather than send null, so a report
        // re-built from one serialises to the bytes that were signed.
        if let Some(g) = &self.game {
            fields.push(("game", g.to_value()));
        }
        fields.push(("result", result));
        object(fields)
    }

    /// Reads a report back. Strict about shape, not about unknown extra keys.
    pub fn from_value(v: &Value) -> Result<Report, &'static str> {
        let get = |k: &str| v.get(k).ok_or("missing field");
        let uint = |val: &Value| val.as_number().and_then(Number::as_u64).ok_or("bad number");
        let string = |val: &Value| val.as_str().map(str::to_string).ok_or("bad string");
        if uint(get("v")?)? != u64::from(VERSION) {
            return Err("unsupported version");
        }
        let phase = match get("phase")?.as_str() {
            Some("start") => Phase::Start,
            Some("result") => Phase::Result,
            _ => return Err("bad phase"),
        };
        let rid =
            hex::decode_array::<8>(get("rid")?.as_str().ok_or("bad rid")?).ok_or("bad rid")?;
        let setup = hex::decode_array::<32>(get("setup")?.as_str().ok_or("bad setup")?)
            .ok_or("bad setup")?;
        let mut players = Vec::new();
        for p in get("players")?.as_array().ok_or("bad players")? {
            let field = |k: &str| p.get(k).ok_or("missing player field");
            let kind = Kind::parse(field("kind")?.as_str().ok_or("bad kind")?).ok_or("bad kind")?;
            let tag = match field("tag")? {
                Value::Null => None,
                t => Some(t.as_str().ok_or("bad tag")?.to_string()),
            };
            players.push(Player {
                slot: u8::try_from(uint(field("slot")?)?).map_err(|_| "bad slot")?,
                kind,
                name: string(field("name")?)?,
                tag,
                team: u8::try_from(uint(field("team")?)?).map_err(|_| "bad team")?,
                race: u8::try_from(uint(field("race")?)?).map_err(|_| "bad race")?,
            });
        }
        let game = match v.get("game") {
            None | Some(Value::Null) => None,
            Some(g) => Some(GameInfo::from_value(g)?),
        };
        let result = match get("result")? {
            Value::Null => None,
            r => {
                let field = |k: &str| r.get(k).ok_or("missing result field");
                let code = u8::try_from(uint(field("code")?)?).map_err(|_| "bad code")?;
                let turns = u32::try_from(uint(field("turns")?)?).map_err(|_| "bad turns")?;
                Some(ResultInfo {
                    outcome: Outcome::from_code(code, turns).ok_or("no match")?,
                    code,
                    turns,
                    sync_mismatch: u32::try_from(uint(field("sync_mismatch")?)?)
                        .map_err(|_| "bad sync_mismatch")?,
                })
            }
        };
        if (phase == Phase::Result) != result.is_some() {
            return Err("result block does not match the phase");
        }
        Ok(Report {
            phase,
            rid,
            setup,
            started: uint(get("started")?)?,
            client: string(get("client")?)?,
            fingerprint: string(get("fp")?)?,
            map: string(get("map")?)?,
            me: u8::try_from(uint(get("me")?)?).map_err(|_| "bad me")?,
            players,
            game,
            result,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(slot: u8, kind: Kind, name: &str, tag: Option<&str>, team: u8) -> Player {
        Player {
            slot,
            kind,
            name: name.to_string(),
            tag: tag.map(str::to_string),
            team,
            race: 0,
        }
    }

    fn roster() -> Vec<Player> {
        vec![
            player(0, Kind::Computer, "Nation of Stromgarde", None, 2),
            player(1, Kind::Human, "MrKrinkle", Some("MrKrinkle#11951"), 1),
        ]
    }

    fn sample(phase: Phase, result: Option<ResultInfo>) -> Report {
        let players = roster();
        Report {
            phase,
            rid: [1, 2, 3, 4, 5, 6, 7, 8],
            setup: setup_id("Battle_2.pud", &players),
            started: 1_760_000_000,
            client: "w2rpm 0.1.0".into(),
            fingerprint: "5459b549".into(),
            map: "Battle_2.pud".into(),
            me: 1,
            players,
            game: None,
            result,
        }
    }

    #[test]
    fn outcomes_from_the_observed_codes() {
        assert_eq!(Outcome::from_code(3, 5994), Some(Outcome::Win));
        assert_eq!(Outcome::from_code(2, 1251), Some(Outcome::Loss));
        assert_eq!(Outcome::from_code(0, 637), Some(Outcome::Left));
        assert_eq!(Outcome::from_code(0, 0), None);
        assert_eq!(Outcome::from_code(5, 10), Some(Outcome::Other(5)));
    }

    #[test]
    fn setup_id_ignores_order_and_changes_with_the_roster() {
        let a = roster();
        let mut b = roster();
        b.reverse();
        assert_eq!(setup_id("m", &a), setup_id("m", &b));
        assert_ne!(setup_id("m", &a), setup_id("n", &a));
        let mut c = roster();
        c[1].team = 2;
        assert_ne!(setup_id("m", &a), setup_id("m", &c));
        let mut d = roster();
        d[1].tag = Some("MrKrinkle#99999".into());
        assert_ne!(setup_id("m", &a), setup_id("m", &d));
    }

    #[test]
    fn start_and_result_round_trip() {
        let start = sample(Phase::Start, None);
        assert_eq!(Report::from_value(&start.to_value()), Ok(start.clone()));
        let done = sample(
            Phase::Result,
            Some(ResultInfo {
                outcome: Outcome::Win,
                code: 3,
                turns: 5994,
                sync_mismatch: 0,
            }),
        );
        assert_eq!(Report::from_value(&done.to_value()), Ok(done.clone()));
        // Through text as well: the signature covers the serialised form.
        let json = done.to_value().to_json();
        let parsed = w2r_formats::json::parse(json.as_bytes()).unwrap();
        assert_eq!(Report::from_value(&parsed), Ok(done));
        assert_eq!(parsed.to_json(), json);
    }

    #[test]
    fn a_phase_without_its_result_block_is_refused() {
        assert!(Report::from_value(&sample(Phase::Result, None).to_value()).is_err());
        let with_result = Some(ResultInfo {
            outcome: Outcome::Loss,
            code: 2,
            turns: 10,
            sync_mismatch: 0,
        });
        assert!(Report::from_value(&sample(Phase::Start, with_result).to_value()).is_err());
    }

    #[test]
    fn long_names_are_clipped() {
        let mut r = sample(Phase::Start, None);
        r.players[0].name = "x".repeat(200);
        let back = Report::from_value(&r.to_value()).unwrap();
        assert_eq!(back.players[0].name.len(), MAX_NAME);
    }
}
