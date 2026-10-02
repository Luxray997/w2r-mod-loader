# Phase 2a-1: `w2r-spec` crate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A pure-Rust `w2r-spec` crate that parses and validates `plugin.json` and `state.json` exactly as the specs and the Python reference do, and computes the simulation fingerprint and peer tag, proven against the shared test vectors. Also closes two JSON-layer gaps found while porting and adds the missing `state.json` spec.

**Architecture:** One safe-Rust library crate (`#![forbid(unsafe_code)]`) with a strict JSON layer on top of `serde_json` (depth limit, duplicate detection, surrogate refusal), small hand-written parsers for SemVer/build/requirement/id/entry (no regex dependency), and thin typed parsers per document. Every rule is pinned by the Phase 1 vectors (`docs/vectors/*.json`); the Python reference in `tools/` stays the oracle and is changed first whenever a rule changes.

**Tech Stack:** Rust 1.92 (stable), `serde` + `serde_json` 1.x, `sha2` 0.11, Python 3.12 for the oracle, `cargo-fuzz` targets (not run in this plan).

**Spec:** `docs/superpowers/specs/2026-10-02-phase2a-loader-core-design.md` (the `w2r-spec` crate, `docs/state.md`, vectors) plus the Phase 1 specs it builds on.

This is plan 1 of 3 for sub-project 2a. Plan 2 (`w2r-abi` and platform-neutral `w2r-host`) and plan 3 (Windows `w2r-core`, guard, proxy, harness, Wine scenarios) are written after this one lands, because they consume its interfaces.

## Global Constraints

- `w2r-spec` has `#![forbid(unsafe_code)]`, no `regex` dependency, no async runtime, no logging framework. External crates: `serde`, `serde_json`, `sha2` only (dev-dependencies: none beyond these).
- Strict JSON: UTF-8 only, no BOM, no `NaN`/`Infinity`, no lone surrogates, no duplicate keys, nesting at most 32 containers (root = depth 1). **`not_json` takes precedence over `duplicate_key`**: a duplicate key is reported only when the whole document is otherwise valid JSON within the limits.
- Size limits: manifest 65,536 bytes; `state.json` 1,048,576 bytes; at most 1,000 plugins in `state.json`.
- Plugin id `[a-z0-9][a-z0-9-]{1,62}[a-z0-9]`, not a Windows device name (`con prn aux nul com1-9 lpt1-9`). SemVer 2.0.0 without build metadata, at most 64 bytes, numeric parts at most 4,294,967,295. Every pattern matches the whole string.
- Capabilities needing approval: `game.read`, `chat.read`, `chat.write`, `net.send`, `hooks.raw`. Implicit capabilities are never listed.
- Version requirement matching uses full SemVer 2.0.0 precedence including pre-release identifiers; comparators carry no pre-release. (Ruling recorded in `docs/load-policy.md` by Task 5.)
- Simulation fingerprint: first 4 bytes of SHA-256 over id-sorted (bytewise) `id 0x00 version 0x00 sha256(32 raw bytes)`; all zero for the empty set; duplicate ids are an error. Peer tag: first 4 bytes of SHA-256 of the UTF-8 plugin id.
- Rust results are `Result<T, Reject>` where `Reject(&'static str)` carries the same reason code strings as `docs/manifest.md`; the first failing check in the documented order wins.
- Commits end with the trailer `Claude-Session: https://claude.ai/code/session_015GrKhNbxr1N7QrzwX3YLE7` (pass it as a second `-m`).
- Never commit `target/` or `__pycache__`.

## Review Focus

1. **Parser disagreement between Python and Rust** on the same bytes: big integers (`2**70`), floats (`1.0`, `1e400`), `-0`, depth exactly 32 vs 33, duplicate-then-syntax-error ordering. A mismatch breaks the "same result for every vector" promise (Tasks 1, 3).
2. **Panics on hostile input** (truncated documents, deeply nested input, huge numbers, invalid UTF-8): a parser that can panic is a denial of service for the game (Task 3 truncation test, Task 5 fuzz targets).
3. **Version precedence edge cases:** pre-release ordering, numeric identifiers longer than 10 digits, `=` requirements against pre-releases (Task 2).
4. **`state.json` abuse:** 1,001 plugins, a plugin key that is a device name or has a trailing newline, an oversize file (Task 4).
5. **Fingerprint input hazards:** duplicate ids, ids that sort differently by bytes than by chars, empty set (Task 5).

---

### Task 1: Workspace, strict JSON layer, Python parity fixes

**Files:**
- Create: `Cargo.toml`, `.gitignore`, `crates/w2r-spec/Cargo.toml`, `crates/w2r-spec/src/lib.rs`, `crates/w2r-spec/src/json.rs`
- Create: `crates/w2r-spec/tests/common/mod.rs`, `crates/w2r-spec/tests/json_vectors.rs`
- Modify: `tools/w2r_spec/jsonutil.py`, `tools/gen_manifest_vectors.py`, `docs/vectors/manifest.json` (regenerated), `docs/manifest.md`
- Untrack: the committed `tools/**/__pycache__/*.pyc` files

**Interfaces:**
- Produces: `w2r_spec::json::{Json, Reject, parse_json, MAX_DEPTH}` where `Json` is `Null | Bool(bool) | Int(i128) | Float(f64) | Str(String) | Arr(Vec<Json>) | Obj(Vec<(String, Json)>)` (object entries keep document order) and `pub struct Reject(pub &'static str)`; `parse_json(raw: &[u8]) -> Result<Json, Reject>` returning `Reject("not_json")` or `Reject("duplicate_key")`.
- Produces: `tests/common/mod.rs` helpers `vectors(name: &str) -> Vec<serde_json::Value>` and `raw_of(case: &Value) -> Vec<u8>` reused by later tasks.

- [ ] **Step 1: Hygiene commit-free setup**

Run:
```bash
cat > .gitignore <<'EOF'
target/
__pycache__/
*.pyc
EOF
git rm -r -q --cached tools/w2r_spec/__pycache__ tools/tests/__pycache__ 2>/dev/null; git status --short | head
```
Expected: the `.pyc` files show as `D` (staged deletions), `.gitignore` as untracked.

- [ ] **Step 2: Write the failing Python vectors (parity rules)**

In `tools/gen_manifest_vectors.py`, add these entries to `RAW_CASES` (before the `oversize` entry):

```python
    ("duplicate_inner_then_syntax_error", "not_json", '{"a": {"b": 1, "b": 2}, x}', 0),
    ("duplicate_then_syntax_error", "not_json", '{"schema": 1, "schema": 2, x}', 0),
    ("duplicate_and_too_deep", "not_json",
     '{"a": 1, "a": 2, "x": ' + "[" * 32 + "]" * 32 + "}", 0),
    ("huge_exponent", "not_json", '{"schema": 1e400}', 0),
    ("float_one_is_not_an_integer", "bad_schema",
     json.dumps(base_manifest()).replace('"schema": 1,', '"schema": 1.0,'), 0),
```
(`float_one_is_not_an_integer` needs a full manifest so that only `schema` is wrong.)

Run: `python3 tools/gen_manifest_vectors.py && python3 -m unittest discover -s tools/tests -t tools 2>&1 | grep -E "^(FAIL|ERROR):|Ran|FAILED|OK"`
Expected: FAIL for `duplicate_inner_then_syntax_error`, `duplicate_and_too_deep`, `huge_exponent` (the others already pass).

- [ ] **Step 3: Fix the Python oracle**

Replace the body of `tools/w2r_spec/jsonutil.py` below the imports with:

```python
import json
import math


class Reject(Exception):
    def __init__(self, code):
        super().__init__(code)
        self.code = code


MAX_DEPTH = 32  # containers nested deeper than this are refused (the root object is depth 1)


def _constant(name):
    raise Reject("not_json")


def _check(doc):
    """Refuse lone surrogates, non-finite numbers, and nesting deeper than MAX_DEPTH."""
    stack = [(doc, 1)]
    while stack:
        node, depth = stack.pop()
        if isinstance(node, str):
            try:
                node.encode("utf-8")
            except UnicodeEncodeError:
                raise Reject("not_json") from None
        elif isinstance(node, float):
            if not math.isfinite(node):
                raise Reject("not_json")
        elif isinstance(node, (dict, list)):
            if depth > MAX_DEPTH:
                raise Reject("not_json")
            if isinstance(node, dict):
                stack.extend((k, depth + 1) for k in node)
                stack.extend((v, depth + 1) for v in node.values())
            else:
                stack.extend((v, depth + 1) for v in node)


def parse(raw):
    """Strict JSON. `not_json` (syntax, BOM, NaN, lone surrogates, non-finite numbers, depth over
    MAX_DEPTH) takes precedence over `duplicate_key`, which is reported only for an otherwise valid
    document."""
    dup = []

    def pairs(items):
        out = {}
        for key, value in items:
            if key in out:
                dup.append(key)
            out[key] = value
        return out

    try:
        doc = json.loads(raw.decode("utf-8"), object_pairs_hook=pairs, parse_constant=_constant)
    except (UnicodeDecodeError, ValueError, RecursionError):
        raise Reject("not_json") from None
    _check(doc)
    if dup:
        raise Reject("duplicate_key")
    return doc
```
Keep the module docstring line `"""Strict JSON parsing shared by the manifest and index reference validators."""` at the top.

