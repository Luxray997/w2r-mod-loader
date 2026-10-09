# Manager library and CLI (Phase 4)

`w2rpm-lib` holds all manager logic; `w2rpm-cli` (`w2rpm`) and the egui app (`w2rpm-gui`, Phase 5) are thin callers; what both need before any command (root, fetcher, preflight, install) is `session.rs`.
The in-game host never links it. Formats are shared with the host through `w2r-formats` (json, semver,
`plugin.json`, `state.json`), so the two cannot disagree about what is valid.

Normative inputs: [index.md](index.md) (index rules, package verification, archive
rules), [manifest.md](manifest.md), [layout.md](layout.md) (paths, `state.json`),
[threat-model.md](threat-model.md).

## Status

All five slices are written and tested (2026-10-04); the exit criterion is met by
`crates/w2rpm-cli/tests/cli.rs`, which runs the real binary against a test repository.

| Slice | Module | What it does |
|---|---|---|
| 1 | `w2r-formats`, `index.rs` | shared parsers; index rules; every case of `docs/vectors/index.json` gives the reference result |
| 2 | `package.rs` | size and SHA-256 against the index entry, then archive rules, packaged `plugin.json` equals the index copy |
| 3 | `install.rs`, `store.rs` | atomic install/update, rollback, remove, enable/disable, `verify`, `state.json` access |
| 4 | `fetch.rs`, `catalog.rs` | HTTPS fetch, index cache with rollback protection, version selection, update list |
| 5 | `w2rpm-cli` | the commands below |

## Using it

```
w2rpm --index-url https://.../index.json <command>
```

`--root` (or `W2R_ROOT`, absolute) moves the w2r folder; `W2RPM_INDEX_URL` is the environment form of `--index-url`. Commands:

- `list`: installed plugins, `(update available: x)` from the cached index only (no network).
- `search [term]`, `refresh`: read or refresh the index. `install`, `update` and `search` use the
  cached index while it still parses and fetch a fresh one otherwise.
- `install id[@version]`: shows name, author, description, whether it affects the simulation and
  every capability (`[new]` for the ones you have not approved yet), then asks. `--yes` accepts the
  list but not `hooks.raw`; that also needs `--allow-hooks-raw`. Without a version it takes the
  highest non-pre-release this manager can install (`min_manager`).
- `update [id]`: installs the newer version; only newly added capabilities are asked about; an
  update that removes capabilities shrinks the approval without a prompt.
- `remove id [--purge]`, `enable id`, `disable id`, `rollback id`, `verify` (exit code 1 on any
  finding).

The default index URL is `session::DEFAULT_INDEX_URL` (GitHub Pages of `w2r-plugins`);
`--index-url` overrides it. `--index-dir DIR` (hidden) serves the index and packages from a folder
for tests; the URL rules and package hashes are still enforced.

## Decisions

- Library functions take the root path and a clock value; no hidden globals, so tests run in a
  temp directory.
- The manager is the only writer of the pinned `sha256` and `approved_capabilities`; the in-game
  screen only flips `enabled` (layout.md).
- **Install order.** Extract into `cache\staging\<id>`; move the old `data\` into it; park the
  current folder as `cache\rollback\<id>\plugin` with a one-entry `state.json`; rename the staged
  folder into `plugins\<id>`; write `state.json` (temp file, rename). Any failure before the state
  write puts the folders back. A crash between the rename and the state write leaves a folder whose
  DLL no longer matches the old pin: the host refuses it, never runs it, and `verify` reports it.
- **Rollback** swaps the live and parked versions (twice returns to the start), re-checks the
  parked DLL against its pin first, and keeps the current `enabled` flag. Only one version is
  parked; a new install replaces it.
- A `state.json` that does not parse is never rewritten (`bad_state`): the host would load
  nothing from it, and rewriting would hide the cause.
- Removing a plugin drops it from `state.json` first, then deletes files. `data\` is kept unless
  `--purge`; a data-only folder is not reported by `verify`.
- The index cache is re-verified on every load, and `cache\index.serial` remembers the highest
  serial accepted, so deleting the cache folder is the only way to forget it (same-user malware can
  do that; the threat model already scopes it out).

## Reason codes of this layer

On top of the index and manifest codes: `size_mismatch`, `hash_mismatch`, `bad_archive`,
`bad_entry_name` (anything but `plugin.json`, the entry DLL, `README.md`, `LICENSE`, or a
directory/symlink), `collision` (two entries with one name; checked against the directory header
because the zip reader merges them), `too_many` (over 64 entries), `too_large` (over 128 MiB
expanded), `ratio` (over 100:1), `missing_file`, `manifest_mismatch`, `bad_manifest`,
`approval_needed`, `manager_too_old`, `not_installed`, `no_rollback`, `bad_state`, `no_index`,
`network`, `io`.

## Known limits

- No lock between two running managers; run one at a time.
- Dependencies, ABI and game build are enforced only by the host at load (`load-policy.md`). At
  install the CLI prints `WARNING:` lines for a newer ABI than this manager speaks and for
  dependencies that are missing, disabled or outside `version_req` (`check.rs`), and lists the
  game builds the plugin targets (it cannot know which build is installed). Warnings never block.
- Installing or removing while the game runs fails on Windows (the DLL is open); the error says so.
- The `dev-repo` feature builds test repositories and must never be enabled in a release build.
- The index is not signed (see [index.md](index.md), "Trust"): integrity comes from HTTPS and the
  SHA-256 in each entry.
