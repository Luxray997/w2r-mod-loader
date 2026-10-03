# Handoff (2026-10-02)

Where the work stands, so it can be resumed on another machine (including Windows).

## State

- `main`: Phase 0 decisions and Phase 1 specs, merged (merge commit `d93db01`). Run all checks with
  `bash tools/run_all.sh` (needs Python 3.12 with `cryptography`, gcc/g++ for the C header check).
- `phase2a-loader-core` (not merged): the Phase 2a design
  (`docs/superpowers/specs/2026-10-02-phase2a-loader-core-design.md`) and plan 1 of 3
  (`docs/superpowers/plans/2026-10-02-phase2a-1-spec-crate.md`, the pure-Rust `w2r-spec` crate).
  No Rust code has been written yet.
- Next step: execute plan 2a-1 (native/inline execution was the chosen method), then write plans
  2a-2 (`w2r-abi` + platform-neutral `w2r-host`) and 2a-3 (Windows `w2r-core`, `guard.c`,
  `version.dll` proxy, harness, scenarios).

## Decisions worth knowing

- Pure Rust plus exactly one small C file (`guard.c`) for the crash guard.
- Crash-guard spike (run under Wine 9 only): a vectored exception handler with
  `__builtin_setjmp/longjmp` from a C shim contained access violation, stack overflow (needs
  `_resetstkoflw`), divide by zero and illegal instruction, repeatedly. It cannot contain an uncaught
  Rust panic from an `extern "C"` callback or `__fastfail`. Suspend the guard inside host functions
  so a fault in host code crashes loudly. **This must be re-run on real Windows**; on Windows you can
  run it natively, which is the first thing worth doing there. (The spike code itself was throwaway
  and is not in the repo; the design is in the Phase 2a doc.)
- Host target `i686-pc-windows-gnu` was chosen because that was what the Linux box had; on Windows
  `i686-pc-windows-msvc` is the natural alternative. The guard is plain C, so either should work, but
  the 2a design lists this as untested.
- JSON rule: `not_json` beats `duplicate_key`; nesting limit 32; non-finite numbers are `not_json`.
- Build-guard byte signatures and hook installation belong to sub-project 2b, not 2a.

## Open items

- Minisign compatibility was written from the format description; verify against the real
  `minisign` binary before release (`docs/PLAN.md`, Phase 6 exit).
- Deferred minor findings from the Phase 1 review (ABI details such as `symbol_read` bounds, token
  validation, small-buffer behaviour, extra vectors). Plan 2a-1 Task 1 also untracks the committed
  `.pyc` files and adds a `.gitignore`.
- `tools/check_abi.sh` skipped its 32-bit step on Linux (no 32-bit gcc); run it where a 32-bit
  toolchain exists.

## Resuming on Windows

- Install: Rust (rustup) with the `i686-pc-windows-msvc` and/or `i686-pc-windows-gnu` target, Python
  3.12 plus `pip install cryptography`, Git for Windows (provides `bash` for `tools/*.sh`), and a C
  compiler for the 32-bit header check (MSVC `cl` or mingw-w64 i686).
- `.gitattributes` forces LF for scripts, vectors and sources so the Linux-generated vectors and
  shell scripts behave the same with `core.autocrlf` on.
- The sibling repo `wc2r-plus` (the C++ mod being ported) is separate and needs its own clone.
