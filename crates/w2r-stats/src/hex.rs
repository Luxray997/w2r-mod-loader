//! Lowercase hex.

pub fn encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 15) as usize] as char);
    }
    out
}

pub fn decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    s.as_bytes()
        .chunks(2)
        .map(|c| u8::from_str_radix(core::str::from_utf8(c).ok()?, 16).ok())
        .collect()
}

pub fn decode_array<const N: usize>(s: &str) -> Option<[u8; N]> {
    decode(s)?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        assert_eq!(encode(&[0, 1, 0xab, 0xff]), "0001abff");
        assert_eq!(decode("0001abff"), Some(vec![0, 1, 0xab, 0xff]));
        assert_eq!(decode("0g"), None);
        assert_eq!(decode("abc"), None);
        assert_eq!(decode_array::<2>("0102"), Some([1, 2]));
        assert_eq!(decode_array::<3>("0102"), None);
    }
}
