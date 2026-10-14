//! The lobby handshake (subsystem `'H'`), ported from wc2r-plus `peer_handshake.cpp` and
//! wire-compatible with it (docs/peer-protocol.md). It lets players running the host find each
//! other in a lobby, fills the roster `peer_send` and `W2R_EVT_PEER_MESSAGE` are gated on, and
//! carries the simulation fingerprint (docs/load-policy.md) so mismatched plugin sets are
//! warned about, or refused when the host asks for that.
//!
//! This is a host feature, not a plugin: the roster is the host's own policy input, the
//! fingerprint covers plugins the host loaded, and refusing a player is a host action. It is
//! pure logic driven by the session (`session.rs`): the game's transport, clock and lobby
//! presence come in as arguments, and everything it wants done comes back as [`Out`] values,
//! so every rule is tested without the game.

use w2r_abi::W2R_LOG_INFO;

use crate::fingerprint;
use crate::peer::{self, BASE_HEADER_LEN, MAX_SLOTS};

pub const SUBSYSTEM: u8 = peer::SUBSYSTEM_HANDSHAKE;
pub const PROTOCOL_VERSION: u8 = 1;
pub const KIND_HELLO: u8 = 1;
pub const KIND_REPLY: u8 = 2;
pub const VERSION_LEN: usize = 16;
/// Bytes after the 5-byte envelope: version, kind, nonce, caps, fingerprint, version text.
pub const BODY_LEN: usize = 1 + 1 + 4 + 4 + 4 + VERSION_LEN;
pub const MESSAGE_LEN: usize = BASE_HEADER_LEN + BODY_LEN;
const _: () = assert!(MESSAGE_LEN == 35, "wc2r-plus sends 35 bytes");

/// Capability bits. wc2r-plus defines 0 and 2; 1 is reserved (older w2rpm builds set it for a
/// "strict" mode that no longer exists; it is ignored).
pub const CAP_HANDSHAKE: u32 = 1 << 0;
pub const CAP_MAP_SHARING: u32 = 1 << 2;
/// This build supports the observer team and the Observer plugin is loaded. Observers only need
/// every player to announce it, not identical plugins.
pub const CAP_OBSERVER: u32 = 1 << 3;

/// What goes in the 16-byte version field. wc2r-plus logs it as "runs wc2r-plus <text>", so
/// the text names us.
pub const VERSION_TEXT: &str = concat!("w2rpm ", env!("CARGO_PKG_VERSION"));
const _: () = assert!(VERSION_TEXT.len() <= VERSION_LEN);

/// A net slot presenting a new nonce sooner than this is ignored (wc2r-plus): cycling nonces
/// must not draw a reply or a log line each time.
pub const NONCE_CHANGE_MIN_MS: u64 = 5_000;
/// How often the transport's peer list is consulted.
pub const ROSTER_CHECK_MS: u64 = 1_000;
/// The host is always net slot 0 (wc2r-plus PROTOCOL.md).
pub const HOST_SLOT: u32 = 0;

/// The local side: what this instance announces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Local {
    pub fingerprint: [u8; 4],
}

impl Local {
    pub fn caps(&self) -> u32 {
        CAP_HANDSHAKE
    }
}

/// A decoded HELLO or REPLY.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Announcement {
    pub kind: u8,
    pub nonce: u32,
    pub caps: u32,
    pub fingerprint: [u8; 4],
    /// Printable ASCII only (anything else became `?`), cut at the first NUL.
    pub version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ignored {
    /// Shorter than [`BODY_LEN`].
    Short(usize),
    /// Another protocol version.
    Version(u8),
}

/// The whole message, envelope included.
pub fn encode(kind: u8, nonce: u32, caps: u32, fingerprint: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(MESSAGE_LEN);
    out.push(peer::TYPE);
    out.extend_from_slice(&peer::MAGIC);
    out.push(SUBSYSTEM);
    out.push(PROTOCOL_VERSION);
    out.push(kind);
    out.extend_from_slice(&nonce.to_le_bytes());
    out.extend_from_slice(&caps.to_le_bytes());
    // The 4 bytes go on the wire in fingerprint order (docs/load-policy.md).
    out.extend_from_slice(&fingerprint);
    let mut version = [0u8; VERSION_LEN];
    version[..VERSION_TEXT.len()].copy_from_slice(VERSION_TEXT.as_bytes());
    out.extend_from_slice(&version);
    debug_assert_eq!(out.len(), MESSAGE_LEN);
    out
}