- [ ] **Step 4: Verify GREEN in Python and update the doc**

Run: `python3 tools/gen_manifest_vectors.py && bash tools/run_all.sh 2>&1 | tail -4`
Expected: all Python tests OK (Python tests for index and manifest both import `jsonutil`).

In `docs/manifest.md`, replace the sentence beginning "While parsing, the first problem met in document order wins; ..." with: "`not_json` always takes precedence over `duplicate_key`: a duplicate key is reported only when the document is otherwise valid JSON within these limits. Non-finite numbers such as `1e400` are `not_json`. Every implementation must use these exact limits so results match."

- [ ] **Step 5: Create the workspace and crate skeleton**

`Cargo.toml`:
```toml
[workspace]
resolver = "2"
members = ["crates/w2r-spec"]
```

`crates/w2r-spec/Cargo.toml`:
```toml
[package]
name = "w2r-spec"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
serde = "1"
serde_json = "1"
sha2 = "0.11"
```

`crates/w2r-spec/src/lib.rs`:
```rust
//! Parsers and validators for the w2rpm specs (plugin manifest, state file, fingerprint).
#![forbid(unsafe_code)]

pub mod json;
```

- [ ] **Step 6: Write the failing Rust test and helpers**

`crates/w2r-spec/tests/common/mod.rs`:
```rust
#![allow(dead_code)]
use serde_json::Value;
use std::path::PathBuf;

pub fn vector_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/vectors").join(name)
}

/// The top-level array of a vector file whose root is an array (manifest.json, state.json).
pub fn vectors(name: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(vector_path(name)).expect("vector file");
    match serde_json::from_str::<Value>(&text).expect("vector json") {
        Value::Array(v) => v,
        _ => panic!("{name}: root is not an array"),
    }
}

/// The bytes a vector case feeds to a parser: `raw` (+ `pad` spaces) or the serialized `manifest`/`state`.
pub fn raw_of(case: &Value) -> Vec<u8> {
    if let Some(raw) = case.get("raw").and_then(Value::as_str) {
        let mut bytes = raw.as_bytes().to_vec();
        bytes.extend(std::iter::repeat(b' ').take(case.get("pad").and_then(Value::as_u64).unwrap_or(0) as usize));
        return bytes;
    }
    for key in ["manifest", "state"] {
        if let Some(obj) = case.get(key) {
            return serde_json::to_vec(obj).unwrap();
        }
    }
    panic!("case has neither raw nor an object: {case}");
}

pub fn name(case: &Value) -> &str {
    case["name"].as_str().unwrap()
}

pub fn expect(case: &Value) -> &str {
    case["expect"].as_str().unwrap()
}
```

`crates/w2r-spec/tests/json_vectors.rs`:
```rust
mod common;
use common::{expect, name, raw_of, vectors};
use w2r_spec::json::{parse_json, Json, Reject};

#[test]
fn json_layer_matches_every_manifest_vector() {
    for case in vectors("manifest.json") {
        let want = expect(&case);
        if want == "too_large" {
            continue; // the size check precedes the JSON layer
        }
        let got = parse_json(&raw_of(&case));
        match want {
            "not_json" | "duplicate_key" => {
                assert_eq!(got.map(|_| ()), Err(Reject(if want == "not_json" { "not_json" } else { "duplicate_key" })), "{}", name(&case));
            }
            _ => assert!(got.is_ok(), "{}: JSON layer should accept, got {:?}", name(&case), got.err()),
        }
    }
}

#[test]
fn depth_32_is_accepted_and_33_is_refused() {
    let ok = format!("{}{}", "[".repeat(32), "]".repeat(32));
    let bad = format!("{}{}", "[".repeat(33), "]".repeat(33));
    assert!(parse_json(ok.as_bytes()).is_ok());
    assert_eq!(parse_json(bad.as_bytes()).err(), Some(Reject("not_json")));
}

#[test]
fn integers_and_floats_are_distinguished() {
    assert_eq!(parse_json(b"1").unwrap(), Json::Int(1));
    assert_eq!(parse_json(b"18446744073709551615").unwrap(), Json::Int(u64::MAX as i128));
    assert!(matches!(parse_json(b"1.0").unwrap(), Json::Float(_)));
    assert!(matches!(parse_json(b"100000000000000000000000").unwrap(), Json::Float(_)));
}

#[test]
fn object_entries_keep_document_order() {
    let v = parse_json(br#"{"b": 1, "a": 2}"#).unwrap();
    let Json::Obj(entries) = v else { panic!("not an object") };
    let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["b", "a"]);
}
```

- [ ] **Step 7: Run to verify it fails**

Run: `cargo test -p w2r-spec 2>&1 | tail -8`
Expected: compile error, `unresolved import w2r_spec::json::parse_json` (the module is empty or missing items).

- [ ] **Step 8: Implement the JSON layer**

`crates/w2r-spec/src/json.rs`:
```rust
//! Strict JSON for untrusted documents. Rules (docs/manifest.md): UTF-8 only, no BOM, no
//! NaN/Infinity, no lone surrogates, nesting at most `MAX_DEPTH` containers, duplicate keys refused.
//! `not_json` takes precedence over `duplicate_key`.
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use std::cell::Cell;
use std::collections::HashSet;
use std::fmt;

pub const MAX_DEPTH: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

/// A reason code from the specs (for example `"not_json"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reject(pub &'static str);

struct Seed<'a> {
    depth: usize,
    dup: &'a Cell<bool>,
}

impl<'de, 'a> DeserializeSeed<'de> for Seed<'a> {
    type Value = Json;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Json, D::Error> {
        d.deserialize_any(Walk { depth: self.depth, dup: self.dup })
    }
}

struct Walk<'a> {
    depth: usize,
    dup: &'a Cell<bool>,
}

