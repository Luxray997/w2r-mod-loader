//! Peer messages (docs/abi.md, "Peer messages"): the envelope every host-level message shares,
//! the plugin subsystem's tag routing, the limits, and the game's transport behind traits so
//! the rules are testable without the game. Receiving goes through the host's hook on the
//! game's `GetNextPeerPacket` (`hook` module, Windows): our messages are consumed before the
//! game sees them; everything else passes through, except the stock `0x2A` legacy file
//! transfer, which wc2r-plus drops and so do we.
//!
//! Subsystem `'H'` is the host's own handshake (`handshake` module); `'M'` and `'L'` are
//! reserved for the other first-party features (Phase 3). Only `'P'` reaches plugins, routed by
//! the 4-byte tag that is SHA-256 of the plugin id.

use std::collections::VecDeque;

use sha2::{Digest, Sha256};
use w2r_abi::{W2R_MAX_PEER_PAYLOAD, W2R_PEER_HEADER_BYTES, W2R_SLOT_ALL};

pub const TYPE: u8 = 0xE7;
pub const MAGIC: [u8; 3] = *b"W2R";
/// type + magic + subsystem.
pub const BASE_HEADER_LEN: usize = 5;
pub const SUBSYSTEM_PLUGIN: u8 = b'P';
pub const SUBSYSTEM_HANDSHAKE: u8 = b'H';
pub const RESERVED_SUBSYSTEMS: [u8; 3] = *b"HML";
/// The plugin header: base header + 4-byte tag.
pub const HEADER_LEN: usize = W2R_PEER_HEADER_BYTES as usize;
pub const MAX_PAYLOAD: usize = W2R_MAX_PEER_PAYLOAD as usize;
pub const MAX_MESSAGE: usize = HEADER_LEN + MAX_PAYLOAD;
/// The game's own limit for its reliable channel.
pub const GAME_LIMIT: usize = 989;
pub const PER_PLUGIN_PER_SECOND: usize = 32;
pub const OVERALL_PER_SECOND: usize = 128;
/// W2R messages accepted from one net slot per second; the rest are dropped on receipt, so a
/// hostile peer cannot drive plugin callbacks at wire rate. Matches what a well-behaved host
/// may send in total.
pub const INBOUND_PER_SENDER_PER_SECOND: usize = OVERALL_PER_SECOND;
/// Our messages consumed in one `GetNextPeerPacket` call before giving the game a turn.
pub const MAX_CONSUMED_PER_CALL: usize = 64;
/// The game's legacy file-transfer message, dropped on receipt.
pub const STOCK_FILE_TRANSFER: u8 = 0x2A;
pub const MAX_SLOTS: u32 = 8;

const _: () = assert!(HEADER_LEN == BASE_HEADER_LEN + 4);
const _: () = assert!(MAX_MESSAGE <= GAME_LIMIT);

/// First 4 bytes of SHA-256 of the UTF-8 id: the plugin's tag, the same on every machine.
pub fn peer_tag(id: &str) -> [u8; 4] {
    let digest = Sha256::digest(id.as_bytes());
    [digest[0], digest[1], digest[2], digest[3]]
}

/// Builds a plugin message. `None` when the payload is over the limit.
pub fn encode(tag: [u8; 4], payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() > MAX_PAYLOAD {
        return None;
    }
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.push(TYPE);
    out.extend_from_slice(&MAGIC);
    out.push(SUBSYSTEM_PLUGIN);
    out.extend_from_slice(&tag);
    out.extend_from_slice(payload);
    Some(out)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Packet<'a> {
    /// The game's own traffic; hand it back untouched.
    NotOurs,
    /// A W2R message: subsystem byte and everything after the base header.
    Ours { subsystem: u8, body: &'a [u8] },
}

pub fn classify(buf: &[u8]) -> Packet<'_> {
    if buf.len() >= BASE_HEADER_LEN && buf[0] == TYPE && buf[1..4] == MAGIC {
        Packet::Ours {
            subsystem: buf[4],
            body: &buf[BASE_HEADER_LEN..],
        }
    } else {
        Packet::NotOurs
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PluginMessage<'a> {
    pub tag: [u8; 4],
    pub payload: &'a [u8],
}

/// The body of a `'P'` message: tag then payload (at most `MAX_PAYLOAD`).
pub fn plugin_message(body: &[u8]) -> Option<PluginMessage<'_>> {
    if body.len() < 4 || body.len() - 4 > MAX_PAYLOAD {
        return None;
    }
    Some(PluginMessage {
        tag: body[..4].try_into().unwrap(),
        payload: &body[4..],
    })
}