/// Decodes the body after the envelope. Longer bodies are fine: a later version may append.
pub fn decode(body: &[u8]) -> Result<Announcement, Ignored> {
    if body.len() < BODY_LEN {
        return Err(Ignored::Short(body.len()));
    }
    if body[0] != PROTOCOL_VERSION {
        return Err(Ignored::Version(body[0]));
    }
    let u32_at = |i: usize| u32::from_le_bytes(body[i..i + 4].try_into().unwrap());
    let raw = &body[14..14 + VERSION_LEN];
    let end = raw.iter().position(|&b| b == 0).unwrap_or(VERSION_LEN);
    let version = raw[..end]
        .iter()
        .map(|&b| {
            if (0x20..=0x7E).contains(&b) {
                b as char
            } else {
                '?'
            }
        })
        .collect();
    Ok(Announcement {
        kind: body[1],
        nonce: u32_at(2),
        caps: u32_at(6),
        fingerprint: body[10..14].try_into().unwrap(),
        version,
    })
}

/// A peer that announced, as last heard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub nonce: u32,
    pub caps: u32,
    pub fingerprint: [u8; 4],
    pub version: String,
    /// Our REPLY went out for this nonce (once; a REPLY is never answered).
    replied: bool,
    /// The mismatch (if any) was logged for this nonce.
    mismatch_logged: bool,
}

/// What the session must do for the handshake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Out {
    Broadcast(Vec<u8>),
    SendTo(u32, Vec<u8>),
    /// A log line, `(level, text)`.
    Log(u32, String),
}

/// Per-frame facts the session reads from the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    pub now: u64,
    pub connected: bool,
    pub is_host: bool,
    /// Lobby slot (0-7) per net slot, for messages shown to the user; `None` when unknown.
    pub lobby_slots: Option<[Option<u8>; 8]>,
}

#[derive(Debug)]
pub struct Handshake {
    local: Local,
    in_lobby: bool,
    nonce: u32,
    self_slot: Option<u32>,
    is_host: bool,
    peers: [Option<Peer>; 8],
    nonce_change_ms: [u64; 8],
    caps_log_ms: [u64; 8],
    /// Who the transport lists, and since when.
    present: Option<[Option<u64>; 8]>,
    last_roster_ms: Option<u64>,
    lobby_slots: [Option<u8>; 8],
    hello_pending: bool,
    told_bad_message: bool,
    /// Announced as [`CAP_MAP_SHARING`] (`map_sharing` in `host.json`).
    map_sharing: bool,
    observer: bool,
}

const NO_PEER: Option<Peer> = None;

impl Handshake {
    pub fn new(local: Local) -> Self {
        Handshake {
            local,
            in_lobby: false,
            nonce: 0,
            self_slot: None,
            is_host: false,
            peers: [NO_PEER; 8],
            nonce_change_ms: [0; 8],
            caps_log_ms: [0; 8],
            present: None,
            last_roster_ms: None,
            lobby_slots: [None; 8],
            hello_pending: false,
            told_bad_message: false,
            map_sharing: false,
            observer: false,
        }
    }

    /// This player's capabilities as announced: the local ones plus map sharing when it is on.
    fn caps(&self) -> u32 {
        self.local.caps()
            | if self.map_sharing { CAP_MAP_SHARING } else { 0 }
            | if self.observer { CAP_OBSERVER } else { 0 }
    }

    /// Map sharing on or off. In a lobby this re-announces (the capabilities changed).
    pub fn set_map_sharing(&mut self, on: bool) {
        if self.map_sharing == on {
            return;
        }
        self.map_sharing = on;
        if self.in_lobby {
            self.hello_pending = true;
        }
    }

    /// Observer support on or off (the Observer plugin loaded). In a lobby this re-announces.
    pub fn set_observer(&mut self, on: bool) {
        if self.observer == on {
            return;
        }
        self.observer = on;
        if self.in_lobby {
            self.hello_pending = true;
        }
    }

