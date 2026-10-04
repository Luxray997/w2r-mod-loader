//! Strict JSON, exactly as docs/manifest.md requires so every implementation refuses the same
//! documents: no BOM, no duplicate keys, no `NaN`/`Infinity`, no lone surrogates, at most 32
//! nested containers (the root is depth 1), valid UTF-8. The reference is
//! `tools/w2r_spec/jsonutil.py`; the order of refusals matches it: the first syntax problem in
//! document order wins (`duplicate_key` counts as one), and the surrogate and depth checks run
//! only after a successful parse.
//!
//! Objects keep their key order. Numbers keep their text, so `1`, `1.0` and `1e0` stay apart.

use std::fmt::Write as _;

pub const MAX_DEPTH: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reject {
    NotJson,
    DuplicateKey,
}

impl Reject {
    pub fn code(&self) -> &'static str {
        match self {
            Reject::NotJson => "not_json",
            Reject::DuplicateKey => "duplicate_key",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Number {
    pub text: String,
}

impl Number {
    /// True for `-?digits` with no fraction or exponent (what Python's `json` makes an `int`).
    pub fn is_integer(&self) -> bool {
        !self.text.contains(['.', 'e', 'E'])
    }

    /// The value when it is a non-negative integer that fits; `None` otherwise.
    pub fn as_u64(&self) -> Option<u64> {
        if !self.is_integer() {
            return None;
        }
        if self.text == "-0" {
            return Some(0);
        }
        self.text.parse().ok()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&[(String, Value)]> {
        match self {
            Value::Object(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<&Number> {
        match self {
            Value::Number(n) => Some(n),
            _ => None,
        }
    }

    /// Compact serialisation (round-trips through [`parse`]).
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        self.write_json(&mut out);
        out
    }

    fn write_json(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => out.push_str(&n.text),
            Value::String(s) => write_string(out, s),
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write_json(out);
                }
                out.push(']');
            }
            Value::Object(pairs) => {
                out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_string(out, k);
                    out.push(':');
                    v.write_json(out);
                }
                out.push('}');
            }
        }
    }
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
    depth: usize,
    max_depth: usize,
    lone_surrogate: bool,
}

/// Parses UTF-8 JSON bytes under the strict rules above.
pub fn parse(raw: &[u8]) -> Result<Value, Reject> {
    let text = core::str::from_utf8(raw).map_err(|_| Reject::NotJson)?;
    if text.starts_with('\u{feff}') {
        return Err(Reject::NotJson);
    }
    let mut p = Parser {
        s: text.as_bytes(),
        pos: 0,
        depth: 0,
        max_depth: 0,
        lone_surrogate: false,
    };
    p.skip_ws();
    let value = p.value()?;
    p.skip_ws();
    if p.pos != p.s.len() {
        return Err(Reject::NotJson);
    }
    if p.lone_surrogate || p.max_depth > MAX_DEPTH {
        return Err(Reject::NotJson);
    }
    Ok(value)
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, lit: &[u8]) -> Result<(), Reject> {
        if self.s[self.pos..].starts_with(lit) {
            self.pos += lit.len();
            Ok(())
        } else {
            Err(Reject::NotJson)
        }
    }

