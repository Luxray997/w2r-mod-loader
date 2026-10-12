//! The settings a match was played with, read from the game's lobby block at match start
//! (`MpLobbySlots` 0x916260, 0x440 bytes, the same block the replay recorder keeps). They go into
//! the report beside the roster so the central database can tell a low-resource game from a
//! high-resource one, a peasants-only game from a normal one, and one `Battle_2.pud` from another
//! with the same file name (the map hash).
//!
//! The raw numbers are sent as the game stores them (`speed`, `flags`), next to a decoded view
//! for the options this build understands. A bit nobody has decoded yet is still in `flags`, so
//! the server can learn it later from the stored reports.
//!
//! Offsets are from the block start (symbols: `MpLobbyGameSpeed` 0x916556, `MpLobbyMatchFlags`
//! 0x916560, `MpLobbyFixedStartLocation` 0x91668a).

use w2r_formats::json::{Number, Value};

use crate::hex;

/// Where the block starts in the game's data section.
pub const LOBBY_AT: usize = 0x0091_6260;
const SPEED_AT: usize = 0x0091_6556 - LOBBY_AT;
const FLAGS_AT: usize = 0x0091_6560 - LOBBY_AT;
const FIXED_AT: usize = 0x0091_668a - LOBBY_AT;
/// The block is `0x916260..0x9166a0`.
pub const BLOCK_LEN: usize = 0x440;

const RESOURCE_MASK: u32 = 0x2_3000;
const TILESET_MASK: u32 = 0x1_c000;
const PEASANTS_ONLY: u32 = 0x200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameInfo {
    /// SHA-256 of the map file that was played, when the host could find it.
    pub map_sha: Option<[u8; 32]>,
    /// The game speed setting, as stored.
    pub speed: u8,
    /// The match flags word, as stored.
    pub flags: u32,
    /// Starting locations are fixed to the slots instead of random.
    pub fixed_start: bool,
}

impl GameInfo {
    /// Reads the settings out of a copy of the lobby block. `None` when the block is too short.
    pub fn from_lobby_block(block: &[u8], map_sha: Option<[u8; 32]>) -> Option<GameInfo> {
        if block.len() < BLOCK_LEN {
            return None;
        }
        Some(GameInfo {
            map_sha,
            speed: block[SPEED_AT],
            flags: u32::from_le_bytes(block[FLAGS_AT..FLAGS_AT + 4].try_into().ok()?),
            fixed_start: block[FIXED_AT] == 1,
        })
    }

    /// The starting resources option: `default`, `random`, `low`, `medium` or `high`.
    pub fn resources(&self) -> &'static str {
        match self.flags & RESOURCE_MASK {
            0x1000 => "low",
            0x2000 => "medium",
            0x3000 => "high",
            0x2_0000 => "random",
            _ => "default",
        }
    }

    /// The tileset option as an index into the game's list (0 is the map's own).
    pub fn tileset(&self) -> u8 {
        match self.flags & TILESET_MASK {
            0x4000 => 1,
            0x8000 => 2,
            0xc000 => 3,
            0x1_c000 => 4,
            _ => 0,
        }
    }

    pub fn peasants_only(&self) -> bool {
        self.flags & PEASANTS_ONLY != 0
    }

    pub fn to_value(&self) -> Value {
        let num = |n: u32| {
            Value::Number(Number {
                text: n.to_string(),
            })
        };
        let text = |s: &str| Value::String(s.to_string());
        Value::Object(vec![
            (
                "map_sha".into(),
                self.map_sha
                    .map_or(Value::Null, |h| Value::String(hex::encode(&h))),
            ),
            ("speed".into(), num(u32::from(self.speed))),
            ("flags".into(), num(self.flags)),
            ("resources".into(), text(self.resources())),
            ("tileset".into(), num(u32::from(self.tileset()))),
            ("peasants_only".into(), Value::Bool(self.peasants_only())),
            ("fixed_start".into(), Value::Bool(self.fixed_start)),
        ])
    }

    /// Reads it back. The decoded keys are derived, so only the raw ones are required.
    pub fn from_value(v: &Value) -> Result<GameInfo, &'static str> {
        let uint = |k: &str| {
            v.get(k)
                .and_then(Value::as_number)
                .and_then(Number::as_u64)
                .ok_or("bad game field")
        };
        let map_sha = match v.get("map_sha") {
            None | Some(Value::Null) => None,
            Some(h) => Some(
                hex::decode_array::<32>(h.as_str().ok_or("bad map_sha")?).ok_or("bad map_sha")?,
            ),
        };
        Ok(GameInfo {
            map_sha,
            speed: u8::try_from(uint("speed")?).map_err(|_| "bad speed")?,
            flags: u32::try_from(uint("flags")?).map_err(|_| "bad flags")?,
            fixed_start: v
                .get("fixed_start")
                .and_then(Value::as_bool)
                .ok_or("bad game field")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(speed: u8, flags: u32, fixed: u8) -> Vec<u8> {
        let mut b = vec![0u8; BLOCK_LEN];
        b[SPEED_AT] = speed;
        b[FLAGS_AT..FLAGS_AT + 4].copy_from_slice(&flags.to_le_bytes());
        b[FIXED_AT] = fixed;
        b
    }

    #[test]
    fn offsets_match_the_symbol_addresses() {
        assert_eq!(SPEED_AT, 0x2f6);
        assert_eq!(FLAGS_AT, 0x300);
        assert_eq!(FIXED_AT, 0x42a);
        assert!(FIXED_AT < BLOCK_LEN);
    }

    #[test]
    fn decodes_the_options() {
        let g = GameInfo::from_lobby_block(&block(3, 0x1000 | 0x200 | 0x8000, 1), None).unwrap();
        assert_eq!(
            (g.speed, g.resources(), g.tileset(), g.peasants_only()),
            (3, "low", 2, true)
        );
        assert!(g.fixed_start);
        let d = GameInfo::from_lobby_block(&block(0, 0, 0), None).unwrap();
        assert_eq!((d.resources(), d.tileset(), d.peasants_only()), ("default", 0, false));
        assert!(!d.fixed_start);
        assert_eq!(
            GameInfo::from_lobby_block(&block(0, 0x3000, 0), None).unwrap().resources(),
            "high"
        );
        assert_eq!(
            GameInfo::from_lobby_block(&block(0, 0x2_0000, 0), None).unwrap().resources(),
            "random"
        );
    }

    #[test]
    fn a_short_block_is_refused() {
        assert!(GameInfo::from_lobby_block(&[0; 16], None).is_none());
    }

    #[test]
    fn round_trips_through_json_with_unknown_bits_kept() {
        let g = GameInfo::from_lobby_block(&block(4, 0x8000_0000 | 0x2000, 0), Some([7; 32]))
            .unwrap();
        let back = GameInfo::from_value(&g.to_value()).unwrap();
        assert_eq!(back, g);
        assert_eq!(back.flags & 0x8000_0000, 0x8000_0000);
    }
}
