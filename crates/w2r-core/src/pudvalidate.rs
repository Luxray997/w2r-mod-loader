//! The map validator a downloaded map must pass before it is written to disk (docs/map-download.md),
//! ported from wc2r-plus `map_download/pud_validator.cpp` and `pud_rules.h`. It checks the shape
//! of a Warcraft II map and that every value the game later reads lies inside what real maps hold:
//! section structure, lengths, dimensions, owners, eras, tile codes, terrain flags, oil map, unit
//! records and sizes, and the custom unit and upgrade data when the map uses it. It does not
//! check what real maps vary freely (the region map, starting resources, allowed units, the
//! description text, unit resource amounts). A map that passes is still loaded by the game's
//! own code, which nobody has audited.
//!
//! Every length is checked against what remains before anything is read; no input panics.

use crate::mapwire::MAX_MAP_BYTES;

pub const MAX_UNITS: u32 = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PudFault {
    TooSmall,
    TooLarge,
    TruncatedHeader,
    TruncatedSection,
    UnknownTag,
    DuplicateTag,
    FirstTagNotType,
    MissingTag,
    BadLength,
    BadType,
    BadVersion,
    DescNotTerminated,
    BadOwner,
    BadEra,
    BadDimensions,
    BadSide,
    BadAiPlayer,
    BadUnitCount,
    UnitOutsideMap,
    BadUnitType,
    BadUnitOwner,
    BadTile,
    BadUnitSize,
    BadUnitData,
    BadUpgradeData,
    BadTerrainFlags,
    BadOilMap,
}

impl PudFault {
    pub fn describe(self) -> &'static str {
        match self {
            PudFault::TooSmall => "too small",
            PudFault::TooLarge => "too large",
            PudFault::TruncatedHeader => "truncated section header",
            PudFault::TruncatedSection => "section runs past the end",
            PudFault::UnknownTag => "unknown section",
            PudFault::DuplicateTag => "duplicate section",
            PudFault::FirstTagNotType => "first section is not TYPE",
            PudFault::MissingTag => "required section missing",
            PudFault::BadLength => "section has the wrong length",
            PudFault::BadType => "TYPE is not a WAR2 MAP",
            PudFault::BadVersion => "unknown VER",
            PudFault::DescNotTerminated => "DESC is not NUL-terminated",
            PudFault::BadOwner => "OWNR value not seen in real maps",
            PudFault::BadEra => "ERA value not seen in real maps",
            PudFault::BadDimensions => "DIM not seen in real maps",
            PudFault::BadSide => "SIDE value not seen in real maps",
            PudFault::BadAiPlayer => "AIPL value not seen in real maps",
            PudFault::BadUnitCount => "UNIT length or count out of range",
            PudFault::UnitOutsideMap => "unit outside the map",
            PudFault::BadUnitType => "unit type not seen in real maps",
            PudFault::BadUnitOwner => "unit owner not seen in real maps",
            PudFault::BadTile => "tile code past the tileset",
            PudFault::BadUnitSize => "unit size not seen in real maps",
            PudFault::BadUnitData => "custom unit data outside what real maps hold",
            PudFault::BadUpgradeData => "custom upgrade data outside what real maps hold",
            PudFault::BadTerrainFlags => "SQM value not seen in real maps",
            PudFault::BadOilMap => "OILM value not seen in real maps",
        }
    }
}

/// Where and what: the fault, the byte offset it was found at and the section it is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub fault: PudFault,
    pub offset: u32,
    pub tag: Option<[u8; 4]>,
}

// ---- the rules (wc2r-plus `pud_rules.h`) ---------------------------------------------------

