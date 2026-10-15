# Replay research (2026-10-09)

Goal: War2Combat-style replays for Remastered build 2818 (spec: `w2r-plugins/IDEAS.md`, "Replays").
Approach: record the start state plus the per-turn command packets, re-simulate on playback. Ghidra:
`D:\Tools\ghidra_scripts\obs.sh f:<addr>`.

## Why commands-only should work: it is already lockstep

`ProcessNetworkTurn` (0x49ea20), once per game turn:
1. `0x49d130(0, 8, &PktPtr[8], &PktLen[8], &PlayerStatus[8])` asks the net layer (vtable +0x38 on
   `DAT_0095bbd4+0x68`) for this turn's packets. Returns 1 when all are in; otherwise the turn waits.
   - `PktPtr` 0x9166b8 (u32 pointers), `PktLen` 0x9166d8 (u32), `PlayerStatus` 0x916220 (u32 per slot).
2. If `DAT_009166b0` (multiplayer flag) is set: for slot 7..0 with `PlayerStatus[slot] & 0x20000`,
   set `DAT_008bfb10 = slot`, `DAT_008bfb0c = DAT_009164e0[slot]` (player), then run
   `0x49e260(PktPtr[slot], PktLen[slot])`. Single player takes `0x49e7b0` instead.
3. `NetTurnCounter` (0x916210) ++.

So every game action reaches the simulation as bytes through one function, in slot order, once per
turn. That is both the recording tap and the playback injection point.

## Packet format (executor 0x49e260)

A stream of opcode-prefixed records, lengths fixed per opcode (consumes `param_2` as it goes):

| op | size | handler | op | size | handler |
|----|------|---------|----|------|---------|
| 05 | 1 | counts only (idle) | 0e | 5 | 0x49c040 |
| 06 | 5 + asciiz | 0x49c2a0 | 0f | 3 | 0x49c0f0 |
| 07 | 5 + asciiz | 0x49c160 | 10 | 7 | 0x49c260 |
| 08 | 2 + 2*n (n = byte 1) | 0x49c310 | 11 | 1 | 0x49bdc0 |
| 09 | 6 | 0x49c1e0 | 12 | 1 | 0x49c460 |
| 0a | 6 | 0x49c3b0 | 13 | 8 | 0x49be20 |
| 0b | 2 | 0x49c3f0 | 14 | 1 | 0x49c130 |
| 0c | 1 | 0x4d8c90 | 15 | 3 | 0x49bff0 |
| 0d | 1 | 0x4d8c20 | 16 | 1 | 0x49c430 |
| 30 | 2 | chat/pause style message (string table 0x6a+) | 17 | 3 | 0x49c0a0 |
| 18 | 4 | `0x4a1810`: sync check; failure sets `TurnSyncMismatch` | | | |

Handlers not decompiled yet; names above are by opcode only. Opcode 0x18 looks like a per-turn state
checksum: during playback it gives free desync detection.

## Open items (needs more Ghidra / in-game)

1. **RNG seed � FOUND (2026-10-10, recorded since; verify in game).** See "RNG" below. Original note: **RNG seed.** The simulation is only reproducible if the PRNG state at match start is recorded.
   Not found yet (also open in `stats-reporting.md`). Find the RNG function (callers in unit
   movement / damage), its state global, and where match start seeds it (0x4d6a60 region).
2. **Start state.** Map hash/path, slot table (0x916260, stride 0x26), options/scheme, game speed,
   `PlayerOwner` 0x918cac. `matchstart.rs` and the reporter already snapshot most of this.
3. **Other inputs not in the packets:** per-frame local-only input (camera, selection) is not
   simulation state, so it is not needed. Verify nothing sim-relevant bypasses 0x49e260
   (single-player path 0x49e7b0, AI per-turn commands, cheats).
4. **Playback driver.** Hook `0x49d130` to return recorded packets (fill the three arrays, status
   0x20000 per active slot) instead of asking the net layer; start a local game with the recorded
   slots/map/seed with no network. Needs: speed control (turn pacing timer), pause, and an
   observer-style full-vision local player (reuse `w2r-observer`).
5. Remastered also has its own replay/save facilities: check whether the game already serializes
   a start state (save-game path) that could give the seed and map for free.

## RNG (Ghidra, build 2818)

