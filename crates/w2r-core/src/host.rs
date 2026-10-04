//! The per-plugin host table (`W2rHost`) and the implicit capabilities it serves today: `log`,
//! `subscribe`, `unsubscribe`. Everything else returns `W2R_E_UNSUPPORTED` until its Phase 2
//! step lands; a plugin must handle that status anyway (older hosts).
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
    pub id: String,
    /// Empty until `w2r_plugin_main` returned a valid table.
    pub table: Cell<W2rPlugin>,
    state: RefCell<State>,
    subs: RefCell<BTreeSet<W2rEventId>>,
    log: Rc<dyn Log>,
    host: W2rHost,
}

impl Slot {
    pub fn new(id: &str, log: Rc<dyn Log>) -> Box<Slot> {
        let mut slot = Box::new(Slot {
            id: id.to_owned(),
            table: Cell::new(EMPTY_TABLE),
            state: RefCell::new(State::Active),
            subs: RefCell::new(BTreeSet::new()),
            log,
            host: host_table(core::ptr::null_mut()),
        });
        let token: *mut Slot = &mut *slot;
        slot.host.token = token.cast();
        slot
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

    pub fn log(&self, level: u32, msg: &str) {
        self.log.line(level, &self.id, msg);
    }

    /// Stops every future call into the plugin and drops its subscriptions.
    /// TODO(phase 2, hooks): also remove its hooks.
    pub fn disable(&self, reason: String) {
        self.log(W2R_LOG_ERROR, &format!("disabled: {reason}"));
        *self.state.borrow_mut() = State::Disabled { reason };
        self.subs.borrow_mut().clear();
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
    pub unsafe extern "C" fn symbol_lookup(
        _: *mut c_void,
        _: W2rStr,
        _: *mut W2rHandle,
    ) -> W2rStatus {
        W2R_E_UNSUPPORTED
    }
    pub unsafe extern "C" fn symbol_read(
        _: *mut c_void,
        _: W2rHandle,
        _: u32,
        _: *mut c_void,
        _: u32,
    ) -> W2rStatus {
        W2R_E_UNSUPPORTED
    }
    pub unsafe extern "C" fn chat_send(_: *mut c_void, _: W2rStr) -> W2rStatus {
        W2R_E_UNSUPPORTED
    }
    pub unsafe extern "C" fn peer_send(_: *mut c_void, _: u32, _: W2rBuf) -> W2rStatus {
        W2R_E_UNSUPPORTED
    }
    pub unsafe extern "C" fn hook_install(
        _: *mut c_void,
        _: u32,
        _: *mut c_void,
        _: *mut *mut c_void,
        _: *mut W2rHandle,
    ) -> W2rStatus {
        W2R_E_UNSUPPORTED
    }
    pub unsafe extern "C" fn hook_remove(_: *mut c_void, _: W2rHandle) -> W2rStatus {
        W2R_E_UNSUPPORTED
    }
}