const DIMS: [u16; 6] = [14, 18, 32, 64, 96, 128];
const VERSIONS: [u16; 2] = [17, 19];
const ERAS: [u16; 3] = [0, 1, 2];
const ERAX: [u16; 1] = [3];
const OWNERS: [u8; 6] = [2, 3, 4, 5, 6, 7];
const SIDES: [u8; 3] = [0, 1, 2];
const AI_PLAYERS: [u8; 8] = [0, 1, 23, 25, 26, 28, 31, 67];
const UNIT_TYPES: [u8; 100] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31, 32, 33, 35, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 49, 50, 51, 52, 53,
    55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78,
    79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101,
    102, 103, 104,
];
const UNIT_OWNERS: [u8; 10] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 15];
/// Tile rows in each era's tileset (`cv4` size over 42 bytes): all four are 158 in this build.
/// A tile code's row is its high twelve bits and must be below the era's row count.
const TILE_ROWS: [u16; 4] = [158; 4];
const UDTA_SIZE_OFFSET: usize = 2448;
const UDTA_UNIT_TYPES: usize = 110;
const UNIT_SIZES: [u16; 5] = [0, 1, 2, 3, 4];
const FOOTPRINT: [u8; UDTA_UNIT_TYPES] = [
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1, 1, 1, 1, 1, 0, 1, 1, 1, 2, 2, 3, 3, 3, 3,
    2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 3, 3, 1, 1,
    2, 2, 2, 2, 2, 4, 2, 1, 1, 1, 1, 2, 3, 4,
];
const SQM_VALUES: [u16; 13] = [0, 1, 2, 17, 64, 129, 130, 137, 141, 513, 576, 2445, 2817];
const OILM_VALUES: [u8; 9] = [0, 248, 249, 250, 251, 252, 253, 254, 255];
const TYPE_PREFIX: [u8; 10] = *b"WAR2 MAP\0\0";
const UDTA_LENGTHS: [u32; 2] = [5696, 5950];

#[derive(Clone, Copy)]
enum Check {
    Range,
    Mask,
}

/// `count` fields of `size` bytes from `offset`, each in `min..=max` (or, for a mask, with no bit
/// outside `max`).
struct Field {
    offset: u32,
    size: u32,
    count: u32,
    check: Check,
    min: u32,
    max: u32,
}

const fn f(offset: u32, size: u32, count: u32, check: Check, min: u32, max: u32) -> Field {
    Field {
        offset,
        size,
        count,
        check,
        min,
        max,
    }
}

const UDTA_RULES: [Field; 28] = [
    f(2, 2, 110, Check::Range, 0, 15),
    f(1238, 4, 110, Check::Range, 0, 9),
    f(1678, 2, 110, Check::Range, 0, 65535),
    f(1898, 1, 110, Check::Range, 0, 255),
    f(2008, 1, 110, Check::Range, 0, 255),
    f(2118, 1, 110, Check::Range, 0, 255),
    f(2228, 1, 110, Check::Range, 0, 255),
    f(2338, 1, 110, Check::Range, 0, 255),
    f(2448, 2, 220, Check::Range, 0, 4),
    f(2888, 2, 220, Check::Range, 0, 127),
    f(3328, 1, 110, Check::Range, 0, 255),
    f(3438, 1, 110, Check::Range, 0, 255),
    f(3548, 1, 110, Check::Range, 0, 255),
    f(3658, 1, 110, Check::Range, 0, 255),
    f(3768, 1, 110, Check::Range, 0, 1),
    f(3878, 1, 110, Check::Range, 0, 255),
    f(3988, 1, 110, Check::Range, 0, 255),
    f(4098, 1, 110, Check::Range, 0, 255),
    f(4208, 1, 110, Check::Range, 0, 1),
    f(4318, 1, 110, Check::Range, 0, 1),
    f(4428, 1, 110, Check::Range, 0, 29),
    f(4538, 1, 110, Check::Range, 0, 2),
    f(4648, 1, 110, Check::Range, 0, 255),
    f(4758, 1, 110, Check::Range, 0, 50),
    f(4868, 1, 58, Check::Range, 0, 6),
    f(4926, 2, 110, Check::Range, 0, 32768),
    f(5146, 1, 110, Check::Mask, 0, 0x7),
    f(5256, 4, 110, Check::Mask, 0, 0xfff_ffff),
];

const UGRD_RULES: [Field; 7] = [
    f(2, 1, 52, Check::Range, 0, 250),
    f(54, 2, 52, Check::Range, 0, 50000),
    f(158, 2, 52, Check::Range, 0, 25555),
    f(262, 2, 52, Check::Range, 0, 25555),
    f(366, 2, 52, Check::Range, 6, 169),
    f(470, 2, 52, Check::Range, 0, 20),
    f(574, 4, 52, Check::Mask, 0, 0x1f_ffff),
];

