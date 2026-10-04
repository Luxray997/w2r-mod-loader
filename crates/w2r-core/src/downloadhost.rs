//! The host's side of map sharing (docs/map-download.md, slice 2): who may be offered the lobby's
//! map, the upload window and pacing, and every limit that stops one player from tying the host
//! up. Ported from wc2r-plus `map_download/download_host.cpp`. Pure state: a millisecond clock
//! and decoded messages go in, messages to send and net slots to remove come out. The glue
//! (slice 3) sends the outbox and carries out the removals.
//!
//! Net slot 0 is the host itself; slots 1 to 7 are the other players.

use crate::maplimits::*;
use crate::mapwire::{Msg, Reason, ReqId, SLOTS, SlotProgress, SlotState, encode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Up {
    None,
    Offered,
    Uploading,
    Done,
    Failed,
    Stalled,
    LeftNoMap,
    NoAnswer,
}

#[derive(Clone)]
struct Slot {
    state: Up,
    req: ReqId,
    sent: u32,
    acked: u32,
    offered_ms: u64,
    accepted_ms: u64,
    last_progress_ms: u64,
    progress_acked: u32,
    joined_ms: u64,
    present: bool,
    in_grace: bool,
    left_why: u8,
    last_request_ms: u64,
    offered: bool,
    uploads: u32,
    completed_same_sha: u32,
    completed_sha: [u8; 32],
    unused_offers: u32,
    unused_sha: [u8; 32],
    last_deny_ms: u64,
    denied_once: bool,
}

impl Default for Slot {
    fn default() -> Self {
        Slot {
            state: Up::None,
            req: [0; 16],
            sent: 0,
            acked: 0,
            offered_ms: 0,
            accepted_ms: 0,
            last_progress_ms: 0,
            progress_acked: 0,
            joined_ms: 0,
            present: false,
            in_grace: false,
            left_why: 0,
            last_request_ms: 0,
            offered: false,
            uploads: 0,
            completed_same_sha: 0,
            completed_sha: [0; 32],
            unused_offers: 0,
            unused_sha: [0; 32],
            last_deny_ms: 0,
            denied_once: false,
        }
    }
}

struct Served {
    name: Vec<u8>,
    bytes: Vec<u8>,
    sha: [u8; 32],
}

pub struct DownloadHost {
    slots: [Slot; SLOTS],
    serving: bool,
    countdown: bool,
    map: Option<Served>,
    tokens: f64,
    last_fill_ms: u64,
    progress_dirty: bool,
    last_progress_ms: u64,
    clock_started: bool,
    outbox: Vec<Out>,
    disconnects: Vec<u8>,
}

impl Default for DownloadHost {
    fn default() -> Self {
        DownloadHost::new()
    }
}

impl DownloadHost {
    pub fn new() -> DownloadHost {
        DownloadHost {
            slots: core::array::from_fn(|_| Slot::default()),
            serving: true,
            countdown: false,
            map: None,
            tokens: 0.0,
            last_fill_ms: 0,
            progress_dirty: true,
            last_progress_ms: 0,
            clock_started: false,
            outbox: Vec::new(),
            disconnects: Vec::new(),
        }
    }

    pub fn set_serving(&mut self, on: bool) {
        self.serving = on;
    }

    /// The start countdown is running: nothing new is offered.
    pub fn set_countdown(&mut self, running: bool) {
        self.countdown = running;
    }

    /// The map the lobby is showing, as the game selected it: its name (what a request must say)
    /// and the file. A file outside 1 to 262,144 bytes cannot be served and clears the map.
    /// Open transfers are cancelled either way: they were for another map.
    pub fn set_servable_map(&mut self, name: &[u8], bytes: Vec<u8>) -> bool {
        self.cancel_open_transfers(Reason::NotCurrentMap);
        if bytes.is_empty() || bytes.len() > MAX_MAP_BYTES as usize || name.is_empty() {
            self.map = None;
            return false;
        }
        let sha = sha256(&bytes);
        self.map = Some(Served {
            name: name.to_vec(),
            bytes,
            sha,
        });
        true
    }

    pub fn clear_servable_map(&mut self) {
        self.cancel_open_transfers(Reason::NotCurrentMap);
        self.map = None;
    }

    pub fn has_map(&self) -> bool {
        self.map.is_some()
    }

    pub fn note_peer_joined(&mut self, slot: usize, now: u64, can_download: bool) {
        if slot == 0 || slot >= SLOTS {
            return;
        }
        self.slots[slot] = Slot {
            present: true,
            joined_ms: now,
            in_grace: can_download,
            ..Slot::default()
        };
        self.progress_dirty = true;
    }

    pub fn note_peer_left(&mut self, slot: usize) {
        if slot < SLOTS {
            self.slots[slot] = Slot::default();
            self.progress_dirty = true;
        }
    }

    /// A fresh lobby: everything forgotten.
    pub fn reset(&mut self) {
        let serving = self.serving;
        *self = DownloadHost::new();
        self.serving = serving;
    }

    pub fn take_outbox(&mut self) -> Vec<Out> {
        core::mem::take(&mut self.outbox)
    }

    /// Net slots to remove from the lobby (a decline, a stall, no answer).
    pub fn take_disconnects(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.disconnects)
    }

    fn push(&mut self, to: Target, msg: &Msg) {
        if let Some(bytes) = encode(msg) {
            self.outbox.push(Out { to, bytes });
        }
    }

    fn cancel_open_transfers(&mut self, reason: Reason) {
        for i in 1..SLOTS {
            let s = &self.slots[i];
            if s.state != Up::Offered && s.state != Up::Uploading {
                continue;
            }
            let req = s.req;
            self.push(Target::Slot(i as u8), &Msg::Cancel { req, reason });
            self.slots[i].state = Up::None;
            self.progress_dirty = true;
        }
    }

    /// One DENY per player per [`DENY_MIN_INTERVAL_MS`]; the rest are dropped silently.
    fn deny(&mut self, now: u64, slot: usize, req: ReqId, reason: Reason) {
        let s = &mut self.slots[slot];
        if s.denied_once && now.saturating_sub(s.last_deny_ms) < DENY_MIN_INTERVAL_MS {
            return;
        }
        s.denied_once = true;
        s.last_deny_ms = now;
        self.push(Target::Slot(slot as u8), &Msg::Deny { req, reason });
    }

    fn concurrent_uploads(&self) -> usize {
        self.slots
            .iter()
            .filter(|s| s.state == Up::Uploading)
            .count()
    }

    fn count_completed(&mut self, slot: usize) {
        let sha = self.map.as_ref().map_or([0; 32], |m| m.sha);
        let s = &mut self.slots[slot];
        if s.completed_sha != sha {
            s.completed_sha = sha;
            s.completed_same_sha = 0;
        }
        s.completed_same_sha += 1;
    }

    fn count_unused_offer(&mut self, slot: usize) {
        let sha = self.map.as_ref().map_or([0; 32], |m| m.sha);
        let s = &mut self.slots[slot];
        if s.unused_sha != sha {
            s.unused_sha = sha;
            s.unused_offers = 0;
        }
        s.unused_offers += 1;
    }

    fn end_upload(&mut self, slot: usize, state: Up) {
        self.slots[slot].state = state;
        self.progress_dirty = true;
    }

    fn kick(&mut self, slot: usize, tell: Reason) {
        self.disconnects.push(slot as u8);
        let req = self.slots[slot].req;
        self.push(Target::Slot(slot as u8), &Msg::Cancel { req, reason: tell });
        let offered = self.slots[slot].state == Up::Offered;
        let next = if tell == Reason::Declined {
            Up::Failed
        } else if offered {
            Up::NoAnswer
        } else {
            Up::Stalled
        };
        self.end_upload(slot, next);
    }

    /// A decoded message from a player (net slot `sender`, 1 to 7). Anything not valid in the
    /// player's current state is dropped.
    pub fn on_client_message(&mut self, now: u64, sender: usize, msg: &Msg) {
        if sender == 0 || sender >= SLOTS {
            return;
        }
        match *msg {
            Msg::Leaving { why } => {
                let s = &mut self.slots[sender];
                if !s.present || s.state == Up::LeftNoMap {
                    return;
                }
                s.state = Up::LeftNoMap;
                s.left_why = why as u8;
                s.in_grace = false;
                self.progress_dirty = true;
            }
            Msg::Request { req, name } => self.on_request(now, sender, req, name),
            Msg::Accept { req } => self.on_accept(now, sender, req),
            Msg::Ack { req, next_offset } => self.on_ack(now, sender, req, next_offset),
            Msg::Cancel { req, reason } => {
                let (state, mine) = (self.slots[sender].state, self.slots[sender].req == req);
                if (state == Up::Offered || state == Up::Uploading) && mine {
                    if state == Up::Offered && reason == Reason::Declined {
                        self.kick(sender, Reason::Declined);
                    } else {
                        if state == Up::Offered {
                            self.count_unused_offer(sender);
                        }
                        self.end_upload(sender, Up::Failed);
                    }
                }
            }
            Msg::Done { req, .. } => {
                let (state, mine) = (self.slots[sender].state, self.slots[sender].req == req);
                if state == Up::Uploading && mine {
                    self.count_completed(sender);
                    self.end_upload(sender, Up::Done);
                }
            }
            Msg::Have { req } => {
                let (state, mine) = (self.slots[sender].state, self.slots[sender].req == req);
                if state != Up::Offered || !mine {
                    return;
                }
                self.count_unused_offer(sender);
                let s = &mut self.slots[sender];
                s.state = Up::None;
                s.in_grace = false;
                self.progress_dirty = true;
            }
            // A host never receives these from a player.
            Msg::Offer { .. } | Msg::Deny { .. } | Msg::Chunk { .. } | Msg::Progress { .. } => {}
        }
    }

    fn on_request(&mut self, now: u64, slot: usize, req: ReqId, name: &[u8]) {
        if !self.slots[slot].present {
            return;
        }
        if self.countdown {
            return self.deny(now, slot, req, Reason::Countdown);
        }
        if !self.serving {
            return self.deny(now, slot, req, Reason::ServingOff);
        }
        let Some(map) = &self.map else {
            return self.deny(now, slot, req, Reason::MapUnservable);
        };
        // Only the name is compared; it never selects a file.
        if map.name != name {
            return self.deny(now, slot, req, Reason::NotCurrentMap);
        }
        let (size, sha) = (map.bytes.len() as u32, map.sha);
        let s = &self.slots[slot];
        if s.state == Up::Offered || s.state == Up::Uploading {
            if s.req != req {
                self.deny(now, slot, req, Reason::Busy);
            }
            return;
        }
        if s.offered && now.saturating_sub(s.last_request_ms) < REQUEST_MIN_INTERVAL_MS {
            return self.deny(now, slot, req, Reason::RateLimited);
        }
        let same_map_done_twice =
            s.completed_same_sha >= MAX_COMPLETED_SAME_MAP_PER_STAY && s.completed_sha == sha;
        let offers_used_up =
            s.unused_offers >= MAX_UNUSED_OFFERS_PER_MAP_PER_STAY && s.unused_sha == sha;
        if s.uploads >= UPLOADS_PER_STAY || same_map_done_twice || offers_used_up {
            return self.deny(now, slot, req, Reason::RateLimited);
        }
        if self.concurrent_uploads() >= MAX_CONCURRENT_UPLOADS {
            return self.deny(now, slot, req, Reason::Busy);
        }
        let s = &mut self.slots[slot];
        s.req = req;
        s.state = Up::Offered;
        s.offered_ms = now;
        s.last_request_ms = now;
        s.offered = true;
        s.in_grace = false;
        self.push(
            Target::Slot(slot as u8),
            &Msg::Offer {
                req,
                size,
                sha256: sha,
                chunk_len: MAX_CHUNK,
            },
        );
        self.progress_dirty = true;
    }

    fn on_accept(&mut self, now: u64, slot: usize, req: ReqId) {
        let s = &self.slots[slot];
        if s.state != Up::Offered || s.req != req {
            return;
        }
        if self.concurrent_uploads() >= MAX_CONCURRENT_UPLOADS {
            // Both upload slots filled between the offer and the answer.
            self.push(
                Target::Slot(slot as u8),
                &Msg::Cancel {
                    req,
                    reason: Reason::Busy,
                },
            );
            self.slots[slot].state = Up::None;
            self.progress_dirty = true;
            return;
        }
        let s = &mut self.slots[slot];
        s.state = Up::Uploading;
        s.uploads += 1;
        s.accepted_ms = now;
        s.last_progress_ms = now;
        s.progress_acked = 0;
        s.sent = 0;
        s.acked = 0;
        self.progress_dirty = true;
    }

    fn on_ack(&mut self, now: u64, slot: usize, req: ReqId, next: u32) {
        let total = self.map.as_ref().map_or(0, |m| m.bytes.len() as u32);
        let s = &mut self.slots[slot];
        if s.state != Up::Uploading || s.req != req {
            return;
        }
        // Only forward, and never past what was sent.
        if next <= s.acked || next > s.sent {
            return;
        }
        s.acked = next;
        if s.acked == total || s.acked - s.progress_acked >= PROGRESS_BYTES {
            s.last_progress_ms = now;
            s.progress_acked = s.acked;
        }
        if s.acked == total {
            self.count_completed(slot);
            self.end_upload(slot, Up::Done);
        } else {
            self.progress_dirty = true;
        }
    }

    /// Once per frame: timeouts, paced chunks and the progress broadcast.
    pub fn tick(&mut self, now: u64) {
        if !self.clock_started {
            self.clock_started = true;
            self.last_fill_ms = now;
            self.last_progress_ms = now;
        }
        for i in 1..SLOTS {
            let s = &self.slots[i];
            match s.state {
                Up::Offered => {
                    if now.saturating_sub(s.offered_ms) >= CONSENT_WAIT_MS + CONSENT_GRACE_MS {
                        self.kick(i, Reason::Timeout);
                    }
                }
                Up::Uploading => {
                    let stalled =
                        s.sent > s.acked && now.saturating_sub(s.last_progress_ms) >= STALL_MS;
                    let late = now.saturating_sub(s.accepted_ms) >= UPLOAD_DEADLINE_MS;
                    if stalled || late {
                        self.kick(i, Reason::Timeout);
                    }
                }
                _ => {}
            }
            let s = &mut self.slots[i];
            if s.in_grace && now.saturating_sub(s.joined_ms) >= JOIN_GRACE_MS {
                s.in_grace = false;
                self.progress_dirty = true;
            }
        }

        let elapsed = now.saturating_sub(self.last_fill_ms);
        self.last_fill_ms = now;
        let cap = HOST_CHUNKS_PER_SECOND as f64;
        self.tokens = (self.tokens + elapsed as f64 * cap / 1000.0).min(cap);
        self.send_chunks();

        let due = now.saturating_sub(self.last_progress_ms) >= PROGRESS_MIN_INTERVAL_MS;
        if (self.progress_dirty || due) && (self.progress_dirty || self.any_uploading()) {
            let mut slots = [SlotProgress::default(); SLOTS];
            for (i, p) in slots.iter_mut().enumerate() {
                *p = SlotProgress {
                    state: self.slot_state(i),
                    percent: self.slot_percent(i),
                };
            }
            self.push(Target::AllModded, &Msg::Progress { slots });
            self.progress_dirty = false;
            self.last_progress_ms = now;
        }
    }

    /// Chunks go round-robin over the uploads, one at a time, while there are tokens and room in
    /// a window. At most [`HOST_CHUNKS_PER_SECOND`] per call (the bucket's size).
    fn send_chunks(&mut self) {
        let Some(total) = self.map.as_ref().map(|m| m.bytes.len() as u32) else {
            return;
        };
        let window = WINDOW_CHUNKS * MAX_CHUNK as u32;
        let mut sent_something = true;
        while self.tokens >= 1.0 && sent_something {
            sent_something = false;
            for i in 1..SLOTS {
                if self.tokens < 1.0 {
                    break;
                }
                let s = &self.slots[i];
                if s.state != Up::Uploading || s.sent >= total || s.sent - s.acked >= window {
                    continue;
                }
                let len = (total - s.sent).min(MAX_CHUNK as u32) as usize;
                let (req, offset) = (s.req, s.sent);
                let map = self.map.as_ref().expect("checked above");
                let data = &map.bytes[offset as usize..offset as usize + len];
                let Some(bytes) = encode(&Msg::Chunk { req, offset, data }) else {
                    continue;
                };
                self.outbox.push(Out {
                    to: Target::Slot(i as u8),
                    bytes,
                });
                self.slots[i].sent += len as u32;
                self.tokens -= 1.0;
                sent_something = true;
            }
        }
    }

    fn any_uploading(&self) -> bool {
        self.slots[1..]
            .iter()
            .any(|s| s.state == Up::Uploading || s.state == Up::Offered)
    }

    fn grace_can_hold(&self) -> bool {
        self.serving && self.map.is_some()
    }

    /// True while Start must wait: an offer or upload is open, or a new player is still inside
    /// the join grace and a map could be offered to them.
    pub fn any_pending(&self, now: u64) -> bool {
        self.pending_slot(now).is_some()
    }

    /// The net slot Start is waiting on, uploads first, then offers, then new players.
    pub fn pending_slot(&self, now: u64) -> Option<usize> {
        let in_grace = |s: &Slot| {
            self.grace_can_hold() && s.in_grace && now.saturating_sub(s.joined_ms) < JOIN_GRACE_MS
        };
        (1..SLOTS)
            .find(|&i| self.slots[i].state == Up::Uploading)
            .or_else(|| (1..SLOTS).find(|&i| self.slots[i].state == Up::Offered))
            .or_else(|| (1..SLOTS).find(|&i| in_grace(&self.slots[i])))
    }

    pub fn slot_state(&self, slot: usize) -> SlotState {
        match self.slots.get(slot).map(|s| s.state) {
            None | Some(Up::None) => SlotState::None,
            Some(Up::Offered) => SlotState::Offered,
            Some(Up::Uploading) => SlotState::Uploading,
            Some(Up::Done) => SlotState::Done,
            Some(Up::Failed) => SlotState::NoMap,
            Some(Up::Stalled) => SlotState::Stalled,
            Some(Up::LeftNoMap) => SlotState::LeftNoMap,
            Some(Up::NoAnswer) => SlotState::NoAnswer,
        }
    }

    /// 0 to 100; for a player who left without the map it carries the [`LeaveWhy`] code.
    pub fn slot_percent(&self, slot: usize) -> u8 {
        let Some(s) = self.slots.get(slot) else {
            return 0;
        };
        match s.state {
            Up::Done => 100,
            Up::LeftNoMap => s.left_why,
            Up::Uploading => match &self.map {
                Some(m) if !m.bytes.is_empty() => {
                    (s.acked as u64 * 100 / m.bytes.len() as u64) as u8
                }
                _ => 0,
            },
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapwire::{LeaveWhy, decode};

    const NAME: &[u8] = b"ridge.pud";
    const R1: ReqId = [1; 16];
    const R2: ReqId = [2; 16];
    const R3: ReqId = [3; 16];

    fn host(len: usize) -> DownloadHost {
        let mut h = DownloadHost::new();
        assert!(h.set_servable_map(NAME, vec![7u8; len]));
        for slot in 1..SLOTS {
            h.note_peer_joined(slot, 0, true);
        }
        h.tick(0);
        h.take_outbox();
        h
    }

    fn request(h: &mut DownloadHost, now: u64, slot: usize, req: ReqId) {
        h.on_client_message(now, slot, &Msg::Request { req, name: NAME });
    }

    /// A short label per outgoing message: `slot:Kind` with the interesting field.
    fn out(h: &mut DownloadHost) -> Vec<String> {
        h.take_outbox()
            .iter()
            .map(|o| {
                let to = match o.to {
                    Target::Slot(s) => s.to_string(),
                    Target::AllModded => "all".to_owned(),
                };
                let what = match decode(&o.bytes[5..]).expect("the host sends valid messages") {
                    Msg::Offer { size, .. } => format!("Offer({size})"),
                    Msg::Deny { reason, .. } => format!("Deny({reason:?})"),
                    Msg::Cancel { reason, .. } => format!("Cancel({reason:?})"),
                    Msg::Chunk { offset, data, .. } => format!("Chunk({offset},{})", data.len()),
                    Msg::Progress { .. } => "Progress".to_owned(),
                    other => format!("{other:?}"),
                };
                format!("{to}:{what}")
            })
            .collect()
    }

    fn without_progress(v: Vec<String>) -> Vec<String> {
        v.into_iter()
            .filter(|s| !s.ends_with(":Progress"))
            .collect()
    }

    fn chunks_to(outs: &[Out], slot: u8) -> Vec<(u32, usize)> {
        outs.iter()
            .filter(|o| o.to == Target::Slot(slot))
            .filter_map(|o| match decode(&o.bytes[5..]) {
                Ok(Msg::Chunk { offset, data, .. }) => Some((offset, data.len())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_clean_transfer_offers_sends_in_order_and_finishes_on_the_last_ack() {
        let mut h = host(2000);
        request(&mut h, 100, 1, R1);
        assert_eq!(without_progress(out(&mut h)), ["1:Offer(2000)"]);
        assert_eq!(h.slot_state(1), SlotState::Offered);
        h.on_client_message(200, 1, &Msg::Accept { req: R1 });
        assert_eq!(h.slot_state(1), SlotState::Uploading);
        h.tick(300);
        let sent = h.take_outbox();
        assert_eq!(chunks_to(&sent, 1), [(0, 960), (960, 960), (1920, 80)]);
        h.on_client_message(
            310,
            1,
            &Msg::Ack {
                req: R1,
                next_offset: 960,
            },
        );
        assert_eq!(h.slot_percent(1), 48);
        h.on_client_message(
            320,
            1,
            &Msg::Ack {
                req: R1,
                next_offset: 1920,
            },
        );
        h.on_client_message(
            330,
            1,
            &Msg::Ack {
                req: R1,
                next_offset: 2000,
            },
        );
        assert_eq!(h.slot_state(1), SlotState::Done);
        assert_eq!(h.slot_percent(1), 100);
        assert!(
            !h.any_pending(6000),
            "nothing open, and everyone is past the join grace"
        );
    }

    #[test]
    fn refusals_say_why_and_a_player_is_denied_at_most_once_in_five_seconds() {
        let mut h = host(500);
        h.on_client_message(
            10,
            1,
            &Msg::Request {
                req: R1,
                name: b"other.pud",
            },
        );
        assert_eq!(without_progress(out(&mut h)), ["1:Deny(NotCurrentMap)"]);
        h.on_client_message(
            20,
            1,
            &Msg::Request {
                req: R1,
                name: b"other.pud",
            },
        );
        assert!(
            without_progress(out(&mut h)).is_empty(),
            "a second deny inside 5 s is silent"
        );
        h.on_client_message(
            5011,
            1,
            &Msg::Request {
                req: R1,
                name: b"other.pud",
            },
        );
        assert_eq!(without_progress(out(&mut h)), ["1:Deny(NotCurrentMap)"]);

        h.set_serving(false);
        request(&mut h, 20_000, 2, R2);
        assert_eq!(without_progress(out(&mut h)), ["2:Deny(ServingOff)"]);
        h.set_serving(true);
        h.set_countdown(true);
        request(&mut h, 30_000, 2, R2);
        assert_eq!(without_progress(out(&mut h)), ["2:Deny(Countdown)"]);
        h.set_countdown(false);
        h.clear_servable_map();
        request(&mut h, 40_000, 2, R2);
        assert_eq!(without_progress(out(&mut h)), ["2:Deny(MapUnservable)"]);
        // A slot that never joined is not answered at all.
        let mut h = DownloadHost::new();
        h.set_servable_map(NAME, vec![1; 10]);
        request(&mut h, 0, 5, R1);
        assert!(h.take_outbox().is_empty());
    }

    #[test]
    fn at_most_one_offer_per_player_per_five_seconds() {
        let mut h = host(500);
        request(&mut h, 0, 1, R1);
        h.take_outbox();
        h.on_client_message(10, 1, &Msg::Have { req: R1 });
        request(&mut h, 1000, 1, R2);
        assert_eq!(without_progress(out(&mut h)), ["1:Deny(RateLimited)"]);
        request(&mut h, 2000, 1, R2);
        assert!(
            without_progress(out(&mut h)).is_empty(),
            "the refusal is itself limited"
        );
        request(&mut h, 6000, 1, R2);
        assert_eq!(without_progress(out(&mut h)), ["1:Offer(500)"]);
    }

    #[test]
    fn a_repeated_request_for_the_open_offer_is_ignored_and_a_different_one_is_busy() {
        let mut h = host(500);
        request(&mut h, 0, 1, R1);
        h.take_outbox();
        request(&mut h, 100, 1, R1);
        assert!(without_progress(out(&mut h)).is_empty());
        request(&mut h, 200, 1, R2);
        assert_eq!(without_progress(out(&mut h)), ["1:Deny(Busy)"]);
    }

    #[test]
    fn two_uploads_at_once_and_a_late_accept_is_cancelled_as_busy() {
        let mut h = host(100_000);
        for (slot, req) in [(1, R1), (2, R2), (3, R3)] {
            request(&mut h, 0, slot, req);
        }
        assert_eq!(
            without_progress(out(&mut h)),
            ["1:Offer(100000)", "2:Offer(100000)", "3:Offer(100000)"]
        );
        h.on_client_message(10, 1, &Msg::Accept { req: R1 });
        h.on_client_message(10, 2, &Msg::Accept { req: R2 });
        h.on_client_message(10, 3, &Msg::Accept { req: R3 });
        assert_eq!(without_progress(out(&mut h)), ["3:Cancel(Busy)"]);
        assert_eq!(h.slot_state(3), SlotState::None);
        // And a new request while two run is refused outright.
        request(&mut h, 20, 4, [4; 16]);
        assert_eq!(without_progress(out(&mut h)), ["4:Deny(Busy)"]);
        assert_eq!(h.pending_slot(30), Some(1), "uploads first");
    }

    #[test]
    fn at_most_eight_chunks_are_in_flight_and_an_ack_makes_room() {
        let mut h = host(100_000);
        request(&mut h, 0, 1, R1);
        h.on_client_message(0, 1, &Msg::Accept { req: R1 });
        h.take_outbox();
        h.tick(1000);
        let first = h.take_outbox();
        assert_eq!(chunks_to(&first, 1).len(), 8, "the window is 8 chunks");
        h.tick(1100);
        assert!(
            chunks_to(&h.take_outbox(), 1).is_empty(),
            "nothing more until an ack"
        );
        h.on_client_message(
            1100,
            1,
            &Msg::Ack {
                req: R1,
                next_offset: 960,
            },
        );
        h.tick(1200);
        let more = h.take_outbox();
        assert_eq!(chunks_to(&more, 1), [(7680, 960)]);
    }

    #[test]
    fn the_host_sends_at_most_64_chunks_a_second() {
        let mut h = host(262_144);
        request(&mut h, 0, 1, R1);
        h.on_client_message(0, 1, &Msg::Accept { req: R1 });
        h.take_outbox();
        let mut total = 0;
        let mut t = 0;
        while t <= 1000 {
            h.tick(t);
            let sent = h.take_outbox();
            let chunks = chunks_to(&sent, 1);
            total += chunks.len();
            if let Some(&(off, len)) = chunks.last() {
                h.on_client_message(
                    t,
                    1,
                    &Msg::Ack {
                        req: R1,
                        next_offset: off + len as u32,
                    },
                );
            }
            t += 10;
        }
        assert!((60..=64).contains(&total), "{total} chunks in one second");
    }

    #[test]
    fn a_stalled_window_removes_the_player() {
        let mut h = host(100_000);
        request(&mut h, 0, 1, R1);
        h.on_client_message(0, 1, &Msg::Accept { req: R1 });
        h.tick(1000);
        h.take_outbox();
        h.tick(14_999);
        assert!(h.take_disconnects().is_empty());
        h.tick(15_000);
        assert_eq!(h.take_disconnects(), [1]);
        assert!(out(&mut h).contains(&"1:Cancel(Timeout)".to_owned()));
        assert_eq!(h.slot_state(1), SlotState::Stalled);
    }

    #[test]
    fn an_upload_that_keeps_creeping_still_ends_at_the_deadline() {
        let mut h = host(200_000);
        request(&mut h, 0, 1, R1);
        h.on_client_message(0, 1, &Msg::Accept { req: R1 });
        let mut t = 0u64;
        let mut acked = 0u32;
        let mut kicked_at = None;
        let mut sent_to = 0u32;
        while t < 130_000 && kicked_at.is_none() {
            h.tick(t);
            for o in h.take_outbox() {
                if let Ok(Msg::Chunk { offset, data, .. }) = decode(&o.bytes[5..]) {
                    sent_to = sent_to.max(offset + data.len() as u32);
                }
            }
            // Acknowledge everything sent, but only every ten seconds: progress, slowly.
            if t % 10_000 == 0 && t > 0 && sent_to > acked {
                h.on_client_message(
                    t,
                    1,
                    &Msg::Ack {
                        req: R1,
                        next_offset: sent_to,
                    },
                );
                acked = sent_to;
            }
            if !h.take_disconnects().is_empty() {
                kicked_at = Some(t);
            }
            t += 1000;
        }
        assert_eq!(
            kicked_at,
            Some(UPLOAD_DEADLINE_MS),
            "no stall, so the 120 s deadline"
        );
    }

    #[test]
    fn an_unanswered_offer_ends_after_the_consent_time_and_a_decline_removes_the_player() {
        let mut h = host(500);
        request(&mut h, 0, 1, R1);
        request(&mut h, 0, 2, R2);
        h.take_outbox();
        h.tick(17_999);
        assert!(h.take_disconnects().is_empty());
        h.on_client_message(
            18_000,
            2,
            &Msg::Cancel {
                req: R2,
                reason: Reason::Declined,
            },
        );
        assert_eq!(h.take_disconnects(), [2]);
        assert_eq!(h.slot_state(2), SlotState::NoMap);
        h.tick(18_001);
        assert_eq!(h.take_disconnects(), [1]);
        assert_eq!(h.slot_state(1), SlotState::NoAnswer);
    }

    #[test]
    fn other_cancels_and_have_end_an_offer_without_removal_and_use_up_the_offers() {
        let mut h = host(500);
        // Cancel for any reason but Declined: not removed, an unused offer.
        request(&mut h, 0, 1, R1);
        h.on_client_message(
            10,
            1,
            &Msg::Cancel {
                req: R1,
                reason: Reason::Busy,
            },
        );
        assert!(h.take_disconnects().is_empty());
        assert_eq!(h.slot_state(1), SlotState::NoMap);
        // HAVE: an unused offer, the slot goes back to nothing.
        request(&mut h, 6000, 1, R2);
        h.on_client_message(6010, 1, &Msg::Have { req: R2 });
        assert_eq!(h.slot_state(1), SlotState::None);
        request(&mut h, 12_000, 1, R3);
        h.on_client_message(12_010, 1, &Msg::Have { req: R3 });
        h.take_outbox();
        // Three unused offers of this map: the fourth request is refused.
        request(&mut h, 18_000, 1, [9; 16]);
        assert_eq!(without_progress(out(&mut h)), ["1:Deny(RateLimited)"]);
    }

    #[test]
    fn a_map_finished_twice_is_not_sent_a_third_time_and_a_new_map_resets_the_count() {
        let mut h = host(100);
        for i in 0..2u64 {
            let req = [10 + i as u8; 16];
            request(&mut h, i * 6000, 1, req);
            h.on_client_message(i * 6000, 1, &Msg::Accept { req });
            h.tick(i * 6000 + 100);
            h.on_client_message(
                i * 6000 + 200,
                1,
                &Msg::Ack {
                    req,
                    next_offset: 100,
                },
            );
            assert_eq!(h.slot_state(1), SlotState::Done);
        }
        h.take_outbox();
        request(&mut h, 20_000, 1, [30; 16]);
        assert_eq!(without_progress(out(&mut h)), ["1:Deny(RateLimited)"]);
        // The host picks another map: that one is new to the player.
        assert!(h.set_servable_map(b"other.pud", vec![9; 100]));
        h.take_outbox();
        h.on_client_message(
            30_000,
            1,
            &Msg::Request {
                req: [31; 16],
                name: b"other.pud",
            },
        );
        assert_eq!(without_progress(out(&mut h)), ["1:Offer(100)"]);
    }

    #[test]
    fn thirty_two_uploads_a_stay_is_the_limit() {
        let mut h = host(100);
        for i in 0..UPLOADS_PER_STAY as u64 {
            let req = [(i + 1) as u8; 16];
            let t = i * 6000;
            request(&mut h, t, 1, req);
            h.on_client_message(t, 1, &Msg::Accept { req });
            // Not completed: cancelled, so the same-map completion limit never applies.
            h.on_client_message(
                t + 1,
                1,
                &Msg::Cancel {
                    req,
                    reason: Reason::Timeout,
                },
            );
            h.take_outbox();
        }
        request(&mut h, 33 * 6000, 1, [99; 16]);
        assert_eq!(without_progress(out(&mut h)), ["1:Deny(RateLimited)"]);
        // A new stay (the player leaves and joins again) starts fresh.
        h.note_peer_left(1);
        h.note_peer_joined(1, 40 * 6000, true);
        request(&mut h, 40 * 6000, 1, [98; 16]);
        assert_eq!(without_progress(out(&mut h)), ["1:Offer(100)"]);
    }

    #[test]
    fn acks_must_move_forward_for_the_open_request_from_that_player() {
        let mut h = host(100_000);
        request(&mut h, 0, 1, R1);
        h.on_client_message(0, 1, &Msg::Accept { req: R1 });
        h.tick(1000);
        h.take_outbox();
        let ack = |h: &mut DownloadHost, slot, req, next| {
            h.on_client_message(
                1100,
                slot,
                &Msg::Ack {
                    req,
                    next_offset: next,
                },
            )
        };
        ack(&mut h, 1, R1, 99_999);
        assert_eq!(h.slots[1].acked, 0, "past what was sent");
        ack(&mut h, 1, R2, 960);
        assert_eq!(h.slots[1].acked, 0, "someone else's request id");
        ack(&mut h, 2, R1, 960);
        assert_eq!(h.slots[2].acked, 0, "another player");
        ack(&mut h, 1, R1, 960);
        assert_eq!(h.slots[1].acked, 960);
        ack(&mut h, 1, R1, 960);
        ack(&mut h, 1, R1, 480);
        assert_eq!(h.slots[1].acked, 960, "no repeat, no going back");
        h.on_client_message(
            1100,
            0,
            &Msg::Ack {
                req: R1,
                next_offset: 1920,
            },
        );
        assert_eq!(h.slots[1].acked, 960, "slot 0 is the host");
        h.on_client_message(
            1100,
            9,
            &Msg::Ack {
                req: R1,
                next_offset: 1920,
            },
        );
    }

    #[test]
    fn leaving_without_the_map_is_recorded_with_its_reason() {
        let mut h = host(500);
        h.on_client_message(
            0,
            1,
            &Msg::Leaving {
                why: LeaveWhy::BadName,
            },
        );
        assert_eq!(h.slot_state(1), SlotState::LeftNoMap);
        assert_eq!(h.slot_percent(1), 3);
        let mut empty = DownloadHost::new();
        empty.on_client_message(
            0,
            1,
            &Msg::Leaving {
                why: LeaveWhy::BadName,
            },
        );
        assert_eq!(
            empty.slot_state(1),
            SlotState::None,
            "a player the host never saw"
        );
    }

    #[test]
    fn changing_the_map_cancels_open_transfers() {
        let mut h = host(500);
        request(&mut h, 0, 1, R1);
        request(&mut h, 0, 2, R2);
        h.on_client_message(0, 2, &Msg::Accept { req: R2 });
        h.take_outbox();
        assert!(h.set_servable_map(b"next.pud", vec![1; 10]));
        let sent = without_progress(out(&mut h));
        assert_eq!(sent, ["1:Cancel(NotCurrentMap)", "2:Cancel(NotCurrentMap)"]);
        assert_eq!(h.slot_state(1), SlotState::None);
        assert!(!h.set_servable_map(b"big.pud", vec![0; MAX_MAP_BYTES as usize + 1]));
        assert!(!h.has_map(), "an unservable file clears the map");
        assert!(!h.set_servable_map(b"empty.pud", Vec::new()));
    }

    #[test]
    fn start_waits_for_new_players_a_download_and_an_offer() {
        let mut h = DownloadHost::new();
        h.set_servable_map(NAME, vec![1; 100]);
        h.note_peer_joined(2, 1000, true);
        assert!(h.any_pending(1000));
        assert_eq!(h.pending_slot(1000), Some(2));
        assert!(!h.any_pending(6000), "the join grace is five seconds");
        h.set_serving(false);
        h.note_peer_joined(3, 7000, true);
        assert!(!h.any_pending(7000), "nothing to wait for when not serving");
        h.set_serving(true);
        h.clear_servable_map();
        assert!(!h.any_pending(7000), "or with no map");
        h.set_servable_map(NAME, vec![1; 100]);
        h.note_peer_joined(4, 7000, false);
        assert!(h.any_pending(7000), "slot 3 is still in its grace");
        assert_eq!(h.pending_slot(7000), Some(3));
        assert!(
            !h.any_pending(12_001),
            "a player without the mod is not waited for"
        );
    }

    #[test]
    fn progress_goes_to_everyone_on_change_and_every_half_second_while_uploading() {
        let mut h = host(100_000);
        request(&mut h, 0, 1, R1);
        h.on_client_message(0, 1, &Msg::Accept { req: R1 });
        h.take_outbox();
        h.tick(100);
        let n = |h: &mut DownloadHost| out(h).iter().filter(|s| *s == "all:Progress").count();
        assert_eq!(n(&mut h), 1, "a change");
        h.tick(200);
        assert_eq!(n(&mut h), 0, "nothing changed and it is not time");
        h.tick(700);
        assert_eq!(n(&mut h), 1, "half a second on, while uploading");
        let mut idle = host(500);
        idle.tick(5000); // the join grace ends here, which is a change
        idle.take_outbox();
        idle.tick(6000);
        idle.tick(7000);
        assert!(
            !out(&mut idle).iter().any(|s| s == "all:Progress"),
            "no uploads, no chatter"
        );
    }

    #[test]
    fn a_reset_forgets_the_lobby_but_keeps_the_serving_choice() {
        let mut h = host(500);
        request(&mut h, 0, 1, R1);
        h.set_serving(false);
        h.reset();
        assert_eq!(h.slot_state(1), SlotState::None);
        assert!(!h.has_map());
        h.set_servable_map(NAME, vec![1; 10]);
        h.note_peer_joined(1, 0, false);
        request(&mut h, 0, 1, R1);
        assert_eq!(without_progress(out(&mut h)), ["1:Deny(ServingOff)"]);
    }
}
