# Phase 1 security review

Reviewed against the finished specs (`abi.md`, `manifest.md`, `index.md`, `load-policy.md`) and
`threat-model.md`. Status per question: **OK**, **FIXED** (a spec change made during this review), or
**OPEN** (needs work in a later phase).

1. **Bounds before allocation: OK, one FIXED.** Manifest 64 KiB and index 8 MiB are checked before
   any parsing; the index is size-checked before the signature and verified before JSON parsing
   (`index.md`). Plugin count (1,000), versions (200), dependencies (16), builds (16), strings and
   package size (64 MiB) are bounded; archive rules bound entries, total size and ratio. Nesting
   depth is bounded by the JSON parser (vector and unit tests). FIXED: the signature file had no
   stated cap; `index.md` now caps it at 1,024 bytes.
2. **Identifiers reaching paths: OK.** `id`, `entry` and data file names have path-safe regexes
   (`manifest.md`, `abi.md`); devices, `..`, separators, drive and UNC forms are refused with vectors
   for the manifest fields. Settings keys never name files. Package URLs are never used as paths.
   OPEN (Phase 2): the data-file-name rule in `abi.md` ("not a device name") has no vector yet; the
   host implementation must add one. Map names are a Phase 3 concern.
3. **Capabilities map to enforced functions: OK.** `game.read` -> `symbol_lookup/read`;
   `chat.read` -> chat events and player names; `chat.write` -> `chat_send`; `net.send` ->
   `peer_send` and the peer-message event; `hooks.raw` -> `hook_install/remove`. Every other host
   function is implicit (`log`, `settings`, `ui`, `fs.data`). An unapproved call returns
   `W2R_E_DENIED`.
4. **Index replay, rollback, expiry: OK, with stated limits.** Lower serial and expired indexes are
   refused; an equal serial is accepted; `generated > expires` is refused (vectors). Limits: the
   cached serial lives in the user's profile, so same-user malware can reset it, and a first
   install has no cached serial, so it accepts any validly signed unexpired index (bounded by
   `expires`). A user who sets their clock back can make an expired index look fresh. These follow
   from the same-user limit in the threat model.
5. **Check-then-swap gap: FIXED.** `load-policy.md` hashes from a handle that denies writers and
   deleters and loads the same path before closing it. FIXED during this review: the load now uses
   an absolute path and a restricted DLL search path so a plugin's own dependencies cannot be
   planted in the game folder. Not covered: same-user malware before the open or after the load.
6. **Hostile peers: OK.** Names and chat text are reduced to printable ASCII; the sender slot is
   the transport's; peer payloads are bounded (960 bytes), rate limited, lobby-only and delivered
   only to the plugin whose tag matches. The payload bytes themselves are untrusted and are the
   plugin's responsibility to parse safely (`abi.md` says so for the SDK to reinforce).
7. **Host network access: OK.** No spec requires it and the ABI offers no network function. A
   native plugin can open sockets itself; that is the "not a sandbox" limit, stated in `abi.md`,
   `load-policy.md` and the threat model.
8. **Limits stated plainly: OK.** No sandbox (`abi.md`, `load-policy.md`, threat model); the pin does
   not stop same-user malware (`load-policy.md`, `layout.md`, threat model); a compromised signing key
   breaks the model (`index.md`, threat model). Minisign interoperability is unverified
   (`index.md`, `PLAN.md` Phase 6 exit).

Also noted: the 32-bit header check could not run in this environment (no 32-bit toolchain);
Windows CI must run `tools/check_abi.sh`.
