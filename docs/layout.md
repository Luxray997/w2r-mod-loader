# File layout

All paths are per-user; nothing needs administrator rights.

```
%USERPROFILE%\Saved Games\Warcraft2Remastered\w2r\
    plugins\<plugin-id>\
        plugin.json          manifest
        <plugin>.dll         the plugin binary
        data\                the plugin's own writable data (capability fs.data)
    config\
        state.json           installed plugins: enabled flag, pinned hash, approved capabilities
        host.json            the host's own settings (require_matching_plugins)
        <plugin-id>.json     per-plugin settings
    cache\
        index.json           last verified index
        index.json.sig       its signature
    logs\
        w2r.log
```

The game folder only holds the `version.dll` proxy. Setting the environment variable `W2R_ROOT`
to an absolute path moves the root elsewhere (development and tests); a relative value is
ignored with a warning. The second line of every log says which root is in use and why.

## Decisions

- **Manifest format: JSON** (`plugin.json`). Same format as the index, so one parser family. The
  schema carries a `schema` integer; unknown fields are rejected, unknown future schema versions
  are refused.
- **One state file** (`state.json`) instead of separate enabled/pins files. A plugin cannot be
  "enabled but unpinned" if both facts live in one record, and one atomic write replaces two.
  Writers (manager, in-game toggle) write a temp file then rename; the in-game screen may only
  flip `enabled`.
- **Per-plugin settings stay separate** from `state.json` so a plugin's bad write cannot damage
  the trust record.

### `state.json` rules

Strict JSON as in [manifest.md](manifest.md), at most 1 MiB. Exactly two top-level fields:
`schema` (exactly `1`) and `plugins`, an object keyed by plugin id (id rule from the manifest).
Each value has exactly `version` (SemVer), `enabled` (boolean), `sha256` (64 hex digits, either
case) and `approved_capabilities` (duplicate-free subset of the five approvable capabilities).
Any violation refuses the whole file and the host loads nothing. `crates/w2r-core/src/state.rs`
is the implementation.

### `host.json` rules

Strict JSON, at most 64 KiB, exactly `schema` (exactly `1`) and the known fields, each a
boolean: `require_matching_plugins` (default `false`; the handshake's strict bit, see
[load-policy.md](load-policy.md)). A missing file is the defaults; a bad file is logged and the
defaults are used until the user changes a setting, which rewrites it (temp file, then rename).
`crates/w2r-core/src/hostcfg.rs` is the implementation.

### `state.json` sketch

```json
{
  "schema": 1,
  "plugins": {
    "example-plugin": {
      "version": "1.2.0",
      "enabled": true,
      "sha256": "<hex of the plugin DLL>",
      "approved_capabilities": ["game.read", "chat.read"]
    }
  }
}
```

### `plugin.json` sketch

```json
{
  "schema": 1,
  "id": "example-plugin",
  "name": "Example Plugin",
  "version": "1.2.0",
  "author": "someone",
  "description": "One sentence.",
  "entry": "example_plugin.dll",
  "abi": 1,
  "game_builds": ["1.0.2.2818"],
  "capabilities": ["game.read", "chat.read"],
  "affects_simulation": false,
  "dependencies": []
}
```

## Capabilities

Granted by default (no prompt): `log`, `settings` (own settings only), `ui` (own settings
section / widgets), `fs.data` (own `data\` folder only).

Require user approval at install; re-asked whenever an update adds one:

| Capability | Meaning |
|---|---|
| `game.read` | read game state through the host's symbol API |
| `chat.read` | see chat text and player names from others |
| `chat.write` | send chat messages as the local player |
| `net.send` | send plugin-namespaced peer messages through the host |
| `hooks.raw` | install raw code hooks / patches (shown as **dangerous**) |

`affects_simulation: true` is not a capability but is always shown prominently, and such plugins
are included in the lobby compatibility hash.

An update that only removes capabilities needs no new prompt.

## Load decision (host, at startup)

For each plugin in `state.json` with `enabled: true`: manifest parses -> game build in range ->
ABI version supported -> DLL SHA-256 equals the pinned hash -> manifest capabilities are a subset
of `approved_capabilities` -> load. Any failure: skip the plugin, log the reason, continue.

## What the pin does and does not protect

It catches corrupted, half-written, or out-of-band-modified files and an index/package mismatch,
for the `entry` DLL; helper DLLs the plugin imports are not hashed in-game (see
[load-policy.md](load-policy.md)). It does **not** stop malware running as the same user, which
can edit `state.json` too. The threat model states this.
