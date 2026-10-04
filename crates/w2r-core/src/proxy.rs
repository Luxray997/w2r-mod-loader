//! The `version.dll` proxy. The game loads a `version.dll` from its own folder before the real
//! one in System32; ours forwards every export to the real DLL and otherwise just gets the host
//! into the process. Ported from wc2r-plus `target/version_proxy.cpp` (MIT).
//!
//! Each export is a naked function holding one `jmp dword ptr [real::X]`, so arguments and
//! the callee's calling convention pass through untouched. Being ordinary `#[no_mangle]`
//! functions, rustc exports them from the cdylib by itself. [`init`] runs from `DllMain`,
//! before the game can call any of them, and only uses `LoadLibraryW`/`GetProcAddress`, which
//! is allowed there.
//!
//! x86 MSVC only: that is the game's architecture and the only place the exports exist.

use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

macro_rules! forwards {
    ($($name:ident),* $(,)?) => {
        /// Resolved addresses in the real version.dll, one per export.
        pub mod real {
            use super::AtomicUsize;
            $(
                #[allow(non_upper_case_globals)]
                pub static $name: AtomicUsize = AtomicUsize::new(0);
            )*
        }

        $(
            /// Forwarder to the real version.dll export of the same name.
            ///
            /// # Safety
            /// Only the game calls this, with that export's real arguments and calling
            /// convention. The jump target is 0 until [`init`] has resolved it.
            #[unsafe(no_mangle)]
            #[unsafe(naked)]
            #[allow(non_snake_case)]
            pub unsafe extern "C" fn $name() {
                core::arch::naked_asm!("jmp dword ptr [{real}]", real = sym real::$name)
            }
        )*

        /// Export name, NUL-terminated name, and the slot the stub jumps through.
        pub static EXPORTS: &[(&str, &[u8], &AtomicUsize)] = &[
            $( (stringify!($name), concat!(stringify!($name), "\0").as_bytes(), &real::$name), )*
        ];
    };
}

forwards!(
    GetFileVersionInfoA,
    GetFileVersionInfoByHandle,
    GetFileVersionInfoExA,
    GetFileVersionInfoExW,
    GetFileVersionInfoSizeA,
    GetFileVersionInfoSizeExA,
    GetFileVersionInfoSizeExW,
    GetFileVersionInfoSizeW,
    GetFileVersionInfoW,
    VerFindFileA,
    VerFindFileW,
    VerInstallFileA,
    VerInstallFileW,
    VerLanguageNameA,
    VerLanguageNameW,
    VerQueryValueA,
    VerQueryValueW,
);

/// `<System32>\version.dll` as a NUL-terminated wide string.
pub fn real_dll_path() -> Option<Vec<u16>> {
    let mut dir = [0u16; 260];
    let n = unsafe { GetSystemDirectoryW(dir.as_mut_ptr(), dir.len() as u32) } as usize;
    if n == 0 || n >= dir.len() {
        return None;
    }
    let mut path: Vec<u16> = dir[..n].to_vec();
    path.extend("\\version.dll".encode_utf16());
    path.push(0);
    Some(path)
}

/// Loads the real version.dll and fills every forwarding slot. Returns the names that could not
/// be resolved; calling one of those would jump to address 0.
pub fn init() -> Result<(), Vec<&'static str>> {
    let all = || EXPORTS.iter().map(|e| e.0).collect::<Vec<_>>();
    let Some(path) = real_dll_path() else {
        return Err(all());
    };
    let real = unsafe { LoadLibraryW(path.as_ptr()) };
    if real.is_null() {
        return Err(all());
    }
    let mut missing = Vec::new();
    for (name, cname, slot) in EXPORTS {
        match unsafe { GetProcAddress(real, cname.as_ptr()) } {
            Some(f) => slot.store(f as usize, Ordering::Release),
            None => missing.push(*name),
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_list_matches_the_reference_list() {
        let listed: Vec<&str> = include_str!("../version_exports.txt")
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let here: Vec<&str> = EXPORTS.iter().map(|e| e.0).collect();
        assert_eq!(here, listed);
        assert_eq!(here.len(), 17);
    }

    #[test]
    fn init_resolves_every_export_from_the_real_dll() {
        assert_eq!(init(), Ok(()));
        for (name, _, slot) in EXPORTS {
            assert_ne!(slot.load(Ordering::Acquire), 0, "{name}");
        }
    }

    /// Calls through a stub for real: the forwarded `GetFileVersionInfoSizeW` must answer for
    /// kernel32.dll like the genuine function does.
    #[test]
    fn a_stub_forwards_to_the_real_function() {
        init().unwrap();
        type SizeW = unsafe extern "system" fn(*const u16, *mut u32) -> u32;
        let stub: SizeW =
            unsafe { core::mem::transmute(GetFileVersionInfoSizeW as unsafe extern "C" fn()) };
        let mut path = real_dll_path().unwrap();
        path.truncate(path.len() - "version.dll\0".len());
        path.extend("kernel32.dll\0".encode_utf16());
        let mut handle = 0u32;
        let size = unsafe { stub(path.as_ptr(), &mut handle) };
        assert!(size > 0, "GetFileVersionInfoSizeW via the stub returned 0");
    }
}
