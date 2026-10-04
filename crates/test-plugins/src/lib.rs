//! Helpers shared by the fixture plugins in `examples/`. Deliberately minimal; the real plugin
//! SDK is `w2r-sdk` (Phase 2+).

use std::sync::atomic::{AtomicPtr, Ordering};

use w2r_abi::*;

static HOST: AtomicPtr<W2rHost> = AtomicPtr::new(core::ptr::null_mut());

pub fn set_host(host: *const W2rHost) {
    HOST.store(host.cast_mut(), Ordering::Relaxed);
}

pub fn str(text: &str) -> W2rStr {
    W2rStr {
        ptr: text.as_ptr().cast(),
        len: text.len() as u32,
    }
}

pub fn log(level: u32, text: &str) -> W2rStatus {
    let host = HOST.load(Ordering::Relaxed);
    if host.is_null() {
        return W2R_E_STATE;
    }
    unsafe { ((*host).log)((*host).token, level, str(text)) }
}

pub fn subscribe(id: W2rEventId) -> W2rStatus {
    let host = HOST.load(Ordering::Relaxed);
    if host.is_null() {
        return W2R_E_STATE;
    }
    unsafe { ((*host).subscribe)((*host).token, id) }
}

pub fn chat_send(text: &str) -> W2rStatus {
    let host = HOST.load(Ordering::Relaxed);
    if host.is_null() {
        return W2R_E_STATE;
    }
    unsafe { ((*host).chat_send)((*host).token, str(text)) }
}

pub const fn table(
    on_load: unsafe extern "C" fn(*const W2rHost) -> W2rStatus,
    on_event: Option<unsafe extern "C" fn(*const W2rEvent)>,
) -> W2rPlugin {
    W2rPlugin {
        struct_size: W2R_PLUGIN_V1_SIZE,
        on_load: Some(on_load),
        on_event,
        on_ui: None,
        on_unload: None,
    }
}
