//! The GUI's own settings: which w2r folder and which index. Kept in
//! `%APPDATA%\w2rpm\gui.json` (override: `W2R_LOADER_GUI_CONFIG`). Not part of the trust record
//! (`state.json`); a bad file is ignored and the defaults are used.

use std::path::{Path, PathBuf};
use w2r_formats::json::{self, Value};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// The w2r folder; `None` means `%USERPROFILE%\Saved Games\Warcraft2Remastered\w2r`.
    pub root: Option<PathBuf>,
    pub index_url: String,
    /// Last window position and size, `[x, y, width, height]` in points.
    pub window: Option<[i32; 4]>,
    /// Last tab (0 Installed, 1 Browse, 2 Settings), Browse filter (0 all, 1 not installed,
    /// 2 has update) and sort (0 default, 1 name, 2 author).
    pub tab: u8,
    pub filter: u8,
    pub sort: u8,
    /// Interface scale in percent; 0 means the default 100.
    pub zoom_pct: u32,
}

/// A small enum index stored as a number; anything out of range is 0.
fn small(v: &Value, key: &str, max: u64) -> u8 {
    v.get(key)
        .and_then(|x| x.as_number()?.as_u64())
        .filter(|n| *n <= max)
        .map_or(0, |n| n as u8)
}

pub fn config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("W2R_LOADER_GUI_CONFIG") {
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
            tab: small(&v, "tab", 2),
            filter: small(&v, "filter", 2),
            sort: small(&v, "sort", 2),
            zoom_pct: v
                .get("zoom_pct")
                .and_then(|x| x.as_number()?.as_u64())
                .map_or(0, |n| (n as u32).clamp(70, 200)),
            window: v.get("window").and_then(Value::as_array).and_then(|a| {
                let n: Vec<i32> = a
                    .iter()
                    .filter_map(|x| x.as_number()?.text.parse().ok())
                    .collect();
                <[i32; 4]>::try_from(n).ok()
            }),
        })
    }

    pub fn to_json(&self) -> String {
        let s = |t: &str| Value::String(t.to_owned());
        let window = self.window.map_or(Value::Null, |w| {
            Value::Array(
                w.iter()
                    .map(|n| Value::Number(json::Number { text: n.to_string() }))
                    .collect(),
            )
        });
        let n = |x: u32| Value::Number(json::Number { text: x.to_string() });
        Value::Object(vec![
            ("tab".into(), n(self.tab.into())),
            ("filter".into(), n(self.filter.into())),
            ("sort".into(), n(self.sort.into())),
            ("zoom_pct".into(), n(self.zoom_pct)),
            ("window".into(), window),
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
            window: Some([10, -20, 800, 600]),
            tab: 1,
            filter: 2,
            sort: 1,
            zoom_pct: 120,
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
