//! Lobby policy (docs/lobby-policy.md): the host can lock team changes and slot swaps, and tells
//! the other players with the `'L'` peer message. This is the pure state and wire format, wire
//! compatible with wc2r-plus `features/lobby_policy.cpp`; the game hooks live in [`hook`].
//!
//! The state is process-global behind a mutex because the game's request handlers and the packet
//! hook reach it without going through the session.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};

use crate::peer::{MAGIC, TYPE};

pub const SUBSYSTEM: u8 = b'L';
pub const PROTOCOL: u8 = 1;
pub const FLAG_TEAMS: u8 = 1 << 0;
pub const FLAG_SLOTS: u8 = 1 << 1;
/// The game's lobby password buffer; a longer one on the wire is refused.
pub const PASSWORD_CAPACITY: usize = 0x18;
/// Net slot of the lobby host.
pub const HOST_NET_SLOT: u32 = 0;
/// A lobby not seen for this long is a new lobby: everything is forgotten.
pub const LOBBY_GAP_MS: u64 = 2000;
/// A lock message counts only while the lobby window was drawn this recently.
pub const IN_LOBBY_MS: u64 = 500;
/// The host repeats the locks this often.
pub const RESEND_MS: u64 = 2000;
const MAX_EVENTS: usize = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Locks {
    pub teams: bool,
    pub slots: bool,
}

/// A lock turning on or off, for the UI to announce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LockEvent {
    /// True for team changes, false for slot swaps.
    pub teams: bool,
    pub locked: bool,
}

/// A whole `'L'` message: the envelope, the protocol, the flags, then the password as a length
/// byte and its bytes. `None` when the password does not fit the game's buffer.
pub fn encode(locks: Locks, password: &[u8]) -> Option<Vec<u8>> {
    if password.len() >= PASSWORD_CAPACITY {
        return None;
    }
    let mut out = Vec::with_capacity(8 + password.len());
    out.push(TYPE);
    out.extend_from_slice(&MAGIC);
    out.push(SUBSYSTEM);
    out.push(PROTOCOL);
    out.push(if locks.teams { FLAG_TEAMS } else { 0 } | if locks.slots { FLAG_SLOTS } else { 0 });
    out.push(password.len() as u8);
    out.extend_from_slice(password);
    Some(out)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub locks: Locks,
    /// Present only when the message carried a well-formed one; unprintable bytes are `?`.
    pub password: Option<Vec<u8>>,
}

/// Parses what follows the envelope. Short or wrong-protocol messages are `None`; a malformed
/// password only costs the password (wc2r-plus does the same).
pub fn decode(body: &[u8]) -> Option<Decoded> {
    if body.len() < 2 || body[0] != PROTOCOL {
        return None;
    }
    let flags = body[1];
    let locks = Locks {
        teams: flags & FLAG_TEAMS != 0,
        slots: flags & FLAG_SLOTS != 0,
    };
    let mut password = None;
    if let Some(&n) = body.get(2) {
        let n = n as usize;
        if n < PASSWORD_CAPACITY && body.len() >= 3 + n {
            password = Some(
                body[3..3 + n]
                    .iter()
                    .map(|&c| if (0x20..0x7f).contains(&c) { c } else { b'?' })
                    .collect(),
            );
        }
    }
    Some(Decoded { locks, password })
}

#[derive(Debug, Default)]
pub struct Policy {
    locks: Locks,
    password: Option<Vec<u8>>,
    events: VecDeque<LockEvent>,
    last_seen: Option<u64>,
    is_host: bool,
    last_send: Option<u64>,
    send_now: bool,
}

impl Policy {
    /// Marks the lobby as seen now. A gap of [`LOBBY_GAP_MS`] since the last time means a new
    /// lobby, so the locks, the password and the events are forgotten first.
    pub fn touch(&mut self, now: u64) {
        if self
            .last_seen
            .is_none_or(|t| now.saturating_sub(t) > LOBBY_GAP_MS)
        {
            *self = Policy::default();
        }
        self.last_seen = Some(now.max(1));
    }

