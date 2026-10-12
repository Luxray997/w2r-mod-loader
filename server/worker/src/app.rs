//! HTTP routes and D1 access. Kept thin: all decisions are in `ingest`.

use worker::wasm_bindgen::JsValue;
use worker::*;

use crate::ingest::{self, Row};
use crate::replay;

/// Most envelopes in one upload. The client sends its outbox in batches.
const MAX_BATCH: usize = 20;
/// Largest request body: 20 envelopes of at most 64 KiB would be far more than real reports.
const MAX_BODY: usize = 512 * 1024;

fn now_secs() -> u64 {
    (Date::now().as_millis() / 1000) as u64
}

/// Asks a Workers rate-limit binding whether `key` may make another request now. A binding that
/// is missing, or a call that fails, lets the request through: limiting is protection, not a gate.
async fn within_limit(env: &Env, binding: &str, key: &str) -> bool {
    use worker::js_sys::{Function, Object, Reflect};
    use worker::wasm_bindgen::JsCast;
    let ask = async {
        let limiter = Reflect::get(env.as_ref(), &JsValue::from_str(binding)).ok()?;
        if limiter.is_undefined() || limiter.is_null() {
            return None;
        }
        let options = Object::new();
        Reflect::set(&options, &JsValue::from_str("key"), &JsValue::from_str(key)).ok()?;
        let limit: Function = Reflect::get(&limiter, &JsValue::from_str("limit")).ok()?.dyn_into().ok()?;
        let promise = limit.call1(&limiter, &options).ok()?;
        let answer = worker::wasm_bindgen_futures::JsFuture::from(worker::js_sys::Promise::from(promise))
            .await
            .ok()?;
        Reflect::get(&answer, &JsValue::from_str("success")).ok()?.as_bool()
    };
    ask.await.unwrap_or(true)
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();
    let path = req.path();
    if path != "/v1/health" {
        let reading = matches!(req.method(), Method::Get | Method::Options);
        let (binding, class) = if reading { ("READ_LIMIT", "r") } else { ("WRITE_LIMIT", "w") };
        let who = req.headers().get("cf-connecting-ip").ok().flatten().unwrap_or_default();
        if !within_limit(&env, binding, &format!("{class}:{who}")).await {
            let mut resp = Response::error("too many requests, slow down", 429)?;
            resp.headers_mut().set("retry-after", "60")?;
            return Ok(resp);
        }
    }
    match (req.method(), path.as_str()) {
        (Method::Options, _) => crate::api::preflight(),
        (Method::Get, "/v1/health") => Response::ok("ok"),
        (Method::Get, "/v1/ratings") => crate::api::ratings(&env.d1("w2r_stats")?, &req.url()?).await,
        (Method::Get, "/v1/leaderboard") => {
            crate::api::leaderboard(&env.d1("w2r_stats")?, &req.url()?).await
        }
        (Method::Get, "/v1/games") => crate::api::recent_games(&env.d1("w2r_stats")?, &req.url()?).await,
        (Method::Post, "/v1/reports") => post_reports(req, env, &ctx).await,
        (Method::Put, "/v1/replays") => put_replay(req, env).await,
        _ => Response::error("not found", 404),
    }
}

/// The scheduled triggers (wrangler.toml): every few minutes group and judge new reports; once a
/// night rebuild every rating from the stored games.
#[event(scheduled)]
async fn scheduled(event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    console_error_panic_hook::set_once();
    let Ok(db) = env.d1("w2r_stats") else { return };
    let nightly = event.cron() == "0 3 * * *";
    match crate::jobs::scheduled(&db, now_secs(), nightly).await {
        Ok(line) => console_log!("scheduled: {line}"),
        Err(e) => console_error!("scheduled failed: {e}"),
    }
}

