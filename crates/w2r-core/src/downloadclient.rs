//! The player's side of map sharing (docs/map-download.md, slice 2): ask the host for the lobby's
//! map, wait for the user's answer, receive it in order, and check it. Ported from wc2r-plus
//! `map_download/download_client.cpp`. Pure state: a millisecond clock and decoded messages go
//! in, messages for the host come out. The glue (slice 3) asks the user, stores the file and
//! tells this machine whether that worked.
//!
//! What a player accepts from a host: an OFFER only for its own pending request and inside the
//! limits; chunks strictly in order, never past the offered size (checked by subtraction, so
//! nothing wraps); and a finished file that hashes to the offered SHA-256 *and* passes the map
//! validator. The hash proves the bytes arrived as sent, not where they came from: the host
//! chooses both, so the validator is what stands between a hostile host and the game.

use crate::maplimits::*;
use crate::mapname;
use crate::mapwire::{Msg, Reason, ReqId, encode};
use crate::pudvalidate;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientState {
    Idle,
    Requested,
    Offered,
    Receiving,
    Done,
    Present,
    Failed,
}

pub struct DownloadClient {
    state: ClientState,
    failed_from: ClientState,
    reason: Reason,
    req: ReqId,
    name: Vec<u8>,
    size: u32,
    sha: [u8; 32],
    buffer: Vec<u8>,
    received: u32,
    start_ms: u64,
    last_request_ms: u64,
    request_retries: u32,
    offered_ms: u64,
    accepted_ms: u64,
    last_progress_ms: u64,
    ignored: u32,
    outbox: Vec<Vec<u8>>,
}

impl Default for DownloadClient {
    fn default() -> Self {
        DownloadClient::new()
    }
}

impl DownloadClient {
    pub fn new() -> DownloadClient {
        DownloadClient {
            state: ClientState::Idle,
            failed_from: ClientState::Idle,
            reason: Reason::Ok,
            req: [0; 16],
            name: Vec::new(),
            size: 0,
            sha: [0; 32],
            buffer: Vec::new(),
            received: 0,
            start_ms: 0,
            last_request_ms: 0,
            request_retries: 0,
            offered_ms: 0,
            accepted_ms: 0,
            last_progress_ms: 0,
            ignored: 0,
            outbox: Vec::new(),
        }
    }

    pub fn state(&self) -> ClientState {
        self.state
    }

    pub fn awaiting_consent(&self) -> bool {
        self.state == ClientState::Offered
    }

    pub fn reason(&self) -> Reason {
        self.reason
    }

    pub fn name(&self) -> &[u8] {
        &self.name
    }

    pub fn offered_size(&self) -> u32 {
        self.size
    }

    pub fn offered_sha(&self) -> &[u8; 32] {
        &self.sha
    }

    pub fn offered_at_ms(&self) -> u64 {
        self.offered_ms
    }

    /// Messages ignored as not valid right now (a wrong sender, request id or state).
    pub fn ignored(&self) -> u32 {
        self.ignored
    }

    /// The finished, verified file; only meaningful in [`ClientState::Done`].
    pub fn bytes(&self) -> &[u8] {
        &self.buffer
    }

    pub fn sha(&self) -> &[u8; 32] {
        &self.sha
    }

    /// Messages for the host (net slot 0).
    pub fn take_outbox(&mut self) -> Vec<Vec<u8>> {
        core::mem::take(&mut self.outbox)
    }

    fn push(&mut self, msg: &Msg) {
        if let Some(bytes) = encode(msg) {
            self.outbox.push(bytes);
        }
    }

    /// Sends a request for `name` under the caller's random request id. False (and nothing sent)
    /// when a request is already open or the name is not one we would ever store.
    pub fn start(&mut self, now: u64, req: ReqId, name: &[u8]) -> bool {
        if self.state != ClientState::Idle || mapname::validate(name).is_err() {
            return false;
        }
        self.req = req;
        self.name = name.to_vec();
        self.start_ms = now;
        self.last_request_ms = now;
        self.request_retries = 0;
        let Some(bytes) = encode(&Msg::Request { req, name }) else {
            return false;
        };
        self.outbox.push(bytes);
        self.state = ClientState::Requested;
        true
    }