/// Every rule must fit inside the shortest section it applies to.
const fn rules_fit(rules: &[Field], length: u32) -> bool {
    let mut i = 0;
    while i < rules.len() {
        if rules[i].offset + rules[i].size * rules[i].count > length {
            return false;
        }
        i += 1;
    }
    true
}
const _: () = assert!(rules_fit(&UDTA_RULES, 5696));
const _: () = assert!(rules_fit(&UGRD_RULES, 782));

// ---- sections ------------------------------------------------------------------------------

/// Section indices into [`TAGS`]; some variants exist only to keep the numbering.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum T {
    Type,
    Ver,
    Desc,
    Ownr,
    Era,
    Erax,
    Dim,
    Udta,
    Alow,
    Ugrd,
    Side,
    Sgld,
    Slbr,
    Soil,
    Aipl,
    Sign,
    Mtxm,
    Sqm,
    Oilm,
    Regm,
    Unit,
}

const TAG_COUNT: usize = 21;

/// `(tag, required, exact length or 0 for "checked later")`, indexed by [`T`].
const TAGS: [(&[u8; 4], bool, u32); TAG_COUNT] = [
    (b"TYPE", true, 16),
    (b"VER ", true, 2),
    (b"DESC", true, 32),
    (b"OWNR", true, 16),
    (b"ERA ", true, 2),
    (b"ERAX", false, 2),
    (b"DIM ", true, 4),
    (b"UDTA", true, 0),
    (b"ALOW", false, 384),
    (b"UGRD", true, 782),
    (b"SIDE", true, 16),
    (b"SGLD", true, 32),
    (b"SLBR", true, 32),
    (b"SOIL", true, 32),
    (b"AIPL", true, 16),
    (b"SIGN", false, 4),
    (b"MTXM", true, 0),
    (b"SQM ", true, 0),
    (b"OILM", true, 0),
    (b"REGM", true, 0),
    (b"UNIT", true, 0),
];

#[derive(Clone, Copy, Default)]
struct Section {
    present: bool,
    offset: u32,
    length: u32,
}

fn r16(d: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([d[at], d[at + 1]])
}

fn verdict(fault: PudFault, offset: u32, tag: Option<usize>) -> Verdict {
    Verdict {
        fault,
        offset,
        tag: tag.map(|t| *TAGS[t].0),
    }
}

fn first_bad_field(section: &[u8], rules: &[Field]) -> Option<u32> {
    for r in rules {
        for k in 0..r.count {
            let at = (r.offset + k * r.size) as usize;
            let mut v = 0u32;
            for b in 0..r.size as usize {
                v |= (section[at + b] as u32) << (8 * b);
            }
            let ok = match r.check {
                Check::Mask => v & !r.max == 0,
                Check::Range => v >= r.min && v <= r.max,
            };
            if !ok {
                return Some(at as u32);
            }
        }
    }
    None
}

fn all_bytes_in(d: &[u8], s: &Section, set: &[u8]) -> bool {
    d[s.offset as usize..(s.offset + s.length) as usize]
        .iter()
        .all(|b| set.contains(b))
}

