//! Refuses to touch the game unless every byte signature of the supported build matches at its
//! expected address. Ported from wc2r-plus `core/build_guard.cpp` (MIT); reads go through the
//! crash guard instead of `IsBadReadPtr`, so an unmapped address is a clean failure.
//!
//! A failed guard means the game runs unmodified: no hooks, no plugins. The log says which
//! signature broke and shows the bytes, which is what a maintainer needs after a game update.

use w2r_abi::{W2R_LOG_DEBUG, W2R_LOG_ERROR, W2R_LOG_INFO};

use crate::guard::guarded;
use crate::host::Log;
use crate::target::{
    EXPECTED_SIZE_OF_IMAGE, GAME_VERSION, PREFERRED_IMAGE_BASE, Signature, signatures,
};

const TAG: &str = "build-guard";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    Unreadable,
    Byte {
        at: usize,
        expected: u8,
        actual: u8,
    },
    Reloc {
        at: usize,
        expected: u32,
        actual: u32,
    },
}

#[derive(Clone, Debug)]
pub struct Report {
    pub base: usize,
    pub size_of_image: Option<u32>,
    pub total: usize,
    /// `(what, failure)` for each signature that did not match.
    pub failed: Vec<(&'static str, Failure)>,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.failed.is_empty()
    }
}

/// Checks the game at `base` (its module handle) against the generated table.
pub fn verify(base: usize, log: &dyn Log) -> Report {
    verify_with(base, PREFERRED_IMAGE_BASE, signatures::SIGNATURES, log)
}

/// Same, with the image base and table as parameters so a test can run it on a byte buffer.
pub fn verify_with(base: usize, preferred: usize, sigs: &[Signature], log: &dyn Log) -> Report {
    let delta = base.wrapping_sub(preferred);
    log.line(
        W2R_LOG_INFO,
        TAG,
        &format!("expecting Warcraft II Remastered v{GAME_VERSION}"),
    );
    log.line(
        W2R_LOG_INFO,
        TAG,
        &format!("module base={base:#010x} preferred={preferred:#010x} reloc delta={delta:#010x}"),
    );
    let size_of_image = size_of_image(base);
    match size_of_image {
        Some(n) if n == EXPECTED_SIZE_OF_IMAGE => {
            log.line(W2R_LOG_DEBUG, TAG, &format!("SizeOfImage={n:#010x}"));
        }
        Some(n) => log.line(
            W2R_LOG_INFO,
            TAG,
            &format!("SizeOfImage={n:#010x} (expected {EXPECTED_SIZE_OF_IMAGE:#010x}) <-- DIFFERS"),
        ),
        None => log.line(W2R_LOG_INFO, TAG, "SizeOfImage: PE header not readable"),
    }

    let mut failed = Vec::new();
    for sig in sigs {
        let addr = base.wrapping_add((sig.static_addr as usize).wrapping_sub(preferred));
        match check(sig, addr, delta) {
            Ok(()) => log.line(
                W2R_LOG_DEBUG,
                TAG,
                &format!(
                    "[ ok ] {:<40} @ {:#010x} ({} bytes, {} reloc)",
                    sig.what,
                    sig.static_addr,
                    sig.bytes.len(),
                    sig.relocs.len()
                ),
            ),
            Err(f) => {
                describe_failure(log, sig, addr, &f);
                failed.push((sig.what, f));
            }
        }
    }

    if failed.is_empty() {
        log.line(
            W2R_LOG_INFO,
            TAG,
            &format!("PASSED ({}/{}) -- safe to hook", sigs.len(), sigs.len()),
        );
    } else {
        log.line(
            W2R_LOG_ERROR,
            TAG,
            &format!(
                "FAILED ({}/{} signatures) -- refusing to hook or load plugins. This host supports \
                 Warcraft II Remastered v{GAME_VERSION} only; if the game updated, a new host \
                 release is needed. The game runs unmodified meanwhile.",
                failed.len(),
                sigs.len()
            ),
        );
    }
    Report {
        base,
        size_of_image,
        total: sigs.len(),
        failed,
    }
}

/// Compares one signature at `addr`. Reads happen under the crash guard.
fn check(sig: &Signature, addr: usize, delta: usize) -> Result<(), Failure> {
    let actual: Vec<u8> = guarded(|| {
        // SAFETY: a bad address faults, and the guard turns the fault into Err.
        unsafe { core::slice::from_raw_parts(addr as *const u8, sig.bytes.len()) }.to_vec()
    })
    .map_err(|_| Failure::Unreadable)?;
    compare(sig, &actual, delta)
}

