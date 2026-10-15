//! Checks `index.json` (docs/index.md): size, then the strict parse and the checks in the spec
//! table order. Reference: `tools/w2r_spec/index.py`.

use crate::Reason;
use w2r_formats::json::{self, Value};
use w2r_formats::manifest::{self, Manifest, is_valid_id};
use w2r_formats::semver::Version;

pub const MAX_INDEX_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_PLUGINS: usize = 1000;
pub const MAX_VERSIONS: usize = 200;
const MAX_SERIAL: u64 = (1 << 53) - 1;

#[derive(Clone, Debug)]
pub struct VersionEntry {
    pub version: Version,
    pub manifest: Manifest,
    /// The index copy of the manifest, compact JSON; the packaged `plugin.json` must equal it.
    pub manifest_json: String,
    pub url: String,
    pub sha256: [u8; 32],
    pub size: u64,
    pub min_manager: Version,
}

#[derive(Clone, Debug)]
pub struct PluginEntry {
    pub id: String,
    pub versions: Vec<VersionEntry>,
}

#[derive(Clone, Debug)]
pub struct Index {
    pub serial: u64,
    pub generated: i64,
    pub plugins: Vec<PluginEntry>,
}

impl Index {
    pub fn plugin(&self, id: &str) -> Option<&PluginEntry> {
        self.plugins.iter().find(|p| p.id == id)
    }
}

/// The checks of the spec table: size, strict parse, fields, rollback, entries. `cached_serial`
/// is the highest serial accepted so far (0 when none).
pub fn parse_index(raw: &[u8], cached_serial: u64) -> Result<Index, Reason> {
    if raw.len() > MAX_INDEX_BYTES {
        return Err("too_large");
    }
    let doc = json::parse(raw).map_err(|r| r.code())?;
    if !matches!(doc, Value::Object(_)) {
        return Err("bad_type");
    }
    keys(&doc, &["schema", "serial", "generated", "plugins"], &[])?;
    let schema = field(&doc, "schema");
    if !is_int(schema) || int(schema) != Some(1) {
        return Err("bad_schema");
    }
    let serial = int(field(&doc, "serial"))
        .filter(|&s| s <= MAX_SERIAL)
        .ok_or("bad_type")?;
    let generated = time(field(&doc, "generated"))?;
    if serial < cached_serial {
        return Err("rollback");
    }
    let plugins = check_plugins(field(&doc, "plugins"))?;
    Ok(Index {
        serial,
        generated,
        plugins,
    })
}

/// A field `keys` already proved present.
pub(crate) fn field<'a>(v: &'a Value, key: &str) -> &'a Value {
    v.get(key).unwrap_or(&Value::Null)
}

pub(crate) fn is_int(v: &Value) -> bool {
    v.as_number().is_some_and(|n| n.is_integer())
}

/// An integer that is non-negative and fits `u64`; `None` for negatives, floats, other types.
pub(crate) fn int(v: &Value) -> Option<u64> {
    v.as_number().and_then(|n| n.as_u64())
}

