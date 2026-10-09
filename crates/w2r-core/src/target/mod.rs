//! What the host knows about the one supported game build. The signature and symbol tables are
//! generated from wc2r-plus by `tools/gen_signatures.py` and `tools/gen_symbols.py`; when the
//! game updates, this directory is the one place to fix.

pub mod signatures;
pub mod symbols;

/// The build every signature below was taken from.
pub const GAME_VERSION: &str = "1.0.2.2818";
/// `Warcraft II.exe` links at this base; static addresses in the tables assume it.
pub const PREFERRED_IMAGE_BASE: usize = 0x0040_0000;
/// Informational: logged when it differs, not a refusal by itself.
pub const EXPECTED_SIZE_OF_IMAGE: u32 = 0x0062_B000;

/// Four bytes inside a signature that hold an absolute address and therefore change with the
/// load address. They are compared after relocation, not byte for byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelocSlot {
    pub offset: usize,
    pub static_addr: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signature {
    pub what: &'static str,
    pub static_addr: u32,
    pub bytes: &'static [u8],
    pub relocs: &'static [RelocSlot],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Function,
    Object,
}

/// A named game function or global. `c_type` is the C declaration from wc2r-plus, for humans
/// and the plugin docs; the host does not parse it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: &'static str,
    pub kind: Kind,
    pub static_addr: u32,
    pub c_type: &'static str,
}

impl Signature {
    pub fn in_reloc(&self, i: usize) -> bool {
        self.relocs
            .iter()
            .any(|r| i >= r.offset && i < r.offset + 4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_table_is_well_formed() {
        assert_eq!(signatures::SIGNATURES.len(), signatures::COUNT);
        const _: () = assert!(signatures::COUNT > 100, "table looks truncated");
        for s in signatures::SIGNATURES {
            assert!(!s.bytes.is_empty(), "{}", s.what);
            assert!(s.static_addr as usize >= PREFERRED_IMAGE_BASE, "{}", s.what);
            assert!(
                (s.static_addr as usize - PREFERRED_IMAGE_BASE) < EXPECTED_SIZE_OF_IMAGE as usize,
                "{} lies outside the image",
                s.what
            );
            for r in s.relocs {
                assert!(
                    r.offset + 4 <= s.bytes.len(),
                    "{}: reloc past the end",
                    s.what
                );
            }
        }
    }

    #[test]
    fn in_reloc_covers_exactly_four_bytes() {
        let s = Signature {
            what: "x",
            static_addr: 0x0040_0000,
            bytes: &[0; 12],
            relocs: &[RelocSlot {
                offset: 6,
                static_addr: 0,
            }],
        };
        let covered: Vec<usize> = (0..12).filter(|&i| s.in_reloc(i)).collect();
        assert_eq!(covered, vec![6, 7, 8, 9]);
    }
}