    fn req_of(msg: &Msg) -> Option<ReqId> {
        match *msg {
            Msg::Offer { req, .. }
            | Msg::Deny { req, .. }
            | Msg::Cancel { req, .. }
            | Msg::Chunk { req, .. } => Some(req),
            _ => None,
        }
    }

    /// A decoded message from net slot `sender`. Only the host (slot 0) is listened to, only for
    /// the open request, and only in the state it makes sense in.
    pub fn on_host_message(&mut self, now: u64, sender: usize, msg: &Msg) {
        if sender != 0
            || matches!(
                self.state,
                ClientState::Idle | ClientState::Done | ClientState::Failed | ClientState::Present
            )
            || Self::req_of(msg) != Some(self.req)
        {
            self.ignored += 1;
            return;
        }
        match *msg {
            Msg::Offer { size, sha256, .. } => {
                if self.state != ClientState::Requested {
                    self.ignored += 1;
                    return;
                }
                self.size = size;
                self.sha = sha256;
                self.offered_ms = now;
                self.state = ClientState::Offered;
            }
            Msg::Deny { reason, .. } => {
                if self.state != ClientState::Requested {
                    self.ignored += 1;
                    return;
                }
                self.fail(reason, false);
            }
            Msg::Cancel { reason, .. } => self.fail(reason, false),
            Msg::Chunk { offset, data, .. } => self.on_chunk(now, offset, data),
            _ => self.ignored += 1,
        }
    }

    fn on_chunk(&mut self, now: u64, offset: u32, data: &[u8]) {
        if self.state != ClientState::Receiving {
            self.ignored += 1;
            return;
        }
        // In order, non-empty, and within what is left (a subtraction, so it cannot wrap).
        if offset != self.received
            || data.is_empty()
            || data.len() as u64 > (self.size - self.received) as u64
        {
            self.ignored += 1;
            return;
        }
        let at = self.received as usize;
        self.buffer[at..at + data.len()].copy_from_slice(data);
        self.received += data.len() as u32;
        self.last_progress_ms = now;
        let (req, next) = (self.req, self.received);
        self.push(&Msg::Ack {
            req,
            next_offset: next,
        });
        if self.received == self.size {
            if sha256(&self.buffer) != self.sha {
                return self.fail(Reason::HashMismatch, true);
            }
            if pudvalidate::validate(&self.buffer).is_some() {
                return self.fail(Reason::InvalidMap, true);
            }
            self.state = ClientState::Done;
            self.push(&Msg::Done {
                req,
                reason: Reason::Ok,
            });
        }
    }

    /// The user's answer to "download this map?". A yes allocates exactly the offered size.
    pub fn on_consent(&mut self, now: u64, accept: bool) {
        if self.state != ClientState::Offered {
            return;
        }
        if !accept {
            return self.fail(Reason::Declined, true);
        }
        self.buffer = vec![0; self.size as usize];
        self.received = 0;
        self.accepted_ms = now;
        self.last_progress_ms = now;
        let req = self.req;
        self.push(&Msg::Accept { req });
        self.state = ClientState::Receiving;
    }

    /// Once per frame: give up on an unanswered request, an unanswered prompt, or a stalled or
    /// overlong download, and repeat an unanswered request.
    pub fn tick(&mut self, now: u64) {
        match self.state {
            ClientState::Requested => {
                if now.saturating_sub(self.start_ms) >= OFFER_WAIT_MS {
                    self.fail(Reason::Timeout, true);
                } else if self.request_retries < REQUEST_RETRIES
                    && now.saturating_sub(self.last_request_ms) >= REQUEST_RETRY_MS
                {
                    let (req, name) = (self.req, self.name.clone());
                    self.push(&Msg::Request { req, name: &name });
                    self.last_request_ms = now;
                    self.request_retries += 1;
                }
            }
            ClientState::Offered => {
                if now.saturating_sub(self.offered_ms) >= CLIENT_CONSENT_GIVE_UP_MS {
                    self.fail(Reason::Declined, true);
                }
            }
            ClientState::Receiving
                if now.saturating_sub(self.last_progress_ms) >= STALL_MS
                    || now.saturating_sub(self.accepted_ms) >= UPLOAD_DEADLINE_MS =>
            {
                self.fail(Reason::Timeout, true);
            }
            _ => {}
        }
    }

