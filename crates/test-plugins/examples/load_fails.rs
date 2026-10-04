//! A valid table whose `on_load` refuses: load-policy check 10, `E_LOAD`.

use w2r_abi::*;
use w2r_test_plugins as tp;

unsafe extern "C" fn on_load(host: *const W2rHost) -> W2rStatus {
    tp::set_host(host);
    tp::log(W2R_LOG_WARN, "refusing to load");
    -42
}

static PLUGIN: W2rPlugin = tp::table(on_load, None);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn w2r_plugin_main(_host: *const W2rHost) -> *const W2rPlugin {
    &PLUGIN
}