pub(crate) fn keys(obj: &Value, required: &[&str], optional: &[&str]) -> Result<(), Reason> {
    let pairs = obj.as_object().ok_or("bad_type")?;
    if pairs
        .iter()
        .any(|(k, _)| !required.contains(&k.as_str()) && !optional.contains(&k.as_str()))
    {
        return Err("unknown_field");
    }
    if required.iter().any(|r| obj.get(r).is_none()) {
        return Err("missing_field");
    }
    Ok(())
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Unix seconds to `YYYY-MM-DDTHH:MM:SSZ` (UTC), the inverse of [`parse_time`].
pub fn format_time(t: i64) -> String {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// `YYYY-MM-DDTHH:MM:SSZ`, a real date and time, to unix seconds.
pub fn parse_time(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20 {
        return None;
    }
    for (i, &c) in b.iter().enumerate() {
        let ok = match i {
            4 | 7 => c == b'-',
            10 => c == b'T',
            13 | 16 => c == b':',
            19 => c == b'Z',
            _ => c.is_ascii_digit(),
        };
        if !ok {
            return None;
        }
    }
    let n = |r: std::ops::Range<usize>| s[r].parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (
        n(0..4)?,
        n(5..7)?,
        n(8..10)?,
        n(11..13)?,
        n(14..16)?,
        n(17..19)?,
    );
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = match mo {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if y < 1 || d < 1 || d > dim || h > 23 || mi > 59 || se > 59 {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se)
}

pub(crate) fn time(v: &Value) -> Result<i64, Reason> {
    v.as_str().and_then(parse_time).ok_or("bad_time")
}

/// `https`, ASCII, at most 2,048 characters, no whitespace or controls, a host, no userinfo.
pub fn url_ok(u: &str) -> bool {
    if u.len() > 2048 || !u.is_ascii() || u.bytes().any(|c| c <= b' ') {
        return false;
    }
    let Some((scheme, rest)) = u.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return false;
    }
    let host = if let Some(inner) = authority.strip_prefix('[') {
        inner.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    !host.is_empty()
}

fn check_plugins(v: &Value) -> Result<Vec<PluginEntry>, Reason> {
    let list = v.as_array().ok_or("bad_type")?;
    if list.len() > MAX_PLUGINS {
        return Err("too_many");
    }
    let mut out: Vec<PluginEntry> = Vec::new();
    for p in list {
        keys(p, &["id", "versions"], &[])?;
        let id = field(p, "id")
            .as_str()
            .filter(|s| is_valid_id(s))
            .ok_or("bad_id")?;
        if out.iter().any(|e| e.id == id) {
            return Err("duplicate");
        }
        let versions = field(p, "versions")
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or("bad_type")?;
        if versions.len() > MAX_VERSIONS {
            return Err("too_many");
        }
        let mut entries: Vec<VersionEntry> = Vec::new();
        for ver in versions {
            let e = check_version(id, ver, &entries)?;
            entries.push(e);
        }
        out.push(PluginEntry {
            id: id.to_owned(),
            versions: entries,
        });
    }
    Ok(out)
}

fn check_version(id: &str, v: &Value, seen: &[VersionEntry]) -> Result<VersionEntry, Reason> {
    keys(
        v,
        &[
            "version",
            "manifest",
            "url",
            "sha256",
            "size",
            "min_manager",
        ],
        &[],
    )?;
    let version_text = field(v, "version").as_str();
    let version = version_text.and_then(Version::parse);
    let min_manager = field(v, "min_manager").as_str().and_then(Version::parse);
    let (Some(version_text), Some(version), Some(min_manager)) =
        (version_text, version, min_manager)
    else {
        return Err("bad_version");
    };
    if seen.iter().any(|e| e.manifest.version_text == version_text) {
        return Err("duplicate");
    }
    let m = field(v, "manifest");
    if !matches!(m, Value::Object(_)) {
        return Err("bad_manifest");
    }
    let manifest_json = m.to_json();
    let manifest = manifest::validate(manifest_json.as_bytes()).map_err(|_| "bad_manifest")?;
    if manifest.id != id || manifest.version_text != version_text {
        return Err("mismatch");
    }
    let url = field(v, "url")
        .as_str()
        .filter(|u| url_ok(u))
        .ok_or("bad_url")?
        .to_owned();
    let sha256 = field(v, "sha256")
        .as_str()
        .and_then(hex32)
        .ok_or("bad_hash")?;
    let size = field(v, "size");
    if !is_int(size) {
        return Err("bad_type");
    }
    let size = int(size)
        .filter(|s| (1..=MAX_PACKAGE_BYTES).contains(s))
        .ok_or("bad_size")?;
    Ok(VersionEntry {
        version,
        manifest,
        manifest_json,
        url,
        sha256,
        size,
        min_manager,
    })
}

/// 64 lowercase hex characters.
pub(crate) fn hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64
        || !s
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VECTORS: &str = include_str!("../../../docs/vectors/index.json");

    #[test]
    fn times() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2026-10-15T00:00:00Z"), Some(1_792_022_400));
        assert!(parse_time("2024-02-29T00:00:00Z").is_some());
        for t in [0, 1_792_022_400, 951_782_400, 4_102_444_799] {
            assert_eq!(parse_time(&format_time(t)), Some(t));
        }
        assert_eq!(parse_time("2025-02-29T00:00:00Z"), None);
        assert_eq!(parse_time("2026-10-15T24:00:00Z"), None);
        assert_eq!(parse_time("2026-10-15T00:00:00+00:00"), None);
    }

    #[test]
    fn urls() {
        assert!(url_ok("https://example.com/a.zip"));
        assert!(url_ok("https://example.com:8443/a.zip"));
        assert!(!url_ok("http://example.com/a.zip"));
        assert!(!url_ok("https://user@example.com/a.zip"));
        assert!(!url_ok("https://@example.com/a.zip"));
        assert!(!url_ok("https:///a.zip"));
        assert!(!url_ok("https://example.com/a b"));
    }

    /// Every vector in docs/vectors/index.json must give the reference result.
    #[test]
    fn spec_vectors() {
        let doc = json::parse(VECTORS.as_bytes()).expect("vectors parse");
        let cached = int(field(&doc, "cached_serial")).unwrap();
        let mut n = 0;
        for case in field(&doc, "cases").as_array().unwrap() {
            let name = field(case, "name").as_str().unwrap();
            let expect = field(case, "expect").as_str().unwrap();
            let raw = field(case, "index").as_str().unwrap();
            let got = match parse_index(raw.as_bytes(), cached) {
                Ok(_) => "ok",
                Err(r) => r,
            };
            assert_eq!(got, expect, "vector {name}");
            n += 1;
        }
        assert!(n >= 40, "ran {n} vectors");
    }
}
