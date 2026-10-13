//! The host's own settings, `config\host.json` (docs/layout.md). Read once at
//! startup under the same strict JSON rules as the other files; a bad file is logged and
//! replaced by the defaults in memory (it is rewritten only when the user changes a setting).

use std::io;
use std::path::Path;

use crate::json::{self, Number, Value};

pub const FILE: &str = "host.json";
pub const MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostConfig {
    /// When hosting, refuse other players' team changes (docs/lobby-policy.md).
    pub lock_team_changes: bool,
    /// When hosting, refuse other players' slot swaps.
    pub lock_slot_swaps: bool,
    /// Draw our Create Multiplayer Game screen instead of the game's (docs/create-screen.md).
    pub enhanced_create_screen: bool,
    /// Draw our multiplayer lobby screen instead of the game's (docs/lobby-screen.md).
    pub enhanced_lobby_screen: bool,
    /// Send and receive maps in multiplayer lobbies (docs/map-download.md).
    pub map_sharing: bool,
    /// Download a missing map without asking first.
    pub auto_download: bool,
    /// Record each match played as a replay (docs/replay-research.md).
    pub record_replays: bool,
    /// Create screen: fill the game name from the map while it is empty and on each map change.
    pub auto_game_name: bool,
}

impl Default for HostConfig {
    /// Everything off except the replacement Create Multiplayer Game and lobby screens, which are
    /// on by default now that they have been verified in the game (docs/create-screen.md,
    /// docs/lobby-screen.md).
    fn default() -> Self {
        HostConfig {
            lock_team_changes: false,
            lock_slot_swaps: false,
            enhanced_create_screen: true,
            enhanced_lobby_screen: true,
            map_sharing: true,
            auto_download: false,
            record_replays: true,
            auto_game_name: false,
        }
    }
}

/// Exactly `schema` (1) and the known boolean fields; anything else refuses the file.
pub fn parse(raw: &[u8]) -> Result<HostConfig, &'static str> {
    if raw.len() > MAX_BYTES {
        return Err("too_large");
    }
    let doc = json::parse(raw).map_err(|_| "not_json")?;
    let Some(fields) = doc.as_object() else {
        return Err("not_object");
    };
    let mut cfg = HostConfig::default();
    let mut seen_schema = false;
    for (key, value) in fields {
        match key.as_str() {
            "schema" => {
                if value.as_number().and_then(Number::as_u64) != Some(1) {
                    return Err("bad_schema");
                }
                seen_schema = true;
            }
            // Retired setting (a strict host used to remove mismatched players); files that still
            // have it are accepted and it is dropped on the next write.
            "require_matching_plugins" => {
                value.as_bool().ok_or("bad_field")?;
            }
            "lock_team_changes" => cfg.lock_team_changes = value.as_bool().ok_or("bad_field")?,
            "lock_slot_swaps" => cfg.lock_slot_swaps = value.as_bool().ok_or("bad_field")?,
            "map_sharing" => cfg.map_sharing = value.as_bool().ok_or("bad_field")?,
            "auto_download" => cfg.auto_download = value.as_bool().ok_or("bad_field")?,
            "record_replays" => cfg.record_replays = value.as_bool().ok_or("bad_field")?,
            "auto_game_name" => cfg.auto_game_name = value.as_bool().ok_or("bad_field")?,
            "enhanced_lobby_screen" => {
                cfg.enhanced_lobby_screen = value.as_bool().ok_or("bad_field")?;
            }
            "enhanced_create_screen" => {
                cfg.enhanced_create_screen = value.as_bool().ok_or("bad_field")?;
            }
            _ => return Err("unknown_field"),
        }
    }
    if !seen_schema {
        return Err("bad_schema");
    }
    Ok(cfg)
}

impl HostConfig {
    pub fn to_json(&self) -> String {
        Value::Object(vec![
            (
                "schema".to_owned(),
                Value::Number(Number { text: "1".into() }),
            ),
            (
                "lock_team_changes".to_owned(),
                Value::Bool(self.lock_team_changes),
            ),
            (
                "lock_slot_swaps".to_owned(),
                Value::Bool(self.lock_slot_swaps),
            ),
            (
                "enhanced_create_screen".to_owned(),
                Value::Bool(self.enhanced_create_screen),
            ),
            (
                "enhanced_lobby_screen".to_owned(),
                Value::Bool(self.enhanced_lobby_screen),
            ),
            ("map_sharing".to_owned(), Value::Bool(self.map_sharing)),
            ("auto_download".to_owned(), Value::Bool(self.auto_download)),
            (
                "record_replays".to_owned(),
                Value::Bool(self.record_replays),
            ),
            (
                "auto_game_name".to_owned(),
                Value::Bool(self.auto_game_name),
            ),
        ])
        .to_json()
    }
}

