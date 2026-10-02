# Phase 1 design: ABI, manifest, index and load-policy specs

Status: draft for review. Scope: the design of the four spec documents Phase 1 produces
(`docs/abi.md` + `w2r_abi.h`, `docs/manifest.md`, `docs/index.md`, `docs/load-policy.md`) and their
test vectors. No host code is written in this phase.

## Goal and success criteria

Define the interfaces everything later depends on, precisely enough that Phase 2 (host) and Phase 4
(manager) can be implemented from the specs alone, and that a plugin author can write a plugin in
any language from the header and docs.

Success:
- Each spec states exact layouts, limits and what the host does with malformed input.
- Manifest and index parsers have valid and invalid test vectors that run on Linux.
- Every rule in `docs/threat-model.md` that touches these interfaces is traceable to a spec section.
- The ABI can grow without breaking existing plugins (add-only).

## Decision: simulation flag in the lobby handshake

The handshake is the wc2r-plus `'H'` message (see wc2r-plus `PROTOCOL.md` section 2). Its 4-byte
"simulation hash" field, always 0 today, becomes a **simulation fingerprint**.

- Fingerprint = first 4 bytes of SHA-256 over the canonical list of
  `(id, version, dll-sha256)`, sorted by `id`, for every **enabled** plugin with
  `affects_simulation: true`. No such plugins: the fingerprint is 0, which is what wc2r-plus sends.
- It detects mismatch; it does not authenticate. A collision causes a missed warning only.
- Capability bit 1 (unused in wc2r-plus) is defined as "host requires matching fingerprints".
- Policy: **warn by default, host-configurable to refuse.** A joiner whose fingerprint differs from
  the host's sees a clear warning. If the host has the strict bit set, the host refuses the joiner.
  Checks apply only between players who announced the handshake; unmodded players are unaffected.