    fn value(&mut self) -> Result<Value, Reject> {
        match self.peek().ok_or(Reject::NotJson)? {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => Ok(Value::String(self.string()?)),
            b't' => self.expect(b"true").map(|_| Value::Bool(true)),
            b'f' => self.expect(b"false").map(|_| Value::Bool(false)),
            b'n' => self.expect(b"null").map(|_| Value::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(Reject::NotJson),
        }
    }

    fn enter(&mut self) {
        self.depth += 1;
        self.max_depth = self.max_depth.max(self.depth);
    }

    fn object(&mut self) -> Result<Value, Reject> {
        self.pos += 1; // {
        self.enter();
        let mut pairs: Vec<(String, Value)> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(Value::Object(pairs));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(Reject::NotJson);
            }
            let key = self.string()?;
            self.skip_ws();
            self.expect(b":")?;
            self.skip_ws();
            let value = self.value()?;
            if pairs.iter().any(|(k, _)| *k == key) {
                return Err(Reject::DuplicateKey);
            }
            pairs.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    self.depth -= 1;
                    return Ok(Value::Object(pairs));
                }
                _ => return Err(Reject::NotJson),
            }
        }
    }

    fn array(&mut self) -> Result<Value, Reject> {
        self.pos += 1; // [
        self.enter();
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    self.depth -= 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(Reject::NotJson),
            }
        }
    }

    fn number(&mut self) -> Result<Value, Reject> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(Reject::NotJson),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(Reject::NotJson);
            }
            self.digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(Reject::NotJson);
            }
            self.digits();
        }
        let text = core::str::from_utf8(&self.s[start..self.pos])
            .unwrap()
            .to_owned();
        Ok(Value::Number(Number { text }))
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
    }

    fn hex4(&mut self) -> Result<u32, Reject> {
        let chunk = self.s.get(self.pos..self.pos + 4).ok_or(Reject::NotJson)?;
        let text = core::str::from_utf8(chunk).map_err(|_| Reject::NotJson)?;
        let v = u32::from_str_radix(text, 16).map_err(|_| Reject::NotJson)?;
        self.pos += 4;
        Ok(v)
    }

    /// A string whose opening quote is at `pos`. A lone surrogate is recorded, not refused, so
    /// a later syntax error still wins as in the reference; the text gets U+FFFD there.
    fn string(&mut self) -> Result<String, Reject> {
        self.pos += 1; // "
        let mut out = String::new();
        loop {
            let b = self.peek().ok_or(Reject::NotJson)?;
            match b {
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.pos += 1;
                    let e = self.peek().ok_or(Reject::NotJson)?;
                    self.pos += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let c = match hi {
                                0xD800..=0xDBFF => {
                                    if self.s[self.pos..].starts_with(b"\\u") {
                                        let save = self.pos;
                                        self.pos += 2;
                                        let lo = self.hex4()?;
                                        if (0xDC00..=0xDFFF).contains(&lo) {
                                            char::from_u32(
                                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00),
                                            )
                                        } else {
                                            self.pos = save;
                                            self.lone_surrogate = true;
                                            None
                                        }
                                    } else {
                                        self.lone_surrogate = true;
                                        None
                                    }
                                }
                                0xDC00..=0xDFFF => {
                                    self.lone_surrogate = true;
                                    None
                                }
                                v => char::from_u32(v),
                            };
                            out.push(c.unwrap_or('\u{fffd}'));
                        }
                        _ => return Err(Reject::NotJson),
                    }
                }
                0x00..=0x1F => return Err(Reject::NotJson),
                _ => {
                    // Copy one whole UTF-8 scalar (input is valid UTF-8).
                    let rest =
                        core::str::from_utf8(&self.s[self.pos..]).map_err(|_| Reject::NotJson)?;
                    let c = rest.chars().next().ok_or(Reject::NotJson)?;
                    out.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) -> Value {
        parse(s.as_bytes()).unwrap()
    }

    #[test]
    fn parses_the_example_manifest_shape() {
        let v =
            ok(r#"{"schema": 1, "id": "x", "list": [1, 2.5, -3e2, true, null], "s": "a\u00e9\n"}"#);
        assert_eq!(
            v.get("schema").unwrap().as_number().unwrap().as_u64(),
            Some(1)
        );
        let list = v.get("list").unwrap().as_array().unwrap();
        assert_eq!(list.len(), 5);
        assert!(list[0].as_number().unwrap().is_integer());
        assert!(!list[1].as_number().unwrap().is_integer());
        assert_eq!(list[2].as_number().unwrap().text, "-3e2");
        assert_eq!(v.get("s").unwrap().as_str(), Some("aé\n"));
        assert_eq!(ok("[]"), Value::Array(vec![]));
        assert_eq!(ok(" {} "), Value::Object(vec![]));
        assert_eq!(ok("\"\\ud83d\\ude00\"").as_str(), Some("😀"));
    }

    #[test]
    fn refuses_what_the_spec_refuses() {
        for bad in [
            "",
            "{",
            "[1,]",
            "{\"a\":1,}",
            "01",
            "1.",
            ".5",
            "+1",
            "1e",
            "NaN",
            "Infinity",
            "-Infinity",
            "nul",
            "tru",
            "\"abc",
            "\"a\nb\"",
            "\"\\x\"",
            "\"\\u12\"",
            "{a:1}",
            "{\"a\" 1}",
            "[1 2]",
            "{} x",
            "\u{feff}{}",
        ] {
            assert_eq!(parse(bad.as_bytes()), Err(Reject::NotJson), "{bad:?}");
        }
        assert_eq!(parse(b"\xff\xfe{}"), Err(Reject::NotJson), "invalid utf-8");
        assert_eq!(parse(br#"{"a":1,"a":2}"#), Err(Reject::DuplicateKey));
    }

    #[test]
    fn lone_surrogates_and_depth_are_checked_after_a_successful_parse() {
        assert_eq!(parse(br#"{"name": "\ud800"}"#), Err(Reject::NotJson));
        assert_eq!(parse(br#"{"\udc00": 1}"#), Err(Reject::NotJson));
        assert_eq!(
            parse(br#"{"\udc00": 1, "a": 1, "a": 2}"#),
            Err(Reject::DuplicateKey),
            "syntax first"
        );
        assert_eq!(parse(br#"{"x": "\ud800", "y": tru}"#), Err(Reject::NotJson));
        let nest = |n: usize| format!("{{\"x\": {}{}}}", "[".repeat(n), "]".repeat(n));
        assert!(parse(nest(31).as_bytes()).is_ok(), "depth 32 total");
        assert_eq!(parse(nest(32).as_bytes()), Err(Reject::NotJson), "depth 33");
        assert_eq!(
            parse(nest(2000).as_bytes()),
            Err(Reject::NotJson),
            "runaway nesting"
        );
        assert_eq!(
            parse("{\"a\": \"\\udc00\", \"b\": [1,]}".as_bytes()),
            Err(Reject::NotJson)
        );
    }

    #[test]
    fn numbers_keep_their_text_and_integers_are_strict() {
        let n = |t: &str| Number { text: t.into() };
        assert_eq!(n("1").as_u64(), Some(1));
        assert_eq!(n("-0").as_u64(), Some(0));
        assert_eq!(n("-1").as_u64(), None);
        assert_eq!(n("1.0").as_u64(), None);
        assert_eq!(n("1e0").as_u64(), None);
        assert_eq!(n("4294967296").as_u64(), Some(4_294_967_296));
        assert_eq!(n("99999999999999999999999").as_u64(), None);
    }

    #[test]
    fn serialisation_round_trips() {
        let src = r#"{"a":[1,2.50,"q\"\\\n\u0001é"],"b":{"c":null,"d":false}}"#;
        let v = ok(src);
        assert_eq!(
            v.to_json(),
            r#"{"a":[1,2.50,"q\"\\\n\u0001é"],"b":{"c":null,"d":false}}"#
        );
        assert_eq!(ok(&v.to_json()), v);
    }
}
