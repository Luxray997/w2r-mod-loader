# Front-end states and unused screens (research 2026-10-14)

Ghidra on `Warcraft II.exe` build 2818 (`D:\Tools\ghidra_scripts`: `UiCallers.java` lists every
screen-name string with the function that draws it and that function's callers; `StateSets.java`
lists the calls to `TransitionGameState` with the arguments pushed before them).

## The state machine

`FUN_00533020` is the front-end loop. Each pass it calls `FUN_004c4870` (current state, u16) and
switches on it; `default` ends the loop. The state is changed with `TransitionGameState`
(`0x4c48e0`, cdecl, one i32; `PreviousGameState` `0x4c48b0` returns the one before).

| State | Handler | Screen |
|---|---|---|
| 0 | `0x546380` | (not identified) |
| 4 | `0x5329f0` | after `0x4c48c0`; most screens go back here |
| 6, 7 | `0x548c20(1)`, `(0)` | not identified |
| 8 | `0x546290` | not identified |
| 9, 0xe | `0x52aa60` | finale: movie, then `finale_text_menu` |
| **0xb** | `0x5275d0` | **`codex_menu` (stub, see below)** |
| 0x10 | `0x5429a0(0)` | `options_menu` |
| 0x11 | `0x527270` | not identified |
| 0x12 | `0x5336b0` | not identified |
| 0x13 | `0x530a30(0)` | `load_menu` |
| 0x14 | `0x529300` | not identified |
| 0x15 | `0x53e290` | `multiplayer_main_screen` |
| 0x16 | `0x53bb50` | `multiplayer_lobby_list_screen` |
| 0x17 | `0x5362e0` | `multiplayer_lobby_screen` |
| 0x18 | `0x53a950` | not identified (sets 0x4 or 0x18 on exit) |
| 0x19 | `0x531b90` | main menu, with `login_prompt` |

States 1, 2, 3, 5, 0xa, 0xc, 0xd, 0xf have no case, so the loop ends. Several are set by
immediate pushes elsewhere (1, 2, 3, 5, 0xc are the in-game and campaign states); `0xb`, `0xa`,
`0xd`, `0xf` and `0x15` are never set by an immediate. `0x15` is reached through computed
transitions (a button-target lookup); nothing was found for `0xb`.

## `codex_menu` (state 0xb): an empty stub

Verified in game 2026-10-14 by forcing state `0xb`: an empty window with one **Previous** button
that returns to the main menu (state 4).

`FUN_005275d0` is the whole screen:

```
rect  = FUN_005bfd90(..., 1080.0, 630.0, 900.0, 810.0)        // x, y, w, h (read as floats, unverified order)
if FUN_005a9bf0(DAT_00965170, "codex_menu", rect, flags 0):     // BeginNamedWindow
    FUN_005aba00(DAT_00965170, 135.0, 1)                        // row layout, height 135
    if FUN_005453b0(DAT_00965170, Localized("common_previous")): // button
        TransitionGameState(4)
FUN_005aa7e0(DAT_00965170)                                      // end window
```

No list, no text, no image is drawn, and nothing in the game enters state `0xb`. It is leftover
from a planned in-game encyclopedia that never shipped.

### Why it is useful

It is a ready-made screen slot: a registered state, a handler, a back button and a working
return path. A plugin can draw its own content in that window with the same Nuklear kit the
create and lobby screens use (`screenkit.rs`), and enter it by calling `TransitionGameState(0xb)`.
Candidates: a plugin hub, a stats/ratings page, a replay browser, release notes.

To use it:
- **Draw:** hook `BeginNamedWindow` (`frame.rs`) and, for the name `codex_menu`, draw after the
  game's own Previous button, or return 0 after drawing a replacement (the lobby screen does this
  with `lobbyscreen::try_draw`).
- **Enter:** the game has no button. Options: hook the main menu window (state `0x19`, handler
  `0x531b90`) and draw one more button, or add a plugin-screen button that calls
  `TransitionGameState(0xb)` (the plugin screen already exists).
- **Size:** the stock window rect above is a starting point, not a measured layout. Check it
  against the main menu's rect before drawing.

## Other screens checked

- **`finale_text_menu`** (`0x52b470`): not hidden. `0x52aa60` plays
  `Movies\<race>_finale.webm` (index `0x95c798`), then sub-state `0x8c9e54` = 1 runs the text
  screen. Forced with `poke8 0x8c9e54 1` then `state 9`: it showed the ending credits. It is the
  campaign ending, reachable normally after finishing a campaign.
- **`login_prompt`**: not hidden. `FUN_00531b90` draws it on the main menu while the Battle.net
  connection flag `0x95cd8c` is 0, with the text `bnet_lost_connection`. Not forced in game.
- **`timeout_screen`** (`0x53e8e0`, called from the game loop at `0x4f8681`): the
  waiting-for-player screen; the natural place for an in-game lag display.
- **Other screens with a caller:** `alliances_menu`, `button_panel`, `cycle_overlay`,
  `default_options_screen`, `load_menu`, `option_display_screen`, `option_full_screen`,
  `options_menu`, `password_popup`, `pause_menu`, `save_menu`, `scrolling_menu`,
  `sound_options_screen`, `tips_menu`.

Only windows named with a `_screen`, `_menu`, `_popup`, `_panel`, `_overlay` suffix were
checked, plus `login_prompt`. Dialogs built from string-table keys or unnamed widgets are not
covered.

## How the states were probed

The states above were probed with a throwaway debug hook that is no longer in the tree. Every 30
frames it read a `debug_ui.txt` file in the host root, ran it and deleted it. Lines: `state <n>`
called `TransitionGameState`, `poke8 <static addr> <value>` wrote a byte. The file had to be plain
ASCII or UTF-8 (a UTF-16 file from PowerShell `echo` was ignored), so it was written with bash
`printf`. The hook did no checking of what it was told to do, which is why it was removed rather
than kept. To probe a state again, write a similar hook locally and do not commit it.
