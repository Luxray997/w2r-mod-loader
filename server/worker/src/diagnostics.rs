//! What the service accepts as a diagnostics bundle (`PUT /v1/diagnostics`). Pure, like `replay`:
//! bytes and an optional key and signature in, a row or a reason out. The zip is stored untouched
//! in R2 under `diagnostics/<sha>.zip` and never opened by the service.
//!
//! The client signs with its installation key when it has one. A signed upload counts against
//! that key's daily allowance; an unsigned one (a machine that has never played a game) only
//! against the service-wide allowance and the per-address rate limit.

use w2r_stats::{blob, hex};

/// Same limit as the client's own (`w2r_loader_lib::diag::MAX_ZIP`).
pub const MAX_BYTES: usize = 4 * 1024 * 1024;
pub const PER_KEY_PER_DAY: u64 = 5;
pub const TOTAL_PER_DAY: u64 = 100;
/// Bundles are deleted by the nightly job after this long.
pub const KEEP_SECS: u64 = 30 * 24 * 3600;

#[derive(Debug, PartialEq, Eq)]
pub enum Reject {
    TooLarge,
    NotAZip,
    BadSignature,
}

impl Reject {
    pub fn reason(&self) -> &'static str {
        match self {
            Reject::TooLarge => "too large",
            Reject::NotAZip => "not a zip file",
            Reject::BadSignature => "bad signature",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    pub sha: String,
    /// The signing installation key, hex; empty when the upload was unsigned.
    pub key: String,
    pub size: usize,
    /// What the user reads out: `W2R-XXXX-XXXX`, from the file's hash, so the same file sent
    /// twice has the same code.
    pub code: String,
}

/// No 0, 1, I or O, so a code survives being read aloud or typed from a screenshot.
const ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";

pub fn code_of(sha: &[u8; 32]) -> String {
    let mut bits: u64 = 0;
    for b in &sha[..5] {
        bits = (bits << 8) | u64::from(*b);
    }
    let mut chars = String::new();
    for i in (0..8).rev() {
        chars.push(ALPHABET[((bits >> (i * 5)) & 31) as usize] as char);
    }
    format!("W2R-{}-{}", &chars[..4], &chars[4..])
}

pub fn check(raw: &[u8], key: Option<&[u8; 32]>, sig: Option<&[u8; 64]>) -> Result<Row, Reject> {
    if raw.len() > MAX_BYTES {
        return Err(Reject::TooLarge);
    }
    if !raw.starts_with(b"PK\x03\x04") {
        return Err(Reject::NotAZip);
    }
    let key_hex = match (key, sig) {
        (None, None) => String::new(),
        (Some(k), Some(s)) if blob::verify_diag(k, s, raw) => hex::encode(k),
        _ => return Err(Reject::BadSignature),
    };
    let sha = blob::digest(raw);
    Ok(Row {
        sha: hex::encode(&sha),
        key: key_hex,
        size: raw.len(),
        code: code_of(&sha),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use w2r_stats::identity::Identity;

    fn zip() -> Vec<u8> {
        let mut v = b"PK\x03\x04".to_vec();
        v.extend_from_slice(&[0; 40]);
        v
    }

    #[test]
    fn an_unsigned_zip_is_accepted_with_a_stable_code() {
        let a = check(&zip(), None, None).unwrap();
        let b = check(&zip(), None, None).unwrap();
        assert_eq!(a.code, b.code);
        assert_eq!(a.key, "");
        assert_eq!(a.code.len(), "W2R-ABCD-EFGH".len());
        assert!(a.code.starts_with("W2R-") && a.code.as_bytes()[8] == b'-');
        let mut other = zip();
        other.push(1);
        assert_ne!(check(&other, None, None).unwrap().code, a.code);
    }

    #[test]
    fn a_signed_zip_names_its_key_and_a_wrong_signature_is_refused() {
        let id = Identity::from_secret([4; 32]);
        let raw = zip();
        let sig = blob::sign_diag(&id, &raw);
        let row = check(&raw, Some(&id.public()), Some(&sig)).unwrap();
        assert_eq!(row.key, hex::encode(&id.public()));
        // A replay signature over the same bytes is not a diagnostics signature.
        let replay_sig = blob::sign(&id, &raw);
        assert_eq!(
            check(&raw, Some(&id.public()), Some(&replay_sig)),
            Err(Reject::BadSignature)
        );
        // Half a signature is refused, not treated as unsigned.
        assert_eq!(check(&raw, Some(&id.public()), None), Err(Reject::BadSignature));
        assert_eq!(check(&raw, None, Some(&sig)), Err(Reject::BadSignature));
    }

    #[test]
    fn junk_and_oversize_are_refused() {
        assert_eq!(check(b"hello", None, None), Err(Reject::NotAZip));
        let mut big = zip();
        big.resize(MAX_BYTES + 1, 0);
        assert_eq!(check(&big, None, None), Err(Reject::TooLarge));
    }
}
