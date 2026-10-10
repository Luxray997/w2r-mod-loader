//! Streaming writer: header first, then one record per turn, then the footer. Generic over
//! `Write` so tests use a `Vec` and the host a buffered file; the host flushes periodically so a
//! crash loses seconds, not the match.

use std::io::{self, Write};

use crate::format::{
    Footer, Header, MAGIC, MAX_CHAT, MAX_HEADER, MAX_PACKET, REC_CHAT, REC_FOOTER, REC_TURN,
    VERSION, put_varint,
};

pub struct Writer<W: Write> {
    out: W,
    scratch: Vec<u8>,
    last_turn: Option<u32>,
}

impl<W: Write> Writer<W> {
    pub fn new(mut out: W, header: &Header) -> io::Result<Self> {
        let h = header.encode();
        if h.len() > MAX_HEADER {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "header too big",
            ));
        }
        out.write_all(MAGIC)?;
        out.write_all(&VERSION.to_le_bytes())?;
        out.write_all(&(h.len() as u32).to_le_bytes())?;
        out.write_all(&h)?;
        Ok(Writer {
            out,
            scratch: Vec::new(),
            last_turn: None,
        })
    }

    /// One turn's packets as `(slot, bytes)`. Turns must be strictly increasing; a repeat or a
    /// step back is refused (the reader would otherwise stop there). Empty and oversized packets
    /// are skipped, and a turn with none still gets a record so pacing survives.
    pub fn turn(&mut self, turn: u32, packets: &[(u8, &[u8])]) -> io::Result<()> {
        if self.last_turn.is_some_and(|t| turn <= t) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "turns must increase",
            ));
        }
        let packets: Vec<_> = packets
            .iter()
            .filter(|(s, d)| *s < 8 && !d.is_empty() && d.len() <= MAX_PACKET)
            .collect();
        let b = &mut self.scratch;
        b.clear();
        b.push(REC_TURN);
        put_varint(b, u64::from(turn));
        b.push(packets.len() as u8);
        for (slot, data) in packets {
            b.push(*slot);
            put_varint(b, data.len() as u64);
            b.extend_from_slice(data);
        }
        self.out.write_all(b)?;
        self.last_turn = Some(turn);
        Ok(())
    }

    /// One chat line that was shown on screen while `turn` was the next to run: who it is from
    /// (a player index), the target mode (all, allies, ...), how long it stays, and its text.
    /// Chat is not part of the turn packets (the game sends it straight between machines), so it
    /// is kept in its own records. Text over 255 bytes is cut.
    pub fn chat(
        &mut self,
        turn: u32,
        who: u8,
        mode: u8,
        duration_ms: u32,
        text: &[u8],
    ) -> io::Result<()> {
        let text = &text[..text.len().min(MAX_CHAT)];
        let b = &mut self.scratch;
        b.clear();
        b.push(REC_CHAT);
        put_varint(b, u64::from(turn));
        b.push(who);
        b.push(mode);
        put_varint(b, u64::from(duration_ms));
        put_varint(b, text.len() as u64);
        b.extend_from_slice(text);
        self.out.write_all(b)
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    /// Writes a footer record and flushes. The recording may go on afterwards (a defeated player's
    /// team can still be playing); the reader keeps the last footer it sees.
    pub fn footer(&mut self, footer: &Footer) -> io::Result<()> {
        let mut b = vec![REC_FOOTER];
        b.extend_from_slice(&footer.results);
        b.extend_from_slice(&footer.turns.to_le_bytes());
        b.extend_from_slice(&footer.sync_mismatch.to_le_bytes());
        self.out.write_all(&b)?;
        self.out.flush()
    }

    /// Writes the final footer and hands the sink back.
    pub fn finish(mut self, footer: &Footer) -> io::Result<W> {
        self.footer(footer)?;
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::Replay;
    use w2r_stats::slot::{SLOT_LEN, SLOTS};

    pub(crate) fn header() -> Header {
        Header {
            build: "2818".into(),
            client: "w2rpm test".into(),
            fingerprint: [1, 2, 3, 4],
            map: "(2)Cool Map".into(),
            started: 1_700_000_000,
            local: 1,
            slots: [[7; SLOT_LEN]; SLOTS],
            owner: [2; crate::format::OWNER_LEN],
            names: [3; crate::format::NAMES_LEN],
            seed: Header::seed_bytes(0xdead_beef, 0x1234_5678),
            lobby: (0..crate::format::LOBBY_LEN).map(|i| i as u8).collect(),
        }
    }

    fn footer() -> Footer {
        Footer {
            results: [3, 2, 0, 0, 2, 2, 2, 2],
            turns: 12,
            sync_mismatch: 0,
        }
    }

    #[test]
    fn round_trip() {
        let mut w = Writer::new(Vec::new(), &header()).unwrap();
        w.turn(0, &[(0, &[5]), (1, &[5])]).unwrap();
        w.turn(1, &[]).unwrap();
        w.turn(5, &[(1, &[0x19, 1, 2, 3, 4]), (0, &[])]).unwrap();
        let bytes = w.finish(&footer()).unwrap();
        let r = Replay::parse(&bytes).unwrap();
        assert_eq!(r.header, header());
        assert_eq!(r.header.game_seed(), Some(0xdead_beef));
        assert_eq!(r.header.rng_at_start(), Some(0x1234_5678));
        assert!(r.complete());
        assert_eq!(r.footer, Some(footer()));
        assert_eq!(r.turns.len(), 3);
        assert_eq!(r.turns[0].packets, [(0, vec![5]), (1, vec![5])]);
        assert!(r.turns[1].packets.is_empty());
        assert_eq!(r.turns[2].turn, 5);
        assert_eq!(r.turns[2].packets, [(1, vec![0x19, 1, 2, 3, 4])]);
        assert_eq!(r.last_turn(), 5);
    }

    #[test]
    fn action_totals_count_commands_but_not_idle_or_sync_records() {
        let mut w = Writer::new(Vec::new(), &header()).unwrap();
        w.turn(0, &[(0, &[5, 0x19, 1, 2, 3, 4][..5]), (1, &[0x08, 2, 1, 2, 3, 4])])
            .unwrap();
        w.turn(1, &[(0, &[0x08, 2, 1, 2, 3, 4]), (1, &[5])]).unwrap();
        let bytes = w.finish(&footer()).unwrap();
        let r = Replay::parse(&bytes).unwrap();
        let totals = r.action_totals();
        assert_eq!(totals.len(), 2);
        assert_eq!(totals[0][1], 1);
        assert_eq!(totals[1][0], totals[0][0] + 1);
        assert_eq!(totals[1][1], 1);
    }

    #[test]
    fn turns_after_an_interim_footer_are_kept_and_the_last_footer_wins() {
        let mut w = Writer::new(Vec::new(), &header()).unwrap();
        w.turn(0, &[(0, &[5])]).unwrap();
        let early = Footer {
            results: [2, 0, 0, 0, 0, 0, 0, 0],
            turns: 1,
            sync_mismatch: 0,
        };
        w.footer(&early).unwrap();
        // valid and "has a result" right now, with nothing more written
        let r = Replay::parse(&w.out).unwrap();
        assert_eq!(r.footer, Some(early));
        w.turn(1, &[(0, &[5])]).unwrap();
        let bytes = w.finish(&footer()).unwrap();
        let r = Replay::parse(&bytes).unwrap();
        assert_eq!(r.turns.len(), 2);
        assert_eq!(r.footer, Some(footer()));
    }

    #[test]
    fn a_header_without_a_lobby_block_reads_as_an_old_recording() {
        let mut h = header();
        h.lobby = Vec::new();
        let w = Writer::new(Vec::new(), &h).unwrap();
        let r = Replay::parse(&w.out).unwrap();
        assert!(r.header.lobby.is_empty());
        // and a header that ends right after the seed (the first file version) parses too
        let mut raw = w.out.clone();
        let len = u32::from_le_bytes(raw[6..10].try_into().unwrap()) - 1;
        raw[6..10].copy_from_slice(&len.to_le_bytes());
        raw.remove(10 + len as usize);
        assert!(Replay::parse(&raw).unwrap().header.lobby.is_empty());
    }

    #[test]
    fn chat_lines_round_trip_between_turns_and_survive_a_cut() {
        let mut w = Writer::new(Vec::new(), &header()).unwrap();
        w.turn(0, &[(0, &[5])]).unwrap();
        w.chat(1, 0, 2, 40, b"hello team").unwrap();
        w.turn(1, &[(0, &[5])]).unwrap();
        w.chat(2, 3, 0, 0, &[b'x'; 400]).unwrap();
        let full = w.out.clone();
        let r = Replay::parse(&full).unwrap();
        assert_eq!(r.turns.len(), 2);
        assert_eq!(r.chats.len(), 2);
        assert_eq!(
            r.chats[0],
            crate::Chat {
                turn: 1,
                who: 0,
                mode: 2,
                duration_ms: 40,
                text: b"hello team".to_vec()
            }
        );
        assert_eq!(r.chats[1].text.len(), 255, "long text is cut");
        // a cut inside the last chat keeps everything before it
        let r = Replay::parse(&full[..full.len() - 3]).unwrap();
        assert_eq!((r.turns.len(), r.chats.len()), (2, 1));
    }

    #[test]
    fn turns_must_increase() {
        let mut w = Writer::new(Vec::new(), &header()).unwrap();
        w.turn(3, &[]).unwrap();
        assert!(w.turn(3, &[]).is_err());
        assert!(w.turn(2, &[]).is_err());
    }

    #[test]
    fn cut_off_file_keeps_whole_records() {
        let mut w = Writer::new(Vec::new(), &header()).unwrap();
        w.turn(0, &[(0, &[5])]).unwrap();
        w.turn(1, &[(0, &[0x19, 1, 2, 3, 4])]).unwrap();
        let bytes = w.out.clone();
        for cut in 1..8 {
            let r = Replay::parse(&bytes[..bytes.len() - cut]).unwrap();
            assert!(!r.complete());
            assert_eq!(r.turns.len(), 1, "cut {cut}");
        }
        let r = Replay::parse(&bytes).unwrap();
        assert_eq!(r.turns.len(), 2);
        assert!(!r.complete());
    }

    #[test]
    fn bad_files_are_refused() {
        assert!(Replay::parse(b"nope").is_err());
        let mut w = Writer::new(Vec::new(), &header()).unwrap();
        w.turn(0, &[]).unwrap();
        let mut bytes = w.finish(&footer()).unwrap();
        let mut bad = bytes.clone();
        bad[4] = 9;
        assert!(Replay::parse(&bad).is_err());
        // damaged header length
        bytes[6..10].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Replay::parse(&bytes).is_err());
        // cut inside the header
        assert!(Replay::parse(&bytes[..20]).is_err());
    }

    #[test]
    fn long_text_is_clipped_on_a_char_boundary() {
        let mut h = header();
        h.map = "é".repeat(2000);
        let w = Writer::new(Vec::new(), &h).unwrap();
        let r = Replay::parse(&w.out).unwrap();
        assert!(r.header.map.len() <= crate::format::MAX_TEXT);
        assert!(r.header.map.chars().all(|c| c == 'é'));
    }
}
