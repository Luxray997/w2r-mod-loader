//! The parsers the host and the manager must agree on, byte for byte: strict JSON, SemVer,
//! `plugin.json` and `state.json`. Specs: `docs/manifest.md`, `docs/layout.md`.

pub mod json;
pub mod manifest;
pub mod semver;
pub mod state;