/// At most `max` events in any `window_ms`.
#[derive(Debug)]
pub struct SlidingWindow {
    max: usize,
    window_ms: u64,
    sent: VecDeque<u64>,
}

impl SlidingWindow {
    pub const fn new(max: usize, window_ms: u64) -> Self {
        SlidingWindow {
            max,
            window_ms,
            sent: VecDeque::new(),
        }
    }

    /// Whether one more event fits now, without recording it. Callers that can still fail
    /// after the check (a send) use this first and [`SlidingWindow::allow`] once it succeeded,
    /// so a refused call costs no budget (docs/abi.md: "the call had no effect").
    pub fn would_allow(&mut self, now: u64) -> bool {
        self.prune(now);
        self.sent.len() < self.max
    }

    /// Records one event if it fits now.
    pub fn allow(&mut self, now: u64) -> bool {
        if !self.would_allow(now) {
            return false;
        }
        self.sent.push_back(now);
        true
    }

    fn prune(&mut self, now: u64) {
        while self
            .sent
            .front()
            .is_some_and(|&t| now.saturating_sub(t) >= self.window_ms)
        {
            self.sent.pop_front();
        }
    }
}

/// The game's reliable peer channel, plus the facts about the net session the handshake
/// needs. The defaults say "cannot tell", which keeps the handshake quiet.
pub trait Transport {
    fn connected(&self) -> bool;
    fn broadcast(&self, msg: &[u8]) -> bool;
    fn send_to(&self, slot: u32, msg: &[u8]) -> bool;
    /// Whether the local player hosts the session (net slot 0).
    fn is_host(&self) -> bool {
        false
    }
    /// The net slots the transport currently knows, when it can list them.
    fn peers(&self) -> Option<Vec<u32>> {
        None
    }
    /// Lobby slot (0-7) per net slot, when readable.
    fn lobby_slots(&self) -> Option<[Option<u8>; 8]> {
        None
    }
    /// Removes a player from the lobby (host only). False when the game refused.
    fn kick(&self, _slot: u32) -> bool {
        false
    }
}

/// Which net slots announced the handshake; the `handshake` module is the live source.
pub trait Roster {
    fn announced(&self, slot: u32) -> bool;
    fn any_announced(&self) -> bool;
}

/// Whether the local player is in a lobby.
pub trait Presence {
    fn in_lobby(&self) -> bool;
}

pub struct NoTransport;
impl Transport for NoTransport {
    fn connected(&self) -> bool {
        false
    }
    fn broadcast(&self, _: &[u8]) -> bool {
        false
    }
    fn send_to(&self, _: u32, _: &[u8]) -> bool {
        false
    }
}

pub struct NoPeers;
impl Roster for NoPeers {
    fn announced(&self, _: u32) -> bool {
        false
    }
    fn any_announced(&self) -> bool {
        false
    }
}

