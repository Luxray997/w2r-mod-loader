# Lobby policy (Phase 3, step 3)

Status: spec, not implemented. Port of wc2r-plus `src/features/lobby_policy.cpp` (438 lines) and the
small `lobby_teams.cpp`. Same parity rule as [chat-plugin.md](chat-plugin.md): the C++ is the
reference, and where this document and the C++ differ, the C++ wins.

## Decision: a host feature, not a plugin

PLAN.md left this open "at its turn". It is a host feature, for the reasons the handshake was:
it owns the reserved `'L'` peer subsystem, it needs the handshake roster (`PeerRunsMod`: locks go
only to peers that announced), it refuses packets (a lock must hold even when no plugin is
installed on the host), and it hooks the game's lobby request handlers, which are outside the
plugin ABI. The chat plugin's lobby data needs (`lobby_teams`) are already read by that plugin
directly and stay there.

## What it does

All of it is in the multiplayer lobby, and only the host can change it.

1. **Lock team changes.** When on, a non-host peer's team-change request is refused
   (`HandleLobbyTeamChangeRequest` detour, sender net slot 0 is the host). Peers see the team
   dropdown disabled.
2. **Lock slot swaps.** Same for `HandleLobbyExchangeTwoSlotsRequest` and the peers' name-cell
   dropdown.
3. **Host-only slot moves.** The host's player-name dropdown gets "Move to Slot N" entries (one per
   open slot) beside Kick; picking one performs the move through the game's own swap handler, after
   re-checking that the lobby did not change in between, and never during the start countdown.
4. **Closed slots.** The slot-state dropdown hides the current state and shows `Closed` for a
   closed slot, as wc2r-plus does.
5. **Broadcast.** The host sends an `'L'` message to every announced peer when the locks change and
   every 2 s while in the lobby: protocol 1, a flags byte (bit 0 teams, bit 1 slots), then the
   lobby password as a length-prefixed string. Peers accept it only from net slot 0, only while in
   the lobby, and render the password characters outside 0x20-0x7e as `?`. Lock changes become
   lock events a UI can show ("Team changes locked").
6. **State reset.** All of it is forgotten when the lobby window has not been seen for 2 s
   (`Touch`), as wc2r-plus does.

Non-goals: anything outside the lobby, any change to what the game puts on the wire for its own
messages.

## Where the choice lives

wc2r-plus stores the host's choice from its own Create Multiplayer Game screen, which is a custom
screen (`lobby_creation_screen.cpp`) and not part of this step. Here the choice is two checkboxes on
the plugin screen, stored in `config\host.json` as
`lock_team_changes` and `lock_slot_swaps` (both default false; `hostcfg.rs` accepts only known
fields, so the schema stays 1 and the two fields are added to the known list). They are applied
once when the player becomes the lobby host, and can be flipped while hosting.

## Host-side pieces

| Piece | Where |
|---|---|
| Pure policy state: locks, events, adoption rules, message encode/decode, the sender and lobby-only rules, the slot-move recheck | new `lobbypolicy.rs`, unit-tested without the game |
| `'L'` delivery | `peer.rs`: replace the "no handler yet" arm with `session::deliver_policy`, like `deliver_handshake` |
| Hooks | `LobbySlotDropdown`, `HandleLobbyTeamChangeRequest`, `HandleLobbyExchangeTwoSlotsRequest` through `hooks.rs`, under the host guard |
| Per-frame work | the lobby-window callback in `session::on_window`: resend, apply a pending move, drop it during the countdown (`MpLobbyCountdownActive`) |
| Config and UI | `hostcfg.rs` fields, `ui.rs` checkboxes, the existing save path in `session.rs` |

Symbols already in the table: `LobbySlotDropdown`, `HandleLobbyTeamChangeRequest`,
`HandleLobbyExchangeTwoSlotsRequest`, `MpLobbyTeamOptions`, `MpLobbyCountdownActive`,
`LobbyPassword`, `NetSession`, `MpLobbySlots`, `StringAssign`. Slot layout (stride 0x26, state at
+0x08, team +0x0a, name +0x0d, open = 5) is the one the chat plugin already reads. Build-specific,
so it is covered by the existing build guard.

