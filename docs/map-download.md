# Map download (Phase 3, step 4)

Status: slices 1 to 3 implemented (slice 3 awaiting a two-player in-game check); slice 4 not started. Port of wc2r-plus `src/features/map_download.cpp` and
`src/features/map_download/` (about 3,700 lines). The wire format and the safety rules are
specified in wc2r-plus `PROTOCOL.md` section 3; the C++ is the reference where this document is
silent or differs. Same parity rule as [chat-plugin.md](chat-plugin.md): players with wc2r-plus,
with w2rpm or with neither must interoperate, so nothing on the wire changes.

## Decision: a host feature, not a plugin

Like the handshake and the lobby policy, it owns a reserved peer subsystem (`'M'`), needs the
handshake roster (a map goes only to players who announced map-sharing capability, bit 2), hooks
three game functions, writes files into the game's own Maps folder and must refuse hostile input
before anything touches the disk. None of that fits behind the plugin ABI.

## What it does

A player who joins a lobby without the host's selected map can fetch it from the host.

1. **Host.** When the lobby's map changes, every modded player asks about it (`REQUEST`, by map
   name only). The host answers only if the name exactly equals the lobby's map name, then offers
   the file the game has selected (`OFFER`: size, SHA-256, chunk length), read from the game's
   Maps folders, never from a path a peer supplied. Chunks go out in a window of 8, 64 chunks per
   second, acknowledged (`ACK`), with progress broadcast to everyone (`PROGRESS`). Limits per
   player per stay: 32 uploads, 2 completed downloads of one map, 3 unused offers of one map; at
   most 2 uploads at once; an upload not finished 120 s after `ACCEPT` removes the player.
2. **Player.** On a missing map the player is asked (or auto-downloads when that setting is on,
   default off), says `HAVE` if a map with that hash is already in the local index, otherwise
   `ACCEPT`s and receives. The result must hash to the offered SHA-256 and pass the PUD
   validator before it is written; it is stored under `Maps\Download\V<n>\<name>` (never
   overwritten, 64 MiB and 512 files quota, reparse points refused), registered with the game's
   map registry and selected. A map the player cannot obtain makes the player leave with a reason
   the host shows.
3. **Start is held** while any download is pending (`MpLobbyStartGameButton` hook), and
   never once the countdown runs.
4. **Hash index.** All maps under Maps are hashed in the background (cached in
   `resources\map_hashes.txt`) so `HAVE` and "do I already have this map by content" are cheap.
5. **Settings.** `map_sharing` (default on) and `auto_download` (default off), shown on the plugin
   screen. The handshake announces map sharing in its capability bit 2 (already reserved).

Safety rules carried over unchanged from `PROTOCOL.md`: map size 1 to 262,144 bytes; names
printable ASCII, no path characters, `.pud` extension, no device names, at most 64 bytes; the
validator checks structure, tags, lengths, dimensions, owners, eras, units, tiles, terrain flags and
oil map (full list in `pud_validator.cpp` and `pud_rules.h`); a map that passes is still loaded by
the game's own code, which nobody has audited, and the settings say so.

## Host-side pieces

| Piece | wc2r-plus | Here |
|---|---|---|
| Name rules | `map_name.cpp` | `mapname.rs`, pure |
| Wire encode and decode (kinds 1 to 11, header 23 bytes: envelope, version, kind, 16-byte request id) | `wire.cpp` | `mapwire.rs`, pure, a round trip and a hostile-input test per kind |
| PUD validator and its rules table | `pud_validator.cpp`, `pud_rules.h` | `pudvalidate.rs`, on the `pud.rs` section reader already written for the preview; hand-built good and bad maps for every fault |
| SHA-256 | own `sha256.cpp` | the `sha2` crate the host already uses |
| Upload and download state machines, limits, grace and deadlines | `download_host.cpp`, `download_client.cpp`, `limits.h` | `downloadhost.rs`, `downloadclient.rs`: pure, a millisecond clock and an outbox in and out, so every timeout and limit is a unit test |
| Host and client roles: who may ask, prompt, leave-if-unobtainable, progress | `host_role.cpp`, `client_role.cpp` | `maproles.rs` |
| Store: `Download\V<n>`, quota, `.part` then rename | `map_store.cpp` | `mapstore.rs`, with the temp-dir tests `hostcfg.rs` uses |
| Hash index and its cache | `map_index.cpp` | `mapindex.rs` |
| Game access: lobby map name, selected path, maps root, select by path, register a folder, servable-file read with final-path containment | `game_map.cpp` | `gamemap.rs`, x86 |
| Hooks | `SelectMapByRegistryName`, `MpLobbyStartGameButton`, `War2MpBNetCtor` | `mapdownload.rs`, x86; symbols are in the table, plus `ScanPudMetadataFromBuffer`, `SelectMapAndReadHeader`, `ScanDirectoryIntoMapRegistry`, `MissingMapFlag`, `MapsBaseDir`, `MpLobbyMapNameText`, `SelectedMapPath` |
| `'M'` delivery | `peer_messages::Register('M', ...)` | the arm in `peer.rs` that today consumes and drops `'M'` |