/// The pure part: `actual` is what the game has at the signature's address.
pub fn compare(sig: &Signature, actual: &[u8], delta: usize) -> Result<(), Failure> {
    if actual.len() != sig.bytes.len() {
        return Err(Failure::Unreadable);
    }
    for (i, (&want, &got)) in sig.bytes.iter().zip(actual).enumerate() {
        if !sig.in_reloc(i) && want != got {
            return Err(Failure::Byte {
                at: i,
                expected: want,
                actual: got,
            });
        }
    }
    for r in sig.relocs {
        let got = u32::from_le_bytes(actual[r.offset..r.offset + 4].try_into().unwrap());
        let want = (r.static_addr as usize).wrapping_add(delta) as u32;
        if got != want {
            return Err(Failure::Reloc {
                at: r.offset,
                expected: want,
                actual: got,
            });
        }
    }
    Ok(())
}

fn describe_failure(log: &dyn Log, sig: &Signature, addr: usize, f: &Failure) {
    let head = format!("[FAIL] {:<40} @ {:#010x}", sig.what, sig.static_addr);
    match f {
        Failure::Unreadable => {
            log.line(W2R_LOG_ERROR, TAG, &format!("{head}: address not readable"))
        }
        Failure::Byte { at, .. } => {
            log.line(
                W2R_LOG_ERROR,
                TAG,
                &format!("{head}: byte mismatch at +{at}"),
            );
            log.line(
                W2R_LOG_ERROR,
                TAG,
                &format!("      expected:{}", hex_row(sig, sig.bytes)),
            );
            if let Ok(actual) = guarded(|| unsafe {
                core::slice::from_raw_parts(addr as *const u8, sig.bytes.len()).to_vec()
            }) {
                log.line(
                    W2R_LOG_ERROR,
                    TAG,
                    &format!("      actual:  {}", hex_row(sig, &actual)),
                );
            }
        }
        Failure::Reloc {
            at,
            expected,
            actual,
        } => log.line(
            W2R_LOG_ERROR,
            TAG,
            &format!("{head}: reloc at +{at} is {actual:#010x}, expected {expected:#010x}"),
        ),
    }
}

fn hex_row(sig: &Signature, bytes: &[u8]) -> String {
    bytes
        .iter()
        .enumerate()
        .map(|(i, b)| {
            if sig.in_reloc(i) {
                " --".to_owned()
            } else {
                format!(" {b:02x}")
            }
        })
        .collect()
}

/// `SizeOfImage` from the PE optional header at `base`, read under the crash guard.
pub fn size_of_image(base: usize) -> Option<u32> {
    guarded(|| {
        let dos = unsafe { core::slice::from_raw_parts(base as *const u8, 0x40) };
        let e_lfanew = pe_offset(dos)? as usize;
        let nt = unsafe { core::slice::from_raw_parts((base + e_lfanew) as *const u8, 0x60) };
        size_of_image_in(nt)
    })
    .ok()
    .flatten()
}

/// `e_lfanew` from a DOS header, if the magic is right.
pub fn pe_offset(dos: &[u8]) -> Option<u32> {
    if dos.len() < 0x40 || dos[..2] != *b"MZ" {
        return None;
    }
    Some(u32::from_le_bytes(dos[0x3C..0x40].try_into().unwrap()))
}

