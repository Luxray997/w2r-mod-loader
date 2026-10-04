//! Exports `w2r_plugin_main` but returns null: load-policy check 9, `E_ENTRY`.

use w2r_abi::*;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn w2r_plugin_main(_host: *const W2rHost) -> *const W2rPlugin {
    core::ptr::null()
}
