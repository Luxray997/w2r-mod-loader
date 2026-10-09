//! Test repositories (feature `dev-repo`): writes an index and the packages into a directory that
//! `fetch::DirFetcher` serves, so the manager can be exercised offline.

use crate::Error;
use crate::index::format_time;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Cursor, Write};
use std::path::Path;
use zip::write::SimpleFileOptions;

pub const BASE_URL: &str = "https://plugins.test.invalid";

/// One plugin version to publish.
pub struct Plugin<'a> {
    pub id: &'a str,
    pub version: &'a str,
    pub capabilities: &'a [&'a str],
    /// `(id, version_req)` pairs.
    pub dependencies: &'a [(&'a str, &'a str)],
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
        let deps: Vec<String> = self
            .dependencies
            .iter()
            .map(|(id, req)| format!(r#"{{"id":"{id}","version_req":"{req}"}}"#))
            .collect();
        format!(
            r#"{{"schema":1,"id":"{id}","name":"{id}","version":"{v}","author":"test","description":"Test plugin {id}.","entry":"{e}","abi":1,"game_builds":["1.0.2.2818"],"capabilities":[{c}],"affects_simulation":false,"dependencies":[{d}]}}"#,
            id = self.id,
            v = self.version,
            e = self.entry_name(),
            c = caps.join(","),
            d = deps.join(",")
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

/// Writes `index.json` and one `<id>-<version>.zip` per plugin into `dir`.
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
        r#"{{"schema":1,"serial":{serial},"generated":"{}","plugins":[{}]}}"#,
        format_time(now),
        entries.join(",")
    );
    fs::write(dir.join("index.json"), &index)?;
    Ok(())
}
