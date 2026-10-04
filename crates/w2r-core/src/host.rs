//! The per-plugin host table (`W2rHost`) and the host functions it serves: `log`, events,
//! `symbol_lookup`/`symbol_read` (`game.read`), `hook_install`/`hook_remove` (`hooks.raw`),
//! `chat_send` (`chat.write`) and `peer_send` (`net.send`). Settings and data files return
//! `W2R_E_UNSUPPORTED` until their step lands; a plugin must handle that status anyway.
//!
//! Rule for every callback here: validate first, borrow a `RefCell` last, and never hold a
//! borrow across anything that can fault. A fault abandons frames without running destructors,
//! so a live `RefMut` would leave the cell borrowed for the rest of the session.

use core::ffi::{c_char, c_void};
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use w2r_abi::*;

use crate::chat::{self, Chat, NoChat};
use crate::guard::guarded;
use crate::hooks::{self, HookError, Owner};
use crate::pe::Image;
use crate::peer::{
    self, NoPeers, NoTransport, Nowhere, Presence, Roster, SlidingWindow, Target, TargetError,
    Transport,
};
use crate::symbols::Symbols;
use crate::target::Kind;

/// Capability names as they appear in `plugin.json` and `state.json` (docs/layout.md).
pub mod caps {
    pub const GAME_READ: &str = "game.read";
    pub const CHAT_READ: &str = "chat.read";
    pub const CHAT_WRITE: &str = "chat.write";
    pub const NET_SEND: &str = "net.send";
    pub const HOOKS_RAW: &str = "hooks.raw";
}

/// Where host and plugin log lines go. Single-threaded: everything runs on the game's main
/// thread.
pub trait Log {
    fn line(&self, level: u32, plugin: &str, msg: &str);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Active,
    /// Never called again this session. `reason` is what was logged.
    Disabled {
        reason: String,
    },
}

/// The game module as the host found it. `None` outside the game (tests).
pub struct Game {
    pub image: Image,
    pub symbols: Symbols,
}

/// The game-facing services behind traits, so every rule is testable with fakes.
pub struct Services {
    pub chat: Rc<dyn Chat>,
    pub transport: Rc<dyn Transport>,
    pub roster: Rc<dyn Roster>,
    pub presence: Rc<dyn Presence>,
}

impl Services {
    /// No game: nothing available.
    pub fn none() -> Services {
        Services {
            chat: Rc::new(NoChat),
            transport: Rc::new(NoTransport),
            roster: Rc::new(NoPeers),
            presence: Rc::new(Nowhere),
        }
    }
}

/// What every slot shares.
pub struct Shared {
    pub log: Rc<dyn Log>,
    pub game: Option<Game>,
    pub services: Services,
    /// `peer_send` across all plugins: 128/s.
    pub peer_rate: RefCell<SlidingWindow>,
}

impl Shared {
    pub fn new(log: Rc<dyn Log>, game: Option<Game>, services: Services) -> Shared {
        Shared {
            log,
            game,
            services,
            peer_rate: RefCell::new(SlidingWindow::new(peer::OVERALL_PER_SECOND, 1000)),
        }
    }
}

const EMPTY_TABLE: W2rPlugin = W2rPlugin {
    struct_size: 0,
    on_load: None,
    on_event: None,
    on_ui: None,
    on_unload: None,
};

/// One loaded (or refused) plugin. Boxed so its address, which is the plugin's `token`, never
/// moves.
pub struct Slot {
    /// Position in the registry; also the hook owner id.
    pub index: usize,
    pub id: String,
    /// Empty until `w2r_plugin_main` returned a valid table.
    pub table: Cell<W2rPlugin>,
    caps: BTreeSet<String>,
    state: RefCell<State>,
    subs: RefCell<BTreeSet<W2rEventId>>,
    /// Functions already refused for a missing capability (logged once each).
    denied: RefCell<BTreeSet<&'static str>>,
    chat_rate: RefCell<SlidingWindow>,
    peer_rate: RefCell<SlidingWindow>,
    /// The plugin's peer-message tag (docs/abi.md).
    pub tag: [u8; 4],
    /// Consecutive callbacks over [`Slot::SLOW_MS`].
    slow: Cell<u32>,
    shared: Rc<Shared>,
    host: W2rHost,
}

