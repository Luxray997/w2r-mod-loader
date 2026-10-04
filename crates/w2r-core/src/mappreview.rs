//! Rendering a map preview (docs/create-screen.md, slice 3), ported from wc2r-plus
//! `map_preview.cpp`: every tile of the map is looked up in the era's tileset art and averaged to
//! a colour, resource and start markers are stamped on top, and the result is a small RGBA image
//! the game draws as a texture. All of it is pure: the tileset art arrives as bytes and the
//! picture leaves as pixels, so it is tested without the game. Pixels are `0xAABBGGRR`, which is
//! `R, G, B, A` in memory on x86, what the game's RGBA8 texture wants.

use crate::pud::{Marker, Pud, markers};

pub const fn rgba(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16) | 0xff00_0000
}

/// A megatile is a 4 by 4 grid of minitiles; `cv4` rows hold this many entries.
const SUB_GRID: usize = 4;
const SUB_PER_TILE: usize = SUB_GRID * SUB_GRID;
const CV4_ROW_ENTRIES: usize = 0x15;
/// Bytes per megatile in `vx4` (16 words), per minitile in `vr4` (8 by 8 palette indices).
const VX4_MEGATILE: usize = 32;
const VR4_MINITILE: usize = 64;
/// The swamp tileset's empty tile is drawn as this megatile (wc2r-plus).
const SWAMP_ERA: u16 = 3;
const SWAMP_EMPTY_TILE: u16 = 0x153;
/// Longest side of the preview image.
pub const TEX_SIZE: usize = 256;
const MINE_TILES: usize = 3;
const START_TILES: usize = 4;

pub const PLAYER_COLOURS: [u32; 8] = [
    rgba(0xc8, 0x00, 0x00),
    rgba(0x00, 0x00, 0xc8),
    rgba(0x00, 0xc8, 0xc8),
    rgba(0x80, 0x00, 0xc8),
    rgba(0xff, 0x80, 0x00),
    rgba(0x00, 0x00, 0x00),
    rgba(0xff, 0xff, 0xff),
    rgba(0xc8, 0xc8, 0x00),
];
const GOLD_MINE: u32 = rgba(0xff, 0xd0, 0x20);
const OIL_PATCH: u32 = rgba(0x20, 0x20, 0x20);

/// Where an era's art lives under the game's `Data\Art\bgs` folder: `(folder, file stem)`.
pub const TILESET_FILES: [(&str, &str); 4] = [
    ("Forest", "forest"),
    ("Iceland", "iceland"),
    ("Swamp", "swamp"),
    ("XSwamp", "xswamp"),
];

/// The averaged colour of every minitile of every megatile, plus the map-tile to megatile table.
#[derive(Clone, Debug, Default)]
pub struct Tileset {
    cv4: Vec<u16>,
    /// `SUB_PER_TILE` colours per megatile.
    sub_colour: Vec<u32>,
}

impl Tileset {
    /// From the four art files: `cv4` (tile to megatile), `vx4` (megatile to minitiles), `vr4`
    /// (minitile pixels) and `ppl` (palette, 6-bit channels).
    pub fn build(cv4: &[u8], vx4: &[u8], vr4: &[u8], ppl: &[u8]) -> Tileset {
        let cv4: Vec<u16> = cv4
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let mut palette = [[0u8; 3]; 256];
        for (i, rgb) in ppl.chunks_exact(3).take(256).enumerate() {
            palette[i] = [rgb[0] << 2, rgb[1] << 2, rgb[2] << 2];
        }
        let megatiles = vx4.len() / VX4_MEGATILE;
        let minitiles = vr4.len() / VR4_MINITILE;
        let mut sub_colour = Vec::with_capacity(megatiles * SUB_PER_TILE);
        for mt in 0..megatiles {
            for k in 0..SUB_PER_TILE {
                let at = mt * VX4_MEGATILE + k * 2;
                let entry = u16::from_le_bytes([vx4[at], vx4[at + 1]]) as usize;
                let mi = entry >> 2;
                let colour = if mi < minitiles {
                    let px = &vr4[mi * VR4_MINITILE..(mi + 1) * VR4_MINITILE];
                    let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
                    for &p in px {
                        let c = palette[p as usize];
                        r += c[0] as u32;
                        g += c[1] as u32;
                        b += c[2] as u32;
                    }
                    let n = VR4_MINITILE as u32;
                    rgba((r / n) as u8, (g / n) as u8, (b / n) as u8)
                } else {
                    rgba(0, 0, 0)
                };
                sub_colour.push(colour);
            }
        }
        Tileset { cv4, sub_colour }
    }

    pub fn is_empty(&self) -> bool {
        self.sub_colour.is_empty()
    }

    fn megatile(&self, raw: u16, era: u16) -> usize {
        let idx = (raw >> 4) as usize * CV4_ROW_ENTRIES + (raw & 0x0f) as usize;
        let mut mt = self.cv4.get(idx).copied().unwrap_or(0);
        if era == SWAMP_ERA && mt == 0 {
            mt = SWAMP_EMPTY_TILE;
        }
        mt as usize
    }

