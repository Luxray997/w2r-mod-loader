//! The simulation fingerprint (docs/load-policy.md, "Simulation fingerprint"): 4 bytes that
//! summarise which simulation-affecting plugins this session loaded, exchanged in the handshake
//! so players can be warned of (or refused for) a mismatch. The reference implementation is
//! `tools/w2r_spec/fingerprint.py`; `docs/vectors/fingerprint.json` pins both.

use sha2::{Digest, Sha256};

/// One plugin that counts: loaded this session, and either `affects_simulation` or approved
/// `hooks.raw`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    pub id: &'a str,
    /// The manifest's version string, as written.
    pub version: &'a str,
    /// The DLL's SHA-256 (the pin, which the loader proved equal to the file).
    pub sha256: &'a [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicateId(pub String);

/// Sort by id (bytewise), append `id 0x00 version 0x00 sha256` per plugin, SHA-256, keep 4
/// bytes. The empty set is all zero without hashing, which is also what wc2r-plus sends.
pub fn compute(entries: &[Entry<'_>]) -> Result<[u8; 4], DuplicateId> {
    if entries.is_empty() {
        return Ok([0; 4]);
    }
    let mut sorted: Vec<&Entry<'_>> = entries.iter().collect();
    sorted.sort_by(|a, b| a.id.as_bytes().cmp(b.id.as_bytes()));
    if let Some(w) = sorted.windows(2).find(|w| w[0].id == w[1].id) {
        return Err(DuplicateId(w[0].id.to_owned()));
    }
    let mut h = Sha256::new();
    for e in sorted {
        h.update(e.id.as_bytes());
        h.update([0]);
        h.update(e.version.as_bytes());
        h.update([0]);
        h.update(e.sha256);
    }
    let digest = h.finalize();
    Ok([digest[0], digest[1], digest[2], digest[3]])
}

/// Lower-case hex, as the log and the plugin screen show it.
pub fn hex(fp: &[u8; 4]) -> String {
    fp.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::{self, Value};

    const VECTORS: &str = include_str!("../../../docs/vectors/fingerprint.json");

    fn sha(hex: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
        }
        out
    }

    fn entries(v: &Value) -> Vec<(String, String, [u8; 32])> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|p| {
                (
                    p.get("id").unwrap().as_str().unwrap().to_owned(),
                    p.get("version").unwrap().as_str().unwrap().to_owned(),
                    sha(p.get("sha256").unwrap().as_str().unwrap()),
                )
            })
            .collect()
    }

    fn borrow<'a>(owned: &'a [(String, String, [u8; 32])]) -> Vec<Entry<'a>> {
        owned
            .iter()
            .map(|(id, version, sha256)| Entry {
                id,
                version,
                sha256,
            })
            .collect()
    }

    #[test]
    fn matches_the_vectors() {
        let doc = json::parse(VECTORS.as_bytes()).unwrap();
        let cases = doc.get("fingerprints").unwrap().as_array().unwrap();
        assert!(cases.len() >= 5);
        for case in cases {
            let name = case.get("name").unwrap().as_str().unwrap();
            let owned = entries(case.get("plugins").unwrap());
            let got = compute(&borrow(&owned)).unwrap();
            assert_eq!(
                hex(&got),
                case.get("expect").unwrap().as_str().unwrap(),
                "{name}"
            );
        }
        let dup = entries(doc.get("duplicate_ids_refused").unwrap());
        assert_eq!(
            compute(&borrow(&dup)),
            Err(DuplicateId("example-plugin".into()))
        );
        for tag in doc.get("tags").unwrap().as_array().unwrap() {
            let id = tag.get("id").unwrap().as_str().unwrap();
            assert_eq!(
                hex(&crate::peer::peer_tag(id)),
                tag.get("expect").unwrap().as_str().unwrap()
            );
        }
    }

    #[test]
    fn empty_is_zero_and_order_does_not_matter() {
        assert_eq!(compute(&[]).unwrap(), [0; 4]);
        let a = Entry {
            id: "a",
            version: "1.0.0",
            sha256: &[1; 32],
        };
        let b = Entry {
            id: "b",
            version: "2.0.0",
            sha256: &[2; 32],
        };
        assert_eq!(compute(&[a, b]).unwrap(), compute(&[b, a]).unwrap());
        assert_ne!(compute(&[a]).unwrap(), [0; 4]);
        assert_ne!(compute(&[a]).unwrap(), compute(&[b]).unwrap());
    }
}
