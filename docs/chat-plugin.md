# Chat plugin (Phase 3, step 2)

Status: spec, not implemented. Port of wc2r-plus `src/features/chat.cpp` (1,104 lines) as a
first-party plugin over the public ABI ([abi.md](abi.md)). Decision recorded in
[PLAN.md](PLAN.md), Phase 3: chat is a plugin (approach A: raw hooks plus the game's own
Nuklear functions), not a host feature.

## Parity rule

wc2r-plus's `chat.cpp` is the reference implementation, and it is proven in the game. The plugin
is a port, not a redesign: the same hooks, the same order of operations inside each hook, the
same constants (colours, channel labels, scan codes, slot indices, strides, Nuklear offsets), the
same setting keys, defaults and ranges, the same log file name and line format. Where this
document and the C++ differ, the C++ wins and this document is wrong. Because messages leave
through the game's own `SendComposedChat`, nothing changes on the wire and players with
wc2r-plus, with w2rpm or with neither interoperate as before. Only the host-side changes (the
ABI additions below) are new.

## What it does (parity target)

All of it is in-match; the lobby chat is the game's own and stays untouched.

1. **History panel.** A scrollable box of past messages (default 8 wrapped lines high, backlog
   100 messages, 25 to 500) above the game's two-line prompt/sticky strip. Shown while paused,
   while typing, or always when fade is 0. Otherwise recent lines only, fading after
   `fade` seconds (default 7, 0 = never).
2. **Channels.** Every message is labelled and coloured: `[All]` grey, `[Team]` green,
   `[Allies]` blue, `[Enemies]`/`[Private]` grey, system lines gold. Enter opens `[Team]`,
   Ctrl+Enter `[Allies]`, Shift+Enter `[All]` (falling back to `[All]` when the channel would
   be empty). Tab cycles Team, All, Allies while typing, skipping empty channels. The prompt
   line carries the channel label and colour.
3. **Player-name colours.** The sender's name is drawn in their in-game colour (black player
   gets a dark grey so it stays readable). System lines get every player name coloured.
4. **Timestamps** (`[HH:MM]`, off by default).
5. **Pause announcements.** "X paused the game. (N pauses remaining)" and "X resumed the game."
   as system lines, replacing the game's sticky message for those two events.
6. **Click closes chat** (off by default): a click discards the draft and returns focus to
   the game so hotkeys work.
7. **Disk log** (off by default): `chat-<date>_<time>.log`, one per launch, plus a
   `=== new match ===` marker.
8. **Settings** on the plugin screen: history height, fade time, click closes chat, backlog
   limit, timestamps, log to disk, background opacity (0 to 255, default 100), colour names,
   enhanced channels, and a master enable.

