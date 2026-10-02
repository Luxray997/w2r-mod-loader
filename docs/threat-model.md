# Threat model (draft)

## Assets
- The player's machine and account (plugins are native code in the game process).
- The integrity of multiplayer matches (simulation must stay in sync).
- The plugin index and release signing key.

## Actors
- **Hostile peer / host in a lobby:** sends peer messages, chat, map files, names.
- **Malicious or compromised plugin author:** publishes a plugin or a malicious update.
- **Network attacker:** tampers with the index or package downloads.
- **Local unprivileged process:** tries to swap plugin files or abuse predictable paths.

## Rules
1. The in-game host performs no network I/O.
2. All data from peers is bounded and sanitised by the host before a plugin sees it. Plugins get
   safe string/path types, not raw buffers, wherever possible.
3. Plugins send peer messages only through the host, which enforces size, rate, lobby-only and
   sender-slot rules.
4. The index is signed (ed25519); packages are verified by SHA-256 from the signed index.
5. A plugin loads only if enabled, hash-pinned, and its capabilities were approved by the user.
   Hashes are rechecked at load, so a swapped DLL is refused.
6. Plugins and the manager install per-user. Nothing requires administrator rights, and nothing
   adds antivirus exclusions.
7. Archives are extracted with traversal, symlink and size checks.
8. Plugins that change the simulation are flagged and their combined hash is exchanged in the
   lobby; mismatched players are warned or refused.

## Out of scope / known limits
- Hash pinning does not protect against malware running as the same user: it can edit
  `state.json` as well. The pin catches corruption, out-of-band file changes and
  index/package mismatches.
- Native plugins can do anything in-process. Capabilities are consent and guardrails, not a
  sandbox. Users must be told this plainly.
- We cannot audit the game's own map/packet parsers.
- A compromised signing key breaks the model; keep it offline and document rotation.

## Lessons carried over from reviews of existing Remastered mods
- Unsanitised remote filenames into `Path.Combine` -> path traversal.
- Memory-scanned strings used as file paths -> UNC / NTLM leak.
- Always-elevated installers and name-based AV exclusions.
- Unthrottled logging of attacker-triggerable events.
- Packet loops that drop the game's own traffic under flood.