/// `SizeOfImage` from an NT headers block (`PE\0\0`, file header, optional header). The field
/// sits at the same offset for PE32 and PE32+.
pub fn size_of_image_in(nt: &[u8]) -> Option<u32> {
    if nt.len() < 0x18 + 0x3C || nt[..4] != *b"PE\0\0" {
        return None;
    }
    let optional = &nt[0x18..];
    let magic = u16::from_le_bytes(optional[..2].try_into().unwrap());
    if magic != 0x10B && magic != 0x20B {
        return None;
    }
    Some(u32::from_le_bytes(optional[0x38..0x3C].try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::RelocSlot;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Lines(RefCell<Vec<String>>);
    impl Log for Lines {
        fn line(&self, level: u32, tag: &str, msg: &str) {
            self.0.borrow_mut().push(format!("[{level}] {tag}: {msg}"));
        }
    }

    const PREFERRED: usize = 0x1000;
    static SIG_A: Signature = Signature {
        what: "alpha",
        static_addr: 0x1010,
        bytes: &[0x55, 0x8b, 0xec, 0x66, 0x8b, 0x0d, 0, 0, 0, 0, 0x8b, 0x45],
        relocs: &[RelocSlot {
            offset: 6,
            static_addr: 0x1050,
        }],
    };
    static SIG_B: Signature = Signature {
        what: "beta",
        static_addr: 0x1020,
        bytes: &[0xe8, 0x1b, 0x6d, 0xff],
        relocs: &[],
    };

    /// A fake image whose "preferred base" is 0x1000, loaded wherever the Vec landed.
    fn image() -> Vec<u8> {
        let mut img = vec![0u8; 0x100];
        img[0x10..0x1C].copy_from_slice(SIG_A.bytes);
        img[0x20..0x24].copy_from_slice(SIG_B.bytes);
        let base = img.as_ptr() as usize;
        let delta = base.wrapping_sub(PREFERRED);
        let relocated = (0x1050usize).wrapping_add(delta) as u32;
        img[0x16..0x1A].copy_from_slice(&relocated.to_le_bytes());
        img
    }

    #[test]
    fn matching_image_passes_and_logs_each_signature() {
        let img = image();
        let log = Lines::default();
        let r = verify_with(img.as_ptr() as usize, PREFERRED, &[SIG_A, SIG_B], &log);
        assert!(r.passed(), "{:?}", r.failed);
        assert_eq!(r.total, 2);
        let lines = log.0.borrow();
        assert!(lines.iter().any(|l| l.contains("[ ok ] alpha")));
        assert!(lines.iter().any(|l| l.contains("[ ok ] beta")));
        assert!(lines.last().unwrap().contains("PASSED (2/2)"));
    }

    #[test]
    fn a_changed_byte_fails_with_its_offset() {
        let mut img = image();
        img[0x21] ^= 0xFF;
        let log = Lines::default();
        let r = verify_with(img.as_ptr() as usize, PREFERRED, &[SIG_A, SIG_B], &log);
        assert_eq!(
            r.failed,
            vec![(
                "beta",
                Failure::Byte {
                    at: 1,
                    expected: 0x1b,
                    actual: 0xe4
                }
            )]
        );
        let lines = log.0.borrow();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("[FAIL] beta") && l.contains("byte mismatch at +1"))
        );
        assert!(lines.iter().any(|l| l.contains("expected: e8 1b 6d ff")));
        assert!(lines.iter().any(|l| l.contains("actual:   e8 e4 6d ff")));
        assert!(
            lines
                .last()
                .unwrap()
                .starts_with("[3] build-guard: FAILED (1/2")
        );
    }

    #[test]
    fn a_changed_byte_inside_a_reloc_slot_is_not_a_byte_mismatch_but_a_reloc_mismatch() {
        let mut img = image();
        img[0x17] ^= 0x01;
        let r = verify_with(
            img.as_ptr() as usize,
            PREFERRED,
            &[SIG_A],
            &Lines::default(),
        );
        assert!(matches!(
            r.failed[0],
            ("alpha", Failure::Reloc { at: 6, .. })
        ));
    }

    #[test]
    fn hex_rows_blank_out_reloc_slots() {
        assert_eq!(
            hex_row(&SIG_A, SIG_A.bytes),
            " 55 8b ec 66 8b 0d -- -- -- -- 8b 45"
        );
    }

    #[cfg(windows)]
    #[test]
    fn an_unmapped_address_is_unreadable_not_a_crash() {
        let sig = Signature {
            static_addr: 0x1000,
            ..SIG_B
        };
        // base chosen so the signature lands near the top of the 32-bit range / in a null page.
        let r = verify_with(0x10, PREFERRED, &[sig], &Lines::default());
        assert_eq!(r.failed, vec![("beta", Failure::Unreadable)]);
    }

    #[test]
    fn pe_parsing_handles_bad_and_good_headers() {
        assert_eq!(pe_offset(&[0; 0x40]), None);
        let mut dos = [0u8; 0x40];
        dos[..2].copy_from_slice(b"MZ");
        dos[0x3C..].copy_from_slice(&0x80u32.to_le_bytes());
        assert_eq!(pe_offset(&dos), Some(0x80));

        let mut nt = [0u8; 0x60];
        assert_eq!(size_of_image_in(&nt), None);
        nt[..4].copy_from_slice(b"PE\0\0");
        assert_eq!(size_of_image_in(&nt), None, "bad optional magic");
        nt[0x18..0x1A].copy_from_slice(&0x10Bu16.to_le_bytes());
        nt[0x18 + 0x38..0x18 + 0x3C].copy_from_slice(&0x0062_B000u32.to_le_bytes());
        assert_eq!(size_of_image_in(&nt), Some(0x0062_B000));
        nt[0x18..0x1A].copy_from_slice(&0x20Bu16.to_le_bytes());
        assert_eq!(size_of_image_in(&nt), Some(0x0062_B000));
    }

    #[cfg(windows)]
    #[test]
    fn size_of_image_of_this_process_is_sane() {
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        let base = unsafe { GetModuleHandleW(core::ptr::null()) } as usize;
        let n = size_of_image(base).expect("own PE header");
        assert!(n > 0x1000 && n % 0x1000 == 0, "{n:#x}");
        assert_eq!(size_of_image(0x10), None);
    }
}
