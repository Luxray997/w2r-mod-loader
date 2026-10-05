# w2rpm plan

Plugin host and plugin manager for Warcraft II Remastered. Everything we write is Rust; the plugin
interface is a C ABI so plugin authors can use any language. UI: the game's own Nuklear context
in-game, egui for the desktop manager.

## Repository layout

One Cargo workspace for the code; a separate repo (`w2r-plugins`) for the signed plugin index.

```
w2rpm/
  crates/
    w2r-abi/      C header + Rust bindings for the plugin interface
    w2r-sdk/      ergonomic Rust SDK for plugin authors
    w2r-core/     in-game host (32-bit cdylib, version.dll proxy)
    w2rpm-lib/    manager logic: index, download, verify, install
    w2rpm-cli/    CLI over w2rpm-lib
    w2rpm-gui/    egui app over w2rpm-lib
  docs/           ABI spec, manifest spec, threat model, layout
```

## Principles

- The in-game host never touches the network. Only the manager downloads.
- A plugin loads only if it is enabled, its hash is pinned, and its capabilities are approved.
- Everything that parses untrusted data (manifests, index, archives, peer messages, map files) is
  safe Rust with fuzz targets.
- Capabilities are a consent prompt and a guardrail, not a security boundary. Native plugins run
  in-process. The docs must say so.

## Phases

### 0. Decisions (done)
Threat model, file locations, capability set. Settled: JSON manifest (`plugin.json`), a single
`state.json` for enabled/pinned/approved state, least-privilege capabilities (see `docs/layout.md`).
Settled in Phase 1: the handshake's simulation field is a fingerprint; warn by default, and the
host may require matching.
Exit: `docs/threat-model.md`, `docs/layout.md`.

### 1. Specs
Status: written (`docs/abi.md`, `manifest.md`, `index.md`, `load-policy.md`); vectors in
`docs/vectors/`; run `bash tools/run_all.sh`.
- Plugin ABI (C header, add-only): entry point, `Host` function table (log, settings, events, UI
  widgets, peer messages, symbol lookup, hook registry), event list.
- Plugin manifest (`plugin.json`): id, version, author, game-build range, capabilities,
  `affects_simulation`, dependencies, file hashes.
- Index format: signed JSON, ed25519 (minisign-compatible), SHA-256 per package.
- Host load policy.
Exit: specs in `docs/`, test vectors for the manifest and index parsers.

### 2. Core host (`w2r-core`)
Status: in progress. Done: workspace, `w2r-abi` bindings, the crash guard (step 4 spike:
SEH via `microseh` outside, `catch_unwind` inside, `guard.rs`), the pinned DLL loader with
checks 9-10 (`loader.rs`), the registry/dispatcher that disables a faulting plugin
(`dispatch.rs`), and the host table with `log`/`subscribe`/`unsubscribe` (`host.rs`; the
rest return `E_UNSUPPORTED` until their step). `crates/test-plugins` holds fixture DLLs;
`tests/load_dll.rs` loads a real crashing plugin and survives. Step 1 is ported: `proxy.rs`
(17 naked forwarders, exported by rustc), `build_guard.rs` + `target/signatures.rs` (197
signatures generated from wc2r-plus by `tools/gen_signatures.py`), `log.rs`, `paths.rs`,
`entry.rs` (`DllMain` → startup thread → log + guard); `tests/proxy_dll.rs` loads the built
DLL like the game does. Build and test the host with `--target i686-pc-windows-msvc`.
1. `version.dll` proxy, build guard, logging (port from wc2r-plus). **Done.**
2. Symbol table: named game functions/globals per detected build. **Done:** `target/symbols.rs`
   (320 entries generated from wc2r-plus `addresses.h`), `symbols.rs`, `symbol_lookup`/
   `symbol_read` behind `game.read`.
3. Hook registry with patch-overlap detection. **Done:** `hooks.rs` over MinHook; `hook_install`
   takes a function-symbol handle (`abi.md`); a disabled plugin loses its hooks; `frame.rs`
   hooks `BeginNamedWindow` as the host's main-thread entry.