    /// Once per frame the lobby window is drawn. The host's own choice is the lock state; a
    /// guest keeps what the host last told it.
    pub fn frame(&mut self, now: u64, is_host: bool, chosen: Locks) {
        self.touch(now);
        self.is_host = is_host;
        if is_host {
            self.adopt(chosen);
        }
    }

    fn adopt(&mut self, locks: Locks) {
        let before = self.locks;
        self.locks = locks;
        if before.teams != locks.teams {
            self.events.push_back(LockEvent {
                teams: true,
                locked: locks.teams,
            });
        }
        if before.slots != locks.slots {
            self.events.push_back(LockEvent {
                teams: false,
                locked: locks.slots,
            });
        }
        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
        if before != locks {
            self.send_now = true;
        }
    }

    /// The host asks once per frame whether to send the locks now: at once after a change, and
    /// otherwise every [`RESEND_MS`]. Always false for a guest.
    pub fn take_send(&mut self, now: u64) -> bool {
        let due = self.is_host
            && (self.send_now
                || self
                    .last_send
                    .is_none_or(|t| now.saturating_sub(t) >= RESEND_MS));
        if due {
            self.send_now = false;
            self.last_send = Some(now.max(1));
        }
        due
    }

    pub fn is_host(&self) -> bool {
        self.is_host
    }

    /// A guest takes a lock message from the host, if it is in the lobby, is not the host and the
    /// sender is the host. True when it was accepted.
    pub fn receive(&mut self, now: u64, sender: u32, body: &[u8]) -> bool {
        let in_lobby = self
            .last_seen
            .is_some_and(|t| now.saturating_sub(t) <= IN_LOBBY_MS);
        if !in_lobby || self.is_host || sender != HOST_NET_SLOT {
            return false;
        }
        let Some(msg) = decode(body) else {
            return false;
        };
        if msg.password.is_some() {
            self.password = msg.password;
        }
        self.adopt(msg.locks);
        true
    }

    /// A peer's request to change team. The host's own are never refused.
    pub fn allow_team_change(&self, sender: u32) -> bool {
        !(self.locks.teams && sender != HOST_NET_SLOT)
    }

    pub fn allow_slot_swap(&self, sender: u32) -> bool {
        !(self.locks.slots && sender != HOST_NET_SLOT)
    }

    pub fn locks(&self) -> Locks {
        self.locks
    }

    pub fn password(&self) -> Option<&[u8]> {
        self.password.as_deref()
    }

    pub fn next_event(&mut self) -> Option<LockEvent> {
        self.events.pop_front()
    }
}

static POLICY: Mutex<Option<Policy>> = Mutex::new(None);

/// The shared policy, created on first use; a poisoned lock is still usable.
pub fn policy() -> MutexGuard<'static, Option<Policy>> {
    let mut g = POLICY.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(Policy::default());
    }
    g
}

pub fn with<R>(f: impl FnOnce(&mut Policy) -> R) -> R {
    f(policy().as_mut().expect("created"))
}

/// A lock message taken off the wire; `body` is everything after the envelope.
pub fn deliver(sender: u32, body: &[u8]) {
    let now = crate::chat::now_ms();
    with(|p| p.receive(now, sender, body));
}

/// Which lobby slot a name cell label is for: the game draws them as `slotType0` to `slotType7`
/// (wc2r-plus `NameCellSlot`).
pub fn name_cell_slot(label: &[u8]) -> Option<u8> {
    match label.strip_prefix(b"slotType")? {
        [d @ b'0'..=b'7'] => Some(d - b'0'),
        _ => None,
    }
}

/// The lobby password the host currently has, for the lock message. Empty off the game.
#[cfg(not(all(windows, target_arch = "x86")))]
pub fn host_password() -> Vec<u8> {
    Vec::new()
}

#[cfg(all(windows, target_arch = "x86"))]
pub use hook::host_password;

#[cfg(all(windows, target_arch = "x86"))]
pub mod hook {
    //! The game's two lobby request handlers (wc2r-plus `TeamChangeDetour`, `SlotSwapDetour`):
    //! a peer's request is dropped while its lock is on, and everything else goes through.

    use core::ffi::{CStr, c_char, c_void};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

    use super::with;
    use crate::guard::guarded;
    use crate::hooks::{self, Owner};
    use crate::host::Log;
    use crate::pe::Image;
    use crate::symbols::Symbols;