    /// For an announced player, whether their simulation plugins match ours.
    pub fn peer_matches(&self, slot: u32) -> Option<bool> {
        self.peer(slot)
            .map(|p| p.fingerprint == self.local.fingerprint)
    }

    /// Whether a player announced map sharing.
    pub fn peer_shares_maps(&self, slot: u32) -> bool {
        self.peer(slot)
            .is_some_and(|p| p.caps & CAP_MAP_SHARING != 0)
    }

    pub fn local(&self) -> Local {
        self.local
    }

    /// The fingerprint is known only after the plugins loaded; set it before any lobby.
    pub fn set_fingerprint(&mut self, fingerprint: [u8; 4]) {
        self.local.fingerprint = fingerprint;
    }

    /// The local player entered a lobby. `nonce` identifies this stay (non-zero).
    pub fn enter(&mut self, nonce: u32) {
        self.in_lobby = true;
        self.nonce = if nonce == 0 { 1 } else { nonce };
        self.self_slot = None;
        self.is_host = false;
        self.peers = [NO_PEER; 8];
        self.nonce_change_ms = [0; 8];
        self.caps_log_ms = [0; 8];
        self.present = None;
        self.last_roster_ms = None;
        self.lobby_slots = [None; 8];
        self.hello_pending = true;
        self.told_bad_message = false;
    }

    /// The local player left. Nobody is announced from here on.
    pub fn leave(&mut self) {
        self.in_lobby = false;
        self.peers = [NO_PEER; 8];
        self.present = None;
        self.hello_pending = false;
    }

    pub fn in_lobby(&self) -> bool {
        self.in_lobby
    }

    pub fn nonce(&self) -> u32 {
        self.nonce
    }

    pub fn self_slot(&self) -> Option<u32> {
        self.self_slot
    }

    pub fn peer(&self, slot: u32) -> Option<&Peer> {
        if !self.in_lobby {
            return None;
        }
        self.peers.get(slot as usize)?.as_ref()
    }

    /// The roster `peer_send` consults (docs/abi.md).
    pub fn announced(&self, slot: u32) -> bool {
        self.peer(slot).is_some()
    }

    pub fn any_announced(&self) -> bool {
        self.in_lobby && self.peers.iter().any(Option::is_some)
    }

    fn hello(&self) -> Vec<u8> {
        encode(KIND_HELLO, self.nonce, self.caps(), self.local.fingerprint)
    }

    fn reply(&self) -> Vec<u8> {
        encode(KIND_REPLY, self.nonce, self.caps(), self.local.fingerprint)
    }

    /// Once per frame while the lobby window is drawn. `peers` lists the net slots the
    /// transport knows (consulted at most every [`ROSTER_CHECK_MS`]); `None` when it cannot say.
    pub fn frame(&mut self, f: Frame, peers: impl FnOnce() -> Option<Vec<u32>>) -> Vec<Out> {
        let mut out = Vec::new();
        if !self.in_lobby {
            return out;
        }
        self.is_host = f.is_host;
        if let Some(slots) = f.lobby_slots {
            self.lobby_slots = slots;
        }
        if !f.connected {
            return out;
        }
        if self
            .last_roster_ms
            .is_none_or(|t| f.now.saturating_sub(t) >= ROSTER_CHECK_MS)
        {
            self.last_roster_ms = Some(f.now);
            if let Some(list) = peers() {
                self.check_roster(f.now, &list, &mut out);
            }
        }
        if self.hello_pending {
            self.hello_pending = false;
            out.push(Out::Broadcast(self.hello()));
            out.push(Out::Log(
                W2R_LOG_INFO,
                format!(
                    "announced (nonce {:08x}, fingerprint {})",
                    self.nonce,
                    fingerprint::hex(&self.local.fingerprint),
                ),
            ));
        }
        out
    }

