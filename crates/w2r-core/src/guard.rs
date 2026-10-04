//! The crash guard: every call into a plugin runs inside [`guarded`].
//!
//! Contract (docs/abi.md, "Crash and unload contract"): a plugin that panics or faults must not
//! take the game down; the host logs the reason and disables the plugin for the session.
//!
//! Mechanism, decided by the Phase 2 spike (the tests in this file are that spike):
//!
//! 1. Outermost, a structured-exception-handling frame from `microseh` (`__try`/`__except` in a
//!    small C stub compiled by MSVC). This catches hardware faults: access violations, `int3`,
//!    illegal instructions, and so on. A caught fault abandons every Rust frame between the
//!    stub and the faulting instruction without running destructors, so [`guarded`] must own no
//!    `Drop` state of its own and callers must not create any inside the closure that they need
//!    cleaned up (the spec already says the host never relies on plugin cleanup).
//! 2. Innermost, `std::panic::catch_unwind`. microseh's trampoline is `extern "system"`, so a
//!    panic that reached it would abort the process; the panic catch therefore sits *inside*
//!    the SEH frame, never outside it. The `__except` filter accepts every code, including the
//!    C++ exception code Rust panics use on MSVC (`0xE06D7363`), which is another reason a
//!    panic must be caught before it gets there.
//!
//! Known limits, by design:
//! - Stack overflow is not recoverable (the guard page is gone); the process dies.
//! - A fault *during* a panic unwind leaves the runtime inconsistent; we do not try to continue
//!   from that.
//! - Hook detours (`hooks.raw`) run outside the guard; the spec says so.
//! - This is crash containment, not a security boundary. A native plugin can corrupt anything.

use std::any::Any;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Why a guarded call did not return normally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// A Rust panic inside the call. Carries the panic message when it was a string.
    Panic(String),
    /// A hardware/OS exception. `code` is the NT status (`0xC0000005` = access violation).
    Exception { code: u32, address: usize },
}

impl Fault {
    pub const ACCESS_VIOLATION: u32 = 0xC000_0005;
    pub const BREAKPOINT: u32 = 0x8000_0003;
    pub const ILLEGAL_INSTRUCTION: u32 = 0xC000_001D;
    /// MSVC C++ `throw` (also what Rust panics use on MSVC). Game code called from a guarded
    /// region can raise it; it is reported by name so the log does not blame a fault.
    pub const CPP_EXCEPTION: u32 = 0xE06D_7363;
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fault::Panic(msg) => write!(f, "panic: {msg}"),
            Fault::Exception { code, address } => {
                let name = match *code {
                    Self::ACCESS_VIOLATION => "access violation",
                    Self::BREAKPOINT => "breakpoint",
                    Self::ILLEGAL_INSTRUCTION => "illegal instruction",
                    Self::CPP_EXCEPTION => "C++ exception",
                    _ => "exception",
                };
                write!(f, "{name} 0x{code:08X} at 0x{address:X}")
            }
        }
    }
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_owned()
    }
}

/// Runs `f`, containing panics and (on Windows) hardware faults.
///
/// Returns `Ok(value)` on normal return. On a fault the closure's frames are abandoned (see the
/// module docs); the caller is expected to disable the plugin that faulted.
pub fn guarded<R>(f: impl FnOnce() -> R) -> Result<R, Fault> {
    // FnOnce -> FnMut adapter; microseh wants FnMut and calls it at most once.
    let mut f = Some(f);
    // The `take` sits inside `catch_unwind`: `run` executes in microseh's `extern "system"`
    // trampoline, where a panic would abort instead of being caught.
    let run = move || {
        catch_unwind(AssertUnwindSafe(|| {
            f.take().expect("guarded closure runs once")()
        }))
    };
    match seh(run) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(payload)) => Err(Fault::Panic(panic_message(payload))),
        Err(fault) => Err(fault),
    }
}

#[cfg(windows)]
fn seh<R>(f: impl FnMut() -> R) -> Result<R, Fault> {
    microseh::try_seh(f).map_err(|e| Fault::Exception {
        code: e.raw_code(),
        address: e.address() as usize,
    })
}

