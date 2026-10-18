//! Fixes the Resources row of the Multiplayer lobbies info panel (the game-info panel beside the
//! game list, `FUN_0053bb50`). The stock code picks the entry of the resources option list
//! (default, random, low, medium, high) with a chain of tests that never matches the match
//! flags the game itself writes (`0x1000` Low, `0x2000` Medium, `0x3000` High, `0x20000`
//! Random), so a Low game reads "Random". This replaces that chain, in place, with the mapping
//! the waiting screen already uses ([`crate::lobbylayout::resources_index`]). x86 only.

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

use crate::hooks::{self, Owner};
use crate::host::Log;
use crate::pe::Image;
use crate::target::PREFERRED_IMAGE_BASE;

/// Static address of the first replaced instruction (`TEST EDI,0x1000`); EDI holds the flags
/// and EAX the option list's data pointer.
const SITE: usize = 0x0053_d27d;

/// The stock instructions being replaced: three `TEST EDI,mask / JZ / MOV EAX,[EAX+n] / JMP`
/// groups and the default `MOV EAX,[EAX]`, up to the instruction at 0x0053d2a6.
const ORIGINAL: [u8; 41] = [
    0xf7, 0xc7, 0x00, 0x10, 0x00, 0x00, 0x74, 0x05, 0x8b, 0x40, 0x04, 0xeb, 0x1c, //
    0xf7, 0xc7, 0x00, 0x20, 0x00, 0x00, 0x74, 0x05, 0x8b, 0x40, 0x08, 0xeb, 0x0f, //
    0xf7, 0xc7, 0x00, 0x30, 0x00, 0x00, 0x74, 0x05, 0x8b, 0x40, 0x0c, 0xeb, 0x02, //
    0x8b, 0x00,
];

/// The replacement: `EAX = EAX[index]` where index is `((flags >> 12) & 3) + 1` when either
/// resource bit is set (Low 2, Medium 3, High 4), else `(flags >> 17) & 1` (Random 1, default 0).
/// Only ECX is clobbered, which is dead here (a call just returned), and the rest is NOPs.
fn replacement() -> [u8; ORIGINAL.len()] {
    let code: [u8; 24] = [
        0x89, 0xf9, // mov ecx, edi
        0xc1, 0xe9, 0x0c, // shr ecx, 12
        0x83, 0xe1, 0x03, // and ecx, 3
        0x74, 0x03, // jz zero
        0x41, // inc ecx
        0xeb, 0x08, // jmp load
        0x89, 0xf9, // zero: mov ecx, edi
        0xc1, 0xe9, 0x11, // shr ecx, 17
        0x83, 0xe1, 0x01, // and ecx, 1
        0x8b, 0x04, 0x88, // load: mov eax, [eax + ecx*4]
    ];
    let mut out = [0x90u8; ORIGINAL.len()];
    out[..code.len()].copy_from_slice(&code);
    out
}

/// The `CALL FUN_0050e1b0` (int to string) that formats the Game Speed row; the stack holds
/// `out, value` (cdecl) and EAX comes back as `out`.
const SPEED_SITE: usize = 0x0053_d1ea;
const SPEED_CALL: [u8; 5] = [0xe8, 0xc1, 0x0f, 0xfd, 0xff];
const INT_TO_STRING: usize = 0x0050_e1b0;
/// `FUN_00547670(out, const char*)`: builds the game's string from a C string (cdecl).
const CSTR_TO_STRING: usize = 0x0054_7670;

static IMAGE_SHIFT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

type ToString<T> = unsafe extern "C" fn(*mut core::ffi::c_void, T) -> *mut core::ffi::c_void;

/// Stands in for the int to string call on the Game Speed row: the speed's name, or the plain
/// number for a value outside the known range.
unsafe extern "C" fn speed_text(out: *mut core::ffi::c_void, value: i32) -> *mut core::ffi::c_void {
    let shift = IMAGE_SHIFT.load(core::sync::atomic::Ordering::Relaxed);
    let name = u8::try_from(value)
        .ok()
        .and_then(crate::lobbylayout::speed_name_c);
    match name {
        Some(n) => {
            // SAFETY: both addresses are the game's functions, checked against the image at install.
            let f: ToString<*const core::ffi::c_char> =
                unsafe { core::mem::transmute(CSTR_TO_STRING + shift) };
            let s = unsafe { f(out, n.as_ptr()) };
            // SAFETY: `out` is the game's MSVC std::string (16-byte buffer or pointer, size at +0x10,
            // capacity at +0x14) that the call just filled.
            unsafe { drop_trailing_colon(s) };
            s
        }
        None => {
            let f: ToString<i32> = unsafe { core::mem::transmute(INT_TO_STRING + shift) };
            unsafe { f(out, value) }
        }
    }
}

/// The game's C string to string helper appends a ":" (it builds the captions); the value must
/// not have one, so cut it back off.
///
/// # Safety
/// `s` must point at a live MSVC `std::string` (x86 layout).
unsafe fn drop_trailing_colon(s: *mut core::ffi::c_void) {
    let base = s.cast::<u8>();
    let size = unsafe { base.add(0x10).cast::<usize>().read() };
    let cap = unsafe { base.add(0x14).cast::<usize>().read() };
    if size == 0 {
        return;
    }
    let data = if cap > 0xf {
        unsafe { base.cast::<*mut u8>().read() }
    } else {
        base
    };
    if unsafe { *data.add(size - 1) } == b':' {
        unsafe {
            *data.add(size - 1) = 0;
            base.add(0x10).cast::<usize>().write(size - 1);
        }
    }
}