    fn check_roster(&mut self, now: u64, list: &[u32], out: &mut Vec<Out>) {
        let mut present = [None; 8];
        for &slot in list {
            if let Some(p) = present.get_mut(slot as usize) {
                *p = Some(now);
            }
        }
        if let Some(before) = self.present {
            let mut someone_new = false;
            for s in 0..8 {
                match (before[s], present[s]) {
                    (Some(since), Some(_)) => present[s] = Some(since),
                    (None, Some(_)) => someone_new = true,
                    _ => {}
                }
            }
            if someone_new {
                self.hello_pending = true;
            }
        }
        for (s, here) in present.iter().enumerate() {
            if here.is_none() && self.peers[s].is_some() {
                out.push(Out::Log(W2R_LOG_INFO, format!("net slot {s} left")));
                self.peers[s] = None;
                self.nonce_change_ms[s] = 0;
            }
        }
        self.present = Some(present);
    }

    /// An `'H'` message from `sender` (the slot the transport reported); `body` is everything
    /// after the envelope.
    pub fn receive(&mut self, now: u64, sender: u32, body: &[u8]) -> Vec<Out> {
        let mut out = Vec::new();
        if !self.in_lobby {
            return out;
        }
        let a = match decode(body) {
            Ok(a) => a,
            Err(why) => {
                // Attacker-triggerable, so one line per stay in the lobby.
                if !self.told_bad_message {
                    self.told_bad_message = true;
                    out.push(Out::Log(
                        W2R_LOG_INFO,
                        format!("ignoring a handshake from net slot {sender}: {why:?}"),
                    ));
                }
                return out;
            }
        };
        if a.nonce == self.nonce {
            if self.self_slot.is_none() && sender < MAX_SLOTS {
                self.self_slot = Some(sender);
                out.push(Out::Log(
                    W2R_LOG_INFO,
                    format!("our own announcement came back on net slot {sender}"),
                ));
            }
            return out;
        }
        if sender >= MAX_SLOTS || a.nonce == 0 {
            return out;
        }
        let s = sender as usize;
        let known = self.peers[s].as_ref().is_some_and(|p| p.nonce == a.nonce);
        if !known {
            let last = self.nonce_change_ms[s];
            if last != 0 && now.saturating_sub(last) < NONCE_CHANGE_MIN_MS {
                return out;
            }
            self.nonce_change_ms[s] = now.max(1);
            out.push(Out::Log(
                W2R_LOG_INFO,
                format!(
                    "net slot {sender} ({}) runs {} (caps {:08x}, fingerprint {}, via {})",
                    self.describe(sender),
                    a.version,
                    a.caps,
                    fingerprint::hex(&a.fingerprint),
                    if a.kind == KIND_HELLO {
                        "hello"
                    } else {
                        "reply"
                    }
                ),
            ));
            self.peers[s] = Some(Peer {
                nonce: a.nonce,
                caps: a.caps,
                fingerprint: a.fingerprint,
                version: a.version,
                replied: false,
                mismatch_logged: false,
            });
        } else {
            let p = self.peers[s].as_mut().expect("known");
            if p.caps != a.caps || p.fingerprint != a.fingerprint {
                p.caps = a.caps;
                p.fingerprint = a.fingerprint;
                p.mismatch_logged = false;
                if now.saturating_sub(self.caps_log_ms[s]) >= NONCE_CHANGE_MIN_MS {
                    self.caps_log_ms[s] = now.max(1);
                    out.push(Out::Log(
                        W2R_LOG_INFO,
                        format!(
                            "net slot {sender} now has caps {:08x}, fingerprint {}",
                            a.caps,
                            fingerprint::hex(&a.fingerprint)
                        ),
                    ));
                }
            }
        }
        if a.kind == KIND_HELLO {
            let p = self.peers[s].as_mut().expect("just set");
            if !p.replied {
                p.replied = true;
                out.push(Out::SendTo(sender, self.reply()));
                out.push(Out::Log(
                    W2R_LOG_INFO,
                    format!("replied to net slot {sender}"),
                ));
            }
        }
        self.enforce(sender, &mut out);
        out
    }

    /// The fingerprint policy for one announced peer: log a mismatch once.
    fn enforce(&mut self, slot: u32, out: &mut Vec<Out>) {
        let ours = self.local.fingerprint;
        let is_host = self.is_host;
        let who = self.describe(slot);
        let Some(p) = self.peers.get_mut(slot as usize).and_then(Option::as_mut) else {
            return;
        };
        if p.fingerprint == ours {
            return;
        }
        let matters = is_host || slot == HOST_SLOT;
        if matters && !p.mismatch_logged {
            p.mismatch_logged = true;
            out.push(Out::Log(
                W2R_LOG_INFO,
                format!(
                    "net slot {slot} ({who}) runs different simulation plugins: theirs {}, ours {}",
                    fingerprint::hex(&p.fingerprint),
                    fingerprint::hex(&ours)
                ),
            ));
        }
    }

