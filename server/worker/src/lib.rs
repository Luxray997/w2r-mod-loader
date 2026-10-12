//! The stats service Worker (docs/stats-service.md). `ingest` is the pure policy and is tested
//! natively; the HTTP and D1 glue below only exists when building for WebAssembly.

pub mod ingest;

#[cfg(target_arch = "wasm32")]
mod app;