impl Slot {
    pub fn new(index: usize, id: &str, caps: &[&str], shared: Rc<Shared>) -> Box<Slot> {
        let mut slot = Box::new(Slot {
            index,
            id: id.to_owned(),
            table: Cell::new(EMPTY_TABLE),
            caps: caps.iter().map(|c| (*c).to_owned()).collect(),
            state: RefCell::new(State::Active),
            subs: RefCell::new(BTreeSet::new()),
            denied: RefCell::new(BTreeSet::new()),
            chat_rate: RefCell::new(SlidingWindow::new(chat::MAX_PER_WINDOW, chat::WINDOW_MS)),
            peer_rate: RefCell::new(SlidingWindow::new(peer::PER_PLUGIN_PER_SECOND, 1000)),
            tag: peer::peer_tag(id),
            slow: Cell::new(0),
            shared,
            host: host_table(core::ptr::null_mut()),
        });
        let token: *mut Slot = &mut *slot;
        slot.host.token = token.cast();
        slot
    }

    /// docs/abi.md: callbacks over this are logged...
    pub const SLOW_MS: u128 = 50;
    /// ...and after this many in a row the plugin list shows a warning.
    pub const SLOW_STREAK: u32 = 10;

    /// Records how long a callback took.
    pub fn note_duration(&self, ms: u128, what: &str) {
        if ms > Self::SLOW_MS {
            self.slow.set(self.slow.get().saturating_add(1));
            self.log(W2R_LOG_WARN, &format!("{what} took {ms} ms"));
        } else {
            self.slow.set(0);
        }
    }

    pub fn is_slow(&self) -> bool {
        self.slow.get() >= Self::SLOW_STREAK
    }

    /// The table handed to the plugin. Valid for as long as the slot lives.
    pub fn host(&self) -> *const W2rHost {
        &self.host
    }

    pub fn state(&self) -> State {
        self.state.borrow().clone()
    }

    pub fn is_active(&self) -> bool {
        matches!(&*self.state.borrow(), State::Active)
    }

    pub fn subscribed(&self, id: W2rEventId) -> bool {
        self.subs.borrow().contains(&id)
    }

    pub fn has_cap(&self, cap: &str) -> bool {
        self.caps.contains(cap)
    }

    pub fn owner(&self) -> Owner {
        Owner::Plugin(self.index)
    }

    pub fn log(&self, level: u32, msg: &str) {
        self.shared.log.line(level, &self.id, msg);
    }

    /// Logs the refusal once per function and returns `W2R_E_DENIED`.
    fn deny(&self, function: &'static str, cap: &str) -> W2rStatus {
        if self.denied.borrow_mut().insert(function) {
            self.log(
                W2R_LOG_WARN,
                &format!("{function}: capability {cap} not approved"),
            );
        }
        W2R_E_DENIED
    }

    /// Stops every future call into the plugin, drops its subscriptions and removes its hooks.
    pub fn disable(&self, reason: String) {
        self.log(W2R_LOG_ERROR, &format!("disabled: {reason}"));
        *self.state.borrow_mut() = State::Disabled { reason };
        self.subs.borrow_mut().clear();
        let removed = hooks::global().remove_all(self.owner());
        if !removed.is_empty() {
            self.log(
                W2R_LOG_INFO,
                &format!("removed hooks: {}", removed.join(", ")),
            );
        }
    }
}

