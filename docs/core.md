# Updating the core mod

The core mod is `version.dll` in the game's `x86` folder (a proxy the game loads). Plugins are
updated through `index.json`; the core is updated through its own file, `core.json`, published
next to the index. It is separate on purpose: the index is parsed strictly (unknown fields are
refused), so a `core` field in it would make every manager built before this change reject the
whole index.

Only the manager can replace the DLL. The game keeps `version.dll` open while it runs, so the
update must happen with the game closed; the manager detects a locked file and says so.

## `core.json`

```json
{
  "schema": 1,
  "serial": 1792035345,
  "version": "0.2.0",
  "url": "https://hastinbe.github.io/w2r-plugins/w2r-core-0.2.0.dll",
  "sha256": "<64 lowercase hex characters of the DLL>",
  "size": 1880064,
  "min_manager": "0.1.0",
  "game_builds": ["1.0.2.2818"]
}
```

Exactly these fields. The package is the DLL itself (no zip).

| Check | Rule | Code |
|---|---|---|
| size | at most 64 KiB | `too_large` |
| JSON | strict rules from [manifest.md](manifest.md) | `not_json`, `duplicate_key` |
| fields | an object with exactly the fields above | `bad_type`, `unknown_field`, `missing_field` |
| `schema` | integer 1 | `bad_schema` |
| `serial` | integer, 0 to 2^53 - 1, not lower than the cached one | `bad_type`, `rollback` |
| `version`, `min_manager` | SemVer; `version` is not a pre-release | `bad_version` |
| `url` | the same rule as index URLs (https, ASCII, no userinfo) | `bad_url` |
| `sha256` | 64 lowercase hex characters | `bad_hash` |
| `size` | integer, 1 to 64 MiB | `bad_type`, `bad_size` |
| `game_builds` | list of at most 16 strings of digits and dots (informational) | `bad_type`, `too_many` |

Trust is the same as for plugins ([index.md](index.md), "Trust"): HTTPS only, the SHA-256 and
size above, and the serial rollback check. Nothing is signed. Whoever controls the Pages site
can ship code that runs in every player's game; that is the accepted trade-off for this project.

## What the manager does

`w2r-loader core update` (and the "Core mod" section of the GUI settings):

1. Resolves the game folder: `--game-dir` / `W2R_GAME_DIR`, else the one remembered in
   `cache\game_dir.txt`, else a few usual install places. A folder is accepted when it, or its
   `x86` child, holds `Warcraft II.exe`. A working folder is remembered.
2. Fetches `core.json` and applies the checks above. A refused file leaves the cache alone.
3. Compares versions. The installed version is read out of the DLL (`W2R-CORE-VERSION:x.y.z;`,
   embedded by `w2r-core/src/entry.rs`). A build from before the marker counts as older. Nothing
   happens when the installed core is the same or newer, or when `min_manager` is above this
   manager.
4. Downloads at most `size` bytes and checks size and SHA-256, then that the bytes are our DLL
   (`MZ` header and the `=== w2rpm host ` marker) and carry exactly the announced version.
5. Refuses when the current `version.dll` is another mod's (`not_ours`: wc2r-plus uses the same
   file name) and when it cannot be opened for writing (`game_running`). The user can choose to
   replace the other mod's file (`w2r-loader core update --replace-other`, or the "Replace it and
   install the mod" button, which the app shows as a warning in Getting started and in the Core
   mod section). The other file is never deleted: it is moved to `version.dll.pre-w2rpm` next to
   it (`.2`, `.3`, ... when that name is taken) and moved back if the install fails.
6. Writes `version.dll.new` next to it, copies the current DLL to `cache\core-rollback\`, then
   renames the new file over `version.dll`. A failure before that rename leaves the old DLL.

`w2r-loader core rollback` swaps the kept DLL back in; run it again to swap forward.
`w2r-loader core status` shows the folder, the installed and the published version. With no
`version.dll` the same `update` command installs it, so a player needs no script.

Reason codes added: `no_game_dir`, `not_ours`, `not_core`, `game_running`, `no_rollback`,
`needs_newer_manager`, `mismatch`.

`game_builds` is shown to the user but not enforced by the manager; the core's own build guard
refuses to run on a game build it was not made for.

## Publishing

1. Raise `version` in `crates/w2r-core/Cargo.toml`.
2. Run `tools\make_core_release.ps1`. It builds the DLL, checks it carries that version and
   writes `w2r-core-<version>.dll` and `core.json` (serial = current Unix time) to
   `target\core-release`.
3. Put both files on the `gh-pages` branch of `w2r-plugins`, next to `index.json`.

Keep old `w2r-core-*.dll` files online; nothing references them, but they cost nothing.
When a core needs a newer manager (an ABI or `Session` change), raise `-MinManager`.