pub struct Nowhere;
impl Presence for Nowhere {
    fn in_lobby(&self) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    All,
    Slot(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetError {
    /// Slot number out of range.
    Invalid,
    /// The slot (or, for `All`, no slot at all) announced the handshake.
    NotAnnounced,
}

/// Resolves a `peer_send` target against the roster.
pub fn resolve_target(roster: &dyn Roster, slot_or_all: u32) -> Result<Target, TargetError> {
    if slot_or_all == W2R_SLOT_ALL {
        if roster.any_announced() {
            Ok(Target::All)
        } else {
            Err(TargetError::NotAnnounced)
        }
    } else if slot_or_all >= MAX_SLOTS {
        Err(TargetError::Invalid)
    } else if roster.announced(slot_or_all) {
        Ok(Target::Slot(slot_or_all))
    } else {
        Err(TargetError::NotAnnounced)
    }
}

/// The game's net session and its transport vtable, as wc2r-plus reads them (x86 only).
#[cfg(all(windows, target_arch = "x86"))]
pub mod game {
    use super::*;
    use crate::guard::guarded;
    use crate::symbols::Symbols;
    use core::ffi::c_void;

    const SESSION_IS_HOST: usize = 0x18;
    const SESSION_CONNECTED: usize = 0x19;
    const SESSION_TRANSPORT: usize = 0x68;
    const VT_SEND_TO_PEER: usize = 0x3C / 4;
    const VT_BROADCAST: usize = 0x40 / 4;
    const VT_ENUM_PEERS: usize = 0x48 / 4;
    /// MSVC frees small blocks through the sized `operator delete`; wc2r-plus frees the peer
    /// list only below this capacity, and so do we (a larger one is left to leak).
    const FREE_BELOW: usize = 0x1000;

    type BroadcastFn = unsafe extern "thiscall" fn(*mut c_void, *const u8, i32) -> i32;
    type SendToFn = unsafe extern "thiscall" fn(*mut c_void, i32, *const u8, i32) -> i32;
    /// Fills a `std::vector<uint8_t>` of net slots that the caller must free.
    type EnumPeersFn = unsafe extern "thiscall" fn(*mut c_void, *mut ByteVec) -> *mut ByteVec;
    /// The game's `operator delete(void*, size_t)`.
    type OperatorDeleteFn = unsafe extern "C" fn(*mut c_void, usize);
    /// `KickLobbyPeer(netSlot, reason)`; wc2r-plus passes reason 0.
    type KickFn = unsafe extern "C" fn(u32, u8);

    /// MSVC `std::vector<uint8_t>` as the game lays it out.
    #[repr(C)]
    struct ByteVec {
        begin: *mut u8,
        end: *mut u8,
        cap: *mut u8,
    }

    pub struct GameTransport {
        /// `NetSession`: a global holding the session pointer.
        session_slot: *const *const u8,
        operator_delete: OperatorDeleteFn,
        kick: KickFn,
        /// `LobbySlotByNetSlot`: 8 u32s, lobby slot in the low byte.
        lobby_slot_by_net_slot: *const u32,
    }

    impl GameTransport {
        pub fn new(symbols: &Symbols) -> Option<GameTransport> {
            Some(GameTransport {
                session_slot: symbols.address("NetSession")? as *const *const u8,
                operator_delete: unsafe {
                    core::mem::transmute::<usize, OperatorDeleteFn>(
                        symbols.address("GameOperatorDelete")?,
                    )
                },
                kick: unsafe {
                    core::mem::transmute::<usize, KickFn>(symbols.address("KickLobbyPeer")?)
                },
                lobby_slot_by_net_slot: symbols.address("LobbySlotByNetSlot")? as *const u32,
            })
        }

        /// Session and transport pointers, or `None` when either is null. Reads under the guard.
        fn transport(&self) -> Option<(*const u8, *mut c_void)> {
            guarded(|| unsafe {
                let session = *self.session_slot;
                if session.is_null() {
                    return None;
                }
                let transport = *(session.add(SESSION_TRANSPORT) as *const *mut c_void);
                if transport.is_null() {
                    None
                } else {
                    Some((session, transport))
                }
            })
            .ok()
            .flatten()
        }

        fn vtable_entry(transport: *mut c_void, index: usize) -> usize {
            unsafe { *(*(transport as *const *const usize)).add(index) }
        }
    }

    impl Transport for GameTransport {
        fn connected(&self) -> bool {
            self.transport()
                .and_then(|(session, _)| {
                    guarded(|| unsafe { *session.add(SESSION_CONNECTED) != 0 }).ok()
                })
                .unwrap_or(false)
        }

        fn broadcast(&self, msg: &[u8]) -> bool {
            let Some((_, transport)) = self.transport() else {
                return false;
            };
            guarded(|| unsafe {
                let f: BroadcastFn =
                    core::mem::transmute(Self::vtable_entry(transport, VT_BROADCAST));
                f(transport, msg.as_ptr(), msg.len() as i32);
            })
            .is_ok()
        }

        fn send_to(&self, slot: u32, msg: &[u8]) -> bool {
            let Some((_, transport)) = self.transport() else {
                return false;
            };
            guarded(|| unsafe {
                let f: SendToFn =
                    core::mem::transmute(Self::vtable_entry(transport, VT_SEND_TO_PEER));
                f(transport, slot as i32, msg.as_ptr(), msg.len() as i32);
            })
            .is_ok()
        }

        fn is_host(&self) -> bool {
            self.transport()
                .and_then(|(session, _)| {
                    guarded(|| unsafe { *session.add(SESSION_IS_HOST) != 0 }).ok()
                })
                .unwrap_or(false)
        }

        fn peers(&self) -> Option<Vec<u32>> {
            let (_, transport) = self.transport()?;
            guarded(|| unsafe {
                let f: EnumPeersFn =
                    core::mem::transmute(Self::vtable_entry(transport, VT_ENUM_PEERS));
                let mut v = ByteVec {
                    begin: core::ptr::null_mut(),
                    end: core::ptr::null_mut(),
                    cap: core::ptr::null_mut(),
                };
                f(transport, &mut v);
                let mut out = Vec::new();
                if !v.begin.is_null() && v.end > v.begin {
                    let n = (v.end as usize - v.begin as usize).min(MAX_SLOTS as usize * 4);
                    out.extend(
                        core::slice::from_raw_parts(v.begin, n)
                            .iter()
                            .map(|&b| b as u32),
                    );
                }
                if !v.begin.is_null() {
                    let capacity = v.cap as usize - v.begin as usize;
                    if capacity < FREE_BELOW {
                        (self.operator_delete)(v.begin.cast(), capacity);
                    }
                }
                out
            })
            .ok()
        }

        fn lobby_slots(&self) -> Option<[Option<u8>; 8]> {
            guarded(|| unsafe {
                let mut out = [None; 8];
                for (i, slot) in out.iter_mut().enumerate() {
                    let l = (*self.lobby_slot_by_net_slot.add(i) & 0xff) as u8;
                    *slot = (l < 8).then_some(l);
                }
                out
            })
            .ok()
        }

        fn kick(&self, slot: u32) -> bool {
            if slot >= MAX_SLOTS || !self.is_host() {
                return false;
            }
            guarded(|| unsafe { (self.kick)(slot, 0) }).is_ok()
        }
    }
}

/// The host's hook on `GetNextPeerPacket`: consumes W2R messages and hands plugin messages to
/// the session; drops the stock file transfer; passes everything else to the game.
#[cfg(windows)]
pub mod hook {
    use super::*;
    use crate::guard::guarded;
    use crate::hooks::{self, Owner};
    use crate::host::Log;
    use crate::pe::Image;
    use crate::symbols::Symbols;
    use core::ffi::c_void;
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
    use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

    pub const SYMBOL: &str = "GetNextPeerPacket";
    const DROP_LOG_EVERY: u32 = 100;

    type GetNextPeerPacketFn = unsafe extern "C" fn(*mut u32, *mut *mut u8, *mut i32) -> i32;

    static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
    static DROPPED: [AtomicU32; 9] = [const { AtomicU32::new(0) }; 9];
    static FLOODED: [AtomicU32; 9] = [const { AtomicU32::new(0) }; 9];
    /// Per net slot (0-7, 8 for anything else). Locked only around `allow`, never across
    /// anything that can fault.
    static INBOUND: std::sync::Mutex<[SlidingWindow; 9]> = std::sync::Mutex::new(
        [const { SlidingWindow::new(INBOUND_PER_SENDER_PER_SECOND, 1000) }; 9],
    );
    const FLOOD_LOG_EVERY: u32 = 1000;

    pub fn install(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
        let Some(target) = symbols.address(SYMBOL) else {
            log.line(
                W2R_LOG_ERROR,
                "host",
                &format!("{SYMBOL} is not in the symbol table"),
            );
            return false;
        };
        // SAFETY: `detour` matches GetNextPeerPacket's cdecl signature from the symbol table.
        let result = unsafe {
            hooks::global().install(
                Owner::Host,
                image,
                target,
                detour as *mut c_void,
                &format!("host:{SYMBOL}"),
                |original| ORIGINAL.store(original as usize, Ordering::Release),
            )
        };
        match result {
            Ok(_) => {
                log.line(
                    W2R_LOG_INFO,
                    "host",
                    &format!("hooked {SYMBOL} @ {target:#010x}"),
                );
                true
            }
            Err(e) => {
                log.line(
                    W2R_LOG_ERROR,
                    "host",
                    &format!("could not hook {SYMBOL}: {e:?}"),
                );
                false
            }
        }
    }

    unsafe extern "C" fn detour(sender: *mut u32, buf: *mut *mut u8, len: *mut i32) -> i32 {
        let original: GetNextPeerPacketFn =
            unsafe { core::mem::transmute(ORIGINAL.load(Ordering::Acquire)) };
        for _ in 0..MAX_CONSUMED_PER_CALL {
            let got = unsafe { original(sender, buf, len) };
            if got == 0 {
                return 0;
            }
            // Host logic under the guard; a host bug must not take the game down. On a fault
            // the packet is handed to the game as if it were not ours.
            let consumed = guarded(|| unsafe { consume(*sender, *buf, *len) }).unwrap_or(false);
            if !consumed {
                return got;
            }
        }
        0
    }

    /// True when the packet was ours (or dropped) and the game must not see it.
    unsafe fn consume(sender: u32, buf: *const u8, len: i32) -> bool {
        if buf.is_null() || len <= 0 {
            return false;
        }
        let bytes = unsafe { core::slice::from_raw_parts(buf, len as usize) };
        match classify(bytes) {
            Packet::NotOurs => {
                if bytes[0] == STOCK_FILE_TRANSFER {
                    drop_file_transfer(sender, len);
                    return true;
                }
                false
            }
            Packet::Ours { .. } if !inbound_allowed(sender) => {
                note_flood(sender);
                true
            }
            Packet::Ours {
                subsystem: SUBSYSTEM_PLUGIN,
                body,
            } => {
                if let Some(msg) = plugin_message(body) {
                    crate::session::deliver_peer(sender, msg);
                }
                true
            }
            Packet::Ours {
                subsystem: SUBSYSTEM_HANDSHAKE,
                body,
            } => {
                crate::session::deliver_handshake(sender, body);
                true
            }
            // 'M' and 'L' have no handler yet; still ours, still consumed.
            Packet::Ours { .. } => true,
        }
    }

    fn inbound_allowed(sender: u32) -> bool {
        let slot = (sender as usize).min(8);
        let mut windows = INBOUND.lock().unwrap_or_else(|e| e.into_inner());
        windows[slot].allow(crate::chat::now_ms())
    }

    fn note_flood(sender: u32) {
        let slot = (sender as usize).min(8);
        let count = FLOODED[slot].fetch_add(1, Ordering::Relaxed) + 1;
        if count == 1 || count % FLOOD_LOG_EVERY == 0 {
            if let Some(s) = crate::entry::startup() {
                s.log.line(
                    W2R_LOG_INFO,
                    "host",
                    &format!(
                        "net slot {sender} sends W2R messages over {INBOUND_PER_SENDER_PER_SECOND}/s; dropping ({count} so far)"
                    ),
                );
            }
        }
    }

    fn drop_file_transfer(sender: u32, len: i32) {
        let slot = (sender as usize).min(8);
        let count = DROPPED[slot].fetch_add(1, Ordering::Relaxed) + 1;
        if count == 1 || count % DROP_LOG_EVERY == 0 {
            if let Some(s) = crate::entry::startup() {
                s.log.line(
                    W2R_LOG_INFO,
                    "host",
                    &format!(
                        "dropped stock 0x2A (legacy file transfer) from net slot {sender} (len {len}, {count} so far)"
                    ),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trips_and_respects_the_limits() {
        let tag = peer_tag("example-plugin");
        let msg = encode(tag, b"hi").unwrap();
        assert_eq!(&msg[..5], &[0xE7, b'W', b'2', b'R', b'P']);
        assert_eq!(&msg[5..9], &tag);
        assert_eq!(&msg[9..], b"hi");
        match classify(&msg) {
            Packet::Ours { subsystem, body } => {
                assert_eq!(subsystem, b'P');
                let pm = plugin_message(body).unwrap();
                assert_eq!(pm.tag, tag);
                assert_eq!(pm.payload, b"hi");
            }
            Packet::NotOurs => panic!("ours"),
        }
        let big = encode(tag, &[0; MAX_PAYLOAD]).unwrap();
        assert_eq!(big.len(), MAX_MESSAGE);
        assert!(encode(tag, &[0; MAX_PAYLOAD + 1]).is_none());
        assert!(plugin_message(&[0; 3]).is_none());
        assert!(plugin_message(&[0; 4 + MAX_PAYLOAD + 1]).is_none());
        assert_eq!(plugin_message(&[1, 2, 3, 4]).unwrap().payload, b"");
    }

    #[test]
    fn classify_tells_ours_from_the_games() {
        assert_eq!(classify(b""), Packet::NotOurs);
        assert_eq!(classify(&[0xE7, b'W', b'2']), Packet::NotOurs);
        assert_eq!(classify(&[0x2A, b'W', b'2', b'R', b'P']), Packet::NotOurs);
        assert_eq!(classify(&[0xE7, b'W', b'2', b'X', b'P']), Packet::NotOurs);
        assert_eq!(
            classify(&[0xE7, b'W', b'2', b'R', b'H']),
            Packet::Ours {
                subsystem: b'H',
                body: &[]
            }
        );
        assert_eq!(
            classify(&[0xE7, b'W', b'2', b'R', b'P', 9]),
            Packet::Ours {
                subsystem: b'P',
                body: &[9]
            }
        );
    }

    #[test]
    fn peer_tag_is_the_sha256_prefix_and_differs_per_id() {
        let expected: [u8; 4] = Sha256::digest(b"example-plugin")[..4].try_into().unwrap();
        assert_eq!(peer_tag("example-plugin"), expected);
        assert_ne!(peer_tag("a"), peer_tag("b"));
    }

    #[test]
    fn sliding_window_counts_within_the_window_only() {
        let mut w = SlidingWindow::new(2, 1000);
        assert!(w.allow(0));
        assert!(w.allow(999));
        assert!(!w.allow(999));
        assert!(w.allow(1000), "the first aged out");
        assert!(!w.allow(1500));
        assert!(w.allow(1999));
    }

    #[test]
    fn targets_resolve_against_the_roster() {
        struct Some2;
        impl Roster for Some2 {
            fn announced(&self, slot: u32) -> bool {
                slot == 2
            }
            fn any_announced(&self) -> bool {
                true
            }
        }
        assert_eq!(resolve_target(&Some2, 2), Ok(Target::Slot(2)));
        assert_eq!(resolve_target(&Some2, 3), Err(TargetError::NotAnnounced));
        assert_eq!(resolve_target(&Some2, 8), Err(TargetError::Invalid));
        assert_eq!(resolve_target(&Some2, W2R_SLOT_ALL), Ok(Target::All));
        assert_eq!(
            resolve_target(&NoPeers, W2R_SLOT_ALL),
            Err(TargetError::NotAnnounced)
        );
        assert_eq!(resolve_target(&NoPeers, 0), Err(TargetError::NotAnnounced));
    }

    /// The host side: `peer_send` through the host table and delivery through the registry,
    /// against fake transport, roster and presence.
    #[test]
    fn peer_send_and_delivery_follow_the_rules() {
        use crate::chat::NoChat;
        use crate::dispatch::Registry;
        use crate::host::{Log, Services, caps};
        use std::cell::{Cell, RefCell};
        use std::rc::Rc;
        use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
        use w2r_abi::*;

        #[derive(Default)]
        struct FakeNet {
            connected: Cell<bool>,
            sent: RefCell<Vec<(Option<u32>, Vec<u8>)>>,
        }
        impl Transport for FakeNet {
            fn connected(&self) -> bool {
                self.connected.get()
            }
            fn broadcast(&self, msg: &[u8]) -> bool {
                self.sent.borrow_mut().push((None, msg.to_vec()));
                true
            }
            fn send_to(&self, slot: u32, msg: &[u8]) -> bool {
                self.sent.borrow_mut().push((Some(slot), msg.to_vec()));
                true
            }
        }
        struct Announced(Vec<u32>);
        impl Roster for Announced {
            fn announced(&self, slot: u32) -> bool {
                self.0.contains(&slot)
            }
            fn any_announced(&self) -> bool {
                !self.0.is_empty()
            }
        }
        struct Here(Cell<bool>);
        impl Presence for Here {
            fn in_lobby(&self) -> bool {
                self.0.get()
            }
        }
        struct Quiet;
        impl Log for Quiet {
            fn line(&self, _: u32, _: &str, _: &str) {}
        }
        static RECEIVED: AtomicUsize = AtomicUsize::new(0);
        static LAST_SENDER: AtomicU32 = AtomicU32::new(99);
        unsafe extern "C" fn on_load(_: *const W2rHost) -> W2rStatus {
            W2R_OK
        }
        unsafe extern "C" fn on_event(ev: *const W2rEvent) {
            let ev = unsafe { &*ev };
            assert_eq!(ev.id, W2R_EVT_PEER_MESSAGE);
            let m = unsafe { &*(ev.payload as *const W2rEvtPeerMessage) };
            assert_eq!(m.struct_size as usize, size_of::<W2rEvtPeerMessage>());
            let data = unsafe { core::slice::from_raw_parts(m.data.ptr, m.data.len as usize) };
            assert_eq!(data, b"ping");
            LAST_SENDER.store(m.net_slot, Ordering::Relaxed);
            RECEIVED.fetch_add(1, Ordering::Relaxed);
        }
        static T: W2rPlugin = W2rPlugin {
            struct_size: W2R_PLUGIN_V1_SIZE,
            on_load: Some(on_load),
            on_event: Some(on_event),
            on_ui: None,
            on_unload: None,
        };
        unsafe extern "C" fn entry(_: *const W2rHost) -> *const W2rPlugin {
            &T
        }
        let buf = |b: &[u8]| W2rBuf {
            ptr: b.as_ptr(),
            len: b.len() as u32,
        };

        let net = Rc::new(FakeNet::default());
        let here = Rc::new(Here(Cell::new(false)));
        let services = Services {
            chat: Rc::new(NoChat),
            transport: net.clone(),
            roster: Rc::new(Announced(vec![2, 5])),
            presence: here.clone(),
        };
        let mut reg = Registry::with(Rc::new(Quiet), None, services);
        let a = reg
            .load_entry_with_caps("alpha", entry, &[caps::NET_SEND])
            .unwrap();
        let b = reg.load_entry("beta", entry).unwrap();
        let h = unsafe { &*reg.slot(a).host() };
        let hb = unsafe { &*reg.slot(b).host() };
        let big = [0u8; MAX_PAYLOAD + 1];

        unsafe {
            assert_eq!((hb.peer_send)(hb.token, 2, buf(b"ping")), W2R_E_DENIED);
            assert_eq!(
                (h.peer_send)(
                    h.token,
                    2,
                    W2rBuf {
                        ptr: core::ptr::null(),
                        len: 4
                    }
                ),
                W2R_E_INVALID
            );
            assert_eq!((h.peer_send)(h.token, 2, buf(b"")), W2R_E_INVALID);
            assert_eq!((h.peer_send)(h.token, 2, buf(&big)), W2R_E_LIMIT);
            assert_eq!((h.peer_send)(h.token, 8, buf(b"ping")), W2R_E_INVALID);
            assert_eq!(
                (h.peer_send)(h.token, 2, buf(b"ping")),
                W2R_E_STATE,
                "not in lobby"
            );
            here.0.set(true);
            assert_eq!(
                (h.peer_send)(h.token, 2, buf(b"ping")),
                W2R_E_STATE,
                "not connected"
            );
            net.connected.set(true);
            assert_eq!(
                (h.peer_send)(h.token, 3, buf(b"ping")),
                W2R_E_STATE,
                "not announced"
            );
            assert_eq!((h.peer_send)(h.token, 2, buf(b"ping")), W2R_OK);
            assert_eq!((h.peer_send)(h.token, W2R_SLOT_ALL, buf(b"ping")), W2R_OK);
            let mut sent = 2;
            while (h.peer_send)(h.token, 5, buf(b"x")) == W2R_OK {
                sent += 1;
                assert!(sent <= PER_PLUGIN_PER_SECOND, "no per-plugin limit");
            }
            assert_eq!(sent, PER_PLUGIN_PER_SECOND);
            assert_eq!((h.peer_send)(h.token, 5, buf(b"x")), W2R_E_RATE);
        }
        let expected = encode(peer_tag("alpha"), b"ping").unwrap();
        assert_eq!(net.sent.borrow()[0], (Some(2), expected.clone()));
        assert_eq!(net.sent.borrow()[1], (None, expected));

        // Receiving: only the matching, subscribed, capable plugin, only from announced peers.
        let msg = PluginMessage {
            tag: peer_tag("alpha"),
            payload: b"ping",
        };
        assert!(!reg.dispatch_peer(2, msg), "not subscribed yet");
        unsafe {
            assert_eq!((h.subscribe)(h.token, W2R_EVT_PEER_MESSAGE), W2R_OK);
            assert_eq!((hb.subscribe)(hb.token, W2R_EVT_PEER_MESSAGE), W2R_E_DENIED);
        }
        assert!(!reg.dispatch_peer(1, msg), "sender did not announce");
        assert!(reg.dispatch_peer(2, msg));
        assert_eq!(RECEIVED.load(Ordering::Relaxed), 1);
        assert_eq!(LAST_SENDER.load(Ordering::Relaxed), 2);
        assert!(
            !reg.dispatch_peer(
                2,
                PluginMessage {
                    tag: [0; 4],
                    payload: b"ping"
                }
            ),
            "unknown tag"
        );
        assert!(!reg.dispatch_peer(
            2,
            PluginMessage {
                tag: peer_tag("beta"),
                payload: b"ping"
            }
        ));
        assert_eq!(RECEIVED.load(Ordering::Relaxed), 1);
        here.0.set(false);
        assert!(!reg.dispatch_peer(2, msg), "lobby only, inbound too");
        assert_eq!(RECEIVED.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_refused_send_costs_no_budget() {
        let mut w = SlidingWindow::new(2, 1000);
        assert!(w.would_allow(0));
        assert!(w.would_allow(0), "a check records nothing");
        assert!(w.allow(0));
        assert!(w.allow(1));
        assert!(!w.would_allow(2));
        assert!(!w.allow(2));
        assert!(w.allow(1000), "the window moved on");
    }
}
