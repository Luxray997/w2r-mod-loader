# w2rpm

Plugin host and plugin manager for Warcraft II Remastered.

Status: planning. See [docs/PLAN.md](docs/PLAN.md), [docs/threat-model.md](docs/threat-model.md)
and [docs/layout.md](docs/layout.md).

## Warning

This is not an official Blizzard mod. It replaces the game's `version.dll` and loads native plugin
DLLs into the game, so it may be flagged by Blizzard's anti-cheat. No bans have been reported for
wc2r-plus, which it is based on, but that is no guarantee. Proceed at your own risk, and only install
plugins you trust: native plugins are not sandboxed (see [docs/threat-model.md](docs/threat-model.md)).

## Specs (Phase 1)

- [docs/abi.md](docs/abi.md) and [crates/w2r-abi/include/w2r_abi.h](crates/w2r-abi/include/w2r_abi.h): plugin ABI
- [docs/manifest.md](docs/manifest.md): `plugin.json`
- [docs/index.md](docs/index.md): signed plugin index
- [docs/load-policy.md](docs/load-policy.md): when the host loads a plugin, and the simulation fingerprint
- [docs/peer-protocol.md](docs/peer-protocol.md): the host's own peer messages (the handshake), wire-compatible with wc2r-plus
- [docs/vectors/](docs/vectors/): test vectors

Run `bash tools/run_all.sh` to check the vectors and the C header.

## Building (Phase 2)

The game is 32-bit, so the host is built for `i686-pc-windows-msvc` (rustup target plus the VS
Build Tools C++ workload):

```
cargo test --workspace --target i686-pc-windows-msvc
cargo build --release -p w2r-core --target i686-pc-windows-msvc
```

The result, `target/i686-pc-windows-msvc/release/w2r_core.dll`, goes into the game's `x86`
folder as `version.dll`. The host writes `w2r.log` under `Saved Games\Warcraft2Remastered\w2r\logs`
(or `%W2R_ROOT%\logs`). `cargo test` without `--target` also works for the pure-Rust parts.
Parts of the host and the whole chat plugin are ports of wc2r-plus by Luxray997 (MIT); see `THIRD_PARTY_NOTICES.md`.

`tools\install_dev.ps1` does the build and install and sets up the hello-world fixture plugin
under Saved Games (`-WithCrasher` adds the crashing one, `-Uninstall` removes the DLL). Set `PYTHON=/path/to/python.exe`
if `python3` is missing (Windows). The 32-bit header check uses `gcc -m32` or, on Windows, MSVC
located via `vswhere`.