4. Event dispatch with crash isolation. Panics must not cross FFI. Access violations need SEH or a
   vectored exception handler (`microseh` or a tiny C shim). **Done:** `guard.rs`, `dispatch.rs`
   (disables a faulting plugin, removes its hooks), `tests/load_dll.rs` (real crashing DLL).
   Startup loading is in: `json.rs`/`manifest.rs`/`state.rs` (checks 1-8 in `plugins.rs`, with
   dependency order and cycles; `tests/load_all.rs`), `session.rs` runs it on the first frame,
   `lobby.rs` turns the lobby window into `LOBBY_ENTER`/`LEAVE`, and `chat.rs` implements
   `chat_send` (lobby chat input + send, rate-limited). The hello-world fixture says hello on
   lobby entry. Verified in the live game on 2026-10-03: guard 197/197 at a relocated base,
   plugins loaded on the main thread, crash-plugin disabled on its fault, hello-plugin's
   `chat_send` returned OK.
5. Lobby message service: size, rate and lobby-only limits for every plugin; per-plugin
   subsystem ids. **Done:** `peer.rs` (envelope, tag routing, 32/s per plugin and 128/s
   overall, lobby-only, target must have announced; `GetNextPeerPacket` hook consumes W2R
   messages and drops the stock 0x2A file transfer), `Registry::dispatch_peer`, `peer_send`.
   The roster of announced peers is empty until the Phase 3 handshake port fills it, so no
   message flows in the live game yet.
