//! The `.w2rr` layout (all integers little-endian, `varint` = LEB128):
//!
//! ```text
//! "W2RR" u16 version
//! u32 header_len, header bytes          (a reader skips what it does not know past its fields)
//! records...                            (until the footer or the end of the file)
//!   0x01 varint turn, u8 count, count x (u8 slot, varint len, len bytes)
//!   0x02 u8 results[8], u32 turns, u32 sync_mismatch       footer
//!   0x03 varint turn, u8 who, u8 mode, varint duration_ms, varint len, len text bytes   chat
//! ```
//!
//! The file is valid at every moment. A footer is written as soon as the match has a result and
//! again when recording is closed; turns can follow a footer (a defeated player's team may still be
//! playing) and the last footer wins. A file without one is a match that was left or cut off
//! (surrender, quit, crash); it is still readable up to the last whole record.

use w2r_stats::slot::{SLOT_LEN, SLOTS};

pub const MAGIC: &[u8; 4] = b"W2RR";
pub const VERSION: u16 = 1;

pub const REC_TURN: u8 = 0x01;
pub const REC_FOOTER: u8 = 0x02;
pub const REC_CHAT: u8 = 0x03;
/// Longest chat text kept (the game sends at most 0x50 bytes, a local line can be longer).
pub const MAX_CHAT: usize = 255;

/// Bytes of the game's `PlayerOwner` table that are stored.
pub const OWNER_LEN: usize = 0x20;
/// Bytes of the game's `PlayerNames` table that are stored.
pub const NAMES_LEN: usize = 0x100;

/// The game's lobby block (`MpLobbySlots` 0x916260 up to 0x9166a0): slot table, game name, speed,
/// resources, match flags, map text, fixed start. `StartGame` builds every in-game table from it
/// (docs/replay-research.md, "Playback design"), so it is the complete start state.
pub const LOBBY_AT: u32 = 0x0091_6260;
pub const LOBBY_LEN: usize = 0x440;

/// Limits that keep a hostile or damaged file from allocating without bound.
pub const MAX_HEADER: usize = 64 * 1024;
pub const MAX_PACKET: usize = 64 * 1024;
pub const MAX_TEXT: usize = 1024;

/// What was true when the match began.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// Game build, e.g. `2818`.
    pub build: String,
    /// The recording host (`w2rpm 0.1.0`).
    pub client: String,
    /// Simulation fingerprint of the loaded plugins: a replay only plays back under the same.
    pub fingerprint: [u8; 4],
    pub map: String,
    /// Unix seconds when recording began.
    pub started: u64,
    /// The recording player's slot.
    pub local: u8,
    pub slots: [[u8; SLOT_LEN]; SLOTS],
    pub owner: [u8; OWNER_LEN],
    pub names: [u8; NAMES_LEN],
    /// 8 bytes, or empty if the host could not read them: the game seed (`GameSeed`, little-endian
    /// u32; the host sets it from the clock and clients get it in the start packet, op 0x23) then
    /// the simulation RNG state just before turn 0 (a cross-check for playback).
    pub seed: Vec<u8>,
    /// `LOBBY_LEN` bytes captured when `StartGame` was entered, or empty (older recordings, which
    /// cannot be played back).
    pub lobby: Vec<u8>,
}

impl Header {
    pub fn seed_bytes(game_seed: u32, rng_before_first_turn: u32) -> Vec<u8> {
        let mut v = game_seed.to_le_bytes().to_vec();
        v.extend_from_slice(&rng_before_first_turn.to_le_bytes());
        v
    }

    /// The seed the game was started with, to set before the match is set up on playback.
    pub fn game_seed(&self) -> Option<u32> {
        Some(u32::from_le_bytes(self.seed.get(..4)?.try_into().ok()?))
    }

    /// The simulation RNG state right before the first turn ran.
    pub fn rng_at_start(&self) -> Option<u32> {
        Some(u32::from_le_bytes(self.seed.get(4..8)?.try_into().ok()?))
    }
}

/// How the match ended, written when recording stops normally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Footer {
    pub results: [u8; 8],
    pub turns: u32,
    pub sync_mismatch: u32,
}

pub(crate) fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

pub(crate) fn put_text(out: &mut Vec<u8>, s: &str) {
    let mut n = s.len().min(MAX_TEXT);
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    put_varint(out, n as u64);
    out.extend_from_slice(&s.as_bytes()[..n]);
}

impl Header {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut o = Vec::with_capacity(0x200 + SLOTS * SLOT_LEN);
        put_text(&mut o, &self.build);
        put_text(&mut o, &self.client);
        o.extend_from_slice(&self.fingerprint);
        put_text(&mut o, &self.map);
        o.extend_from_slice(&self.started.to_le_bytes());
        o.push(self.local);
        for s in &self.slots {
            o.extend_from_slice(s);
        }
        o.extend_from_slice(&self.owner);
        o.extend_from_slice(&self.names);
        let seed = &self.seed[..self.seed.len().min(MAX_TEXT)];
        put_varint(&mut o, seed.len() as u64);
        o.extend_from_slice(seed);
        let lobby = &self.lobby[..self.lobby.len().min(LOBBY_LEN)];
        put_varint(&mut o, lobby.len() as u64);
        o.extend_from_slice(lobby);
        o
    }
}

/// `YYYYMMDD-HHMMSS-<map>.w2rr` (UTC) for a recording that began at `started` unix seconds. The
/// map name is reduced to ASCII letters, digits, space-free `-_()` so it is a safe file name.
pub fn file_name(started: u64, map: &str) -> String {
    let days = (started / 86_400) as i64;
    let rem = started % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let name: String = map
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}-{name}.w2rr",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::file_name;

    #[test]
    fn names_are_dated_and_safe() {
        assert_eq!(file_name(0, "x"), "19700101-000000-x.w2rr");
        // 2023-11-14 22:13:20 UTC
        assert_eq!(
            file_name(1_700_000_000, "(2)Bad/Map: name"),
            "20231114-221320-(2)Bad_Map__name.w2rr"
        );
        // leap day
        assert_eq!(file_name(1_709_164_800, ""), "20240229-000000-.w2rr");
    }
}