    fn sub(&self, megatile: usize, sub: usize) -> u32 {
        self.sub_colour
            .get(megatile * SUB_PER_TILE + sub)
            .copied()
            .unwrap_or(rgba(0, 0, 0))
    }
}

/// The finished picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub map_w: u16,
    pub map_h: u16,
    pub width: usize,
    pub height: usize,
    /// `width * height` pixels, row by row.
    pub texels: Vec<u32>,
}

#[allow(clippy::manual_checked_ops)]
/// Renders `pud` with `tileset` (the map's own era unless `era` overrides it, 0 to 3). The long
/// side is `tex_size` pixels. Resource markers go down first, start locations over them.
pub fn render(pud: &Pud, tileset: &Tileset, era: u16, tex_size: usize) -> Preview {
    let (w, h) = (pud.w as usize, pud.h as usize);
    let src_w = w * SUB_GRID;
    let src_h = h * SUB_GRID;
    let src_max = src_w.max(src_h);
    let tex_w = (tex_size * src_w / src_max).max(1);
    let tex_h = (tex_size * src_h / src_max).max(1);

    // The megatile under each map tile, once.
    let megatiles: Vec<usize> = (0..w * h)
        .map(|i| {
            let raw = u16::from_le_bytes([pud.mtxm[i * 2], pud.mtxm[i * 2 + 1]]);
            tileset.megatile(raw, era)
        })
        .collect();

    let mut texels = vec![rgba(0, 0, 0); tex_w * tex_h];
    for py in 0..tex_h {
        let sy0 = py * src_h / tex_h;
        let sy1 = ((py + 1) * src_h / tex_h).max(sy0 + 1);
        for px in 0..tex_w {
            let sx0 = px * src_w / tex_w;
            let sx1 = ((px + 1) * src_w / tex_w).max(sx0 + 1);
            let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            for sy in sy0..sy1.min(src_h) {
                for sx in sx0..sx1.min(src_w) {
                    let mt = megatiles[(sy / SUB_GRID) * w + sx / SUB_GRID];
                    let c = tileset.sub(mt, (sy % SUB_GRID) * SUB_GRID + sx % SUB_GRID);
                    r += c & 0xff;
                    g += (c >> 8) & 0xff;
                    b += (c >> 16) & 0xff;
                    n += 1;
                }
            }
            if n > 0 {
                texels[py * tex_w + px] = rgba((r / n) as u8, (g / n) as u8, (b / n) as u8);
            }
        }
    }

    let per_tile = tex_w / w.max(1);
    let mine_size = (per_tile * MINE_TILES).max(2);
    let start_size = (per_tile * START_TILES).max(3);
    let units = markers(pud);
    let mut stamp = |m: &Marker, size: usize, colour: u32| {
        let cx = m.x as usize * tex_w / w;
        let cy = m.y as usize * tex_h / h;
        for ty in cy..(cy + size).min(tex_h) {
            for tx in cx..(cx + size).min(tex_w) {
                texels[ty * tex_w + tx] = colour;
            }
        }
    };
    for m in units.iter().filter(|m| !m.is_start) {
        stamp(m, mine_size, if m.is_gold { GOLD_MINE } else { OIL_PATCH });
    }
    for m in units.iter().filter(|m| m.is_start) {
        stamp(m, start_size, PLAYER_COLOURS[(m.owner & 7) as usize]);
    }

    Preview {
        map_w: pud.w,
        map_h: pud.h,
        width: tex_w,
        height: tex_h,
        texels,
    }
}

/// A map path from the game's own list is read as a file, so refuse the ones that could leave
/// the game folder or name a network share: no UNC prefix, no `..` component, no NUL.
pub fn safe_map_path(path: &str) -> bool {
    if path.is_empty() || path.contains('\0') || path.starts_with("\\\\") || path.starts_with("//")
    {
        return false;
    }
    !path.split(['\\', '/']).any(|part| part == "..")
}

