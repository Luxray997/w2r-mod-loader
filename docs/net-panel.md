# In-game connection table

A HUD table of every other person in the match: ping, jitter, loss and a short plain-language
reading of the link, so a connection is not judged by its ping alone.

- Toggle: the small signal-bars button in the top strip just right of the sidebar's Menu and Pause buttons (coloured by the worst link while the table is closed) (shown whenever another human is in the match), or Ctrl+Shift+N (game window focused). Setting "Show Connection Table In Game"
  (`net_panel` in `host.json`, default off) decides whether a match starts with it up.
- Source: the same figures as the lobby bars (`netquality.rs`, the game's own 4 s heartbeat per
  peer), so it works against players who do not run the mod. Polled every HUD frame during a match
  so the numbers are ready when the table opens; loss needs about 20 s of heartbeats.
- Reading (`netquality::assess`): ping sets the base rating; loss >= 3% / 10% and jitter >= 30 / 80 ms
  pull it down a step / two. Summary text names the dominant problem: "Smooth", "Steady, a little
  laggy" (ping > 100), "Steady but very laggy" (> 200), "Uneven: occasional spikes", "Very uneven: lag
  spikes", "Some packet loss", "Dropping packets: expect stutter", "Connection failing" (no packet 5 s).
- Code: `netpanel.rs`, hooked from `frame.rs` before `game_hud` like the APM panel.

## Not yet checked in game
- Panel position (top right, 60 px down) against the kick panel and chat.
- Net slot -> player mapping (`NetSlotPlayer`) and that peer records exist during a match.
- The jitter thresholds: jitter is measured at frame resolution (~16 ms), so 30 ms may be too touchy.
- The table is hidden when alone (single-player).
- The button is a window that takes input, so clicking it makes it the toolkit's active window; the
  APM panel avoids input for that reason (HUD command-button tooltips stop until the next click).
  Check whether tooltips are affected after clicking the button.