    /// The player already has a map with the offered hash: tell the host and stop.
    pub fn on_already_have(&mut self) {
        if self.state != ClientState::Offered {
            return;
        }
        let req = self.req;
        self.push(&Msg::Have { req });
        self.state = ClientState::Present;
    }

    /// The glue could not write the finished file.
    pub fn on_store_failed(&mut self) {
        if self.state == ClientState::Done {
            self.fail(Reason::StoreFailed, true);
        }
    }

    /// Stop whatever is open (the lobby is being left, the map changed) and tell the host.
    pub fn abort(&mut self, reason: Reason) {
        if !matches!(
            self.state,
            ClientState::Done | ClientState::Failed | ClientState::Idle | ClientState::Present
        ) {
            self.fail(reason, true);
        }
    }

    /// 0 to 100.
    pub fn percent(&self) -> u8 {
        match self.state {
            ClientState::Done => 100,
            ClientState::Receiving if self.size > 0 => {
                (self.received as u64 * 100 / self.size as u64) as u8
            }
            _ => 0,
        }
    }

    /// Whether asking again later can work: the host was busy, rate-limiting us, not yet on this
    /// map, or never answered a request.
    pub fn retryable(&self) -> bool {
        self.state == ClientState::Failed
            && (matches!(
                self.reason,
                Reason::RateLimited | Reason::Busy | Reason::NotCurrentMap
            ) || (self.reason == Reason::Timeout && self.failed_from == ClientState::Requested))
    }

    fn fail(&mut self, reason: Reason, tell_host: bool) {
        if tell_host {
            let req = self.req;
            self.push(&Msg::Cancel { req, reason });
        }
        self.failed_from = self.state;
        self.state = ClientState::Failed;
        self.reason = reason;
        self.buffer = Vec::new();
        self.received = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downloadhost::DownloadHost;
    use crate::mapwire::{SlotState, decode};
    use crate::pudvalidate::tests::sample_map;

    const NAME: &[u8] = b"ridge.pud";
    const REQ: ReqId = [5; 16];

    /// A client that has asked and been offered `bytes`.
    fn offered(bytes: &[u8]) -> DownloadClient {
        let mut c = DownloadClient::new();
        assert!(c.start(0, REQ, NAME));
        c.take_outbox();
        let offer = Msg::Offer {
            req: REQ,
            size: bytes.len() as u32,
            sha256: sha256(bytes),
            chunk_len: 960,
        };
        c.on_host_message(100, 0, &offer);
        assert_eq!(c.state(), ClientState::Offered);
        c
    }

    fn kinds(c: &mut DownloadClient) -> Vec<String> {
        c.take_outbox()
            .iter()
            .map(|b| match decode(&b[5..]).expect("valid") {
                Msg::Request { name, .. } => format!("Request({})", String::from_utf8_lossy(name)),
                Msg::Cancel { reason, .. } => format!("Cancel({reason:?})"),
                Msg::Done { reason, .. } => format!("Done({reason:?})"),
                Msg::Ack { next_offset, .. } => format!("Ack({next_offset})"),
                other => format!("{other:?}")
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .to_owned(),
            })
            .collect()
    }

    fn chunk(offset: u32, data: &[u8]) -> Msg<'_> {
        Msg::Chunk {
            req: REQ,
            offset,
            data,
        }
    }

    #[test]
    fn a_name_that_could_never_be_stored_is_not_requested() {
        let mut c = DownloadClient::new();
        assert!(!c.start(0, REQ, b"..\\evil.pud"));
        assert!(!c.start(0, REQ, b"con.pud"));
        assert!(!c.start(0, REQ, b""));
        assert_eq!(c.state(), ClientState::Idle);
        assert!(c.take_outbox().is_empty());
        assert!(c.start(0, REQ, NAME));
        assert!(!c.start(1, REQ, NAME), "one request at a time");
        assert_eq!(kinds(&mut c), ["Request(ridge.pud)"]);
    }

