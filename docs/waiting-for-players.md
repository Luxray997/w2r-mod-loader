# Names on the "Waiting for players" screen

Warcraft II on Battle.net lists the players the match is waiting on, one per line. The Remastered
build shows only the words "Waiting for players". Ghidra, build 2818:

- `FUN_004f8580` is the stall loop. `ProcessNetworkTurn` enters it after about 2.5 s without a
  full set of turns; it calls `FUN_004f86e0` and then `FUN_0053e8e0` every iteration.
- `FUN_0053e8e0` draws the Nuklear window `timeout_screen` into the menu context
  (`NkContextMenus`). Inside its `text_content` group it begins one row (`LayoutRowDynamic`, height
  120, one column), looks up `mp_waiting_for_players` and draws it with `DrawLabel`, align `0x12`
  (centred).
- `FUN_004f86e0` is the old per-player list. It works out who the match is waiting on, but writes
  the names into a dialog object (`DAT_009378fc`) that the new screen never creates, so the code
  runs and shows nothing. Its test, per lobby seat `i` (`MpLobbySlots`, stride `0x26`):
  state byte at `+8` is 0 (a human), and `TurnSlotFlags[netSlot at +4]` has `0x10000` (in the
  match) and not `0x20000` (turn received). The name is the C string at `+0xd` (up to 25 bytes).

## What we do

`waitinglist.rs` repeats that test and adds one row per player under the game's line:

```
Waiting for players...
Ann - 0:07
Bob - 0:03
```

- The `LookupLocalized` detour in `lobbyscreen.rs` only records the pointer it returns for
  `mp_waiting_for_players`.
- A host hook on `DrawLabel` lets the game draw its line, then, when the label is that exact
  pointer, adds a `LayoutRowDynamic` row (1.6 font heights) and a centred `DrawLabel` per player.
- The time is how long the match has been waiting on that player: counted from the first frame they
  appear in the list, forgotten when they catch up, everything restarted when the screen has been
  gone for 1.5 s. It counts from when the screen appeared, so it starts about 2.5 s after the
  stall began. Using our own clock means it works on every machine, whether or not that machine
  keeps a network record for the player.
- Names are cut at the NUL and stripped of control characters. With nobody to list, nothing is
  added. If a symbol is missing or the hook fails, the game's screen is untouched (logged).
- It works for every player, with or without the mod, and on every machine that sees the screen
  (not only the host). The pure parts (the test, name cleaning, the clocks, the line) are
  unit-tested; the game side is x86 only and the roster read runs under the crash guard.

## To check in the game

1. Two players in a match; stall the second (suspend the process or drop its network for 5 s). The
   first sees the line, then "<name> - 0:00" counting up beneath it.
2. The rows sit inside the window and are readable (spacing, no overlap with the game's line).
   If they overlap, change `ROW_FONT_HEIGHTS`.
3. When the turns resume the screen goes away as before; a later stall starts the clock at 0:00.
4. Three or more players: only the ones missing are listed.
5. If no rows ever show: the label may be drawn with another alignment or the string pointer may
   differ from the lookup's (check the log for the hook line); the roster flags may also differ
   from the old code's assumption.

## Ideas

- Also show how long each player has been *silent* (from `netquality`'s last-heard time), which
  separates a slow player from a dead link; needs the network record, which a joiner may not keep
  for other joiners.
- A host-only "Kick" button beside a name (the in-game kick already exists).
