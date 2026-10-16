# Chat commands

Commands typed into the game's chat, handled by the host (`crates/w2r-core/src/plugincmd.rs`).
They work in the multiplayer lobby and in a match.

## `/plugins`

Replaces the typed text with the list of the plugins the player is running and sends it as an
ordinary chat line, so a host who wants to know can ask, and the player decides whether to answer:

```
Plugins: w2r-chat 1.0.0, w2r-observer 0.2.0
```

- Only plugins that are running are listed, with their versions. Developer-only research plugins
  (`w2r-lobby-log`, `w2r-sdk-probe`) are never named.
- The line is cut at the chat limit (200 bytes) with "+N more". "none" when no plugin runs.
- The command must be on its own (`/plugins`, any case); anything after it is sent as typed.
- Nothing is requested or sent automatically: this is the only way the list leaves the machine.