    const TEAM_CHANGE: &str = "HandleLobbyTeamChangeRequest";
    const SLOT_SWAP: &str = "HandleLobbyExchangeTwoSlotsRequest";

    const DROPDOWN: &str = "LobbySlotDropdown";

    type RequestFn = unsafe extern "C" fn(*const u8, i32);
    type DropdownFn = unsafe extern "C" fn(
        i8,
        i8,
        *const c_char,
        *mut c_void,
        *mut c_void,
        *mut i8,
        f32,
        i8,
    ) -> i8;

    static ORIGINAL_DROPDOWN: AtomicUsize = AtomicUsize::new(0);
    static TEAM_OPTIONS: AtomicUsize = AtomicUsize::new(0);
    static PASSWORD: AtomicUsize = AtomicUsize::new(0);

    /// The password in the game's buffer (a NUL-terminated string of at most capacity - 1 bytes).
    pub fn host_password() -> Vec<u8> {
        let at = PASSWORD.load(Ordering::Acquire);
        if at == 0 {
            return Vec::new();
        }
        let buf = unsafe { core::slice::from_raw_parts(at as *const u8, super::PASSWORD_CAPACITY) };
        let n = buf
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(super::PASSWORD_CAPACITY - 1);
        buf[..n.min(super::PASSWORD_CAPACITY - 1)].to_vec()
    }

    static ORIGINAL_TEAM: AtomicUsize = AtomicUsize::new(0);
    static ORIGINAL_SWAP: AtomicUsize = AtomicUsize::new(0);

    pub fn install(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
        let a = hook_one(
            image,
            symbols,
            log,
            TEAM_CHANGE,
            team_change as *mut c_void,
            &ORIGINAL_TEAM,
        );
        let b = hook_one(
            image,
            symbols,
            log,
            SLOT_SWAP,
            slot_swap as *mut c_void,
            &ORIGINAL_SWAP,
        );
        PASSWORD.store(
            symbols.address("LobbyPassword").unwrap_or(0),
            Ordering::Release,
        );
        TEAM_OPTIONS.store(
            symbols.address("MpLobbyTeamOptions").unwrap_or(0),
            Ordering::Release,
        );
        let c = hook_one(
            image,
            symbols,
            log,
            DROPDOWN,
            dropdown as *mut c_void,
            &ORIGINAL_DROPDOWN,
        );
        a && b && c
    }

    fn hook_one(
        image: &Image,
        symbols: &Symbols,
        log: &dyn Log,
        name: &str,
        detour: *mut c_void,
        original: &'static AtomicUsize,
    ) -> bool {
        let Some(target) = symbols.address(name) else {
            log.line(
                W2R_LOG_ERROR,
                "host",
                &format!("{name} is not in the symbol table"),
            );
            return false;
        };
        // SAFETY: both detours match the handlers' cdecl `(packet, net slot)` signature.
        let result = unsafe {
            hooks::global().install(
                Owner::Host,
                image,
                target,
                detour,
                &format!("host:{name}"),
                |o| original.store(o as usize, Ordering::Release),
            )
        };
        match result {
            Ok(_) => {
                log.line(
                    W2R_LOG_INFO,
                    "host",
                    &format!("hooked {name} @ {target:#010x} (lobby locks)"),
                );
                true
            }
            Err(e) => {
                log.line(
                    W2R_LOG_ERROR,
                    "host",
                    &format!("could not hook {name}: {e:?}"),
                );
                false
            }
        }
    }

    fn refuse(sender: i32, what: &str, allowed: impl FnOnce(&super::Policy, u32) -> bool) -> bool {
        let now = crate::chat::now_ms();
        let sender = sender.max(0) as u32;
        let ok = with(|p| {
            p.touch(now);
            allowed(p, sender)
        });
        if !ok && let Some(s) = crate::entry::startup() {
            s.log.line(
                W2R_LOG_INFO,
                "lobbypolicy",
                &format!("refused a {what} from net slot {sender} (locked)"),
            );
        }
        !ok
    }