/// The new `rel32` for the call at `site` to reach `target`.
fn rel32(site: usize, target: usize) -> [u8; 4] {
    target.wrapping_sub(site.wrapping_add(5)).to_le_bytes()
}

fn install_speed(image: &Image, base: usize, log: &dyn Log) -> bool {
    let shift = base.wrapping_sub(PREFERRED_IMAGE_BASE);
    let site = SPEED_SITE.wrapping_add(shift);
    if !image.is_code(site, SPEED_CALL.len()) {
        log.line(W2R_LOG_ERROR, "host", "game info speed: site is not code");
        return false;
    }
    // SAFETY: `site` is inside the game's mapped image (checked above).
    let current = unsafe { core::slice::from_raw_parts(site as *const u8, SPEED_CALL.len()) };
    if current != SPEED_CALL {
        log.line(
            W2R_LOG_ERROR,
            "host",
            "game info speed: the game's bytes are not the expected ones; left alone",
        );
        return false;
    }
    if let Err(e) =
        hooks::global().reserve(Owner::Host, site, SPEED_CALL.len(), "host:game info speed")
    {
        log.line(W2R_LOG_ERROR, "host", &format!("game info speed: {e:?}"));
        return false;
    }
    IMAGE_SHIFT.store(shift, core::sync::atomic::Ordering::Relaxed);
    let mut patched = [0xe8, 0, 0, 0, 0];
    patched[1..].copy_from_slice(&rel32(site, speed_text as *const () as usize));
    if !unsafe { write_code(site, &patched) } {
        log.line(W2R_LOG_ERROR, "host", "game info speed: could not write");
        return false;
    }
    log.line(
        W2R_LOG_INFO,
        "host",
        &format!("patched game info Game Speed row @ {site:#010x}"),
    );
    true
}

pub fn install(image: &Image, base: usize, log: &dyn Log) -> bool {
    install_speed(image, base, log);
    let site = base + (SITE - PREFERRED_IMAGE_BASE);
    if !image.is_code(site, ORIGINAL.len()) {
        log.line(W2R_LOG_ERROR, "host", "game info patch: site is not code");
        return false;
    }
    // SAFETY: `site` is inside the game's mapped image (checked above).
    let current = unsafe { core::slice::from_raw_parts(site as *const u8, ORIGINAL.len()) };
    if current != ORIGINAL {
        log.line(
            W2R_LOG_ERROR,
            "host",
            "game info patch: the game's bytes are not the expected ones; left alone",
        );
        return false;
    }
    if let Err(e) = hooks::global().reserve(
        Owner::Host,
        site,
        ORIGINAL.len(),
        "host:game info resources",
    ) {
        log.line(W2R_LOG_ERROR, "host", &format!("game info patch: {e:?}"));
        return false;
    }
    if !unsafe { write_code(site, &replacement()) } {
        log.line(W2R_LOG_ERROR, "host", "game info patch: could not write");
        return false;
    }
    log.line(
        W2R_LOG_INFO,
        "host",
        &format!("patched game info Resources row @ {site:#010x}"),
    );
    true
}

/// # Safety
/// `addr..addr+bytes.len()` must be mapped game code that no thread is mid-way through
/// executing (this runs at startup).
unsafe fn write_code(addr: usize, bytes: &[u8]) -> bool {
    use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
    use windows_sys::Win32::System::Memory::{PAGE_EXECUTE_READWRITE, VirtualProtect};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut old = 0u32;
    if unsafe {
        VirtualProtect(
            addr as *const _,
            bytes.len(),
            PAGE_EXECUTE_READWRITE,
            &mut old,
        )
    } == 0
    {
        return false;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, bytes.len());
        let mut ignored = 0u32;
        VirtualProtect(addr as *const _, bytes.len(), old, &mut ignored);
        FlushInstructionCache(GetCurrentProcess(), addr as *const _, bytes.len());
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The patch's arithmetic, as the bytes above compute it, against the waiting screen's map.
    fn model(flags: u32) -> usize {
        let low = (flags >> 12) & 3;
        if low != 0 {
            low as usize + 1
        } else {
            ((flags >> 17) & 1) as usize
        }
    }

    #[test]
    fn the_patch_agrees_with_the_waiting_screens_resources_map() {
        for flags in [
            0u32,
            0x1000,
            0x2000,
            0x3000,
            0x2_0000,
            0x200,
            0x1000 | 0x200 | 0x4000,
        ] {
            assert_eq!(
                model(flags),
                crate::lobbylayout::resources_index(flags),
                "{flags:#x}"
            );
        }
    }

    #[test]
    fn the_replacement_fits_and_jumps_land_on_instruction_boundaries() {
        let r = replacement();
        // jz at 8 skips 3 (inc + jmp) to the zero block at 13; jmp at 11 skips 8 to the load at 21.
        assert_eq!((r[8], r[9]), (0x74, 0x03));
        assert_eq!((r[11], r[12]), (0xeb, 0x08));
        assert_eq!(&r[13..15], &[0x89, 0xf9]);
        assert_eq!(&r[21..24], &[0x8b, 0x04, 0x88]);
        assert_eq!(r.len(), 41);
    }
}
