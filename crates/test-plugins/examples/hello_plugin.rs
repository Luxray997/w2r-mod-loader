//! Logs on load, and on lobby entry says hello in the lobby chat (needs `chat.write`). The
//! Phase 2 "hello world".

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
    if id == W2R_EVT_LOBBY_ENTER {
        let status = tp::chat_send("hello from w2rpm");
        tp::log(W2R_LOG_INFO, &format!("chat_send returned {status}"));
    }
}

static PLUGIN: W2rPlugin = tp::table(on_load, Some(on_event));

#[unsafe(no_mangle)]
pub unsafe extern "C" fn w2r_plugin_main(_host: *const W2rHost) -> *const W2rPlugin {
    &PLUGIN
}
