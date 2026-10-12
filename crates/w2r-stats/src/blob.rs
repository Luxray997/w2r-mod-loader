//! Signing of a whole file, used for replays: the installation key signs `w2r-replay-1\n` followed
//! by the file's SHA-256, so the server can tie an upload to the key that reported the same game
//! without parsing or re-serialising anything. The digest is what is signed, not the file, so a
//! signature is 64 bytes whatever the size.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::identity::Identity;

const DOMAIN: &[u8] = b"w2r-replay-1\n";

pub fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn message(digest: &[u8; 32]) -> Vec<u8> {
    let mut m = DOMAIN.to_vec();
    m.extend_from_slice(digest);
    m
}

pub fn sign(identity: &Identity, bytes: &[u8]) -> [u8; 64] {
    identity.sign(&message(&digest(bytes)))
}

/// True when `sig` is a valid signature by `key` over `bytes`.
pub fn verify(key: &[u8; 32], sig: &[u8; 64], bytes: &[u8]) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(key) else {
        return false;
    };
    key.verify(&message(&digest(bytes)), &Signature::from_bytes(sig))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_covers_exactly_these_bytes_and_this_key() {
        let id = Identity::from_secret([5; 32]);
        let sig = sign(&id, b"replay bytes");
        assert!(verify(&id.public(), &sig, b"replay bytes"));
        assert!(!verify(&id.public(), &sig, b"replay bytez"));
        let other = Identity::from_secret([6; 32]);
        assert!(!verify(&other.public(), &sig, b"replay bytes"));
    }

    #[test]
    fn a_report_signature_is_not_a_replay_signature() {
        // Different domain strings: one cannot be replayed as the other.
        let id = Identity::from_secret([5; 32]);
        let report_style = id.sign(&[b"w2r-report-1\n".as_slice(), &digest(b"x")].concat());
        assert!(!verify(&id.public(), &report_style, b"x"));
    }
}
