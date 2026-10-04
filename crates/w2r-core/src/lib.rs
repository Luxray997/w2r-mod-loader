//! In-game plugin host. Loaded into `Warcraft II.exe` (32-bit) as a `version.dll` proxy: the
//! built `w2r_core.dll` is installed under that name in the game's `x86` folder.
//!
//! Nothing here touches the network; only the manager downloads. See docs/PLAN.md.

pub mod build_guard;
pub mod dispatch;
pub mod guard;
pub mod hooks;
pub mod host;
pub mod log;
pub mod pe;
pub mod symbols;
pub mod target;

#[cfg(windows)]
pub mod entry;
#[cfg(windows)]
pub mod frame;
#[cfg(windows)]
pub mod loader;
#[cfg(windows)]
pub mod paths;
#[cfg(all(windows, target_arch = "x86", target_env = "msvc"))]
pub mod proxy;
