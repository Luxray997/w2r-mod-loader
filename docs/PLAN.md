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
Exit: a hello-world plugin prints a chat line on lobby entry and survives a deliberate crash.

### 3. Port wc2r-plus features as first-party plugins
Order: handshake, chat, lobby policy, map download. Fix the known review findings while porting:
per-era tile and REGM validation in the PUD validator, throttled logging, no packet-loop
starvation, no unvalidated UNC paths.
Exit: feature parity, with a regression test per fixed finding.

### 4. Manager library and CLI
Fetch/verify index, download, hash check, atomic install, enable/disable, update, rollback,
`verify`. Safe archive handling (no traversal, size limits, no symlinks).
Exit: `w2rpm install|list|verify` against a test index.

### 5. egui GUI
Browse/search, capability prompt before install, enable/disable, update indicators, game-folder
picker. No logic of its own; it calls `w2rpm-lib`.

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
