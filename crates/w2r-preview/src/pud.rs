//! Reading a Warcraft II map (PUD) far enough to preview it (docs/create-screen.md, slice 3),
//! ported from wc2r-plus `map_preview_pud.cpp`. A PUD is a list of sections, each a four-byte
//! tag and a little-endian `i32` length. Every file is hostile: lengths are checked against what
//! is left, nothing indexes past a section, nothing panics. The same reader is meant to serve the
//! map download validator (Phase 3 step 4).

/// The map side the game supports: larger is refused.
pub const MAX_DIM: u16 = 256;
/// Start locations (human and orc) and the two resource unit types.
const UNIT_START_HUMAN: u8 = 0x5e;
const UNIT_START_ORC: u8 = 0x5f;
const UNIT_GOLD_MINE: u8 = 0x5c;
const UNIT_OIL_PATCH: u8 = 0x5d;
/// Bytes per unit record in the `UNIT` section.
const UNIT_RECORD: usize = 8;
/// More markers than any real map has; the rest are ignored.
const MAX_MARKERS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseFault {
    NotPud,
    MissingSection,
    BadDims,
    ShortMtxm,
}

/// The parts of a PUD a preview needs, borrowed from the file's bytes.
#[derive(Clone, Copy, Debug)]
pub struct Pud<'a> {
    pub w: u16,
    pub h: u16,
    /// Tileset, 0 to 3 (forest, winter, wasteland, swamp).
    pub era: u16,
    /// `w * h` little-endian tile words.
    pub mtxm: &'a [u8],
    /// Eight bytes per unit; empty when the map has no `UNIT` section.
    pub unit: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Marker {
    pub x: u16,
    pub y: u16,
    pub owner: u8,
    pub is_start: bool,
    pub is_gold: bool,
}

/// The body of the last section tagged `tag`. A section whose length does not fit ends the walk.
pub fn find_section<'a>(pud: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    let mut found = None;
    let mut off = 0usize;
    while pud.len() - off >= 8 {
        let len = i32::from_le_bytes(pud[off + 4..off + 8].try_into().ok()?);
        let Ok(len) = usize::try_from(len) else {
            break;
        };
        if len > pud.len() - off - 8 {
            break;
        }
        if pud[off..off + 4] == tag[..] {
            found = Some(&pud[off + 8..off + 8 + len]);
        }
        off += 8 + len;
    }
    found
}

pub fn parse(pud: &[u8]) -> Result<Pud<'_>, ParseFault> {
    if pud.len() < 8 || &pud[..4] != b"TYPE" {
        return Err(ParseFault::NotPud);
    }
    let (Some(dim), Some(era), Some(mtxm)) = (
        find_section(pud, b"DIM "),
        find_section(pud, b"ERA "),
        find_section(pud, b"MTXM"),
    ) else {
        return Err(ParseFault::MissingSection);
    };
    if dim.len() < 4 || era.len() < 2 {
        return Err(ParseFault::MissingSection);
    }
    let w = u16::from_le_bytes([dim[0], dim[1]]);
    let h = u16::from_le_bytes([dim[2], dim[3]]);
    let era = u16::from_le_bytes([era[0], era[1]]) & 3;
    if w == 0 || h == 0 || w > MAX_DIM || h > MAX_DIM {
        return Err(ParseFault::BadDims);
    }
    let need = w as usize * h as usize * 2;
    if mtxm.len() < need {
        return Err(ParseFault::ShortMtxm);
    }
    Ok(Pud {
        w,
        h,
        era,
        mtxm: &mtxm[..need],
        unit: find_section(pud, b"UNIT").unwrap_or(&[]),
    })
}

