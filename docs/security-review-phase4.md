# Security review: Phase 4 (manager library and CLI)

Reviewed 2026-10-04 by reading `w2r-loader-lib` and `w2r-loader-cli` against [threat-model.md](threat-model.md).
One pass over four areas. Same-user malware is out of scope (it can edit `state.json` and the DLLs
directly), as the threat model says.

## Fixed

1. **A missing clock turned off index expiry.** The CLI used `0` when `SystemTime` was before 1970,
   so every index looked unexpired. Now `no_clock` is an error.
2. **Rollback trusted a parked manifest's hash but not its identity.** `rollback` re-hashed the
   parked DLL but never checked that the parked `plugin.json` named the same id and version as the
   recorded state. It now does (`bad_manifest`); test
   `rollback_refuses_a_parked_manifest_for_another_version`.
3. **Index serial written last.** A crash between writing the cache and the serial file could leave
   an older serial and weaken the replay check. The serial is written first.

## Checked, no change needed

- **Archive extraction.** Only four names are ever written (`plugin.json`, the entry DLL,
  `README.md`, `LICENSE`), each matched exactly, so an archive name never becomes a path. The entry
  name must end `.dll`, so it cannot equal the other three. Directories, symlinks, alternate data
  streams, drive and UNC forms fail the allowlist. Declared sizes are never trusted: each file is
  read through `take(size + 1)` and compared, the ratio and total limits run before the read, and
  duplicate names are caught against the directory header because the zip reader merges them.
- **Fetch.** `https` only, also across redirects (3 at most), 60 s overall, body limited to the
  declared size (package) or 8 MiB / 4 KiB (index, signature). The package is hashed against the
  signed index before the archive is opened.
- **Install swap.** The id is checked against the manifest id rule before it is joined to a path.
  Folders are renamed within one volume; failures before the state write restore them.
  A crash after the rename leaves a folder the host refuses (pin mismatch), never one it runs.
- **Removal.** `remove_dir_all` does not follow junctions on current Rust, so a junction inside a
  plugin folder is removed as a link, not emptied.
- **The hidden `--index-dir` flag** only replaces the transport. Signature, URL rules and hashes
  still apply.
- **`dev-sign`** is a dev-dependency of the CLI only; no release build contains the test key.

## Documented, not fixed

- No lock between two running managers.
- The cache folder can be deleted by the same user to forget the highest serial (replay then needs
  a signed old index, which only a compromised key or a stale mirror provides).
- Key rotation is announced (`next_key`) but never switches keys by itself: a manager release must
  embed the new key (`index.md`). A manager can trust several keys at once (`--key` repeated).
