//! The observer team's gate (docs/observer-research.md). Team 9 ("OBS") only works when every
//! machine in the game runs the observer code, so it is offered, and accepted, only while every
//! other player in the lobby has announced observer support (`Handshake::everyone_has`: a W2R Mod Loader
//! with the Observer plugin loaded; their other plugins may differ). The decision is recomputed
//! every lobby frame:
//!
//! - the OBS entry is shown in the team dropdown only while it is allowed (the list's end pointer
//!   moves between eight and nine entries);
//! - the host drops a team change to 9 while it is not allowed, and puts anyone already on team 9
//!   back on team 1 (a player without the plugin joined, say) and tells the lobby.
//!
//! A player without the plugin never sees team 9, which their dropdown could not display.

use std::sync::atomic::{AtomicBool, Ordering};

/// The lobby team value that marks an observer.
pub const OBSERVER_TEAM: u8 = 9;
static ALLOWED: AtomicBool = AtomicBool::new(false);
/// Whether this machine is an observer in the match that is running (set when it starts).
static LOCAL_OBSERVER: AtomicBool = AtomicBool::new(false);

/// Whether the local player watches the match that is running instead of playing it.
pub fn local_is_observer() -> bool {
    LOCAL_OBSERVER.load(Ordering::Relaxed)
}

pub fn set_local_observer(on: bool) {
    LOCAL_OBSERVER.store(on, Ordering::Relaxed);
}

/// The people on the observer team in the match that is running (bit n: player n), read from the
/// lobby when it starts.
static MATCH_OBSERVERS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn match_observers() -> u8 {
    MATCH_OBSERVERS.load(Ordering::Acquire)
}

#[cfg_attr(not(target_arch = "x86"), allow(dead_code))]
pub fn set_match_observers(mask: u8) {
    MATCH_OBSERVERS.store(mask, Ordering::Release);
}

/// Whether team 9 may be used right now.
pub fn allowed() -> bool {
    ALLOWED.load(Ordering::Relaxed)
}

/// Whether a team-change request for `team` must be dropped.
pub fn refuses_team(team: u8) -> bool {
    team == OBSERVER_TEAM && !allowed()
}

/// The slots (0-7) whose team is 9 in the given team bytes.
pub fn observer_slots(teams: &[u8; 8]) -> impl Iterator<Item = usize> + '_ {
    (0..8).filter(|&i| teams[i] == OBSERVER_TEAM)
}

#[cfg(all(windows, target_arch = "x86"))]
mod game {
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::guard::guarded;

    /// The team a removed observer goes back to.
    const FALLBACK_TEAM: u8 = 1;

    /// `MpLobbySlots`: stride and where the team byte sits.
    const SLOT_STRIDE: usize = 0x26;
    const SLOT_TEAM: usize = 0x0a;

    type BroadcastFn = unsafe extern "C" fn(*const u8);

    static VECTOR: AtomicUsize = AtomicUsize::new(0);
    static FIRST: AtomicUsize = AtomicUsize::new(0);
    static SLOTS: AtomicUsize = AtomicUsize::new(0);
    static BROADCAST: AtomicUsize = AtomicUsize::new(0);

    /// Whether the OBS team entry was added, which is when this player announces observer support.
    pub fn available() -> bool {
        VECTOR.load(Ordering::Acquire) != 0
    }

    /// Remembers where the extended team option list and the slot table are.
    pub fn installed(vector: usize, first: usize, slots: usize, broadcast: usize) {
        VECTOR.store(vector, Ordering::Release);
        FIRST.store(first, Ordering::Release);
        SLOTS.store(slots, Ordering::Release);
        BROADCAST.store(broadcast, Ordering::Release);
    }

    /// Every lobby frame: `everyone` is the handshake's answer, `host_connected` whether the local
    /// player hosts a connected lobby. Does nothing unless the OBS entry was added.
    pub fn frame(everyone: bool, host_connected: bool) {
        let vector = VECTOR.load(Ordering::Acquire);
        if vector == 0 {
            ALLOWED.store(false, Ordering::Relaxed);
            return;
        }
        let was = ALLOWED.swap(everyone, Ordering::Relaxed);
        let _ = guarded(|| unsafe {
            // The list the game and our screen read: eight entries, or nine while allowed.
            let first = FIRST.load(Ordering::Acquire) as *const *const core::ffi::c_char;
            let count = if everyone { 9 } else { 8 };
            (vector as *mut *const *const core::ffi::c_char)
                .add(1)
                .write(first.add(count));
            if !host_connected || everyone {
                return;
            }
            let slots = SLOTS.load(Ordering::Acquire) as *mut u8;
            let broadcast = BROADCAST.load(Ordering::Acquire);
            if slots.is_null() || broadcast == 0 {
                return;
            }
            let broadcast: BroadcastFn = core::mem::transmute(broadcast);
            let mut moved = 0;
            for i in 0..8 {
                let slot = slots.add(i * SLOT_STRIDE);
                if *slot.add(SLOT_TEAM) == OBSERVER_TEAM {
                    *slot.add(SLOT_TEAM) = FALLBACK_TEAM;
                    broadcast(slot);
                    moved += 1;
                }
            }
            if moved > 0 || was {
                crate::session::log_info(
                    "observer",
                    "OBS is off: not every player has the Observer plugin",
                );
            }
            if moved > 0 {
                crate::lobbyscreen::push_notice(
                    "Observer team removed: not every player has the Observer plugin".to_owned(),
                );
            }
        });
    }
}

#[cfg(all(windows, target_arch = "x86"))]
pub use game::{available, frame, installed};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_nine_is_refused_unless_allowed() {
        ALLOWED.store(false, Ordering::Relaxed);
        assert!(refuses_team(9));
        assert!(!refuses_team(8));
        assert!(!refuses_team(1));
        ALLOWED.store(true, Ordering::Relaxed);
        assert!(!refuses_team(9));
        ALLOWED.store(false, Ordering::Relaxed);
    }

    #[test]
    fn observer_slots_lists_team_nine_only() {
        let teams = [1, 9, 2, 9, 0, 0, 0, 0];
        assert_eq!(observer_slots(&teams).collect::<Vec<_>>(), vec![1, 3]);
    }
}
