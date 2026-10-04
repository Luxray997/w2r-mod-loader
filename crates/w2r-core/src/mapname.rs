//! Which map names may be requested, offered or written to disk (docs/map-download.md). Ported
//! from wc2r-plus `map_download/map_name.cpp`: 1 to 64 printable ASCII bytes, no path or Windows
//! reserved characters, no leading or trailing space or dot, a `.pud` extension with something
//! before it, and not a Windows device name. A name that passes cannot reach outside the folder
//! it is joined to.

pub const MAX_NAME_LEN: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameFault {
    Empty,
    TooLong,
    NotPrintableAscii,
    ForbiddenChar,
    LeadingSpaceOrDot,
    TrailingSpaceOrDot,
    NotPud,
    EmptyStem,
    DeviceName,
}

impl NameFault {
    pub fn describe(self) -> &'static str {
        match self {
            NameFault::Empty => "empty",
            NameFault::TooLong => "too long",
            NameFault::NotPrintableAscii => "not printable ASCII",
            NameFault::ForbiddenChar => "forbidden character",
            NameFault::LeadingSpaceOrDot => "starts with a space or dot",
            NameFault::TrailingSpaceOrDot => "ends with a space or dot",
            NameFault::NotPud => "not a .pud",
            NameFault::EmptyStem => "nothing before .pud",
            NameFault::DeviceName => "a Windows device name",
        }
    }
}

const DEVICES: [&str; 27] = [
    "con", "prn", "aux", "nul", "com0", "com1", "com2", "com3", "com4", "com5", "com6", "com7",
    "com8", "com9", "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    "conin$", "conout$", "clock$",
];

pub fn validate(name: &[u8]) -> Result<(), NameFault> {
    if name.is_empty() {
        return Err(NameFault::Empty);
    }
    if name.len() > MAX_NAME_LEN {
        return Err(NameFault::TooLong);
    }
    for &c in name {
        if !(0x20..=0x7e).contains(&c) {
            return Err(NameFault::NotPrintableAscii);
        }
        if b"\\/:*?\"<>|".contains(&c) {
            return Err(NameFault::ForbiddenChar);
        }
    }
    if name[0] == b' ' || name[0] == b'.' {
        return Err(NameFault::LeadingSpaceOrDot);
    }
    let last = name[name.len() - 1];
    if last == b' ' || last == b'.' {
        return Err(NameFault::TrailingSpaceOrDot);
    }
    if name.len() < 4 || !name[name.len() - 4..].eq_ignore_ascii_case(b".pud") {
        return Err(NameFault::NotPud);
    }
    if name.len() == 4 {
        return Err(NameFault::EmptyStem);
    }
    // Windows ignores everything from the first dot, and trailing spaces before it.
    let mut base = name.iter().position(|&b| b == b'.').unwrap_or(name.len());
    while base > 0 && name[base - 1] == b' ' {
        base -= 1;
    }
    if DEVICES
        .iter()
        .any(|d| name[..base].eq_ignore_ascii_case(d.as_bytes()))
    {
        return Err(NameFault::DeviceName);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str) -> Result<(), NameFault> {
        validate(name.as_bytes())
    }

    #[test]
    fn ordinary_names_pass() {
        for ok in [
            "ridge.pud",
            "Gold Rush 2.PUD",
            "a.pud",
            "(4)Big Map v1.2.pud",
            "x_y-z.Pud",
        ] {
            assert_eq!(check(ok), Ok(()), "{ok}");
        }
        assert_eq!(
            check(&format!("{}.pud", "a".repeat(60))),
            Ok(()),
            "64 bytes exactly"
        );
    }

    #[test]
    fn each_fault_is_reported() {
        assert_eq!(validate(b""), Err(NameFault::Empty));
        assert_eq!(
            check(&format!("{}.pud", "a".repeat(61))),
            Err(NameFault::TooLong)
        );
        assert_eq!(check("caf\u{e9}.pud"), Err(NameFault::NotPrintableAscii));
        assert_eq!(validate(b"tab\t.pud"), Err(NameFault::NotPrintableAscii));
        assert_eq!(validate(b"del\x7f.pud"), Err(NameFault::NotPrintableAscii));
        for bad in [
            "a\\b.pud", "a/b.pud", "c:x.pud", "a*.pud", "a?.pud", "a\".pud", "a<.pud", "a>.pud",
            "a|.pud",
        ] {
            assert_eq!(check(bad), Err(NameFault::ForbiddenChar), "{bad}");
        }
        assert_eq!(check(" a.pud"), Err(NameFault::LeadingSpaceOrDot));
        assert_eq!(check(".a.pud"), Err(NameFault::LeadingSpaceOrDot));
        assert_eq!(check("a.pud "), Err(NameFault::TrailingSpaceOrDot));
        assert_eq!(check("a.pud."), Err(NameFault::TrailingSpaceOrDot));
        assert_eq!(check("a.txt"), Err(NameFault::NotPud));
        assert_eq!(check("pud"), Err(NameFault::NotPud));
        assert_eq!(check("a.pudx"), Err(NameFault::NotPud));
        assert_eq!(check(" .pud"), Err(NameFault::LeadingSpaceOrDot));
        assert_eq!(check("x.pud").is_ok(), true);
        assert_eq!(
            validate(b".pud"),
            Err(NameFault::LeadingSpaceOrDot),
            "the dot rule comes first"
        );
    }

    #[test]
    fn windows_device_names_are_refused_however_they_are_dressed() {
        for dev in [
            "CON.pud",
            "nul.pud",
            "Aux.pud",
            "com1.pud",
            "LPT9.pud",
            "conin$.pud",
            "clock$.pud",
            "con .pud",
        ] {
            assert_eq!(check(dev), Err(NameFault::DeviceName), "{dev}");
        }
        assert_eq!(
            check("con.x.pud"),
            Err(NameFault::DeviceName),
            "text after a dot is ignored by Windows"
        );
        assert_eq!(check("console.pud"), Ok(()));
        assert_eq!(check("com10.pud"), Ok(()));
    }

    #[test]
    fn a_name_that_passes_is_one_path_component() {
        // The property the store relies on: nothing here can climb or name a drive.
        for name in ["..pud", "...pud", "a..pud"] {
            let r = check(name);
            if r.is_ok() {
                assert!(!name.contains(['\\', '/', ':']));
            }
        }
        assert!(check("..\\evil.pud").is_err());
        assert!(check("../evil.pud").is_err());
        assert!(check("C:evil.pud").is_err());
    }
}
