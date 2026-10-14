# Observer research (2026-10-09)

Goal: a War2Combat-style observer for the Remastered build 2818. Sources: War2Mod-Server
`w2p.cpp` (classic game, `C:\Users\beau\Downloads\War2Mod-Server`) and Ghidra on `Warcraft II.exe`
(`D:\Tools\ghidra_scripts\obs.sh f:<addr> x:<addr>` decompiles / lists references).

## How War2Combat does it (classic)

An observer is a human slot whose lobby team is **9** (teams are 1-8). Everything else follows:
- lobby team dropdown has a tenth entry "OBS", refused for computer slots (line 11284);
- `obss()` (4926) each frame: sets the observer's bit in the vision mask and allies the observer with
  every player (`ally`, `viz_obs`), so the whole map is visible;
- a hidden, unplaced peasant is created for the observer so the game does not count them defeated
  (8660-8700); resources zeroed;
- while an observer has a unit selected, `LOCAL_PLAYER` is swapped to the unit's owner around the
  status / button draw calls, so the HUD shows that player's resources and commands (10216-10265);
- no resource transfers to or from observers; match result shows "OBS!" (9745); the report result
  is rewritten (9230).

## Remastered 2818 findings

- Lobby slot table `0x916260`, stride 0x26. Byte `+0x0a` is the team, stored **1-based** (0 = none).
  Offsets: `+0x08` slot kind, `+0x09` race.
- `HandleLobbyTeamChangeRequest` (0x497e60) stores `packet[2]` into the team byte with **no range
  check**, so a team 9 request is accepted by the host as is. It then calls 0x498090 (re-broadcast).
- Team labels: `MpLobbyTeamOptions` (0x95d3c8) is a vector of 8 strings `mp_lobby_team_1..8`
  (built in 0x4361b0). The native row reads `labels[team-1]`; team 9 would read past the end, so the
  native dropdown needs a 9th string. Our own lobby screen draws its own team column, so it can
  show "OBS" itself.
- Match start, 0x4d6a60 (non-load path): fills the alliance matrix `0x919578` (stride 0x10, indexed
  `[player*16+other]`, 1 = allied) from `lobbyTeam[a] == lobbyTeam[b]`, and the shared-vision mask
  `0x919678`. All observers on team 9 would therefore be allied with each other only. An observer
  plugin must set the observer's row (and the others' column) itself after this runs.
- Start-game / player counting: 0x4910c0 counts slots by kind and team byte; not yet read in full.

## Still unknown (needs more Ghidra)

1. Vision/fog: the per-player visibility structure, and what `0x919678` and `LOCAL_PLAYER`
   (0x918ccd) bit masks (0x918be9, 0x922f23) do each frame.
2. Whether the engine treats a player with no units as defeated (victory check) and the unit
   creation function to give an observer a hidden unit; or hook the result check instead.
3. Per-player resource storage, to show stats for other players.
4. Lockstep: does a team-9 slot with no commands stall the turn sync? (`ProcessNetworkTurn`,
   `TurnPlayerStatus`).
5. Camera: whether the viewport can move over fog-covered ground for the local player.

## Proposed order

1. Prototype `w2r-observer` plugin: after match start (`W2R_EVT_MATCH_START` fires before the game's
   own reset, so hook 0x4d6a60 or act on the next frame), ally the team-9 players with everyone
   and give them full vision. Test with a one-human-plus-AI game with the local slot on team 9
   (team byte edited in the lobby by the plugin).
2. Defeat handling, then HUD follow, then stats overlay and observer chat.

## Gate: only when every player has the mod (2026-10-09)

`observer.rs` allows team 9 only while `Handshake::everyone_has()` is true: in a lobby, our own
simulation fingerprint non-zero, and every other present net slot has announced the same
fingerprint (so the same simulation plugins, including `w2r-observer`). Recomputed each lobby frame:
- the OBS entry is in the team list only while allowed (list end pointer moves between 8 and 9);
- `HandleLobbyTeamChangeRequest` drops a request for team 9 while not allowed (all machines running
  the host; a stock host cannot be protected);
- the host puts anyone on team 9 back on team 1 and rebroadcasts the slot (`BroadcastLobbySlot`,
  0x498090) when a player without the plugin joins.
Not covered: the plugin itself does not re-check at match start, and a host without w2rpm accepts
team 9 unchecked.

## Stock-client behaviour and the synced-command route (2026-10-13, Ghidra on 2818)

War2Combat (classic) needs the mod on every machine too: its in-game `/obs <color>` sends a repurposed
cheat packet (bit 13) that every machine handles in lockstep (hidden unplaced peasant, zero resources,
team 9). Lobby `obs` is the same team-9 byte.

Stock client with a team-9 slot:
- Receiving: `FUN_00497eb0` (full lobby snapshot, non-host) copies the team byte from the packet with no
  range check; the single-slot update (type 0x1b, built by `BroadcastLobbySlot`) is assumed the same.
- Lobby row draw `FUN_00535cb0` reads `labels[team-1]` from the 8-entry array at `0x95d3c8`: team 9 reads
  past the end (junk text or a crash). The mod's ninth label is what avoids this.
- Match: alliance matrix is built from team equality on every machine (observer allied only with other
  observers); `CheckMatchEnd` (0x4f4930) runs per machine and defeats a player with no assets
  (`PlayerHasAssets` 0x4f4240 reads sim counters at 0x91b38c.., so deterministic). A stock observer is
  defeated at once.

