//! The read side: ratings, the leaderboard and recent games, as JSON for the in-game lobby and
//! for a future website. Everything here is public data (BattleTags are public) and read-only.

use serde_json::{Value as J, json};
use worker::wasm_bindgen::JsValue;
use worker::*;

use crate::jobs::{n, s};

/// A lobby asks for a whole room at once; more than this is a mistake.
const MAX_TAGS: usize = 16;
const MAX_LIMIT: usize = 100;
/// Leaderboard pages are cached this long (seconds): ratings change a few times an hour.
const CACHE_SECS: u32 = 60;

fn param(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

fn reply(body: &J) -> Result<Response> {
    let mut resp = Response::ok(body.to_string())?;
    let h = resp.headers_mut();
    h.set("content-type", "application/json")?;
    h.set("access-control-allow-origin", "*")?;
    h.set("cache-control", &format!("public, max-age={CACHE_SECS}"))?;
    Ok(resp)
}

pub fn preflight() -> Result<Response> {
    let mut resp = Response::empty()?.with_status(204);
    let h = resp.headers_mut();
    h.set("access-control-allow-origin", "*")?;
    h.set("access-control-allow-methods", "GET, OPTIONS")?;
    h.set("access-control-max-age", "86400")?;
    Ok(resp)
}

fn player(r: &J) -> J {
    json!({
        "tag": s(r, "tag"),
        "mu": n(r, "mu"),
        "sigma": n(r, "sigma"),
        "skill": n(r, "skill"),
        "games": n(r, "games") as u32,
        "wins": n(r, "wins") as u32,
        "losses": n(r, "losses") as u32,
        "last_game": n(r, "last_game") as u64,
    })
}

/// `GET /v1/ratings?tags=a%231,b%232`: the ratings of the tags that have one. A tag with no rating
/// is simply absent; a client treats it as a new player.
pub async fn ratings(db: &D1Database, url: &Url) -> Result<Response> {
    let tags: Vec<String> = param(url, "tags")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .take(MAX_TAGS)
        .map(str::to_string)
        .collect();
    if tags.is_empty() {
        return reply(&json!({"ratings": []}));
    }
    let binds: Vec<JsValue> = tags.iter().map(|t| JsValue::from_str(t)).collect();
    let sql = format!(
        "SELECT tag, mu, sigma, skill, games, wins, losses, last_game FROM ratings WHERE tag IN ({})",
        vec!["?"; binds.len()].join(",")
    );
    let found = db.prepare(sql).bind(&binds)?.all().await?.results::<J>()?;
    reply(&json!({"ratings": found.iter().map(player).collect::<Vec<_>>()}))
}

/// `GET /v1/leaderboard?limit=50&min_games=3`: best skill first (`mu - 3 sigma`).
pub async fn leaderboard(db: &D1Database, url: &Url) -> Result<Response> {
    let limit = param(url, "limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50)
        .clamp(1, MAX_LIMIT);
    let min_games = param(url, "min_games")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(1);
    let found = db
        .prepare(
            "SELECT tag, mu, sigma, skill, games, wins, losses, last_game FROM ratings \
             WHERE games >= ?1 ORDER BY skill DESC, tag LIMIT ?2",
        )
        .bind(&[
            JsValue::from_f64(f64::from(min_games)),
            JsValue::from_f64(limit as f64),
        ])?
        .all()
        .await?
        .results::<J>()?;
    let ranked: Vec<J> = found
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let mut p = player(r);
            p["rank"] = json!(i + 1);
            p
        })
        .collect();
    reply(&json!({"players": ranked}))
}

/// `GET /v1/games?limit=20`: the latest verified games, newest first, with their settings and what
/// each rating did.
pub async fn recent_games(db: &D1Database, url: &Url) -> Result<Response> {
    let limit = param(url, "limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20)
        .clamp(1, MAX_LIMIT);
    let found = db
        .prepare(
            "SELECT id, started, map, map_sha, resources, speed, peasants_only, fixed_start, roster, \
             winner_team, turns, rated, changes FROM games WHERE status = 'verified' \
             ORDER BY started DESC, id DESC LIMIT ?1",
        )
        .bind(&[JsValue::from_f64(limit as f64)])?
        .all()
        .await?
        .results::<J>()?;
    let list: Vec<J> = found
        .iter()
        .map(|g| {
            let parsed = |k: &str| serde_json::from_str::<J>(&s(g, k)).unwrap_or(J::Null);
            json!({
                "id": n(g, "id") as u64,
                "started": n(g, "started") as u64,
                "map": s(g, "map"),
                "map_sha": g.get("map_sha").cloned().unwrap_or(J::Null),
                "resources": g.get("resources").cloned().unwrap_or(J::Null),
                "speed": g.get("speed").cloned().unwrap_or(J::Null),
                "peasants_only": g.get("peasants_only").cloned().unwrap_or(J::Null),
                "fixed_start": g.get("fixed_start").cloned().unwrap_or(J::Null),
                "winner_team": g.get("winner_team").cloned().unwrap_or(J::Null),
                "turns": g.get("turns").cloned().unwrap_or(J::Null),
                "rated": n(g, "rated") != 0.0,
                "players": parsed("roster"),
                "changes": parsed("changes"),
            })
        })
        .collect();
    reply(&json!({"games": list}))
}
