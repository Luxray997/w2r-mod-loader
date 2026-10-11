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

pub fn install(image: &Image, base: usize, log: &dyn Log) -> bool {
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
        log.line(
            W2R_LOG_ERROR,
            "host",
            &format!("game info patch: {e:?}"),
        );
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
    if unsafe { VirtualProtect(addr as *const _, bytes.len(), PAGE_EXECUTE_READWRITE, &mut old) }
        == 0
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
        for flags in [0u32, 0x1000, 0x2000, 0x3000, 0x2_0000, 0x200, 0x1000 | 0x200 | 0x4000] {
            assert_eq!(model(flags), crate::lobbylayout::resources_index(flags), "{flags:#x}");
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
