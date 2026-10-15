//! "Is a newer manager published?" The answer is a notice only: nothing is downloaded or run.
//! `manager.json` sits next to `index.json`: `{"schema":1,"version":"0.3.0","url":"https://..."}`
//! where `url` is the page to get it from. Unknown fields are ignored so it can grow, and a missing
//! or bad file just means no notice.

use crate::fetch::Fetcher;
use crate::index::url_ok;
use w2r_formats::json::{self, Value};
use w2r_formats::semver::Version;

const MAX_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Latest {
    pub version: Version,
    pub url: String,
}

/// `manager.json` lives in the same folder as `index.json`.
pub fn url_for(index_url: &str) -> Option<String> {
    index_url
        .rsplit_once('/')
        .map(|(dir, _)| format!("{dir}/manager.json"))
}

pub fn parse(raw: &[u8]) -> Option<Latest> {
    let doc = json::parse(raw).ok()?;
    if doc.get("schema")?.as_number()?.as_u64()? != 1 {
        return None;
    }
    let version = Version::parse(doc.get("version").and_then(Value::as_str)?)?;
    let url = doc.get("url").and_then(Value::as_str)?.to_owned();
    url_ok(&url).then_some(Latest { version, url })
}

/// The published manager when it is newer than `current`.
pub fn newer(fetcher: &dyn Fetcher, index_url: &str, current: &Version) -> Option<Latest> {
    let raw = fetcher.get(&url_for(index_url)?, MAX_BYTES).ok()?;
    parse(&raw).filter(|l| l.version > *current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_refuses() {
        let ok = br#"{"schema":1,"version":"0.3.0","url":"https://example.com/w2rpm"}"#;
        assert_eq!(parse(ok).unwrap().version, Version::parse("0.3.0").unwrap());
        assert!(parse(br#"{"schema":2,"version":"0.3.0","url":"https://example.com/"}"#).is_none());
        assert!(parse(br#"{"schema":1,"version":"0.3.0","url":"http://example.com/"}"#).is_none());
        assert!(parse(b"nope").is_none());
        assert_eq!(
            url_for("https://x.io/p/index.json").unwrap(),
            "https://x.io/p/manager.json"
        );
    }
}