/// Off Windows there is no SEH; a hardware fault is fatal. Panics are still contained, which is
/// enough for the pure-Rust parts of the host to be unit-tested on Linux CI.
#[cfg(not(windows))]
fn seh<R>(mut f: impl FnMut() -> R) -> Result<R, Fault> {
    Ok(f())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    #[test]
    fn normal_return_passes_the_value_through() {
        assert_eq!(guarded(|| 41 + 1), Ok(42));
    }

    #[test]
    fn panic_is_contained_with_its_message() {
        let r = guarded(|| -> i32 { panic!("boom {}", 7) });
        assert_eq!(r, Err(Fault::Panic("boom 7".into())));
    }

    #[test]
    fn panic_from_a_deeper_rust_frame_is_reported_as_a_panic_not_an_exception() {
        fn level3() -> u8 {
            panic!("deep")
        }
        fn level2() -> u8 {
            level3() + 1
        }
        fn level1() -> u8 {
            level2() + 1
        }
        assert_eq!(guarded(level1), Err(Fault::Panic("deep".into())));
    }

    #[cfg(windows)]
    fn null_write() {
        // black_box keeps the compiler from turning this into a ud2 / unreachable.
        let p: *mut u32 = std::hint::black_box(ptr::null_mut());
        unsafe { ptr::write_volatile(p, 1) };
    }

    #[cfg(windows)]
    fn code_of<R: fmt::Debug>(r: Result<R, Fault>) -> u32 {
        match r {
            Err(Fault::Exception { code, .. }) => code,
            other => panic!("expected an exception, got {other:?}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn access_violation_is_contained() {
        assert_eq!(code_of(guarded(null_write)), Fault::ACCESS_VIOLATION);
    }

    #[cfg(all(windows, any(target_arch = "x86", target_arch = "x86_64")))]
    #[test]
    fn breakpoint_trap_is_contained() {
        let r = guarded(|| unsafe { core::arch::asm!("int3") });
        assert_eq!(code_of(r), Fault::BREAKPOINT);
    }

    #[cfg(all(windows, any(target_arch = "x86", target_arch = "x86_64")))]
    #[test]
    fn illegal_instruction_is_contained() {
        let r = guarded(|| unsafe { core::arch::asm!("ud2") });
        assert_eq!(code_of(r), Fault::ILLEGAL_INSTRUCTION);
    }

    #[cfg(windows)]
    #[test]
    fn the_guard_keeps_working_after_repeated_faults() {
        for _ in 0..50 {
            assert_eq!(code_of(guarded(null_write)), Fault::ACCESS_VIOLATION);
            assert_eq!(guarded(|| 1), Ok(1));
            assert!(matches!(guarded(|| panic!("p")), Err(Fault::Panic(_))));
        }
    }

    #[cfg(windows)]
    #[test]
    fn nested_guards_contain_the_inner_fault_only() {
        let outer = guarded(|| {
            let inner = guarded(null_write);
            assert_eq!(code_of(inner), Fault::ACCESS_VIOLATION);
            "outer survived"
        });
        assert_eq!(outer, Ok("outer survived"));
    }

    #[cfg(windows)]
    #[test]
    fn a_fault_in_a_nested_panic_catch_is_still_contained() {
        // catch_unwind frames between the SEH stub and the fault get abandoned; make sure that
        // does not break anything observable.
        let r = guarded(|| {
            let _ = catch_unwind(|| {
                let _ = catch_unwind(null_write);
            });
            unreachable!("the fault should have left the closure")
        });
        assert_eq!(code_of(r), Fault::ACCESS_VIOLATION);
        assert_eq!(guarded(|| 2), Ok(2));
    }

    /// The real shape: a plugin callback reached through a `W2rPlugin` table, faulting in C ABI
    /// code with no Rust frames of its own.
    #[cfg(windows)]
    #[test]
    fn fault_through_a_plugin_table_function_pointer_is_contained() {
        use w2r_abi::{W2R_EVT_LOBBY_ENTER, W2R_PLUGIN_V1_SIZE, W2rEvent, W2rPlugin};

        unsafe extern "C" fn crashing_on_event(ev: *const W2rEvent) {
            // Reads the event (valid), then writes through its null payload (fault).
            let ev = unsafe { &*ev };
            let p = std::hint::black_box(ev.payload as *mut u32);
            unsafe { ptr::write_volatile(p, ev.id) };
        }
        unsafe extern "C" fn fine_on_event(_: *const W2rEvent) {}

        let event = W2rEvent {
            struct_size: size_of::<W2rEvent>() as u32,
            id: W2R_EVT_LOBBY_ENTER,
            payload: ptr::null(),
        };
        let bad = W2rPlugin {
            struct_size: W2R_PLUGIN_V1_SIZE,
            on_load: None,
            on_event: Some(crashing_on_event),
            on_ui: None,
            on_unload: None,
        };
        let good = W2rPlugin {
            on_event: Some(fine_on_event),
            ..bad
        };

        let r = guarded(|| unsafe { (bad.on_event.unwrap())(&event) });
        assert_eq!(code_of(r), Fault::ACCESS_VIOLATION);
        assert_eq!(
            guarded(|| unsafe { (good.on_event.unwrap())(&event) }),
            Ok(())
        );
    }

    #[test]
    fn fault_display_is_readable() {
        let f = Fault::Exception {
            code: Fault::ACCESS_VIOLATION,
            address: 0xDEAD,
        };
        assert_eq!(f.to_string(), "access violation 0xC0000005 at 0xDEAD");
        assert_eq!(Fault::Panic("x".into()).to_string(), "panic: x");
    }
}
