//! Loads fine, then writes through a null pointer on the first lobby-enter event.

use w2r_abi::*;
use w2r_test_plugins as tp;

unsafe extern "C" fn on_load(host: *const W2rHost) -> W2rStatus {
    tp::set_host(host);
    tp::subscribe(W2R_EVT_LOBBY_ENTER)
}

unsafe extern "C" fn on_event(_event: *const W2rEvent) {
    tp::log(W2R_LOG_WARN, "about to crash");
    let p: *mut u32 = std::hint::black_box(core::ptr::null_mut());
    unsafe { p.write_volatile(1) };
    tp::log(W2R_LOG_ERROR, "unreachable: the write did not fault");
}

static PLUGIN: W2rPlugin = tp::table(on_load, Some(on_event));

#[unsafe(no_mangle)]
pub unsafe extern "C" fn w2r_plugin_main(_host: *const W2rHost) -> *const W2rPlugin {
    &PLUGIN
}
