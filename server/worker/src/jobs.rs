//! The scheduled job: turns stored reports into games, verdicts and ratings. Uploads only store
//! reports (one D1 query each); everything else happens here, in a handful of batched queries,
//! because a free-plan invocation may run at most 50 D1 queries.
//!
//! - every few minutes (`catch_up`): bind new keys to their tags, put loose reports into games,
//!   judge the games they touched, and rate the ones that just became verified;
//! - nightly (`rebuild`), or sooner after a dispute reverses a rated game: recompute every rating
//!   from the games in the order they were played, so a late-arriving report cannot leave the
//!   ladder in arrival order.
//!
//! Nothing here is the source of truth: `reports` is. Ratings can always be rebuilt.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value as J;
use worker::wasm_bindgen::JsValue;
use worker::*;

use crate::games::{self, Known, Loose, Standing, Status, Target, Vote};

/// Loose reports taken per run; the rest wait for the next one.
const PER_RUN: usize = 40;
/// Statements per `batch` call.
const CHUNK: usize = 80;
/// Most games a rebuild reads (a limit of this design, see docs/stats-service.md).
const REBUILD_LIMIT: usize = 3000;

pub fn n(v: &J, k: &str) -> f64 {
    v.get(k).and_then(J::as_f64).unwrap_or(0.0)
}

pub fn s(v: &J, k: &str) -> String {
    v.get(k).and_then(J::as_str).unwrap_or("").to_string()
}

fn num(x: impl Into<f64>) -> JsValue {
    JsValue::from_f64(x.into())
}

fn text(x: &str) -> JsValue {
    JsValue::from_str(x)
}

fn opt(v: &J, k: &str) -> JsValue {
    match v.get(k).and_then(J::as_f64) {
        Some(x) => JsValue::from_f64(x),
        None => JsValue::NULL,
    }
}

fn opt_text(v: &J, k: &str) -> JsValue {
    match v.get(k).and_then(J::as_str) {
        Some(x) => JsValue::from_str(x),
        None => JsValue::NULL,
    }
}

async fn rows(db: &D1Database, sql: &str, binds: &[JsValue]) -> Result<Vec<J>> {
    db.prepare(sql).bind(binds)?.all().await?.results::<J>()
}

fn placeholders(count: usize) -> String {
    vec!["?"; count].join(",")
}

async fn run_batches(db: &D1Database, mut statements: Vec<D1PreparedStatement>) -> Result<()> {
    while !statements.is_empty() {
        let rest = statements.split_off(statements.len().min(CHUNK));
        db.batch(statements).await?;
        statements = rest;
    }
    Ok(())
}

async fn meta(db: &D1Database) -> Result<BTreeMap<String, String>> {
    Ok(rows(db, "SELECT k, v FROM meta", &[])
        .await?
        .iter()
        .map(|r| (s(r, "k"), s(r, "v")))
        .collect())
}

async fn set_meta(db: &D1Database, k: &str, v: &str) -> Result<()> {
    db.prepare("INSERT OR REPLACE INTO meta (k, v) VALUES (?1, ?2)")
        .bind(&[text(k), text(v)])?
        .run()
        .await?;
    Ok(())
}

async fn trusted(db: &D1Database) -> Result<BTreeSet<String>> {
    Ok(rows(db, "SELECT key FROM trusted_keys", &[])
        .await?
        .iter()
        .map(|r| s(r, "key"))
        .collect())
}

fn vote_of(r: &J) -> Vote {
    Vote {
        key: s(r, "key"),
        tag: s(r, "me_tag"),
        team: n(r, "me_team") as u8,
        outcome: s(r, "outcome"),
        turns: n(r, "turns") as u32,
        sync_mismatch: n(r, "sync") as u32,
    }
}

/// `{"tag":..,"won":..,"before":mu,"after":mu}` for each player a game moved.
fn changes_json(changes: &[games::Change]) -> String {
    let list: Vec<J> = changes
        .iter()
        .map(|c| {
            serde_json::json!({
                "tag": c.tag,
                "won": c.won,
                "mu_before": c.before.mu,
                "mu_after": c.after.mu,
            })
        })
        .collect();
    J::Array(list).to_string()
}

