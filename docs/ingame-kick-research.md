# In-game kick research

Question: can the host remove a player after the match has started? Ghidra on `Warcraft II.exe`
build 2818 (`D:\Tools\ghidra_scripts\obs.sh f:<addr> x:<addr>`). Read-only, nothing hooked yet.
Addresses are static (pre-ASLR). `DAT_0095bbd4` is the net manager; `+0x68` is its transport object
(vtable), `+0x18` is the host flag and `+0x19` the joiner flag (as `peer.rs` has them). Game state is
`FUN_004c4870()` (`DAT_0091c184`): 3 = in game, 0x17 = lobby, 0xd = pre-game.

## How the game drops a peer in a match

1. The transport reports a lost connection through `FUN_00520cc0(slot)`. In game state 3 it calls
   `FUN_0049f560(slot)`, which only sets `DAT_00916a1c[slot] = 3` ("left"). Outside a match it clears
   the lobby seat instead. Slot 0 (the host) leaving shows `mp_error_host_disconnected` on joiners.
2. `FUN_0049f7e0` runs each time a full set of turn packets was received (`ProcessNetworkTurn`
   `0x49ea20`, via `FUN_0049d130`). For every slot with `DAT_00916a1c[slot] != 0` it:
   - prints `mp_lobby_left` (the "<name> left the game" line),
   - marks the player's result: `FUN_004f4f30(owner)` sets `PlayerMatchResult[owner] = 2` (defeated),
     or with code `0x40000006` sets `DAT_0091aa7c[slot] = 1` and `FUN_004f43a0` sets the result to 1,
   - calls `FUN_004df720(owner)` (player cleanup, units not yet analysed),
   - calls transport vtable `+0x24` (acknowledge).
   So the leave takes effect at a turn boundary, on whichever turn each client happens to be on when
   its own connection to that peer closes. Nothing in the turn stream carries the leave.
3. `PlayerMatchResult` value 1 is therefore "dropped by the host / majority" (answers the open
   question in `docs/stats-reporting.md`); 2 is an ordinary leave or defeat.

## Stalls and desyncs

- `ProcessNetworkTurn` waits for turns. After about 2.5 s without them it calls `FUN_004f8580`, the
  `mp_waiting_for_players` screen (`FUN_0053e8e0`), with a per-player list (`FUN_004f86e0`; the row
  callback `FUN_004f8510` only draws a marker). It has no drop action of its own; it ends when turns
  arrive or the local user leaves.
- When a turn fails to process, `FUN_004f8330` drops every slot whose flag word
  (`DAT_00916220[slot]`: `0x10000` in match, `0x20000` turn received, `0x40000` possibly dropped) has
  `0x10000` and not `0x40000`, with `FUN_0049ef20(slot, 0, 0x40000006)`. If the dropped side is the
  majority it ends the match for everyone (`FUN_00565420(0x40000006)`).

## Drop primitive

`FUN_0049ef20(slot, _, reason)` calls transport vtable `+0x28(slot, reason)`, the same call
`KickLobbyPeer` (`0x0049b5e0`) makes after sending a 2-byte message `{0x29, reason}`.

## The kick message

`FUN_00499950` (a lobby message pump) handles type `0x29` only when the sender is slot 0 and the
local client is a joiner: it shows `mp_kick_player_msg`. The in-game pump `FUN_0049e9a0` dispatches
only message types 0 and 1 (chat) through the table at `0x837f50`, so a `0x29` sent mid-match is
probably ignored. The kicked client would then learn about it only through the closed connection and
would treat the host as having left.

## The transport (Ghidra, 2026-10-10)

`DAT_0095bbd4` is a `War2MultiplayerBNet`; the object at `+0x68` is a `WRTSNet::MultiplayerImpl`
(vtable `0x88934c`). Its slot `+0x28`, `FUN_007d44b0(slot, reason)`, is the drop:

- returns 3 if `slot == 0xff` or it is our own slot, 4 if we are not the host (vtable `+0x1c`),
  5 if the slot has no player record;
- marks the victim's record dropped (state 8, reason stored) and then **sends a 12-byte control
  message `{slot, id, reason}` to every other connected player** (`FUN_007d6640`, channel 0).

So the transport is host-authoritative: one host-side call tells every other client, and each of
them runs its normal peer-left path (`FUN_0049f560` flags the slot, `FUN_0049f7e0` removes the player
at the next turn). No peer-to-peer topology problem, and no extra plugin message is needed. Slot `+0x24`
(`FUN_007d4420`) is just the connected-player count; slot `+0x3c` (`FUN_007d5270`) sends to one slot.

The `Callbacks` vtable (`0x844074`) holds `FUN_00520cc0` (peer left) at `0x8440a4`.

## Open (needs a game test with a second human)

- The host's own removal of the victim: `FUN_0049ef20` does nothing locally after the drop call, so
  either the transport fires the callback for the host too later, or the host never removes the player
  and desyncs. If it does not fire, call `FUN_0049f560(slot)` on the host right after the drop.
- Turn skew: each client removes the player at *its* next full set of turns, so the removal can land
  on different turn numbers on different clients. The game already relies on this for ordinary leaves;
  check `NetTurnCounter` (`0x916210`) and `TurnSyncMismatch` (`0x9166fc`) on every client.
- What the victim's client does (the `0x29` message is probably ignored mid-match; it should end up
  with a host-disconnected error when the host stops sending to it).
- What `FUN_004df720` and `FUN_004f3f80` do to the dropped player's units and to the win check.

## Plan

Host-only action during a match (hotkey or small player list) that calls `FUN_0049ef20(slot, 0,
0x40000001)` (drop plus the game's "player left" message on the host) and, if the host does not
remove the player by itself, `FUN_0049f560(slot)`. Log `NetTurnCounter` and `TurnSyncMismatch` on
every client while testing.

## Implementation (2026-10-10, built, not yet run in game)

- `kickpanel.rs`: the pure part (pick, two-press `Confirm`), unit-tested.
- `ingamekick.rs`: reads the roster (net slots 1..8 that are seated, `TurnSlotFlags` in match and not
  dropped, human owner), calls `DropNetPeer` (`0x0049ef20`, reason `0x40000001`, null message), then
  watches `PlayerMatchResult[player]`: if it is still 0 after 1 s and `PeerLeftFlags[slot]` is clear,
  the host calls `NetPeerLeft` (`0x0049f560`) itself; gives up after 4 s. Every step is logged
  (`kick` source) with `NetTurnCounter` and `TurnSyncMismatch`.
- Two ways in, host only, live match only (game state 3, not a replay):
  1. Ctrl+Shift+K opens a panel on the HUD (top right): PgUp/PgDn pick, Delete twice removes, Esc or
     Ctrl+Shift+K closes.
  2. The Alliances dialog (`alliances_menu`): `AlliancesFooter` (`0x00524ac0`) is hooked and a
     "name [Kick]" row per player is drawn above Cancel/OK; the button reads "Sure?" after the first
     click. Row geometry is derived from the footer rectangle, so it may need adjusting once seen.
- New symbols: `DropNetPeer`, `GameStateCurrent` (`0x0091c184`), `NetPeerLeft`, `PeerLeftFlags`
  (`0x00916a1c`), `TurnSlotFlags` (`0x00916220`).

To test with a second human: host a game, start it, kick from either place, then read the log lines
on both machines and check the kicked client's screen.
