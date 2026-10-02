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
1. `version.dll` proxy, build guard, logging (port from wc2r-plus).
2. Symbol table: named game functions/globals per detected build.
3. Hook registry with patch-overlap detection.
4. Event dispatch with crash isolation. Panics must not cross FFI. Access violations need SEH or a
   vectored exception handler (`microseh` or a tiny C shim). **Spike this first.**
5. Lobby message service: size, rate and lobby-only limits for every plugin; per-plugin
   subsystem ids.
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