/// The tileset the preview should use for the create screen's tileset dropdown key, if it names
/// one (`customscenario_tileset_forest` and so on); `None` keeps the map's own.
pub fn tileset_for_key(key: &str) -> Option<u16> {
    match key {
        "customscenario_tileset_forest" => Some(0),
        "customscenario_tileset_winter" => Some(1),
        "customscenario_tileset_wasteland" => Some(2),
        "customscenario_tileset_orcswamp" => Some(3),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pud::parse;
    use crate::pud::tests::{pud, unit};

    /// Two megatiles: 0 is black, 1 is all minitile 1 which is solid palette entry 1 (red).
    fn tiny_tileset() -> Tileset {
        let mut cv4 = vec![0u8; CV4_ROW_ENTRIES * 2];
        cv4[2..4].copy_from_slice(&1u16.to_le_bytes()); // tile word 1 -> megatile 1
        let mut vx4 = vec![0u8; 2 * VX4_MEGATILE];
        for k in 0..SUB_PER_TILE {
            let at = VX4_MEGATILE + k * 2;
            vx4[at..at + 2].copy_from_slice(&(1u16 << 2).to_le_bytes());
        }
        let mut vr4 = vec![0u8; 2 * VR4_MINITILE];
        vr4[VR4_MINITILE..].fill(1);
        let ppl = [0, 0, 0, 63, 0, 0];
        Tileset::build(&cv4, &vx4, &vr4, &ppl)
    }

    #[test]
    fn a_tileset_averages_minitiles_through_the_palette() {
        let ts = tiny_tileset();
        assert!(!ts.is_empty());
        assert_eq!(ts.sub(0, 0), rgba(0, 0, 0));
        assert_eq!(ts.sub(1, 5), rgba(252, 0, 0), "63 << 2 is 252");
        assert_eq!(ts.sub(9, 0), rgba(0, 0, 0), "out of range is black");
        assert!(Tileset::build(&[], &[], &[], &[]).is_empty());
        // A megatile pointing past the minitile art is black, not a panic.
        let far = Tileset::build(&[], &[0xff; VX4_MEGATILE], &[0; VR4_MINITILE], &[]);
        assert_eq!(far.sub(0, 0), rgba(0, 0, 0));
    }

    #[test]
    fn a_map_of_red_tiles_renders_red_at_the_right_size() {
        let bytes = pud(4, 2, 0, 1, &[]);
        let p = parse(&bytes).unwrap();
        let r = render(&p, &tiny_tileset(), 0, 64);
        assert_eq!(
            (r.width, r.height),
            (64, 32),
            "the long side is the texture size"
        );
        assert_eq!(r.texels.len(), 64 * 32);
        assert!(r.texels.iter().all(|&t| t == rgba(252, 0, 0)));
        let black = pud(2, 2, 0, 0, &[]);
        let r = render(&parse(&black).unwrap(), &tiny_tileset(), 0, 16);
        assert!(r.texels.iter().all(|&t| t == rgba(0, 0, 0)));
    }

    #[test]
    fn markers_are_stamped_resources_first_then_starts_over_them() {
        let units = [
            unit(2, 2, 0x5c, 15), // gold mine at (2,2)
            unit(2, 2, 0x5e, 1),  // a start on top of it
            unit(0, 0, 0x5d, 15), // oil patch in the corner
        ];
        let bytes = pud(8, 8, 0, 1, &units);
        let r = render(&parse(&bytes).unwrap(), &tiny_tileset(), 0, 64);
        let at = |x: usize, y: usize| r.texels[y * r.width + x];
        // 8 texels per tile: mine is 24 square, start 32 square.
        assert_eq!(at(0, 0), OIL_PATCH);
        assert_eq!(at(16, 16), PLAYER_COLOURS[1], "the start covers the mine");
        assert_eq!(at(16 + 31, 16 + 31), PLAYER_COLOURS[1]);
        assert_eq!(at(63, 63), rgba(252, 0, 0), "terrain elsewhere");
    }

    #[test]
    fn a_marker_near_the_edge_is_clipped_not_wrapped() {
        let bytes = pud(4, 4, 0, 1, &[unit(3, 3, 0x5e, 0)]);
        let r = render(&parse(&bytes).unwrap(), &tiny_tileset(), 0, 16);
        assert_eq!(r.texels.len(), 16 * 16);
        assert_eq!(r.texels[15 * 16 + 15], PLAYER_COLOURS[0]);
        assert_eq!(r.texels[0], rgba(252, 0, 0));
    }

    #[test]
    fn the_swamp_empty_tile_is_substituted_only_for_swamp() {
        let ts = Tileset {
            cv4: vec![0],
            sub_colour: vec![],
        };
        assert_eq!(ts.megatile(0, 3), SWAMP_EMPTY_TILE as usize);
        assert_eq!(ts.megatile(0, 0), 0);
    }

    #[test]
    fn map_paths_cannot_leave_the_folder_or_name_a_share() {
        assert!(safe_map_path("Maps\\Duels\\ridge.pud"));
        assert!(safe_map_path("D:\\Games\\W2\\Maps\\a b.pud"));
        assert!(safe_map_path("Maps/a.pud"));
        assert!(!safe_map_path(""));
        assert!(!safe_map_path("\\\\server\\share\\a.pud"));
        assert!(!safe_map_path("//server/share/a.pud"));
        assert!(!safe_map_path("Maps\\..\\..\\secret"));
        assert!(!safe_map_path("..\\a.pud"));
        assert!(!safe_map_path("Maps\\a\0.pud"));
        assert!(
            safe_map_path("Maps\\a..b.pud"),
            "dots inside a name are fine"
        );
    }

    #[test]
    fn dropdown_keys_name_the_tilesets() {
        assert_eq!(tileset_for_key("customscenario_tileset_forest"), Some(0));
        assert_eq!(tileset_for_key("customscenario_tileset_orcswamp"), Some(3));
        assert_eq!(tileset_for_key("customscenario_tileset_random"), None);
        assert_eq!(tileset_for_key(""), None);
    }
}