/// Body: `{"reports":[<envelope>, ...]}`. Answer: one verdict per envelope, in order. A report that
/// is already stored counts as accepted, so a client that lost the answer can safely resend.
async fn post_reports(mut req: Request, env: Env, ctx: &Context) -> Result<Response> {
    let bytes = req.bytes().await?;
    if bytes.len() > MAX_BODY {
        return Response::error("too large", 413);
    }
    let Ok(body) = w2r_formats::json::parse(&bytes) else {
        return Response::error("not json", 400);
    };
    let Some(list) = body.get("reports").and_then(|v| v.as_array()) else {
        return Response::error("missing reports", 400);
    };
    if list.len() > MAX_BATCH {
        return Response::error("too many reports", 413);
    }
    let db = env.d1("w2r_stats")?;
    let now = now_secs();
    let mut verdicts = Vec::with_capacity(list.len());
    for item in list {
        // The parser keeps key order and number text, so the envelope serialises back to the
        // bytes that were signed.
        let raw = item.to_json();
        verdicts.push(match ingest::check(raw.as_bytes(), now) {
            Ok(row) => {
                store(&db, &row).await?;
                r#"{"ok":true}"#.to_string()
            }
            // Reasons are fixed ASCII strings without quotes or backslashes.
            Err(why) => format!(r#"{{"ok":false,"why":"{}"}}"#, why.reason()),
        });
    }
    // Judge and rate what arrived without making the player wait: the same work the scheduled job
    // does, run after the answer is sent. The cron trigger stays as a backup and for the nightly
    // rebuild. Together with the inserts it stays under a free-plan invocation's 50 D1 queries
    // (20 reports at most, then about a dozen).
    if let Ok(db) = env.d1("w2r_stats") {
        ctx.wait_until(async move {
            if let Err(e) = crate::jobs::scheduled(&db, now_secs(), false).await {
                console_error!("after upload: {e}");
            }
        });
    }
    let mut resp = Response::ok(format!(r#"{{"results":[{}]}}"#, verdicts.join(",")))?;
    resp.headers_mut().set("content-type", "application/json")?;
    Ok(resp)
}

async fn store(db: &D1Database, r: &Row) -> Result<()> {
    let opt = |v: Option<u32>| v.map_or(JsValue::NULL, |n| JsValue::from_f64(f64::from(n)));
    db.prepare(
        "INSERT OR IGNORE INTO reports \
         (key, rid, phase, setup, started, received, map, me, me_tag, me_team, outcome, code, turns, \
          sync, map_sha, speed, flags, resources, tileset, peasants_only, fixed_start, envelope) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, \
                 ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
    )
    .bind(&[
        r.key.as_str().into(),
        r.rid.as_str().into(),
        r.phase.into(),
        r.setup.as_str().into(),
        JsValue::from_f64(r.started as f64),
        JsValue::from_f64(r.received as f64),
        r.map.as_str().into(),
        JsValue::from_f64(f64::from(r.me)),
        r.me_tag.as_str().into(),
        JsValue::from_f64(f64::from(r.me_team)),
        r.outcome.map_or(JsValue::NULL, JsValue::from),
        opt(r.code.map(u32::from)),
        opt(r.turns),
        opt(r.sync),
        r.map_sha.as_deref().map_or(JsValue::NULL, JsValue::from),
        opt(r.speed.map(u32::from)),
        opt(r.flags),
        r.resources.map_or(JsValue::NULL, JsValue::from),
        opt(r.tileset.map(u32::from)),
        opt(r.peasants_only.map(u32::from)),
        opt(r.fixed_start.map(u32::from)),
        r.envelope.as_str().into(),
    ])?
    .run()
    .await?;
    Ok(())
}

/// Most replays one key may add per day, a cap on how much a single install can store.
const REPLAYS_PER_DAY: u64 = 200;

fn json(status: u16, body: &str) -> Result<Response> {
    let mut resp = Response::ok(body.to_string())?.with_status(status);
    resp.headers_mut().set("content-type", "application/json")?;
    Ok(resp)
}

fn refusal(status: u16, why: &str) -> Result<Response> {
    // `why` is always one of our fixed ASCII phrases.
    json(status, &format!(r#"{{"ok":false,"why":"{why}"}}"#))
}

/// `PUT /v1/replays`: the raw `.w2rr` file, with `x-w2r-key` (the installation's public key, hex)
/// and `x-w2r-sig` (its signature over the file's SHA-256). Stored only when the same key has
/// already reported the game; until then the answer is 409 and the client tries again later.
async fn put_replay(mut req: Request, env: Env) -> Result<Response> {
    let header = |name: &str| req.headers().get(name).ok().flatten().unwrap_or_default();
    let key = w2r_stats::hex::decode_array::<32>(&header("x-w2r-key"));
    let sig = w2r_stats::hex::decode_array::<64>(&header("x-w2r-sig"));
    let (Some(key), Some(sig)) = (key, sig) else {
        return refusal(400, "missing or bad key or signature header");
    };
    let raw = req.bytes().await?;
    let now = now_secs();
    let row = match replay::check(&raw, &key, &sig, now) {
        Ok(row) => row,
        Err(why) => {
            let status = if why == replay::Reject::TooLarge { 413 } else { 422 };
            return refusal(status, why.reason());
        }
    };
    let db = env.d1("w2r_stats")?;

    // Already stored: the same file again is a success, so a client that lost the answer can resend.
    let have = db
        .prepare("SELECT 1 AS one FROM replays WHERE sha = ?1")
        .bind(&[row.sha.as_str().into()])?
        .first::<serde_json::Value>(None)
        .await?;
    if have.is_some() {
        return json(200, r#"{"ok":true,"stored":false}"#);
    }

    // The same key must have reported this game (same roster hash, start within ten minutes).
    let lo = row.started.saturating_sub(replay::LINK_WINDOW_SECS);
    let hi = row.started + replay::LINK_WINDOW_SECS;
    let linked = db
        .prepare("SELECT 1 AS one FROM reports WHERE key = ?1 AND setup = ?2 AND started BETWEEN ?3 AND ?4 LIMIT 1")
        .bind(&[
            row.key.as_str().into(),
            row.setup.as_str().into(),
            JsValue::from_f64(lo as f64),
            JsValue::from_f64(hi as f64),
        ])?
        .first::<serde_json::Value>(None)
        .await?;
    if linked.is_none() {
        return refusal(409, "no report for this game yet");
    }

    let today = db
        .prepare("SELECT COUNT(*) AS n FROM replays WHERE key = ?1 AND received > ?2")
        .bind(&[
            row.key.as_str().into(),
            JsValue::from_f64(now.saturating_sub(24 * 3600) as f64),
        ])?
        .first::<serde_json::Value>(None)
        .await?
        .and_then(|v| v.get("n").and_then(|n| n.as_u64()))
        .unwrap_or(0);
    if today >= REPLAYS_PER_DAY {
        return refusal(429, "too many replays today");
    }

    // The file goes to R2 under its hash first; the metadata row follows. If the row fails the
    // object is simply written again by the retry (same key, same bytes).
    env.bucket("REPLAYS")?.put(row.sha.clone(), raw.clone()).execute().await?;
    db.prepare(
        "INSERT OR IGNORE INTO replays          (sha, key, received, size, started, map, setup, build, client)          VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )
    .bind(&[
        row.sha.as_str().into(),
        row.key.as_str().into(),
        JsValue::from_f64(now as f64),
        JsValue::from_f64(row.size as f64),
        JsValue::from_f64(row.started as f64),
        row.map.as_str().into(),
        row.setup.as_str().into(),
        row.build.as_str().into(),
        row.client.as_str().into(),
    ])?
    .run()
    .await?;
    json(200, r#"{"ok":true,"stored":true}"#)
}