/// Reads `root\config\host.json`. A missing file is the defaults; anything else wrong comes
/// back as a warning text next to the defaults.
pub fn read(root: &Path) -> (HostConfig, Option<String>) {
    use std::io::Read;
    let path = root.join("config").join(FILE);
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return (HostConfig::default(), None),
        Err(e) => {
            return (
                HostConfig::default(),
                Some(format!("{FILE} unreadable ({e}); using defaults")),
            );
        }
    };
    let mut raw = Vec::new();
    if let Err(e) = file.take(MAX_BYTES as u64 + 1).read_to_end(&mut raw) {
        return (
            HostConfig::default(),
            Some(format!("{FILE} unreadable ({e}); using defaults")),
        );
    }
    match parse(&raw) {
        Ok(cfg) => (cfg, None),
        Err(code) => (
            HostConfig::default(),
            Some(format!("{FILE} refused ({code}); using defaults")),
        ),
    }
}

/// Writes atomically: a temp file next to it, then a rename.
pub fn write(root: &Path, cfg: &HostConfig) -> io::Result<()> {
    let path = root.join("config").join(FILE);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, cfg.to_json())?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_round_trips_and_refuses_the_unknown() {
        let cfg = HostConfig {
            lock_team_changes: true,
            lock_slot_swaps: false,
            enhanced_create_screen: true,
            enhanced_lobby_screen: true,
            map_sharing: false,
            auto_download: true,
            record_replays: false,
            auto_game_name: true,
        };
        assert_eq!(parse(cfg.to_json().as_bytes()), Ok(cfg));
        assert_eq!(
            parse(b"{\"schema\": 1, \"lock_slot_swaps\": 1}"),
            Err("bad_field")
        );
        assert_eq!(
            parse(b"{\"schema\": 1}"),
            Ok(HostConfig::default()),
            "fields are optional"
        );
        assert!(
            HostConfig::default().enhanced_create_screen,
            "the replacement screen is on unless the file says otherwise"
        );
        assert!(
            HostConfig::default().enhanced_lobby_screen,
            "the lobby screen is on unless the file says otherwise"
        );
        assert!(
            HostConfig::default().record_replays,
            "matches are recorded unless the file says otherwise"
        );
        assert_eq!(
            parse(b"{\"schema\": 1, \"record_replays\": false}").map(|c| c.record_replays),
            Ok(false)
        );
        assert_eq!(
            parse(b"{\"schema\": 1, \"enhanced_create_screen\": false}")
                .map(|c| c.enhanced_create_screen),
            Ok(false)
        );
        assert_eq!(
            parse(b"{\"require_matching_plugins\": true}"),
            Err("bad_schema")
        );
        assert_eq!(parse(b"{\"schema\": 2}"), Err("bad_schema"));
        // The retired setting is still accepted (and ignored), but only as a boolean.
        assert_eq!(
            parse(b"{\"schema\": 1, \"require_matching_plugins\": true}"),
            Ok(HostConfig::default())
        );
        assert_eq!(
            parse(b"{\"schema\": 1, \"require_matching_plugins\": 1}"),
            Err("bad_field")
        );
        assert_eq!(
            parse(b"{\"schema\": 1, \"strict\": true}"),
            Err("unknown_field")
        );
        assert_eq!(parse(b"[]"), Err("not_object"));
        assert_eq!(parse(b"nope"), Err("not_json"));
        assert_eq!(parse(&vec![b' '; MAX_BYTES + 1]), Err("too_large"));
    }

    #[test]
    fn read_and_write_through_a_root() {
        let root = std::env::temp_dir().join(format!("w2r-hostcfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config")).unwrap();
        assert_eq!(
            read(&root),
            (HostConfig::default(), None),
            "missing is default"
        );
        let cfg = HostConfig {
            lock_team_changes: false,
            lock_slot_swaps: true,
            enhanced_create_screen: false,
            enhanced_lobby_screen: false,
            map_sharing: true,
            auto_download: false,
            record_replays: true,
            auto_game_name: false,
        };
        write(&root, &cfg).unwrap();
        assert_eq!(read(&root), (cfg, None));
        std::fs::write(root.join("config").join(FILE), b"{\"schema\": 3}").unwrap();
        let (got, warning) = read(&root);
        assert_eq!(got, HostConfig::default());
        assert_eq!(
            warning.as_deref(),
            Some("host.json refused (bad_schema); using defaults")
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