Non-goals: lobby chat changes, new chat features, changing what is sent on the wire (messages
go out through the game's own `SendComposedChat`; players without the plugin interoperate).

## Why this does not fit the ABI yet

The chat code needs four things the ABI does not offer today. Everything else it uses is
already there.

| Need | Today | Required change |
|---|---|---|
| Call game functions that are not hooked (about 14 Nuklear/layout helpers: `OverrideNextRect`, `BeginNamedGroup`, `CountWrappedLines`, `NkFillRect`, ...) | `symbol_lookup` returns an opaque handle; only `symbol_read` (read-only) and `hook_install` use it | `symbol_address(token, handle, out *usize)` |
| Read and write game globals (`ChatTargetMode`, `ChatRecipientMask`, message slots, the Nuklear context's style fields) | `symbol_read` only reads, 4,096 bytes at a time | the same `symbol_address`; the plugin writes through the pointer |
| Settings | `settings_*` exist in the ABI but the host returns `W2R_E_UNSUPPORTED` | implement them (stored in `config\<plugin-id>.json`, rules already in abi.md) |
| Disk log | `data_*` (`fs.data`) exist in the ABI, host returns `W2R_E_UNSUPPORTED` | implement them |

**`symbol_address`.** Appended at the end of `W2rHost` (so no `W2R_ABI_VERSION` bump; older
hosts report a smaller `struct_size`). It needs `hooks.raw`, not `game.read`: a raw address
lets a plugin read and write any game memory, which is exactly what `hooks.raw` is already
shown as **dangerous** for, and the consent prompt must say so. It returns the runtime
address (image base added) of a symbol of any kind and `W2R_E_NOT_FOUND` for a bad handle.
Because the plugin then calls into the game and writes game memory outside the host's guard,
the chat plugin carries its own protection: every detour wraps its body in `catch_unwind`, and
every access to Nuklear structure offsets is bounds-checked against the structure sizes the
plugin was built for. The manifest pins `game_builds` to `1.0.2.2818` (the offsets are
build-specific, like wc2r-plus).

`MATCH_START`, `MATCH_END` and `SETTINGS_CHANGED` are in the ABI, but the host never fires the
match events today (only subscription is implemented and tested). Firing them is a host
prerequisite: wc2r-plus clears the backlog and writes the log marker on match start.

## Hooks the plugin installs

All are `cdecl`, all by `symbol_lookup` then `hook_install`, so each is inside the game
module and cannot overlap a host patch. The detour always forwards to the trampoline unless
noted. Signatures are the game's, as in `src/target/addresses.h` of wc2r-plus.

| Symbol (our table) | Signature | The plugin's change |
|---|---|---|
| `PushMapMessage` | `(const char* text, int highlight, unsigned ms)` | capture the line into the backlog (with channel and name spans), then forward |
| `DisplayChatOrGameMessage` | `(const char* text, uint8_t who, unsigned ms, char targetMode)` | publish the sender context (who, raw text, target mode) for the nested `PushMapMessage`, restore after |
| `BuildMapMessagesPanel` | `(void* ctx, float x, float y, float w, float h, int lineHeight)` | replaces the panel: history group, recent-lines group, strip group; temporarily flattens scrollbar style fields and restores them |
| `DrawMapMessageList` | `(void* ctx, int lineHeight)` | draws the prompt line in its channel colour, then the sticky line |
| `WriteStickyMessage` | `(const char* text, unsigned ms)` | drop it while inside the pause/resume handlers (the plugin announces instead) |
| `ChatTextboxEventProc` | `(void* dlg, uint8_t* event)` | on Enter-open, choose the channel from the modifier keys; swallow Tab while composing |
| `MinimapTerrainKeyProc` | `(void* dlg, uint8_t* event)` | Tab while composing cycles the channel |
| `SendComposedChat` | `(const char* text)` | set target mode and recipient mask for the chosen channel around the call, restore after |
| `WriteChatPromptLine` | `(const char* fmt, const char* a1, const char* a2)` | prefix the channel label, remember the prompt channel |
| `HandlePauseCommand`, `HandleResumeCommand` | `()` | set an "in handler" flag around the call; announce when the pause flag changed |

Game globals the plugin reads or writes: `MapMessageSlots` (16 slots, stride from wc2r-plus
`wc2r::kMessageSlotStride`), `ChatTargetMode`, `ChatRecipientMask`, `ChatDialog`,
`MatchPauseFlags`, `CommandIssuer`, `NetworkedMatch`, `PausesRemaining`, `LocalPlayer`, plus
the Nuklear context offsets in wc2r-plus `struct_offsets.h` (`kCtxScrollH`, `kCtxScrollV`,
`kCtxScrollbarSize`, panel bounds, mouse buttons). All symbols named above are already in
`target/symbols.rs` except possibly the pause handlers (ours are `HandlePauseCommand` and
`HandleResumeCommand`); the spec step verifies each by name and signature against the table.

## Data the plugin needs from elsewhere

- **Player names and colours.** wc2r-plus reads them through `team_colours` and `lobby_teams`
  features that are not ported. The plugin reads the game's own player tables directly through
  `symbol_address`, so chat does not wait for the lobby-policy step. Which tables (slot name
  array `MpLobbySlotNames`, player colour index) is the first thing the implementation step
  must settle against a running game; until then name colouring is the last slice.
- **Teammate and allied masks** (for `[Team]`/`[Allies]`): `TeammatesAtMatchStart` in
  wc2r-plus comes from the lobby's team options. Same rule: read the game's data; the Team
  and Allies channels are the second-to-last slice.

## Plugin shape

- Crate `crates/chat-plugin` (cdylib, i686, Rust; builds against `w2r-abi`, with the SDK pieces
  it needs inline until `w2r-sdk` exists). It moves to the `w2r-plugins` repo in Phase 6.
- Manifest: id `w2r-chat`, capabilities `hooks.raw`, `chat.read` is **not** needed (the plugin
  sees chat through its own hooks, with the game's raw text; it must therefore apply the same
  printable-ASCII reduction the host applies for other plugins before storing or drawing),
  `affects_simulation: false` (it only changes display and the target mode of an outgoing
  message the game would send anyway).
- State is a main-thread value behind a mutex, as wc2r-plus does with a critical section: the
  game may call the message hooks off the main thread; the draw hooks run on it.

## Slices and exit criteria

Each slice ends with an in-game check by the user (they cannot easily run a second w2rpm
player, so checks are single-player or against a vanilla peer, and the host's watchdog
([watchdog.rs](../crates/w2r-core/src/watchdog.rs)) is on).

0. **Host prerequisites.** `symbol_address`; `settings_*`; `data_*`; fire `MATCH_START` and `MATCH_END`.
   Unit and integration tests in `w2r-core/tests`, an ABI doc update, and a
   capability-prompt wording update for `hooks.raw`. Exit: a fixture plugin round-trips each.
1. **Capture and history.** `PushMapMessage`, `DisplayChatOrGameMessage`,
   `BuildMapMessagesPanel`, `DrawMapMessageList`: the scrollable history works, including while
   paused, with fade and backlog settings. No channels, colours or timestamps yet.
2. **Pause and timestamps.** Pause/resume announcements, timestamps, disk log.
3. **Channels.** Compose-channel selection, Tab cycling, prompt label and colours.
4. **Name colours.**
5. **Click closes chat and polish.** Background opacity, scroll indicator, unfocus.

Exit for the step: feature parity with the list above on 1.0.2.2818, one regression test per
behaviour that has a pure core (backlog cap and fade, channel selection and cycling, name-span
finding, timestamp formatting), and a security pass focused on the detours (they are outside the
host's guard, see the abi.md "Hook detours" note).

## Risks

- **Render-path faults freeze or crash the game** (this session's lobby banner is the example:
  a Nuklear window in the wrong place hung the game's own combo code). The panel replacement
  runs every in-match frame; slices ship behind the settings master switch, and the plugin falls
  back to the game's original `BuildMapMessagesPanel` whenever any precondition fails.
- **Struct offsets are build-specific**; `game_builds` pins the plugin.
- **`symbol_address` weakens `game.read`'s guarantees**, hence `hooks.raw`.

## Open questions

1. Which player-colour and name tables to read for name colouring (settled in slice 4).
2. Whether `symbol_address` should also accept a `size`, so the host can refuse an address
   range outside the game's image (cheap, and keeps the plugin from being handed a stack
   address by mistake). Recommended: yes, a lookup returns base plus size of the symbol.
