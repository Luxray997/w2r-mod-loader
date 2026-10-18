//! Rust bindings for the W2R Mod Loader plugin ABI, version 1.
//!
//! `include/w2r_abi.h` is the source of truth; this file mirrors it by hand. The layout tests at
//! the bottom check the same facts as `tests/abi_layout.c`, so a drift in either direction fails.
//!
//! ADD-ONLY: fields are only ever appended, constants are never renumbered, nothing is removed.
//! Every host function and plugin callback runs on the game's main thread. `W2R_CALL` is
//! `__cdecl`, which is what `extern "C"` means on 32-bit Windows.

#![no_std]

use core::ffi::{c_char, c_void};

pub const W2R_ABI_VERSION: u32 = 1;

// ---- status codes --------------------------------------------------------------------------
pub type W2rStatus = i32;
pub const W2R_OK: W2rStatus = 0;
/// capability not approved
pub const W2R_E_DENIED: W2rStatus = -1;
/// bad argument, or access to game memory failed
pub const W2R_E_INVALID: W2rStatus = -2;
/// a size or count limit was exceeded
pub const W2R_E_LIMIT: W2rStatus = -3;
/// not allowed right now (wrong thread, not in a lobby, ...)
pub const W2R_E_STATE: W2rStatus = -4;
/// host does not provide this (older host)
pub const W2R_E_UNSUPPORTED: W2rStatus = -5;
pub const W2R_E_NOT_FOUND: W2rStatus = -6;
/// rate limit hit; the call had no effect
pub const W2R_E_RATE: W2rStatus = -7;
/// hook overlaps an existing patch
pub const W2R_E_CONFLICT: W2rStatus = -8;
/// not ready yet, ask again on a later frame (the map index is still being built)
pub const W2R_E_BUSY: W2rStatus = -9;

// ---- value types ---------------------------------------------------------------------------
/// UTF-8, NOT NUL-terminated.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct W2rStr {
    pub ptr: *const c_char,
    pub len: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct W2rBuf {
    pub ptr: *const u8,
    pub len: u32,
}

/// host-issued, 0 is never valid
pub type W2rHandle = u32;

pub const W2R_LOG_DEBUG: u32 = 0;
pub const W2R_LOG_INFO: u32 = 1;
pub const W2R_LOG_WARN: u32 = 2;
pub const W2R_LOG_ERROR: u32 = 3;

pub const W2R_FILE_READ: u32 = 1;
/// create or truncate
pub const W2R_FILE_WRITE: u32 = 2;

/// peer_send: every peer that announced the handshake
pub const W2R_SLOT_ALL: u32 = 0xFFFF_FFFF;

// ---- limits (also in docs/abi.md) ----------------------------------------------------------
pub const W2R_MAX_LOG_BYTES: u32 = 512;
pub const W2R_MAX_SETTING_KEY: u32 = 64;
pub const W2R_MAX_SETTING_STR: u32 = 1024;
pub const W2R_MAX_FILE_NAME: u32 = 64;
pub const W2R_MAX_IO_BYTES: u32 = 65536;
pub const W2R_MAX_SYMBOL_NAME: u32 = 64;
pub const W2R_MAX_READ_BYTES: u32 = 4096;
pub const W2R_MAX_CHAT_BYTES: u32 = 200;
/// 0xE7 'W' '2' 'R' 'P' tag[4]
pub const W2R_PEER_HEADER_BYTES: u32 = 9;
pub const W2R_MAX_PEER_PAYLOAD: u32 = 960;