- `docs/threat-model.md` rule 8 is updated to say this ("warned, or refused when the host requires
  matching").

## ABI (`docs/abi.md`, `w2r_abi.h`)

### Entry and versioning

- A plugin DLL exports one symbol: `w2r_plugin_main(const W2rHost *host) -> const W2rPlugin *`.
- `W2rHost` begins with `uint32_t struct_size` and `uint32_t abi_version`. New function pointers
  are only appended. A plugin must check `struct_size` before using a field beyond the version it
  was built against; a host never removes or reorders fields.
- `W2rPlugin` (returned by the plugin) is also size-prefixed: `struct_size`, `on_load`, `on_event`,
  `on_unload`. The host calls only callbacks within the declared `struct_size` and treats null as
  "not implemented".
- `abi_version` is a single integer matching the manifest `abi` field. The host refuses a plugin
  whose `abi` is greater than the host supports (load policy).
- Calling convention `__cdecl` on 32-bit Windows; all types are fixed-width C types, no C++ or
  Rust types cross the boundary.

### Host function table sections

Each section lists its functions in the header and its limits in `abi.md`. All functions return a
`W2rStatus` code; none block on the network (the host performs no network I/O).

| Section | Contents | Capability |
|---|---|---|
| log | levelled, host-throttled log lines | default |
| settings | get/set own settings (typed keys, bounded sizes) | default |
| ui | own settings section and widgets on the host's Nuklear context | default |
| fs | own `data\` folder via host-resolved handles, no raw paths | default (`fs.data`) |
| events | subscribe/unsubscribe by event id | per event |
| game | symbol lookup by name; read through returned handles | `game.read` |
| chat | read incoming chat text/names; send chat as local player | `chat.read`, `chat.write` |
| net | send plugin-namespaced peer messages | `net.send` |
| hooks | register a raw code hook; host reports overlap with existing patches | `hooks.raw` |

A call into a section the plugin was not approved for returns `W2R_E_DENIED` and is logged once.
Capabilities are a consent prompt and guardrail, not a boundary (docs say so).

### Strings, paths and untrusted data

- Strings are `(const char *ptr, uint32_t len)` UTF-8 pairs, not NUL-terminated, with a stated
  maximum per field. The host passes peer-originated text only after replacing anything outside the
  documented character set (printable ASCII for names, as in wc2r-plus).
- Plugins never receive raw peer buffers or filesystem paths from peers. Files are addressed by
  host-issued handles.
- All memory crossing the boundary is owned by the side that allocated it; the host copies inputs
  it keeps and documents the lifetime of every pointer it hands out (valid only during the call
  unless stated).

### Events

Event ids are `uint32_t` and add-only. Initial list (payload structs size-prefixed like the
tables): `lobby_enter`, `lobby_leave`, `lobby_player_join`, `lobby_player_leave`, `chat_received`,
`peer_message` (own subsystem only), `match_start`, `match_end`, `settings_changed`, `shutdown`.
Each event states the thread it fires on (the game's main/UI thread) and its payload limits.

### Peer messages

Reuses the wc2r-plus envelope: `0xE7`, magic `'W' '2' 'R'`, then a **subsystem byte**. The host
reserves first-party subsystems (`'H'`, `'M'`, `'L'`) and assigns each plugin a subsystem id from a
documented range at load. The plugin supplies only the payload. The host enforces, for every
plugin: payload at most 989 bytes minus header, per-plugin and global rate limits, lobby-only
sending, and use of the transport-reported sender slot (never a slot inside a message). Unknown
subsystem ids are dropped before any plugin sees them. A plugin only receives messages for its own
subsystem, from peers who announced the handshake.

### Crash and unload contract

- Every call into a plugin is made from a host guard that stops panics and (on Windows) access
  violations from propagating; a faulting plugin is disabled for the rest of the session and the
  reason is logged. The spec states which guard mechanism Phase 2 must spike first.
- `on_unload` is best effort; the host does not rely on it for cleanup of its own state.
- A plugin must not unwind across the boundary. The Rust SDK catches panics; other languages are
  told to do the equivalent.

## Manifest (`docs/manifest.md`)

The `plugin.json` sketch in `layout.md` is normative; this spec adds field rules:

- `schema` integer; unknown schema versions refused; unknown fields rejected.
- `id`: `[a-z0-9][a-z0-9-]{1,62}[a-z0-9]`, unique, never used in a path unsanitised.
- `version`: SemVer 2.0.0 (no build metadata).
- `entry`: a plain filename ending `.dll` with no path separators, resolved inside the plugin folder.
- `abi`: integer, see ABI versioning.
- `game_builds`: list of exact build strings or `min-max` ranges (four-part numeric versions).
- `capabilities`: subset of the settled set; unknown capability names refused.
- `dependencies`: `{id, version_req}` entries using SemVer requirements; cycles are refused.
- Size limits: manifest at most 64 KiB; string fields length-bounded; list lengths bounded.
- File hashes: the manifest carries no hashes. The DLL hash lives in `state.json` (the pin) and the
  signed index (the package hash). This avoids a manifest that vouches for itself.

## Index (`docs/index.md`)

- `index.json` + `index.json.sig`, minisign-compatible ed25519 over the exact bytes of
  `index.json`. Verify before parsing anything beyond the signature.
- Top level: `schema`, `serial` (monotonic integer), `generated` and `expires` (UTC timestamps),
  `plugins` list. The manager refuses an index with `serial` lower than the cached one and an
  index past `expires`, so a network attacker cannot replay an old index to hide an update. (This
  is an addition to the earlier plan.)
- Per plugin and version: manifest copy for display, `url` (https only), `sha256` and `size` of the
  package, minimum manager version. The manager compares the downloaded package to the hash from
  the signed index and also re-parses the packaged `plugin.json`, refusing on any disagreement.
- Archive rules (consumed by Phase 4, defined here): no path traversal, no symlinks, no absolute
  paths, bounded file count and total/uncompressed size, only expected file names.
- Key handling: the public key is embedded in the manager. Rotation: the index may include a
  `next_key` signed by the current key; documented procedure for an offline key.

## Load policy (`docs/load-policy.md`)

The ordered checks from `layout.md`, each with a machine-readable reason code logged on failure:

1. `state.json` entry exists and `enabled` is true.
2. `plugin.json` parses and validates (`E_MANIFEST`).
3. Game build is in `game_builds` (`E_BUILD`).
4. `abi` is supported (`E_ABI`).
5. DLL SHA-256 equals the pin (`E_HASH`).
6. Manifest capabilities are a subset of `approved_capabilities` (`E_CAPS`).
7. Dependencies are present, enabled, version-compatible and load first (`E_DEP`).
8. Load; the entry point must return a valid `W2rPlugin` (`E_ENTRY`).

Any failure skips that plugin, logs the code and continues. The hash is read once and the DLL is
loaded from the same opened file (or a private copy) to avoid a check-then-swap race; the spec
requires Phase 2 to do so. Also defined here: the simulation fingerprint computation and the
handshake behaviour above.

## Test vectors

`docs/vectors/` holds JSON files, each with expected result and, for invalid cases, the reason code:
- manifests: valid minimal, valid full, each field rule violated, unknown field, oversize, bad schema;
- index: valid, tampered body, wrong key, bad signature encoding, replayed lower `serial`, expired,
  non-https url, hash mismatch metadata;
- simulation fingerprint: empty set, one plugin, order independence.

The throwaway ed25519 test key and signatures are generated by a small script committed with the
vectors; the private test key is clearly marked test-only.

## Security review for this phase

Per `PLAN.md`, a review pass at the end: parser limits, the pin/race rule, the index freshness
rules, and a check that each capability maps to an enforced host function.

## Not in scope

Host implementation, SDK ergonomics, UI layout of the in-game list, the plugin repository CI,
code signing of the manager.
