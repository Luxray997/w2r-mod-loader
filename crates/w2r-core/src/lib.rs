//! In-game plugin host. Loaded into `Warcraft II.exe` (32-bit) as a `version.dll` proxy.
//!
//! Nothing here touches the network; only the manager downloads. See docs/PLAN.md.

pub mod dispatch;
pub mod guard;
pub mod host;
#[cfg(windows)]
pub mod loader;