The two pieces the create screen deferred land here: the registry rescan behind the browser's
reload button (`ScanDirectoryIntoMapRegistry`) and the on-disk search-index cache.

## Known review findings to fix while porting

The Phase 3 note in PLAN.md lists them: per-era tile and REGM validation in the PUD validator,
throttled logging, no packet-loop starvation, no unvalidated UNC paths. Concretely: the validator
gets a tile check per era (not one set for all), every log line about a peer is rate-limited, the
upload loop yields after its per-frame budget so it cannot starve `GetNextPeerPacket`, and every
path from the game or the wire goes through the same containment check (`mappreview::safe_map_path`
plus the final-path test for reads), UNC and `..` refused.

## Slices and exit criteria

Each ends with unit tests; the game-facing ones end with a two-player in-game check.

1. **Pure core.** `mapname.rs`, `mapwire.rs`, `pudvalidate.rs`. Exit: round-trip and hostile-input
   tests for the wire, every `PudFault` reachable by a test map, names accepted and refused as in
   `PROTOCOL.md`.
2. **State machines.** `downloadhost.rs`, `downloadclient.rs`. Exit: scenarios with a fake clock for
   a clean transfer, decline, `HAVE`, stall, deadline, rate limits, per-stay limits, duplicates and
   hash mismatch, and a long-chunk flood.
3. **Disk and game glue.** Store, index and cache, game map access, hooks, the `'M'` arm, the lobby
   frame tick, settings and capability bit. Exit: a lobby where the guest lacks the map downloads
   it from the host (and the reverse with wc2r-plus on the other end if available), Start is held
   meanwhile, the map is selected, the lobby log shows each step.
4. **Prompt and status.** The "download this map?" prompt through the game's own popup
   (`GameListPopup`, as wc2r-plus does elsewhere; never a Nuklear window begun from a hook, the
   lobby banner lesson), per-slot status lines in the log and lobby chat, and a Start button shown
   disabled while a download is pending. Exit: the prompt appears for a missing map, Download and
   Leave work, a declined player is reported to the host.

## Open questions

1. wc2r-plus draws per-slot **download bars** in its replacement multiplayer lobby screen
   (`ui/multiplayer_lobby_screen.cpp`, a large screen port in its own right). Slice 4 above gives
   the same information through the popup, the log and chat lines, without porting that screen.
   Port the lobby screen too (as a separate spec, after this), or leave it at slice 4?
2. A second modded player is needed to check slices 3 and 4 end to end; so far the lobby policy
   slices are waiting on the same thing.

## Slice 1 notes (2026-10-04)

- The validator's "per-era tile check" review finding is a table (`TILE_ROWS`) with one entry per
  era. On this build every era's `cv4` is 6,636 bytes, 158 rows, so all four entries are 158; the
  table is there so a different build is a one-line change, and ERAX selects the swamp entry.
- REGM is length-checked only, as `PROTOCOL.md` says real maps use nearly every value.
- `mapwire::decode` takes what follows the five-byte envelope (what the packet hook has);
  `encode` returns the whole message. Both agree with the lengths in `PROTOCOL.md`'s table, and a
  test cuts and lengthens every kind by one byte.
- The `'M'` arm in `peer.rs` still consumes and drops the message; the state machines in slice 2
  are what it will feed.

## Slice 2 notes (2026-10-04)

- `downloadhost.rs` and `downloadclient.rs` are pure: a `u64` millisecond clock and decoded
  `mapwire::Msg` values in; `Out` messages (to a net slot or to every modded player), net slots to
  remove and the finished file out. Every number is in `maplimits.rs`.
- Chunk pacing is a 64-chunks-a-second token bucket capped at one second's worth, so one `tick`
  emits at most 64 chunks; the glue still sends the outbox under its own per-frame budget so a
  large upload can never starve the packet loop.
- The client allocates the buffer only on accept, at exactly the offered size, takes chunks only in
  order with the length check done by subtraction, and checks hash and validator before `Done`.
- The `retries` row of `PROTOCOL.md` (ask again after 5.5 s, at most 12 times per map) is policy
  across requests, so it belongs to the roles in slice 3, not to one request's machine; the
  machine reports `retryable()`.
- A test runs host and player against each other with a real valid map through the real
  validator and reassembles it byte for byte.

## Slice 3 notes (2026-10-04)

- The player-side `ask` setting has no prompt until slice 4. With `auto_download` off (the
  default) a guest who lacks the map declines after 1.5 s ("no screen can show the prompt") and
  the game's own missing-map message takes over. To exercise the download now, set
  `"auto_download": true` in `config\host.json` or tick "Download missing maps without asking" on
  the plugin screen.
- Reload and the on-disk search-index cache for the create screen's browser are not done here;
  `mapindex.rs` is the hash index for sharing, with its own cache (`cache\map_hashes.txt`).
- Paths between the game and us are UTF-8; a map path in a legacy code page that is not valid UTF-8
  is not served or hashed. (wc2r-plus falls back to the ANSI code page.)
- Messages for `'M'` wait in a 256-entry inbox and are handled on the next lobby frame, so the
  packet hook never runs map code.
- Not ported: the per-slot status events and the prompt (slice 4), and the leave-the-lobby path
  for an unobtainable map (the game's own missing-map message is used instead).
