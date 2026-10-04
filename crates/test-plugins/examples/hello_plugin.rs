//! Logs on load and on lobby events, and shows a few widgets on the plugin screen. The Phase 2
//! "hello world"; it no longer posts to the lobby chat.

use w2r_abi::*;
use w2r_test_plugins as tp;

unsafe extern "C" fn on_load(host: *const W2rHost) -> W2rStatus {
    tp::set_host(host);
    tp::log(W2R_LOG_INFO, "hello from hello_plugin");
    tp::subscribe(W2R_EVT_LOBBY_LEAVE);
    tp::subscribe(W2R_EVT_LOBBY_ENTER)
}

unsafe extern "C" fn on_event(event: *const W2rEvent) {
    let id = unsafe { (*event).id };
    tp::log(W2R_LOG_INFO, &format!("event {id}"));
}

static GREET: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1);
static VOLUME: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(5);

unsafe extern "C" fn on_ui(ui: *const W2rUi) {
    use std::sync::atomic::Ordering::Relaxed;
    let ui = unsafe { &*ui };
    unsafe {
        (ui.label)(ui.ctx, tp::str("hello_plugin says hi"));
        let mut greet = GREET.load(Relaxed);
        if (ui.checkbox)(ui.ctx, tp::str("Example option"), &mut greet) != 0 {
            GREET.store(greet, Relaxed);
            tp::log(W2R_LOG_INFO, &format!("greet set to {greet}"));
        }
        let mut volume = VOLUME.load(Relaxed);
        if (ui.slider_int)(ui.ctx, tp::str("Enthusiasm"), &mut volume, 0, 10) != 0 {
            VOLUME.store(volume, Relaxed);
        }
        if (ui.button)(ui.ctx, tp::str("Log a line")) != 0 {
            tp::log(
                W2R_LOG_INFO,
                &format!("button pressed, enthusiasm {volume}"),
            );
        }
    }
}

static PLUGIN: W2rPlugin = tp::table_with_ui(on_load, Some(on_event), Some(on_ui));

#[unsafe(no_mangle)]
pub unsafe extern "C" fn w2r_plugin_main(_host: *const W2rHost) -> *const W2rPlugin {
    &PLUGIN
}