    #[test]
    fn an_unanswered_request_is_repeated_twice_and_then_given_up() {
        let mut c = DownloadClient::new();
        c.start(0, REQ, NAME);
        c.take_outbox();
        c.tick(2499);
        assert!(c.take_outbox().is_empty());
        c.tick(2500);
        assert_eq!(kinds(&mut c), ["Request(ridge.pud)"]);
        c.tick(5000);
        assert_eq!(kinds(&mut c), ["Request(ridge.pud)"]);
        c.tick(7500);
        assert!(c.take_outbox().is_empty(), "only two repeats");
        c.tick(9999);
        assert_eq!(c.state(), ClientState::Requested);
        c.tick(10_000);
        assert_eq!(c.state(), ClientState::Failed);
        assert_eq!(c.reason(), Reason::Timeout);
        assert!(
            c.retryable(),
            "a request nobody answered may be tried again later"
        );
        assert_eq!(kinds(&mut c), ["Cancel(Timeout)"]);
    }

    #[test]
    fn refusals_are_retryable_only_when_waiting_can_help() {
        for (reason, retry) in [
            (Reason::RateLimited, true),
            (Reason::Busy, true),
            (Reason::NotCurrentMap, true),
            (Reason::ServingOff, false),
            (Reason::MapUnservable, false),
            (Reason::Countdown, false),
            (Reason::TooLarge, false),
        ] {
            let mut c = DownloadClient::new();
            c.start(0, REQ, NAME);
            c.take_outbox();
            c.on_host_message(10, 0, &Msg::Deny { req: REQ, reason });
            assert_eq!(c.state(), ClientState::Failed);
            assert_eq!(c.retryable(), retry, "{reason:?}");
            assert!(c.take_outbox().is_empty(), "a refusal is not answered");
        }
    }

    #[test]
    fn only_the_host_for_the_open_request_in_the_right_state_is_heard() {
        let mut c = DownloadClient::new();
        c.on_host_message(
            0,
            0,
            &Msg::Deny {
                req: REQ,
                reason: Reason::Busy,
            },
        );
        assert_eq!(c.ignored(), 1, "nothing is open");
        c.start(0, REQ, NAME);
        let offer = |req| Msg::Offer {
            req,
            size: 10,
            sha256: [1; 32],
            chunk_len: 960,
        };
        c.on_host_message(1, 3, &offer(REQ));
        c.on_host_message(1, 0, &offer([6; 16]));
        c.on_host_message(
            1,
            0,
            &Msg::Progress {
                slots: Default::default(),
            },
        );
        assert_eq!((c.state(), c.ignored()), (ClientState::Requested, 4));
        c.on_host_message(1, 0, &chunk(0, b"x"));
        assert_eq!(c.ignored(), 5, "a chunk before an accept");
        c.on_host_message(2, 0, &offer(REQ));
        assert_eq!(c.state(), ClientState::Offered);
        c.on_host_message(
            3,
            0,
            &Msg::Deny {
                req: REQ,
                reason: Reason::Busy,
            },
        );
        assert_eq!(
            c.state(),
            ClientState::Offered,
            "a deny only answers a request"
        );
        c.on_host_message(3, 0, &offer(REQ));
        assert_eq!(c.ignored(), 7, "a second offer");
        c.on_host_message(
            4,
            0,
            &Msg::Cancel {
                req: REQ,
                reason: Reason::NotCurrentMap,
            },
        );
        assert_eq!(
            c.state(),
            ClientState::Failed,
            "the host may cancel at any point"
        );
        c.on_host_message(5, 0, &offer(REQ));
        assert_eq!(c.state(), ClientState::Failed);
    }

