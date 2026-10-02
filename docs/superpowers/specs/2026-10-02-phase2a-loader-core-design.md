# Phase 2a design: loader core

Status: draft for review. Implements the first slice of Phase 2 in `docs/PLAN.md`. Builds on the
Phase 1 specs (`docs/abi.md`, `manifest.md`, `index.md`, `load-policy.md`) and the crash-guard spike.

## Goal and success criteria

A 32-bit Windows DLL that sits in the game as `version.dll`, applies the load policy to the user's
installed plugins, loads the ones that pass, delivers events to them, and survives a plugin that
crashes. Everything that does not need the game's internals.

Done when, all verified by tests that run in CI:
- The Rust manifest parser and fingerprint pass every case in `docs/vectors/manifest.json` and
  `fingerprint.json`; a new `state.json` parser passes its own vectors.
- Every load-policy reason code (`E_STATE` ... `E_LOAD`) has a test, including dependency order,
  failure propagation and tag collision, on Linux against a fake platform.
- Under Wine, a harness loads a hello-world plugin and a deliberately crashing plugin: the crasher is
  disabled and logged (access violation, stack overflow repeated, divide by zero, illegal
  instruction), the hello plugin keeps receiving events, and a DLL changed after its hash was pinned
  is refused.
- A real-Windows run of the same harness is recorded as an outstanding manual check (Wine is not
  Windows).

## Out of scope for 2a

Symbol table, byte-signature build guard and hook installation (2b); lobby messages, handshake and
fingerprint exchange (2c); in-game UI and the enable/disable toggle (2d); the manager (Phase 4).
The ABI functions for those return `W2R_E_UNSUPPORTED` in 2a. The "hello plugin prints a chat line
on lobby entry" exit criterion of Phase 2 is met at the end of Phase 2; in 2a the plugin logs on a
`lobby_enter` event that the harness injects.

## Decisions

- **Pure Rust plus one C file.** The only C is `guard.c` (about 40 lines): the vectored exception
  handler and `__builtin_setjmp/longjmp` recovery from the spike. Rust cannot express it safely.
- **Target `i686-pc-windows-gnu`** for the host DLL and harness (the toolchain installed here, and
  what Wine runs). Plugins may use any toolchain because the interface is C. Revisit the MSVC target
  before a release; the guard is plain C so it should not matter, but it is untested.
- **Testability by a platform seam.** All policy and logic lives in platform-neutral crates that run
  on Linux. Only a thin crate touches Windows.
- **Initialisation off the loader lock; plugin code on the game thread.** `DllMain` only resolves the
  real `version.dll` and starts an init thread (as wc2r-plus does). The init thread does file work
  (state, manifests, checks 1-7). `LoadLibrary`, `w2r_plugin_main` and `on_load` run on the game
  thread at the first `Host::tick()`, so every callback honours the main-thread rule in `abi.md`. In
  2a `tick()` is called by the harness; 2b wires it to a game hook.
- **Game identity in 2a is version resource plus `SizeOfImage`.** A small built-in table lists
  supported builds (v1.0.2.2818, `SizeOfImage` 0x0062B000, from wc2r-plus). An unknown build means no
  plugin loads and one clear log line. 2b adds the byte-signature check as a further gate for
  `game.read` and `hooks.raw`.
- **Plugin log lines are sanitised** (control characters replaced) and tagged with the plugin id, to
  avoid log injection (deferred minor from the Phase 1 review).

## Crates

```
crates/
  w2r-abi/      #[repr(C)] mirror of w2r_abi.h; test compares size_of/offset_of with the C header
                compiled for the same target (a small C program prints them)
  w2r-spec/     pure Rust: manifest, semver/requirement/build types, state.json, fingerprint,
                peer tag. Passes the shared vectors. cargo-fuzz targets: manifest, state
  w2r-host/     platform-neutral: load pipeline, registry, dispatcher, host-function logic,
                settings/fs.data, fault bookkeeping. Generic over `trait Platform`
  w2r-core/     cdylib `version.dll`: Windows `Platform`, proxy exports, DllMain, build identity,
                logger sink, guard.c
  w2r-harness/  Windows exe: runs the real platform against a temp profile; scenarios below
  examples/
    hello/      cdylib plugin: logs on events, uses settings
    crasher/    cdylib plugin: faults on demand (the spike's cases)
```

