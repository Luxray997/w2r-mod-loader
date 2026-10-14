# Minimap pings

Setting (plugin screen, Host section; `config\host.json`, applies at once): **Minimap Pings (Alt+G, click)**,
field `minimap_ping`, on by default.

In a match: press **Alt+G**, then click the minimap or the map (Esc or a second Alt+G cancels, and it times out
after 6 s), or hold **Ctrl+Alt** and click. The click is swallowed (the game's four click-handler slots at
`0x91d1f4` are pointed at its own no-op while a ping is armed and until the button is released), so it neither
moves the camera nor gives an order. Two outlined circles in the pinger's colour expand and fade like a sonar for 4 s, on the minimap and, when the spot is on
screen, over the map, with a two-note sound (about 2 s, stereo). One live ping per player; the same player cannot ping twice within
0.4 s.

Who sees it: the pinger, their allies (`DiplomacyStance[sender*16+me] != 0`) and observers. Observers send
nothing. Single-player: you see your own ping, nothing is sent.

Wire (`crates/w2r-core/src/ping.rs`): one message of subsystem `'G'`, `E7 'W' '2' 'R' 'G' 01 x y` (tile, below 128),
sent with the game's `SendToNetSlot` straight to each teammate/observer that is in the match, is a person and
announced the handshake in the lobby (the host keeps that net-slot mask from the last lobby frame). Players without
the mod are never sent anything. The receiver takes the sender from the transport, maps it to a player with
`NetSlotPlayer`, drops tiles off the map, and the packet hook strips the message before the game sees it.

Geometry (Ghidra, build 2818; docs/ingame-ui-research.md): minimap picture rect `MinimapClickRect` (128 px square,
tile = pixel * side / 128), map view rect `ViewportRect`, view origin `ViewportOrigin`, scroll `CameraScrollX/Y`
(pixels, tile = pixel >> 5), mouse `MouseCursorPos`. The map rings are drawn by an overlay window opened just before
`game_hud`, like the APM panel; the minimap rings are drawn by a hook on `HudDrawImage` right after the game draws the
minimap texture (an overlay window would be covered by the picture). The click handlers are re-pointed at the no-op by a
hook on the routine that picks them (`MousePickHandlers`, 0x4c9090), because the game re-picks them on every mouse event.

Not verified in game yet; each ping logs `ping at tile x,y (map side, scroll, origin), sent to n` in the host log.
Things to check: that `MapSide` is the side in tiles (32/64/96/128), the click is really swallowed (including with
selected units), the ring lines up on both minimap and map, and that the game tolerates the message mid-match.

Sound (`blip_wav`): two sine notes, 522 and 799 Hz, shaped by `pingtable.rs`, the loudness of each note in each ear every
5 ms measured from a reference sound. Only the numbers are stored; the tones are generated.
