# w2rpm

Plugin host and plugin manager for Warcraft II Remastered.

Status: planning. See [docs/PLAN.md](docs/PLAN.md), [docs/threat-model.md](docs/threat-model.md)
and [docs/layout.md](docs/layout.md).

## Specs (Phase 1)

- [docs/abi.md](docs/abi.md) and [crates/w2r-abi/include/w2r_abi.h](crates/w2r-abi/include/w2r_abi.h): plugin ABI
- [docs/manifest.md](docs/manifest.md): `plugin.json`
- [docs/index.md](docs/index.md): signed plugin index
- [docs/load-policy.md](docs/load-policy.md): when the host loads a plugin, and the simulation fingerprint
- [docs/vectors/](docs/vectors/): test vectors

Run `bash tools/run_all.sh` to check the vectors and the C header. Set `PYTHON=/path/to/python.exe`
if `python3` is missing (Windows). The 32-bit header check uses `gcc -m32` or, on Windows, MSVC
located via `vswhere`.
