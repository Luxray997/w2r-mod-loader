# In-game APM counter

Settings (plugin screen, "APM Counter" group at the end of the Host section; stored in `config\host.json`; every change applies at once):

| Setting | Field | Default |
|---|---|---|
| Display APM In Game | `apm_display` | off |
| Alert when APM falls below [ ] | `apm_alert_below` (0-999, 0 = no alert) | 60 |
| Color APM Text Red When Low (APM turns red while below the threshold) | `apm_color_text` | on |
| Play Sound When APM Is Low (a two-note chirp while below the threshold) | `apm_play_sound` | off |

How it counts (`crates/w2r-core/src/apm.rs`): the game's `QueueCommand` (0x49ec10) is hooked; every UI
order, selection or build the local player queues is one action (records 0x06-0x17; the idle record 05, the
sync record 18 and the lobby records 19+ are not). It works in multiplayer matches (verified in game); in single-player it stays at 0, which is accepted (not investigated).
Turns close the count: `ProcessNetworkTurn` (networked) and `ProcessLocalMatchTick` (single-player). The
figure covers the last 20 s of play; time only advances while turns run and the game is not paused (gaps over 0.5 s count as 0.5 s), so a
pause does not lower it. Nothing is shown for the first 5 s. The alert waits for the first minute, repeats at
most every 30 s, and never fires for a player who has not given a single order. The display is a small panel
at the bottom right of the HUD; it is not drawn while a replay plays (the replay scoreboard has its own APM
column). The log line `apm: first command of the match: record 0x..` marks the first counted command.
