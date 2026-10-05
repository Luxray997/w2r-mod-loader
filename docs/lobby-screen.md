# Multiplayer lobby screen (Phase 3, after map download)

Status: spec, not implemented. Port of wc2r-plus `features/ui/multiplayer_lobby_screen.cpp` (997
lines) and `features/ui/lobby_chat.cpp` (265), plus the small `crown_image` and `team_colours`
tables. Same parity rule as [chat-plugin.md](chat-plugin.md): the C++ is the reference where this
document is silent or differs. A host feature (it replaces a game screen and reads state owned by
the handshake, lobby policy and map download), like the
[create screen](create-screen.md) it sits beside.

## Why

Three features already written and waiting on a second-player check have no good place to show
themselves in the stock lobby:

- **Map download** has a footer button for the prompt and sentences in `w2r.log` for status
  ([map-download.md](map-download.md), slice 4 notes). wc2r-plus shows a per-slot progress bar, a
  progress panel over the map preview and local chat notices ("Ann declined map download and was
  disconnected").
- **Lobby policy** has no visible state except the create screen's lock buttons. wc2r-plus shows
  Teams and Slots locked or unlocked, with the host able to flip them, and a "Teams locked by host"
  notice in chat.
- **The handshake** knows which players run the mod and who the host is; nothing shows it.

The lobby banner is parked because a Nuklear window begun inside the lobby froze the game. The
create screen proved the way around it: replace the screen's content from inside the game's own
window. This is the same move for the lobby, and the Download button after Leave goes away.

## What it is

The game's window named `multiplayer_lobby_screen` is the full-window Nuklear window the lobby
draws every frame. A **named-window hook** (`session::on_window`, already how `lobby.rs` sees the
lobby) lets us draw the window's content ourselves and tell the game to skip its own
(`Result::Suppress`, after we end the window). Our layout:

- **Banner and game name** (the game's `DrawBanner`, then the lobby name centred).
- **Left column** (58 % of the content width):
  - the **Players panel** (58 % of the height): a header "Players (n/max)", Race, Team, then 8
    rows. Each row is the game's own `MpLobbySlotRow(i, ratios, rowH, spacer)`, so slot dropdowns,
    Closed slots, "Move to Slot N" and team changes keep working untouched, with our column ratios
    (name 56 %, race 30 %, team 14 %) and a left gutter for the marks;
  - the **chat history** (the game's own chat lines from `MpLobbyChatBegin/End`, with our notices
    interleaved) and the **chat input** with a Send button;
- **Right column**: the map name, the **map preview** (square, the one `previewdraw.rs` already
  paints for the create screen) and, below, seven **info rows**: Password, Game speed, Resources,
  Peasants only, Starting location, Teams lock, Slots lock (host: a button that flips it).
- **Footer**: Leave, and Start for the host (the game's own `MpLobbyLeaveButton` and
  `MpLobbyStartGameButton`, so the Start hold from map download keeps working). Both are disabled
  for 1.5 s after entering (`MpLobbyEnterMs`), and everything is disabled during the start
  countdown or while a download prompt is asking.
- **Marks drawn over the slot rows** after the window content, in the window's own draw list: a
  crown beside the host, a green dot beside players who announced the mod (`LobbySlotRunsMod`), the
  per-slot download bar along the bottom of the name cell, and, when starting locations are fixed,
  a colour swatch per slot.
- **Download panel** over the map preview while this player waits or downloads ("Looking for
  map...", "Downloading" and a bar); the preview is hidden then and when the map is missing.
- **Start tooltip**: hovering the Start button while downloads are pending lists who.
- **Team numbers**: a `LookupLocalizedString` hook rewrites the team dropdown's labels to "1" to
  "8" (digits) only while our slots panel draws (`g_rewriteTeamLabels`).

## What it stands on, and what is new here

Almost everything is already in the Rust host:

| Piece | Already here | New |
|---|---|---|
| Kit: rectangle placement, label, heading, button, text edit, list, panel frame, window rect, `Localized`, skin push/pop, lobby frame and footer geometry | `screenkit.rs` (+ the geometry constants) | `row_ratios`, `wrapped_label`, `count_wrapped_lines`, `window_canvas`, `fill_rect` wrappers if the create screen did not need them; check before adding |
| Game symbols: `MpLobbySlotRow`, `MpLobbyLeaveButton`, `MpLobbyStartGameButton`, `MpLobbySendChat`, `MpLobbyMaxPlayers`, `MpLobbySlots`, `MpLobbySlotNames`, `MpLobbyChat*`, `MpLobbyTeamOptions`, `MpLobbyResourceKeys`, `MpLobbyTilesetKeys`, `MpLobbyMatchFlags`, `MpLobbyFixedStartLocation`, `MpLobbyEnterMs`, `MpLobbyCountdownActive`, `LookupLocalized`, `LayoutSpaceBegin/End`, `LayoutRowRatios/Dynamic`, `CountWrappedLines`, `DrawWrappedLabel`, `InputKeyPressed`, `Spacing`, `DisableBegin/End`, `NkFillRect`, `NkDrawImage`, `CreateGameTexture`, `DrawBanner` | all in `target/symbols.rs` | the layout checks for any this screen is the first to use |
| Map preview painting | `previewdraw.rs`, `mappreview.rs`, `pud.rs` | a draw-at-rectangle entry that takes the game's selected map path and id (the create screen's takes its own selection) |
| Download state per slot, prompt, events | `maproles.rs` (`slot_progress`, `prompt`, `answer_prompt`, `DownloadEvent`, `event_text`) | a read-only snapshot API from `mapdownload.rs`: `slot_status(i)`, `prompt()`, `pending_slots()`, `map_missing()`, `take_events()` |
| Lock state, password, lock events | `lobbypolicy.rs` (`password`, `next_event`, locks) | a `set_locks` entry for the host's buttons if the create screen's is not reusable |
| Host and mod flags per slot | handshake roster, `session.rs` (`lobby_slot_of`) | `lobby_slot_runs_mod(i)` |
| Crown and swatches | none | `crown.rs`: the RGBA crown bitmap wc2r-plus ships (`crown_image.h`) uploaded once with `CreateGameTexture` (rects fallback if it fails), and the eight team colours (`team_colours.h`) |
| The screen | none | `lobbyscreen.rs` (layout, marks, panel, tooltip, hook), `lobbychat.rs` (history with notices, input) |

The pure parts are separate and unit-tested: the slot geometry (pitch, spacer, gutter, ratios), the
mark rectangles, the download bar fill for each state, the info-row text, the notice anchoring and
trimming, the tooltip placement, and the host-slot rule (`HostSlot`: by net slot 0 first, else by
host name, case-insensitive).

## Behaviour that must match

- **Entry grace.** Buttons are disabled for 1.5 s after the game sets `MpLobbyEnterMs`.
- **Blocked** (countdown running, or a prompt asking): the whole content is wrapped in
  `DisableBegin/End`.
- **Chat history.** Draws the game's own `MpLobbyChatBegin..End` strings (stride 0x18 MSVC
  strings), wrapped to the cell width, scroll kept at the bottom when the user is at the bottom
  and grew, restored when something clears the scroll mid-frame (a context-sequence jump of more
  than one). Notices are local only, anchored after the chat line count at the time, capped at 32,
  cleared when the lobby window has not been drawn for 2 s. Notice colour `rgb(232,196,96)`.
- **Notice texts** are those of `maproles::event_text` (already matching wc2r-plus) plus the lock
  notices "Teams locked by host", "Teams unlocked by host", "Slots locked by host", "Slots unlocked
  by host". The player's name is the last seated name for the slot in the previous 5 s, else "A
  player".
- **Send** is the Send button or Enter; the game's own send function takes the input buffer.
  Chat is rate-limited by the game; we add nothing.
- **Password row** reads "Unknown" until the host's `'L'` message arrived, "None" if empty, else the
  characters (already restricted to printable ASCII on receipt).
- **The window rectangle must be sane and every required symbol resolved**, otherwise the hook
  returns `Continue` and the stock lobby draws.
- **Hostile data.** Slot names, the map name and the lobby name come from peers: bound them and
  strip control characters before they reach the game's label functions (the create screen's rule).

