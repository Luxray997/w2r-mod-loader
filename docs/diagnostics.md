# Diagnostics bundle

Status (2026-10-17): client (library, CLI, manager window) and server route are written and
unit-tested; the Worker has **not been redeployed** and the Send button has not been tried against
the live service. Run migration 0008 and deploy before it can work (see "Deploying").

A player who hits a bug presses **Settings > About > Send diagnostics...** in the manager (or runs
`w2r-loader diagnostics --send`). The manager builds a zip of the logs and settings, shows the file
list, and on **Send** uploads it. The service answers with a code like `W2R-7K3F-92QX`; the player
gives you that code. Nothing is sent without pressing Send.

## What is in the zip

Only this fixed list is ever read (`crates/w2r-loader-lib/src/diag.rs`):

| File | Notes |
|---|---|
| `summary.txt` | manager version, root, OS, time, plus the manager's own view (core, game folder, plugins) |
| `notes.txt` | what the player typed (up to 2000 characters), only if they typed something |
| `logs/w2r.log`, `logs/w2r.1.log` | first 300 (resp. 20) lines, then the last 3 MiB; the tail is cut to 1 MiB and then 256 KiB if the zip would pass 4 MiB |
| `logs/share.log` | upload log |
| `config/*.json` | host, state, sharing and per-plugin settings, ledgers; **never `identity.json`** (the signing secret) |
| `plugins/<id>/plugin.json` | manifests |
| `logs/*.dmp` | newest two, only if the player ticks "Include crash/hang dumps"; not scrubbed |

Never read: replays, plugin data folders (chat logs), `outbox\`, `identity.json`.

Lobby and network detail comes from `w2r.log` (the host logs lobby and connection events). The
in-game net panel (Ctrl+Shift+N) is not persisted, so its table is not in the bundle.

## Scrubbing

Applied to every text file, across files consistently (`Scrubber`):

- Windows user name, profile folder name, computer name -> `<user>`, `<pc>` (any case)
- BattleTags `Name#1234` -> `playerN`; the bare name is replaced too (3+ letters, not a common log
  word such as `host`), same `N` for the same person in every file
- IPv4 addresses -> `ipN` (same address, same `N`); `127.*` and `0.0.0.0` stay, version strings
  like `1.0.2.2818` are not addresses

Not caught: a player name that never appears with a tag anywhere in the bundle, IPv6 addresses,
and anything inside dumps. If this matters, tell players to tick nothing extra and read the file
list; **Save zip...** lets them inspect it first.

## Service side

`PUT /v1/diagnostics` (`server/worker/src/diagnostics.rs`, `app.rs`):

- body is the zip (max 4 MiB, must start with `PK\3\4`; the service never opens it)
- optional headers `x-w2r-key` and `x-w2r-sig`: the installation key's signature over
  `w2r-diag-1\n` + SHA-256 of the zip (`w2r_stats::blob::sign_diag`; its own domain, so a replay or
  report signature cannot be replayed here). Both or neither; a half or wrong pair is refused
- limits: 5 uploads per minute per address (`DIAG_LIMIT`), 5 per key per day, 100 in total per day
- stored in R2 (`w2r-replays` bucket) as `diagnostics/<sha256>.zip`, indexed in the D1 table
  `diagnostics (sha, code, key, received, size)`; the code is derived from the hash, so the same
  file sent twice gets the same code
- the nightly cron deletes bundles older than 30 days
- answer: `{"ok":true,"code":"W2R-XXXX-XXXX"}` or `{"ok":false,"why":"..."}` with 413, 422 or 429

## Reading a bundle (maintainer)

```bash
tools/fetch_diagnostics.sh W2R-7K3F-92QX   # into diagnostics-fetched/<code>/files (git-ignored)
tools/fetch_diagnostics.sh --list 20        # newest uploads, to find a code
```

It needs `wrangler` logged in (it runs from `server/worker`): one D1 lookup of the code, one R2
download, then `unzip`. The code must match the code alphabet, which also keeps it out of the
query as anything but a literal. Tested against a stub `wrangler`; not yet against the live service.

## Deploying

```bash
cd server/worker
npx wrangler d1 execute w2r-stats --remote --file migrations-0008-diagnostics.sql
npx wrangler deploy
```

`wrangler.toml` gained the `DIAG_LIMIT` rate-limit binding. A new database made from `schema.sql`
already has the table. Then ship a manager release (the button is in the manager, not the game).

## Open ideas

- Offer to send when the manager sees a new `logs\*.dmp` since the last launch.
- Persist the net panel's last table so a lag report can include it.
- Strip IPv6 and a configurable list of extra words.
