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

The result block also carries `score`: for every player in the game, the game's own score tables
(`ScoreUnitsKilled`, `ScoreBuildingsRazed`, `ScoreUnitsLost`, `ScoreBuildingsLost`, and gold,
lumber and oil mined), read at match end (`matchend.rs`; the same counters the replay panel shows).
Every client sees the same simulation, so every reporter should send the same numbers. It is absent
from reports made before 2026-10-12 and from a match recovered after a crash.

Not captured yet: AI difficulty, per-player tallies over time (derivable from replays).

## Games, ratings and the read API

Uploads store reports and then, after the answer is sent, run the judging job in the background
(`ctx.wait_until`; `server/worker/src/jobs.rs`), because a free-plan invocation may run at most 50
D1 queries, so the job works in a few batched queries. The same job runs from a cron trigger every
5 minutes as a backup, and a nightly one (03:00 UTC) rebuilds every rating. (On 2026-10-12 the cron
triggers had not fired in the first 20 minutes after deploying; the post-upload run does not
depend on them.)

- **Binding:** a key speaks for the first BattleTag it reported under (`bindings`). Reports under
  any other tag are kept but `ignored`.
- **Grouping:** reports with the same roster hash that began within 300 s of each other are one
  `games` row (`games::plan`). The same people playing again an hour later is a new game.
- **Judging** (`games::judge`, all in one place, with tests). The policy is **lenient while few
  players run the mod**: most opponents will not be reporting, so one reporter's account of a game
  is enough to rate it, and nobody is assumed to be cheating yet. A vote is a result report from a
  reporter who is in the roster under that tag and team, with no desync flag and at least 600 turns
  (`MIN_TURNS`). `win` means that team won, `loss` means the other one did, and `left` counts as a
  loss only when the leaver was the last human on their side **and the score tables show a fight**.
  A game where nobody killed a unit or razed a building (all `kills` and `razed` zero) decides
  nothing, whoever won or left. A report with no score at all (an older client) can still carry a
  win or a loss, but a leave from one is not a result. Then:
  - **verified** (rated): the votes name one winner. If one reporter from each side agrees the game
    is also **corroborated**; otherwise the reason reads "single report, accepted on trust";
  - **trust:** a key's weight is 1 plus the number of corroborated games it has taken part in (an
    allowlisted key in `trusted_keys` outweighs any record). Only corroborated games earn it;
  - **dispute:** when reports name different winners, the side whose reporters weigh at least 3
    times the other's (`TRUST_RATIO`) wins ("settled by the reporters' track record", not itself
    corroborated); otherwise the game is **disputed** and stays out of the ratings;
  - **unverified**: no usable result (too short, left, out of sync), or not two sides.
- **Rated** means verified, two sides, no computers. Observers are never rated. A single report
  that is later contradicted by the other side turns the game into a dispute (or settles it by
  track record), and the next rebuild takes the rating back.
- **Ratings:** TrueSkill (`w2r_stats::rating`), applied as games become verified. A game that is
  older than one already rated, or a verdict that later reverses, sets `meta.rebuild`; the job then
  recomputes every rating from the games in time order (at most once an hour, and every night). A
  rebuild reads up to 3000 games, a limit of this design.
  In a team game each player's share of the result is weighted by what they did
  (`rating::contribution_weights`, `MIN_WEIGHT` 0.5): fighting (units killed plus buildings razed,
  from the game's score table) counts 70 %, orders given (from the replay packets) 30 %, each as the
  player's share of their team's average, capped at 1. Economy is not counted, so a player who only
  gathered while a teammate fought earns less of the win (and loses less of a loss). A winner who
  left early also earns only the share of the game they stayed for (`presence`: their last packet
  turn over the last anyone's arrived); a loser who left (presence under 0.9) takes the whole loss,
  so quitting never softens it. No score or activity table, or no activity on a team: equal.
  The result report carries both tables (`result.score`, `result.activity`); activity is
  `{slot, last_seen, last_action, actions}` per human, kept by the replay recorder
  (`w2r_replay::activity`; packet ids are the humans in slot order, not lobby slots) and only
  present when the match was recorded.
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

Migrations `0005` (columns and tables), `0006` (the `corroborated` flag) and `0007` (`combat` on
reports, `score` on games) ask for a rebuild, which
also groups the reports already stored. The manager has a sharing switch on its Settings tab (same file as the
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

A website for the leaderboard (the API is ready). (The lobby already shows ratings and the host has
a Balance button.) Per-mode ratings (team games, free-for-all), draws, and a resumable rebuild for more
than a few thousand games. Keeping only one or two replays per game. The in-game replay upload has
not been seen from the game itself yet.
