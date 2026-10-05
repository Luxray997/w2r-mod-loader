//! Minisign-format ed25519 signatures, algorithm `ED` (BLAKE2b-512 prehash). Rules in
//! `docs/index.md`; reference `tools/w2r_spec/minisign.py`. The legacy `Ed` mode is refused.
//! Written from the format description: still to be checked against the real `minisign` binary
//! (Phase 6).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use blake2::{Blake2b512, Digest};
use ed25519_dalek::{Signature, VerifyingKey};

pub const MAX_SIG_BYTES: usize = 4096;
const TRUSTED_PREFIX: &str = "trusted comment: ";
const UNTRUSTED_PREFIX: &str = "untrusted comment:";

/// A public key body: `"Ed" || key id (8) || key (32)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKey {
    pub key_id: [u8; 8],
    pub key: [u8; 32],
}

fn decode(s: &str) -> Option<Vec<u8>> {
    STANDARD.decode(s).ok()
}

/// Splits on `\n`, accepting `\r\n`; exactly one final newline, and nothing after it.
fn lines(text: &str) -> Option<Vec<&str>> {
    let body = text.strip_suffix('\n')?;
    Some(
        body.split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .collect(),
    )
}

/// True when `body` is base64 of a 42-byte minisign public key starting `Ed`.
pub fn key_body_ok(body: &str) -> bool {
    decode(body).is_some_and(|raw| raw.len() == 42 && raw.starts_with(b"Ed"))
}

impl PublicKey {
    /// Parses a `.pub` file (comment line, then the base64 body).
    pub fn parse_file(text: &str) -> Option<PublicKey> {
        let l = lines(text)?;
        if l.len() != 2 || !l[0].starts_with(UNTRUSTED_PREFIX) {
            return None;
        }
        Self::parse_body(l[1])
    }

    /// Parses the bare base64 body, the form `next_key` and an embedded key use.
    pub fn parse_body(body: &str) -> Option<PublicKey> {
        let raw = decode(body)?;
        if raw.len() != 42 || &raw[..2] != b"Ed" {
            return None;
        }
        Some(PublicKey {
            key_id: raw[2..10].try_into().ok()?,
            key: raw[10..].try_into().ok()?,
        })
    }
}

/// Both signatures must verify over the exact bytes of `message`.
pub fn verify(message: &[u8], sig_text: &str, key: &PublicKey) -> bool {
    verify_inner(message, sig_text, key).is_some()
}

fn verify_inner(message: &[u8], sig_text: &str, key: &PublicKey) -> Option<()> {
    if sig_text.len() > MAX_SIG_BYTES {
        return None;
    }
    let l = lines(sig_text)?;
    if l.len() != 4 || !l[0].starts_with(UNTRUSTED_PREFIX) || !l[2].starts_with(TRUSTED_PREFIX) {
        return None;
    }
    let raw = decode(l[1])?;
    if raw.len() != 74 || &raw[..2] != b"ED" || raw[2..10] != key.key_id {
        return None;
    }
    let sig = Signature::from_slice(&raw[10..]).ok()?;
    let global = Signature::from_slice(&decode(l[3])?).ok()?;
    let vk = VerifyingKey::from_bytes(&key.key).ok()?;
    let digest = Blake2b512::digest(message);
    vk.verify_strict(&digest, &sig).ok()?;
    let mut signed = raw[10..].to_vec();
    signed.extend_from_slice(&l[2].as_bytes()[TRUSTED_PREFIX.len()..]);
    vk.verify_strict(&signed, &global).ok()
}