// ---- events --------------------------------------------------------------------------------
pub type W2rEventId = u32;
pub const W2R_EVT_LOBBY_ENTER: W2rEventId = 1;
pub const W2R_EVT_LOBBY_LEAVE: W2rEventId = 2;
/// payload W2rEvtPlayer; name empty without chat.read
pub const W2R_EVT_LOBBY_PLAYER_JOIN: W2rEventId = 3;
/// payload W2rEvtPlayer
pub const W2R_EVT_LOBBY_PLAYER_LEAVE: W2rEventId = 4;
/// payload W2rEvtChat; needs chat.read
pub const W2R_EVT_CHAT_RECEIVED: W2rEventId = 5;
/// payload W2rEvtPeerMessage; needs net.send
pub const W2R_EVT_PEER_MESSAGE: W2rEventId = 6;
pub const W2R_EVT_MATCH_START: W2rEventId = 7;
pub const W2R_EVT_MATCH_END: W2rEventId = 8;
pub const W2R_EVT_SETTINGS_CHANGED: W2rEventId = 9;
pub const W2R_EVT_SHUTDOWN: W2rEventId = 10;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct W2rEvent {
    pub struct_size: u32,
    pub id: W2rEventId,
    /// NULL for events without one; valid only during the callback
    pub payload: *const c_void,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct W2rEvtPlayer {
    pub struct_size: u32,
    pub net_slot: u32,
    /// sanitised: printable ASCII only
    pub name: W2rStr,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct W2rEvtChat {
    pub struct_size: u32,
    pub net_slot: u32,
    pub name: W2rStr,
    /// sanitised: printable ASCII only
    pub text: W2rStr,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct W2rEvtPeerMessage {
    pub struct_size: u32,
    /// the slot the transport reported, never one named inside the message
    pub net_slot: u32,
    /// the payload after the 9-byte header
    pub data: W2rBuf,
}

// ---- UI (only valid inside on_ui) ----------------------------------------------------------
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct W2rUi {
    pub struct_size: u32,
    pub ctx: *mut c_void,
    pub label: unsafe extern "C" fn(ctx: *mut c_void, text: W2rStr),
    /// 1 when pressed
    pub button: unsafe extern "C" fn(ctx: *mut c_void, text: W2rStr) -> i32,
    /// 1 when changed
    pub checkbox: unsafe extern "C" fn(ctx: *mut c_void, text: W2rStr, value: *mut i32) -> i32,
    pub slider_int: unsafe extern "C" fn(
        ctx: *mut c_void,
        text: W2rStr,
        value: *mut i32,
        min: i32,
        max: i32,
    ) -> i32,
    /// Appended after the first release: check `struct_size` covers this before using it.
    /// An edit box over the plugin's own NUL-terminated buffer of `cap` bytes; 1 when the text
    /// changed this frame.
    pub text_edit:
        unsafe extern "C" fn(ctx: *mut c_void, text: W2rStr, buf: *mut u8, cap: u32) -> i32,
    /// Appended with `text_edit`. 0 greys out and disables the rows that follow, 1 restores them.
    pub set_active: unsafe extern "C" fn(ctx: *mut c_void, active: i32),
}

/// `W2rUi.struct_size` of the first release: a plugin that wants `text_edit` or `set_active`
/// checks `ui.struct_size >= W2R_UI_TEXT_EDIT_SIZE`.
pub const W2R_UI_TEXT_EDIT_SIZE: u32 = core::mem::size_of::<W2rUi>() as u32;

// ---- host table: provided by the host, one instance per plugin -----------------------------
/// The host fills every version-1 field, so these pointers are never null for a version-1 host.
/// A plugin built against a later version must check `struct_size` before touching fields past
/// the version it was built for.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct W2rHost {
    pub struct_size: u32,
    pub abi_version: u32,
    /// opaque; pass back as the first argument of every call
    pub token: *mut c_void,

    // log (implicit)
    pub log: unsafe extern "C" fn(token: *mut c_void, level: u32, msg: W2rStr) -> W2rStatus,

    // settings (implicit): the plugin's own settings only
    pub settings_get_i64:
        unsafe extern "C" fn(token: *mut c_void, key: W2rStr, out: *mut i64) -> W2rStatus,
    pub settings_set_i64:
        unsafe extern "C" fn(token: *mut c_void, key: W2rStr, value: i64) -> W2rStatus,
    pub settings_get_str: unsafe extern "C" fn(
        token: *mut c_void,
        key: W2rStr,
        buf: *mut c_char,
        cap: u32,
        len_out: *mut u32,
    ) -> W2rStatus,
    pub settings_set_str:
        unsafe extern "C" fn(token: *mut c_void, key: W2rStr, value: W2rStr) -> W2rStatus,

    // fs.data (implicit): flat files in the plugin's own data folder, by host handle
    pub data_open: unsafe extern "C" fn(
        token: *mut c_void,
        name: W2rStr,
        flags: u32,
        out: *mut W2rHandle,
    ) -> W2rStatus,
    pub data_read: unsafe extern "C" fn(
        token: *mut c_void,
        file: W2rHandle,
        dst: *mut c_void,
        cap: u32,
        read_out: *mut u32,
    ) -> W2rStatus,
    pub data_write: unsafe extern "C" fn(
        token: *mut c_void,
        file: W2rHandle,
        src: *const c_void,
        len: u32,
    ) -> W2rStatus,
    pub data_close: unsafe extern "C" fn(token: *mut c_void, file: W2rHandle) -> W2rStatus,

    // events
    pub subscribe: unsafe extern "C" fn(token: *mut c_void, id: W2rEventId) -> W2rStatus,
    pub unsubscribe: unsafe extern "C" fn(token: *mut c_void, id: W2rEventId) -> W2rStatus,

    // game.read
    pub symbol_lookup:
        unsafe extern "C" fn(token: *mut c_void, name: W2rStr, out: *mut W2rHandle) -> W2rStatus,
    pub symbol_read: unsafe extern "C" fn(
        token: *mut c_void,
        symbol: W2rHandle,
        offset: u32,
        dst: *mut c_void,
        len: u32,
    ) -> W2rStatus,

    // chat.write
    pub chat_send: unsafe extern "C" fn(token: *mut c_void, text: W2rStr) -> W2rStatus,

    // net.send
    pub peer_send: unsafe extern "C" fn(
        token: *mut c_void,
        net_slot_or_all: u32,
        payload: W2rBuf,
    ) -> W2rStatus,

    // hooks.raw (dangerous)
    pub hook_install: unsafe extern "C" fn(
        token: *mut c_void,
        target: u32,
        detour: *mut c_void,
        original_out: *mut *mut c_void,
        out: *mut W2rHandle,
    ) -> W2rStatus,
    pub hook_remove: unsafe extern "C" fn(token: *mut c_void, hook: W2rHandle) -> W2rStatus,

    // hooks.raw (dangerous): the runtime address of any symbol, for a plugin that calls game
    // functions or writes game memory itself (docs/chat-plugin.md)
    pub symbol_address: unsafe extern "C" fn(
        token: *mut c_void,
        symbol: W2rHandle,
        address_out: *mut usize,
    ) -> W2rStatus,

    // Create-screen extension (implicit). Appended after the first release: check
    // `struct_size >= W2R_HOST_CREATE_UI_SIZE` before using these.
    /// Registers the plugin's one callback for the Create Multiplayer Game screen. The host calls
    /// it every frame the enhanced screen draws, with a `W2rUi` whose rows appear under the map
    /// preview. Registering again replaces the callback; null removes it.
    pub create_ui_register: unsafe extern "C" fn(
        token: *mut c_void,
        callback: Option<unsafe extern "C" fn(ui: *const W2rUi)>,
    ) -> W2rStatus,
    /// Only inside the create-screen callback. Counts the maps of the host's map index with
    /// `players` maximum players (0 = any). `W2R_E_BUSY` while the index is being built.
    pub create_map_count: unsafe extern "C" fn(
        token: *mut c_void,
        players: u32,
        count_out: *mut u32,
    ) -> W2rStatus,
    /// Only inside the create-screen callback. Makes the `nth` (0-based, in index order) map with
    /// `players` maximum players (0 = any) the screen's selected map.
    pub create_map_select:
        unsafe extern "C" fn(token: *mut c_void, players: u32, nth: u32) -> W2rStatus,

    // Appended after the create-screen functions: check `struct_size >= W2R_HOST_MAP_FOLDER_SIZE`.
    /// Only inside the create-screen callback. Limits `create_map_count` and `create_map_select`
    /// of this callback to the maps in `folder` (a path under the maps folder, `\` or `/`
    /// separated, subfolders included). An empty string removes the limit. The limit lasts until
    /// the callback returns, so call this first, every frame. A folder with no maps gives a count
    /// of 0.
    pub create_map_folder: unsafe extern "C" fn(token: *mut c_void, folder: W2rStr) -> W2rStatus,
}

// ---- plugin table: returned by the plugin --------------------------------------------------
/// Callbacks are `Option` because a plugin may leave any of them NULL; the host checks
/// `on_load` is present (`E_ENTRY` otherwise) and skips the others when absent.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct W2rPlugin {
    pub struct_size: u32,
    /// non-zero fails the load (E_LOAD)
    pub on_load: Option<unsafe extern "C" fn(host: *const W2rHost) -> W2rStatus>,
    pub on_event: Option<unsafe extern "C" fn(event: *const W2rEvent)>,
    pub on_ui: Option<unsafe extern "C" fn(ui: *const W2rUi)>,
    /// best effort
    pub on_unload: Option<unsafe extern "C" fn()>,
}

/// The single export every plugin DLL provides, named `w2r_plugin_main`.
pub type W2rPluginMain = unsafe extern "C" fn(host: *const W2rHost) -> *const W2rPlugin;
pub const W2R_PLUGIN_MAIN_SYMBOL: &core::ffi::CStr = c"w2r_plugin_main";

/// Minimum `struct_size` a version-1 `W2rPlugin` may report.
pub const W2R_PLUGIN_V1_SIZE: u32 = core::mem::size_of::<W2rPlugin>() as u32;
/// `struct_size` a version-1 host reports.
pub const W2R_HOST_V1_SIZE: u32 =
    (core::mem::offset_of!(W2rHost, symbol_address) + core::mem::size_of::<usize>()) as u32;
/// `struct_size` of a host that has the create-screen functions.
pub const W2R_HOST_CREATE_UI_SIZE: u32 =
    (core::mem::offset_of!(W2rHost, create_map_select) + core::mem::size_of::<usize>()) as u32;
/// `struct_size` of a host that also has `create_map_folder`.
pub const W2R_HOST_MAP_FOLDER_SIZE: u32 = core::mem::size_of::<W2rHost>() as u32;

// Mirrors tests/abi_layout.c. Keep both in sync.
const _: () = {
    use core::mem::{offset_of, size_of};
    assert!(W2R_ABI_VERSION == 1);
    assert!(offset_of!(W2rHost, struct_size) == 0);
    assert!(offset_of!(W2rHost, abi_version) == 4);
    assert!(offset_of!(W2rHost, token) == 8);
    assert!(offset_of!(W2rPlugin, struct_size) == 0);
    assert!(offset_of!(W2rUi, struct_size) == 0);
    assert!(offset_of!(W2rEvent, struct_size) == 0);
    assert!(offset_of!(W2rStr, len) == size_of::<*const c_void>());
    assert!(size_of::<W2rStatus>() == 4);
    assert!(W2R_OK == 0);
    assert!(W2R_E_DENIED < 0 && W2R_E_BUSY < 0);
    assert!(W2R_MAX_PEER_PAYLOAD + W2R_PEER_HEADER_BYTES <= 989);
    // Option<fn> must be pointer-sized with NULL as None for the plugin table to match C.
    assert!(size_of::<Option<unsafe extern "C" fn()>>() == size_of::<*const c_void>());
};

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    // The game target. 17 host function pointers after a 12-byte header.
    #[cfg(target_pointer_width = "32")]
    #[test]
    fn sizes_match_the_c_header_on_x86() {
        assert_eq!(size_of::<W2rStr>(), 8);
        assert_eq!(size_of::<W2rEvent>(), 12);
        assert_eq!(size_of::<W2rPlugin>(), 20);
        assert_eq!(size_of::<W2rUi>(), 32);
        assert_eq!(size_of::<W2rHost>(), 100);
        assert_eq!(W2R_HOST_V1_SIZE, 84);
        assert_eq!(W2R_PLUGIN_V1_SIZE, 20);
        assert_eq!(W2R_HOST_CREATE_UI_SIZE, 96);
        assert_eq!(W2R_HOST_MAP_FOLDER_SIZE, 100);
    }

    // Not a shipping target, but the tests run here too; pointer alignment pads the u32 headers.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn sizes_match_the_c_header_on_x64() {
        assert_eq!(size_of::<W2rStr>(), 16);
        assert_eq!(size_of::<W2rEvent>(), 16);
        assert_eq!(size_of::<W2rPlugin>(), 40);
        assert_eq!(size_of::<W2rUi>(), 64);
        assert_eq!(size_of::<W2rHost>(), 192);
    }
}