    /// Guests see the team dropdown and their own name cell disabled while the host has them
    /// locked; the host's team dropdown is never disabled (wc2r-plus `DropdownDetour`). Host menus
    /// come with slice 3.
    unsafe extern "C" fn dropdown(
        mut disabled: i8,
        prev: i8,
        label: *const c_char,
        value: *mut c_void,
        options: *mut c_void,
        changed: *mut i8,
        row_h: f32,
        localize: i8,
    ) -> i8 {
        let _ = guarded(|| {
            let (is_host, locks) = with(|p| {
                p.touch(crate::chat::now_ms());
                (p.is_host(), p.locks())
            });
            let team_options = TEAM_OPTIONS.load(Ordering::Acquire);
            if team_options != 0 && options as usize == team_options {
                if is_host {
                    disabled = 0;
                } else if locks.teams {
                    disabled = 1;
                }
            }
            if disabled == 0 && !is_host && locks.slots && !label.is_null() {
                let label = unsafe { CStr::from_ptr(label) }.to_bytes();
                if super::name_cell_slot(label).is_some() {
                    disabled = 1;
                }
            }
        });
        let original: DropdownFn =
            unsafe { core::mem::transmute(ORIGINAL_DROPDOWN.load(Ordering::Acquire)) };
        unsafe {
            original(
                disabled, prev, label, value, options, changed, row_h, localize,
            )
        }
    }

    unsafe extern "C" fn team_change(packet: *const u8, sender: i32) {
        // A host fault means "do not refuse": the lobby keeps working without the lock.
        let refused = guarded(|| refuse(sender, "team change", |p, s| p.allow_team_change(s)))
            .unwrap_or(false);
        if refused {
            return;
        }
        let original: RequestFn =
            unsafe { core::mem::transmute(ORIGINAL_TEAM.load(Ordering::Acquire)) };
        unsafe { original(packet, sender) };
    }