fn host_table(token: *mut c_void) -> W2rHost {
    W2rHost {
        struct_size: W2R_HOST_V1_SIZE,
        abi_version: W2R_ABI_VERSION,
        token,
        log: cb::log,
        settings_get_i64: cb::settings_get_i64,
        settings_set_i64: cb::settings_set_i64,
        settings_get_str: cb::settings_get_str,
        settings_set_str: cb::settings_set_str,
        data_open: cb::data_open,
        data_read: cb::data_read,
        data_write: cb::data_write,
        data_close: cb::data_close,
        subscribe: cb::subscribe,
        unsubscribe: cb::unsubscribe,
        symbol_lookup: cb::symbol_lookup,
        symbol_read: cb::symbol_read,
        chat_send: cb::chat_send,
        peer_send: cb::peer_send,
        hook_install: cb::hook_install,
        hook_remove: cb::hook_remove,
    }
}

mod cb {
    use super::*;

    unsafe fn slot<'a>(token: *mut c_void) -> Option<&'a Slot> {
        unsafe { token.cast::<Slot>().as_ref() }
    }

    /// A panic in host code must not unwind into the plugin (that would abort the game).
    fn run(f: impl FnOnce() -> W2rStatus) -> W2rStatus {
        catch_unwind(AssertUnwindSafe(f)).unwrap_or(W2R_E_INVALID)
    }

    /// Borrows the plugin's string. A bad pointer faults here, inside the plugin's guard, and
    /// disables the plugin.
    unsafe fn text<'a>(s: W2rStr) -> Result<&'a str, W2rStatus> {
        if s.len == 0 {
            return Ok("");
        }
        if s.ptr.is_null() {
            return Err(W2R_E_INVALID);
        }
        let bytes = unsafe { core::slice::from_raw_parts(s.ptr.cast::<u8>(), s.len as usize) };
        core::str::from_utf8(bytes).map_err(|_| W2R_E_INVALID)
    }

    fn known_event(id: W2rEventId) -> bool {
        (W2R_EVT_LOBBY_ENTER..=W2R_EVT_SHUTDOWN).contains(&id)
    }

