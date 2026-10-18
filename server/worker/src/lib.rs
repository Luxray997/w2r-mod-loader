//! The stats service Worker (docs/stats-service.md). `ingest` is the pure policy and is tested
//! natively; the HTTP and D1 glue below only exists when building for WebAssembly.

pub mod diagnostics;
pub mod games;
pub mod ingest;
pub mod replay;

#[cfg(target_arch = "wasm32")]
mod api;
#[cfg(target_arch = "wasm32")]
mod app;
#[cfg(target_arch = "wasm32")]
mod jobs;