/// Checks a whole map file. `None` when it is acceptable.
pub fn validate(data: &[u8]) -> Option<Verdict> {
    let fail = |fault, offset| Some(verdict(fault, offset, None));
    if data.len() < 8 {
        return fail(PudFault::TooSmall, 0);
    }
    if data.len() > MAX_MAP_BYTES as usize {
        return fail(PudFault::TooLarge, 0);
    }
    let total = data.len();

    let mut sections = [Section::default(); TAG_COUNT];
    let mut off = 0usize;
    let mut first = true;
    while off < total {
        let remaining = total - off;
        if remaining < 8 {
            return fail(PudFault::TruncatedHeader, off as u32);
        }
        let length =
            u32::from_le_bytes([data[off + 4], data[off + 5], data[off + 6], data[off + 7]]);
        if length as usize > remaining - 8 {
            return fail(PudFault::TruncatedSection, off as u32);
        }
        let Some(tag) = TAGS.iter().position(|t| t.0[..] == data[off..off + 4]) else {
            return fail(PudFault::UnknownTag, off as u32);
        };
        if first && tag != T::Type as usize {
            return fail(PudFault::FirstTagNotType, off as u32);
        }
        first = false;
        if sections[tag].present {
            return Some(verdict(PudFault::DuplicateTag, off as u32, Some(tag)));
        }
        sections[tag] = Section {
            present: true,
            offset: (off + 8) as u32,
            length,
        };
        off += 8 + length as usize;
    }

    for (i, s) in sections.iter().enumerate() {
        if !s.present {
            if TAGS[i].1 {
                return Some(verdict(PudFault::MissingTag, 0, Some(i)));
            }
            continue;
        }
        if TAGS[i].2 != 0 && s.length != TAGS[i].2 {
            return Some(verdict(PudFault::BadLength, s.offset, Some(i)));
        }
    }
    let sec = |t: T| sections[t as usize];
    let at = |t: T| sec(t).offset as usize;

    if data[at(T::Type)..at(T::Type) + 10] != TYPE_PREFIX {
        return Some(verdict(
            PudFault::BadType,
            sec(T::Type).offset,
            Some(T::Type as usize),
        ));
    }
    if !VERSIONS.contains(&r16(data, at(T::Ver))) {
        return Some(verdict(
            PudFault::BadVersion,
            sec(T::Ver).offset,
            Some(T::Ver as usize),
        ));
    }
    {
        let d = sec(T::Desc);
        let body = &data[d.offset as usize..(d.offset + d.length) as usize];
        if !body.contains(&0) {
            return Some(verdict(
                PudFault::DescNotTerminated,
                d.offset,
                Some(T::Desc as usize),
            ));
        }
    }
    if !all_bytes_in(data, &sec(T::Ownr), &OWNERS) {
        return Some(verdict(
            PudFault::BadOwner,
            sec(T::Ownr).offset,
            Some(T::Ownr as usize),
        ));
    }
    let era = r16(data, at(T::Era));
    if !ERAS.contains(&era) {
        return Some(verdict(
            PudFault::BadEra,
            sec(T::Era).offset,
            Some(T::Era as usize),
        ));
    }
    let mut era_index = era as usize;
    if sec(T::Erax).present {
        if !ERAX.contains(&r16(data, at(T::Erax))) {
            return Some(verdict(
                PudFault::BadEra,
                sec(T::Erax).offset,
                Some(T::Erax as usize),
            ));
        }
        era_index = 3;
    }

    let width = r16(data, at(T::Dim));
    let height = r16(data, at(T::Dim) + 2);
    if width != height || !DIMS.contains(&width) {
        return Some(verdict(
            PudFault::BadDimensions,
            sec(T::Dim).offset,
            Some(T::Dim as usize),
        ));
    }
    let cells = width as u32 * height as u32;

    if !UDTA_LENGTHS.contains(&sec(T::Udta).length) {
        return Some(verdict(
            PudFault::BadLength,
            sec(T::Udta).offset,
            Some(T::Udta as usize),
        ));
    }
    if !all_bytes_in(data, &sec(T::Side), &SIDES) {
        return Some(verdict(
            PudFault::BadSide,
            sec(T::Side).offset,
            Some(T::Side as usize),
        ));
    }
    if !all_bytes_in(data, &sec(T::Aipl), &AI_PLAYERS) {
        return Some(verdict(
            PudFault::BadAiPlayer,
            sec(T::Aipl).offset,
            Some(T::Aipl as usize),
        ));
    }
    for t in [T::Mtxm, T::Sqm, T::Regm] {
        if sec(t).length != 2 * cells {
            return Some(verdict(
                PudFault::BadLength,
                sec(t).offset,
                Some(t as usize),
            ));
        }
    }
    let oilm = sec(T::Oilm);
    if oilm.length != cells && oilm.length != 2 * cells {
        return Some(verdict(
            PudFault::BadLength,
            oilm.offset,
            Some(T::Oilm as usize),
        ));
    }

    let mtxm = sec(T::Mtxm);
    for i in (0..mtxm.length as usize).step_by(2) {
        let row = r16(data, mtxm.offset as usize + i) >> 4;
        if row >= TILE_ROWS[era_index] {
            return Some(verdict(
                PudFault::BadTile,
                mtxm.offset + i as u32,
                Some(T::Mtxm as usize),
            ));
        }
    }

    let udta = sec(T::Udta);
    let udta_at = udta.offset as usize;
    // A leading zero word means the map carries its own unit data (wc2r-plus `customUnits`).
    let custom_units = r16(data, udta_at) == 0;
    let sizes = udta_at + UDTA_SIZE_OFFSET;
    for t in 0..UDTA_UNIT_TYPES {
        let w = r16(data, sizes + 4 * t);
        if !UNIT_SIZES.contains(&w) || r16(data, sizes + 4 * t + 2) != w {
            return Some(verdict(
                PudFault::BadUnitSize,
                (sizes + 4 * t) as u32,
                Some(T::Udta as usize),
            ));
        }
    }
    let section_bytes = |s: Section| &data[s.offset as usize..(s.offset + s.length) as usize];
    if custom_units && let Some(bad) = first_bad_field(section_bytes(udta), &UDTA_RULES) {
        return Some(verdict(
            PudFault::BadUnitData,
            udta.offset + bad,
            Some(T::Udta as usize),
        ));
    }
    let ugrd = sec(T::Ugrd);
    if r16(data, ugrd.offset as usize) == 0
        && let Some(bad) = first_bad_field(section_bytes(ugrd), &UGRD_RULES)
    {
        return Some(verdict(
            PudFault::BadUpgradeData,
            ugrd.offset + bad,
            Some(T::Ugrd as usize),
        ));
    }
    let sqm = sec(T::Sqm);
    for i in (0..sqm.length as usize).step_by(2) {
        if !SQM_VALUES.contains(&r16(data, sqm.offset as usize + i)) {
            return Some(verdict(
                PudFault::BadTerrainFlags,
                sqm.offset + i as u32,
                Some(T::Sqm as usize),
            ));
        }
    }
    if !all_bytes_in(data, &oilm, &OILM_VALUES) {
        return Some(verdict(
            PudFault::BadOilMap,
            oilm.offset,
            Some(T::Oilm as usize),
        ));
    }

    let unit = sec(T::Unit);
    if unit.length % 8 != 0 || unit.length / 8 > MAX_UNITS {
        return Some(verdict(
            PudFault::BadUnitCount,
            unit.offset,
            Some(T::Unit as usize),
        ));
    }
    for i in 0..(unit.length / 8) as usize {
        let rec = unit.offset as usize + i * 8;
        let here = rec as u32;
        let kind = data[rec + 4];
        if !UNIT_TYPES.contains(&kind) {
            return Some(verdict(PudFault::BadUnitType, here, Some(T::Unit as usize)));
        }
        let mut size = FOOTPRINT[kind as usize] as u32;
        if custom_units {
            size = size.max(r16(data, sizes + 4 * kind as usize) as u32);
        }
        if size == 0 {
            size = 1;
        }
        if r16(data, rec) as u32 + size > width as u32
            || r16(data, rec + 2) as u32 + size > height as u32
        {
            return Some(verdict(
                PudFault::UnitOutsideMap,
                here,
                Some(T::Unit as usize),
            ));
        }
        if !UNIT_OWNERS.contains(&data[rec + 5]) {
            return Some(verdict(
                PudFault::BadUnitOwner,
                here,
                Some(T::Unit as usize),
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = tag.to_vec();
        v.extend_from_slice(&(body.len() as u32).to_le_bytes());
        v.extend_from_slice(body);
        v
    }

    /// The parts of a map a test may want to change, with values that pass.
    #[derive(Clone)]
    struct Map {
        ver: u16,
        desc: Vec<u8>,
        ownr: Vec<u8>,
        era: u16,
        erax: Option<u16>,
        dim: (u16, u16),
        udta: Vec<u8>,
        ugrd: Vec<u8>,
        side: Vec<u8>,
        aipl: Vec<u8>,
        mtxm_tile: u16,
        sqm: u16,
        oilm: u8,
        units: Vec<[u8; 8]>,
        /// The REGM length in cells (it must be `2 * w * h` bytes).
        regm_cells: Option<usize>,
    }

    fn good() -> Map {
        let mut udta = vec![0u8; 5696];
        udta[0] = 1; // use the default unit data
        let mut ugrd = vec![0u8; 782];
        ugrd[0] = 1; // and the default upgrades
        Map {
            ver: 19,
            desc: b"A test map\0"
                .iter()
                .copied()
                .chain(core::iter::repeat_n(0, 21))
                .collect(),
            ownr: vec![2; 16],
            era: 0,
            erax: None,
            dim: (32, 32),
            udta,
            ugrd,
            side: vec![0; 16],
            aipl: vec![0; 16],
            mtxm_tile: 0x10,
            sqm: 0,
            oilm: 0,
            units: vec![unit(1, 1, 0x5e, 0)],
            regm_cells: None,
        }
    }

    fn unit(x: u16, y: u16, kind: u8, owner: u8) -> [u8; 8] {
        let mut r = [0u8; 8];
        r[..2].copy_from_slice(&x.to_le_bytes());
        r[2..4].copy_from_slice(&y.to_le_bytes());
        r[4] = kind;
        r[5] = owner;
        r
    }

    fn build(m: &Map) -> Vec<u8> {
        let cells = m.dim.0 as usize * m.dim.1 as usize;
        let mut type_body = b"WAR2 MAP\0\0".to_vec();
        type_body.extend_from_slice(&[0; 6]);
        let mut out = section(b"TYPE", &type_body);
        out.extend(section(b"VER ", &m.ver.to_le_bytes()));
        out.extend(section(b"DESC", &m.desc));
        out.extend(section(b"OWNR", &m.ownr));
        out.extend(section(b"ERA ", &m.era.to_le_bytes()));
        if let Some(x) = m.erax {
            out.extend(section(b"ERAX", &x.to_le_bytes()));
        }
        out.extend(section(
            b"DIM ",
            &[m.dim.0.to_le_bytes(), m.dim.1.to_le_bytes()].concat(),
        ));
        out.extend(section(b"UDTA", &m.udta));
        out.extend(section(b"UGRD", &m.ugrd));
        out.extend(section(b"SIDE", &m.side));
        out.extend(section(b"SGLD", &[0; 32]));
        out.extend(section(b"SLBR", &[0; 32]));
        out.extend(section(b"SOIL", &[0; 32]));
        out.extend(section(b"AIPL", &m.aipl));
        out.extend(section(b"MTXM", &m.mtxm_tile.to_le_bytes().repeat(cells)));
        out.extend(section(b"SQM ", &m.sqm.to_le_bytes().repeat(cells)));
        out.extend(section(b"OILM", &vec![m.oilm; cells]));
        out.extend(section(
            b"REGM",
            &vec![0; m.regm_cells.unwrap_or(cells) * 2],
        ));
        out.extend(section(b"UNIT", &m.units.concat()));
        out
    }

    fn fault(m: &Map) -> Option<PudFault> {
        validate(&build(m)).map(|v| v.fault)
    }

    #[test]
    fn a_well_formed_map_passes() {
        assert_eq!(validate(&build(&good())), None);
        let mut m = good();
        m.erax = Some(3);
        m.dim = (128, 128);
        m.ver = 17;
        assert_eq!(validate(&build(&m)), None, "xswamp, big, old version");
        let mut m = good();
        m.udta = vec![0; 5950];
        m.udta[0] = 1;
        assert_eq!(fault(&m), None, "the longer UDTA is allowed");
    }

    #[test]
    fn file_structure_faults() {
        assert_eq!(validate(&[]).unwrap().fault, PudFault::TooSmall);
        assert_eq!(
            validate(&vec![0; MAX_MAP_BYTES as usize + 1])
                .unwrap()
                .fault,
            PudFault::TooLarge
        );
        let bytes = build(&good());
        assert_eq!(
            validate(&bytes[..bytes.len() - 3]).unwrap().fault,
            PudFault::TruncatedSection
        );
        let mut extra = bytes.clone();
        extra.extend_from_slice(b"JUNK");
        assert_eq!(validate(&extra).unwrap().fault, PudFault::TruncatedHeader);
        let mut unknown = bytes.clone();
        unknown.extend(section(b"ZZZZ", b""));
        let v = validate(&unknown).unwrap();
        assert_eq!(v.fault, PudFault::UnknownTag);
        let mut dup = bytes.clone();
        dup.extend(section(b"SIDE", &[0; 16]));
        let v = validate(&dup).unwrap();
        assert_eq!((v.fault, v.tag), (PudFault::DuplicateTag, Some(*b"SIDE")));
        let mut not_first = section(b"VER ", &19u16.to_le_bytes());
        not_first.extend_from_slice(&bytes);
        assert_eq!(
            validate(&not_first).unwrap().fault,
            PudFault::FirstTagNotType
        );
        // A section dropped: UNIT is the last one.
        let unit_at = bytes.len() - (8 + 8);
        let v = validate(&bytes[..unit_at]).unwrap();
        assert_eq!((v.fault, v.tag), (PudFault::MissingTag, Some(*b"UNIT")));
        // A wrong fixed length: SIDE with 15 bytes.
        let mut m = good();
        m.side = vec![0; 15];
        let v = validate(&build(&m)).unwrap();
        assert_eq!((v.fault, v.tag), (PudFault::BadLength, Some(*b"SIDE")));
    }

    #[test]
    fn header_and_player_faults() {
        let mut m = good();
        m.ver = 18;
        assert_eq!(fault(&m), Some(PudFault::BadVersion));
        m = good();
        m.desc = vec![b'x'; 32];
        assert_eq!(fault(&m), Some(PudFault::DescNotTerminated));
        m = good();
        m.ownr[3] = 1;
        assert_eq!(fault(&m), Some(PudFault::BadOwner));
        m = good();
        m.era = 3;
        assert_eq!(fault(&m), Some(PudFault::BadEra));
        m = good();
        m.erax = Some(2);
        assert_eq!(fault(&m), Some(PudFault::BadEra));
        m = good();
        m.dim = (32, 64);
        assert_eq!(fault(&m), Some(PudFault::BadDimensions), "not square");
        m.dim = (20, 20);
        assert_eq!(fault(&m), Some(PudFault::BadDimensions), "not a real size");
        m = good();
        m.side[0] = 3;
        assert_eq!(fault(&m), Some(PudFault::BadSide));
        m = good();
        m.aipl[0] = 2;
        assert_eq!(fault(&m), Some(PudFault::BadAiPlayer));
        let mut bytes = build(&good());
        bytes[8 + 3] = b'X';
        assert_eq!(validate(&bytes).unwrap().fault, PudFault::BadType);
    }

    #[test]
    fn tile_terrain_and_oil_faults() {
        let mut m = good();
        m.mtxm_tile = 158 << 4;
        assert_eq!(fault(&m), Some(PudFault::BadTile));
        m.mtxm_tile = (157 << 4) | 0xf;
        assert_eq!(fault(&m), None, "the last row is fine");
        m = good();
        m.sqm = 3;
        assert_eq!(fault(&m), Some(PudFault::BadTerrainFlags));
        m = good();
        m.oilm = 7;
        assert_eq!(fault(&m), Some(PudFault::BadOilMap));
        m.oilm = 251;
        assert_eq!(fault(&m), None);
        // A section sized for a different map.
        m = good();
        m.regm_cells = Some(1000);
        let v = validate(&build(&m)).unwrap();
        assert_eq!((v.fault, v.tag), (PudFault::BadLength, Some(*b"REGM")));
    }

    #[test]
    fn unit_faults() {
        let mut m = good();
        m.units = vec![unit(0, 0, 0xff, 0)];
        assert_eq!(fault(&m), Some(PudFault::BadUnitType));
        m.units = vec![unit(0, 0, 34, 0)];
        assert_eq!(
            fault(&m),
            Some(PudFault::BadUnitType),
            "34 is not a real unit"
        );
        m.units = vec![unit(0, 0, 0x5e, 9)];
        assert_eq!(fault(&m), Some(PudFault::BadUnitOwner));
        m.units = vec![unit(32, 0, 0x5e, 0)];
        assert_eq!(fault(&m), Some(PudFault::UnitOutsideMap));
        // A town hall (footprint 4) too close to the edge.
        m.units = vec![unit(29, 0, 88, 0)];
        assert_eq!(FOOTPRINT[88], 4);
        assert_eq!(fault(&m), Some(PudFault::UnitOutsideMap));
        m.units = vec![unit(28, 28, 88, 0)];
        assert_eq!(fault(&m), None);
        m.units = vec![[0; 8]; MAX_UNITS as usize + 1];
        assert_eq!(fault(&m), Some(PudFault::BadUnitCount));
        let mut bytes = build(&good());
        let n = bytes.len();
        bytes.truncate(n - 8);
        let unit_len_at = bytes.windows(4).rposition(|w| w == b"UNIT").unwrap() + 4;
        bytes[unit_len_at..unit_len_at + 4].copy_from_slice(&5u32.to_le_bytes());
        bytes.extend_from_slice(&[0; 5]);
        assert_eq!(
            validate(&bytes).unwrap().fault,
            PudFault::BadUnitCount,
            "not a whole record"
        );
    }

    #[test]
    fn custom_data_is_checked_only_when_the_map_uses_it() {
        // A map that says "default units" may carry any bytes in the data it does not use.
        let mut m = good();
        m.udta[2] = 200;
        assert_eq!(fault(&m), None);
        // The same byte under "custom units" is out of range (offset 2 holds 0..=15).
        m.udta[0] = 0;
        m.udta[2] = 200;
        let v = validate(&build(&m)).unwrap();
        assert_eq!((v.fault, v.tag), (PudFault::BadUnitData, Some(*b"UDTA")));
        m.udta[2] = 9;
        assert_eq!(fault(&m), None, "in range under custom units");
        // Custom upgrades: zeros are out of range for the cost field at 366.
        let mut m = good();
        m.ugrd[0] = 0;
        assert_eq!(fault(&m), Some(PudFault::BadUpgradeData));
        // Unit sizes are checked either way: 5 is not a size, and width must equal height.
        let mut m = good();
        m.udta[UDTA_SIZE_OFFSET] = 5;
        assert_eq!(fault(&m), Some(PudFault::BadUnitSize));
        let mut m = good();
        m.udta[UDTA_SIZE_OFFSET + 2] = 1;
        assert_eq!(fault(&m), Some(PudFault::BadUnitSize));
        // Under custom units a bigger declared size pushes a unit off the map.
        let mut m = good();
        m.udta[0] = 0;
        let kind = 0x5e;
        m.udta[UDTA_SIZE_OFFSET + 4 * kind] = 4;
        m.udta[UDTA_SIZE_OFFSET + 4 * kind + 2] = 4;
        m.units = vec![unit(30, 30, kind as u8, 0)];
        assert_eq!(fault(&m), Some(PudFault::UnitOutsideMap));
    }

    #[test]
    fn nothing_panics_on_any_prefix_or_mutation() {
        let bytes = build(&good());
        for cut in 0..bytes.len() {
            let _ = validate(&bytes[..cut]);
        }
        for i in (0..bytes.len()).step_by(7) {
            for v in [0u8, 0xff, 0x80] {
                let mut x = bytes.clone();
                x[i] = v;
                let _ = validate(&x);
            }
        }
        let _ = validate(&[0xff; 8]);
        let _ = validate(&[0; 100]);
    }

    #[test]
    fn the_rule_tables_hold_what_the_c_plus_plus_has() {
        assert_eq!(UNIT_TYPES.len(), 100);
        assert!(
            UNIT_TYPES.windows(2).all(|w| w[0] < w[1]),
            "sorted and unique"
        );
        assert_eq!(FOOTPRINT.len(), 110);
        assert_eq!(UDTA_RULES.len(), 28);
        assert_eq!(UGRD_RULES.len(), 7);
        assert_eq!(TAGS.len(), TAG_COUNT);
    }
}
