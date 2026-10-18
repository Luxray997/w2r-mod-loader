//! The one `config\host.json` setting the manager edits: whether matches against computer players
//! are left out of the saved replays (`skip_computer_replays`, default on). The in-game host owns
//! the file and refuses unknown fields, so every other key is written back untouched.

use std::path::Path;

use w2r_formats::json::{self, Value};

use crate::Error;

const KEY: &str = "skip_computer_replays";
const MAX_BYTES: u64 = 64 * 1024;

fn path(root: &Path) -> std::path::PathBuf {
    root.join("config").join("host.json")
}

fn read_doc(root: &Path) -> Result<Option<Vec<(String, Value)>>, Error> {
    use std::io::Read;
    let file = match std::fs::File::open(path(root)) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut raw = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut raw)?;
    if raw.len() as u64 > MAX_BYTES {
        return Err(Error::new("too_large", "host.json"));
    }
    let doc = json::parse(&raw).map_err(|e| Error::new("bad_host_json", e.code()))?;
    match doc {
        Value::Object(fields) => Ok(Some(fields)),
        _ => Err(Error::new("bad_host_json", "not_object")),
    }
}

/// True when a game with computer players is not saved (the default when the file or key is absent).
pub fn skip_computer_replays(root: &Path) -> Result<bool, Error> {
    let Some(fields) = read_doc(root)? else {
        return Ok(true);
    };
    match fields.iter().find(|(k, _)| k == KEY) {
        None => Ok(true),
        Some((_, v)) => v
            .as_bool()
            .ok_or_else(|| Error::new("bad_host_json", format!("{KEY} is not true or false"))),
    }
}

pub fn set_skip_computer_replays(root: &Path, on: bool) -> Result<(), Error> {
    let mut fields = read_doc(root)?.unwrap_or_else(|| {
        vec![(
            "schema".to_owned(),
            Value::Number(json::Number { text: "1".into() }),
        )]
    });
    match fields.iter_mut().find(|(k, _)| k == KEY) {
        Some((_, v)) => *v = Value::Bool(on),
        None => fields.push((KEY.to_owned(), Value::Bool(on))),
    }
    let p = path(root);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, Value::Object(fields).to_json())?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_on_and_keeps_other_keys() {
        let root = std::env::temp_dir().join(format!("w2r-hostconf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(skip_computer_replays(&root).unwrap(), true);
        set_skip_computer_replays(&root, false).unwrap();
        assert_eq!(skip_computer_replays(&root).unwrap(), false);
        std::fs::write(
            path(&root),
            b"{\"schema\": 1, \"net_panel\": true, \"skip_computer_replays\": true}",
        )
        .unwrap();
        set_skip_computer_replays(&root, false).unwrap();
        let text = std::fs::read_to_string(path(&root)).unwrap();
        assert!(text.contains("\"net_panel\":true") || text.contains("\"net_panel\": true"));
        assert_eq!(skip_computer_replays(&root).unwrap(), false);
        std::fs::write(path(&root), b"nope").unwrap();
        assert!(skip_computer_replays(&root).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
