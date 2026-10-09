//! The GUI's own settings: which w2r folder and which index. Kept in
//! `%APPDATA%\w2rpm\gui.json` (override: `W2RPM_GUI_CONFIG`). Not part of the trust record
//! (`state.json`); a bad file is ignored and the defaults are used.

use std::path::{Path, PathBuf};
use w2r_formats::json::{self, Value};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// The w2r folder; `None` means `%USERPROFILE%\Saved Games\Warcraft2Remastered\w2r`.
    pub root: Option<PathBuf>,
    pub index_url: String,
}

pub fn config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("W2RPM_GUI_CONFIG") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("w2rpm").join("gui.json"))
}

impl Settings {
    pub fn parse(raw: &[u8]) -> Option<Settings> {
        let v = json::parse(raw).ok()?;
        let str_of = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        Some(Settings {
            root: str_of("root")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute()),
            index_url: str_of("index_url").unwrap_or_default(),
        })
    }

    pub fn to_json(&self) -> String {
        let s = |t: &str| Value::String(t.to_owned());
        Value::Object(vec![
            (
                "root".into(),
                self.root
                    .as_ref()
                    .map_or(Value::Null, |p| s(&p.to_string_lossy())),
            ),
            ("index_url".into(), s(&self.index_url)),
        ])
        .to_json()
    }

    pub fn load() -> Settings {
        config_path()
            .and_then(|p| std::fs::read(p).ok())
            .filter(|b| b.len() <= 64 * 1024)
            .and_then(|b| Settings::parse(&b))
            .unwrap_or_default()
    }

    /// Temp file, then rename, like the other writers of small config files.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = config_path() else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, self.to_json())?;
        std::fs::rename(&tmp, &path)
    }
}

/// What a folder picker returned, as the w2r root: the w2r folder itself, or the
/// `Warcraft2Remastered` folder above it (the manager creates `w2r` on the first install).
pub fn root_from_pick(picked: &Path) -> PathBuf {
    let is_w2r = picked
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("w2r"))
        || picked.join("plugins").is_dir()
        || picked.join("config").is_dir();
    if is_w2r {
        picked.to_path_buf()
    } else {
        picked.join("w2r")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = Settings {
            root: Some(PathBuf::from(if cfg!(windows) {
                "C:\\g\\w2r"
            } else {
                "/g/w2r"
            })),
            index_url: "https://example.org/index.json".into(),
        };
        assert_eq!(Settings::parse(s.to_json().as_bytes()), Some(s));
    }

    #[test]
    fn relative_root_is_dropped_and_garbage_is_none() {
        let s = Settings::parse(br#"{"root":"rel/w2r","index_url":"u"}"#).unwrap();
        assert_eq!(s.root, None);
        assert_eq!(Settings::parse(b"not json"), None);
    }

    #[test]
    fn pick_maps_to_the_w2r_folder() {
        assert_eq!(root_from_pick(Path::new("/x/w2r")), PathBuf::from("/x/w2r"));
        assert_eq!(
            root_from_pick(Path::new("/x/Warcraft2Remastered")),
            PathBuf::from("/x/Warcraft2Remastered/w2r")
        );
    }
}