    unsafe extern "C" fn slot_swap(packet: *const u8, sender: i32) {
        let refused =
            guarded(|| refuse(sender, "slot swap", |p, s| p.allow_slot_swap(s))).unwrap_or(false);
        if refused {
            return;
        }
        let original: RequestFn =
            unsafe { core::mem::transmute(ORIGINAL_SWAP.load(Ordering::Acquire)) };
        unsafe { original(packet, sender) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTH: Locks = Locks {
        teams: true,
        slots: true,
    };

    #[test]
    fn messages_round_trip_and_match_the_wc2r_plus_layout() {
        let m = encode(BOTH, b"pw").unwrap();
        assert_eq!(&m[..5], &[0xE7, b'W', b'2', b'R', b'L']);
        assert_eq!(&m[5..], &[1, 0b11, 2, b'p', b'w']);
        let d = decode(&m[5..]).unwrap();
        assert_eq!(d.locks, BOTH);
        assert_eq!(d.password.as_deref(), Some(&b"pw"[..]));
        let none = encode(Locks::default(), b"").unwrap();
        assert_eq!(&none[5..], &[1, 0, 0]);
        assert_eq!(
            decode(&none[5..]).unwrap().password.as_deref(),
            Some(&b""[..])
        );
        assert!(encode(BOTH, &[b'a'; PASSWORD_CAPACITY]).is_none());
        assert!(encode(BOTH, &[b'a'; PASSWORD_CAPACITY - 1]).is_some());
    }

    #[test]
    fn short_or_foreign_messages_are_ignored_and_a_bad_password_costs_only_itself() {
        assert_eq!(decode(&[]), None);
        assert_eq!(decode(&[1]), None);
        assert_eq!(decode(&[2, 3, 0]), None, "other protocol");
        let flags_only = decode(&[1, 1]).unwrap();
        assert!(flags_only.locks.teams && !flags_only.locks.slots);
        assert_eq!(flags_only.password, None);
        assert_eq!(
            decode(&[1, 0, 5, b'a']).unwrap().password,
            None,
            "truncated"
        );
        assert_eq!(
            decode(&[1, 0, PASSWORD_CAPACITY as u8]).unwrap().password,
            None,
            "too long"
        );
        let odd = decode(&[1, 0, 3, b'a', 0x01, 0xff]).unwrap();
        assert_eq!(odd.password.as_deref(), Some(&b"a??"[..]));
    }

    #[test]
    fn the_host_enforces_its_own_choice_and_never_refuses_itself() {
        let mut p = Policy::default();
        p.frame(
            100,
            true,
            Locks {
                teams: true,
                slots: false,
            },
        );
        assert!(!p.allow_team_change(1));
        assert!(p.allow_team_change(0), "the host may always");
        assert!(p.allow_slot_swap(1));
        p.frame(200, true, BOTH);
        assert!(!p.allow_slot_swap(3));
        p.frame(300, true, Locks::default());
        assert!(p.allow_team_change(1) && p.allow_slot_swap(1));
    }

    #[test]
    fn lock_changes_become_events_once() {
        let mut p = Policy::default();
        p.frame(0, true, Locks::default());
        assert_eq!(p.next_event(), None);
        p.frame(10, true, BOTH);
        p.frame(20, true, BOTH);
        assert_eq!(
            p.next_event(),
            Some(LockEvent {
                teams: true,
                locked: true
            })
        );
        assert_eq!(
            p.next_event(),
            Some(LockEvent {
                teams: false,
                locked: true
            })
        );
        assert_eq!(p.next_event(), None);
        for i in 0..40 {
            p.frame(
                30 + i,
                true,
                Locks {
                    teams: i % 2 == 0,
                    slots: false,
                },
            );
        }
        let mut n = 0;
        while p.next_event().is_some() {
            n += 1;
        }
        assert!(n <= MAX_EVENTS);
    }

    #[test]
    fn a_guest_accepts_locks_only_from_the_host_inside_the_lobby() {
        let msg = encode(BOTH, b"pw").unwrap();
        let body = &msg[5..];
        let mut p = Policy::default();
        p.frame(1000, false, Locks::default());
        assert!(!p.receive(1000, 2, body), "not from the host");
        assert!(p.allow_team_change(2));
        assert!(
            !p.receive(1000 + IN_LOBBY_MS + 1, 0, body),
            "lobby not drawn lately"
        );
        assert!(p.receive(1100, 0, body));
        assert!(!p.allow_team_change(2));
        assert!(!p.allow_slot_swap(2));
        assert_eq!(p.password(), Some(&b"pw"[..]));
        assert!(!p.receive(1100, 0, &[9]), "bad message");

        let mut host = Policy::default();
        host.frame(1000, true, Locks::default());
        assert!(
            !host.receive(1000, 0, body),
            "the host takes no locks from the wire"
        );
        assert!(host.allow_team_change(1));
    }

    #[test]
    fn the_host_sends_on_change_and_then_every_two_seconds_and_a_guest_never() {
        let mut p = Policy::default();
        p.frame(1000, true, Locks::default());
        assert!(p.take_send(1000), "first frame");
        assert!(!p.take_send(1500));
        p.frame(1600, true, BOTH);
        assert!(p.take_send(1600), "a change goes out at once");
        assert!(!p.take_send(3599));
        assert!(p.take_send(3600), "resend");
        let mut g = Policy::default();
        g.frame(0, false, BOTH);
        assert!(!g.take_send(10_000));
    }

    #[test]
    fn name_cells_are_slot_type_zero_to_seven() {
        assert_eq!(name_cell_slot(b"slotType0"), Some(0));
        assert_eq!(name_cell_slot(b"slotType7"), Some(7));
        assert_eq!(name_cell_slot(b"slotType8"), None);
        assert_eq!(name_cell_slot(b"slotType10"), None);
        assert_eq!(name_cell_slot(b"slotType"), None);
        assert_eq!(name_cell_slot(b"team3"), None);
    }

    #[test]
    fn a_new_lobby_forgets_everything() {
        let msg = encode(BOTH, b"pw").unwrap();
        let mut p = Policy::default();
        p.frame(0, false, Locks::default());
        assert!(p.receive(0, 0, &msg[5..]));
        p.frame(LOBBY_GAP_MS, false, Locks::default());
        assert!(!p.allow_team_change(1), "a short gap is the same lobby");
        p.frame(LOBBY_GAP_MS * 2 + 1, false, Locks::default());
        assert!(p.allow_team_change(1));
        assert_eq!(p.password(), None);
        assert_eq!(p.next_event(), None, "events were dropped too");
    }
}