Three generators; only one is simulation state.
- **Sim RNG `DAT_00916b58`** (`SimRngState`): `s = s * 0x15a4e35 + 1`, result `(s >> 16) & 0x7fff`
  (`FUN_004a0200`; ranged variant `FUN_004a0230(lo, hi)`). ~60 call sites (combat, movement, AI,
  `FUN_004ab090`, `FUN_004bd770`, ...). `FUN_004a01d0` reads it.
- **Game seed `DAT_00916b5c`** (`GameSeed`): getter `FUN_004a01e0`, setter `FUN_004a01f0`. Match setup
  `FUN_004c4280` (the start-game function; it also calls `FUN_004d6a60`, the alliance setup) does, for
  network games: if `DAT_00922f5b == 0` (host) `GameSeed = time(0)`; then `FUN_004a0220(GameSeed)`
  copies it into `SimRngState`, and CRT `srand(GameSeed)` (`FUN_007f9550`). Clients get the seed in
  start packet **op 0x23** (handler `FUN_00497470`: bytes 1..4 = seed, then 8 slot/player bytes) or
  in join handler `FUN_004f70f0`.
- Cosmetic only: `DAT_008c8db4` LCG `* 0x41c64e6d + 0x3039` (`FUN_004f01b0`: scenario art, palette
  shuffle in `FUN_004f0880`) and the CRT `rand` (lobby slot order `FUN_004f86e0`, a blit routine).
  Neither affects the simulation.
- Several setup functions between the seeding and turn 0 draw from `SimRngState`
  (`FUN_004a0670`, `FUN_004a07e0`, ...), so playback must set `GameSeed` *before* calling the start
  function and let setup run unchanged. The recorder stores `GameSeed` and the RNG value before the
  first turn (header `seed`, 8 bytes) so playback can check it ends up in the same state.

## Match lifecycle (Ghidra, build 2818)

- `RunGame` **0x4c57f0** is the whole match: seeds timings, sets `MatchLoopActive` (0x91c6c5) = 1,
  resets `NetTurnCounter` (0x916210) to 0, waits for the first `ProcessNetworkTurn`, runs the
  match loop (`FUN_004c5220`, tick body `FUN_004c4e80`), tears down and **returns the next game
  state** (`DAT_0091c6c8`). Its return is the one signal that a match is over, whatever ended it.
- `FUN_004c4e40(state)` is the exit request: if the loop is active it clears `MatchLoopActive`,
  sets `DAT_0091c6c6` and stores the state to return; `TransitionGameState` (0x4c48e0) routes to it
  when the current state (`DAT_0091c184`) is 3.
- Surrender and quit never write `PlayerMatchResult` (seen 2026-10-09) but do return from `RunGame`,
  with the tables intact; the lobby wipes them only afterwards. So the host hooks `RunGame` and
  concludes there (stats `left` report, replay closing footer); lobby re-entry is a fallback.

## Recording (implemented 2026-10-09, not yet run in game)

- Crate `w2r-replay`: `.w2rr` format (`format.rs` documents the layout), streaming `Writer`, tolerant
  `Replay::parse` (a file without a footer is a cut-off recording and still reads), `packet.rs`
  opcode scanner, `format::file_name`.
- Host `w2r-core/src/replayrec.rs` hooks `ProcessNetworkTurn` (not the gather call `0x49d130`: a
  second caller, the timeout check at `0x49e060`, also gathers). After the game ran a turn (returns
  1) it copies each slot's packet (`TurnPacketPtrs 0x9166b8`, `TurnPacketLens 0x9166d8`, slots with
  `TurnPlayerStatus & 0x20000`) for turn `NetTurnCounter - 1`.
