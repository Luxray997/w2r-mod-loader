//! Map sharing on the game (docs/map-download.md, slice 3): the real [`Env`] behind the roles, the
//! three game hooks (`SelectMapByRegistryName`, `MpLobbyStartGameButton`, `War2MpBNetCtor`), the
//! `'M'` inbox and the per-frame work. Ported from wc2r-plus `features/map_download.cpp` and
//! `map_download/game_map.cpp`. The rules are in [`crate::maproles`] and the state machines under
//! it; this file only touches the game. x86 only.
//!
//! Everything runs on the game's main thread except [`deliver`], which only queues.

use core::ffi::{CStr, c_char, c_void};
use std::collections::VecDeque;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

use crate::guard::guarded;
use crate::hooks::{self, Owner};
use crate::host::Log;
use crate::mapindex::{self, MapIndex};
use crate::maplimits::MAX_MAP_BYTES;
use crate::maproles::{ClientRole, Env, HostRole, ServeRead, route};
use crate::mapstore::path_under;
use crate::mapwire::ReqId;
use crate::pe::Image;
use crate::symbols::Symbols;

/// The game's path buffers.
const MAX_GAME_PATH: usize = 0x104;
/// The lobby's map name field.
const LOBBY_MAP_NAME_MAX: usize = 64;
/// A lobby not seen for this long is a new lobby.
const LOBBY_GAP_MS: u64 = 2_000;
const INBOX_CAP: usize = 256;
/// The info block `SelectMapAndReadHeader` fills.
const MAP_INFO_BYTES: usize = 0x48;

type ScanPudMetadataFn = unsafe extern "C" fn(*const u8, u32) -> u8;
type SelectMapAndReadHeaderFn = unsafe extern "C" fn(*const c_char, *mut c_void, i32) -> i32;
type ScanDirectoryFn = unsafe extern "C" fn(*mut c_void);
type SelectByNameFn = unsafe extern "C" fn(*const c_char) -> u8;
type StartButtonFn = unsafe extern "C" fn();
type NetCtorFn = unsafe extern "thiscall" fn(*mut c_void) -> *mut c_void;
type LocalizeFn = unsafe extern "C" fn(*const c_char) -> *const c_char;
type MenuButtonFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> i8;
type CtxFn = unsafe extern "C" fn(*mut c_void);

/// Addresses of the game's map state and helpers, fixed for the session.
#[derive(Clone, Copy)]
struct GameMap {
    lobby_map_name: *const u8,
    selected_path: *const u8,
    maps_base_dir: *const u8,
    missing_map_flag: *mut u8,
    countdown: *const u8,
    scan_pud: ScanPudMetadataFn,
    select_header: SelectMapAndReadHeaderFn,
    scan_directory: ScanDirectoryFn,
    // The disabled Start button.
    ctx_slot: *const *mut c_void,
    localize: LocalizeFn,
    menu_button: MenuButtonFn,
    disable_begin: CtxFn,
    disable_end: CtxFn,
}

// SAFETY: plain game addresses.
unsafe impl Send for GameMap {}
unsafe impl Sync for GameMap {}

static GAME: OnceLock<GameMap> = OnceLock::new();
static ORIGINAL_SELECT: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_START: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_CTOR: AtomicUsize = AtomicUsize::new(0);

struct State {
    host: HostRole,
    client: ClientRole,
    index: MapIndex,
    index_started: bool,
    inbox: VecDeque<(u32, Vec<u8>)>,
    last_seen: u64,
    was_host: bool,
    start_logged: bool,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let st = g.get_or_insert_with(|| {
        let index = MapIndex::new();
        State {
            host: HostRole::new(),
            client: ClientRole::new(index.clone(), PathBuf::new()),
            index,
            index_started: false,
            inbox: VecDeque::new(),
            last_seen: 0,
            was_host: false,
            start_logged: false,
        }
    });
    f(st)
}

fn log(msg: &str) {
    crate::session::log_info("mapdl", msg);
}

fn read_cstr(p: *const u8, max: usize) -> Option<String> {
    let bytes = unsafe { core::slice::from_raw_parts(p, max) };
    let n = bytes.iter().position(|&b| b == 0)?;
    Some(String::from_utf8_lossy(&bytes[..n]).into_owned())
}

impl GameMap {
    /// `<base>\Maps`, from the game's base directory string.
    fn maps_root(&self) -> Option<PathBuf> {
        let base = read_cstr(self.maps_base_dir, MAX_GAME_PATH)?;
        (!base.is_empty()).then(|| PathBuf::from(base).join("Maps"))
    }
}

fn mtime_of(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64 / 100)
}

/// The real game and network behind the roles.
struct RealEnv {
    game: GameMap,
    net: crate::session::NetView,
}

