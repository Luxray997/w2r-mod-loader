//! Text the game hands us as narrow strings: map paths, the install folder, lobby names.
//!
//! The game's file layer works in a Windows code page, so a path with a character outside ASCII
//! (an accented folder name, a Cyrillic map name) arrives as code-page bytes and is not valid
//! UTF-8. Reading it lossily turned every such byte into U+FFFD, and the file was then "not
//! found": a map already on disk was downloaded again, and an install folder with such a
//! character made every download fail (wc2r-plus 1.1.0, `FromGamePath`).
//!
//! `decode` follows wc2r-plus: valid UTF-8 first (what we hand the game ourselves, and what a
//! plain-ASCII path always is), otherwise the system's ANSI code page. The result is a normal Rust
//! string, which `std::fs` opens through the wide APIs, so it needs no further conversion. What we
//! pass back to the game is UTF-8, as wc2r-plus does (`ToGamePath`).

/// A game string as text. Never fails; bytes that fit neither encoding become U+FFFD.
pub fn decode(bytes: &[u8]) -> String {
    decode_with(bytes, ansi_decode)
}

fn decode_with(bytes: &[u8], fallback: impl FnOnce(&[u8]) -> Option<String>) -> String {
    match core::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => fallback(bytes).unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned()),
    }
}

/// Strict conversion with the ANSI code page: refuses bytes the page cannot represent instead of
/// guessing, so the caller can fall back to the lossy form.
#[cfg(windows)]
fn ansi_decode(bytes: &[u8]) -> Option<String> {
    use windows_sys::Win32::Globalization::{CP_ACP, MB_ERR_INVALID_CHARS, MultiByteToWideChar};
    if bytes.is_empty() || bytes.len() > i32::MAX as usize {
        return None;
    }
    let len = bytes.len() as i32;
    unsafe {
        let n = MultiByteToWideChar(
            CP_ACP,
            MB_ERR_INVALID_CHARS,
            bytes.as_ptr(),
            len,
            core::ptr::null_mut(),
            0,
        );
        if n <= 0 {
            return None;
        }
        let mut wide = vec![0u16; n as usize];
        let got = MultiByteToWideChar(
            CP_ACP,
            MB_ERR_INVALID_CHARS,
            bytes.as_ptr(),
            len,
            wide.as_mut_ptr(),
            n,
        );
        (got == n).then(|| String::from_utf16(&wide).ok()).flatten()
    }
}

#[cfg(not(windows))]
fn ansi_decode(_bytes: &[u8]) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for Windows-1252: every byte maps to the char with the same value (Latin-1).
    fn latin1(b: &[u8]) -> Option<String> {
        Some(b.iter().map(|&c| c as char).collect())
    }

    #[test]
    fn ascii_and_utf8_pass_through() {
        assert_eq!(
            decode_with(b"C:\\Games\\Maps", |_| panic!("no fallback")),
            "C:\\Games\\Maps"
        );
        let utf8 = "C:\\Jeux\\Cartes\u{e9}".as_bytes();
        assert_eq!(
            decode_with(utf8, |_| panic!("no fallback")),
            "C:\\Jeux\\Cartes\u{e9}"
        );
    }

    #[test]
    fn code_page_bytes_decode_through_the_fallback() {
        // 0xE9 alone is e-acute in Windows-1252 and an invalid UTF-8 sequence.
        let got = decode_with(b"Cartes\xe9.pud", latin1);
        assert_eq!(got, "Cartes\u{e9}.pud");
    }

    #[test]
    fn undecodable_bytes_never_fail() {
        let got = decode_with(b"bad\xff.pud", |_| None);
        assert_eq!(got, "bad\u{fffd}.pud");
    }

    #[cfg(not(windows))]
    #[test]
    fn without_windows_the_fallback_is_lossy() {
        assert_eq!(decode(b"a\xffb"), "a\u{fffd}b");
    }
}