- Files: `<root>/replays/YYYYMMDD-HHMMSS-<map>.w2rr`, created at the first turn and valid at every
  moment (flushed every 5 s). The result footer is written the instant `CheckMatchEnd` reports it
  (`note_result`), so quitting, crashing or never reaching a lobby loses nothing; recording continues
  afterwards (a defeated player's team may still be playing) and the next lobby / match writes a
  final footer. The reader accepts turns after a footer and keeps the last. No footer = the match
  was left (surrender, quit) or cut off. Old `.part` files are renamed at launch.
  Nothing is pruned yet.
- Header carries slots/owner/names tables, local slot, map name, fingerprint; `seed` is empty until
  the RNG seed is found.
- Open: first turn number after `rearm` (is `NetTurnCounter` 0 at match start? logged as "first
  turn"); saved-game loads would record from mid-game; what the vs-AI path (`0x49e7b0`) packets look
  like; confirm `ProcessNetworkTurn` really returns 1 once per turn.

## Playback design (research 2026-10-10)

How a match starts: `FUN_004d0f90` (the in-game state handler) calls `StartGame` **0x4c4280**
`(a, mapBuf, mapLen)` -> `RunGame` 0x4c57f0 -> `FUN_004c4190`/`FUN_004d6480` cleanup. A custom game
vs AI passes `a = 0` and loads the map itself (`FUN_004d2670(DAT_009190e0, &DAT_009196d0, ...)`).
Inside `StartGame` the networked branch runs `FUN_0049c9f0`, which converts the **lobby block**
into every in-game table: player kinds (`0x918cac..`), races (`0x9191b0..`), per-player records
(`0x91ad90`, stride 0x38, name at +0x18), flags (`0x91af58` from `MpLobbyMatchFlags 0x916560`),
resources/speed bytes (`0x916550/555/556`), map name (`0x91657d`), and `0x91af53 = GameSeed` (low
byte, used by the cosmetic palette shuffle). So **the lobby block is the complete start state**:
`0x916260..0x9166a0` (slot table 0x26 x 8, game name 0x916534, speed, flags, map text, fixed start
`0x91668a`, `0x916686`, ...). Our recording currently stores only the slot table, the owner table
and the names; it must store the whole block, captured at `StartGame` entry (before `0x49c9f0`
normalises it: it rewrites empty slots to kind 8 and ORs bits into the match flags).

Playback as "a normal game with overrides" (no lobby/network emulation needed). The user starts any
custom game vs AI on the same map (as War2BNE InSight made you do), with replay armed; the host:
1. **StartGame entry:** copy the recorded lobby block over the live one (restore the live one when
   `RunGame` returns). Refuse (log, play a normal game) if the live map text differs from the header.
2. **Seed:** hook `SetGameSeed` (0x4a01f0, called by `StartGame` after `time(0)`) to substitute the
   recorded seed. `0x4a0220` then seeds the sim RNG from it. Cross-check: the sim RNG before turn 0
   must equal the header's `rng_at_start`; a mismatch means something else differs.
3. **Packets:** wrap `0x49d130` (turn gather). Call the original (the loopback net layer keeps
   pacing, the turn queue and `TurnPlayerStatus`), and when it returns 1 overwrite
   `TurnPacketPtrs/Lens/Status` for the slots with recorded packets (set `0x20000`; clear it for
   slots with none). The live player's own packet is thereby replaced by the recorded one. Past
   the last recorded turn: stop injecting and end the replay.
4. **Check:** compare the recorded `0x18` sync records against the live game's own (`0x4a1810`
   verifies them; a mismatch sets `TurnSyncMismatch`), and the final result/turn count against the
   footer.

Later: speed control (return 0 from the gather to wait, or call it more often), pause, jump (needs
snapshots or re-simulation), full-vision / follow-player view by reusing `w2r-observer`'s team 9
approach, a replay browser (CLI `w2rpm replays`, then an in-game screen next to the create screen),
bundling or fetching the map (the host already has peer map download) and a map hash in the header.

Comparison with War2BNE InSight (its documented limits):
| InSight | Here |
|---|---|
| cannot record games vs computers | recorded (AI is simulated locally from the seed; playback proves it) |
| Battle.net games only | any game that runs `ProcessNetworkTurn` (verified: custom vs AI) |
| cannot start recording mid-game | same: needs turn 0 |
| map not stored | same at first; map hash + download later |
| chat not recorded | ops 06/07 carry text; to be checked in a game with several humans |
| replay needs a private game plus placeholder computers | any custom game on the map; slots are overwritten |
| cannot watch with other players | possible later: each client plays the same file |

Risks / unknowns: `StartGame` calling convention and arguments for custom games (read from the
disassembly before hooking); whether anything outside the lobby block feeds the sim (AI strength,
cheats, campaign flags); the loopback net layer's turn pacing when its packets are replaced; the
saved-game path (`a != 0`) is not covered; `DAT_00922f5b` semantics (host/client flag) were
contradictory in the decompile and need a look at the disassembly.

## Staged plan

1. **Record (no game changes):** hook `0x49e260` (or tap after `0x49d130`), append
   `(turn, slot, bytes)` into an in-memory log; write `.w2rr` at match end next to `matchend`
   (header: build, plugin versions, map hash, slots, options, seed, result, turns). New crate
   `w2r-replay` with the file format + reader + tests (format is host-independent).
2. **Verify determinism offline:** compare the recorded 0x18 checksums across two clients for the
   same game: confirms the stream is complete.
3. **Playback:** 0x49d130 injection, local-only start, controls.
4. **Browser + `w2rpm replays` CLI/GUI listing, then sharing.**

## Follow camera (2026-10-13)

The camera is not in the turn packets, so the recorder writes it itself: record `0x04` (turn,
centre x, centre y in map pixels), only when it moved. Scroll = `CameraScrollX/Y` 0x91ad80/84
(i32 pixels, top-left), view size = `ViewportWidth/Height` 0x91cb08/0c (u16), clamp = `ClampCamera`
0x4afd00. Playback (`replayplay::follow_camera`, once per frame) eases towards the sample in force
at `NetTurnCounter`, only while the recorded player is the one watched; `\` toggles it. Pure part:
`w2r-replay/src/camera.rs`. Hint rows on the HUD panel draw the key in yellow.

## Combat events (2026-10-14)

For the timeline, jump-to-battle and heat map the recorder needs where and when fights happened,
which the order packets do not say. Ghidra, build 2818:

- Every kill is credited by **`KillCredit` 0x4def00**, cdecl `(u8 killer_player, unit* victim)`,
  called from exactly one place (the lethal-damage code `FUN_004bd8f0`, after `hp <= damage`). It does
  nothing when killer == victim owner, adds 1 to `ScoreUnitsKilled` (victim type < 0x3a) or
  `ScoreBuildingsRazed`, and adds the victim type's point value (UDTA table 0x9184a0) to the
  killer's score at 0x9193a8.
- Lost counters are bumped separately by per-type callbacks (tables at 0x8c0ef0..0x8c107c; code
  0x4b5090/50b0/50e0/5120/5160), so `KillCredit` is the better hook: one place, has the killer.
- Unit struct (stride 0x98): map pixels x, y as two i16 at +0 and +2 (32 per tile), type id +0x27,
  owner +0x2c, flags +0x1e.

Record `0x05` (turn, kind 0 unit / 1 building, killer, victim, unit type, tile x, tile y) is written
by `combatrec.rs` (hook, reads the victim before running the original) through
`replayrec::note_event`. It is never replayed: the sim kills the same units again by itself.
`w2r-replay/src/heat.rs` holds the pure part: `kill_event`, `grid` / `grid_between` (+ `blurred`,
`bytes`) for the heat map, `intensity` for the timeline, `battles` (kills close in time and place
joined into fights) and `jump` for next/previous battle. `inspect` prints event count, intensity and
battles. Old recordings have no events. Not yet run in game.

Timeline and battle jump (written 2026-10-14, first test pending): the HUD panel draws a bar graph
of kill weight across the recording (`heat::graph`, 70 bars, played part warm, playhead line) above
the progress bar when the recording has events. `.` jumps forward to the next battle
(`heat::detect`: gap 150 turns, radius 14 tiles, min weight 8): playback fast-forwards (the tick
deadline is stretched as for the speed keys, `SEEK_SPEED` 200, ignoring pause) to 100 turns before
the fight starts, then plays on at normal speed, turns the camera follow off and eases the view onto
the fight, saying "Battle k of n". Pressing `.` again while jumping heads for the following battle.
`,` only explains that a replay cannot rewind: restarting at a chosen turn would need a way to start
the replay game again from code. How fast a jump really runs depends on the frame loop (one turn
per frame, drawing included?) and is unmeasured.

Playback constraint: there are no savestates, so seeking forward is fast-forward with drawing off
and seeking back is restart plus fast-forward.

## Roadmap

- **Ghost cursor (not started):** record the recorded player's mouse position (and cursor frame:
  arrow / move / attack) like the camera, and draw a ghost of it during playback. Reuse the game's
  own cursor sprite at runtime by calling its cursor draw routine at the recorded position (nothing
  is copied or shipped); fall back to a tinted shape if it cannot be called twice per frame. Needs
  Ghidra first: the mouse x/y globals and the cursor draw routine. Optionally record clicks and
  flash a ring where an order was given. Store map pixels over the map, screen pixels over the HUD.
