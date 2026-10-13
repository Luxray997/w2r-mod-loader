//! Tolerant reader. Damage after the header is not an error: parsing stops at the last whole
//! record and `complete` stays false, so a cut-off recording still plays as far as it goes.

use w2r_stats::slot::{self, SLOT_LEN, SLOTS};

use crate::format::{
    Footer, Header, LOBBY_LEN, MAGIC, SLOT_BACKUP_LEN, MAX_CHAT, MAX_HEADER, MAX_PACKET, MAX_TEXT, NAMES_LEN,
    OWNER_LEN, REC_CAMERA, REC_CHAT, REC_FOOTER, REC_TURN, VERSION,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    pub turn: u32,
    pub packets: Vec<(u8, Vec<u8>)>,
}

/// One chat line that was shown on screen (see `Writer::chat`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chat {
    /// The turn that was next to run when the line appeared.
    pub turn: u32,
    pub who: u8,
    pub mode: u8,
    pub duration_ms: u32,
    pub text: Vec<u8>,
}

/// Where the recorded player's camera was looking (see `Writer::camera`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Camera {
    /// The turn that was next to run when the view was there.
    pub turn: u32,
    /// The centre of the map view, in map pixels.
    pub x: u32,
    pub y: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replay {
    pub header: Header,
    pub turns: Vec<Turn>,
    /// Chat lines in the order they appeared.
    pub chats: Vec<Chat>,
    /// The recorded player's camera, in order. Empty in recordings made before it was kept.
    pub cameras: Vec<Camera>,
    /// The last footer: present once the match produced a result or the recording was closed.
    pub footer: Option<Footer>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotAReplay,
    UnsupportedVersion(u16),
    BadHeader,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NotAReplay => f.write_str("not a replay file"),
            Error::UnsupportedVersion(v) => write!(f, "unsupported replay version {v}"),
            Error::BadHeader => f.write_str("damaged replay header"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Copy)]
