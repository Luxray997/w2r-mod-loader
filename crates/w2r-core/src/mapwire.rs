//! The map-sharing wire format, subsystem `'M'` (docs/map-download.md), byte for byte the one in
//! wc2r-plus `PROTOCOL.md` section 3 and `map_download/wire.cpp`. A message is the envelope
//! (`0xE7`, `W2R`, `M`), the version (1), the kind, a 16-byte request id and a payload whose
//! length is fixed by the kind. Anything else (wrong length, unknown version, kind, reason or
//! state, a value out of range) is dropped: [`decode`] returns a fault, never a partial message.
//!
//! Integers are little-endian. [`decode`] takes what follows the five-byte envelope, which is
//! what the packet hook hands over; [`encode`] returns the whole message.

use crate::peer::{MAGIC, TYPE};

pub const SUBSYSTEM: u8 = b'M';
pub const VERSION: u8 = 1;
pub const REQ_ID_LEN: usize = 16;
pub const SHA_LEN: usize = 32;
pub const SLOTS: usize = 8;
pub const MAX_MAP_BYTES: u32 = 262_144;
pub const MAX_CHUNK: u16 = 960;
pub const MAX_NAME_LEN: usize = 64;

/// The envelope: type, magic, subsystem.
const ENVELOPE: usize = 5;
/// After the envelope: version, kind and the request id.
const BODY_HEADER: usize = 2 + REQ_ID_LEN;
/// The whole message's header, as `PROTOCOL.md` counts it.
pub const HEADER_LEN: usize = ENVELOPE + BODY_HEADER;
/// The longest message: a full chunk.
pub const MAX_MESSAGE_LEN: usize = HEADER_LEN + 4 + 2 + MAX_CHUNK as usize;

pub type ReqId = [u8; REQ_ID_LEN];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Request = 1,
    Offer = 2,
    Deny = 3,
    Accept = 4,
    Chunk = 5,
    Ack = 6,
    Cancel = 7,
    Done = 8,
    Progress = 9,
    Have = 10,
    Leaving = 11,
}

impl Kind {
    fn from_u8(b: u8) -> Option<Kind> {
        Some(match b {
            1 => Kind::Request,
            2 => Kind::Offer,
            3 => Kind::Deny,
            4 => Kind::Accept,
            5 => Kind::Chunk,
            6 => Kind::Ack,
            7 => Kind::Cancel,
            8 => Kind::Done,
            9 => Kind::Progress,
            10 => Kind::Have,
            11 => Kind::Leaving,
            _ => return None,
        })
    }
}

/// Why a player left the lobby without the map; shown to the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LeaveWhy {
    HostCannotSend = 1,
    DownloadsOff = 2,
    BadName = 3,
    DownloadFailed = 4,
}

impl LeaveWhy {
    fn from_u8(b: u8) -> Option<LeaveWhy> {
        Some(match b {
            1 => LeaveWhy::HostCannotSend,
            2 => LeaveWhy::DownloadsOff,
            3 => LeaveWhy::BadName,
            4 => LeaveWhy::DownloadFailed,
            _ => return None,
        })
    }
}

/// The reason in DENY, CANCEL and DONE.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Reason {
    Ok = 0,
    NotCurrentMap = 1,
    ServingOff = 2,
    MapUnservable = 3,
    RateLimited = 4,
    Busy = 5,
    Declined = 6,
    TooLarge = 7,
    BadName = 8,
    Timeout = 9,
    HashMismatch = 10,
    InvalidMap = 11,
    StoreFailed = 12,
    LeftLobby = 13,
    Countdown = 14,
}

impl Reason {
    const COUNT: u8 = 15;

    fn from_u8(b: u8) -> Option<Reason> {
        const ALL: [Reason; 15] = [
            Reason::Ok,
            Reason::NotCurrentMap,
            Reason::ServingOff,
            Reason::MapUnservable,
            Reason::RateLimited,
            Reason::Busy,
            Reason::Declined,
            Reason::TooLarge,
            Reason::BadName,
            Reason::Timeout,
            Reason::HashMismatch,
            Reason::InvalidMap,
            Reason::StoreFailed,
            Reason::LeftLobby,
            Reason::Countdown,
        ];
        (b < Self::COUNT).then(|| ALL[b as usize])
    }
}