### `trait Platform` (the seam)

Responsibilities, not signatures: read `state.json` and manifests (bounded reads); open a plugin DLL
with sharing that denies writers and deleters and hash it from that handle; load a module from the
open handle's path (absolute path, restricted search path) and look up `w2r_plugin_main`; run a
closure under the crash guard and report the fault code; suspend and resume the guard around host
functions; a bounded file API for `fs.data` and `config\<id>.json` (atomic replace, reparse points
refused); a clock; a log sink. The fake platform used on Linux can inject any failure, including a
fault on any callback.

## Behaviour

### Startup

1. `DllMain(attach)`: `DisableThreadLibraryCalls`, resolve the real `version.dll` from the system
   directory by absolute path, start the init thread. Nothing else.
2. Init thread: create the profile folders (`docs/layout.md`), open the log, identify the game build,
   read `state.json`, and for every enabled plugin run checks 1 to 7 of `docs/load-policy.md`,
   recording a reason code for each failure. No plugin DLL is loaded here.
3. First `tick()` on the game thread: in dependency order (ties by id), run checks 8 to 10 per plugin
   at its turn: dependency propagation, load from the locked handle, `w2r_plugin_main`,
   `on_load`. Each call into the plugin runs under the guard. Failures are logged as
   `plugin <id>: <code>: <detail>`.
4. Later ticks: deliver queued events, flush debounced settings, run `on_ui` (a no-op until 2d).

### Load pipeline details beyond `load-policy.md`

- The DLL's handle stays open from hashing until `LoadLibraryExW` has returned; the path passed is
  absolute and the search flags are `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32`.
- Peer-tag collisions are decided by id order among plugins that passed checks 1 to 6 and are never
  revisited in the same launch.
- A plugin never loaded because of a failure appears in the results table with its code; 2d shows it.
- `state.json` is read once; the host never writes it in 2a.

### Crash guard and faults

- The guard is the C shim from the spike, exposed as `guarded_call(fn, arg) -> Result<(), Fault>`
  with a thread-local arming state. Faults treated as fatal: access violation, stack overflow,
  illegal instruction, divide by zero, privileged instruction, in-page error, array bounds,
  breakpoint, and the fast-fail code if it is ever delivered.
- Every host function handed to a plugin goes through a wrapper that suspends the guard on entry and
  resumes it on exit. A fault inside host code therefore is not recovered and crashes loudly rather
  than leaving host locks or borrows corrupted.
- On a fault in a plugin callback: mark the plugin disabled for the session; drop its subscriptions;
  stop calling `on_ui` and `on_event`; remove its hooks (2b); skip `on_unload`; log the plugin id,
  callback, fault code and faulting address; continue with the other plugins. After a stack overflow
  the guard page is restored (`_resetstkoflw`) before returning.
- Not containable and documented as such: a Rust panic escaping an `extern "C"` callback, an
  exception escaping from C++, `__fastfail`, and faults on non-game threads (hook detours). The Rust
  SDK (a later sub-project) wraps callbacks in `catch_unwind`.
- A callback slower than 50 ms is logged; ten in a row sets a slow-plugin flag (shown in 2d).

### Dispatcher

- `emit(event)` delivers to subscribed, non-disabled plugins in load order, each under the guard.
  Subscription requires the event's capability (`abi.md`); `lobby_player_*` names are blanked without
  `chat.read`.
- Delivery is synchronous on the calling (game) thread. A plugin cannot re-enter `emit`; an event
  emitted from inside a callback is queued and delivered after the current dispatch returns.
- 2a emits `settings_changed` and `shutdown` itself; the harness (and later 2b/2c) injects the rest.

### Host functions in 2a