impl<'de, 'a> Visitor<'de> for Walk<'a> {
    type Value = Json;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_unit<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }
    fn visit_bool<E>(self, v: bool) -> Result<Json, E> {
        Ok(Json::Bool(v))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Json, E> {
        Ok(Json::Int(v as i128))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Json, E> {
        Ok(Json::Int(v as i128))
    }
    fn visit_f64<E>(self, v: f64) -> Result<Json, E> {
        Ok(Json::Float(v))
    }
    fn visit_str<E>(self, v: &str) -> Result<Json, E> {
        Ok(Json::Str(v.to_owned()))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Json, A::Error> {
        let depth = self.depth + 1;
        if depth > MAX_DEPTH {
            return Err(de::Error::custom("too deep"));
        }
        let mut out = Vec::new();
        while let Some(v) = a.next_element_seed(Seed { depth, dup: self.dup })? {
            out.push(v);
        }
        Ok(Json::Arr(out))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Json, A::Error> {
        let depth = self.depth + 1;
        if depth > MAX_DEPTH {
            return Err(de::Error::custom("too deep"));
        }
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        while let Some(key) = a.next_key::<String>()? {
            let value = a.next_value_seed(Seed { depth, dup: self.dup })?;
            if seen.insert(key.clone()) {
                out.push((key, value));
            } else {
                self.dup.set(true);
            }
        }
        Ok(Json::Obj(out))
    }
}

pub fn parse_json(raw: &[u8]) -> Result<Json, Reject> {
    let not_json = Reject("not_json");
    let text = std::str::from_utf8(raw).map_err(|_| not_json)?;
    let dup = Cell::new(false);
    let mut de = serde_json::Deserializer::from_str(text);
    let value = Seed { depth: 0, dup: &dup }.deserialize(&mut de).map_err(|_| not_json)?;
    de.end().map_err(|_| not_json)?;
    if dup.get() {
        return Err(Reject("duplicate_key"));
    }
    Ok(value)
}
```

- [ ] **Step 9: Run to verify it passes**

Run: `cargo test -p w2r-spec 2>&1 | tail -12`
Expected: 4 tests PASS in `json_vectors`. If `json_layer_matches_every_manifest_vector` fails on a specific vector, decide whether the Rust layer or the vector is wrong (the Python oracle passed it in Step 4); fix the Rust layer unless the vector contradicts a stated rule.

- [ ] **Step 10: Commit**

```bash
git add .gitignore Cargo.toml Cargo.lock crates tools docs/manifest.md docs/vectors/manifest.json
git commit -m "Add w2r-spec JSON layer; align Python oracle precedence and non-finite numbers" -m "Claude-Session: https://claude.ai/code/session_015GrKhNbxr1N7QrzwX3YLE7"
```

---

### Task 2: Version types

**Files:**
- Create: `crates/w2r-spec/src/version.rs`, `crates/w2r-spec/tests/version.rs`
- Modify: `crates/w2r-spec/src/lib.rs` (add `pub mod version;`)

**Interfaces:**
- Produces: `version::SemVer { major: u32, minor: u32, patch: u32, pre: Vec<String> }` with `SemVer::parse(&str) -> Option<SemVer>` and `Ord`; `version::VersionReq` with `VersionReq::parse(&str) -> Option<VersionReq>` and `matches(&self, &SemVer) -> bool`; `version::Build(pub [u32; 4])` with `Build::parse(&str) -> Option<Build>`; `version::BuildSpec` (`Exact(Build) | Range(Build, Build)`) with `BuildSpec::parse(&str) -> Option<BuildSpec>` and `matches(&self, Build) -> bool`; `version::Capability` (`GameRead | ChatRead | ChatWrite | NetSend | HooksRaw`) with `Capability::parse(&str) -> Option<Capability>` and `as_str(self) -> &'static str`; `version::is_device_name_upper(&str) -> bool`.

- [ ] **Step 1: Write the failing tests**

`crates/w2r-spec/tests/version.rs`:
```rust
use w2r_spec::version::*;

fn v(s: &str) -> SemVer {
    SemVer::parse(s).unwrap_or_else(|| panic!("should parse: {s}"))
}

#[test]
fn semver_accepts_and_rejects_like_the_oracle() {
    for ok in ["0.0.1", "1.2.0", "2.0.0-beta.1", "4294967295.0.0", "1.0.0-a-b", "1.0.0-0", "1.0.0-x.0.y"] {
        assert!(SemVer::parse(ok).is_some(), "{ok}");
    }
    let long_ok = format!("1.0.0-{}", "a".repeat(58)); // 64 bytes
    let long_bad = format!("1.0.0-{}", "a".repeat(59)); // 65 bytes
    assert!(SemVer::parse(&long_ok).is_some());
    assert!(SemVer::parse(&long_bad).is_none());
    for bad in [
        "01.2.0", "1.2", "1.2.3.4", "1.2.0+abc", "1.2.0\n", "", "1.2.", "1..2", "1.2.3-", "1.2.3-01",
        "1.2.3-a..b", "1.2.4294967296", "-1.0.0", "1.0.0-é", "1.0.0 ",
    ] {
        assert!(SemVer::parse(bad).is_none(), "{bad:?}");
    }
}

#[test]
fn semver_precedence_follows_the_spec_example() {
    let order = [
        "1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta", "1.0.0-beta.2",
        "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0",
    ];
    for w in order.windows(2) {
        assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
    }
    assert!(v("1.9.0") < v("1.10.0"));
    assert!(v("1.0.0-99999999999") < v("1.0.0-100000000000")); // numeric identifiers compare numerically, any length
}

#[test]
fn requirements_parse_and_match() {
    for ok in [">=1.0.0", ">=1.0.0,<2.0.0", "=1.2.3", ">1.0.0,<=1.5.0,>0.1.0,<9.9.9"] {
        assert!(VersionReq::parse(ok).is_some(), "{ok}");
    }
    for bad in [
        "", "^1.0", ">=1.0.0\n", ">=01.0.0", ">=1.0.4294967296", ">= 1.0.0", ">=1.0.0, <2.0.0", "1.0.0",
        ">=1.0.0,<2.0.0,>3.0.0,<4.0.0,=5.0.0", ">=1.0.0-beta", ">=1.0",
    ] {
        assert!(VersionReq::parse(bad).is_none(), "{bad:?}");
    }
    let r = VersionReq::parse(">=1.0.0,<2.0.0").unwrap();
    assert!(r.matches(&v("1.0.0")));
    assert!(r.matches(&v("1.9.9")));
    assert!(!r.matches(&v("2.0.0")));
    assert!(!r.matches(&v("0.9.9")));
    assert!(r.matches(&v("2.0.0-beta.1")), "literal precedence: a 2.0.0 pre-release is below 2.0.0");
    assert!(VersionReq::parse("=1.2.3").unwrap().matches(&v("1.2.3")));
    assert!(!VersionReq::parse("=1.2.3").unwrap().matches(&v("1.2.3-rc.1")));
}

#[test]
fn builds_parse_and_match() {
    assert_eq!(Build::parse("1.0.2.2818"), Some(Build([1, 0, 2, 2818])));
    for bad in ["1.0.2", "1.0.2.2818.1", "1.0.2.x", "1.0.2.123456", "1.0.2.2818\n", "", "1..2.3"] {
        assert!(Build::parse(bad).is_none(), "{bad:?}");
    }
    let exact = BuildSpec::parse("1.0.2.2818").unwrap();
    assert!(exact.matches(Build([1, 0, 2, 2818])));
    assert!(!exact.matches(Build([1, 0, 2, 2819])));
    let range = BuildSpec::parse("1.0.2.2900-1.0.3.100").unwrap();
    assert!(range.matches(Build([1, 0, 2, 2900])));
    assert!(range.matches(Build([1, 0, 3, 100])));
    assert!(range.matches(Build([1, 0, 2, 5000])));
    assert!(!range.matches(Build([1, 0, 3, 101])));
    assert!(!range.matches(Build([1, 0, 2, 2899])));
    assert!(BuildSpec::parse("1.0.2.3000-1.0.2.2818").is_none(), "reversed range");
    assert!(BuildSpec::parse("1.0.2.1-1.0.2.2-1.0.2.3").is_none());
}

#[test]
fn capabilities_round_trip_and_implicit_ones_are_unknown() {
    for name in ["game.read", "chat.read", "chat.write", "net.send", "hooks.raw"] {
        assert_eq!(Capability::parse(name).unwrap().as_str(), name);
    }
    for name in ["fs.data", "log", "settings", "ui", "", "Game.read", "game.read "] {
        assert!(Capability::parse(name).is_none(), "{name:?}");
    }
}

#[test]
fn device_names() {
    for u in ["CON", "PRN", "AUX", "NUL", "COM1", "COM9", "LPT1", "LPT9"] {
        assert!(is_device_name_upper(u), "{u}");
    }
    for u in ["COM0", "COM10", "LPT0", "CONSOLE", "COM", ""] {
        assert!(!is_device_name_upper(u), "{u}");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Add `pub mod version;` to `lib.rs` and create an empty `src/version.rs`, then run `cargo test -p w2r-spec --test version 2>&1 | tail -6`.
Expected: compile errors, `cannot find type SemVer in this scope` and similar.

- [ ] **Step 3: Implement**

`crates/w2r-spec/src/version.rs`:
```rust
//! SemVer, version requirements, game builds, capabilities (docs/manifest.md).
use std::cmp::Ordering;

/// `0` or a number without a leading zero, at most 4,294,967,295.
fn num(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 10 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if s.len() > 1 && s.starts_with('0') {
        return None;
    }
    u32::try_from(s.parse::<u64>().ok()?).ok()
}

fn pre_identifier_ok(id: &str) -> bool {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return false;
    }
    if id.bytes().all(|b| b.is_ascii_digit()) {
        return id == "0" || !id.starts_with('0');
    }
    true
}

fn all_digits(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemVer {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub pre: Vec<String>,
}

impl SemVer {
    /// SemVer 2.0.0 without build metadata, at most 64 bytes, numeric parts at most `u32::MAX`.
    pub fn parse(s: &str) -> Option<SemVer> {
        if s.len() > 64 || !s.is_ascii() {
            return None;
        }
        let (core, pre) = match s.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (s, None),
        };
        let mut parts = core.split('.');
        let major = num(parts.next()?)?;
        let minor = num(parts.next()?)?;
        let patch = num(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(p) => {
                let ids: Vec<&str> = p.split('.').collect();
                if !ids.iter().all(|i| pre_identifier_ok(i)) {
                    return None;
                }
                ids.into_iter().map(str::to_owned).collect()
            }
        };
        Some(SemVer { major, minor, patch, pre })
    }
}

fn cmp_identifier(a: &str, b: &str) -> Ordering {
    match (all_digits(a), all_digits(b)) {
        (true, true) => a.len().cmp(&b.len()).then_with(|| a.cmp(b)),
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => a.cmp(b),
    }
}

impl Ord for SemVer {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => {
                    for (a, b) in self.pre.iter().zip(&other.pre) {
                        let o = cmp_identifier(a, b);
                        if o != Ordering::Equal {
                            return o;
                        }
                    }
                    self.pre.len().cmp(&other.pre.len())
                }
            })
    }
}

