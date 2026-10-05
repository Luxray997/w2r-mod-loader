//! SemVer 2.0.0 without build metadata, as docs/manifest.md defines it, plus the `version_req`
//! comparator lists used by dependencies. Mirrors `tools/w2r_spec/manifest.py`.

use std::cmp::Ordering;

pub const MAX_LEN: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identifier {
    Numeric(u64),
    Alpha(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub pre: Vec<Identifier>,
}

/// `0` or `[1-9][0-9]*`, at most `u32::MAX`.
fn number(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) || (s.len() > 1 && s.starts_with('0'))
    {
        return None;
    }
    s.parse().ok()
}

fn identifier(s: &str) -> Option<Identifier> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return None;
    }
    if s.bytes().all(|b| b.is_ascii_digit()) {
        if s.len() > 1 && s.starts_with('0') {
            return None;
        }
        // Python compares arbitrary ints; anything this long is beyond u64 and not worth it.
        return s.parse().ok().map(Identifier::Numeric);
    }
    Some(Identifier::Alpha(s.to_owned()))
}

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        if s.len() > MAX_LEN {
            return None;
        }
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let mut parts = core.split('.');
        let major = number(parts.next()?)?;
        let minor = number(parts.next()?)?;
        let patch = number(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(p) => p.split('.').map(identifier).collect::<Option<Vec<_>>>()?,
        };
        Some(Version {
            major,
            minor,
            patch,
            pre,
        })
    }

    fn core(&self) -> (u32, u32, u32) {
        (self.major, self.minor, self.patch)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        for (i, id) in self.pre.iter().enumerate() {
            f.write_str(if i == 0 { "-" } else { "." })?;
            match id {
                Identifier::Numeric(n) => write!(f, "{n}")?,
                Identifier::Alpha(a) => f.write_str(a)?,
            }
        }
        Ok(())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// SemVer precedence: numeric core, then a pre-release sorts before none, identifiers compared
/// numerically or ASCII-wise, a shorter list first.
impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core().cmp(&other.core()).then_with(|| {
            match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => {
                    for (a, b) in self.pre.iter().zip(&other.pre) {
                        let o = match (a, b) {
                            (Identifier::Numeric(x), Identifier::Numeric(y)) => x.cmp(y),
                            (Identifier::Numeric(_), Identifier::Alpha(_)) => Ordering::Less,
                            (Identifier::Alpha(_), Identifier::Numeric(_)) => Ordering::Greater,
                            (Identifier::Alpha(x), Identifier::Alpha(y)) => x.cmp(y),
                        };
                        if o != Ordering::Equal {
                            return o;
                        }
                    }
                    self.pre.len().cmp(&other.pre.len())
                }
            }
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Ge,
    Le,
    Gt,
    Lt,
    Eq,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comparator {
    pub op: Op,
    pub version: Version,
}

/// 1 to 4 comparators `(>=|<=|>|<|=)MAJOR.MINOR.PATCH` joined by `,`, no spaces.
/// Longest operator first, so `>=` is not read as `>` followed by `=1...`.
fn split_op(part: &str) -> Option<(Op, &str)> {
    [
        (">=", Op::Ge),
        ("<=", Op::Le),
        (">", Op::Gt),
        ("<", Op::Lt),
        ("=", Op::Eq),
    ]
    .into_iter()
    .find_map(|(prefix, op)| part.strip_prefix(prefix).map(|rest| (op, rest)))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionReq(pub Vec<Comparator>);

impl VersionReq {
    pub fn parse(s: &str) -> Option<VersionReq> {
        let parts: Vec<&str> = s.split(',').collect();
        if parts.is_empty() || parts.len() > 4 {
            return None;
        }
        let mut out = Vec::with_capacity(parts.len());
        for part in parts {
            let (op, rest) = split_op(part)?;
            let mut nums = rest.split('.');
            let major = number(nums.next()?)?;
            let minor = number(nums.next()?)?;
            let patch = number(nums.next()?)?;
            if nums.next().is_some() {
                return None;
            }
            out.push(Comparator {
                op,
                version: Version {
                    major,
                    minor,
                    patch,
                    pre: Vec::new(),
                },
            });
        }
        Some(VersionReq(out))
    }

    pub fn matches(&self, v: &Version) -> bool {
        self.0.iter().all(|c| match c.op {
            Op::Ge => v >= &c.version,
            Op::Le => v <= &c.version,
            Op::Gt => v > &c.version,
            Op::Lt => v < &c.version,
            Op::Eq => v == &c.version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s}"))
    }

    #[test]
    fn parses_valid_and_refuses_invalid_versions() {
        assert_eq!(v("1.2.0").core(), (1, 2, 0));
        assert_eq!(v("0.0.1").core(), (0, 0, 1));
        assert_eq!(v("4294967295.0.0").major, u32::MAX);
        assert_eq!(
            v("2.0.0-beta.1").pre,
            vec![Identifier::Alpha("beta".into()), Identifier::Numeric(1)]
        );
        assert_eq!(v("1.0.0-01a").pre, vec![Identifier::Alpha("01a".into())]);
        assert_eq!(v("1.0.0--").pre, vec![Identifier::Alpha("-".into())]);
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "01.0.0",
            "1.00.0",
            "1.0.0-",
            "1.0.0-01",
            "1.0.0-a..b",
            "1.0.0+build",
            "1.0.0-beta+1",
            "4294967296.0.0",
            "-1.0.0",
            " 1.0.0",
            "1.0.0\n",
            "1.0.0-é",
        ] {
            assert!(Version::parse(bad).is_none(), "{bad:?}");
        }
        assert!(
            Version::parse(&format!("1.0.0-{}", "a".repeat(60))).is_none(),
            "over 64 bytes"
        );
    }

    #[test]
    fn precedence_follows_semver() {
        let order = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.1.0",
            "2.0.0",
        ];
        for w in order.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
        }
        assert_eq!(v("1.0.0"), v("1.0.0"));
    }

    #[test]
    fn version_reqs_parse_and_match() {
        let r = VersionReq::parse(">=1.0.0,<2.0.0").unwrap();
        assert!(r.matches(&v("1.0.0")));
        assert!(r.matches(&v("1.9.9")));
        assert!(!r.matches(&v("2.0.0")));
        assert!(!r.matches(&v("0.9.0")));
        assert!(!r.matches(&v("1.0.0-beta.1")), "pre-release is below 1.0.0");
        assert!(VersionReq::parse("=1.2.3").unwrap().matches(&v("1.2.3")));
        assert!(VersionReq::parse(">1.2.3").unwrap().matches(&v("1.2.4")));
        assert!(
            VersionReq::parse("<=1.2.3,>=1.2.3,=1.2.3,>1.0.0").is_some(),
            "four comparators"
        );
        for bad in [
            "",
            ">=1.0",
            ">=1.0.0, <2.0.0",
            "1.0.0",
            "~1.0.0",
            ">=01.0.0",
            ">=1.0.0,<2.0.0,>0.0.0,<9.0.0,=1.0.0",
            ">=4294967296.0.0",
            ">=1.0.0-beta",
        ] {
            assert!(VersionReq::parse(bad).is_none(), "{bad:?}");
        }
    }
}