6. Widget API on the game's Nuklear context; in-game plugin list with enable/disable toggles.
   **Done, verified in the live game on 2026-10-03** (button, screen, both toggles writing
   `state.json`, the fixture's checkbox, slider and button): `ui.rs` (`W2rUi` over a `Widgets` trait, one
   row per call; the plugin screen), `options.rs` (a "Plugins" button over the options screen
   opens our screen, drawn with the game's menu widgets and skins), toggles rewrite
   `state.json` atomically for the next launch, slow callbacks (>50 ms, 10 in a row) are
   logged and shown.
7. End-of-phase security review. **Done (2026-10-03),** three passes (FFI boundary, untrusted
   parsers, filesystem/startup). Fixed: JSON nesting refused before recursing (a deep
   `plugin.json` could overflow the main thread's stack); manifests and `state.json` read
   through a size-bounded reader (a sparse multi-GB file aborted on allocation); string and
   duplicate-key parsing made linear; `\u+041` refused like the reference; a plugin now gets the
   capabilities its manifest declares, not the whole approval list; every host function checks
   the token (magic), the main thread and that the plugin is still active; the hook registry
   lock is no longer held across a write to plugin memory, and inline hooks reserve the bytes
   MinHook relocates; `log` is throttled as `abi.md` promised; refused `chat_send`/`peer_send`
   cost no rate budget; inbound peer messages are lobby-only and rate-limited per sender; the
   session is built outside its `RefCell` borrow and game widget calls run under their own
   guard, so a fault under a host hook is logged instead of silently disabling the host; a
   `W2rUi` kept past `on_ui` is inert; the log is fully shared (second instance, tail viewers)
   with a per-process fallback, filters bidi/separator characters, and names the root in use;
   `W2R_ROOT` must be absolute; `install_dev.ps1` backs up `state.json` and only removes a
   `version.dll` that is ours. Documented, not fixed: the pin covers the entry DLL only, and a
   junction in the plugin path is inside the same-user exclusion (`load-policy.md`).
Exit: a hello-world plugin prints a chat line on lobby entry and survives a deliberate crash.

### 3. Port wc2r-plus features as first-party plugins
Status: in progress. Order: handshake, chat, lobby policy, map download. Fix the known review
findings while porting: per-era tile and REGM validation in the PUD validator, throttled
logging, no packet-loop starvation, no unvalidated UNC paths.
Decision (handshake step): features that own a reserved subsystem (`'H'`, `'M'`, `'L'`) are
compiled into the host, not shipped as plugins. The handshake's roster gates `peer_send`, its
fingerprint covers what the host loaded, and refusing a player is a host action; none of that
fits behind the plugin ABI, and the host must work with no plugin installed. Chat, lobby
policy and map download decide at their turn whether they are host features or first-party
plugins over the public ABI (chat may well be the latter).
1. Handshake. **Done (code and unit tests, 2026-10-03; not yet run in the live game):**
   `handshake.rs` (wire-compatible with wc2r-plus, docs/peer-protocol.md; nonce-change and
   caps-change log throttles with a regression test; roster from the transport's peer list,
   re-HELLO on joiners, leavers forgotten), `fingerprint.rs` (checked against
   docs/vectors/fingerprint.json; `plugins.rs` computes it over loaded `affects_simulation` or
   `hooks.raw` plugins and logs it), `hostcfg.rs` (`config\host.json`,
   `require_matching_plugins`), the strict kick through `KickLobbyPeer`, lobby warnings listed
   on the plugin screen next to a strict-mode checkbox and logged. **The in-lobby banner is parked:**
   a Nuklear window begun from an `EndCurrentWindow` hook froze the game's lobby (2026-10-04; even
   an empty window did; the hang is in the game's own combo code, not ours). wc2r-plus never
   draws one either; it uses the game's own `NKPopup`. Follow-up step: show the warning through
   `GameListPopup` (see wc2r-plus `features/ui/game_list_message.cpp`). `watchdog.rs` logs the
   last host stage and writes a minidump when the main thread stops reaching our hooks. `session.rs` now queues
   messages that arrive off the main thread or while the session is busy (bounded, 256) and
   drains them on the next frame, closing the Phase 2 gap. `Transport` gained `is_host`,
   `peers` (EnumPeers vtable slot, freed through the game's sized `operator delete`),
   `lobby_slots` and `kick`. To verify in the game: lobby entry logs `announced (nonce …)`;
   with a second player (wc2r-plus or another w2rpm) the log shows `net slot N … runs …` and
   `replied`; `tools\install_dev.ps1 -Simulation` gives a non-zero fingerprint so a mismatch
   warning can be seen.
2. Chat, a first-party plugin (`crates/chat-plugin`, id `w2r-chat`, spec [chat-plugin.md](chat-plugin.md)).
   **In progress (2026-10-04).** Host prerequisites done and verified in the game: `symbol_address`
   (`hooks.raw`), `settings_*` and `data_*` (`store.rs`), `W2R_EVT_MATCH_START` (`matchstart.rs`);
   `MATCH_END` stays reserved. Plugin slices done and verified in the game: 1 scrollable history
   plus the paused-game HUD (`BuildInMatchHud` replaced, from wc2r-plus `in_match_hud.cpp`), 2 pause
   announcements, timestamps and the per-launch disk log, 4 player-name colours. Slice 3 (channels) done and verified. Slice 5 (click
   closes chat, the `click_unfocus` setting) written and installed, **awaiting the in-game check**;
   the `players:` diagnostic is removed. Known limit: the host's data
   folder cap (64 files, 16 MiB) stops the disk log after about 64 launches with logging on.
3. Lobby policy: a host feature, spec in [lobby-policy.md](lobby-policy.md) (four slices, the last
   is the Create Multiplayer Game screen). **Slice 1 written 2026-10-04 (`lobbypolicy.rs`: wire format, state, the
   two request hooks, `lock_team_changes`/`lock_slot_swaps` in `host.json` and two plugin-screen
   checkboxes), awaiting the in-game check), slice 2 written (host broadcast on change and every 2 s,
   guest adoption, disabled team/name dropdowns on guests, lock events in the log), also awaiting
   the check; slice 3 written (host "Move to Slot N" menu, Closed slots, on the same
   `LobbySlotDropdown` hook), awaiting the check; slice 4a written (`lobbycreate.rs`: Teams/Slots lock buttons over the stock Create
   Multiplayer Game screen), awaiting the check; 4a verified. 4b (full replacement screen with the map browser and preview) is specified
   in [create-screen.md](create-screen.md), four slices; slice 1 (`screenkit.rs`, `createscreen.rs`,
   the `enhanced_create_screen` setting, off by default) verified; slice 2 (`mapbrowser.rs`,
   `browserview.rs`: folders, up, reload, size filter, search, publish; no disk index cache yet)
   verified; slice 3 (`pud.rs`, `mappreview.rs`, `previewdraw.rs`: bounds-checked PUD reader,
   tileset-colour preview painted into a game texture, shown on the screen and in the browser)
   verified, and slice 4 is done: `enhanced_create_screen` defaults to on, the 4a overlay was removed (the
   lock buttons exist only on the replacement screen and the plugin screen). Lobby policy slices 1 to 3 (locks, broadcast,
   host menus) still await a second-player check.**
4. Map download: a host feature, spec in [map-download.md](map-download.md) (four slices: pure core,
   state machines, disk and game glue, prompt and status). **Spec written 2026-10-04. Slices 1 and 2 done
   (`mapname.rs`, `mapwire.rs`, `pudvalidate.rs`, `maplimits.rs`, `downloadhost.rs`,
   `downloadclient.rs`: pure, 50 tests including a host-to-player run end to end). Slice 3 written:
   `mapstore.rs`, `mapindex.rs`, `maproles.rs` (tested with a fake game), `mapdownload.rs` (the
   three hooks, the `'M'` inbox, the lobby-frame work), `map_sharing` and `auto_download` in
   `host.json` with plugin-screen checkboxes, capability bit 2 in the handshake. Slice 4 written: the
   "Download <map> (N KB)? Ns" button after the stock Leave button (the stock button is the "no"),
   per-slot status lines in `w2r.log`. **Slices 3 and 4 await a two-player in-game check.**
5. Multiplayer lobby screen: a host feature, spec in [lobby-screen.md](lobby-screen.md) (four slices: static screen, right column, marks, download visuals and notices). **Spec written 2026-10-04; slice 1 committed (a4ed70f), awaiting the in-game check; slices 1 and 2 and 3 committed (a4ed70f, ac23d2c, df9fd0d), slice 4 written not installed; none checked in game yet.**
Exit: feature parity, with a regression test per fixed finding.

### 4. Manager library and CLI
Status: written and tested (2026-10-04), spec [manager.md](manager.md). `w2r-formats` (json, semver,
manifest, state) was extracted from `w2r-core` so host and manager share one parser; `w2rpm-lib`
(minisign, index, package, install, store, fetch, catalog) and `w2rpm-cli` (`w2rpm`) sit on top.
Also done: install-time dependency/ABI warnings, multi-key trust with a rotation notice, and the
security review ([security-review-phase4.md](security-review-phase4.md), three fixes). Not done: an
embedded release key and default index URL (Phase 6).
Fetch/verify index, download, hash check, atomic install, enable/disable, update, rollback,
`verify`. Safe archive handling (no traversal, size limits, no symlinks).
Exit: `w2rpm install|list|verify` against a test index.

### 5. egui GUI
Status: written and tested (2026-10-04); window not yet clicked through by a person. Browse/search,
capability prompt before install (`hooks.raw` needs its own tick), enable/disable, update
indicators and update button, rollback, remove (optional data purge), verify, game-folder and
key-file pickers. No logic of its own; it calls `w2rpm-lib`. The code both front ends needed
(root, keys, fetcher, clock, preflight, install) moved from the CLI into `w2rpm-lib::session`.
`crates/w2rpm-gui`: `jobs.rs` (job sequencing and the `View` snapshot, UI-free), `settings.rs`
(`%APPDATA%\w2rpm\gui.json`), `app.rs` (egui); `tests/jobs.rs` runs the whole cycle against a
signed test repo. Still needs the Phase 6 key and index URL (entered in Settings until then).

### 6. Plugin repository and CI
`w2r-plugins` repo: source + manifest per plugin, CI builds binaries and publishes hashes, PR
review before listing, offline release-signing key with a documented rotation process.
Exit: one third-party-style plugin goes through the whole pipeline, and the index signature is
verified against the real `minisign` binary (the Phase 1 reference implementation was written from
the format description).

## Cross-cutting
- Tests: unit tests for parsers/validators on Linux; `cargo-fuzz` targets for manifest, index,
  archive unpacking, wire decoders; in-game integration tests need Windows.
- CI: Windows runners for 32-bit builds; Linux for library/CLI tests.
- A security review pass at the end of every phase.

## Biggest risks
1. Crash isolation in Rust (Phase 2).
2. Game updates break the symbol table and Nuklear offsets (mitigation: build guard, one place to fix).
3. Native plugins are not sandboxed.
4. Scope: Phases 3 and 5 are large. Ship 0-2 plus the CLI first.