impl PartialOrd for SemVer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Ge,
    Le,
    Gt,
    Lt,
    Eq,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionReq {
    pub comparators: Vec<(Op, SemVer)>,
}

impl VersionReq {
    /// 1 to 4 comparators `(>=|<=|>|<|=)MAJOR.MINOR.PATCH` joined by `,`, no spaces.
    pub fn parse(s: &str) -> Option<VersionReq> {
        let items: Vec<&str> = s.split(',').collect();
        if items.is_empty() || items.len() > 4 {
            return None;
        }
        let mut comparators = Vec::new();
        for item in items {
            let (op, rest) = if let Some(r) = item.strip_prefix(">=") {
                (Op::Ge, r)
            } else if let Some(r) = item.strip_prefix("<=") {
                (Op::Le, r)
            } else if let Some(r) = item.strip_prefix('>') {
                (Op::Gt, r)
            } else if let Some(r) = item.strip_prefix('<') {
                (Op::Lt, r)
            } else if let Some(r) = item.strip_prefix('=') {
                (Op::Eq, r)
            } else {
                return None;
            };
            let mut p = rest.split('.');
            let (major, minor, patch) = (num(p.next()?)?, num(p.next()?)?, num(p.next()?)?);
            if p.next().is_some() {
                return None;
            }
            comparators.push((op, SemVer { major, minor, patch, pre: Vec::new() }));
        }
        Some(VersionReq { comparators })
    }

