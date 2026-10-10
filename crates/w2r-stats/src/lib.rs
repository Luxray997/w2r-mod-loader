//! Game result reports (docs/stats-reporting.md): what the host reads from the game's tables,
//! the report schema, the per-install signing identity, the signed envelope and the on-disk
//! queue that holds reports until an uploader exists. Pure Rust, no game or Windows calls, so
//! everything here is tested without the game; the host supplies entropy and file locations.

pub mod envelope;
pub mod hex;
pub mod history;
pub mod identity;
pub mod queue;
pub mod report;
pub mod slot;