    /// "Player N" by lobby slot when known, else the net slot.
    fn describe(&self, slot: u32) -> String {
        match self.lobby_slots.get(slot as usize).copied().flatten() {
            Some(l) => format!("Player {}", l + 1),
            None => format!("net slot {slot}"),
        }
    }

    /// Whether every other player in the lobby announced observer support (a w2rpm that has it, with
    /// the Observer plugin loaded), so the observer team may be used. Their other plugins may differ.
    /// False when unsure: not in a lobby, we have no observer support ourselves, the roster not read
    /// yet, or somebody present who has not announced.
    pub fn everyone_has(&self) -> bool {
        if !self.in_lobby || !self.observer {
            return false;
        }
        let Some(present) = &self.present else {
            return false;
        };
        // The host is net slot 0 even before it has heard its own announcement.
        let me = self
            .self_slot
            .or(self.is_host.then_some(HOST_SLOT));
        (0..8).all(|s| {
            Some(s as u32) == me
                || present[s].is_none()
                || self.peers[s].as_ref().is_some_and(|p| p.caps & CAP_OBSERVER != 0)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP_A: [u8; 4] = [0xaa, 0xbb, 0xcc, 0xdd];
    const FP_B: [u8; 4] = [1, 2, 3, 4];

    fn body(msg: &[u8]) -> &[u8] {
        match peer::classify(msg) {
            peer::Packet::Ours { subsystem, body } => {
                assert_eq!(subsystem, b'H');
                body
            }
            peer::Packet::NotOurs => panic!("ours"),
        }
    }

    fn frame(now: u64, is_host: bool) -> Frame {
        Frame {
            now,
            connected: true,
            is_host,
            lobby_slots: None,
        }
    }

    fn broadcasts(out: &[Out]) -> Vec<&Vec<u8>> {
        out.iter()
            .filter_map(|o| match o {
                Out::Broadcast(m) => Some(m),
                _ => None,
            })
            .collect()
    }

    fn sends(out: &[Out]) -> Vec<(u32, &Vec<u8>)> {
        out.iter()
            .filter_map(|o| match o {
                Out::SendTo(s, m) => Some((*s, m)),
                _ => None,
            })
            .collect()
    }

    fn logs(out: &[Out]) -> Vec<&str> {
        out.iter()
            .filter_map(|o| match o {
                Out::Log(_, m) => Some(m.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_wire_format_is_the_wc2r_plus_one() {
        let msg = encode(KIND_HELLO, 0x0403_0201, CAP_HANDSHAKE | (1 << 1), FP_A);
        assert_eq!(msg.len(), 35);
        assert_eq!(&msg[..5], &[0xE7, b'W', b'2', b'R', b'H']);
        assert_eq!(msg[5], 1, "protocol version");
        assert_eq!(msg[6], 1, "kind");
        assert_eq!(&msg[7..11], &[1, 2, 3, 4], "nonce little-endian");
        assert_eq!(&msg[11..15], &[3, 0, 0, 0], "caps little-endian");
        assert_eq!(&msg[15..19], &FP_A, "fingerprint in order");
        assert_eq!(&msg[19..19 + VERSION_TEXT.len()], VERSION_TEXT.as_bytes());
        assert!(msg[19 + VERSION_TEXT.len()..].iter().all(|&b| b == 0));

        let a = decode(body(&msg)).unwrap();
        assert_eq!(
            a,
            Announcement {
                kind: KIND_HELLO,
                nonce: 0x0403_0201,
                caps: 3,
                fingerprint: FP_A,
                version: VERSION_TEXT.to_owned(),
            }
        );
        // Extra bytes are fine; a short body or another version is not.
        let mut longer = msg.clone();
        longer.extend_from_slice(&[9, 9, 9]);
        assert_eq!(decode(body(&longer)).unwrap(), a);
        assert_eq!(decode(&msg[5..34]), Err(Ignored::Short(29)));
        let mut v2 = msg.clone();
        v2[5] = 2;
        assert_eq!(decode(body(&v2)), Err(Ignored::Version(2)));
        // What wc2r-plus 1.0.0 sends: zero fingerprint, its own version text, and the
        // non-printable bytes a hostile peer might put there become '?'.
        let mut theirs = encode(KIND_HELLO, 7, CAP_HANDSHAKE, [0; 4]);
        theirs[19..35].copy_from_slice(b"1.0.0\0\0\0\0\0\0\0\0\0\0\0");
        assert_eq!(decode(body(&theirs)).unwrap().version, "1.0.0");
        theirs[19..35].copy_from_slice(b"1.\x01\xff\n0\0\0\0\0\0\0\0\0\0\0");
        assert_eq!(decode(body(&theirs)).unwrap().version, "1.???0");
    }

    /// Two instances in one lobby: A enters and announces, B hears it and replies once, both
    /// end up announced to each other, and nobody answers a REPLY.
    #[test]
    fn hello_and_reply_fill_both_rosters_without_a_loop() {
        let mut a = Handshake::new(Local {
            fingerprint: FP_A,
        });
        let mut b = Handshake::new(Local {
            fingerprint: FP_A,
        });
        assert!(
            a.frame(frame(0, true), || None).is_empty(),
            "not in a lobby"
        );
        a.enter(0x1111);
        b.enter(0x2222);
        let out = a.frame(frame(10, true), || Some(vec![0, 3]));
        let hello = broadcasts(&out);
        assert_eq!(hello.len(), 1);
        assert!(
            a.frame(frame(20, true), || None).is_empty(),
            "announced once"
        );
        assert!(!a.announced(3));

        // A's own broadcast comes back to A: that is how A learns its net slot.
        let out = a.receive(30, 0, body(hello[0]));
        assert_eq!(a.self_slot(), Some(0));
        assert!(sends(&out).is_empty() && !a.announced(0));

        // B (net slot 3) hears A's HELLO and replies to A only.
        let out = b.receive(40, 0, body(hello[0]));
        assert!(b.announced(0));
        assert!(b.any_announced());
        let reply = sends(&out);
        assert_eq!(reply.len(), 1);
        assert_eq!(reply[0].0, 0);
        assert_eq!(decode(body(reply[0].1)).unwrap().kind, KIND_REPLY);
        assert!(logs(&out)[0].starts_with("net slot 0 (net slot 0) runs w2rpm "));
        assert!(logs(&out)[0].contains("via hello"));
        // The same HELLO again (a duplicate) draws no second reply and no log.
        assert!(b.receive(41, 0, body(hello[0])).is_empty());

        // A hears the REPLY: B is announced, and a REPLY is never answered.
        let out = a.receive(50, 3, body(reply[0].1));
        assert!(a.announced(3));
        assert!(sends(&out).is_empty() && broadcasts(&out).is_empty());
        assert!(!a.announced(8), "out of range");

        // Leaving forgets everyone.
        a.leave();
        assert!(!a.announced(3) && !a.any_announced());
        assert!(a.receive(70, 3, body(reply[0].1)).is_empty(), "lobby only");
    }

    /// Regression test for the "unthrottled logging of attacker-triggerable events" finding:
    /// a peer cycling nonces gets one reply and one log line per 5 s, not one per packet.
    #[test]
    fn nonce_cycling_and_garbage_are_throttled() {
        let mut h = Handshake::new(Local {
            fingerprint: [0; 4],
        });
        h.enter(0x1111);
        h.frame(frame(0, false), || None);
        let mut replies = 0;
        let mut lines = 0;
        for i in 0..1000u32 {
            let hello = encode(KIND_HELLO, 0x5000 + i, CAP_HANDSHAKE, [0; 4]);
            let out = h.receive(100 + i as u64, 2, body(&hello));
            replies += sends(&out).len();
            lines += logs(&out).len();
        }
        assert_eq!(replies, 1);
        assert_eq!(lines, 2, "one 'runs' line and one 'replied' line");
        // 5 s later one more is accepted.
        let hello = encode(KIND_HELLO, 0x9999, CAP_HANDSHAKE, [0; 4]);
        let out = h.receive(100 + NONCE_CHANGE_MIN_MS, 2, body(&hello));
        assert_eq!(sends(&out).len(), 1);
        assert!(h.announced(2));

        // Garbage: one log line per stay in the lobby, never a reply.
        let mut lines = 0;
        for i in 0..1000u64 {
            let out = h.receive(7000 + i, 4, &[2, 1, 0, 0]);
            assert!(sends(&out).is_empty());
            lines += logs(&out).len();
            let mut v2 = encode(KIND_HELLO, 0x77, CAP_HANDSHAKE, [0; 4]);
            v2[5] = 9;
            lines += logs(&h.receive(7000 + i, 4, body(&v2))).len();
        }
        assert_eq!(lines, 1);
        assert!(!h.announced(4));
        // A caps change is logged at most once per 5 s too.
        let mut lines = 0;
        for i in 0..100u32 {
            let hello = encode(KIND_HELLO, 0x9999, CAP_HANDSHAKE | (i % 2) << 2, [0; 4]);
            lines += logs(&h.receive(20_000 + i as u64, 2, body(&hello))).len();
        }
        assert_eq!(lines, 1);
    }

    #[test]
    fn the_roster_drops_leavers_and_re_announces_for_joiners() {
        let mut h = Handshake::new(Local {
            fingerprint: [0; 4],
        });
        h.enter(0x1111);
        let out = h.frame(frame(0, true), || Some(vec![0, 2]));
        assert_eq!(broadcasts(&out).len(), 1);
        let hello = encode(KIND_HELLO, 0x2222, CAP_HANDSHAKE, [0; 4]);
        h.receive(10, 2, body(&hello));
        assert!(h.announced(2));
        // The list is consulted at most once per second.
        let mut asked = 0;
        let out = h.frame(frame(500, true), || {
            asked += 1;
            Some(vec![0])
        });
        assert_eq!(asked, 0);
        assert!(out.is_empty());
        assert!(h.announced(2));
        // Slot 2 left: forgotten. Slot 5 arrived: HELLO again so it can learn about us.
        let out = h.frame(frame(1000, true), || Some(vec![0, 5]));
        assert!(!h.announced(2));
        assert_eq!(broadcasts(&out).len(), 1);
        assert_eq!(logs(&out)[0], "net slot 2 left");
        // No change: nothing.
        let out = h.frame(frame(2000, true), || Some(vec![0, 5]));
        assert!(out.is_empty());
        // A list the transport cannot give changes nothing.
        let out = h.frame(frame(3000, true), || None);
        assert!(out.is_empty());
        // Not connected: nothing goes out, and the roster is not consulted.
        h.enter(0x3333);
        let out = h.frame(
            Frame {
                connected: false,
                ..frame(4000, true)
            },
            || panic!("not consulted"),
        );
        assert!(out.is_empty());
        assert_eq!(broadcasts(&h.frame(frame(4001, true), || None)).len(), 1);
    }

    #[test]
    fn a_mismatched_peer_is_logged_once_and_warned_about_never_removed() {
        let mut host = Handshake::new(Local {
            fingerprint: FP_A,
        });
        host.enter(0x1111);
        host.frame(
            Frame {
                lobby_slots: Some([Some(0), None, Some(4), None, None, None, None, None]),
                ..frame(0, true)
            },
            || Some(vec![0, 2]),
        );
        let same = encode(KIND_HELLO, 0x2222, CAP_HANDSHAKE, FP_A);
        let differs = encode(KIND_HELLO, 0x3333, CAP_HANDSHAKE, FP_B);
        assert!(logs(&host.receive(10, 1, body(&same))).iter().all(|m| !m.contains("different")));
        let out = host.receive(10, 2, body(&differs));
        let l = logs(&out);
        assert!(l.iter().any(|m| m.contains("Player 5")), "{l:?}");
        assert!(
            l.iter()
                .any(|m| m.contains("theirs 01020304, ours aabbccdd"))
        );
        assert!(!out.iter().any(|o| !matches!(o, Out::Log(..) | Out::SendTo(..))));
        // The same announcement again: no second warning line.
        let out = host.receive(11, 2, body(&differs));
        assert!(logs(&out).is_empty());
    }

    #[test]
    fn everyone_has_needs_every_present_player_to_announce_observer_support() {
        let mut host = Handshake::new(Local {
            fingerprint: FP_A,
        });
        host.set_observer(true);
        assert!(!host.everyone_has(), "not in a lobby");
        host.enter(0x1111);
        assert!(!host.everyone_has(), "roster not read yet");
        host.frame(frame(0, true), || Some(vec![0, 1, 2]));
        assert!(!host.everyone_has(), "two players have not announced");
        let capable = CAP_HANDSHAKE | CAP_OBSERVER;
        host.receive(10, 1, body(&encode(KIND_HELLO, 0x2222, capable, FP_A)));
        assert!(!host.everyone_has());
        host.receive(10, 2, body(&encode(KIND_HELLO, 0x3333, CAP_HANDSHAKE, FP_A)));
        assert!(!host.everyone_has(), "a w2rpm without the observer plugin does not count");
        host.receive(20_000, 2, body(&encode(KIND_HELLO, 0x3333, capable, FP_B)));
        assert!(host.everyone_has(), "other plugins may differ");
        // A player who leaves no longer counts against it.
        host.frame(frame(2_000, true), || Some(vec![0, 1]));
        assert!(host.everyone_has());
        // Without observer support of our own it is never allowed.
        let mut none = Handshake::new(Local {
            fingerprint: FP_A,
        });
        none.enter(1);
        none.frame(frame(0, true), || Some(vec![0]));
        assert!(!none.everyone_has());
        none.set_observer(true);
        assert!(none.everyone_has(), "alone in the lobby with it is fine");
    }

    #[test]
    fn a_joiner_is_warned_about_the_host_only() {
        let mut j = Handshake::new(Local {
            fingerprint: FP_A,
        });
        j.enter(0x1111);
        j.frame(frame(0, false), || Some(vec![0, 1, 3]));
        // Another joiner with other plugins: not our business.
        let other = encode(KIND_REPLY, 0x4444, CAP_HANDSHAKE, FP_B);
        let out = j.receive(10, 1, body(&other));
        assert!(logs(&out).iter().all(|m| !m.contains("different")));
        // The host (slot 0) with other plugins: told.
        let host = encode(KIND_REPLY, 0x5555, CAP_HANDSHAKE, FP_B);
        let out = j.receive(10, 0, body(&host));
        assert!(logs(&out).iter().any(|m| m.contains("different")));
    }

    #[test]
    fn a_zero_nonce_from_the_game_side_is_fixed_up_and_own_echo_is_not_a_peer() {
        let mut h = Handshake::new(Local {
            fingerprint: [0; 4],
        });
        h.enter(0);
        assert_ne!(h.nonce(), 0);
        let out = h.frame(frame(0, true), || None);
        let hello = broadcasts(&out)[0].clone();
        // A peer sending nonce 0 is ignored; our own echo is not a peer.
        let zero = encode(KIND_HELLO, 0, CAP_HANDSHAKE, [0; 4]);
        assert!(h.receive(1, 2, body(&zero)).is_empty());
        assert!(!h.announced(2));
        h.receive(2, 5, body(&hello));
        assert!(!h.announced(5));
        assert_eq!(h.self_slot(), Some(5));
    }

    #[test]
    fn map_sharing_is_announced_as_capability_bit_two_and_re_announced_when_it_changes() {
        let mut h = Handshake::new(Local {
            fingerprint: [0; 4],
        });
        h.enter(1);
        let caps_of = |h: &Handshake| decode(&h.hello()[5..]).unwrap().caps;
        assert_eq!(caps_of(&h), CAP_HANDSHAKE);
        h.hello_pending = false;
        h.set_map_sharing(true);
        assert_eq!(caps_of(&h), CAP_HANDSHAKE | CAP_MAP_SHARING);
        assert!(h.hello_pending, "a change inside a lobby re-announces");
        h.hello_pending = false;
        h.set_map_sharing(true);
        assert!(!h.hello_pending, "no change, no announcement");
        let mut s = Handshake::new(Local {
            fingerprint: [0; 4],
        });
        s.set_map_sharing(true);
        assert_eq!(s.caps(), CAP_HANDSHAKE | CAP_MAP_SHARING);
        assert!(!s.peer_shares_maps(1), "nobody announced yet");
    }
}