    pub fn matches(&self, v: &SemVer) -> bool {
        self.comparators.iter().all(|(op, c)| match op {
            Op::Ge => v >= c,
            Op::Le => v <= c,
            Op::Gt => v > c,
            Op::Lt => v < c,
            Op::Eq => v == c,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Build(pub [u32; 4]);

impl Build {
    /// Four dot-separated numbers of 1 to 5 digits.
    pub fn parse(s: &str) -> Option<Build> {
        let mut out = [0u32; 4];
        let mut parts = s.split('.');
        for slot in out.iter_mut() {
            let p = parts.next()?;
            if p.is_empty() || p.len() > 5 || !all_digits(p) {
                return None;
            }
            *slot = p.parse().ok()?;
        }
        if parts.next().is_some() {
            return None;
        }
        Some(Build(out))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildSpec {
    Exact(Build),
    Range(Build, Build),
}

impl BuildSpec {
    pub fn parse(s: &str) -> Option<BuildSpec> {
        match s.split_once('-') {
            None => Build::parse(s).map(BuildSpec::Exact),
            Some((a, b)) => {
                let (a, b) = (Build::parse(a)?, Build::parse(b)?);
                (a <= b).then_some(BuildSpec::Range(a, b))
            }
        }
    }

    pub fn matches(&self, build: Build) -> bool {
        match *self {
            BuildSpec::Exact(b) => b == build,
            BuildSpec::Range(lo, hi) => lo <= build && build <= hi,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    GameRead,
    ChatRead,
    ChatWrite,
    NetSend,
    HooksRaw,
}

impl Capability {
    pub fn parse(s: &str) -> Option<Capability> {
        Some(match s {
            "game.read" => Capability::GameRead,
            "chat.read" => Capability::ChatRead,
            "chat.write" => Capability::ChatWrite,
            "net.send" => Capability::NetSend,
            "hooks.raw" => Capability::HooksRaw,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Capability::GameRead => "game.read",
            Capability::ChatRead => "chat.read",
            Capability::ChatWrite => "chat.write",
            Capability::NetSend => "net.send",
            Capability::HooksRaw => "hooks.raw",
        }
    }
}

/// Windows device names (already upper-cased by the caller): CON PRN AUX NUL COM1-9 LPT1-9.
pub fn is_device_name_upper(u: &str) -> bool {
    matches!(u, "CON" | "PRN" | "AUX" | "NUL")
        || ((u.starts_with("COM") || u.starts_with("LPT")) && u.len() == 4 && matches!(u.as_bytes()[3], b'1'..=b'9'))
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p w2r-spec --test version 2>&1 | tail -10`
Expected: 6 tests PASS.

- [ ] **Step 5: Document the matching ruling and commit**

In `docs/load-policy.md`, in the "Order" bullet list under "Details", add a bullet: "- **Requirement matching.** `version_req` is evaluated with full SemVer 2.0.0 precedence, pre-release identifiers included: `2.0.0-beta.1` is lower than `2.0.0`, so it satisfies `<2.0.0` and not `>=2.0.0`. Comparators carry no pre-release. `=1.2.3` matches only exactly `1.2.3`."

```bash
git add crates docs/load-policy.md
git commit -m "Add w2r-spec version, build and capability types" -m "Claude-Session: https://claude.ai/code/session_015GrKhNbxr1N7QrzwX3YLE7"
```

---

### Task 3: Manifest parser

**Files:**
- Create: `crates/w2r-spec/src/manifest.rs`, `crates/w2r-spec/tests/manifest_vectors.rs`
- Modify: `crates/w2r-spec/src/lib.rs` (add `pub mod manifest;`)

**Interfaces:**
- Consumes: `json::{Json, Reject, parse_json}`, `version::{SemVer, VersionReq, BuildSpec, Capability, is_device_name_upper}` (Tasks 1, 2); `tests/common` helpers.
- Produces: `manifest::{Manifest, Dependency, parse, valid_id, MAX_BYTES}`. `parse(raw: &[u8]) -> Result<Manifest, Reject>`; `valid_id(&str) -> bool` (also used by `state`); `Manifest { id: String, name: String, version: SemVer, author: String, description: String, entry: String, abi: u32, game_builds: Vec<BuildSpec>, capabilities: Vec<Capability>, affects_simulation: bool, dependencies: Vec<Dependency> }`; `Dependency { id: String, req: VersionReq }`.

- [ ] **Step 1: Write the failing tests**

`crates/w2r-spec/tests/manifest_vectors.rs`:
```rust
mod common;
use common::{expect, name, raw_of, vectors};
use w2r_spec::json::Reject;
use w2r_spec::manifest;
use w2r_spec::version::Capability;

#[test]
fn every_manifest_vector_matches() {
    let cases = vectors("manifest.json");
    assert!(cases.len() > 80, "vector file looks truncated: {}", cases.len());
    for case in cases {
        let got = manifest::parse(&raw_of(&case));
        let want = expect(&case);
        if want == "ok" {
            assert!(got.is_ok(), "{}: expected ok, got {:?}", name(&case), got.err());
        } else {
            assert_eq!(got.map(|_| ()), Err(Reject(leak(want))), "{}", name(&case));
        }
    }
}

fn leak(s: &str) -> &'static str {
    // Reject carries &'static str; vectors are read at runtime, so intern through a leak (test only).
    Box::leak(s.to_owned().into_boxed_str())
}

#[test]
fn valid_example_fields_are_extracted() {
    let case = vectors("manifest.json").into_iter().find(|c| name(c) == "valid_full").unwrap();
    let m = manifest::parse(&raw_of(&case)).unwrap();
    assert_eq!(m.id, "example-plugin");
    assert_eq!(m.version.pre, vec!["beta".to_string(), "1".to_string()]);
    assert_eq!(m.abi, 1);
    assert_eq!(m.game_builds.len(), 2);
    assert_eq!(m.capabilities.len(), 5);
    assert!(m.capabilities.contains(&Capability::HooksRaw));
    assert!(m.affects_simulation);
    assert_eq!(m.dependencies.len(), 1);
    assert_eq!(m.dependencies[0].id, "other-plugin");
}

#[test]
fn id_rule() {
    for ok in ["abc", "a-b", "ab0", "example-plugin", &"a".repeat(64)] {
        assert!(manifest::valid_id(ok), "{ok}");
    }
    for bad in ["ab", "-ab", "ab-", "Abc", "a b", "abc\n", "con", "nul", "com1", "lpt9", &"a".repeat(65), "../x", "é-é"] {
        assert!(!manifest::valid_id(bad), "{bad:?}");
    }
}

/// A parser fed hostile input must return, never panic: truncate every vector at every byte.
#[test]
fn never_panics_on_truncated_or_corrupted_input() {
    for case in vectors("manifest.json") {
        let raw = raw_of(&case);
        for cut in 0..raw.len().min(2000) {
            let _ = manifest::parse(&raw[..cut]);
        }
        let mut flipped = raw.clone();
        for i in (0..flipped.len().min(2000)).step_by(7) {
            flipped[i] ^= 0x55;
            let _ = manifest::parse(&flipped);
            flipped[i] ^= 0x55;
        }
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Add `pub mod manifest;` to `lib.rs`, create an empty `src/manifest.rs`, then run `cargo test -p w2r-spec --test manifest_vectors 2>&1 | tail -6`.
Expected: compile error, `cannot find function parse in module manifest`.

- [ ] **Step 3: Implement**

`crates/w2r-spec/src/manifest.rs`:
```rust
//! `plugin.json` (docs/manifest.md). The first failing check in the documented order wins.
use crate::json::{parse_json, Json, Reject};
use crate::version::{is_device_name_upper, BuildSpec, Capability, SemVer, VersionReq};
use std::collections::HashSet;

pub const MAX_BYTES: usize = 64 * 1024;

const FIELDS: [&str; 12] = [
    "schema", "id", "name", "version", "author", "description", "entry", "abi", "game_builds",
    "capabilities", "affects_simulation", "dependencies",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    pub id: String,
    pub req: VersionReq,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: SemVer,
    pub author: String,
    pub description: String,
    pub entry: String,
    pub abi: u32,
    pub game_builds: Vec<BuildSpec>,
    pub capabilities: Vec<Capability>,
    pub affects_simulation: bool,
    pub dependencies: Vec<Dependency>,
}

/// `[a-z0-9][a-z0-9-]{1,62}[a-z0-9]` and not a Windows device name.
pub fn valid_id(s: &str) -> bool {
    let b = s.as_bytes();
    if !(3..=64).contains(&b.len()) {
        return false;
    }
    let edge = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    if !edge(b[0]) || !edge(b[b.len() - 1]) {
        return false;
    }
    if !b[1..b.len() - 1].iter().all(|&c| edge(c) || c == b'-') {
        return false;
    }
    !is_device_name_upper(&s.to_ascii_uppercase())
}

fn entry_ok(e: &str) -> bool {
    let Some(stem) = e.strip_suffix(".dll") else { return false };
    if e.contains("..") {
        return false;
    }
    let b = stem.as_bytes();
    if b.is_empty() || b.len() > 60 {
        return false;
    }
    let first = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let rest = |c: u8| first(c) || c == b'.' || c == b'-';
    if !first(b[0]) || !b[1..].iter().all(|&c| rest(c)) {
        return false;
    }
    !is_device_name_upper(&e.split('.').next().unwrap_or("").to_ascii_uppercase())
}

fn text(v: &Json, max_bytes: usize) -> Result<String, Reject> {
    let Json::Str(s) = v else { return Err(Reject("bad_type")) };
    if s.is_empty() || s.chars().any(|c| (c as u32) < 0x20 || c as u32 == 0x7F) {
        return Err(Reject("bad_string"));
    }
    if s.len() > max_bytes {
        return Err(Reject("too_long"));
    }
    Ok(s.clone())
}

fn field<'a>(obj: &'a [(String, Json)], key: &str) -> &'a Json {
    &obj.iter().find(|(k, _)| k == key).expect("field presence was checked").1
}

fn builds(v: &Json) -> Result<Vec<BuildSpec>, Reject> {
    let Json::Arr(items) = v else { return Err(Reject("bad_type")) };
    if !(1..=16).contains(&items.len()) {
        return Err(Reject("bad_build"));
    }
    items
        .iter()
        .map(|i| match i {
            Json::Str(s) => BuildSpec::parse(s).ok_or(Reject("bad_build")),
            _ => Err(Reject("bad_build")),
        })
        .collect()
}

fn capabilities(v: &Json) -> Result<Vec<Capability>, Reject> {
    let Json::Arr(items) = v else { return Err(Reject("bad_type")) };
    let mut out = Vec::new();
    for i in items {
        let Json::Str(s) = i else { return Err(Reject("bad_capability")) };
        let c = Capability::parse(s).ok_or(Reject("bad_capability"))?;
        if out.contains(&c) {
            return Err(Reject("bad_capability"));
        }
        out.push(c);
    }
    Ok(out)
}

fn dependencies(v: &Json, own_id: &str) -> Result<Vec<Dependency>, Reject> {
    let bad = Reject("bad_dependency");
    let Json::Arr(items) = v else { return Err(Reject("bad_type")) };
    if items.len() > 16 {
        return Err(bad);
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in items {
        let Json::Obj(entries) = item else { return Err(bad) };
        let keys: HashSet<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        if keys != HashSet::from(["id", "version_req"]) {
            return Err(bad);
        }
        let (Json::Str(id), Json::Str(req)) = (field(entries, "id"), field(entries, "version_req")) else {
            return Err(bad);
        };
        if !valid_id(id) || id == own_id || !seen.insert(id.clone()) {
            return Err(bad);
        }
        out.push(Dependency { id: id.clone(), req: VersionReq::parse(req).ok_or(bad)? });
    }
    Ok(out)
}

pub fn parse(raw: &[u8]) -> Result<Manifest, Reject> {
    if raw.len() > MAX_BYTES {
        return Err(Reject("too_large"));
    }
    let Json::Obj(obj) = parse_json(raw)? else { return Err(Reject("not_object")) };
    if obj.iter().any(|(k, _)| !FIELDS.contains(&k.as_str())) {
        return Err(Reject("unknown_field"));
    }
    if FIELDS.iter().any(|f| !obj.iter().any(|(k, _)| k == f)) {
        return Err(Reject("missing_field"));
    }
    if *field(&obj, "schema") != Json::Int(1) {
        return Err(Reject("bad_schema"));
    }
    let id = match field(&obj, "id") {
        Json::Str(s) if valid_id(s) => s.clone(),
        _ => return Err(Reject("bad_id")),
    };
    let name = text(field(&obj, "name"), 64)?;
    let author = text(field(&obj, "author"), 64)?;
    let description = text(field(&obj, "description"), 256)?;
    let version = match field(&obj, "version") {
        Json::Str(s) => SemVer::parse(s).ok_or(Reject("bad_version"))?,
        _ => return Err(Reject("bad_version")),
    };
    let entry = match field(&obj, "entry") {
        Json::Str(s) if entry_ok(s) => s.clone(),
        _ => return Err(Reject("bad_entry")),
    };
    let abi = match field(&obj, "abi") {
        Json::Int(n) if (1..=u32::MAX as i128).contains(n) => *n as u32,
        _ => return Err(Reject("bad_abi")),
    };
    let game_builds = builds(field(&obj, "game_builds"))?;
    let capabilities = capabilities(field(&obj, "capabilities"))?;
    let Json::Bool(affects_simulation) = *field(&obj, "affects_simulation") else {
        return Err(Reject("bad_type"));
    };
    let dependencies = dependencies(field(&obj, "dependencies"), &id)?;
    Ok(Manifest {
        id, name, version, author, description, entry, abi, game_builds, capabilities,
        affects_simulation, dependencies,
    })
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p w2r-spec 2>&1 | tail -15`
Expected: all tests PASS across `json_vectors`, `version`, `manifest_vectors`. For a failing vector, compare with `python3 -c` on the same bytes using `tools/w2r_spec/manifest.py` to see which implementation deviates from `docs/manifest.md`, and fix the deviating one (Python first if the doc and vector agree with Rust).

- [ ] **Step 5: Commit**

```bash
git add crates
git commit -m "Add w2r-spec manifest parser passing all manifest vectors" -m "Claude-Session: https://claude.ai/code/session_015GrKhNbxr1N7QrzwX3YLE7"
```

---

### Task 4: `state.json` spec, oracle, vectors, Rust parser

**Files:**
- Create: `docs/state.md`, `tools/w2r_spec/state.py`, `tools/gen_state_vectors.py`, `tools/tests/test_state.py`, `docs/vectors/state.json` (generated)
- Create: `crates/w2r-spec/src/state.rs`, `crates/w2r-spec/tests/state_vectors.rs`
- Modify: `crates/w2r-spec/src/lib.rs` (add `pub mod state;`)

**Interfaces:**
- Consumes: `manifest::valid_id`, `version::{SemVer, Capability}`, `json` (Task 1-3); Python `manifest.valid_id`, `manifest.valid_semver`, `manifest.CAPABILITIES`, `jsonutil`.
- Produces (Rust): `state::{State, PluginState, parse, MAX_BYTES, MAX_PLUGINS}`; `parse(raw: &[u8]) -> Result<State, Reject>`; `State { plugins: Vec<(String, PluginState)> }` in document order; `PluginState { version: SemVer, enabled: bool, sha256: [u8; 32], approved_capabilities: Vec<Capability> }`.
- Produces (Python): `state.validate_state(raw: bytes) -> str`, `state.REASONS`.

- [ ] **Step 1: Write the Python vector generator and failing test**

`tools/gen_state_vectors.py`:
```python
"""Regenerate docs/vectors/state.json. Run: python3 tools/gen_state_vectors.py"""
import json
from pathlib import Path

OUT = Path(__file__).resolve().parents[1] / "docs" / "vectors" / "state.json"
HASH = "ab" * 32


def plugin(**changes):
    p = {"version": "1.2.0", "enabled": True, "sha256": HASH,
         "approved_capabilities": ["game.read", "chat.read"]}
    p.update(changes)
    return p


def state(plugins=None, **changes):
    s = {"schema": 1, "plugins": {"example-plugin": plugin()} if plugins is None else plugins}
    s.update(changes)
    return s


def without(key):
    s = state()
    del s[key]
    return s


def without_plugin_key(key):
    p = plugin()
    del p[key]
    return state({"example-plugin": p})


OBJECT_CASES = [
    ("valid_empty", "ok", state({})),
    ("valid_example", "ok", state()),
    ("valid_two_plugins_disabled_second", "ok", state({
        "example-plugin": plugin(),
        "other-plugin": plugin(enabled=False, approved_capabilities=[], version="0.0.1-rc.1")})),
    ("valid_all_capabilities", "ok", state({"example-plugin": plugin(
        approved_capabilities=["game.read", "chat.read", "chat.write", "net.send", "hooks.raw"])})),
    ("schema_2", "bad_schema", state(schema=2)),
    ("schema_string", "bad_schema", state(schema="1")),
    ("unknown_top_field", "unknown_field", state(extra=1)),
    ("missing_plugins", "missing_field", without("plugins")),
    ("missing_schema", "missing_field", without("schema")),
    ("plugins_is_list", "bad_type", state([])),
    ("too_many_plugins", "too_many", state({"plug%04d" % i: {} for i in range(1001)})),
    ("max_plugins_is_ok_in_count_but_entries_checked", "bad_type", state({"plug%04d" % i: [] for i in range(1000)})),
    ("key_uppercase", "bad_id", state({"Example": plugin()})),
    ("key_device_name", "bad_id", state({"con": plugin()})),
    ("key_trailing_newline", "bad_id", state({"example-plugin\n": plugin()})),
    ("key_too_short", "bad_id", state({"ab": plugin()})),
    ("entry_not_object", "bad_type", state({"example-plugin": "x"})),
    ("entry_unknown_field", "unknown_field", state({"example-plugin": plugin(extra=1)})),
    ("entry_missing_version", "missing_field", without_plugin_key("version")),
    ("entry_missing_sha256", "missing_field", without_plugin_key("sha256")),
    ("version_bad", "bad_version", state({"example-plugin": plugin(version="1.2")})),
    ("version_trailing_newline", "bad_version", state({"example-plugin": plugin(version="1.2.0\n")})),
    ("enabled_not_bool", "bad_type", state({"example-plugin": plugin(enabled=1)})),
    ("sha256_uppercase", "bad_hash", state({"example-plugin": plugin(sha256="AB" * 32)})),
    ("sha256_short", "bad_hash", state({"example-plugin": plugin(sha256="ab" * 31)})),
    ("sha256_trailing_newline", "bad_hash", state({"example-plugin": plugin(sha256=HASH + "\n")})),
    ("sha256_not_string", "bad_hash", state({"example-plugin": plugin(sha256=5)})),
    ("capability_unknown", "bad_capability", state({"example-plugin": plugin(approved_capabilities=["net.recv"])})),
    ("capability_implicit_refused", "bad_capability", state({"example-plugin": plugin(approved_capabilities=["log"])})),
    ("capability_duplicate", "bad_capability", state({"example-plugin": plugin(approved_capabilities=["game.read", "game.read"])})),
    ("capabilities_not_list", "bad_type", state({"example-plugin": plugin(approved_capabilities="game.read")})),
]

RAW_CASES = [
    ("not_json", "not_json", "{", 0),
    ("not_object", "not_object", "[]", 0),
    ("bom", "not_json", "﻿" + json.dumps(state()), 0),
    ("duplicate_plugin_key", "duplicate_key",
     '{"schema": 1, "plugins": {"example-plugin": {}, "example-plugin": {}}}', 0),
    ("lone_surrogate", "not_json", '{"schema": 1, "plugins": {}, "x": "\\ud800"}', 0),
    ("oversize", "too_large", json.dumps(state({})), 1048576),
]


def main():
    cases = [{"name": n, "expect": e, "state": s} for n, e, s in OBJECT_CASES]
    for name, expect, raw, pad in RAW_CASES:
        case = {"name": name, "expect": expect, "raw": raw}
        if pad:
            case["pad"] = pad
        cases.append(case)
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(json.dumps(cases, indent=1, sort_keys=True) + "\n", encoding="utf-8")
    print("wrote", OUT, len(cases), "cases")


if __name__ == "__main__":
    main()
```

`tools/tests/test_state.py`:
```python
import json
import unittest
from pathlib import Path

from w2r_spec import state

VECTORS = Path(__file__).resolve().parents[2] / "docs" / "vectors" / "state.json"


def raw_of(case):
    if "raw" in case:
        return case["raw"].encode("utf-8") + b" " * case.get("pad", 0)
    return json.dumps(case["state"]).encode("utf-8")


class StateVectors(unittest.TestCase):
    cases = json.loads(VECTORS.read_text(encoding="utf-8")) if VECTORS.exists() else []

    def test_every_vector(self):
        self.assertGreater(len(self.cases), 30)
        for case in self.cases:
            with self.subTest(case["name"]):
                self.assertEqual(state.validate_state(raw_of(case)), case["expect"])

    def test_names_unique(self):
        names = [c["name"] for c in self.cases]
        self.assertEqual(len(names), len(set(names)))

    def test_every_reason_code_has_a_vector(self):
        self.assertEqual(state.REASONS - {c["expect"] for c in self.cases}, set())


if __name__ == "__main__":
    unittest.main()
```

Run: `python3 tools/gen_state_vectors.py && python3 -m unittest discover -s tools/tests -t tools 2>&1 | grep -E "Error|Ran|FAILED|OK" | head -4`
Expected: ImportError, `cannot import name 'state' from 'w2r_spec'`.

- [ ] **Step 2: Implement the Python oracle**

`tools/w2r_spec/state.py`:
```python
"""Reference validator for state.json (docs/state.md). Returns "ok" or a reason code."""
import re

from . import jsonutil
from .manifest import CAPABILITIES, valid_id, valid_semver

MAX_BYTES = 1024 * 1024
MAX_PLUGINS = 1000
TOP = {"schema", "plugins"}
ENTRY = {"version", "enabled", "sha256", "approved_capabilities"}
SHA256_RE = re.compile(r"[0-9a-f]{64}")

REASONS = {
    "too_large", "not_json", "duplicate_key", "not_object", "unknown_field", "missing_field",
    "bad_schema", "bad_type", "too_many", "bad_id", "bad_version", "bad_hash", "bad_capability",
}


def _is_int(v):
    return isinstance(v, int) and not isinstance(v, bool)


def _check_entry(entry):
    if not isinstance(entry, dict):
        return "bad_type"
    if set(entry) - ENTRY:
        return "unknown_field"
    if ENTRY - set(entry):
        return "missing_field"
    if not valid_semver(entry["version"]):
        return "bad_version"
    if not isinstance(entry["enabled"], bool):
        return "bad_type"
    sha = entry["sha256"]
    if not isinstance(sha, str) or not SHA256_RE.fullmatch(sha):
        return "bad_hash"
    caps = entry["approved_capabilities"]
    if not isinstance(caps, list):
        return "bad_type"
    if not all(isinstance(c, str) and c in CAPABILITIES for c in caps) or len(set(caps)) != len(caps):
        return "bad_capability"
    return None


def validate_state(raw):
    if len(raw) > MAX_BYTES:
        return "too_large"
    try:
        doc = jsonutil.parse(raw)
    except jsonutil.Reject as r:
        return r.code
    if not isinstance(doc, dict):
        return "not_object"
    if set(doc) - TOP:
        return "unknown_field"
    if TOP - set(doc):
        return "missing_field"
    if not _is_int(doc["schema"]) or doc["schema"] != 1:
        return "bad_schema"
    plugins = doc["plugins"]
    if not isinstance(plugins, dict):
        return "bad_type"
    if len(plugins) > MAX_PLUGINS:
        return "too_many"
    for pid, entry in plugins.items():
        if not valid_id(pid):
            return "bad_id"
        err = _check_entry(entry)
        if err:
            return err
    return "ok"
```

- [ ] **Step 3: Verify the Python side is GREEN**

Run: `python3 -m unittest discover -s tools/tests -t tools 2>&1 | tail -4`
Expected: OK, including `StateVectors`. (The first vector run happened in Step 1 before the module existed, which was the RED.)

- [ ] **Step 4: Write `docs/state.md`**

Required content (normative; must match `state.py`):
- Purpose: the installed-plugin record read by the host at startup (`config\state.json`, see `layout.md`); the manager writes it; in 2d the in-game screen may flip `enabled` only. The host in 2a only reads it and never repairs a bad file: a bad file means no plugin loads and the reason code is logged.
- The example from `layout.md`, and a field table: top level exactly `schema` (integer 1) and `plugins` (object keyed by plugin id); each entry exactly `version` (SemVer, manifest limits), `enabled` (boolean), `sha256` (64 lowercase hex), `approved_capabilities` (duplicate-free subset of the five approval-requiring capabilities).
- Limits: at most 1,048,576 bytes; at most 1,000 plugins; keys follow the manifest id rule (device names refused); JSON strictness as in `manifest.md` (not_json beats duplicate_key; a repeated plugin id is `duplicate_key`).
- Reason-code table (13 codes) and the check order: size, JSON, top-level type, field set, `schema`, `plugins` type, count (`too_many`), then each plugin in document order: key id, entry type, entry field set, `version`, `enabled`, `sha256`, `approved_capabilities`.
- What the pin does and does not protect (copy the last section of `layout.md` in substance).
- Writers: temp file then atomic rename; this spec defines reading only.
- Vectors: `docs/vectors/state.json`, regenerate with `python3 tools/gen_state_vectors.py`.

- [ ] **Step 5: Write the failing Rust test**

`crates/w2r-spec/tests/state_vectors.rs`:
```rust
mod common;
use common::{expect, name, raw_of, vectors};
use w2r_spec::json::Reject;
use w2r_spec::state;
use w2r_spec::version::Capability;

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_owned().into_boxed_str())
}

#[test]
fn every_state_vector_matches() {
    let cases = vectors("state.json");
    assert!(cases.len() > 30, "vector file looks truncated: {}", cases.len());
    for case in cases {
        let got = state::parse(&raw_of(&case));
        let want = expect(&case);
        if want == "ok" {
            assert!(got.is_ok(), "{}: expected ok, got {:?}", name(&case), got.err());
        } else {
            assert_eq!(got.map(|_| ()), Err(Reject(leak(want))), "{}", name(&case));
        }
    }
}

#[test]
fn fields_are_extracted_in_document_order() {
    let case = vectors("state.json").into_iter().find(|c| name(c) == "valid_two_plugins_disabled_second").unwrap();
    // serde_json sorts keys of a Value, so build the raw text by hand to control the order.
    let raw = br#"{"schema":1,"plugins":{"zeta-plugin":{"version":"1.0.0","enabled":true,"sha256":"abababababababababababababababababababababababababababababababab","approved_capabilities":["net.send"]},"alpha-plugin":{"version":"0.0.1-rc.1","enabled":false,"sha256":"cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd","approved_capabilities":[]}}}"#;
    let _ = case;
    let s = state::parse(raw).unwrap();
    let ids: Vec<&str> = s.plugins.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["zeta-plugin", "alpha-plugin"]);
    assert!(s.plugins[0].1.enabled);
    assert!(!s.plugins[1].1.enabled);
    assert_eq!(s.plugins[0].1.sha256[0], 0xab);
    assert_eq!(s.plugins[0].1.approved_capabilities, vec![Capability::NetSend]);
}

#[test]
fn never_panics_on_truncated_or_corrupted_input() {
    for case in vectors("state.json") {
        let raw = raw_of(&case);
        for cut in 0..raw.len().min(3000) {
            let _ = state::parse(&raw[..cut]);
        }
    }
}
```

- [ ] **Step 6: Run to verify it fails**

Add `pub mod state;` to `lib.rs`, create an empty `src/state.rs`, run `cargo test -p w2r-spec --test state_vectors 2>&1 | tail -5`.
Expected: compile error, `cannot find function parse in module state`.

- [ ] **Step 7: Implement**

`crates/w2r-spec/src/state.rs`:
```rust
//! `state.json` (docs/state.md): the installed-plugin record read by the host at startup.
use crate::json::{parse_json, Json, Reject};
use crate::manifest::valid_id;
use crate::version::{Capability, SemVer};

pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_PLUGINS: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginState {
    pub version: SemVer,
    pub enabled: bool,
    pub sha256: [u8; 32],
    pub approved_capabilities: Vec<Capability>,
}

/// Plugins in document order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub plugins: Vec<(String, PluginState)>,
}

fn field<'a>(obj: &'a [(String, Json)], key: &str) -> &'a Json {
    &obj.iter().find(|(k, _)| k == key).expect("field presence was checked").1
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    for (i, pair) in b.chunks(2).enumerate() {
        out[i] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Some(out)
}

fn entry(v: &Json) -> Result<PluginState, Reject> {
    let Json::Obj(obj) = v else { return Err(Reject("bad_type")) };
    const KEYS: [&str; 4] = ["version", "enabled", "sha256", "approved_capabilities"];
    if obj.iter().any(|(k, _)| !KEYS.contains(&k.as_str())) {
        return Err(Reject("unknown_field"));
    }
    if KEYS.iter().any(|k| !obj.iter().any(|(ek, _)| ek == k)) {
        return Err(Reject("missing_field"));
    }
    let version = match field(obj, "version") {
        Json::Str(s) => SemVer::parse(s).ok_or(Reject("bad_version"))?,
        _ => return Err(Reject("bad_version")),
    };
    let Json::Bool(enabled) = *field(obj, "enabled") else { return Err(Reject("bad_type")) };
    let sha256 = match field(obj, "sha256") {
        Json::Str(s) => hex32(s).ok_or(Reject("bad_hash"))?,
        _ => return Err(Reject("bad_hash")),
    };
    let Json::Arr(caps) = field(obj, "approved_capabilities") else { return Err(Reject("bad_type")) };
    let mut approved = Vec::new();
    for c in caps {
        let Json::Str(s) = c else { return Err(Reject("bad_capability")) };
        let cap = Capability::parse(s).ok_or(Reject("bad_capability"))?;
        if approved.contains(&cap) {
            return Err(Reject("bad_capability"));
        }
        approved.push(cap);
    }
    Ok(PluginState { version, enabled, sha256, approved_capabilities: approved })
}

pub fn parse(raw: &[u8]) -> Result<State, Reject> {
    if raw.len() > MAX_BYTES {
        return Err(Reject("too_large"));
    }
    let Json::Obj(top) = parse_json(raw)? else { return Err(Reject("not_object")) };
    if top.iter().any(|(k, _)| k != "schema" && k != "plugins") {
        return Err(Reject("unknown_field"));
    }
    if !top.iter().any(|(k, _)| k == "schema") || !top.iter().any(|(k, _)| k == "plugins") {
        return Err(Reject("missing_field"));
    }
    if *field(&top, "schema") != Json::Int(1) {
        return Err(Reject("bad_schema"));
    }
    let Json::Obj(plugins) = field(&top, "plugins") else { return Err(Reject("bad_type")) };
    if plugins.len() > MAX_PLUGINS {
        return Err(Reject("too_many"));
    }
    let mut out = Vec::with_capacity(plugins.len());
    for (id, v) in plugins {
        if !valid_id(id) {
            return Err(Reject("bad_id"));
        }
        out.push((id.clone(), entry(v)?));
    }
    Ok(State { plugins: out })
}
```

- [ ] **Step 8: Run to verify it passes**

Run: `cargo test -p w2r-spec 2>&1 | tail -15 && bash tools/run_all.sh 2>&1 | tail -3`
Expected: all Rust tests PASS; Python tests OK.

- [ ] **Step 9: Commit**

```bash
git add docs/state.md docs/vectors/state.json tools crates
git commit -m "Add state.json spec, oracle, vectors and Rust parser" -m "Claude-Session: https://claude.ai/code/session_015GrKhNbxr1N7QrzwX3YLE7"
```

---

### Task 5: Fingerprint, peer tag, fuzz targets, wiring

**Files:**
- Create: `crates/w2r-spec/src/fingerprint.rs`, `crates/w2r-spec/tests/fingerprint_vectors.rs`
- Create: `fuzz/Cargo.toml`, `fuzz/fuzz_targets/manifest_parse.rs`, `fuzz/fuzz_targets/state_parse.rs`, `fuzz/fuzz_targets/json_parse.rs`
- Modify: `crates/w2r-spec/src/lib.rs`, `tools/run_all.sh`, `README.md`, `docs/PLAN.md`

**Interfaces:**
- Consumes: `docs/vectors/fingerprint.json` (Phase 1); `sha2`.
- Produces: `fingerprint::{SimPlugin, DuplicateId, fingerprint, peer_tag}`: `SimPlugin { id: String, version: String, sha256: [u8; 32] }`; `fingerprint(&[SimPlugin]) -> Result<[u8; 4], DuplicateId>`; `peer_tag(id: &str) -> [u8; 4]`.

- [ ] **Step 1: Write the failing test**

`crates/w2r-spec/tests/fingerprint_vectors.rs`:
```rust
use serde_json::Value;
use w2r_spec::fingerprint::{fingerprint, peer_tag, DuplicateId, SimPlugin};

fn doc() -> Value {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/vectors/fingerprint.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex32(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

fn plugins(v: &Value) -> Vec<SimPlugin> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|p| SimPlugin {
            id: p["id"].as_str().unwrap().to_owned(),
            version: p["version"].as_str().unwrap().to_owned(),
            sha256: unhex32(p["sha256"].as_str().unwrap()),
        })
        .collect()
}

#[test]
fn fingerprints_match_the_vectors() {
    let d = doc();
    for case in d["fingerprints"].as_array().unwrap() {
        let got = fingerprint(&plugins(&case["plugins"])).unwrap();
        assert_eq!(hex(&got), case["expect"].as_str().unwrap(), "{}", case["name"]);
    }
}

#[test]
fn duplicate_ids_are_refused() {
    let d = doc();
    assert_eq!(fingerprint(&plugins(&d["duplicate_ids_refused"])), Err(DuplicateId));
}

#[test]
fn peer_tags_match_the_vectors() {
    let d = doc();
    for case in d["tags"].as_array().unwrap() {
        assert_eq!(hex(&peer_tag(case["id"].as_str().unwrap())), case["expect"].as_str().unwrap());
    }
}

#[test]
fn sorting_is_bytewise_not_locale_or_case_folded() {
    let mk = |id: &str| SimPlugin { id: id.into(), version: "1.0.0".into(), sha256: [7; 32] };
    // "Z" (0x5A) sorts before "a" (0x61) bytewise; input order must not matter.
    let a = fingerprint(&[mk("Z-plugin"), mk("a-plugin")]).unwrap();
    let b = fingerprint(&[mk("a-plugin"), mk("Z-plugin")]).unwrap();
    assert_eq!(a, b);
}

#[test]
fn independent_recomputation_of_the_one_plugin_vector() {
    // Hand-built preimage: "example-plugin" 00 "1.2.0" 00 then 32 bytes of 0x11.
    use sha2::{Digest, Sha256};
    let mut pre = b"example-plugin\0".to_vec();
    pre.extend_from_slice(b"1.2.0\0");
    pre.extend_from_slice(&[0x11; 32]);
    let digest = Sha256::digest(&pre);
    let one = SimPlugin { id: "example-plugin".into(), version: "1.2.0".into(), sha256: [0x11; 32] };
    assert_eq!(fingerprint(&[one]).unwrap(), digest.as_slice()[..4]);
    assert_eq!(hex(&digest.as_slice()[..4]), "cb345189"); // also checked with sha256sum in Phase 1
}
```

- [ ] **Step 2: Run to verify it fails**

Add `pub mod fingerprint;` to `lib.rs`, create an empty `src/fingerprint.rs`, run `cargo test -p w2r-spec --test fingerprint_vectors 2>&1 | tail -5`.
Expected: compile error, unresolved imports `fingerprint`, `peer_tag`, `DuplicateId`, `SimPlugin`.

- [ ] **Step 3: Implement**

`crates/w2r-spec/src/fingerprint.rs`:
```rust
//! Simulation fingerprint and peer tag (docs/load-policy.md, docs/abi.md).
use sha2::{Digest, Sha256};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimPlugin {
    pub id: String,
    pub version: String,
    pub sha256: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DuplicateId;

/// 4 bytes in wire order; all zero for the empty set. Input: the plugins loaded this session that
/// are simulation-affecting (or approved `hooks.raw`).
pub fn fingerprint(plugins: &[SimPlugin]) -> Result<[u8; 4], DuplicateId> {
    let mut seen = HashSet::new();
    if !plugins.iter().all(|p| seen.insert(p.id.as_str())) {
        return Err(DuplicateId);
    }
    if plugins.is_empty() {
        return Ok([0; 4]);
    }
    let mut sorted: Vec<&SimPlugin> = plugins.iter().collect();
    sorted.sort_by(|a, b| a.id.as_bytes().cmp(b.id.as_bytes()));
    let mut h = Sha256::new();
    for p in sorted {
        h.update(p.id.as_bytes());
        h.update([0u8]);
        h.update(p.version.as_bytes());
        h.update([0u8]);
        h.update(p.sha256);
    }
    let d = h.finalize();
    let mut out = [0u8; 4];
    out.copy_from_slice(&d.as_slice()[..4]);
    Ok(out)
}

/// First 4 bytes of SHA-256 of the UTF-8 plugin id.
pub fn peer_tag(id: &str) -> [u8; 4] {
    let d = Sha256::digest(id.as_bytes());
    let mut out = [0u8; 4];
    out.copy_from_slice(&d.as_slice()[..4]);
    out
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p w2r-spec 2>&1 | tail -12`
Expected: all tests PASS. If `digest.as_slice()` does not exist in `sha2` 0.11, use `&digest[..]` (the output type derefs to a slice) in both the source and the test.

- [ ] **Step 5: Add the fuzz targets**

`fuzz/Cargo.toml`:
```toml
[package]
name = "w2r-spec-fuzz"
version = "0.0.0"
edition = "2021"
publish = false

[package.metadata]
cargo-fuzz = true

[dependencies]
libfuzzer-sys = "0.4"
w2r-spec = { path = "../crates/w2r-spec" }

[workspace]

[[bin]]
name = "manifest_parse"
path = "fuzz_targets/manifest_parse.rs"
test = false
doc = false

[[bin]]
name = "state_parse"
path = "fuzz_targets/state_parse.rs"
test = false
doc = false

[[bin]]
name = "json_parse"
path = "fuzz_targets/json_parse.rs"
test = false
doc = false
```

`fuzz/fuzz_targets/manifest_parse.rs`:
```rust
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let _ = w2r_spec::manifest::parse(data);
});
```
`fuzz/fuzz_targets/state_parse.rs`:
```rust
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let _ = w2r_spec::state::parse(data);
});
```
`fuzz/fuzz_targets/json_parse.rs`:
```rust
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let _ = w2r_spec::json::parse_json(data);
});
```

Run: `cargo check --manifest-path fuzz/Cargo.toml 2>&1 | tail -5`
Expected: `Finished`. (Running the targets needs nightly and `cargo-fuzz`; this plan only requires that they compile. If the check fails because `libfuzzer-sys` needs a C++ toolchain flag, record a ledger ruling and keep the files.)

- [ ] **Step 6: Wire everything together**

`tools/run_all.sh`: add before the final line `cargo test --workspace --quiet` (and keep the other commands).

`README.md`: add to the Specs list: `- [docs/state.md](docs/state.md): state.json`; add a "Rust crates" line: "`crates/w2r-spec`: parsers and validators for the specs, tested against `docs/vectors/`."

`docs/PLAN.md`, Phase 2 heading: add "Status: 2a-1 (`w2r-spec`) done: manifest, state and fingerprint parsers in safe Rust, passing the shared vectors."

- [ ] **Step 7: Full verification and commit**

Run: `bash tools/run_all.sh 2>&1 | tail -8`
Expected: Python tests OK, Rust tests pass, `C11 ok`, `C++17 ok`.

```bash
git add crates fuzz tools/run_all.sh README.md docs/PLAN.md Cargo.lock
git commit -m "Add fingerprint and peer tag, fuzz targets, wire up checks" -m "Claude-Session: https://claude.ai/code/session_015GrKhNbxr1N7QrzwX3YLE7"
```
