//! Every limit of the map-sharing protocol in one place, from wc2r-plus
//! `map_download/limits.h` and `PROTOCOL.md` section 3. Times are milliseconds. Each limit is
//! enforced on the side it protects: the host bounds what a player can make it do, the player
//! bounds what a host can make it keep.

pub use crate::mapwire::{MAX_CHUNK, MAX_MAP_BYTES, MAX_NAME_LEN};

/// Unacknowledged chunks in flight per upload.
pub const WINDOW_CHUNKS: u32 = 8;
/// Chunks the host sends per second across all uploads.
pub const HOST_CHUNKS_PER_SECOND: u32 = 64;

/// How long the prompt gives the player to answer.
pub const CONSENT_WAIT_MS: u64 = 15_000;
/// Extra time the host allows for the offer to arrive and the answer to come back.
pub const CONSENT_GRACE_MS: u64 = 3_000;
/// The player's own give-up on an unanswered prompt.
pub const CLIENT_CONSENT_GIVE_UP_MS: u64 = CONSENT_WAIT_MS + 2 * CONSENT_GRACE_MS;

/// An upload (or download) that makes no progress for this long ends.
pub const STALL_MS: u64 = 15_000;
/// An upload not finished this long after ACCEPT ends.
pub const UPLOAD_DEADLINE_MS: u64 = 120_000;
/// Bytes of acknowledged data that count as progress for the stall clock.
pub const PROGRESS_BYTES: u32 = WINDOW_CHUNKS * MAX_CHUNK as u32;
/// A new player is given this long to ask before Start may proceed.
pub const JOIN_GRACE_MS: u64 = 5_000;
/// How long a player waits for an offer after asking.
pub const OFFER_WAIT_MS: u64 = 10_000;

pub const REQUEST_RETRY_MS: u64 = 2_500;
pub const REQUEST_RETRIES: u32 = 2;
/// At most one offer per player per this long; a request inside it is refused.
pub const REQUEST_MIN_INTERVAL_MS: u64 = 5_000;
/// At most one DENY per player per this long.
pub const DENY_MIN_INTERVAL_MS: u64 = 5_000;

/// Per player per stay in the lobby.
pub const UPLOADS_PER_STAY: u32 = 32;
pub const MAX_COMPLETED_SAME_MAP_PER_STAY: u32 = 2;
pub const MAX_UNUSED_OFFERS_PER_MAP_PER_STAY: u32 = 3;
/// Uploads running at once on one host.
pub const MAX_CONCURRENT_UPLOADS: usize = 2;

/// PROGRESS goes out when something changed, and at most this often while uploads run.
pub const PROGRESS_MIN_INTERVAL_MS: u64 = 500;

/// Who the host sends a message to: one net slot, or every player that announced map sharing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Slot(u8),
    AllModded,
}

/// A message the host wants sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Out {
    pub to: Target,
    pub bytes: Vec<u8>,
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).into()
}