fn standing_stmt(db: &D1Database, tag: &str, e: &Standing, last_game: u64) -> Result<D1PreparedStatement> {
    db.prepare(
        "INSERT OR REPLACE INTO ratings (tag, mu, sigma, skill, games, wins, losses, last_game) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )
    .bind(&[
        text(tag),
        num(e.rating.mu),
        num(e.rating.sigma),
        num(e.rating.conservative()),
        num(e.games),
        num(e.wins),
        num(e.losses),
        num(last_game as f64),
    ])
}

fn verdict_stmt(
    db: &D1Database,
    id: f64,
    v: &games::Verdict,
    reports: usize,
    now: u64,
) -> Result<D1PreparedStatement> {
    db.prepare(
        "UPDATE games SET status = ?2, reason = ?3, winner_team = ?4, turns = ?5, reports = ?6, \
         updated = ?7, corroborated = ?8 WHERE id = ?1",
    )
    .bind(&[
        num(id),
        text(v.status.as_str()),
        text(v.reason),
        v.winner_team.map_or(JsValue::NULL, |t| num(f64::from(t))),
        v.turns.map_or(JsValue::NULL, |t| num(f64::from(t))),
        num(reports as f64),
        num(now as f64),
        num(if v.corroborated { 1.0 } else { 0.0 }),
    ])
}

/// How many corroborated games each of `keys` has taken part in (what its word is worth in a
/// dispute).
async fn trust_of(db: &D1Database, keys: &BTreeSet<String>) -> Result<BTreeMap<String, u32>> {
    if keys.is_empty() {
        return Ok(BTreeMap::new());
    }
    let binds: Vec<JsValue> = keys.iter().map(|k| text(k)).collect();
    let found = rows(
        db,
        &format!(
            "SELECT r.key AS key, COUNT(DISTINCT r.game_id) AS c FROM reports r \
             JOIN games g ON g.id = r.game_id \
             WHERE g.corroborated = 1 AND r.phase = 'result' AND r.ignored = 0 AND r.key IN ({}) \
             GROUP BY r.key",
            placeholders(binds.len())
        ),
        &binds,
    )
    .await?;
    Ok(found.iter().map(|r| (s(r, "key"), n(r, "c") as u32)).collect())
}

/// Groups loose reports into games and judges the games they touched. Returns how many reports it
/// placed.
pub async fn catch_up(db: &D1Database, now: u64) -> Result<usize> {
    let loose_rows = rows(
        db,
        &format!("SELECT id, setup, started FROM reports WHERE game_id IS NULL ORDER BY started LIMIT {PER_RUN}"),
        &[],
    )
    .await?;
    if loose_rows.is_empty() {
        return Ok(0);
    }
    // A key speaks for the first tag it reported under; later reports under another are ignored.
    run_batches(
        db,
        vec![
            db.prepare(
                "INSERT OR IGNORE INTO bindings (key, tag, first_seen) \
                 SELECT key, me_tag, received FROM reports WHERE game_id IS NULL ORDER BY received, id",
            ),
            db.prepare(
                "UPDATE reports SET ignored = 1 WHERE game_id IS NULL AND me_tag != \
                 (SELECT tag FROM bindings b WHERE b.key = reports.key)",
            ),
        ],
    )
    .await?;

    let loose: Vec<Loose> = loose_rows
        .iter()
        .map(|r| Loose {
            id: n(r, "id") as i64,
            setup: s(r, "setup"),
            started: n(r, "started") as u64,
        })
        .collect();
    let setups: BTreeSet<&str> = loose.iter().map(|l| l.setup.as_str()).collect();
    let setup_binds: Vec<JsValue> = setups.iter().map(|x| text(x)).collect();
    let known_sql = format!(
        "SELECT id, setup, started FROM games WHERE setup IN ({})",
        placeholders(setup_binds.len())
    );
    let known: Vec<Known> = rows(db, &known_sql, &setup_binds)
        .await?
        .iter()
        .map(|r| Known {
            id: n(r, "id") as i64,
            setup: s(r, "setup"),
            started: n(r, "started") as u64,
        })
        .collect();
    let plan = games::plan(&known, &loose);

    // New games take their roster and settings from the first report placed in them.
    if !plan.new_games.is_empty() {
        let mut first_of: BTreeMap<usize, i64> = BTreeMap::new();
        for (id, target) in &plan.placed {
            if let Target::New(i) = target {
                first_of.entry(*i).or_insert(*id);
            }
        }
        let ids: Vec<JsValue> = first_of.values().map(|id| num(*id as f64)).collect();
        let source = rows(
            db,
            &format!(
                "SELECT id, map, map_sha, resources, speed, flags, peasants_only, fixed_start, \
                 json_extract(envelope, '$.body.players') AS roster FROM reports WHERE id IN ({})",
                placeholders(ids.len())
            ),
            &ids,
        )
        .await?;
        let mut inserts = Vec::new();
        for (i, (setup, started)) in plan.new_games.iter().enumerate() {
            let Some(r) = first_of
                .get(&i)
                .and_then(|id| source.iter().find(|r| n(r, "id") as i64 == *id))
            else {
                continue;
            };
            inserts.push(
                db.prepare(
                    "INSERT INTO games (setup, started, map, map_sha, resources, speed, flags, \
                     peasants_only, fixed_start, roster, updated) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                )
                .bind(&[
                    text(setup),
                    num(*started as f64),
                    text(&s(r, "map")),
                    opt_text(r, "map_sha"),
                    opt_text(r, "resources"),
                    opt(r, "speed"),
                    opt(r, "flags"),
                    opt(r, "peasants_only"),
                    opt(r, "fixed_start"),
                    text(&s(r, "roster")),
                    num(now as f64),
                ])?,
            );
        }
        run_batches(db, inserts).await?;
    }

    // Find the ids the new games got, then attach every report.
    let after: Vec<Known> = rows(db, &known_sql, &setup_binds)
        .await?
        .iter()
        .map(|r| Known {
            id: n(r, "id") as i64,
            setup: s(r, "setup"),
            started: n(r, "started") as u64,
        })
        .collect();
    let id_of = |t: &Target| -> Option<i64> {
        match t {
            Target::Existing(id) => Some(*id),
            Target::New(i) => {
                let (setup, started) = plan.new_games.get(*i)?;
                after
                    .iter()
                    .find(|k| &k.setup == setup && k.started == *started)
                    .map(|k| k.id)
            }
        }
    };
    let mut attach = Vec::new();
    let mut touched: BTreeSet<i64> = BTreeSet::new();
    let mut placed = 0;
    for (report, target) in &plan.placed {
        let Some(game) = id_of(target) else { continue };
        attach.push(
            db.prepare("UPDATE reports SET game_id = ?2 WHERE id = ?1")
                .bind(&[num(*report as f64), num(game as f64)])?,
        );
        touched.insert(game);
        placed += 1;
    }
    run_batches(db, attach).await?;
    judge_games(db, &touched, now).await?;
    Ok(placed)
}

/// Judges the given games and rates the ones that just became verified.
async fn judge_games(db: &D1Database, ids: &BTreeSet<i64>, now: u64) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let binds: Vec<JsValue> = ids.iter().map(|i| num(*i as f64)).collect();
    let list = placeholders(binds.len());
    let game_rows = rows(
        db,
        &format!("SELECT id, started, roster, status, rated, winner_team FROM games WHERE id IN ({list})"),
        &binds,
    )
    .await?;
    let vote_rows = rows(
        db,
        &format!(
            "SELECT game_id, key, me_tag, me_team, outcome, turns, sync FROM reports \
             WHERE phase = 'result' AND ignored = 0 AND game_id IN ({list})"
        ),
        &binds,
    )
    .await?;
    let trusted = trusted(db).await?;
    let keys: BTreeSet<String> = vote_rows.iter().map(|v| s(v, "key")).collect();
    let trust = trust_of(db, &keys).await?;

    struct Rated {
        id: f64,
        started: u64,
        roster: Vec<w2r_stats::slot::Player>,
        winner: u8,
    }
    let mut statements = Vec::new();
    let mut to_rate: Vec<Rated> = Vec::new();
    let mut reversed = false;
    for g in &game_rows {
        let id = n(g, "id");
        let Some(roster) = games::roster_from_json(&s(g, "roster")) else {
            continue;
        };
        let votes: Vec<Vote> = vote_rows
            .iter()
            .filter(|v| n(v, "game_id") == id)
            .map(vote_of)
            .collect();
        let verdict = games::judge(&roster, &votes, &trusted, &trust);
        statements.push(verdict_stmt(db, id, &verdict, votes.len(), now)?);
        let was_rated = n(g, "rated") != 0.0;
        let same_winner = verdict.winner_team.map(f64::from) == g.get("winner_team").and_then(J::as_f64);
        match (verdict.rated, was_rated) {
            (true, false) => to_rate.push(Rated {
                id,
                started: n(g, "started") as u64,
                roster,
                winner: verdict.winner_team.unwrap_or(0),
            }),
            // It was rated and no longer deserves it, or the winner changed: only a rebuild fixes that.
            (false, true) => reversed = true,
            (true, true) if !same_winner => reversed = true,
            _ => {}
        }
        debug_assert!(verdict.status != Status::Verified || verdict.winner_team.is_some());
    }
    run_batches(db, statements).await?;

    if !to_rate.is_empty() {
        to_rate.sort_by_key(|g| g.started);
        let mut tags: BTreeSet<String> = BTreeSet::new();
        for g in &to_rate {
            tags.extend(g.roster.iter().filter_map(|p| p.tag.clone()));
        }
        let tag_binds: Vec<JsValue> = tags.iter().map(|t| text(t)).collect();
        let mut standings: BTreeMap<String, Standing> = BTreeMap::new();
        for r in rows(
            db,
            &format!(
                "SELECT tag, mu, sigma, games, wins, losses FROM ratings WHERE tag IN ({})",
                placeholders(tag_binds.len())
            ),
            &tag_binds,
        )
        .await?
        {
            standings.insert(
                s(&r, "tag"),
                Standing {
                    rating: w2r_stats::rating::Rating {
                        mu: n(&r, "mu"),
                        sigma: n(&r, "sigma"),
                    },
                    games: n(&r, "games") as u32,
                    wins: n(&r, "wins") as u32,
                    losses: n(&r, "losses") as u32,
                },
            );
        }
        let newest_rated = rows(db, "SELECT MAX(started) AS m FROM games WHERE rated = 1", &[])
            .await?
            .first()
            .map_or(0.0, |r| n(r, "m")) as u64;
        let mut writes = Vec::new();
        let mut touched_tags: BTreeMap<String, u64> = BTreeMap::new();
        for g in &to_rate {
            let changes = games::apply(&mut standings, &g.roster, g.winner);
            for c in &changes {
                touched_tags.insert(c.tag.clone(), g.started);
            }
            writes.push(
                db.prepare("UPDATE games SET rated = 1, changes = ?2 WHERE id = ?1")
                    .bind(&[num(g.id), text(&changes_json(&changes))])?,
            );
            if g.started < newest_rated {
                // Played before a game that is already counted: right once rebuilt in time order.
                reversed = true;
            }
        }
        for (tag, last) in &touched_tags {
            if let Some(e) = standings.get(tag) {
                writes.push(standing_stmt(db, tag, e, *last)?);
            }
        }
        run_batches(db, writes).await?;
    }
    if reversed {
        set_meta(db, "rebuild", "1").await?;
    }
    Ok(())
}

/// Recomputes every verdict and rating from the stored reports, games in the order they began.
pub async fn rebuild(db: &D1Database, now: u64) -> Result<usize> {
    let game_rows = rows(
        db,
        &format!(
            "SELECT id, started, roster FROM games ORDER BY started, id LIMIT {REBUILD_LIMIT}"
        ),
        &[],
    )
    .await?;
    let vote_rows = rows(
        db,
        "SELECT game_id, key, me_tag, me_team, outcome, turns, sync FROM reports \
         WHERE phase = 'result' AND ignored = 0 AND game_id IS NOT NULL",
        &[],
    )
    .await?;
    let trusted = trusted(db).await?;
    let mut by_game: BTreeMap<i64, Vec<Vote>> = BTreeMap::new();
    for v in &vote_rows {
        by_game.entry(n(v, "game_id") as i64).or_default().push(vote_of(v));
    }
    let mut standings: BTreeMap<String, Standing> = BTreeMap::new();
    let mut last_seen: BTreeMap<String, u64> = BTreeMap::new();
    let mut statements = Vec::new();
    // First pass, without track records: which games both sides agree on, and so who has earned
    // trust. The second pass uses that to settle disputes.
    let mut trust: BTreeMap<String, u32> = BTreeMap::new();
    for g in &game_rows {
        let Some(roster) = games::roster_from_json(&s(g, "roster")) else {
            continue;
        };
        let votes = by_game.get(&(n(g, "id") as i64)).cloned().unwrap_or_default();
        if games::judge(&roster, &votes, &trusted, &BTreeMap::new()).corroborated {
            let keys: BTreeSet<&str> = votes.iter().map(|v| v.key.as_str()).collect();
            for k in keys {
                *trust.entry(k.to_string()).or_default() += 1;
            }
        }
    }
    for g in &game_rows {
        let id = n(g, "id");
        let Some(roster) = games::roster_from_json(&s(g, "roster")) else {
            continue;
        };
        let votes = by_game.remove(&(id as i64)).unwrap_or_default();
        let verdict = games::judge(&roster, &votes, &trusted, &trust);
        statements.push(verdict_stmt(db, id, &verdict, votes.len(), now)?);
        let (rated, changes) = match (verdict.rated, verdict.winner_team) {
            (true, Some(winner)) => {
                let changes = games::apply(&mut standings, &roster, winner);
                for c in &changes {
                    last_seen.insert(c.tag.clone(), n(g, "started") as u64);
                }
                (1.0, changes_json(&changes))
            }
            _ => (0.0, "[]".to_string()),
        };
        statements.push(
            db.prepare("UPDATE games SET rated = ?2, changes = ?3 WHERE id = ?1")
                .bind(&[num(id), num(rated), text(&changes)])?,
        );
    }
    statements.push(db.prepare("DELETE FROM ratings"));
    for (tag, e) in &standings {
        statements.push(standing_stmt(db, tag, e, last_seen.get(tag).copied().unwrap_or(0))?);
    }
    run_batches(db, statements).await?;
    set_meta(db, "rebuild", "0").await?;
    Ok(game_rows.len())
}

/// A rebuild asked for by a reversed verdict or a late report waits at least this long since the
/// last one, so a burst of out-of-order games costs one pass, not many.
const REBUILD_EVERY_SECS: u64 = 3600;

/// What the scheduled event does. `nightly` is the once-a-day trigger, which always rebuilds.
pub async fn scheduled(db: &D1Database, now: u64, nightly: bool) -> Result<String> {
    let placed = catch_up(db, now).await?;
    let flags = meta(db).await?;
    let wanted = flags.get("rebuild").is_some_and(|v| v == "1");
    let last: u64 = flags.get("last_rebuild").and_then(|v| v.parse().ok()).unwrap_or(0);
    let rebuilt = if nightly || (wanted && now.saturating_sub(last) >= REBUILD_EVERY_SECS) {
        let games = rebuild(db, now).await?;
        set_meta(db, "last_rebuild", &now.to_string()).await?;
        Some(games)
    } else {
        None
    };
    Ok(format!("placed {placed} reports, rebuilt {rebuilt:?}"))
}