- `log`: 512-byte limit, 20 lines/s per plugin with a burst of 40, one summary line per second for
  dropped lines, plugin id tag, control characters replaced.
- `settings_*`: per-plugin `config\<id>.json`, limits from `abi.md` (128 keys, 1,024-byte strings,
  256 KiB). Writes are debounced: flushed at most once per second on a tick and at shutdown, by
  writing a temp file and renaming. Corrupt or oversize files are ignored with a log line and
  replaced on the next write; a plugin cannot damage `state.json` because it is never opened for
  writing by the host here.
- `data_*`: flat files in `plugins\<id>\data\`, name rules, 64 files, 16 MiB total, 8 open handles,
  64 KiB per call, reparse points refused, handles are host-issued integers.
- `subscribe`/`unsubscribe`, the main-thread check (`W2R_E_STATE` from any other thread).
- `symbol_*`, `chat_send`, `peer_send`, `hook_*`: `W2R_E_UNSUPPORTED`.

### `state.json` (new spec, deliverable)

`docs/state.md` with vectors in `docs/vectors/state.json`, written first (as in Phase 1) and then
implemented in `w2r-spec`. It formalises the sketch in `docs/layout.md`: strict JSON as in
`manifest.md`, at most 1 MiB, `schema` 1, plugin ids by the manifest id rule, `version` SemVer,
`enabled` boolean, `sha256` 64 lowercase hex, `approved_capabilities` a duplicate-free subset of the
five approval-requiring capabilities, at most 1,000 plugins. A bad file means no plugin loads and the
reason is logged; the host never "repairs" it.

### Logging (port from wc2r-plus)

`logs\w2r.log`, opened `_SH_DENYWR`, rotated to `w2r.1.log` at startup past 5 MB, at most 20 MB
written per launch after which a single notice is written and logging stops. Lines carry a time,
a tag and a level. Never logs peer-originated text unsanitised.

### `version.dll` proxy

Seventeen exports forwarded with `global_asm!` stubs that `jmp` through pointers resolved from the
system `version.dll` by absolute path (what wc2r-plus does). The export list is generated from the
real DLL's exports and checked by a test.

## Testing

- **Linux unit tests** on `w2r-spec` and `w2r-host` using the fake platform: every reason code,
  dependency order and propagation, tag collision, event capability gating, rate limits, settings
  debounce, `fs.data` quotas and name rules, fault bookkeeping (a fake guard returns injected faults).
- **Vectors** shared with Phase 1, plus new state vectors. The Python reference in `tools/` remains
  the oracle: a test runs both implementations on every vector and compares.
- **Fuzz targets** (`cargo-fuzz`, nightly): manifest, state, settings file. Run briefly in CI.
- **Wine scenarios** in `w2r-harness`: hello; crasher with each fault repeated three times;
  crasher plus hello together (hello keeps working); tampered DLL refused; DLL replaced while
  held open fails to write; dependency chain with a failing middle plugin; settings round trip;
  unknown game build loads nothing.
- CI: Linux job for the platform-neutral crates; Windows job for the 32-bit build and harness.

## Risks

1. **Wine is not Windows.** Exception delivery, stack-overflow recovery and handle sharing may differ.
   Mitigation: harness is runnable on Windows unchanged; a manual Windows run is an exit item.
2. **The game's own exception handlers.** The vectored handler is registered first, so it sees faults
   before the game's; faults outside a guarded call are passed on untouched. Needs checking in the
   real game (2b).
3. **Uncontainable deaths** (panic from a non-SDK Rust plugin, `__fastfail`). Documented, not solved.
4. **Scope creep into 2b.** The platform seam must not grow game-specific methods in 2a.

## Open items for the plan

- Whether `cc` (a crates.io dependency) may be used to build `guard.c`, or the build script calls
  the cross compiler directly as the spike did. Recommendation: call it directly, to keep the
  dependency list short.
- Which small crates are acceptable (`serde_json` with a strict wrapper, `sha2`). Recommendation:
  `serde_json` for parsing with the strictness checks written in `w2r-spec`, and `sha2`; no async
  runtime, no logging framework.