Sim-consistent route (nothing local-only):
- Cheat flag word `DAT_0091b270`; applied by `FUN_004b3d20(newBits, playerName)`. The synced path is
  `FUN_0049c040` (reads 4 bytes at packet+1), called from the lockstep command dispatcher `FUN_004f16f0`
  (switch on packet+6; unlisted ids are ignored). Known bits: 0x20000 music, 0x4000/0x8000/0x10000 joke
  messages, 0x100 and sign bit read by CheckMatchEnd. A new bit (e.g. 0x400000) can carry "player N
  observes"; stock machines would store it and print "cheat enabled".
- Unit creation: `FUN_004edb10(x_px, y_px, type, owner)` (fails with message_exit_blocked); map spawn loop
  is `FUN_004d1d60` (8-byte records, sets `_DAT_0091ba4c = 1` around the call). Grid occupancy is written
  by `FUN_004b4910` (flags in `DAT_0091ad58`, pointers in `DAT_0091ad6c`) and `FUN_004b4a00`; unplace
  routine not found yet. Unit struct stride 0x98: +0x27 type, +0x2c owner, +0x1e flags, +0x18/+0x1a tile.
- Plan: hook 4edb10 while `_DAT_0091ba4c == 1`; for an observer owner create one peasant (counted as an
  asset on every machine) hidden/unplaced and skip the rest, replacing the local PlayerHasAssets and
  EliminatePlayer hooks. Still to find: unplace/hide, and what sets resources to 0.

### Unplace and resources (2818, same day)
- **Unplace: `FUN_004b5000(unit)`** (classic `F_UNIT_UNPLACE`). Clears the grid flag (0x100 land / 0x200 air, picked
  by unit+0x1c bit 4) in `DAT_0091ad58[tile]` and zeroes the unit pointer in `DAT_0091ad6c` / `DAT_0091ad70`
  when it still points at this unit. Inverse of `FUN_004b4a00`. Used in `FUN_004be570` (unit boards a
  transport) followed by: `unit+0x1e |= 8`, `unit+0x75 |= 0x10`, `unit+6 |= 0x20`, `FUN_004df750(unit)`
  (drops it from selection/hotkey groups, arrays at 0x933e18..0x933fc8), `FUN_004f1160(unit)` (releases
  queued orders). Boarding also links the container (`unit+0x88`), which an observer's peasant does not need.
- **Resources:** `PlayerGold` 0x919128, `PlayerLumber` 0x9190e8, `PlayerOil` 0x919168, uint32 per player
  (symbols already in symbols.rs). Zero the observer's three entries after the game sets starting resources.
- Unknown: whether match start sets starting resources before or after `FUN_004d1d60`; whether `+0x1e |= 8`
  alone is the right "hidden" marker for a unit with no container (check in game).

### Gate changed to a capability bit (2026-10-14)
`everyone_has` no longer compares simulation fingerprints. A w2rpm with the Observer plugin loaded
announces `CAP_OBSERVER` (handshake caps bit 3, set once the OBS team entry was added); OBS is offered
while every other present player announced it. Other plugins may differ. Consequence: nothing checks the
Observer plugin's version, so two machines with different builds of it (say with and without the spawn
skip) would play a different simulation and can desync.

### Observer HUD and camera follow (2026-10-14)
- `viewer.rs` (shared with replays) is switched on for a live observer by `replayplay::live_start`
  (called from `alliancesscreen::on_match_start` when the local player is a person on team 9).
  Keys `[ ] Home Insert backtick`, panel "OBSERVING" with game time, scoreboard without APM.
- Camera follow: players who run the mod send the centre of their view to the observers' net slots
  (`camerashare.rs`, subsystem 'V', 10-byte message, at most every 250 ms and only when moved, repeated
  every 2 s). The observer eases to the watched player's last centre (`replayplay::follow_live_camera`),
  semicolon (;) toggles. Not sent to anyone who is not an observer; nothing is sent when nobody observes.
- Stats screen: `StatsAddPlayer` 0x4d5c20 is skipped for observers and `StatsBuild` 0x4d56f0 runs with
  observers marked as empty seats in `PlayerOwnerAtStart` 0x918cbc (its "Ally/Enemy" words come from
  the first person in the slot list).

## Live APM, auto-director and observer chat

- **Live APM:** every machine gets every player's command packet each turn, so an observer counts
  orders per player with `apm::Tracker` (`liveapm.rs`, fed from the turn detour in `replayrec.rs`
  through `with_turn_packets`; pauses are skipped). The observer scoreboard's APM column reads it.
- **Auto-director** (`director.rs`, End toggles while observing): every second each player's score is
  `0.7*old + 0.3*(apm + 90*fighting events)`; fighting is kills + losses + razed + buildings lost from
  the score tables. The view moves to a new player only if they beat the one on screen by 1.4x + 30
  and the last move was 8+ s ago.
- **Observer chat:** `ChatSendLocal` (chatrec.rs) sends to `ChatRecipientMask & ~(1<<local)`. For an
  observer the mask is cut to the observers in the match (`observer::match_observers`), the text is
  prefixed `[Obs] `, and `LocalPlayer` is set to the real seat for the call (it is the watched player
  while viewing). With no other observer nothing is sent. On a playing machine `ChatReceive` is
  given a `ChatNetMask` without the observer's net slot, so observer lines never show to players.

## Observing mid-game (`/obs`)

A defeated player (their `PlayerMatchResult` entry is non-zero) types `/obs`. `chatrec.rs` catches it
before `ChatSendLocal`, `obsjoin::start_local` marks them an observer (`observer::set_local_observer`,
their bit in `observer::match_observers`), starts the live viewing system (`replayplay::live_start`) and
broadcasts one `'O'` message to the other mod peers; they add the sender's bit (checked against the
net slot's real player), so camera sharing now sends to the newcomer and observer chat mutes them. The
simulation is untouched. Limit: per-player fog is tracked only from when viewing starts, so what other
players explored earlier shows as unexplored until they walk it again.
