//! Test fixtures: build a package and its index entry without going through a signed index.

use crate::index::VersionEntry;
use crate::store::Root;
use sha2::{Digest, Sha256};
use std::io::{Cursor, Write};
use std::path::PathBuf;
use w2r_formats::manifest;
use w2r_formats::semver::Version;
use zip::write::SimpleFileOptions;

pub const ENTRY: &str = "example_plugin.dll";

pub fn manifest_json(version: &str, caps: &[&str]) -> String {
    let caps: Vec<String> = caps.iter().map(|c| format!("\"{c}\"")).collect();
    format!(
        r#"{{"schema":1,"id":"example-plugin","name":"Example Plugin","version":"{version}","author":"someone","description":"One sentence.","entry":"{ENTRY}","abi":1,"game_builds":["1.0.2.2818"],"capabilities":[{}],"affects_simulation":false,"dependencies":[]}}"#,
        caps.join(",")
    )
}

/// A package holding `dll` and a matching `plugin.json`, plus the index entry describing it.
pub fn package(version: &str, caps: &[&str], dll: &[u8]) -> (VersionEntry, Vec<u8>) {
    let json = manifest_json(version, caps);
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    w.start_file("plugin.json", SimpleFileOptions::default())
        .unwrap();
    w.write_all(json.as_bytes()).unwrap();
    w.start_file(ENTRY, SimpleFileOptions::default()).unwrap();
    w.write_all(dll).unwrap();
    let zip = w.finish().unwrap().into_inner();
    let m = manifest::validate(json.as_bytes()).unwrap();
    let entry = VersionEntry {
        version: Version::parse(version).unwrap(),
        manifest: m,
        manifest_json: json,
        url: "https://example.com/p.zip".into(),
        sha256: Sha256::digest(&zip).into(),
        size: zip.len() as u64,
        min_manager: Version::parse("0.1.0").unwrap(),
    };
    (entry, zip)
}

/// A fresh root under the temp directory, removed on drop.
pub struct TempRoot(pub Root, PathBuf);

impl TempRoot {
    pub fn new(name: &str) -> TempRoot {
        let p = std::env::temp_dir().join(format!("w2rpm-root-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TempRoot(Root(p.clone()), p)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.1);
    }
}
