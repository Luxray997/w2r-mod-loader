//! Exercises the Phase 3 host functions from a real DLL and logs each result, so a run in the
//! game (`tools\install_dev.ps1 -WithHostCheck`) can be read off `w2r.log`: settings round trip,
//! a data file written and read back, `symbol_address` for a function and a global, and the
//! match-start event. Needs `game.read` and `hooks.raw`. It installs no hook.

use w2r_abi::*;
use w2r_test_plugins as tp;

fn host() -> &'static W2rHost {
    unsafe { &*HOST }
}

static mut HOST: *const W2rHost = core::ptr::null();

fn note(text: &str) {
    tp::log(W2R_LOG_INFO, text);
}

fn check_settings() {
    let h = host();
    unsafe {
        let set = (h.settings_set_i64)(h.token, tp::str("check.n"), 41);
        let bump = (h.settings_set_str)(h.token, tp::str("check.s"), tp::str("seen"));
        let mut n = 0i64;
        let get = (h.settings_get_i64)(h.token, tp::str("check.n"), &mut n);
        let mut buf = [0u8; 16];
        let mut len = 0u32;
        let gets = (h.settings_get_str)(
            h.token,
            tp::str("check.s"),
            buf.as_mut_ptr().cast(),
            16,
            &mut len,
        );
        note(&format!(
            "settings: set {set}/{bump}, get {get} n={n}, get_str {gets} s={:?}",
            String::from_utf8_lossy(&buf[..len.min(16) as usize])
        ));
    }
}

fn check_data() {
    let h = host();
    unsafe {
        let mut f = 0;
        let open = (h.data_open)(h.token, tp::str("check.txt"), W2R_FILE_WRITE, &mut f);
        let line = b"host check\n";
        let wrote = (h.data_write)(h.token, f, line.as_ptr().cast(), line.len() as u32);
        let closed = (h.data_close)(h.token, f);
        let mut r = 0;
        let reopen = (h.data_open)(h.token, tp::str("check.txt"), W2R_FILE_READ, &mut r);
        let mut buf = [0u8; 32];
        let mut got = 0u32;
        let read = (h.data_read)(h.token, r, buf.as_mut_ptr().cast(), 32, &mut got);
        let _ = (h.data_close)(h.token, r);
        note(&format!(
            "data: open {open}, write {wrote}, close {closed}, reopen {reopen}, read {read} got {got} {:?}",
            String::from_utf8_lossy(&buf[..got.min(32) as usize])
        ));
    }
}

fn check_symbols() {
    let h = host();
    for name in ["BeginNamedWindow", "LocalPlayer", "ChatTargetMode"] {
        unsafe {
            let mut handle = 0;
            let found = (h.symbol_lookup)(h.token, tp::str(name), &mut handle);
            let mut addr = 0usize;
            let status = (h.symbol_address)(h.token, handle, &mut addr);
            note(&format!(
                "symbol {name}: lookup {found}, address {status} {addr:#010x}"
            ));
        }
    }
}

unsafe extern "C" fn on_load(host: *const W2rHost) -> W2rStatus {
    unsafe { HOST = host };
    tp::set_host(host);
    note("host-check: start");
    check_settings();
    check_data();
    check_symbols();
    tp::subscribe(W2R_EVT_MATCH_START)
}

unsafe extern "C" fn on_event(event: *const W2rEvent) {
    let id = unsafe { (*event).id };
    if id == W2R_EVT_MATCH_START {
        note("host-check: MATCH_START");
    }
}

static TABLE: W2rPlugin = tp::table(on_load, Some(on_event));

#[unsafe(no_mangle)]
pub unsafe extern "C" fn w2r_plugin_main(_host: *const W2rHost) -> *const W2rPlugin {
    &TABLE
}