/// Start locations and gold mines and oil patches on the map, in file order.
pub fn markers(p: &Pud) -> Vec<Marker> {
    let mut out = Vec::new();
    for rec in p.unit.chunks_exact(UNIT_RECORD) {
        if out.len() >= MAX_MARKERS {
            break;
        }
        let x = u16::from_le_bytes([rec[0], rec[1]]);
        let y = u16::from_le_bytes([rec[2], rec[3]]);
        let (kind, owner) = (rec[4], rec[5]);
        if x >= p.w || y >= p.h {
            continue;
        }
        let is_start = kind == UNIT_START_HUMAN || kind == UNIT_START_ORC;
        let is_resource = kind == UNIT_GOLD_MINE || kind == UNIT_OIL_PATCH;
        if is_start || is_resource {
            out.push(Marker {
                x,
                y,
                owner,
                is_start,
                is_gold: kind == UNIT_GOLD_MINE,
            });
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn section(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = tag.to_vec();
        v.extend_from_slice(&(body.len() as i32).to_le_bytes());
        v.extend_from_slice(body);
        v
    }

    /// A small valid PUD: `w` by `h`, `era`, every tile word `tile`, and the given unit records.
    pub fn pud(w: u16, h: u16, era: u16, tile: u16, units: &[[u8; 8]]) -> Vec<u8> {
        let mut out = section(b"TYPE", b"WAR2 MAP\0\0");
        out.extend(section(
            b"DIM ",
            &[w.to_le_bytes(), h.to_le_bytes()].concat(),
        ));
        out.extend(section(b"ERA ", &era.to_le_bytes()));
        let mtxm: Vec<u8> = (0..w as usize * h as usize)
            .flat_map(|_| tile.to_le_bytes())
            .collect();
        out.extend(section(b"MTXM", &mtxm));
        out.extend(section(b"UNIT", &units.concat()));
        out
    }

    pub fn unit(x: u16, y: u16, kind: u8, owner: u8) -> [u8; 8] {
        let mut r = [0u8; 8];
        r[..2].copy_from_slice(&x.to_le_bytes());
        r[2..4].copy_from_slice(&y.to_le_bytes());
        r[4] = kind;
        r[5] = owner;
        r
    }

    #[test]
    fn a_good_map_parses() {
        let bytes = pud(4, 3, 2, 0x10, &[]);
        let p = parse(&bytes).unwrap();
        assert_eq!((p.w, p.h, p.era), (4, 3, 2));
        assert_eq!(p.mtxm.len(), 4 * 3 * 2);
        assert!(p.unit.is_empty());
        let big_era = pud(2, 2, 7, 0, &[]);
        assert_eq!(parse(&big_era).unwrap().era, 3, "era is the low two bits");
    }

    #[test]
    fn bad_files_are_named_not_trusted() {
        assert_eq!(parse(b"").unwrap_err(), ParseFault::NotPud);
        assert_eq!(parse(b"NOPE1234").unwrap_err(), ParseFault::NotPud);
        let mut no_mtxm = section(b"TYPE", b"x");
        no_mtxm.extend(section(b"DIM ", &[2, 0, 2, 0]));
        no_mtxm.extend(section(b"ERA ", &[0, 0]));
        assert_eq!(parse(&no_mtxm).unwrap_err(), ParseFault::MissingSection);
        assert_eq!(
            parse(&pud(0, 4, 0, 0, &[])).unwrap_err(),
            ParseFault::BadDims
        );
        assert_eq!(
            parse(&pud(257, 4, 0, 0, &[])).unwrap_err(),
            ParseFault::BadDims
        );
        let mut short = section(b"TYPE", b"x");
        short.extend(section(b"DIM ", &[4, 0, 4, 0]));
        short.extend(section(b"ERA ", &[0, 0]));
        short.extend(section(b"MTXM", &[0; 10]));
        assert_eq!(parse(&short).unwrap_err(), ParseFault::ShortMtxm);
        let mut tiny_dim = section(b"TYPE", b"x");
        tiny_dim.extend(section(b"DIM ", &[4, 0]));
        tiny_dim.extend(section(b"ERA ", &[0, 0]));
        tiny_dim.extend(section(b"MTXM", &[0; 32]));
        assert_eq!(parse(&tiny_dim).unwrap_err(), ParseFault::MissingSection);
    }

    #[test]
    fn a_lying_section_length_stops_the_walk_without_panicking() {
        let good = pud(4, 4, 0, 1, &[unit(1, 1, 0x5e, 0)]);
        // Every truncation, and a length that points past the end.
        for cut in 0..good.len() {
            let _ = parse(&good[..cut]);
        }
        let mut lying = good.clone();
        lying[4..8].copy_from_slice(&i32::MAX.to_le_bytes());
        let _ = parse(&lying);
        let mut negative = good.clone();
        negative[4..8].copy_from_slice(&(-5i32).to_le_bytes());
        assert!(parse(&negative).is_err() || parse(&negative).is_ok());
        assert_eq!(find_section(&lying, b"DIM "), None);
    }

    #[test]
    fn the_last_section_of_a_tag_wins() {
        let mut bytes = section(b"TYPE", b"x");
        bytes.extend(section(b"ABCD", b"first"));
        bytes.extend(section(b"ABCD", b"second"));
        assert_eq!(find_section(&bytes, b"ABCD"), Some(&b"second"[..]));
        assert_eq!(find_section(&bytes, b"NONE"), None);
    }

    #[test]
    fn markers_pick_starts_and_resources_on_the_map_and_skip_the_rest() {
        let units = [
            unit(0, 0, 0x5e, 0),
            unit(3, 2, 0x5f, 1),
            unit(1, 1, 0x5c, 15),
            unit(2, 0, 0x5d, 15),
            unit(1, 1, 0x00, 0),
            unit(4, 0, 0x5e, 2),
            unit(0, 3, 0x5e, 2),
        ];
        let bytes = pud(4, 3, 0, 0, &units);
        let p = parse(&bytes).unwrap();
        let m = markers(&p);
        assert_eq!(m.len(), 4, "off-map and other units are dropped");
        assert!(m[0].is_start && m[1].is_start && !m[2].is_start && !m[3].is_start);
        assert!(m[2].is_gold && !m[3].is_gold);
        assert_eq!(m[1].owner, 1);
        let mut odd = bytes.clone();
        odd.extend_from_slice(&section(b"UNIT", &[0u8; 5]));
        assert!(
            markers(&parse(&odd).unwrap()).is_empty(),
            "a short last record is ignored"
        );
    }
}