impl Env for RealEnv {
    fn lobby_map_name(&self) -> String {
        let bytes =
            unsafe { core::slice::from_raw_parts(self.game.lobby_map_name, LOBBY_MAP_NAME_MAX) };
        let n = bytes
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(LOBBY_MAP_NAME_MAX);
        String::from_utf8_lossy(&bytes[..n]).into_owned()
    }

    fn selected_map_path(&self) -> Option<String> {
        read_cstr(self.game.selected_path, MAX_GAME_PATH).filter(|s| !s.is_empty())
    }

    fn stat(&self, game_path: &str) -> Option<(u64, u64)> {
        let meta = fs::metadata(game_path).ok()?;
        Some((meta.len(), mtime_of(&meta)))
    }

    /// The selected file, read only if it really lies inside the game's Maps folders after links
    /// are resolved (wc2r-plus `ReadServableFile`), and only up to the protocol's size limit.
    fn read_servable(&self, game_path: &str) -> ServeRead {
        let Ok(file) = fs::File::open(game_path) else {
            return ServeRead::Unreadable;
        };
        let Ok(final_path) = fs::canonicalize(game_path) else {
            return ServeRead::Unreadable;
        };
        let Some(maps) = self.game.maps_root() else {
            return ServeRead::OutsideMaps(final_path.display().to_string());
        };
        let data_maps = maps.parent().map(|b| b.join("Data").join("Maps"));
        let inside = [Some(maps), data_maps]
            .into_iter()
            .flatten()
            .filter_map(|r| fs::canonicalize(r).ok())
            .any(|root| path_under(&final_path, &root));
        if !inside {
            return ServeRead::OutsideMaps(final_path.display().to_string());
        }
        let mut bytes = Vec::new();
        let ok = file
            .take(MAX_MAP_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .is_ok()
            && (1..=MAX_MAP_BYTES as usize).contains(&bytes.len());
        if ok {
            ServeRead::Ok(bytes)
        } else {
            ServeRead::Unreadable
        }
    }

    fn file_has_sha(&self, game_path: &str, sha: &[u8; 32]) -> bool {
        mapindex::hash_file(Path::new(game_path)).is_some_and(|h| h == *sha)
    }

    /// Selects the file for the lobby through the game's own map code (wc2r-plus `SelectByPath`).
    fn select_by_path(&mut self, game_path: &str) -> bool {
        if game_path.is_empty() || game_path.len() >= MAX_GAME_PATH {
            return false;
        }
        let mut bytes = Vec::new();
        let read = fs::File::open(game_path).and_then(|f| {
            f.take(mapindex::MAX_LOCAL_MAP_BYTES + 1)
                .read_to_end(&mut bytes)
        });
        if read.is_err() || bytes.is_empty() || bytes.len() as u64 > mapindex::MAX_LOCAL_MAP_BYTES {
            return false;
        }
        let Ok(c_path) = std::ffi::CString::new(game_path) else {
            return false;
        };
        let g = self.game;
        let ok = guarded(|| unsafe {
            if (g.scan_pud)(bytes.as_ptr(), bytes.len() as u32) == 0 {
                return false;
            }
            let mut info = [0u8; MAP_INFO_BYTES];
            if (g.select_header)(c_path.as_ptr(), info.as_mut_ptr().cast(), 0) == 0 {
                return false;
            }
            *g.missing_map_flag = 0;
            true
        });
        ok.unwrap_or(false)
    }

    /// Tells the game's registry about a folder: a `std::wstring` laid out by hand with a
    /// capacity of at least 8 so the game reads the pointer, not an inline buffer.
    fn register_directory(&mut self, dir: &Path) {
        let wide: Vec<u16> = dir
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();
        let len = wide.len() - 1;
        let mut obj = [0u8; 24];
        obj[..4].copy_from_slice(&(wide.as_ptr() as usize as u32).to_le_bytes());
        obj[16..20].copy_from_slice(&(len as u32).to_le_bytes());
        obj[20..24].copy_from_slice(&(len.max(8) as u32).to_le_bytes());
        let g = self.game;
        let _ = guarded(|| unsafe { (g.scan_directory)(obj.as_mut_ptr().cast()) });
    }

    fn to_game_path(&self, path: &Path) -> Option<String> {
        let s = path.to_str()?;
        (!s.is_empty() && s.len() < 260).then(|| s.to_owned())
    }

    fn peer_runs_mod(&self, slot: u32) -> bool {
        self.net.peer_runs_mod(slot)
    }

    fn peer_shares_maps(&self, slot: u32) -> bool {
        self.net.peer_shares_maps(slot)
    }

    fn countdown_running(&self) -> bool {
        unsafe { *self.game.countdown != 0 }
    }

    fn send_to(&mut self, slot: u8, bytes: &[u8]) {
        self.net.send_to(slot as u32, bytes);
    }

    fn send_all_modded(&mut self, bytes: &[u8]) {
        for slot in 1..8u32 {
            if self.net.peer_shares_maps(slot) {
                self.net.send_to(slot, bytes);
            }
        }
    }

    fn kick(&mut self, slot: u8) {
        self.net.kick(slot as u32);
    }

    fn make_req_id(&mut self) -> Option<ReqId> {
        use windows_sys::Win32::Security::Cryptography::{
            BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
        };
        let mut id = [0u8; 16];
        let status = unsafe {
            BCryptGenRandom(
                core::ptr::null_mut(),
                id.as_mut_ptr(),
                16,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        (status == 0).then_some(id)
    }

    fn log(&mut self, msg: &str) {
        log(msg);
    }
}

/// An `'M'` message taken off the wire; it waits for the next lobby frame.
pub fn deliver(sender: u32, body: &[u8]) {
    with_state(|st| {
        if st.inbox.len() < INBOX_CAP {
            st.inbox.push_back((sender, body.to_vec()));
        }
    });
}

/// Where downloads go, and the hash cache; the index build starts once, when the game's own
/// network object exists and the maps folder is known.
fn start_index_once(game: &GameMap) {
    with_state(|st| {
        if st.index_started {
            return;
        }
        let Some(maps) = game.maps_root() else {
            return;
        };
        st.index_started = true;
        let download = maps.join("Download");
        st.client.set_download_root(download.clone());
        let cache = crate::entry::startup().map(|s| s.root.join("cache").join("map_hashes.txt"));
        st.index.start_build(maps.clone(), cache);
        log(&format!(
            "indexing maps under '{}' (downloads go to Download\\V<n>)",
            maps.display()
        ));
    });
}

/// Once per frame the lobby window is drawn.
pub fn frame(now: u64, enabled: bool, ask: bool) {
    let Some(game) = GAME.get().copied() else {
        return;
    };
    let Some(net) = crate::session::net_view() else {
        return;
    };
    start_index_once(&game);
    let fresh = with_state(|st| {
        let fresh = st.last_seen == 0 || now.saturating_sub(st.last_seen) > LOBBY_GAP_MS;
        st.last_seen = now.max(1);
        fresh
    });
    let is_host = net.is_host();
    let mut env = RealEnv { game, net };
    with_state(|st| {
        if fresh || is_host != st.was_host {
            st.was_host = is_host;
            st.host.reset();
            st.client.reset();
            st.inbox.clear();
            st.start_logged = false;
        }
        if !env.net.connected() {
            return;
        }
        while let Some((sender, body)) = st.inbox.pop_front() {
            route(
                is_host,
                now,
                sender as usize,
                &body,
                &mut st.host,
                &mut st.client,
            );
        }
        if is_host {
            st.host.tick(&mut env, now, enabled);
        } else {
            if enabled {
                st.client.tick(&mut env, now, ask);
            }
            if st.client.should_show_stock_missing(&env, now, enabled) {
                unsafe { *game.missing_map_flag = 1 };
                log("the map cannot be had -- showing the game's own missing-map message");
            }
        }
    });
}

unsafe extern "C" fn select_by_name(name: *const c_char) -> u8 {
    let original: SelectByNameFn =
        unsafe { core::mem::transmute(ORIGINAL_SELECT.load(Ordering::Acquire)) };
    let found = unsafe { original(name) };
    let _ = guarded(|| {
        if name.is_null() {
            return;
        }
        let text = unsafe { CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned();
        let short: String = text.chars().take(64).collect();
        log(&format!(
            "lookup '{short}' -> {}",
            if found != 0 { "hit" } else { "MISS" }
        ));
        let is_host = crate::session::net_view().is_some_and(|n| n.is_host());
        if !is_host || found != 0 {
            with_state(|st| {
                st.client
                    .note_lookup(&text, found != 0, crate::chat::now_ms())
            });
        }
    });
    found
}

/// The host's Start button, held while a download is pending (drawn disabled).
unsafe extern "C" fn start_button() {
    let original: StartButtonFn =
        unsafe { core::mem::transmute(ORIGINAL_START.load(Ordering::Acquire)) };
    let hold = guarded(|| {
        let is_host = crate::session::net_view().is_some_and(|n| n.is_host());
        if !is_host {
            return false;
        }
        let now = crate::chat::now_ms();
        let (pending, slot, first) = with_state(|st| {
            let pending = st.host.any_pending(now);
            let first = pending && !st.start_logged;
            st.start_logged = pending;
            (pending, st.host.pending_slot(now), first)
        });
        if first {
            log(&format!(
                "Start held -- a download is pending (net slot {})",
                slot.map_or(-1, |s| s as i32)
            ));
        }
        pending
    })
    .unwrap_or(false);
    if !hold {
        return unsafe { original() };
    }
    let Some(g) = GAME.get().copied() else {
        return;
    };
    let _ = guarded(|| unsafe {
        let ctx = *g.ctx_slot;
        if ctx.is_null() {
            return;
        }
        let label = (g.localize)(c"customscenario_start".as_ptr());
        (g.disable_begin)(ctx);
        (g.menu_button)(ctx, label);
        (g.disable_end)(ctx);
    });
}

unsafe extern "thiscall" fn net_ctor(this: *mut c_void) -> *mut c_void {
    let original: NetCtorFn =
        unsafe { core::mem::transmute(ORIGINAL_CTOR.load(Ordering::Acquire)) };
    let r = unsafe { original(this) };
    if let Some(g) = GAME.get().copied() {
        let _ = guarded(|| start_index_once(&g));
    }
    r
}

fn hook_one(
    image: &Image,
    symbols: &Symbols,
    log_: &dyn Log,
    name: &str,
    detour: *mut c_void,
    original: &'static AtomicUsize,
) -> bool {
    let Some(target) = symbols.address(name) else {
        log_.line(
            W2R_LOG_ERROR,
            "host",
            &format!("{name} is not in the symbol table"),
        );
        return false;
    };
    // SAFETY: each detour matches the symbol table's signature for `name`.
    let result = unsafe {
        hooks::global().install(
            Owner::Host,
            image,
            target,
            detour,
            &format!("host:{name}"),
            |o| original.store(o as usize, Ordering::Release),
        )
    };
    match result {
        Ok(_) => {
            log_.line(
                W2R_LOG_INFO,
                "host",
                &format!("hooked {name} @ {target:#010x} (map sharing)"),
            );
            true
        }
        Err(e) => {
            log_.line(
                W2R_LOG_ERROR,
                "host",
                &format!("could not hook {name}: {e:?}"),
            );
            false
        }
    }
}

/// Resolves the game's map state and installs the three hooks. False when any symbol is missing;
/// map sharing then stays off.
pub fn install(image: &Image, symbols: &Symbols, log_: &dyn Log) -> bool {
    let at = |n: &'static str| symbols.address(n).ok_or(n);
    let resolved = (|| -> Result<GameMap, &'static str> {
        // SAFETY: each pointer type is the symbol table's `c_type` for that name.
        unsafe {
            Ok(GameMap {
                lobby_map_name: at("MpLobbyMapNameText")? as *const u8,
                selected_path: at("SelectedMapPath")? as *const u8,
                maps_base_dir: at("MapsBaseDir")? as *const u8,
                missing_map_flag: at("MissingMapFlag")? as *mut u8,
                countdown: at("MpLobbyCountdownActive")? as *const u8,
                scan_pud: core::mem::transmute::<usize, ScanPudMetadataFn>(at(
                    "ScanPudMetadataFromBuffer",
                )?),
                select_header: core::mem::transmute::<usize, SelectMapAndReadHeaderFn>(at(
                    "SelectMapAndReadHeader",
                )?),
                scan_directory: core::mem::transmute::<usize, ScanDirectoryFn>(at(
                    "ScanDirectoryIntoMapRegistry",
                )?),
                ctx_slot: at("NkContextMenus")? as *const *mut c_void,
                localize: core::mem::transmute::<usize, LocalizeFn>(at("LookupLocalized")?),
                menu_button: core::mem::transmute::<usize, MenuButtonFn>(at("DrawMenuButton")?),
                disable_begin: core::mem::transmute::<usize, CtxFn>(at("DisableBegin")?),
                disable_end: core::mem::transmute::<usize, CtxFn>(at("DisableEnd")?),
            })
        }
    })();
    let game = match resolved {
        Ok(g) => g,
        Err(name) => {
            log_.line(
                W2R_LOG_ERROR,
                "host",
                &format!("map sharing: symbol {name} missing; map sharing is off"),
            );
            return false;
        }
    };
    let _ = GAME.set(game);
    let a = hook_one(
        image,
        symbols,
        log_,
        "SelectMapByRegistryName",
        select_by_name as *mut c_void,
        &ORIGINAL_SELECT,
    );
    let b = hook_one(
        image,
        symbols,
        log_,
        "MpLobbyStartGameButton",
        start_button as *mut c_void,
        &ORIGINAL_START,
    );
    let c = hook_one(
        image,
        symbols,
        log_,
        "War2MpBNetCtor",
        net_ctor as *mut c_void,
        &ORIGINAL_CTOR,
    );
    a && b && c
}
