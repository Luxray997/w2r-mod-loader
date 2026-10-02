# Host load policy

At startup the in-game host decides, for each plugin, whether to load it. The inputs are the
user's `state.json`, the plugin's `plugin.json` and DLL, and the running game build (see
[layout.md](layout.md)). Capabilities are a consent prompt and a guardrail, not a sandbox: a
loaded native plugin runs in the game process and can do anything that process can.

## Decision

For each plugin with `enabled: true` in `state.json`, run the checks below in order. The first
failure skips that plugin, logs one line `plugin <id>: <code>: <detail>` and continues with the
next plugin. A failing plugin never stops the game or other plugins.

| # | Code | The plugin is skipped when |
|---|---|---|
| 1 | `E_STATE` | `plugin.json` is missing, or the plugin's folder name differs from the manifest `id` |
| 2 | `E_MANIFEST` | `plugin.json` fails [manifest.md](manifest.md) (log the specific manifest reason code) |
| 3 | `E_BUILD` | the running game build is covered by no `game_builds` entry |
| 4 | `E_ABI` | the manifest `abi` is greater than the host's `W2R_ABI_VERSION` |
| 5 | `E_HASH` | the SHA-256 of the DLL differs from `state.json` `sha256` |
| 6 | `E_CAPS` | manifest `capabilities` is not a subset of `approved_capabilities` |
| 7 | `E_TAG` | an already-accepted plugin has the same 4-byte peer tag (the later plugin by id order is refused) |
| 8 | `E_DEP` | a dependency is missing, disabled, failed an earlier check, does not satisfy `version_req`, or is part of a dependency cycle (every member of the cycle fails) |
| 9 | `E_ENTRY` | the DLL has no `w2r_plugin_main`, it returns null or a `W2rPlugin` whose `struct_size` is below the version-1 minimum, or the call faults |
| 10 | `E_LOAD` | `on_load` returns a non-zero status |

Details:

- **Build matching.** A build is `a.b.c.d`. An entry matches when it is equal to the running build,
  or when it is a range and min <= build <= max, compared numerically component by component.
- **Order.** Plugins load in dependency order; ties are broken by plugin id (bytewise). Checks 1 to
  7 run for every enabled plugin first, so check 8 can see which dependencies survived.
- **Peer tag.** `tag = first 4 bytes of SHA-256(UTF-8 plugin id)` (see [abi.md](abi.md)).
- **No live changes.** `state.json` is read at startup only. The in-game toggle may flip `enabled`
  and takes effect at the next launch. There is no live load or unload in version 1.

## No check-then-swap gap

Hashing the DLL and then loading it by path leaves a window in which the file could be replaced.
The host closes it:

1. Open the DLL with sharing that denies writers and deleters (`FILE_SHARE_READ` only).
2. Hash the file from that handle and compare it with the pin.
3. Call `LoadLibrary` on the same path while the handle is still open, then close it.

The load uses an absolute path and `LoadLibraryExW` with `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR |
LOAD_LIBRARY_SEARCH_SYSTEM32`, so a DLL the plugin itself depends on is looked up only in the
plugin's own folder and the system folder, never the game folder or the current directory.
Writers cannot touch the file between the hash and the load. This does not protect against malware
running as the same user, which can edit `state.json` as well, or act before the open or after the
load. The pin catches corrupted, half-written and out-of-band-modified files and an
index/package mismatch; the [threat model](threat-model.md) says what it does not cover.

## Simulation fingerprint

Plugins with `affects_simulation: true` change what the game simulates, so players must agree on
them.

- **Input:** every plugin that is *loaded this session* and has `affects_simulation: true`, as
  `(id, version, dll sha256)`.
- **Encoding:** sort by `id` (bytewise UTF-8). For each plugin append `id`, `0x00`, `version`,
  `0x00`, then the 32 raw bytes of the DLL SHA-256. Hash the concatenation with SHA-256 and keep the
  first 4 bytes. The empty set gives `00 00 00 00` without hashing. Duplicate ids cannot occur.
- **On the wire:** the 4 bytes go, in that order, into the handshake's 4-byte "simulation" field at
  offset 15 (the little-endian u32 of those bytes). wc2r-plus sends zeros, which equals "no
  simulation-affecting plugins".
- **Strict bit:** handshake capability bit 1, unused so far, means "this host requires matching
  fingerprints".
- **Policy:** a player whose fingerprint differs from the host's sees a clear warning in the lobby.
  When the host has the strict bit set, the host refuses that player. The check applies only
  between players who announced the handshake; players without the mod are never affected.
- The fingerprint only detects mismatch. It does not authenticate anyone, and a collision at worst
  misses a warning.

Test vectors: `docs/vectors/fingerprint.json` (fingerprints, order independence, duplicate-id
refusal and peer tags). Run with `bash tools/run_all.sh`.
