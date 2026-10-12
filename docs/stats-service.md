# Stats service (Cloudflare Worker, Rust to WASM)

Status (2026-10-11): ingest and the client uploader are written and unit-tested; the Worker has
**not been deployed** and nothing has run against a real endpoint. Ratings, corroboration and the
read API are not written. Design background: `docs/stats-reporting.md`.

## Pieces

| Where | What |
|---|---|
| `server/worker/` | The Worker, its own cargo workspace (never built by the root `cargo test`). `src/ingest.rs` is pure policy and is tested natively; `src/app.rs` is the HTTP/D1 glue and only builds for `wasm32-unknown-unknown`. It links `w2r-stats`, so the server verifies envelopes with the exact code that signs them. |
| `server/worker/schema.sql` | D1 schema. `reports` is the append-only log; everything else is to be derived. |
| `crates/w2r-stats/src/share.rs` | The uploader logic, shared by host and manager: `config\sharing.json` (off by default), a ledger in `config\upload-state.json`, batches of 20 to `POST {url}/v1/reports`. `DEFAULT_URL` (empty until deployed) is used when `sharing.json` names no URL. |
| `crates/w2r-core/src/uploader.rs` | **Automatic upload from the game.** WinHTTP transport (system TLS and proxy, nothing linked in). A `w2r-share` thread runs after each result report is queued, 20 s after startup for leftovers, and when the setting is turned on. One line per upload goes to `logs\share.log`. |
| Plugin screen checkbox | "Share my match results ..." (off by default) writes `sharing.json`. |
| `w2rpm share status/on/off/url/upload` | Manager CLI for the same file, plus a manual upload. |

## Ingest rules (server and client agree)

A report is stored only if its signature verifies, its roster hashes to its `setup`, the reporter
is a human in the roster, **every human has a BattleTag**, and its start time is within 14 days
back and 1 hour ahead. Computers need no tag. Rejections are permanent; the client records them
and does not retry. The client also never sends a game with an untagged human. Re-sending a stored
report is accepted (`INSERT OR IGNORE` on `(key, rid)`).

## What a report says about the match

Besides the roster and the reporter's result, a report carries a `game` object read from the
game's lobby block at match start (`crates/w2r-stats/src/game.rs`): `map_sha` (SHA-256 of the map
file, so two different maps called `Battle_2.pud` can be told apart), `speed` and `flags` exactly
as the game stores them, and their decoded view: `resources` (default, random, low, medium, high),
`tileset` (index into the game's list), `peasants_only`, `fixed_start`. Undecoded flag bits stay
in `flags`, and the envelope is stored verbatim, so anything learned later can be recomputed. The
decoder was checked against the lobby blocks of 20 saved replays (speeds 6 to 8, high and default
resources, peasants-only on and off, tileset 4, fixed and random starts). The D1 `reports` table
has a column for each; `migrations-0002-game-settings.sql` adds them to a database made from the
first schema. Reports from clients without `game` are still accepted, with empty columns.

Not captured yet: AI difficulty, game-end cause, per-player tallies over time. Replays (median
40 KB, largest 780 KB, with public chat) could be uploaded as a second, separate artifact to R2
if a full record is wanted; not built.

## Local testing (done 2026-10-11)

Node 24 LTS is unpacked at `E:\tools\node` (not on PATH; C: is full, so also set
`npm_config_cache=E:\tools\npm-cache`). `wrangler` is a dev dependency of `server/worker`;
`worker-build` is in the cargo bin folder. From `server/worker`: `worker-build --release`,
`npx wrangler d1 execute w2r-stats --local --file schema.sql`, `npx wrangler dev --local`. Posting six
real outbox reports to it stored all six, rejected a tampered copy ("bad signature") and stored
nothing new on a resend.

## Deploying (not done)

1. Node, `wrangler` and `worker-build` are installed here; `wrangler login` needs your Cloudflare account.
2. `wrangler d1 create w2r-stats`, put the id in `server/worker/wrangler.toml`.
3. `wrangler d1 execute w2r-stats --file schema.sql --remote`, then `wrangler deploy`.
4. Client: `w2rpm share url https://<name>.<account>.workers.dev`, `w2rpm share on`.

## Still to build

Set `DEFAULT_URL` once the Worker is deployed (until then nothing is sent, even with the checkbox
on). A sharing toggle in the egui manager. Rate limiting (Workers rate-limit binding, per key and
IP), a cron pass that groups reports into games and applies the trust tiers, rating recompute with
`w2r-stats::rating`, `GET /v1/ratings`. The in-game WinHTTP upload has not been run in the game.
