//! Test repositories (feature `dev-sign`): signs with a fixed TEST-ONLY key and writes an index,
//! its signature and the packages into a directory that `fetch::DirFetcher` serves. The seed is
//! `bytes(range(32))`, the same as `docs/vectors/index.json`. Never use this key for anything real.

use crate::Error;
use crate::index::format_time;
use crate::minisign::PublicKey;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use blake2::{Blake2b512, Digest};
use ed25519_dalek::{Signer, SigningKey};
use sha2::Sha256;
use std::fs;
use std::io::{Cursor, Write};
use std::path::Path;
use zip::write::SimpleFileOptions;

pub const BASE_URL: &str = "https://plugins.test.invalid";

fn signing_key() -> SigningKey {
    let seed: [u8; 32] = std::array::from_fn(|i| i as u8);
    SigningKey::from_bytes(&seed)
}

pub fn public_key() -> PublicKey {
    let sk = signing_key();
    let key = sk.verifying_key().to_bytes();
    let id: [u8; 8] = Sha256::digest(key)[..8].try_into().unwrap();
    PublicKey { key_id: id, key }
}

/// A minisign `.pub` file for [`public_key`].
pub fn public_key_file() -> String {
    let pk = public_key();
    let mut raw = b"Ed".to_vec();
    raw.extend_from_slice(&pk.key_id);
    raw.extend_from_slice(&pk.key);
    format!(
        "untrusted comment: w2rpm test key\n{}\n",
        STANDARD.encode(raw)
    )
}

/// A signature file over `message` in the `ED` (prehashed) form.
pub fn sign(message: &[u8], trusted_comment: &str) -> String {
    let sk = signing_key();
    let pk = public_key();
    let sig = sk.sign(&Blake2b512::digest(message)).to_bytes();
    let mut signed = sig.to_vec();
    signed.extend_from_slice(trusted_comment.as_bytes());
    let global = sk.sign(&signed).to_bytes();
    let mut body = b"ED".to_vec();
    body.extend_from_slice(&pk.key_id);
    body.extend_from_slice(&sig);
    format!(
        "untrusted comment: signature from w2rpm test key\n{}\ntrusted comment: {trusted_comment}\n{}\n",
        STANDARD.encode(body),
        STANDARD.encode(global)
    )
}

/// One plugin version to publish.
pub struct Plugin<'a> {
    pub id: &'a str,
    pub version: &'a str,
    pub capabilities: &'a [&'a str],
    pub dll: &'a [u8],
}

impl Plugin<'_> {
    fn entry_name(&self) -> String {
        format!("{}.dll", self.id.replace('-', "_"))
    }

    fn manifest(&self) -> String {
        let caps: Vec<String> = self
            .capabilities
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect();
        format!(
            r#"{{"schema":1,"id":"{id}","name":"{id}","version":"{v}","author":"test","description":"Test plugin {id}.","entry":"{e}","abi":1,"game_builds":["1.0.2.2818"],"capabilities":[{c}],"affects_simulation":false,"dependencies":[]}}"#,
            id = self.id,
            v = self.version,
            e = self.entry_name(),
            c = caps.join(",")
        )
    }

    fn package(&self) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        w.start_file("plugin.json", SimpleFileOptions::default())
            .unwrap();
        w.write_all(self.manifest().as_bytes()).unwrap();
        w.start_file(self.entry_name(), SimpleFileOptions::default())
            .unwrap();
        w.write_all(self.dll).unwrap();
        w.finish().unwrap().into_inner()
    }
}

/// Writes `index.json`, `index.json.sig`, `test.pub` and one `<id>-<version>.zip` per plugin
/// into `dir`. The index is valid from `now` for 30 days.
pub fn build(dir: &Path, now: i64, serial: u64, plugins: &[Plugin]) -> Result<(), Error> {
    fs::create_dir_all(dir)?;
    let mut ids: Vec<&str> = plugins.iter().map(|p| p.id).collect();
    ids.dedup();
    let mut entries = Vec::new();
    for id in ids {
        let mut versions = Vec::new();
        for p in plugins.iter().filter(|p| p.id == id) {
            let zip = p.package();
            let file = format!("{}-{}.zip", p.id, p.version);
            fs::write(dir.join(&file), &zip)?;
            let hash: String = Sha256::digest(&zip)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            versions.push(format!(
                r#"{{"version":"{}","manifest":{},"url":"{BASE_URL}/{file}","sha256":"{hash}","size":{},"min_manager":"0.1.0"}}"#,
                p.version,
                p.manifest(),
                zip.len()
            ));
        }
        entries.push(format!(
            r#"{{"id":"{id}","versions":[{}]}}"#,
            versions.join(",")
        ));
    }
    let index = format!(
        r#"{{"schema":1,"serial":{serial},"generated":"{}","expires":"{}","plugins":[{}]}}"#,
        format_time(now),
        format_time(now + 30 * 86_400),
        entries.join(",")
    );
    fs::write(dir.join("index.json"), &index)?;
    fs::write(
        dir.join("index.json.sig"),
        sign(index.as_bytes(), "w2rpm test index"),
    )?;
    fs::write(dir.join("test.pub"), public_key_file())?;
    Ok(())
}