/// A player's download state as the host reports it in PROGRESS.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum SlotState {
    #[default]
    None = 0,
    Offered = 1,
    Uploading = 2,
    Done = 3,
    NoMap = 4,
    Stalled = 5,
    LeftNoMap = 6,
    NoAnswer = 7,
}

impl SlotState {
    fn from_u8(b: u8) -> Option<SlotState> {
        Some(match b {
            0 => SlotState::None,
            1 => SlotState::Offered,
            2 => SlotState::Uploading,
            3 => SlotState::Done,
            4 => SlotState::NoMap,
            5 => SlotState::Stalled,
            6 => SlotState::LeftNoMap,
            7 => SlotState::NoAnswer,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlotProgress {
    pub state: SlotState,
    /// 0 to 100 (for `LeftNoMap` it carries the [`LeaveWhy`] instead, as wc2r-plus does).
    pub percent: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeFault {
    TooShort,
    UnsupportedVersion,
    UnknownKind,
    BadLength,
    BadField,
}

impl DecodeFault {
    pub fn describe(self) -> &'static str {
        match self {
            DecodeFault::TooShort => "too short",
            DecodeFault::UnsupportedVersion => "unsupported version",
            DecodeFault::UnknownKind => "unknown kind",
            DecodeFault::BadLength => "wrong length for its kind",
            DecodeFault::BadField => "field out of range",
        }
    }
}

/// One message. Request names and chunk data borrow from the packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Msg<'a> {
    Request {
        req: ReqId,
        name: &'a [u8],
    },
    Offer {
        req: ReqId,
        size: u32,
        sha256: [u8; SHA_LEN],
        chunk_len: u16,
    },
    Deny {
        req: ReqId,
        reason: Reason,
    },
    Accept {
        req: ReqId,
    },
    Chunk {
        req: ReqId,
        offset: u32,
        data: &'a [u8],
    },
    Ack {
        req: ReqId,
        next_offset: u32,
    },
    Cancel {
        req: ReqId,
        reason: Reason,
    },
    Done {
        req: ReqId,
        reason: Reason,
    },
    Progress {
        slots: [SlotProgress; SLOTS],
    },
    Have {
        req: ReqId,
    },
    Leaving {
        why: LeaveWhy,
    },
}

fn get32(p: &[u8]) -> u32 {
    u32::from_le_bytes([p[0], p[1], p[2], p[3]])
}

fn get16(p: &[u8]) -> u16 {
    u16::from_le_bytes([p[0], p[1]])
}

/// Parses what follows the envelope. Nothing here panics on any input.
pub fn decode(body: &[u8]) -> Result<Msg<'_>, DecodeFault> {
    if body.len() < 2 {
        return Err(DecodeFault::TooShort);
    }
    if body[0] != VERSION {
        return Err(DecodeFault::UnsupportedVersion);
    }
    if body.len() < BODY_HEADER {
        return Err(DecodeFault::TooShort);
    }
    let kind = Kind::from_u8(body[1]).ok_or(DecodeFault::UnknownKind)?;
    let req: ReqId = body[2..BODY_HEADER].try_into().expect("16 bytes");
    let p = &body[BODY_HEADER..];
    let exactly = |n: usize| {
        if p.len() == n {
            Ok(())
        } else {
            Err(DecodeFault::BadLength)
        }
    };
    let reason = |b: u8| Reason::from_u8(b).ok_or(DecodeFault::BadField);
    match kind {
        Kind::Request => {
            let Some(&name_len) = p.first() else {
                return Err(DecodeFault::BadLength);
            };
            let name_len = name_len as usize;
            if name_len == 0 || name_len > MAX_NAME_LEN {
                return Err(DecodeFault::BadField);
            }
            exactly(1 + name_len)?;
            Ok(Msg::Request { req, name: &p[1..] })
        }
        Kind::Offer => {
            exactly(4 + SHA_LEN + 2)?;
            let size = get32(p);
            let chunk_len = get16(&p[4 + SHA_LEN..]);
            if size == 0 || size > MAX_MAP_BYTES || chunk_len == 0 || chunk_len > MAX_CHUNK {
                return Err(DecodeFault::BadField);
            }
            Ok(Msg::Offer {
                req,
                size,
                sha256: p[4..4 + SHA_LEN].try_into().expect("32 bytes"),
                chunk_len,
            })
        }
        Kind::Deny => {
            exactly(1)?;
            Ok(Msg::Deny {
                req,
                reason: reason(p[0])?,
            })
        }
        Kind::Cancel => {
            exactly(1)?;
            Ok(Msg::Cancel {
                req,
                reason: reason(p[0])?,
            })
        }
        Kind::Done => {
            exactly(1)?;
            Ok(Msg::Done {
                req,
                reason: reason(p[0])?,
            })
        }
        Kind::Leaving => {
            exactly(1)?;
            if req != [0; REQ_ID_LEN] {
                return Err(DecodeFault::BadField);
            }
            Ok(Msg::Leaving {
                why: LeaveWhy::from_u8(p[0]).ok_or(DecodeFault::BadField)?,
            })
        }
        Kind::Have => {
            exactly(0)?;
            Ok(Msg::Have { req })
        }
        Kind::Accept => {
            exactly(0)?;
            Ok(Msg::Accept { req })
        }
        Kind::Chunk => {
            if p.len() < 6 {
                return Err(DecodeFault::BadLength);
            }
            let offset = get32(p);
            let data_len = get16(&p[4..]) as usize;
            if data_len == 0 || data_len > MAX_CHUNK as usize {
                return Err(DecodeFault::BadField);
            }
            exactly(6 + data_len)?;
            if offset >= MAX_MAP_BYTES {
                return Err(DecodeFault::BadField);
            }
            Ok(Msg::Chunk {
                req,
                offset,
                data: &p[6..],
            })
        }
        Kind::Ack => {
            exactly(4)?;
            let next_offset = get32(p);
            if next_offset > MAX_MAP_BYTES {
                return Err(DecodeFault::BadField);
            }
            Ok(Msg::Ack { req, next_offset })
        }
        Kind::Progress => {
            exactly(2 * SLOTS)?;
            if req != [0; REQ_ID_LEN] {
                return Err(DecodeFault::BadField);
            }
            let mut slots = [SlotProgress::default(); SLOTS];
            for (i, s) in slots.iter_mut().enumerate() {
                let state = SlotState::from_u8(p[2 * i]).ok_or(DecodeFault::BadField)?;
                let percent = p[2 * i + 1];
                if percent > 100 {
                    return Err(DecodeFault::BadField);
                }
                *s = SlotProgress { state, percent };
            }
            Ok(Msg::Progress { slots })
        }
    }
}

fn header(kind: Kind, req: &[u8; REQ_ID_LEN], capacity: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(capacity);
    out.push(TYPE);
    out.extend_from_slice(&MAGIC);
    out.push(SUBSYSTEM);
    out.push(VERSION);
    out.push(kind as u8);
    out.extend_from_slice(req);
    out
}

/// The whole message, or `None` when a field is outside what the wire allows (the same
/// conditions wc2r-plus's encoders refuse).
pub fn encode(msg: &Msg) -> Option<Vec<u8>> {
    const ZERO: ReqId = [0; REQ_ID_LEN];
    match *msg {
        Msg::Request { req, name } => {
            if name.is_empty() || name.len() > MAX_NAME_LEN {
                return None;
            }
            let mut out = header(Kind::Request, &req, HEADER_LEN + 1 + name.len());
            out.push(name.len() as u8);
            out.extend_from_slice(name);
            Some(out)
        }
        Msg::Offer {
            req,
            size,
            sha256,
            chunk_len,
        } => {
            if size == 0 || size > MAX_MAP_BYTES || chunk_len == 0 || chunk_len > MAX_CHUNK {
                return None;
            }
            let mut out = header(Kind::Offer, &req, HEADER_LEN + 4 + SHA_LEN + 2);
            out.extend_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&sha256);
            out.extend_from_slice(&chunk_len.to_le_bytes());
            Some(out)
        }
        Msg::Deny { req, reason } => with_reason(Kind::Deny, &req, reason),
        Msg::Cancel { req, reason } => with_reason(Kind::Cancel, &req, reason),
        Msg::Done { req, reason } => with_reason(Kind::Done, &req, reason),
        Msg::Accept { req } => Some(header(Kind::Accept, &req, HEADER_LEN)),
        Msg::Have { req } => Some(header(Kind::Have, &req, HEADER_LEN)),
        Msg::Chunk { req, offset, data } => {
            if data.is_empty() || data.len() > MAX_CHUNK as usize || offset >= MAX_MAP_BYTES {
                return None;
            }
            let mut out = header(Kind::Chunk, &req, HEADER_LEN + 6 + data.len());
            out.extend_from_slice(&offset.to_le_bytes());
            out.extend_from_slice(&(data.len() as u16).to_le_bytes());
            out.extend_from_slice(data);
            Some(out)
        }
        Msg::Ack { req, next_offset } => {
            if next_offset > MAX_MAP_BYTES {
                return None;
            }
            let mut out = header(Kind::Ack, &req, HEADER_LEN + 4);
            out.extend_from_slice(&next_offset.to_le_bytes());
            Some(out)
        }
        Msg::Progress { slots } => {
            if slots.iter().any(|s| s.percent > 100) {
                return None;
            }
            let mut out = header(Kind::Progress, &ZERO, HEADER_LEN + 2 * SLOTS);
            for s in slots {
                out.push(s.state as u8);
                out.push(s.percent);
            }
            Some(out)
        }
        Msg::Leaving { why } => {
            let mut out = header(Kind::Leaving, &ZERO, HEADER_LEN + 1);
            out.push(why as u8);
            Some(out)
        }
    }
}

fn with_reason(kind: Kind, req: &ReqId, reason: Reason) -> Option<Vec<u8>> {
    let mut out = header(kind, req, HEADER_LEN + 1);
    out.push(reason as u8);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQ: ReqId = [7; REQ_ID_LEN];

    fn round_trip(msg: Msg) -> Vec<u8> {
        let bytes = encode(&msg).expect("encodes");
        assert_eq!(&bytes[..5], &[0xE7, b'W', b'2', b'R', b'M']);
        assert_eq!(decode(&bytes[ENVELOPE..]), Ok(msg), "{msg:?}");
        bytes
    }

    #[test]
    fn every_kind_round_trips_at_the_length_protocol_md_gives() {
        let sha = [0xab; SHA_LEN];
        assert_eq!(
            round_trip(Msg::Request {
                req: REQ,
                name: b"ridge.pud"
            })
            .len(),
            24 + 9
        );
        assert_eq!(
            round_trip(Msg::Offer {
                req: REQ,
                size: 4000,
                sha256: sha,
                chunk_len: 960
            })
            .len(),
            61
        );
        assert_eq!(
            round_trip(Msg::Deny {
                req: REQ,
                reason: Reason::Busy
            })
            .len(),
            24
        );
        assert_eq!(round_trip(Msg::Accept { req: REQ }).len(), 23);
        assert_eq!(
            round_trip(Msg::Chunk {
                req: REQ,
                offset: 960,
                data: &[1, 2, 3]
            })
            .len(),
            29 + 3
        );
        assert_eq!(
            round_trip(Msg::Ack {
                req: REQ,
                next_offset: 1920
            })
            .len(),
            27
        );
        assert_eq!(
            round_trip(Msg::Cancel {
                req: REQ,
                reason: Reason::Declined
            })
            .len(),
            24
        );
        assert_eq!(
            round_trip(Msg::Done {
                req: REQ,
                reason: Reason::Ok
            })
            .len(),
            24
        );
        assert_eq!(round_trip(Msg::Have { req: REQ }).len(), 23);
        let mut slots = [SlotProgress::default(); SLOTS];
        slots[2] = SlotProgress {
            state: SlotState::Uploading,
            percent: 55,
        };
        assert_eq!(round_trip(Msg::Progress { slots }).len(), 39);
        assert_eq!(
            round_trip(Msg::Leaving {
                why: LeaveWhy::BadName
            })
            .len(),
            24
        );
        let full = round_trip(Msg::Chunk {
            req: REQ,
            offset: 0,
            data: &[0; 960],
        });
        assert_eq!(full.len(), MAX_MESSAGE_LEN);
        assert!(MAX_MESSAGE_LEN <= crate::peer::GAME_LIMIT);
    }

    #[test]
    fn the_bytes_are_laid_out_as_documented() {
        let m = encode(&Msg::Offer {
            req: REQ,
            size: 0x0003_0201,
            sha256: [9; 32],
            chunk_len: 0x0203,
        })
        .unwrap();
        assert_eq!(m[5], 1, "version");
        assert_eq!(m[6], 2, "kind OFFER");
        assert_eq!(&m[7..23], &REQ);
        assert_eq!(&m[23..27], &[1, 2, 3, 0], "size little-endian");
        assert_eq!(&m[27..59], &[9; 32]);
        assert_eq!(&m[59..61], &[3, 2]);
        let c = encode(&Msg::Chunk {
            req: REQ,
            offset: 5,
            data: b"xy",
        })
        .unwrap();
        assert_eq!(&c[23..29], &[5, 0, 0, 0, 2, 0]);
        assert_eq!(&c[29..], b"xy");
    }

    #[test]
    fn short_foreign_and_unknown_messages_are_refused() {
        assert_eq!(decode(&[]), Err(DecodeFault::TooShort));
        assert_eq!(decode(&[1]), Err(DecodeFault::TooShort));
        assert_eq!(decode(&[2, 4]), Err(DecodeFault::UnsupportedVersion));
        assert_eq!(
            decode(&[1, 4, 0, 0]),
            Err(DecodeFault::TooShort),
            "shorter than the header"
        );
        let mut m = encode(&Msg::Accept { req: REQ }).unwrap();
        m[6] = 0;
        assert_eq!(decode(&m[5..]), Err(DecodeFault::UnknownKind));
        m[6] = 12;
        assert_eq!(decode(&m[5..]), Err(DecodeFault::UnknownKind));
    }

    #[test]
    fn a_wrong_length_is_dropped_for_every_kind() {
        let sha = [1; 32];
        let samples: Vec<Vec<u8>> = vec![
            encode(&Msg::Request {
                req: REQ,
                name: b"a.pud",
            })
            .unwrap(),
            encode(&Msg::Offer {
                req: REQ,
                size: 10,
                sha256: sha,
                chunk_len: 10,
            })
            .unwrap(),
            encode(&Msg::Deny {
                req: REQ,
                reason: Reason::Busy,
            })
            .unwrap(),
            encode(&Msg::Accept { req: REQ }).unwrap(),
            encode(&Msg::Chunk {
                req: REQ,
                offset: 0,
                data: &[1, 2],
            })
            .unwrap(),
            encode(&Msg::Ack {
                req: REQ,
                next_offset: 1,
            })
            .unwrap(),
            encode(&Msg::Cancel {
                req: REQ,
                reason: Reason::Busy,
            })
            .unwrap(),
            encode(&Msg::Done {
                req: REQ,
                reason: Reason::Ok,
            })
            .unwrap(),
            encode(&Msg::Progress {
                slots: [SlotProgress::default(); SLOTS],
            })
            .unwrap(),
            encode(&Msg::Have { req: REQ }).unwrap(),
            encode(&Msg::Leaving {
                why: LeaveWhy::DownloadsOff,
            })
            .unwrap(),
        ];
        for good in samples {
            for cut in HEADER_LEN..good.len() {
                assert!(
                    decode(&good[ENVELOPE..cut]).is_err(),
                    "kind {} cut to {cut}",
                    good[6]
                );
            }
            let mut longer = good.clone();
            longer.push(0);
            assert!(
                decode(&longer[ENVELOPE..]).is_err(),
                "kind {} one byte long",
                good[6]
            );
        }
    }

    #[test]
    fn out_of_range_fields_are_refused() {
        let bad = |bytes: Vec<u8>| decode(&bytes[ENVELOPE..]).map(|_| ());
        // OFFER with size 0, too big, chunk 0, chunk too big.
        for (size, chunk) in [
            (0u32, 10u16),
            (MAX_MAP_BYTES + 1, 10),
            (10, 0),
            (10, MAX_CHUNK + 1),
        ] {
            let mut m = encode(&Msg::Offer {
                req: REQ,
                size: 10,
                sha256: [0; 32],
                chunk_len: 10,
            })
            .unwrap();
            m[23..27].copy_from_slice(&size.to_le_bytes());
            m[59..61].copy_from_slice(&chunk.to_le_bytes());
            assert_eq!(bad(m), Err(DecodeFault::BadField), "{size} {chunk}");
        }
        // A reason past the last one.
        let mut m = encode(&Msg::Deny {
            req: REQ,
            reason: Reason::Busy,
        })
        .unwrap();
        m[23] = 15;
        assert_eq!(bad(m), Err(DecodeFault::BadField));
        // A request with a zero or oversize name length.
        let mut m = encode(&Msg::Request {
            req: REQ,
            name: b"a.pud",
        })
        .unwrap();
        m[23] = 0;
        assert_eq!(bad(m.clone()), Err(DecodeFault::BadField));
        m[23] = 65;
        assert_eq!(bad(m), Err(DecodeFault::BadField));
        // A chunk at or past the map limit, with zero or oversize data length.
        let mut m = encode(&Msg::Chunk {
            req: REQ,
            offset: 0,
            data: &[1],
        })
        .unwrap();
        m[23..27].copy_from_slice(&MAX_MAP_BYTES.to_le_bytes());
        assert_eq!(bad(m.clone()), Err(DecodeFault::BadField));
        m[23..27].copy_from_slice(&0u32.to_le_bytes());
        m[27..29].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(bad(m), Err(DecodeFault::BadField));
        // An ACK past the limit (the limit itself is allowed: the last byte).
        let mut m = encode(&Msg::Ack {
            req: REQ,
            next_offset: MAX_MAP_BYTES,
        })
        .unwrap();
        assert!(bad(m.clone()).is_ok());
        m[23..27].copy_from_slice(&(MAX_MAP_BYTES + 1).to_le_bytes());
        assert_eq!(bad(m), Err(DecodeFault::BadField));
        // PROGRESS and LEAVING must carry a zero request id.
        let mut m = encode(&Msg::Progress {
            slots: [SlotProgress::default(); SLOTS],
        })
        .unwrap();
        m[7] = 1;
        assert_eq!(bad(m), Err(DecodeFault::BadField));
        let mut m = encode(&Msg::Leaving {
            why: LeaveWhy::BadName,
        })
        .unwrap();
        m[7] = 1;
        assert_eq!(bad(m), Err(DecodeFault::BadField));
        // A leave reason of zero or past the last.
        for why in [0u8, 5] {
            let mut m = encode(&Msg::Leaving {
                why: LeaveWhy::BadName,
            })
            .unwrap();
            m[23] = why;
            assert_eq!(bad(m), Err(DecodeFault::BadField), "why {why}");
        }
        // A slot state past the last, and a percent over 100.
        let mut m = encode(&Msg::Progress {
            slots: [SlotProgress::default(); SLOTS],
        })
        .unwrap();
        m[23] = 8;
        assert_eq!(bad(m.clone()), Err(DecodeFault::BadField));
        m[23] = 2;
        m[24] = 101;
        assert_eq!(bad(m), Err(DecodeFault::BadField));
    }

    #[test]
    fn the_encoder_refuses_what_the_decoder_would() {
        assert!(
            encode(&Msg::Request {
                req: REQ,
                name: b""
            })
            .is_none()
        );
        assert!(
            encode(&Msg::Request {
                req: REQ,
                name: &[b'a'; 65]
            })
            .is_none()
        );
        assert!(
            encode(&Msg::Offer {
                req: REQ,
                size: 0,
                sha256: [0; 32],
                chunk_len: 1
            })
            .is_none()
        );
        assert!(
            encode(&Msg::Offer {
                req: REQ,
                size: 1,
                sha256: [0; 32],
                chunk_len: 961
            })
            .is_none()
        );
        assert!(
            encode(&Msg::Chunk {
                req: REQ,
                offset: 0,
                data: &[]
            })
            .is_none()
        );
        assert!(
            encode(&Msg::Chunk {
                req: REQ,
                offset: MAX_MAP_BYTES,
                data: &[1]
            })
            .is_none()
        );
        assert!(
            encode(&Msg::Chunk {
                req: REQ,
                offset: 0,
                data: &[0; 961]
            })
            .is_none()
        );
        assert!(
            encode(&Msg::Ack {
                req: REQ,
                next_offset: MAX_MAP_BYTES + 1
            })
            .is_none()
        );
        let mut slots = [SlotProgress::default(); SLOTS];
        slots[7].percent = 101;
        assert!(encode(&Msg::Progress { slots }).is_none());
    }

    #[test]
    fn no_input_makes_the_decoder_panic() {
        // Every prefix and every single-byte mutation of a few messages.
        let msgs = [
            encode(&Msg::Request {
                req: REQ,
                name: b"ridge.pud",
            })
            .unwrap(),
            encode(&Msg::Chunk {
                req: REQ,
                offset: 960,
                data: &[5; 40],
            })
            .unwrap(),
            encode(&Msg::Progress {
                slots: [SlotProgress::default(); SLOTS],
            })
            .unwrap(),
        ];
        for m in msgs {
            for cut in 0..=m.len() {
                let _ = decode(&m[ENVELOPE.min(cut)..cut]);
            }
            for i in ENVELOPE..m.len() {
                for v in [0u8, 1, 0x7f, 0xff] {
                    let mut x = m.clone();
                    x[i] = v;
                    let _ = decode(&x[ENVELOPE..]);
                }
            }
        }
    }
}