## Known review findings to fix while porting

- The password goes to peers in clear on the game's reliable channel; keep it only to peers that
  announced (wc2r-plus does the same), and bound the length (< `kLobbyPasswordCapacity`).
- `HostPlayerCell` keeps static per-slot menu buffers whose pointers the game's dropdown reads; the
  port keeps them for the call only and never hands the game a pointer into a `Vec` that can move.
- Inbound `'L'` is already rate-limited per sender by `peer.rs`; a malformed or short message is
  ignored, never an error.

## Slices and exit criteria

Each slice ends with an in-game check by the user. A second player is needed for the peer side; the
host side is checkable alone.

1. **Policy core and enforcement.** `lobbypolicy.rs`, the `'L'` arm, the two request hooks, the host
   config fields and checkboxes. Exit: unit tests (message round trip, sender and lobby-only rules,
   adoption, reset); in the game, a locked lobby refuses a peer's team change and slot swap and logs
   it.
2. **Broadcast and peer view.** The 2 s resend, peer-side adoption, the disabled dropdowns, the lock
   events in the log.
3. **Host menus.** "Move to Slot N" and the Closed rendering through the `LobbySlotDropdown` hook.
   The in-lobby banner stays parked (reason in PLAN.md).
4. **Lock choice on the Create Multiplayer Game screen.** See "Slice 4" below. Done: the buttons are
   part of the replacement screen ([create-screen.md](create-screen.md)); the interim overlay on the
   stock screen (4a) was removed.

## Slice 4: the Create Multiplayer Game screen

What wc2r-plus does (`features/ui/lobby_creation_screen.cpp`, 376 lines): it hooks the game's
`LobbyCreationContent` (`char(__cdecl*)()`, drawn every frame of that screen) and, when its setting
is on, draws a whole replacement screen instead of calling the original. The lock choice (two
"Locked/Unlocked" buttons under Teams and Slots, stored as the `lock_teams`/`lock_slots` settings
and applied once when the player becomes host through `ApplyCreateChoiceOnce`) is a small part of
it. The rest is a map browser and preview: `map_browser.cpp` (510), `map_search_cache.cpp` (276),
`map_panel.cpp` (62), `map_preview.cpp` (722), `map_preview_pud.cpp` (76), and the drawing helpers
in `screen_kit.cpp` (497), about 2,500 lines, none of which is lobby policy (the PUD validator
and the folder browsing belong with map download, step 4).

Two ways to do it here:

- **4a, add the two lock buttons to the stock screen (recommended).** Hook `LobbyCreationContent`,
  call the original, then draw a small "Teams / Slots" pair of buttons in a fixed corner of the
  same window, using the same primitives wc2r-plus's screen kit uses (`OverrideNextRect`, the
  game's label and button functions, `PushSkin`). The choice is `lock_team_changes` and
  `lock_slot_swaps` in `host.json`, already read when hosting, so nothing else changes. About
  150 lines plus the screen-kit subset it needs (rectangle placement, a button, a label), which
  later work (map download UI) reuses. Risk: this is a Nuklear draw from a hook, the thing that
  froze the lobby once; the difference is that this hook is the game's own screen content
  function (the same place wc2r-plus draws), and the draw is guarded and falls back to the stock
  screen if any precondition fails, like the chat panel.
- **4b, the full replacement screen.** Port everything above. It is really the map-browser
  feature, not lobby policy, and belongs after (or inside) map download. Until then the
  plugin-screen checkboxes remain the way to set the locks.

Exit for 4a: in the game, the stock Create Multiplayer Game screen shows the two buttons, they
flip and persist across launches in `host.json`, the stock screen is otherwise unchanged, and the
frame hook logs once if it falls back.

## Decisions (2026-10-04)

1. **The lock choice belongs on the Create Multiplayer Game screen** (slice 4; the full screen
   replacement is a separate, larger feature, see "Slice 4"). Until it lands, the two plugin-screen checkboxes are the way to set the choice; afterwards they stay as a
   second place to change it, and `host.json` stays the single stored value (wc2r-plus's "last
   choice").
2. **Password sharing keeps parity**: sent to announced peers in clear, as wc2r-plus does.
