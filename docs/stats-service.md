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
is a human in the roster, **the game has human opponents** (at least two humans on different,
non-observer teams: games against the computer alone, or humans only allied, are refused),
**every human has a BattleTag**, and its start time is within 14 days back and 1 hour ahead.
Computers need no tag. Games against the computer stay in the local outbox and in `w2rpm stats`. Rejections are permanent; the client records them
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

## Replays

A finished replay (`replays\*.w2rr`, median 40 KB, largest about 780 KB, **with public chat**) is
sent too, untouched, as `PUT /v1/replays` with two headers: `x-w2r-key` (the installation's public
key) and `x-w2r-sig` (its signature over `w2r-replay-1
` + the file's SHA-256, `w2r_stats::blob`).
The server verifies the signature, reads only the header (`Replay::parse_header`), and stores the
file in the R2 bucket `w2r-replays` under its SHA-256 (limit 1.5 MB); the `replays` table in D1
holds only the metadata (key, map, start time, roster hash, size). Replays were first kept as D1
BLOBs and moved to R2 on 2026-10-12 (migration 0004), so they do not count against the 5 GB of D1.

- **Linking:** the roster hash computed from the replay's header equals `reports.setup` of the
  same game, and the start times agree (checked on seven games: identical to the second). A replay
  is stored only when the *same key* has reported that game; until then the answer is 409 and the
  client tries again on its next run. This keeps strangers from filling the table.
- **Client** (`w2r_replay::upload`, used by the host and `w2rpm share upload`): one ledger of its
  own (`configeplay-upload-state.json`), at most 10 files per run, nothing touched in the last 20
  seconds, nothing while a recording is open (`replayrec::recording`), and `replayrec::end` asks for
  an upload when it closes a file. Skipped for good: no settings block (older recordings), no human
  opponents, a human without a BattleTag, over 1.5 MB, older than 14 days.
- **Answers:** 200 stored (or already stored), 409/429/5xx try later, anything else final.
- **Cap:** 200 replays per key per day.
- **Consent:** the same checkbox, worded "Share my match results and replays (including chat)...".
  Chat is kept for now; whether to strip it is to be decided after seeing how players react.
- **CPU:** the Worker hashes and verifies a signature per upload (a few ms for a large file in WASM);
  untested against the free plan's 10 ms CPU limit.

Not captured yet: AI difficulty, per-player tallies over time (derivable from replays).

## Games, ratings and the read API

Uploads only store reports. A scheduled job (`server/worker/src/jobs.rs`, every 5 minutes, plus a
nightly rebuild at 03:00 UTC) does the rest, because a free-plan invocation may run at most 50 D1
queries. So a game is judged up to five minutes after its last report arrives.

- **Binding:** a key speaks for the first BattleTag it reported under (`bindings`). Reports under
  any other tag are kept but `ignored`.
- **Grouping:** reports with the same roster hash that began within 300 s of each other are one
  `games` row (`games::plan`). The same people playing again an hour later is a new game.
- **Judging** (`games::judge`, all in one place, with tests): a vote is a result report from a
  reporter who is in the roster under that tag and team, with no desync flag and at least 600 turns
  (`MIN_TURNS`). `win` means that team won, `loss` means the other one did, and `left` counts as a
  loss only when the leaver was the last human on their side. Then:
  - **verified**: both sides back the same winner (one reporter from each), or a key in
    `trusted_keys` is among the reporters;
  - **disputed**: reports name different winners (kept out of the ratings);
  - **unverified**: anything else, mostly "only one side has reported" or three-way games.
- **Rated** means verified, two sides, no computers. Observers are never rated.
- **Ratings:** TrueSkill (`w2r_stats::rating`), applied as games become verified. A game that is
  older than one already rated, or a verdict that later reverses, sets `meta.rebuild`; the job then
  recomputes every rating from the games in time order (at most once an hour, and every night). A
  rebuild reads up to 3000 games, a limit of this design.
- **Read API** (public, JSON, CORS open, cached 60 s): `GET /v1/ratings?tags=Name%231234,...` (up to
  16 tags; unknown tags are absent), `GET /v1/leaderboard?limit=50&min_games=1`, and
  `GET /v1/games?limit=20` (latest verified games with settings and what each rating did).
- **Rate limits** (Workers rate-limit bindings, per address, per minute): 30 writes
  (`WRITE_LIMIT`) and 120 reads (`READ_LIMIT`); `/v1/health` is exempt.
- **Trusted keys:** insert a hex key into `trusted_keys` with `wrangler d1 execute`. It then
  verifies games it played in on its own.
- **Not solved:** a key can claim any BattleTag until that tag's real owner reports (a key binds
  once). With a handful of known players that is acceptable; the trusted list and `ignored`
  are the tools if it is abused.

Migration `0005` adds the columns and tables and asks for a first rebuild, which also groups the
reports already stored. The manager has a sharing switch on its Settings tab (same file as the
in-game checkbox).

## Local testing (done 2026-10-11)

Node 24 LTS is unpacked at `E:\tools\node` (not on PATH; C: is full, so also set
`npm_config_cache=E:\tools\npm-cache`). `wrangler` is a dev dependency of `server/worker`;
`worker-build` is in the cargo bin folder. From `server/worker`: `worker-build --release`,
`npx wrangler d1 execute w2r-stats --local --file schema.sql`, `npx wrangler dev --local`. Posting six
real outbox reports to it stored all six, rejected a tampered copy ("bad signature") and stored
nothing new on a resend.

## Deploying

Deployed 2026-10-11 to `https://w2r-stats.saweet.net` (custom domain route in `wrangler.toml`,
`workers_dev = false`; the D1 binding is `w2r_stats`). Node is at `E:\tools\node`; `wrangler` is a
dev dependency and `worker-build` is installed through cargo. To change the service, from
`server/worker`:

1. Apply any new `migrations-*.sql` to the live database first (`npx wrangler d1 execute w2r-stats
   --remote --file <file>`), because the new Worker expects the columns and tables. Test the same
   file against a local copy first (`--local`, then `npx wrangler dev --local`).
2. `npx wrangler deploy` (it runs `worker-build --release`).
3. Check `https://w2r-stats.saweet.net/v1/health`.

Migrations so far: `0002` (game-settings columns), `0003` (the replays table) and `0004` (replays table without the file column, files in R2; it drops the old
table, so clients send their replays again: delete `configeplay-upload-state.json`, then run
`w2rpm share upload`). A database made fresh from `schema.sql` needs none of them, only the
`w2r-replays` bucket (`npx wrangler r2 bucket create w2r-replays`).

## Still to build

A website for the leaderboard (the API is ready). The lobby showing ratings and balancing teams
from them. Per-mode ratings (team games, free-for-all), draws, and a resumable rebuild for more
than a few thousand games. Keeping only one or two replays per game. The in-game replay upload has
not been seen from the game itself yet.
