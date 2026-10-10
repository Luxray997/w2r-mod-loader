# Stats reporting, trust and ratings (design, 2026-10-09)

Every plugin user reports game results to an endpoint. The server turns agreeing reports into
verified games, then into stats and ratings. Ratings will later drive lobby team balance and
matchmaking, so a wrong verified game costs more than a missing one: **when in doubt, don't rate.**

Status (2026-10-09): the client side works end to end locally. `crates/w2r-stats` holds the pure
logic (slot/BattleTag parsing, schema v1, ed25519 identity, signed envelope, bounded outbox queue);
`matchend.rs` queues a signed `start` report on the first turn and a `result` report at the end or
on lobby re-entry after a leave. Reports land in `<host root>\outbox\`; the key is
`<host root>\config\identity.json`. Verified in game against the AI (win): both files verify with
`cargo run -p w2r-stats --example verify -- <files>`. Nothing is uploaded, and there is no consent
setting yet (add one with the uploader). The server does not exist.

## What the client can read (build 2818)

`PlayerMatchResult` 0x91aa84 (u8 x8: 0 playing, 2 defeated, 5 only allies left, 3 local fallback,
1 unknown), lobby slot table 0x916260 (stride 0x26, team at +0x0a 1-based, kind at +0x08),
`PlayerNames`, `PlayerOwner`, `NetTurnCounter` (turns played), `TurnSyncMismatch` (the game's own
lockstep desync flag; 0 at the end means every client ran the same simulation). No RNG seed found.
Peer messages are lobby-only, so keys are exchanged in the lobby, never in game.

## Identity

The game's slot table carries each human's **BattleTag** (`Name#12345`, slot offset +0x0d, confirmed
by the user 2026-10-09; in a match the `#` shows as a NUL byte, in the lobby as `#`). That is the
player identity: it is stable, unique and does not depend on a display name. The server still cannot
trust a client's claim about *someone else's* tag, so the key binding below stays: a key is bound to
the BattleTag it reports as its own (its slot, `local`), and a conflicting binding is flagged.

Reporter key: per-install ed25519 keypair, generated on first use, stored with the plugin settings. Every report is
signed. First report under a player name binds name to key on the server; a second key claiming the
same name is flagged, not trusted. Observers (team 9) and computer slots are never rated.

## Two-phase reports

1. **Start report** at match start: game id, roster (name, team, race, human/computer), map hash,
   options hash, plugin fingerprint, client version. Rejected unless the reporter is in the roster.
2. **Result report** at `MATCH_END`: game id, the reporter's own slot, the 8 results, turns,
   `sync_mismatch`, and whether the reporter saw the game to its natural end.

A result is accepted only for a game with a start report from the same key. Game id =
hash(map hash, options hash, sorted roster, start-time bucket). Collisions (same people, same map,
same bucket) are split by the start reports' own timestamps.

## Trust

Trust belongs to a key, is kept server-side, and is revocable. Levels:

| Level | Who | Effect |
|---|---|---|
| 0 unknown | any new key | counts as one weak vote |
| 1 established | earned: key age and N games whose reports matched consensus, no disputes | counts as a full vote |
| 2 trusted | allowlisted by maintainers (and optionally vouched for by trusted keys, depth-limited) | **alone verifies a game it took part in** |
| 3 admin | maintainers | can resolve disputes, revoke keys |

Rules:
- A key only attests to games it was a **participant** in (its name is in the start roster). A
  trusted player cannot verify a game they weren't in.
- Verification threshold: one level-2 vote, or two agreeing votes of level 1+ that include a player
  from each side of the result, or three agreeing level-0 votes from distinct, non-colluding keys.
  Votes against interest (a player reporting their own loss) count extra.
- A trusted key that is later found to have reported falsely is revoked. Because ratings are derived
  from the immutable report log (below), revocation re-evaluates every game it carried.
- Trusted keys are listed in a file the maintainers sign; the server refuses an unsigned change.
  The allowlist, and why each key is on it, is public.

Game states: `unverified` (shown in history only), `verified` (counts for stats and rating),
`disputed` (reports disagree: excluded, reporters flagged), `void` (revoked or admin-removed).

## Anti-abuse

Rate limits per key and per IP, plausibility checks (minimum duration in turns, results consistent
with teams), flag keys often in disputed games, discount repeated rating swings between the same
small group (win trading). Reporting is opt-in; the settings page shows exactly what is sent. Only
names and key hashes are stored, not IPs.

## Ratings (built for balance and matchmaking later)

- The report log is append-only. Ratings, stats and trust-derived verification are **recomputed
  from the log**, never edited in place, so revoking a key or fixing a rule replays history.
- Model: TrueSkill-style `mu`/`sigma` per player and per mode. It handles teams and free-for-all,
  and its win-probability / match-quality figure is what team balancing and matchmaking need.
  Modes: 1v1, team (2v2 and up), FFA; optionally per race. Pick the exact system once there is data;
  keep the log shape independent of it.
- Rated games are `verified` only, human players only, and the game must have run to its natural
  end (not a lobby-time abandonment). Draws, disconnects and surrenders need explicit result codes
  (still to be confirmed in game).
- New players get high `sigma`; the lobby should show provisional ratings as such.
- Later features read from the same store: lobby team balance (best split by summed ratings /
  predicted quality), then matchmaking (queue + match quality). Neither is in scope now.

## Build order

1. Spike: log the match-end snapshot in a real game and confirm result codes. (Written, untested in game.)
2. Report schema, key generation, signing, a local queue that survives a failed upload.
3. Server: ingest, quorum, trust tiers, SQLite, recompute job.
4. In-lobby `!stats` and rating display for plugin users; later balance and matchmaking.

## Open questions

- Result codes seen so far: local win 3, local loss 2 (the AI's own entry stays 0 after the local
  player loses), empty slots read 2. Surrender does not reach `CheckMatchEnd`; the lobby has wiped
  the tables by the time it is re-entered, so `matchend.rs` keeps the last state seen per frame
  (`note_frame`) and logs it on lobby entry. A surrender (5699 turns) and an early leave (637 turns)
  against the AI both left every human entry at 0 with the results screen saying Defeat. Whether
  leaving early against a *human* counts as a draw (the user's recollection) is untested: it needs
  a second human, and the draw may be decided server-side rather than in these tables. Still
  unknown: draw, disconnect, meaning of `== 1`.
- A stable game id without an RNG seed.
- Where the endpoint is hosted and who holds the allowlist signing key.
- How trust levels are earned vs. granted; whether players can see them.