## Setting, fallback and the lesson of the banner

`host.json` gets `enhanced_lobby_screen` (default **false** until verified, then true like the
create screen), shown as a checkbox on the plugin screen. The takeover happens only when the
setting is on, every required symbol resolved, the window rectangle is sane and no fault has
occurred. A fault inside our draw (a panic caught by the guard, a bad pointer check) draws the stock
lobby for the rest of the session and logs once. The user must never be stranded in a lobby they
cannot leave: the Leave button is the game's own and is drawn first.

Hard constraints from this codebase, learned the hard way (see PLAN.md, the banner):

- Never begin a Nuklear window inside the lobby frame. Everything is drawn inside the game's own
  window; in-window placement (`OverrideNextRect` after the game's content) works, nested named
  windows do not.
- No custom popup. The prompt is drawn as a footer-sized area or a panel (below). A real
  `NKPopup` needs hand-built MSVC `std::function` objects and is not worth the risk.
- The 4-slice rule applies: each slice is installed behind the setting and checked by the user.

## The download prompt

wc2r-plus draws the "Download <map> (N KB)?" question as a game popup with Leave and Download
buttons and a seconds countdown. Here, without a popup:

- **Asking**: a panel over the preview area (the preview is hidden), with the map name and size,
  "N seconds remaining", and a Download button; the stock Leave button is still the "no", as in
  slice 4 of map download. The footer button after Leave is removed when this screen is on; with
  the screen off the old footer button stays.
- **Waiting** ("Looking for map" with animated dots) and **Downloading** (bar, percent) are the
  panel states from wc2r-plus.

This keeps `maproles::prompt` and `answer_prompt` as they are; only the drawing changes.

## Modules (all in `w2r-core`, x86 only where they touch the game)

- `lobbyscreen.rs`: the hook, the layout, the marks, the info column, the footer, the tooltip.
- `lobbychat.rs`: history with notices, input and Send.
- `crown.rs`: the crown pixels, the team colours, texture creation with a rectangles fallback.
- Small additions to `screenkit.rs`, `previewdraw.rs`, `mapdownload.rs`, `lobbypolicy.rs` as in the
  table. The old footer button in `mapdownload.rs` stays behind "the screen is off".

## Slices and exit criteria

Each slice ends with an in-game check by the user, behind the setting. The screen itself can be
verified alone (slice 1 to 3); the download and lock visuals need the second player.

1. **Static screen.** The hook, the setting, banner, frame, Players panel with the game's own slot
   rows, the footer with Leave and Start, entry grace and blocked state, team digits. Our content replaces the
   game's, so the chat history and input are drawn in this slice too, just without notices. Exit: host and join a lobby on the new screen, change slots, team
   and race, send chat, Leave and Start work, the setting off gives the stock lobby.
2. **Right column.** The map name, the preview from the game's selection, the info rows, the lock
   rows with the host's flip buttons, the password row. Exit: the rows match the create screen's
   choices; locks flip and persist; a joined player sees the host's locks and password.
3. **Marks.** Crown, mod dots, fixed-start swatches. Exit: the host has a crown, a modded guest a
   green dot, a guest without the mod none.
4. **Download visuals and notices.** `mapdownload.rs` snapshot API, per-slot bars, the preview
   panel, the Asking panel, the Start tooltip, notices in the history (downloads and locks), the
   footer button hidden while this screen draws. Exit (two players): a guest lacking the map sees
   the prompt panel, Download works, the host sees that guest's bar fill, a declined player is
   reported in chat, Start is held and explains why on hover.

## Open questions

1. **Asking panel versus popup.** The panel is safe and simpler; wc2r-plus's popup looks like the
   game. Is a panel acceptable, or is the popup worth the std::function work? Recommended: panel.
2. **Default.** On after slice 4 is verified (as the create screen), or stay off?
3. **Crown art.** Port wc2r-plus's embedded crown bitmap (same licence, MIT) or draw it with
   rectangles only? Recommended: port the bitmap, keep the rectangles as the fallback.
4. **Translations.** Same rule as the create screen: the game's own keys where they exist
   (`mp_lobby_players`, `mp_lobby_race`, `mp_lobby_team`, `option_speed_game_speed`,
   `customscenario_*`, `mp_chat_send`), English literals elsewhere ("Password:", "Teams:", "Slots:",
   "Locked", "Unlocked", the notices).