    #[test]
    fn an_unanswered_prompt_is_declined_after_the_give_up_time_and_a_no_tells_the_host() {
        let mut c = offered(b"0123456789");
        c.tick(100 + CLIENT_CONSENT_GIVE_UP_MS - 1);
        assert_eq!(c.state(), ClientState::Offered);
        c.tick(100 + CLIENT_CONSENT_GIVE_UP_MS);
        assert_eq!(c.state(), ClientState::Failed);
        assert_eq!(kinds(&mut c), ["Cancel(Declined)"]);
        let mut c = offered(b"0123456789");
        assert!(c.awaiting_consent());
        c.on_consent(200, false);
        assert_eq!(
            (c.state(), c.reason()),
            (ClientState::Failed, Reason::Declined)
        );
        assert!(!c.retryable());
        assert_eq!(kinds(&mut c), ["Cancel(Declined)"]);
        c.on_consent(300, true);
        assert_eq!(
            c.state(),
            ClientState::Failed,
            "an answer after the fact does nothing"
        );
    }

    #[test]
    fn a_clean_download_is_acknowledged_checked_and_finished() {
        let map = sample_map();
        let mut c = offered(&map);
        c.on_consent(200, true);
        assert_eq!(kinds(&mut c), ["Accept"]);
        let mut offset = 0usize;
        let mut expect = Vec::new();
        for part in map.chunks(960) {
            c.on_host_message(300, 0, &chunk(offset as u32, part));
            offset += part.len();
            expect.push(format!("Ack({offset})"));
        }
        assert_eq!(c.state(), ClientState::Done);
        assert_eq!(c.percent(), 100);
        assert_eq!(c.bytes(), &map[..]);
        expect.push("Done(Ok)".to_owned());
        assert_eq!(kinds(&mut c), expect);
    }

    #[test]
    fn chunks_out_of_order_duplicated_empty_or_too_long_are_dropped() {
        let map = sample_map();
        let mut c = offered(&map);
        c.on_consent(200, true);
        c.take_outbox();
        c.on_host_message(300, 0, &chunk(960, &map[960..1920]));
        c.on_host_message(300, 0, &chunk(0, &[]));
        assert_eq!(
            (c.percent(), c.ignored()),
            (0, 2),
            "a gap, and an empty chunk"
        );
        c.on_host_message(300, 0, &chunk(0, &map[..960]));
        c.on_host_message(300, 0, &chunk(0, &map[..960]));
        assert_eq!(c.ignored(), 3, "a duplicate");
        // A chunk that runs past the offered size, by subtraction, even at the very end.
        let tail = map.len() - 960;
        let rest = vec![0u8; tail + 1];
        c.on_host_message(300, 0, &chunk(960, &rest));
        assert_eq!(c.ignored(), 4);
        assert_eq!(c.state(), ClientState::Receiving);
        assert_eq!(kinds(&mut c), ["Ack(960)"]);
    }

    #[test]
    fn a_file_that_does_not_hash_to_the_offer_is_refused_and_so_is_a_bad_map() {
        // The bytes differ from what was offered.
        let map = sample_map();
        let mut c = offered(&map);
        c.on_consent(200, true);
        c.take_outbox();
        let mut tampered = map.clone();
        tampered[100] ^= 1;
        for (i, part) in tampered.chunks(960).enumerate() {
            c.on_host_message(300, 0, &chunk((i * 960) as u32, part));
        }
        assert_eq!(
            (c.state(), c.reason()),
            (ClientState::Failed, Reason::HashMismatch)
        );
        assert!(kinds(&mut c).ends_with(&["Cancel(HashMismatch)".to_owned()]));
        assert!(c.bytes().is_empty(), "nothing is kept");
        assert!(!c.retryable());

        // The hash matches but the file is not a valid map: a hostile host chose both.
        let junk = vec![0x41u8; 3000];
        let mut c = offered(&junk);
        c.on_consent(200, true);
        c.take_outbox();
        for (i, part) in junk.chunks(960).enumerate() {
            c.on_host_message(300, 0, &chunk((i * 960) as u32, part));
        }
        assert_eq!(
            (c.state(), c.reason()),
            (ClientState::Failed, Reason::InvalidMap)
        );
        assert!(kinds(&mut c).ends_with(&["Cancel(InvalidMap)".to_owned()]));
    }

