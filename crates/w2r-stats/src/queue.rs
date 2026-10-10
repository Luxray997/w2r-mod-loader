//! The on-disk queue: one `.json` file per sealed report in a folder, written through a temp file
//! and a rename so a crash never leaves half a report. Names sort oldest first (the caller starts
//! them with a zero-padded timestamp). The queue is bounded: past `MAX_FILES` the oldest are
//! dropped, because reports are small and an uploader that never succeeds must not fill the disk.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const MAX_FILES: usize = 200;

#[derive(Clone, Debug)]
pub struct Queue {
    dir: PathBuf,
}

impl Queue {
    pub fn open(dir: &Path) -> io::Result<Queue> {
        fs::create_dir_all(dir)?;
        Ok(Queue {
            dir: dir.to_path_buf(),
        })
    }

    /// File name for a report: `<seconds, 12 digits>-<phase>-<rid hex>.json`.
    pub fn file_name(started: u64, phase: &str, rid_hex: &str) -> String {
        format!("{started:012}-{phase}-{rid_hex}.json")
    }

    /// Stores `bytes` under `name`. Returns the path.
    pub fn push(&self, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
        if !name.ends_with(".json") || name.contains(['/', '\\']) || name.starts_with('.') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "bad queue file name",
            ));
        }
        let path = self.dir.join(name);
        let tmp = self.dir.join(format!("{name}.tmp"));
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, &path)?;
        self.trim();
        Ok(path)
    }

    /// Queued files, oldest first.
    pub fn list(&self) -> Vec<PathBuf> {
        let Ok(rd) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        files.sort();
        files
    }

    pub fn remove(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn trim(&self) {
        let files = self.list();
        if files.len() > MAX_FILES {
            for p in &files[..files.len() - MAX_FILES] {
                let _ = fs::remove_file(p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "w2r-queue-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn pushes_lists_oldest_first_and_removes() {
        let d = dir("a");
        let q = Queue::open(&d).unwrap();
        let late = Queue::file_name(2000, "result", "bb");
        let early = Queue::file_name(1000, "start", "aa");
        q.push(&late, b"2").unwrap();
        q.push(&early, b"1").unwrap();
        let names: Vec<String> = q
            .list()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![early.clone(), late.clone()]);
        q.remove(&q.list()[0]).unwrap();
        assert_eq!(q.list().len(), 1);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn leftover_temp_files_are_not_listed() {
        let d = dir("b");
        let q = Queue::open(&d).unwrap();
        fs::write(d.join("x.json.tmp"), b"half").unwrap();
        assert!(q.list().is_empty());
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn the_queue_is_bounded_and_drops_the_oldest() {
        let d = dir("c");
        let q = Queue::open(&d).unwrap();
        for i in 0..(MAX_FILES as u64 + 5) {
            q.push(&Queue::file_name(i, "start", "00"), b"x").unwrap();
        }
        let files = q.list();
        assert_eq!(files.len(), MAX_FILES);
        assert!(files[0].to_string_lossy().contains("000000000005-"));
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn refuses_names_that_could_escape_the_folder() {
        let d = dir("d");
        let q = Queue::open(&d).unwrap();
        assert!(q.push("../x.json", b"").is_err());
        assert!(q.push("a\\b.json", b"").is_err());
        assert!(q.push("x.txt", b"").is_err());
        assert!(q.push(".hidden.json", b"").is_err());
        let _ = fs::remove_dir_all(d);
    }
}
