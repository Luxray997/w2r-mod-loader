//! In-game plugin host. Loaded into `Warcraft II.exe` (32-bit) as a `version.dll` proxy: the
//! built `w2r_core.dll` is installed under that name in the game's `x86` folder.
//!
//! Nothing here touches the network; only the manager downloads. See docs/PLAN.md.

pub mod build_guard;
pub mod chat;
pub mod dispatch;
pub mod fingerprint;
pub mod guard;
pub mod handshake;
pub mod hooks;
pub mod host;
pub mod hostcfg;
pub mod json;
pub mod lobby;
pub mod log;
pub mod manifest;
pub mod pe;
pub mod peer;
pub mod semver;
pub mod state;
pub mod store;
pub mod symbols;
pub mod target;
pub mod ui;
#[cfg(windows)]
pub mod watchdog;

#[cfg(windows)]
pub mod entry;
#[cfg(windows)]
pub mod frame;
#[cfg(windows)]
pub mod loader;
#[cfg(all(windows, target_arch = "x86"))]
pub mod matchstart;
#[cfg(all(windows, target_arch = "x86"))]
pub mod options;
#[cfg(windows)]
pub mod paths;
#[cfg(windows)]
pub mod plugins;
#[cfg(all(windows, target_arch = "x86", target_env = "msvc"))]
pub mod proxy;
#[cfg(windows)]
pub mod session;