    #[test]
    fn a_stalled_or_endless_download_is_given_up() {
        let map = sample_map();
        let mut c = offered(&map);
        c.on_consent(1000, true);
        c.on_host_message(2000, 0, &chunk(0, &map[..960]));
        c.tick(2000 + STALL_MS - 1);
        assert_eq!(c.state(), ClientState::Receiving);
        c.tick(2000 + STALL_MS);
        assert_eq!(
            (c.state(), c.reason()),
            (ClientState::Failed, Reason::Timeout)
        );
        assert!(!c.retryable(), "a timeout mid-download is not retried");
        // Slow but steady still ends at the deadline.
        let mut c = offered(&map);
        c.on_consent(0, true);
        let mut t = 0;
        let mut offset = 0usize;
        while c.state() == ClientState::Receiving && t < 200_000 {
            t += 10_000;
            if offset + 100 < map.len() {
                c.on_host_message(t, 0, &chunk(offset as u32, &map[offset..offset + 100]));
                offset += 100;
            }
            c.tick(t);
        }
        assert_eq!(t, UPLOAD_DEADLINE_MS, "ended at the 120 s deadline");
        assert_eq!(c.reason(), Reason::Timeout);
    }

    #[test]
    fn already_having_the_map_ends_the_exchange_with_have() {
        let mut c = offered(b"0123456789");
        c.on_already_have();
        assert_eq!(c.state(), ClientState::Present);
        assert_eq!(kinds(&mut c), ["Have"]);
        c.on_already_have();
        assert!(c.take_outbox().is_empty());
        c.abort(Reason::LeftLobby);
        assert_eq!(c.state(), ClientState::Present, "nothing to abort");
    }

    #[test]
    fn a_failed_store_and_an_abort_tell_the_host() {
        let map = sample_map();
        let mut c = offered(&map);
        c.on_consent(0, true);
        for (i, part) in map.chunks(960).enumerate() {
            c.on_host_message(10, 0, &chunk((i * 960) as u32, part));
        }
        assert_eq!(c.state(), ClientState::Done);
        c.take_outbox();
        c.on_store_failed();
        assert_eq!(
            (c.state(), c.reason()),
            (ClientState::Failed, Reason::StoreFailed)
        );
        assert_eq!(kinds(&mut c), ["Cancel(StoreFailed)"]);
        let mut c = offered(&map);
        c.on_consent(0, true);
        c.take_outbox();
        c.abort(Reason::LeftLobby);
        assert_eq!(kinds(&mut c), ["Cancel(LeftLobby)"]);
        c.on_store_failed();
        assert_eq!(c.reason(), Reason::LeftLobby);
    }

    /// Host and player wired together: the whole exchange with a real map and a real validator.
    #[test]
    fn a_host_and_a_player_complete_a_download_end_to_end() {
        let map = sample_map();
        let mut host = DownloadHost::new();
        assert!(host.set_servable_map(NAME, map.clone()));
        host.note_peer_joined(1, 0, true);
        let mut client = DownloadClient::new();
        assert!(client.start(0, REQ, NAME));
        let mut t = 0u64;
        let mut accepted = false;
        let mut chunks_seen = 0;
        while client.state() != ClientState::Done && t < 30_000 {
            host.tick(t);
            for o in host.take_outbox() {
                match o.to {
                    crate::maplimits::Target::Slot(1) | crate::maplimits::Target::AllModded => {
                        if let Ok(msg) = decode(&o.bytes[5..]) {
                            if matches!(msg, Msg::Chunk { .. }) {
                                chunks_seen += 1;
                            }
                            client.on_host_message(t, 0, &msg);
                        }
                    }
                    _ => {}
                }
            }
            if client.awaiting_consent() && !accepted {
                accepted = true;
                client.on_consent(t, true);
            }
            client.tick(t);
            for bytes in client.take_outbox() {
                let msg = decode(&bytes[5..]).expect("the player sends valid messages");
                host.on_client_message(t, 1, &msg);
            }
            t += 20;
        }
        assert_eq!(client.state(), ClientState::Done, "finished by {t} ms");
        assert_eq!(client.bytes(), &map[..]);
        assert_eq!(host.slot_state(1), SlotState::Done);
        assert_eq!(chunks_seen, map.len().div_ceil(960));
        assert!(!host.any_pending(t + 10_000));
    }
}
