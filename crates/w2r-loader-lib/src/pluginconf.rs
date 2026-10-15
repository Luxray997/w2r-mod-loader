//! A plugin's own settings file, `config\<id>.json`, as the host writes it
//! (`{"schema":1,"values":{key: integer | string}}`, docs/abi.md). The manager edits it for the
//! user while the game is closed; the limits are the host's, so a file written here always loads.

use crate::fetch::write_atomic;
use crate::store::{Id, Root, read_bounded};
use crate::Error;
use w2r_formats::json::{self, Number, Value};

const MAX_FILE: usize = 256 * 1024;
const MAX_KEYS: usize = 128;
const MAX_STR: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Val {
    Int(i64),
    Str(String),
}

fn key_ok(k: &str) -> bool {
    (1..=64).contains(&k.len())
        && k.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'.'))
}

fn path(root: &Root, id: &str) -> Result<std::path::PathBuf, Error> {
    let id = Id::new(id)?;
    Ok(root.config_dir().join(format!("{}.json", id.as_str())))
}

/// The plugin's settings in file order; no file means none set yet.
pub fn load(root: &Root, id: &str) -> Result<Vec<(String, Val)>, Error> {
    let raw = match read_bounded(&path(root, id)?, MAX_FILE) {
        Ok(r) => r,
        Err(e) if e.code == "not_found" => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let doc = json::parse(&raw).map_err(|r| Error::new(r.code(), "the settings file is not valid"))?;
    let values = doc
        .get("values")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::new("bad_settings", "no \"values\" object"))?;
    let mut out = Vec::new();
    for (k, v) in values {
        let val = match v {
            Value::String(s) => Val::Str(s.clone()),
            Value::Number(n) if n.is_integer() => Val::Int(
                n.text
                    .parse()
                    .map_err(|_| Error::new("bad_settings", format!("{k} is out of range")))?,
            ),
            _ => return Err(Error::new("bad_settings", format!("{k} is not a number or text"))),
        };
        out.push((k.clone(), val));
    }
    Ok(out)
}

/// Replaces the file with `values` (checked against the host's limits first).
pub fn save(root: &Root, id: &str, values: &[(String, Val)]) -> Result<(), Error> {
    let file = path(root, id)?;
    if values.len() > MAX_KEYS {
        return Err(Error::new("limit", "too many settings"));
    }
    let mut pairs = Vec::new();
    for (k, v) in values {
        if !key_ok(k) {
            return Err(Error::new("bad_settings", format!("bad key {k:?}")));
        }
        pairs.push((
            k.clone(),
            match v {
                Val::Int(n) => Value::Number(Number { text: n.to_string() }),
                Val::Str(s) if s.len() <= MAX_STR => Value::String(s.clone()),
                Val::Str(_) => return Err(Error::new("limit", format!("{k} is longer than {MAX_STR} bytes"))),
            },
        ));
    }
    let doc = Value::Object(vec![
        ("schema".into(), Value::Number(Number { text: "1".into() })),
        ("values".into(), Value::Object(pairs)),
    ]);
    std::fs::create_dir_all(root.config_dir())?;
    write_atomic(&file, doc.to_json().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempRoot;

    #[test]
    fn missing_is_empty_and_a_save_round_trips() {
        let t = TempRoot::new("pluginconf");
        assert!(load(&t.0, "w2r-chat").unwrap().is_empty());
        let v = vec![
            ("bg".to_owned(), Val::Int(160)),
            ("team_hex".to_owned(), Val::Str("ff00ff".into())),
        ];
        save(&t.0, "w2r-chat", &v).unwrap();
        assert_eq!(load(&t.0, "w2r-chat").unwrap(), v);
    }

    #[test]
    fn host_written_files_load_and_bad_input_is_refused() {
        let t = TempRoot::new("pluginconf2");
        std::fs::create_dir_all(t.0.config_dir()).unwrap();
        std::fs::write(
            t.0.config_dir().join("w2r-chat.json"),
            br#"{"schema":1,"values":{"bg":160,"all_hex":""}}"#,
        )
        .unwrap();
        assert_eq!(load(&t.0, "w2r-chat").unwrap().len(), 2);
        assert_eq!(load(&t.0, "../x").unwrap_err().code, "bad_id");
        assert_eq!(
            save(&t.0, "w2r-chat", &[("Bad Key".into(), Val::Int(1))]).unwrap_err().code,
            "bad_settings"
        );
        let long = Val::Str("x".repeat(MAX_STR + 1));
        assert_eq!(save(&t.0, "w2r-chat", &[("k".into(), long)]).unwrap_err().code, "limit");
    }
}
