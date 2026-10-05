//! In-game plugin host. Loaded into `Warcraft II.exe` (32-bit) as a `version.dll` proxy: the
//! built `w2r_core.dll` is installed under that name in the game's `x86` folder.
//!
//! Nothing here touches the network; only the manager downloads. See docs/PLAN.md.

pub mod build_guard;
pub mod chat;
pub mod dispatch;
pub mod downloadclient;
pub mod downloadhost;
pub mod fingerprint;
pub mod guard;
pub mod handshake;
pub mod hooks;
pub mod host;
pub mod hostcfg;
pub mod json;
pub mod lobby;
pub mod lobbylayout;
pub mod lobbypolicy;
pub mod log;
pub mod manifest;
pub mod mapbrowser;
pub mod mapindex;
pub mod maplimits;
pub mod mapname;
pub mod mappreview;
pub mod maproles;
pub mod mapsearchcache;
pub mod mapstore;
pub mod mapwire;
pub mod pe;
pub mod peer;
pub mod pud;
pub mod pudvalidate;
pub mod screenkit;
pub mod semver;
pub mod state;
pub mod store;
pub mod symbols;
pub mod target;
pub mod ui;
#[cfg(windows)]
pub mod watchdog;

#[cfg(all(windows, target_arch = "x86"))]
pub mod browserview;
#[cfg(all(windows, target_arch = "x86"))]
pub mod createscreen;
#[cfg(windows)]
pub mod entry;
#[cfg(windows)]
pub mod frame;
#[cfg(windows)]
pub mod loader;
#[cfg(all(windows, target_arch = "x86"))]
pub mod lobbycreate;
#[cfg(all(windows, target_arch = "x86"))]
pub mod lobbyscreen;
#[cfg(all(windows, target_arch = "x86"))]
pub mod mapdownload;
#[cfg(all(windows, target_arch = "x86"))]
pub mod matchstart;
#[cfg(all(windows, target_arch = "x86"))]
pub mod options;
#[cfg(windows)]
pub mod paths;
#[cfg(windows)]
pub mod plugins;
#[cfg(all(windows, target_arch = "x86"))]
pub mod previewdraw;
#[cfg(all(windows, target_arch = "x86", target_env = "msvc"))]
pub mod proxy;
#[cfg(windows)]
pub mod session;
