//! What the service accepts into the report log. Pure: bytes and a clock in, a row or a reason
//! out, so it is tested natively and runs unchanged inside the Worker.
//!
//! Policy (docs/stats-reporting.md, "when in doubt, don't rate"): a report is stored only when its
//! signature verifies, its roster hashes to the `setup` it claims, the reporter is a human in that
//! roster, and every human in the roster has a BattleTag. Whether a game is *true* is decided
//! later, from several reports; this only keeps malformed or unratable ones out of the log.

use w2r_stats::envelope::Envelope;
use w2r_stats::hex;
use w2r_stats::report::{Outcome, Phase, Report, setup_id};
use w2r_stats::slot::Kind;

/// A report older than this is refused: the log is for recent games, not a replayed archive.
pub const MAX_AGE_SECS: u64 = 14 * 24 * 3600;
/// Reporter clocks drift; a start further ahead than this is a wrong clock or a forgery.
pub const MAX_AHEAD_SECS: u64 = 3600;
/// The longest plausible game in turns (a game turn is about a tenth of a second).
pub const MAX_TURNS: u32 = 2_000_000;

#[derive(Debug, PartialEq, Eq)]
pub enum Reject {
    Malformed(&'static str),
    BadSignature,
    /// The roster does not hash to the `setup` the report claims.
    SetupMismatch,
    /// The reporter is not a human in the roster.
    NotAParticipant,
    /// A human in the roster has no BattleTag, so the game cannot be rated.
    MissingTag,
    TooOld,
    FromTheFuture,
    Implausible(&'static str),
}

impl Reject {
    pub fn reason(&self) -> &'static str {
        match self {
            Reject::Malformed(why) => why,
            Reject::BadSignature => "bad signature",
            Reject::SetupMismatch => "setup does not match the roster",
            Reject::NotAParticipant => "reporter is not a human in the roster",
            Reject::MissingTag => "a human has no BattleTag",
            Reject::TooOld => "too old",
            Reject::FromTheFuture => "start time is in the future",
            Reject::Implausible(why) => why,
        }
    }
}

/// A report that passed, flattened into what the `reports` table stores.
#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    pub key: String,
    pub rid: String,
    pub phase: &'static str,
    pub setup: String,
    pub started: u64,
    pub received: u64,
    pub map: String,
    pub me: u8,
    pub me_tag: String,
    pub outcome: Option<&'static str>,
    pub code: Option<u8>,
    pub turns: Option<u32>,
    pub sync: Option<u32>,
    pub envelope: String,
}

/// Checks one envelope. `now` is the server's unix time in seconds.
pub fn check(raw: &[u8], now: u64) -> Result<Row, Reject> {
    let envelope = Envelope::parse(raw).map_err(Reject::Malformed)?;
    let report = envelope.report().map_err(|e| match e {
        "bad signature" => Reject::BadSignature,
        other => Reject::Malformed(other),
    })?;
    validate(&report, now)?;
    let me_tag = report
        .players
        .iter()
        .find(|p| p.slot == report.me)
        .and_then(|p| p.tag.clone())
        .ok_or(Reject::MissingTag)?;
    let result = report.result.as_ref();
    Ok(Row {
        key: hex::encode(&envelope.key),
        rid: hex::encode(&report.rid),
        phase: report.phase.as_str(),
        setup: hex::encode(&report.setup),
        started: report.started,
        received: now,
        map: report.map.clone(),
        me: report.me,
        me_tag,
        outcome: result.map(|r| match r.outcome {
            Outcome::Win => "win",
            Outcome::Loss => "loss",
            Outcome::Left => "left",
            Outcome::Other(_) => "other",
        }),
        code: result.map(|r| r.code),
        turns: result.map(|r| r.turns),
        sync: result.map(|r| r.sync_mismatch),
        // Stored as received, not re-serialised, so the log keeps the exact signed bytes.
        envelope: String::from_utf8_lossy(raw).into_owned(),
    })
}

