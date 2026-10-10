//! The per-install reporter identity: an ed25519 key made on first use and kept in a small JSON
//! file (`{"v":1,"secret":"<64 hex>"}`). It identifies this installation to the stats server and
//! signs every report; it is not a player account (the BattleTag in the roster is). Losing the
//! file means a new identity and a fresh trust history, so a file that exists but cannot be read is
//! an error, never silently replaced.

use std::fs;
use std::io;
use std::path::Path;

use ed25519_dalek::{Signer, SigningKey};
use w2r_formats::json;

use crate::hex;

pub struct Identity {
    key: SigningKey,
}

impl core::fmt::Debug for Identity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Identity({})", hex::encode(&self.public()))
    }
}

#[derive(Debug)]
pub enum IdentityError {
    Io(io::Error),
    /// The file exists but is not a valid identity file.
    Corrupt,
    /// There is no file and the system gave no randomness.
    NoEntropy,
}

impl From<io::Error> for IdentityError {
    fn from(e: io::Error) -> Self {
        IdentityError::Io(e)
    }
}

impl Identity {
    pub fn from_secret(secret: [u8; 32]) -> Identity {
        Identity {
            key: SigningKey::from_bytes(&secret),
        }
    }

    pub fn public(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.key.sign(message).to_bytes()
    }
}

fn read(path: &Path) -> Result<[u8; 32], IdentityError> {
    let raw = fs::read(path)?;
    let v = json::parse(&raw).map_err(|_| IdentityError::Corrupt)?;
    let version = v
        .get("v")
        .and_then(|n| n.as_number())
        .and_then(|n| n.as_u64());
    if version != Some(1) {
        return Err(IdentityError::Corrupt);
    }
    let secret = v
        .get("secret")
        .and_then(|s| s.as_str())
        .ok_or(IdentityError::Corrupt)?;
    hex::decode_array::<32>(secret).ok_or(IdentityError::Corrupt)
}

/// Loads the identity at `path`, or makes one from `entropy` and writes it there.
pub fn load_or_create(
    path: &Path,
    entropy: impl FnOnce() -> Option<[u8; 32]>,
) -> Result<Identity, IdentityError> {
    match read(path) {
        Ok(secret) => return Ok(Identity::from_secret(secret)),
        Err(IdentityError::Io(e)) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let secret = entropy().ok_or(IdentityError::NoEntropy)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(
        &tmp,
        format!("{{\"v\":1,\"secret\":\"{}\"}}\n", hex::encode(&secret)),
    )?;
    fs::rename(&tmp, path)?;
    Ok(Identity::from_secret(secret))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn temp_dir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "w2r-stats-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn creates_once_then_reloads_the_same_key() {
        let dir = temp_dir("id");
        let path = dir.join("identity.json");
        let a = load_or_create(&path, || Some([7; 32])).unwrap();
        // The second call must not ask for entropy.
        let b = load_or_create(&path, || panic!("entropy asked twice")).unwrap();
        assert_eq!(a.public(), b.public());
        assert_eq!(a.public(), Identity::from_secret([7; 32]).public());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_corrupt_file_is_an_error_not_a_new_identity() {
        let dir = temp_dir("id");
        let path = dir.join("identity.json");
        fs::write(&path, b"{\"v\":1,\"secret\":\"zz\"}").unwrap();
        assert!(matches!(
            load_or_create(&path, || Some([1; 32])),
            Err(IdentityError::Corrupt)
        ));
        assert_eq!(fs::read(&path).unwrap(), b"{\"v\":1,\"secret\":\"zz\"}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn no_entropy_and_no_file_is_an_error() {
        let dir = temp_dir("id");
        let path = dir.join("identity.json");
        assert!(matches!(
            load_or_create(&path, || None),
            Err(IdentityError::NoEntropy)
        ));
        assert!(!path.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn debug_does_not_print_the_secret() {
        let id = Identity::from_secret([9; 32]);
        assert!(!format!("{id:?}").contains(&hex::encode(&[9u8; 32])));
    }
}
