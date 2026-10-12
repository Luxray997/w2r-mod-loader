//! HTTP routes and D1 access. Kept thin: all decisions are in `ingest`.

use worker::wasm_bindgen::JsValue;
use worker::*;

use crate::ingest::{self, Row};

/// Most envelopes in one upload. The client sends its outbox in batches.
const MAX_BATCH: usize = 20;
/// Largest request body: 20 envelopes of at most 64 KiB would be far more than real reports.
const MAX_BODY: usize = 512 * 1024;

fn now_secs() -> u64 {
    (Date::now().as_millis() / 1000) as u64
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();
    let path = req.path();
    match (req.method(), path.as_str()) {
        (Method::Get, "/v1/health") => Response::ok("ok"),
        (Method::Post, "/v1/reports") => post_reports(req, env).await,
        _ => Response::error("not found", 404),
    }
}

/// Body: `{"reports":[<envelope>, ...]}`. Answer: one verdict per envelope, in order. A report that
/// is already stored counts as accepted, so a client that lost the answer can safely resend.
async fn post_reports(mut req: Request, env: Env) -> Result<Response> {
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
    let mut resp = Response::ok(format!(r#"{{"results":[{}]}}"#, verdicts.join(",")))?;
    resp.headers_mut().set("content-type", "application/json")?;
    Ok(resp)
}

async fn store(db: &D1Database, r: &Row) -> Result<()> {
    let opt = |v: Option<u32>| v.map_or(JsValue::NULL, |n| JsValue::from_f64(f64::from(n)));
    db.prepare(
        "INSERT OR IGNORE INTO reports \
         (key, rid, phase, setup, started, received, map, me, me_tag, outcome, code, turns, sync, \
          map_sha, speed, flags, resources, tileset, peasants_only, fixed_start, envelope) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
                 ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
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