fn validate(report: &Report, now: u64) -> Result<(), Reject> {
    if setup_id(&report.map, &report.players) != report.setup {
        return Err(Reject::SetupMismatch);
    }
    if report.players.len() < 2 {
        return Err(Reject::Implausible("fewer than two players"));
    }
    let mut slots: Vec<u8> = report.players.iter().map(|p| p.slot).collect();
    slots.sort_unstable();
    slots.dedup();
    if slots.len() != report.players.len() || slots.iter().any(|&s| s >= 8) {
        return Err(Reject::Implausible("bad slot numbers"));
    }
    let me = report.players.iter().find(|p| p.slot == report.me);
    if me.is_none_or(|p| p.kind != Kind::Human) {
        return Err(Reject::NotAParticipant);
    }
    if report
        .players
        .iter()
        .any(|p| p.kind == Kind::Human && p.tag.is_none())
    {
        return Err(Reject::MissingTag);
    }
    if report.started + MAX_AGE_SECS < now {
        return Err(Reject::TooOld);
    }
    if report.started > now + MAX_AHEAD_SECS {
        return Err(Reject::FromTheFuture);
    }
    if let Some(r) = &report.result {
        if r.turns > MAX_TURNS {
            return Err(Reject::Implausible("too many turns"));
        }
        if report.phase != Phase::Result {
            return Err(Reject::Malformed("result block does not match the phase"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use w2r_stats::envelope::seal;
    use w2r_stats::identity::Identity;
    use w2r_stats::report::ResultInfo;
    use w2r_stats::slot::Player;

    const NOW: u64 = 1_791_700_000;

    fn human(slot: u8, name: &str, team: u8, tag: bool) -> Player {
        Player {
            slot,
            kind: Kind::Human,
            name: name.into(),
            tag: tag.then(|| format!("{name}#1234")),
            team,
            race: 0,
        }
    }

    fn report(players: Vec<Player>, me: u8, phase: Phase) -> Report {
        Report {
            phase,
            rid: [9; 8],
            setup: setup_id("Battle_2.pud", &players),
            started: NOW - 600,
            client: "w2rpm 0.1.0".into(),
            fingerprint: "5459b549".into(),
            map: "Battle_2.pud".into(),
            me,
            players,
            result: (phase == Phase::Result).then(|| ResultInfo {
                outcome: Outcome::Win,
                code: 3,
                turns: 9000,
                sync_mismatch: 0,
            }),
        }
    }

    fn sealed(r: &Report) -> Vec<u8> {
        let id = Identity::from_secret([7; 32]);
        seal(&id, r).to_json().into_bytes()
    }

    fn two_humans() -> Vec<Player> {
        vec![human(0, "Ann", 1, true), human(1, "Bob", 2, true)]
    }

    #[test]
    fn a_good_result_becomes_a_row() {
        let raw = sealed(&report(two_humans(), 0, Phase::Result));
        let row = check(&raw, NOW).unwrap();
        assert_eq!(row.phase, "result");
        assert_eq!(row.me_tag, "Ann#1234");
        assert_eq!(row.outcome, Some("win"));
        assert_eq!(row.turns, Some(9000));
        assert_eq!(row.envelope.as_bytes(), raw.as_slice());
        assert_eq!(row.key.len(), 64);
    }

    #[test]
    fn a_start_has_no_result_columns() {
        let row = check(&sealed(&report(two_humans(), 1, Phase::Start)), NOW).unwrap();
        assert_eq!(
            (row.phase, row.outcome, row.code, row.turns, row.sync),
            ("start", None, None, None, None)
        );
        assert_eq!(row.me_tag, "Bob#1234");
    }

    #[test]
    fn a_changed_body_fails_the_signature() {
        let raw = String::from_utf8(sealed(&report(two_humans(), 0, Phase::Result))).unwrap();
        let forged = raw.replace("\"turns\":9000", "\"turns\":9001");
        assert_eq!(check(forged.as_bytes(), NOW), Err(Reject::BadSignature));
    }

    #[test]
    fn a_roster_that_does_not_hash_to_setup_is_refused() {
        let mut r = report(two_humans(), 0, Phase::Result);
        r.setup = [0; 32];
        assert_eq!(check(&sealed(&r), NOW), Err(Reject::SetupMismatch));
    }

    #[test]
    fn the_reporter_must_be_a_human_in_the_roster() {
        let mut bot = human(1, "Azeroth", 2, false);
        bot.kind = Kind::Computer;
        let r = report(vec![human(0, "Ann", 1, true), bot], 1, Phase::Result);
        assert_eq!(check(&sealed(&r), NOW), Err(Reject::NotAParticipant));
        let r = report(two_humans(), 5, Phase::Result);
        assert_eq!(check(&sealed(&r), NOW), Err(Reject::NotAParticipant));
    }

    #[test]
    fn a_human_without_a_tag_makes_the_game_unratable() {
        let r = report(
            vec![human(0, "Ann", 1, true), human(1, "Bob", 2, false)],
            0,
            Phase::Result,
        );
        assert_eq!(check(&sealed(&r), NOW), Err(Reject::MissingTag));
    }

    #[test]
    fn computers_need_no_tag() {
        let mut bot = human(1, "Azeroth", 2, false);
        bot.kind = Kind::Computer;
        let r = report(vec![human(0, "Ann", 1, true), bot], 0, Phase::Result);
        assert!(check(&sealed(&r), NOW).is_ok());
    }

    #[test]
    fn old_and_future_reports_are_refused() {
        let mut r = report(two_humans(), 0, Phase::Result);
        r.started = NOW - MAX_AGE_SECS - 1;
        assert_eq!(check(&sealed(&r), NOW), Err(Reject::TooOld));
        r.started = NOW + MAX_AHEAD_SECS + 1;
        assert_eq!(check(&sealed(&r), NOW), Err(Reject::FromTheFuture));
    }

    #[test]
    fn garbage_is_malformed_not_a_panic() {
        assert!(matches!(check(b"nope", NOW), Err(Reject::Malformed(_))));
        assert!(matches!(check(b"", NOW), Err(Reject::Malformed(_))));
    }
}
