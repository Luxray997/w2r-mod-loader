//! The private fog maps kept for replay viewing, one set per player (see `fogtrack.rs`). Pure
//! data, no game access, so it is tested without the game.
//!
//! The game's three fog maps are 16 KB each: what is in sight now (`current`, 0 lit .. 0x10 black),
//! what has been explored (`remembered`, same scale) and, per tile, a bit per player that is in
//! the dark there (`dark`).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Size of each fog map.
pub const MAP_LEN: usize = 0x4000;
pub const PLAYERS: usize = 8;
/// What the game's own reset writes: black, and "in the dark for everyone".
const BLACK: u8 = 0x10;
const DARK_BITS: u8 = 0xff;

static ARMED: AtomicBool = AtomicBool::new(false);
static MAPS: Mutex<Option<Vec<Maps>>> = Mutex::new(None);

/// One player's maps. The vectors are never resized, so their addresses stay valid while armed.
pub struct Maps {
    pub current: Vec<u8>,
    pub remembered: Vec<u8>,
    pub dark: Vec<u8>,
}

impl Maps {
    fn fresh() -> Maps {
        Maps {
            current: vec![BLACK; MAP_LEN],
            remembered: vec![BLACK; MAP_LEN],
            dark: vec![DARK_BITS; MAP_LEN],
        }
    }
}

/// A replay is about to start: every player's maps begin as the game's own reset leaves them.
pub fn arm() {
    if let Ok(mut m) = MAPS.lock() {
        *m = Some((0..PLAYERS).map(|_| Maps::fresh()).collect());
    }
    ARMED.store(true, Ordering::Release);
}

pub fn disarm() {
    ARMED.store(false, Ordering::Release);
    if let Ok(mut m) = MAPS.lock() {
        *m = None;
    }
}

pub fn armed() -> bool {
    ARMED.load(Ordering::Acquire)
}

/// Copies `player`'s private maps into the given buffers (the real maps, for drawing). False if
/// not armed or there is no such player.
pub fn copy_into(player: u8, current: &mut [u8], remembered: &mut [u8]) -> bool {
    let Ok(guard) = MAPS.lock() else {
        return false;
    };
    let Some(maps) = guard.as_ref().and_then(|v| v.get(usize::from(player))) else {
        return false;
    };
    let n = current.len().min(MAP_LEN);
    current[..n].copy_from_slice(&maps.current[..n]);
    let n = remembered.len().min(MAP_LEN);
    remembered[..n].copy_from_slice(&maps.remembered[..n]);
    true
}

/// The address of `player`'s private "dark" map (valid until `disarm`), if armed.
pub fn dark_ptr(player: u8) -> Option<usize> {
    let guard = MAPS.lock().ok()?;
    let maps = guard.as_ref()?.get(usize::from(player))?;
    Some(maps.dark.as_ptr() as usize)
}

/// The players whose private maps should follow a game call, with the addresses of their three
/// maps `[current, remembered, dark]`: everyone but `local` (whose maps are the real ones) in a
/// slot that is not empty. The addresses stay valid until `disarm`.
pub fn targets(local: u8, is_empty: impl Fn(usize) -> bool) -> Vec<(u8, [usize; 3])> {
    if !armed() {
        return Vec::new();
    }
    let Ok(guard) = MAPS.lock() else {
        return Vec::new();
    };
    let Some(all) = guard.as_ref() else {
        return Vec::new();
    };
    all.iter()
        .enumerate()
        .filter(|(p, _)| *p != usize::from(local) && !is_empty(*p))
        .map(|(p, m)| {
            (
                p as u8,
                [
                    m.current.as_ptr() as usize,
                    m.remembered.as_ptr() as usize,
                    m.dark.as_ptr() as usize,
                ],
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The state is global, so the cases run in one test.
    #[test]
    fn arm_copy_targets_and_disarm() {
        disarm();
        assert!(!armed());
        assert!(!copy_into(1, &mut [0; 8], &mut [0; 8]), "not armed");
        assert!(targets(0, |_| false).is_empty());

        arm();
        assert!(armed());
        let (mut cur, mut rem) = (vec![0u8; 16], vec![0u8; 16]);
        assert!(copy_into(3, &mut cur, &mut rem));
        assert!(cur.iter().chain(&rem).all(|&v| v == BLACK));
        assert!(!copy_into(8, &mut cur, &mut rem), "there is no player 8");
        assert!(dark_ptr(2).is_some() && dark_ptr(8).is_none());

        // not the local player, not empty slots
        let t = targets(0, |p| p >= 3);
        assert_eq!(t.iter().map(|(p, _)| *p).collect::<Vec<_>>(), [1, 2]);
        let [c, r, d] = t[0].1;
        assert!(c != r && r != d && c != d, "three separate buffers");
        // the buffers are the ones copy_into reads
        // SAFETY: the maps are alive while armed.
        unsafe { *(c as *mut u8) = 0x42 };
        let (mut cur, mut rem) = (vec![0u8; 4], vec![0u8; 4]);
        assert!(copy_into(1, &mut cur, &mut rem));
        assert_eq!(cur[0], 0x42);

        disarm();
        assert!(!armed());
        assert!(!copy_into(3, &mut cur, &mut rem));
        assert!(targets(0, |_| false).is_empty());
        assert!(dark_ptr(2).is_none());
    }
}
