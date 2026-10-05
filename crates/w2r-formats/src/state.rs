//! `config\state.json`: the user's trust record (docs/layout.md): which plugins are enabled,
//! the pinned DLL hash and the approved capabilities. Read once at startup, strictly: unknown
//! fields, bad ids, bad hashes or unknown capabilities refuse the whole file, and the host then
//! loads nothing rather than guess.

use crate::json::{self, Value};
use crate::manifest::{CAPABILITIES, Reason, is_valid_id};
use crate::semver::Version;

pub const MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginState {
    pub id: String,
    pub version: Version,
    pub enabled: bool,
    pub sha256: [u8; 32],
    pub approved_capabilities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct State {
    /// In file order.
    pub plugins: Vec<PluginState>,
}

impl State {
    pub fn get(&self, id: &str) -> Option<&PluginState> {
        self.plugins.iter().find(|p| p.id == id)
    }
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(core::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(out)
}

fn keys_are(pairs: &[(String, Value)], expected: &[&str]) -> bool {
    let mut keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
    keys.sort_unstable();
    let mut want = expected.to_vec();
    want.sort_unstable();
    keys == want
}

fn plugin(id: &str, v: &Value) -> Result<PluginState, Reason> {
    if !is_valid_id(id) {
        return Err("bad_id");
    }
    let pairs = v.as_object().ok_or("bad_type")?;
    if !keys_are(
        pairs,
        &["version", "enabled", "sha256", "approved_capabilities"],
    ) {
        return Err("bad_plugin");
    }
    let version = v
        .get("version")
        .and_then(Value::as_str)
        .and_then(Version::parse)
        .ok_or("bad_version")?;
    let enabled = v
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or("bad_type")?;
    let sha256 = v
        .get("sha256")
        .and_then(Value::as_str)
        .and_then(hex32)
        .ok_or("bad_sha256")?;
    let caps = v
        .get("approved_capabilities")
        .and_then(Value::as_array)
        .ok_or("bad_type")?;
    let mut approved_capabilities: Vec<String> = Vec::with_capacity(caps.len());
    for c in caps {
        let c = c.as_str().ok_or("bad_capability")?;
        if !CAPABILITIES.contains(&c) || approved_capabilities.iter().any(|s| s == c) {
            return Err("bad_capability");
        }
        approved_capabilities.push(c.to_owned());
    }
    Ok(PluginState {
        id: id.to_owned(),
        version,
        enabled,
        sha256,
        approved_capabilities,
    })
}

/// Parses raw `state.json` bytes. `Err` carries a reason code.
pub fn parse(raw: &[u8]) -> Result<State, Reason> {
    if raw.len() > MAX_BYTES {
        return Err("too_large");
    }
    let doc = json::parse(raw).map_err(|r| r.code())?;
    let pairs = doc.as_object().ok_or("not_object")?;
    if !keys_are(pairs, &["schema", "plugins"]) {
        return Err("bad_fields");
    }
    if doc
        .get("schema")
        .and_then(Value::as_number)
        .and_then(|n| n.as_u64())
        != Some(1)
    {
        return Err("bad_schema");
    }
    let plugins = doc
        .get("plugins")
        .and_then(Value::as_object)
        .ok_or("bad_type")?;
    let plugins = plugins
        .iter()
        .map(|(id, v)| plugin(id, v))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(State { plugins })
}

impl State {
    /// The file form (docs/layout.md), with plugins in their current order.
    pub fn to_json(&self) -> String {
        let plugins = self
            .plugins
            .iter()
            .map(|p| {
                let caps = p
                    .approved_capabilities
                    .iter()
                    .map(|c| Value::String(c.clone()))
                    .collect();
                let hex: String = p.sha256.iter().map(|b| format!("{b:02x}")).collect();
                let fields = vec![
                    (
                        "version".to_owned(),
                        Value::String(version_text(&p.version)),
                    ),
                    ("enabled".to_owned(), Value::Bool(p.enabled)),
                    ("sha256".to_owned(), Value::String(hex)),
                    ("approved_capabilities".to_owned(), Value::Array(caps)),
                ];
                (p.id.clone(), Value::Object(fields))
            })
            .collect();
        Value::Object(vec![
            (
                "schema".to_owned(),
                Value::Number(json::Number { text: "1".into() }),
            ),
            ("plugins".to_owned(), Value::Object(plugins)),
        ])
        .to_json()
    }

    /// Flips one plugin's `enabled`; false when the id is unknown.
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> bool {
        match self.plugins.iter_mut().find(|p| p.id == id) {
            Some(p) => {
                p.enabled = enabled;
                true
            }
            None => false,
        }
    }
}

fn version_text(v: &Version) -> String {
    v.to_string()
}

/// Writes `state.json` atomically: a temp file next to it, then a rename (docs/layout.md).
pub fn write(path: &std::path::Path, state: &State) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, state.to_json())?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKETCH: &str = r#"{
  "schema": 1,
  "plugins": {
    "example-plugin": {
      "version": "1.2.0",
      "enabled": true,
      "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
      "approved_capabilities": ["game.read", "chat.read"]
    },
    "other": {
      "version": "0.1.0-rc.1",
      "enabled": false,
      "sha256": "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855",
      "approved_capabilities": []
    }
  }
}"#;

    #[test]
    fn parses_the_layout_sketch() {
        let s = parse(SKETCH.as_bytes()).unwrap();
        assert_eq!(s.plugins.len(), 2);
        let p = s.get("example-plugin").unwrap();
        assert!(p.enabled);
        assert_eq!(p.sha256[0], 0xe3);
        assert_eq!(p.sha256[31], 0x55);
        assert_eq!(p.approved_capabilities, ["game.read", "chat.read"]);
        assert_eq!(p.version, Version::parse("1.2.0").unwrap());
        let o = s.get("other").unwrap();
        assert!(!o.enabled);
        assert_eq!(o.sha256, p.sha256, "hex case does not matter");
        assert!(s.get("nope").is_none());
        assert_eq!(
            parse(br#"{"schema": 1, "plugins": {}}"#).unwrap(),
            State::default()
        );
    }

    #[test]
    fn refuses_anything_off_spec() {
        let with = |from: &str, to: &str| SKETCH.replacen(from, to, 1);
        let cases = [
            (with("\"schema\": 1", "\"schema\": 2"), "bad_schema"),
            (
                with("\"schema\": 1", "\"schema\": 1, \"extra\": 0"),
                "bad_fields",
            ),
            (with("\"example-plugin\"", "\"Example\""), "bad_id"),
            (with("\"example-plugin\"", "\"con\""), "bad_id"),
            (
                with("\"version\": \"1.2.0\"", "\"version\": \"1.2\""),
                "bad_version",
            ),
            (with("\"enabled\": true", "\"enabled\": 1"), "bad_type"),
            (
                with("\"sha256\": \"e3b0", "\"sha256\": \"zzb0"),
                "bad_sha256",
            ),
            (with("b855\",", "b85\","), "bad_sha256"),
            (with("\"chat.read\"", "\"fs.data\""), "bad_capability"),
            (
                with(
                    "\"game.read\", \"chat.read\"",
                    "\"game.read\", \"game.read\"",
                ),
                "bad_capability",
            ),
            (
                with(
                    "\"approved_capabilities\": [\"game.read\", \"chat.read\"]",
                    "\"approved_capabilities\": [\"game.read\"], \"pinned\": true",
                ),
                "bad_plugin",
            ),
            (with("\"enabled\": true,", ""), "bad_plugin"),
            ("[]".to_owned(), "not_object"),
            ("{".to_owned(), "not_json"),
            (
                with("\"schema\": 1", "\"schema\": 1, \"schema\": 1"),
                "duplicate_key",
            ),
            (with("\"plugins\": {", "\"plugins\": ["), "not_json"),
        ];
        for (raw, expect) in cases {
            assert_eq!(parse(raw.as_bytes()), Err(expect), "{raw}");
        }
        assert_eq!(parse(&vec![b' '; MAX_BYTES + 1]), Err("too_large"));
    }
}

#[cfg(test)]
mod write_tests {
    use super::*;

    #[test]
    fn to_json_round_trips_and_write_is_atomic() {
        let raw = br#"{"schema": 1, "plugins": {"b-plugin": {"version": "1.2.0-rc.1", "enabled": true, "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "approved_capabilities": ["chat.write"]}, "a-plugin": {"version": "0.1.0", "enabled": false, "sha256": "0000000000000000000000000000000000000000000000000000000000000000", "approved_capabilities": []}}}"#;
        let mut state = parse(raw).unwrap();
        let again = parse(state.to_json().as_bytes()).unwrap();
        assert_eq!(again, state);
        assert_eq!(again.plugins[0].id, "b-plugin", "order kept");
        assert!(state.set_enabled("a-plugin", true));
        assert!(!state.set_enabled("nope", true));

        let dir = std::env::temp_dir().join(format!("w2r-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        write(&path, &state).unwrap();
        assert!(!path.with_extension("json.tmp").exists());
        let back = parse(&std::fs::read(&path).unwrap()).unwrap();
        assert!(back.get("a-plugin").unwrap().enabled);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
