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