    /// docs/abi.md: events that need a capability to subscribe.
    fn event_cap(id: W2rEventId) -> Option<&'static str> {
        match id {
            W2R_EVT_CHAT_RECEIVED => Some(caps::CHAT_READ),
            W2R_EVT_PEER_MESSAGE => Some(caps::NET_SEND),
            _ => None,
        }
    }

    fn require(slot: &Slot, function: &'static str, cap: &str) -> Result<(), W2rStatus> {
        if slot.has_cap(cap) {
            Ok(())
        } else {
            Err(slot.deny(function, cap))
        }
    }

    fn game(slot: &Slot) -> Result<&Game, W2rStatus> {
        slot.shared.game.as_ref().ok_or(W2R_E_STATE)
    }

    pub unsafe extern "C" fn log(token: *mut c_void, level: u32, msg: W2rStr) -> W2rStatus {
        run(|| {
            let Some(slot) = (unsafe { slot(token) }) else {
                return W2R_E_INVALID;
            };
            if level > W2R_LOG_ERROR {
                return W2R_E_INVALID;
            }
            if msg.len > W2R_MAX_LOG_BYTES {
                return W2R_E_LIMIT;
            }
            let text = match unsafe { text(msg) } {
                Ok(t) => t,
                Err(e) => return e,
            };
            slot.log(level, text);
            W2R_OK
        })
    }

    pub unsafe extern "C" fn subscribe(token: *mut c_void, id: W2rEventId) -> W2rStatus {
        run(|| {
            let Some(slot) = (unsafe { slot(token) }) else {
                return W2R_E_INVALID;
            };
            if !known_event(id) {
                return W2R_E_NOT_FOUND;
            }
            if let Some(cap) = event_cap(id) {
                if let Err(e) = require(slot, "subscribe", cap) {
                    return e;
                }
            }
            slot.subs.borrow_mut().insert(id);
            W2R_OK
        })
    }

    pub unsafe extern "C" fn unsubscribe(token: *mut c_void, id: W2rEventId) -> W2rStatus {
        run(|| {
            let Some(slot) = (unsafe { slot(token) }) else {
                return W2R_E_INVALID;
            };
            if !known_event(id) {
                return W2R_E_NOT_FOUND;
            }
            slot.subs.borrow_mut().remove(&id);
            W2R_OK
        })
    }

    pub unsafe extern "C" fn symbol_lookup(
        token: *mut c_void,
        name: W2rStr,
        out: *mut W2rHandle,
    ) -> W2rStatus {
        run(|| {
            let Some(slot) = (unsafe { slot(token) }) else {
                return W2R_E_INVALID;
            };
            if let Err(e) = require(slot, "symbol_lookup", caps::GAME_READ) {
                return e;
            }
            if out.is_null() {
                return W2R_E_INVALID;
            }
            if name.len > W2R_MAX_SYMBOL_NAME {
                return W2R_E_LIMIT;
            }
            let name = match unsafe { text(name) } {
                Ok(t) => t,
                Err(e) => return e,
            };
            if let Err(e) = game(slot) {
                return e;
            }
            match Symbols::handle(name) {
                Some(h) => {
                    unsafe { *out = h };
                    W2R_OK
                }
                None => W2R_E_NOT_FOUND,
            }
        })
    }

    pub unsafe extern "C" fn symbol_read(
        token: *mut c_void,
        symbol: W2rHandle,
        offset: u32,
        dst: *mut c_void,
        len: u32,
    ) -> W2rStatus {
        run(|| {
            let Some(slot) = (unsafe { slot(token) }) else {
                return W2R_E_INVALID;
            };
            if let Err(e) = require(slot, "symbol_read", caps::GAME_READ) {
                return e;
            }
            if dst.is_null() {
                return W2R_E_INVALID;
            }
            if len > W2R_MAX_READ_BYTES {
                return W2R_E_LIMIT;
            }
            if len == 0 {
                return W2R_OK;
            }
            let game = match game(slot) {
                Ok(g) => g,
                Err(e) => return e,
            };
            let Some(sym) = Symbols::by_handle(symbol) else {
                return W2R_E_NOT_FOUND;
            };
            let Some(addr) = game.symbols.address_of(sym).checked_add(offset as usize) else {
                return W2R_E_INVALID;
            };
            if !game.image.contains(addr, len as usize) {
                return W2R_E_INVALID;
            }
            // Both sides can fault (the game page could be unmapped, dst is plugin memory); the
            // guard turns either into E_INVALID.
            match guarded(|| unsafe {
                core::ptr::copy_nonoverlapping(addr as *const u8, dst.cast::<u8>(), len as usize)
            }) {
                Ok(()) => W2R_OK,
                Err(_) => W2R_E_INVALID,
            }
        })
    }

    /// `target` is a `symbol_lookup` handle of a function symbol; `original_out` receives the
    /// trampoline before the hook goes live.
    pub unsafe extern "C" fn hook_install(
        token: *mut c_void,
        target: u32,
        detour: *mut c_void,
        original_out: *mut *mut c_void,
        out: *mut W2rHandle,
    ) -> W2rStatus {
        run(|| {
            let Some(slot) = (unsafe { slot(token) }) else {
                return W2R_E_INVALID;
            };
            if let Err(e) = require(slot, "hook_install", caps::HOOKS_RAW) {
                return e;
            }
            if detour.is_null() || out.is_null() {
                return W2R_E_INVALID;
            }
            let game = match game(slot) {
                Ok(g) => g,
                Err(e) => return e,
            };
            let Some(sym) = Symbols::by_handle(target) else {
                return W2R_E_NOT_FOUND;
            };
            if sym.kind != Kind::Function {
                return W2R_E_INVALID;
            }
            let addr = game.symbols.address_of(sym);
            let label = format!("{}:{}", slot.id, sym.name);
            let publish = |original: *mut c_void| {
                if !original_out.is_null() {
                    unsafe { *original_out = original };
                }
            };
            let result = install(slot.owner(), &game.image, addr, detour, &label, publish);
            match result {
                Ok(handle) => {
                    unsafe { *out = handle };
                    slot.log(W2R_LOG_INFO, &format!("hooked {} @ {addr:#010x}", sym.name));
                    W2R_OK
                }
                Err(HookError::NotCode) => W2R_E_INVALID,
                Err(HookError::Conflict(with)) => {
                    slot.log(
                        W2R_LOG_WARN,
                        &format!("hook_install {}: overlaps {with}", sym.name),
                    );
                    W2R_E_CONFLICT
                }
                Err(HookError::Limit) => W2R_E_LIMIT,
                Err(HookError::NotFound) => W2R_E_NOT_FOUND,
                Err(HookError::Engine(status)) => {
                    slot.log(
                        W2R_LOG_ERROR,
                        &format!("hook_install {}: MH_STATUS {status}", sym.name),
                    );
                    W2R_E_INVALID
                }
            }
        })
    }

    #[cfg(windows)]
    fn install(
        owner: Owner,
        image: &Image,
        addr: usize,
        detour: *mut c_void,
        label: &str,
        publish: impl FnOnce(*mut c_void),
    ) -> Result<W2rHandle, HookError> {
        // SAFETY: the plugin asked for it with `hooks.raw`; a wrong detour is its own crash
        // (docs/abi.md, detours are outside the guard).
        unsafe { hooks::global().install(owner, image, addr, detour, label, publish) }
    }

    #[cfg(not(windows))]
    fn install(
        _: Owner,
        _: &Image,
        _: usize,
        _: *mut c_void,
        _: &str,
        _: impl FnOnce(*mut c_void),
    ) -> Result<W2rHandle, HookError> {
        Err(HookError::Engine(-1))
    }

    pub unsafe extern "C" fn hook_remove(token: *mut c_void, hook: W2rHandle) -> W2rStatus {
        run(|| {
            let Some(slot) = (unsafe { slot(token) }) else {
                return W2R_E_INVALID;
            };
            if let Err(e) = require(slot, "hook_remove", caps::HOOKS_RAW) {
                return e;
            }
            match hooks::global().remove(slot.owner(), hook) {
                Ok(()) => W2R_OK,
                Err(HookError::NotFound) => W2R_E_NOT_FOUND,
                Err(e) => {
                    slot.log(W2R_LOG_ERROR, &format!("hook_remove: {e:?}"));
                    W2R_E_INVALID
                }
            }
        })
    }

    /// docs/abi.md: printable ASCII, at most 200 bytes, 4 per 5 s, only while chat is available.
    pub unsafe extern "C" fn chat_send(token: *mut c_void, text: W2rStr) -> W2rStatus {
        run(|| {
            let Some(slot) = (unsafe { slot(token) }) else {
                return W2R_E_INVALID;
            };
            if let Err(e) = require(slot, "chat_send", caps::CHAT_WRITE) {
                return e;
            }
            if text.ptr.is_null() || text.len == 0 || text.len > W2R_MAX_CHAT_BYTES {
                return W2R_E_INVALID;
            }
            let bytes =
                unsafe { core::slice::from_raw_parts(text.ptr.cast::<u8>(), text.len as usize) };
            if !chat::valid_text(bytes) {
                return W2R_E_INVALID;
            }
            let line = core::str::from_utf8(bytes).expect("printable ASCII");
            let chat = &slot.shared.services.chat;
            if !chat.available() {
                return W2R_E_STATE;
            }
            if !slot.chat_rate.borrow_mut().allow(chat::now_ms()) {
                return W2R_E_RATE;
            }
            if chat.send(line) { W2R_OK } else { W2R_E_STATE }
        })
    }

    /// docs/abi.md: 1 to 960 bytes, lobby only with the session connected, the target (0-7 or
    /// `W2R_SLOT_ALL`) must have announced the handshake, 32/s per plugin and 128/s overall.
    pub unsafe extern "C" fn peer_send(
        token: *mut c_void,
        slot_or_all: u32,
        payload: W2rBuf,
    ) -> W2rStatus {
        run(|| {
            let Some(slot) = (unsafe { slot(token) }) else {
                return W2R_E_INVALID;
            };
            if let Err(e) = require(slot, "peer_send", caps::NET_SEND) {
                return e;
            }
            if payload.ptr.is_null() || payload.len == 0 {
                return W2R_E_INVALID;
            }
            if payload.len > W2R_MAX_PEER_PAYLOAD {
                return W2R_E_LIMIT;
            }
            if slot_or_all != W2R_SLOT_ALL && slot_or_all >= peer::MAX_SLOTS {
                return W2R_E_INVALID;
            }
            let services = &slot.shared.services;
            if !services.presence.in_lobby() || !services.transport.connected() {
                return W2R_E_STATE;
            }
            let target = match peer::resolve_target(&*services.roster, slot_or_all) {
                Ok(t) => t,
                Err(TargetError::Invalid) => return W2R_E_INVALID,
                Err(TargetError::NotAnnounced) => return W2R_E_STATE,
            };
            let now = chat::now_ms();
            if !slot.peer_rate.borrow_mut().allow(now)
                || !slot.shared.peer_rate.borrow_mut().allow(now)
            {
                return W2R_E_RATE;
            }
            let bytes = unsafe { core::slice::from_raw_parts(payload.ptr, payload.len as usize) };
            let msg = peer::encode(slot.tag, bytes).expect("length checked");
            let sent = match target {
                Target::All => services.transport.broadcast(&msg),
                Target::Slot(n) => services.transport.send_to(n, &msg),
            };
            if sent { W2R_OK } else { W2R_E_STATE }
        })
    }

    // ---- not implemented yet; each is a later Phase 2 step ------------------------------------

    pub unsafe extern "C" fn settings_get_i64(_: *mut c_void, _: W2rStr, _: *mut i64) -> W2rStatus {
        W2R_E_UNSUPPORTED
    }
    pub unsafe extern "C" fn settings_set_i64(_: *mut c_void, _: W2rStr, _: i64) -> W2rStatus {
        W2R_E_UNSUPPORTED
    }
    pub unsafe extern "C" fn settings_get_str(
        _: *mut c_void,
        _: W2rStr,
        _: *mut c_char,
        _: u32,
        _: *mut u32,
    ) -> W2rStatus {
        W2R_E_UNSUPPORTED
    }
    pub unsafe extern "C" fn settings_set_str(_: *mut c_void, _: W2rStr, _: W2rStr) -> W2rStatus {
        W2R_E_UNSUPPORTED
    }
    pub unsafe extern "C" fn data_open(
        _: *mut c_void,
        _: W2rStr,
        _: u32,
        _: *mut W2rHandle,
    ) -> W2rStatus {
        W2R_E_UNSUPPORTED
    }
    pub unsafe extern "C" fn data_read(
        _: *mut c_void,
        _: W2rHandle,
        _: *mut c_void,
        _: u32,
        _: *mut u32,
    ) -> W2rStatus {
        W2R_E_UNSUPPORTED
    }
    pub unsafe extern "C" fn data_write(
        _: *mut c_void,
        _: W2rHandle,
        _: *const c_void,
        _: u32,
    ) -> W2rStatus {
        W2R_E_UNSUPPORTED
    }
    pub unsafe extern "C" fn data_close(_: *mut c_void, _: W2rHandle) -> W2rStatus {
        W2R_E_UNSUPPORTED
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::Registry;
    use crate::target::symbols::SYMBOLS;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Lines(RefCell<Vec<String>>);
    impl Log for Lines {
        fn line(&self, level: u32, plugin: &str, msg: &str) {
            self.0
                .borrow_mut()
                .push(format!("[{level}] {plugin}: {msg}"));
        }
    }

    unsafe extern "C" fn on_load(_: *const W2rHost) -> W2rStatus {
        W2R_OK
    }
    static T: W2rPlugin = W2rPlugin {
        struct_size: W2R_PLUGIN_V1_SIZE,
        on_load: Some(on_load),
        on_event: None,
        on_ui: None,
        on_unload: None,
    };
    unsafe extern "C" fn entry(_: *const W2rHost) -> *const W2rPlugin {
        &T
    }

    fn s(text: &str) -> W2rStr {
        W2rStr {
            ptr: text.as_ptr().cast(),
            len: text.len() as u32,
        }
    }

    #[test]
    fn capabilities_gate_the_functions_and_are_logged_once() {
        let log = Rc::new(Lines::default());
        let mut reg = Registry::new(log.clone());
        let i = reg.load_entry("p", entry).unwrap();
        let h = unsafe { &*reg.slot(i).host() };
        let mut handle = 0;
        let mut buf = [0u8; 4];
        unsafe {
            assert_eq!(
                (h.symbol_lookup)(h.token, s("ProcessNetworkTurn"), &mut handle),
                W2R_E_DENIED
            );
            assert_eq!(
                (h.symbol_lookup)(h.token, s("ProcessNetworkTurn"), &mut handle),
                W2R_E_DENIED
            );
            assert_eq!(
                (h.symbol_read)(h.token, 1, 0, buf.as_mut_ptr().cast(), 4),
                W2R_E_DENIED
            );
            assert_eq!(
                (h.hook_install)(
                    h.token,
                    1,
                    buf.as_mut_ptr().cast(),
                    core::ptr::null_mut(),
                    &mut handle
                ),
                W2R_E_DENIED
            );
            assert_eq!((h.hook_remove)(h.token, 1), W2R_E_DENIED);
            assert_eq!((h.subscribe)(h.token, W2R_EVT_CHAT_RECEIVED), W2R_E_DENIED);
            assert_eq!((h.subscribe)(h.token, W2R_EVT_PEER_MESSAGE), W2R_E_DENIED);
            assert_eq!((h.subscribe)(h.token, W2R_EVT_LOBBY_ENTER), W2R_OK);
        }
        let lines = log.0.borrow();
        let denials: Vec<&String> = lines
            .iter()
            .filter(|l| l.contains("not approved"))
            .collect();
        assert_eq!(denials.len(), 5, "{denials:?}");
        assert_eq!(
            denials[0],
            "[2] p: symbol_lookup: capability game.read not approved"
        );
        assert!(denials[4].contains("subscribe: capability chat.read"));
    }

    #[test]
    fn without_a_game_symbol_functions_report_state_after_validation() {
        let log = Rc::new(Lines::default());
        let mut reg = Registry::new(log);
        let i = reg
            .load_entry_with_caps("p", entry, &[caps::GAME_READ, caps::HOOKS_RAW])
            .unwrap();
        let h = unsafe { &*reg.slot(i).host() };
        let mut handle = 0;
        let mut buf = [0u8; 4];
        let long = "x".repeat(W2R_MAX_SYMBOL_NAME as usize + 1);
        unsafe {
            assert_eq!(
                (h.symbol_lookup)(h.token, s("x"), core::ptr::null_mut()),
                W2R_E_INVALID
            );
            assert_eq!(
                (h.symbol_lookup)(h.token, s(&long), &mut handle),
                W2R_E_LIMIT
            );
            assert_eq!(
                (h.symbol_lookup)(h.token, s("ProcessNetworkTurn"), &mut handle),
                W2R_E_STATE
            );
            assert_eq!(
                (h.symbol_read)(h.token, 1, 0, core::ptr::null_mut(), 4),
                W2R_E_INVALID
            );
            assert_eq!(
                (h.symbol_read)(
                    h.token,
                    1,
                    0,
                    buf.as_mut_ptr().cast(),
                    W2R_MAX_READ_BYTES + 1
                ),
                W2R_E_LIMIT
            );
            assert_eq!(
                (h.symbol_read)(h.token, 1, 0, buf.as_mut_ptr().cast(), 0),
                W2R_OK
            );
            assert_eq!(
                (h.symbol_read)(h.token, 1, 0, buf.as_mut_ptr().cast(), 4),
                W2R_E_STATE
            );
            assert_eq!(
                (h.hook_install)(
                    h.token,
                    1,
                    buf.as_mut_ptr().cast(),
                    core::ptr::null_mut(),
                    &mut handle
                ),
                W2R_E_STATE
            );
            assert_eq!((h.hook_remove)(h.token, 99), W2R_E_NOT_FOUND);
        }
    }

    /// Uses this test process as the "game": its own image, with the symbol table relocated to
    /// its base. Reads land inside the test binary, which is all `symbol_read` has to check.
    #[cfg(windows)]
    #[test]
    fn symbol_lookup_and_read_against_a_live_image() {
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        let base = unsafe { GetModuleHandleW(core::ptr::null()) } as usize;
        let image = Image::from_module(base).unwrap();
        let log = Rc::new(Lines::default());
        let mut reg = Registry::with_game(
            log,
            Game {
                image: image.clone(),
                symbols: Symbols::new(base),
            },
        );
        let i = reg
            .load_entry_with_caps("p", entry, &[caps::GAME_READ, caps::HOOKS_RAW])
            .unwrap();
        let h = unsafe { &*reg.slot(i).host() };

        // The lowest symbol is the one most likely to fall inside a smaller image.
        let lowest = SYMBOLS.iter().min_by_key(|s| s.static_addr).unwrap();
        let rva = lowest.static_addr as usize - crate::target::PREFERRED_IMAGE_BASE;
        assert!(
            image.contains(base + rva, 16),
            "test binary smaller than {rva:#x}; pick another symbol"
        );

        let mut handle = 0;
        let mut buf = [0u8; 16];
        unsafe {
            assert_eq!(
                (h.symbol_lookup)(h.token, s("NoSuchSymbol"), &mut handle),
                W2R_E_NOT_FOUND
            );
            assert_eq!(
                (h.symbol_lookup)(h.token, s(lowest.name), &mut handle),
                W2R_OK
            );
            assert_eq!(Symbols::by_handle(handle).unwrap().name, lowest.name);
            assert_eq!(
                (h.symbol_read)(h.token, handle, 0, buf.as_mut_ptr().cast(), 16),
                W2R_OK
            );
            let expected = core::slice::from_raw_parts((base + rva) as *const u8, 16);
            assert_eq!(&buf, expected);
            assert_eq!(
                (h.symbol_read)(h.token, 0, 0, buf.as_mut_ptr().cast(), 16),
                W2R_E_NOT_FOUND
            );
            assert_eq!(
                (h.symbol_read)(h.token, handle, 0x7FFF_FFFF, buf.as_mut_ptr().cast(), 16),
                W2R_E_INVALID,
                "past the image"
            );
            // Hooking a global is refused; a function handle gets as far as the code check.
            let global = SYMBOLS.iter().find(|s| s.kind == Kind::Object).unwrap();
            let gh = Symbols::handle(global.name).unwrap();
            assert_eq!(
                (h.hook_install)(
                    h.token,
                    gh,
                    buf.as_mut_ptr().cast(),
                    core::ptr::null_mut(),
                    &mut handle
                ),
                W2R_E_INVALID
            );
            assert_eq!(
                (h.hook_install)(
                    h.token,
                    0,
                    buf.as_mut_ptr().cast(),
                    core::ptr::null_mut(),
                    &mut handle
                ),
                W2R_E_NOT_FOUND
            );
        }
    }
}