struct Cur<'a>(&'a [u8]);

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    }
    fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for i in 0..10 {
            let b = self.u8()?;
            v |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
    fn len(&mut self, max: usize) -> Option<usize> {
        let n = usize::try_from(self.varint()?).ok()?;
        (n <= max).then_some(n)
    }
    fn text(&mut self) -> Option<String> {
        let n = self.len(MAX_TEXT)?;
        Some(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

fn header(raw: &[u8]) -> Option<Header> {
    let mut c = Cur(raw);
    let build = c.text()?;
    let client = c.text()?;
    let fingerprint = c.take(4)?.try_into().ok()?;
    let map = c.text()?;
    let started = u64::from_le_bytes(c.take(8)?.try_into().ok()?);
    let local = c.u8()?;
    let mut slots = [[0u8; SLOT_LEN]; SLOTS];
    for s in &mut slots {
        s.copy_from_slice(c.take(SLOT_LEN)?);
    }
    let owner = c.take(OWNER_LEN)?.try_into().ok()?;
    let names = c.take(NAMES_LEN)?.try_into().ok()?;
    let n = c.len(MAX_TEXT)?;
    let seed = c.take(n)?.to_vec();
    // Added after the first version of the header: absent in older files.
    let lobby = if c.0.is_empty() {
        Vec::new()
    } else {
        let n = c.len(LOBBY_LEN)?;
        c.take(n)?.to_vec()
    };
    // Added after the lobby block: absent in older files.
    let map_sha = if c.0.is_empty() {
        None
    } else {
        let n = c.len(32)?;
        c.take(n)?.try_into().ok()
    };
    // Added after the map hash: absent in older files.
    let slot_backup = if c.0.is_empty() {
        Vec::new()
    } else {
        let n = c.len(SLOT_BACKUP_LEN)?;
        c.take(n)?.to_vec()
    };
    Some(Header {
        build,
        client,
        fingerprint,
        map,
        started,
        local,
        slots,
        owner,
        names,
        seed,
        lobby,
        map_sha,
        slot_backup,
    })
}

/// One chat record after its tag byte; `None` if it is cut off or invalid.
fn chat_record(r: &mut Cur) -> Option<Chat> {
    let turn = u32::try_from(r.varint()?).ok()?;
    let (who, mode) = (r.u8()?, r.u8()?);
    let duration_ms = u32::try_from(r.varint()?).ok()?;
    let n = r.len(MAX_CHAT)?;
    Some(Chat {
        turn,
        who,
        mode,
        duration_ms,
        text: r.take(n)?.to_vec(),
    })
}

/// One camera record after its tag byte; `None` if it is cut off or invalid.
fn camera_record(r: &mut Cur) -> Option<Camera> {
    Some(Camera {
        turn: u32::try_from(r.varint()?).ok()?,
        x: u32::try_from(r.varint()?).ok()?,
        y: u32::try_from(r.varint()?).ok()?,
    })
}

/// One turn record after its tag byte; `None` if it is cut off or invalid.
fn turn_record(r: &mut Cur, after: Option<u32>) -> Option<Turn> {
    let turn = u32::try_from(r.varint()?).ok()?;
    let count = r.u8()?;
    if count > 8 || after.is_some_and(|t| turn <= t) {
        return None;
    }
    let mut packets = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let slot = r.u8()?;
        let n = r.len(MAX_PACKET)?;
        if slot >= 8 {
            return None;
        }
        packets.push((slot, r.take(n)?.to_vec()));
    }
    Some(Turn { turn, packets })
}

/// Checks the magic and version and reads the header; the cursor is left at the first record.
fn start(raw: &[u8]) -> Result<(Header, Cur<'_>), Error> {
    let mut c = Cur(raw);
    if c.take(4) != Some(MAGIC) {
        return Err(Error::NotAReplay);
    }
    let version = c
        .take(2)
        .map(|b| u16::from_le_bytes(b.try_into().unwrap()))
        .ok_or(Error::NotAReplay)?;
    if version != VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    let len = c.u32().ok_or(Error::BadHeader)? as usize;
    if len > MAX_HEADER {
        return Err(Error::BadHeader);
    }
    let header = header(c.take(len).ok_or(Error::BadHeader)?).ok_or(Error::BadHeader)?;
    Ok((header, c))
}

impl Replay {
    /// Just the header of a file, without reading the turns. For a server that keeps the file
    /// as it came and only needs to know which match it is.
    pub fn parse_header(raw: &[u8]) -> Result<Header, Error> {
        start(raw).map(|(header, _)| header)
    }

    pub fn parse(raw: &[u8]) -> Result<Replay, Error> {
        let (header, mut c) = start(raw)?;
        let mut turns: Vec<Turn> = Vec::new();
        let mut footer = None;
        let mut chats: Vec<Chat> = Vec::new();
        let mut cameras: Vec<Camera> = Vec::new();
        // Each record is read from a copy of the cursor, so a cut-off one is dropped whole.
        loop {
            let mut r = c;
            match r.u8() {
                Some(REC_TURN) => match turn_record(&mut r, turns.last().map(|t| t.turn)) {
                    Some(t) => turns.push(t),
                    None => break,
                },
                // A footer is written when the result is known and again at the end; turns may
                // follow one. The last one wins.
                Some(REC_FOOTER) => {
                    let (Some(res), Some(t), Some(m)) = (r.take(8), r.u32(), r.u32()) else {
                        break;
                    };
                    footer = Some(Footer {
                        results: res.try_into().unwrap(),
                        turns: t,
                        sync_mismatch: m,
                    });
                }
                Some(REC_CHAT) => match chat_record(&mut r) {
                    Some(chat) => chats.push(chat),
                    None => break,
                },
                Some(REC_CAMERA) => match camera_record(&mut r) {
                    Some(cam) => cameras.push(cam),
                    None => break,
                },
                _ => break,
            }
            c = r;
        }
        Ok(Replay {
            header,
            turns,
            chats,
            cameras,
            footer,
        })
    }

    pub fn complete(&self) -> bool {
        self.footer.is_some()
    }

    /// Running totals of what each slot did on purpose (see `packet::player_actions`): entry `i`
    /// is the count over the first `i + 1` recorded turns.
    pub fn action_totals(&self) -> Vec<[u32; 8]> {
        let mut total = [0u32; 8];
        self.turns
            .iter()
            .map(|t| {
                for (slot, data) in &t.packets {
                    if let Some(n) = total.get_mut(usize::from(*slot)) {
                        *n += crate::packet::player_actions(data) as u32;
                    }
                }
                total
            })
            .collect()
    }

    /// The last recorded turn number.
    pub fn last_turn(&self) -> u32 {
        self.turns.last().map_or(0, |t| t.turn)
    }

    /// The slot backup to put back before `StartGame` runs, and whether it was recorded. An older
    /// recording has none; the backup is the lobby's own slots as the game starts, so the lobby
    /// block's slot table stands in for it.
    pub fn slot_backup(&self) -> Option<(bool, Vec<u8>)> {
        if self.header.slot_backup.len() == SLOT_BACKUP_LEN {
            return Some((true, self.header.slot_backup.clone()));
        }
        self.header
            .lobby
            .get(..SLOT_BACKUP_LEN)
            .map(|b| (false, b.to_vec()))
    }

    /// The lobby entry (before the game moved the players to their start positions) that the
    /// recording player sat in, found by net id and name; `header.local` is the position after.
    pub fn local_entry(&self) -> Option<usize> {
        let lobby = &self.header.lobby;
        let local = usize::from(self.header.local);
        if lobby.len() < SLOTS * SLOT_LEN || local >= SLOTS {
            return None;
        }
        let key = |s: &[u8]| -> Vec<u8> {
            let mut k = s[4..8].to_vec();
            k.extend_from_slice(&s[slot::NAME_AT..SLOT_LEN]);
            k
        };
        let want = key(&self.header.slots[local]);
        (0..SLOTS).find(|&i| key(&lobby[i * SLOT_LEN..(i + 1) * SLOT_LEN]) == want)
    }

    pub fn players(&self) -> Vec<slot::Player> {
        slot::parse_table(&self.header.slots)
    }
}
