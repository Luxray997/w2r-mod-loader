//! Connection quality per remote player (docs/connection-quality-research.md). The game's own
//! transport pings every peer: it sends a numbered heartbeat on channel 0 every 4 s and the peer
//! echoes the number. Each peer record holds the heartbeats sent (`+0x50`), the ones echoed
//! (`+0x54`) and the time anything was last heard (`+0x44`). Nothing here needs the other player
//! to run the mod: we only read what the game already keeps and time how the counters move.
//!
//! The pure part (this file's top level) turns polled counters into ping, jitter, loss and a
//! rating, and says where to draw. `game` (x86 only) reads the records.

use std::collections::VecDeque;

use crate::lobbylayout::pack;
use crate::screenkit::{Rect, fl};

/// Heartbeat round trips kept for the loss estimate (about two minutes at one every 4 s).
const WINDOW: usize = 30;
/// Round trips the ping is the median of.
const PING_WINDOW: usize = 5;
/// Round trips the jitter is taken over.
pub const JITTER_WINDOW: usize = 8;
/// Round trips needed before a loss figure is shown.
const MIN_FOR_LOSS: usize = 5;
/// A round trip this much over the median (and over the floor) is counted as a resent heartbeat.
const LATE_FACTOR: f32 = 3.0;
const LATE_FLOOR_MS: f32 = 250.0;
/// A poll gap longer than this (a hitch) makes the timing of the counters unreliable.
const MAX_GAP_MS: f64 = 100.0;
/// No packet for this long: the link is failing (the game drops a peer after 8 s).
pub const SILENT_MS: f32 = 5000.0;

/// What a peer record holds, read raw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Raw {
    /// Heartbeats sent (`+0x50`).
    pub sent: u32,
    /// Heartbeats echoed back (`+0x54`).
    pub echoed: u32,
    /// The game-clock time of the last packet from the peer (`+0x44`); only compared for change.
    pub heard: u32,
}

/// What the numbers say about one peer.
#[derive(Clone, Debug, PartialEq)]
pub struct Metrics {
    /// Median of the last few round trips, in ms.
    pub ping_ms: Option<f32>,
    pub jitter_ms: Option<f32>,
    /// Share of recent heartbeats that were resent, in percent; `None` until there are enough.
    pub loss_pct: Option<f32>,
    /// Time since any packet arrived.
    pub silent_ms: f32,
    /// Age of a heartbeat that has not been echoed yet.
    pub waiting_ms: Option<f32>,
    /// Round trips measured so far.
    pub samples: usize,
}

#[derive(Clone, Debug)]
pub struct Tracker {
    last: Option<Raw>,
    last_poll: f64,
    /// When the heartbeat in flight was sent.
    open_since: Option<f64>,
    heard_at: f64,
    rtts: VecDeque<f32>,
}

impl Default for Tracker {
    fn default() -> Tracker {
        Tracker::new()
    }
}

impl Tracker {
    pub const fn new() -> Tracker {
        Tracker {
            last: None,
            last_poll: 0.0,
            open_since: None,
            heard_at: 0.0,
            rtts: VecDeque::new(),
        }
    }

    /// Forgets everything: the record belongs to someone else now.
    pub fn reset(&mut self) {
        *self = Tracker::new();
    }

    /// Takes the counters read at `now_ms`. An event is placed in the middle of the gap since the
    /// previous poll, which is when it happened on average; a gap that long is a hitch and its
    /// timing is dropped rather than guessed.
    pub fn feed(&mut self, now_ms: f64, raw: Raw) {
        let Some(prev) = self.last else {
            self.last = Some(raw);
            self.last_poll = now_ms;
            self.heard_at = now_ms;
            if raw.echoed != raw.sent {
                self.open_since = Some(now_ms);
            }
            return;
        };
        // Counters that went backwards: a new peer took the record.
        if raw.sent.wrapping_sub(prev.sent) > u32::MAX / 2 {
            self.reset();
            self.feed(now_ms, raw);
            return;
        }
        let gap = (now_ms - self.last_poll).max(0.0);
        let reliable = gap <= MAX_GAP_MS;
        let mid = self.last_poll + gap * 0.5;
        let sent_moved = raw.sent != prev.sent;
        if raw.heard != prev.heard {
            self.heard_at = now_ms;
        }
        // The heartbeat that was in flight is done once the next one went out or the echo caught up.
        if let Some(t0) = self.open_since
            && (sent_moved || raw.echoed == raw.sent)
        {
            if reliable {
                self.push((mid - t0).max(0.0) as f32);
            }
            self.open_since = None;
        }
        if sent_moved {
            let one = raw.sent.wrapping_sub(prev.sent) == 1;
            if raw.echoed == raw.sent {
                // Sent and echoed inside one gap: all we know is that it was shorter.
                if one && reliable {
                    self.push((gap * 0.5) as f32);
                }
            } else {
                self.open_since = Some(if one && reliable { mid } else { now_ms });
            }
        }
        self.last = Some(raw);
        self.last_poll = now_ms;
    }

    fn push(&mut self, rtt: f32) {
        if self.rtts.len() == WINDOW {
            self.rtts.pop_front();
        }
        self.rtts.push_back(rtt);
    }

    pub fn metrics(&self, now_ms: f64) -> Metrics {
        let recent: Vec<f32> = self
            .rtts
            .iter()
            .rev()
            .take(PING_WINDOW)
            .rev()
            .copied()
            .collect();
        let ping = median(&recent);
        // The median of the gaps between consecutive round trips over a longer stretch: a single
        // resent heartbeat spoils two gaps, not the figure.
        let jitter = {
            let longer: Vec<f32> = self
                .rtts
                .iter()
                .rev()
                .take(JITTER_WINDOW)
                .rev()
                .copied()
                .collect();
            let gaps: Vec<f32> = longer.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
            median(&gaps)
        };
        let waiting = self.open_since.map(|t| (now_ms - t).max(0.0) as f32);
        let all: Vec<f32> = self.rtts.iter().copied().collect();
        let loss = median(&all).and_then(|m| {
            let limit = (m * LATE_FACTOR).max(LATE_FLOOR_MS);
            let stuck = waiting.is_some_and(|w| w > limit);
            let total = all.len() + usize::from(stuck);
            (total >= MIN_FOR_LOSS).then(|| {
                let late = all.iter().filter(|&&r| r > limit).count() + usize::from(stuck);
                late as f32 * 100.0 / total as f32
            })
        });
        Metrics {
            ping_ms: ping,
            jitter_ms: jitter,
            loss_pct: loss,
            silent_ms: if self.last.is_some() {
                (now_ms - self.heard_at).max(0.0) as f32
            } else {
                0.0
            },
            waiting_ms: waiting,
            samples: self.rtts.len(),
        }
    }
}

fn median(values: &[f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let mid = v.len() / 2;
    Some(if v.len() % 2 == 1 {
        v[mid]
    } else {
        (v[mid - 1] + v[mid]) * 0.5
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Quality {
    NoData,
    Bad,
    Poor,
    Fair,
    Good,
}

impl Quality {
    /// Bars lit out of four.
    pub fn bars(self) -> usize {
        match self {
            Quality::NoData => 0,
            Quality::Bad => 1,
            Quality::Poor => 2,
            Quality::Fair => 3,
            Quality::Good => 4,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Quality::NoData => "Measuring",
            Quality::Bad => "Bad connection",
            Quality::Poor => "Poor connection",
            Quality::Fair => "Fair connection",
            Quality::Good => "Good connection",
        }
    }

    pub fn colour(self) -> u32 {
        match self {
            Quality::NoData => DIM,
            Quality::Bad => pack(214, 84, 64, 255),
            Quality::Poor => pack(236, 170, 40, 255),
            Quality::Fair => pack(196, 212, 84, 255),
            Quality::Good => pack(96, 200, 120, 255),
        }
    }

    fn worse(self, steps: usize) -> Quality {
        let rank = (self as usize).saturating_sub(steps).max(Quality::Bad as usize);
        [Quality::NoData, Quality::Bad, Quality::Poor, Quality::Fair, Quality::Good][rank]
    }
}

pub const DIM: u32 = pack(90, 84, 72, 200);
/// The ping number is a soft white, so it sits below the player name.
pub const LABEL_COLOUR: u32 = pack(235, 228, 215, 255);

/// Ping decides, loss and silence pull it down.
pub fn quality(m: &Metrics) -> Quality {
    let basis = match (m.ping_ms, m.waiting_ms) {
        (Some(p), Some(w)) => Some(p.max(w)),
        (p, w) => p.or(w),
    };
    let Some(ping) = basis else {
        return if m.silent_ms >= SILENT_MS {
            Quality::Bad
        } else {
            Quality::NoData
        };
    };
    let mut q = if ping <= 100.0 {
        Quality::Good
    } else if ping <= 200.0 {
        Quality::Fair
    } else if ping <= 350.0 {
        Quality::Poor
    } else {
        Quality::Bad
    };
    match m.loss_pct {
        Some(l) if l >= 10.0 => q = q.worse(2),
        Some(l) if l >= 3.0 => q = q.worse(1),
        _ => {}
    }
    if m.silent_ms >= SILENT_MS {
        q = Quality::Bad;
    }
    q
}

/// Jitter at which a link starts to feel uneven, and at which it clearly does (ms).
pub const JITTER_NOTICEABLE_MS: f32 = 30.0;
pub const JITTER_HEAVY_MS: f32 = 80.0;

/// A plain-language reading of one link that weighs ping, jitter and loss separately, so a high
/// ping alone is not mistaken for a bad connection (a steady 180 ms plays fine; a 40 ms link that
/// drops packets does not).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Assessment {
    pub quality: Quality,
    /// A short phrase, e.g. "Smooth" or "Steady but laggy".
    pub summary: &'static str,
}

pub fn assess(m: &Metrics) -> Assessment {
    let base = quality(m);
    if m.silent_ms >= SILENT_MS {
        return Assessment { quality: Quality::Bad, summary: "Connection failing" };
    }
    let (Some(ping), true) = (m.ping_ms, base != Quality::NoData) else {
        return Assessment { quality: base, summary: "Measuring" };
    };
    let jitter = m.jitter_ms.unwrap_or(0.0);
    let loss = m.loss_pct.unwrap_or(0.0);
    // Jitter pulls the rating down like loss does, a step at a time.
    let q = if jitter >= JITTER_HEAVY_MS {
        base.worse(2)
    } else if jitter >= JITTER_NOTICEABLE_MS {
        base.worse(1)
    } else {
        base
    };
    let summary = if loss >= 10.0 {
        "Dropping packets: expect stutter"
    } else if loss >= 3.0 {
        "Some packet loss"
    } else if jitter >= JITTER_HEAVY_MS {
        "Very uneven: lag spikes"
    } else if jitter >= JITTER_NOTICEABLE_MS {
        "Uneven: occasional spikes"
    } else if ping > 200.0 {
        "Steady but very laggy"
    } else if ping > 100.0 {
        "Steady, a little laggy"
    } else {
        "Smooth"
    };
    Assessment { quality: q, summary }
}

/// The number drawn beside the bars: the ping in ms, empty before the first measurement.
pub fn label(m: &Metrics) -> String {
    match m.ping_ms {
        Some(p) if p >= 1000.0 => "999+".to_owned(),
        Some(p) => format!("{}", p.round().max(1.0) as u32),
        None => String::new(),
    }
}

/// The hover text: the rating, then what it is made of.
pub fn tip(m: &Metrics) -> String {
    let mut out = String::from(quality(m).word());
    if let Some(p) = m.ping_ms {
        out.push_str(&format!(": ping {} ms", p.round() as u32));
        if let Some(j) = m.jitter_ms {
            out.push_str(&format!(", jitter {} ms", j.round() as u32));
        }
        match m.loss_pct {
            Some(l) => out.push_str(&format!(", loss {}%", l.round() as u32)),
            None => out.push_str(", loss still being measured"),
        }
    }
    if m.silent_ms >= SILENT_MS {
        out.push_str(&format!(". No packets for {:.1} s", m.silent_ms / 1000.0));
    } else if let Some(w) = m.waiting_ms
        && w >= 1000.0
        && m.ping_ms.is_some()
    {
        out.push_str(&format!(". Waiting for a reply for {:.1} s", w / 1000.0));
    }
    out.push_str(" (measured by the game's own link check, every 4 s)");
    out
}

const BAR_COUNT: usize = 4;
const AREA_WIDTH: f32 = 0.9;
const AREA_HEIGHT: f32 = 0.5;
const SIDE_INSET: f32 = 0.2;
const LABEL_WIDTH: f32 = 2.6;
/// Space between the number and the bars, as a fraction of the row height.
const LABEL_GAP: f32 = 0.3;

/// Where a slot's quality marks go, from its name cell: four rising bars at the right end and a
/// number to their left. `None` when the cell is too small to show them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Marks {
    pub bars: [Rect; BAR_COUNT],
    pub label: Rect,
    /// The area the pointer must be in to show the tooltip.
    pub hover: Rect,
}

/// Width of the game's dropdown arrow at the right of a row the host can open, as a fraction of
/// the row height.
const ARROW_RESERVE: f32 = 1.3;

/// `dropdown`: the row has the game's dropdown arrow at its right end, so the marks stop short of it.
pub fn marks(cell: Rect, dropdown: bool) -> Option<Marks> {
    if cell.w <= 0.0 || cell.h <= 0.0 {
        return None;
    }
    let area_w = fl(cell.h * AREA_WIDTH);
    let area_h = fl(cell.h * AREA_HEIGHT);
    let label_w = fl(cell.h * LABEL_WIDTH);
    let side = fl(cell.h * SIDE_INSET);
    // Leave most of the cell to the name.
    if area_w < 8.0 || area_h < 6.0 || cell.w < (area_w + label_w + side) * 1.8 {
        return None;
    }
    let reserve = if dropdown { fl(cell.h * ARROW_RESERVE) } else { 0.0 };
    let x0 = fl(cell.x + cell.w - reserve - side - area_w);
    let y1 = fl(cell.y + (cell.h + area_h) * 0.5);
    let step = area_w / BAR_COUNT as f32;
    let bar_w = fl(step * 0.7);
    let mut bars = [Rect::default(); BAR_COUNT];
    for (i, bar) in bars.iter_mut().enumerate() {
        let h = fl(area_h * (i + 1) as f32 / BAR_COUNT as f32);
        *bar = Rect::new(fl(x0 + step * i as f32), fl(y1 - h), bar_w, h);
    }
    let label = Rect::new(fl(x0 - label_w - cell.h * LABEL_GAP), cell.y, label_w, cell.h);
    let hover = Rect::new(label.x, cell.y, fl(cell.x + cell.w - label.x), cell.h);
    Some(Marks { bars, label, hover })
}

#[cfg(all(windows, target_arch = "x86"))]
pub mod game {
    //! Reads the transport's peer records (x86 only). `FUN_007d5fd0` is a `thiscall` on the object
    //! at `NetSession + 0x68` that maps a net slot to its record; the fields are those of the
    //! research doc. Every read is under the crash guard because the network layer may free a
    //! record at any time.

    use core::ffi::c_void;
    use std::sync::Mutex;
    use std::sync::OnceLock;
    use std::time::Instant;

    use super::{Metrics, Raw, Tracker};
    use crate::guard::guarded;
    use crate::symbols::Symbols;

    const SESSION_TRANSPORT: usize = 0x68;
    const REC_HEARD: usize = 0x44;
    const REC_SENT: usize = 0x50;
    const REC_ECHOED: usize = 0x54;
    const REC_SKIP: usize = 0xfd;
    const REC_NET_SLOT: usize = 0x10a;
    const NET_SLOTS: usize = 8;

    type FindPeerFn = unsafe extern "thiscall" fn(*mut c_void, u32) -> *const u8;

    pub struct Reader {
        session_slot: *const *const u8,
        find_peer: FindPeerFn,
        lobby_slot_by_net_slot: *const u32,
    }

    // SAFETY: plain game addresses, fixed for the session; used from the main thread.
    unsafe impl Send for Reader {}
    unsafe impl Sync for Reader {}

    struct State {
        trackers: [Tracker; NET_SLOTS],
        record: [usize; NET_SLOTS],
        /// The metrics of the last poll per net slot, for sharing with the other players.
        by_net: [Option<Metrics>; NET_SLOTS],
    }

    static STATE: Mutex<State> = Mutex::new(State {
        trackers: [const { Tracker::new() }; NET_SLOTS],
        record: [0; NET_SLOTS],
        by_net: [const { None }; NET_SLOTS],
    });

    /// What the last poll measured, per net slot (not lobby slot).
    pub fn latest_by_net() -> [Option<Metrics>; NET_SLOTS] {
        STATE.lock().unwrap_or_else(|e| e.into_inner()).by_net.clone()
    }
    static START: OnceLock<Instant> = OnceLock::new();

    fn now_ms() -> f64 {
        START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
    }

    impl Reader {
        pub fn new(symbols: &Symbols) -> Option<Reader> {
            // SAFETY: the symbol table's `c_type` for `TransportFindPeer`.
            Some(Reader {
                session_slot: symbols.address("NetSession")? as *const *const u8,
                find_peer: unsafe {
                    core::mem::transmute::<usize, FindPeerFn>(
                        symbols.address("TransportFindPeer")?,
                    )
                },
                lobby_slot_by_net_slot: symbols.address("LobbySlotByNetSlot")? as *const u32,
            })
        }

        /// The counters of each net slot that has a live peer record.
        fn read(&self) -> [Option<(usize, Raw)>; NET_SLOTS] {
            guarded(|| unsafe {
                let mut out = [None; NET_SLOTS];
                let session = *self.session_slot;
                if session.is_null() {
                    return out;
                }
                let transport = *(session.add(SESSION_TRANSPORT) as *const *mut c_void);
                if transport.is_null() {
                    return out;
                }
                for (slot, entry) in out.iter_mut().enumerate() {
                    let record = (self.find_peer)(transport, slot as u32);
                    if record.is_null()
                        || *record.add(REC_NET_SLOT) == 0xff
                        || *record.add(REC_SKIP) != 0
                    {
                        continue;
                    }
                    let u32_at = |o: usize| record.add(o).cast::<u32>().read_unaligned();
                    *entry = Some((
                        record as usize,
                        Raw {
                            sent: u32_at(REC_SENT),
                            echoed: u32_at(REC_ECHOED),
                            heard: u32_at(REC_HEARD),
                        },
                    ));
                }
                out
            })
            .unwrap_or([None; NET_SLOTS])
        }

        /// Reads the records once and returns each lobby slot's metrics (`None` for a seat with
        /// no peer record: empty, closed, or ourselves).
        pub fn poll(&self) -> [Option<Metrics>; NET_SLOTS] {
            let now = now_ms();
            let raw = self.read();
            let table: [u32; NET_SLOTS] = guarded(|| unsafe {
                core::array::from_fn(|i| self.lobby_slot_by_net_slot.add(i).read_unaligned())
            })
            .unwrap_or([u32::MAX; NET_SLOTS]);
            let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
            let mut out: [Option<Metrics>; NET_SLOTS] = Default::default();
            state.by_net = Default::default();
            for (net, entry) in raw.iter().enumerate() {
                let Some((record, counters)) = entry else {
                    state.trackers[net].reset();
                    state.record[net] = 0;
                    continue;
                };
                if state.record[net] != *record {
                    state.trackers[net].reset();
                    state.record[net] = *record;
                }
                state.trackers[net].feed(now, *counters);
                let lobby = (table[net] & 0xff) as usize;
                if lobby < NET_SLOTS {
                    let m = state.trackers[net].metrics(now);
                    // Our own record never sends or hears anything: not a link to rate.
                    if counters.sent == 0 && m.silent_ms >= super::SILENT_MS {
                        continue;
                    }
                    state.by_net[net] = Some(m.clone());
                    out[lobby] = Some(m);
                }
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(sent: u32, echoed: u32, heard: u32) -> Raw {
        Raw { sent, echoed, heard }
    }

    /// Polls every 10 ms with the counters changing at the given times.
    fn run(events: &[(f64, Raw)], until: f64) -> Tracker {
        let mut t = Tracker::new();
        let mut current = raw(0, 0, 0);
        let mut next = 0;
        let mut now = 0.0;
        while now <= until {
            while next < events.len() && events[next].0 <= now {
                current = events[next].1;
                next += 1;
            }
            t.feed(now, current);
            now += 10.0;
        }
        t
    }

    #[test]
    fn a_round_trip_is_the_time_between_the_send_and_the_echo() {
        // Heartbeat 1 goes out at t=1000 and comes back at t=1060.
        let t = run(&[(1000.0, raw(1, 0, 1)), (1060.0, raw(1, 1, 2))], 1200.0);
        let m = t.metrics(1200.0);
        assert_eq!(m.samples, 1);
        let ping = m.ping_ms.unwrap();
        assert!((ping - 60.0).abs() <= 10.0, "{ping}");
        assert_eq!(m.waiting_ms, None);
    }

    #[test]
    fn a_heartbeat_sent_and_echoed_between_two_polls_is_a_short_one() {
        let t = run(&[(1004.0, raw(1, 1, 5))], 1100.0);
        let m = t.metrics(1100.0);
        assert_eq!(m.samples, 1);
        assert!(m.ping_ms.unwrap() <= 10.0);
    }

    #[test]
    fn a_heartbeat_in_flight_is_waiting_and_not_a_sample() {
        let t = run(&[(1000.0, raw(1, 0, 1))], 1500.0);
        let m = t.metrics(1500.0);
        assert_eq!(m.samples, 0);
        assert!((m.waiting_ms.unwrap() - 500.0).abs() <= 10.0);
    }

    #[test]
    fn a_hitch_between_polls_drops_the_timing() {
        let mut t = Tracker::new();
        t.feed(0.0, raw(0, 0, 0));
        t.feed(10.0, raw(1, 0, 1));
        // The game froze for a second; the echo arrived somewhere in it.
        t.feed(1010.0, raw(1, 1, 2));
        assert_eq!(t.metrics(1010.0).samples, 0);
        assert_eq!(t.metrics(1010.0).waiting_ms, None);
    }

    #[test]
    fn the_ping_is_the_median_of_recent_round_trips_and_jitter_their_spread() {
        let mut events = Vec::new();
        let mut at = 1000.0;
        for (n, rtt) in [40.0, 44.0, 40.0, 200.0, 44.0].iter().enumerate() {
            let n = n as u32;
            events.push((at, raw(n + 1, n, n * 2 + 1)));
            events.push((at + rtt, raw(n + 1, n + 1, n * 2 + 2)));
            at += 4000.0;
        }
        let t = run(&events, at);
        let m = t.metrics(at);
        assert_eq!(m.samples, 5);
        assert!((m.ping_ms.unwrap() - 44.0).abs() <= 10.0, "{:?}", m.ping_ms);
        assert!(m.jitter_ms.unwrap() > 50.0);
    }

    #[test]
    fn late_heartbeats_count_as_loss() {
        let mut events = Vec::new();
        let mut at = 1000.0;
        // Ten heartbeats, two of them took a second (resent).
        for n in 0..10u32 {
            let rtt = if n == 3 || n == 7 { 1000.0 } else { 30.0 };
            events.push((at, raw(n + 1, n, n * 2 + 1)));
            events.push((at + rtt, raw(n + 1, n + 1, n * 2 + 2)));
            at += 4000.0;
        }
        let t = run(&events, at);
        let m = t.metrics(at);
        assert_eq!(m.samples, 10);
        assert!((m.loss_pct.unwrap() - 20.0).abs() < 0.5, "{:?}", m.loss_pct);
        assert!(quality(&m) < Quality::Good);
    }

    #[test]
    fn loss_is_not_reported_before_enough_samples() {
        let t = run(&[(1000.0, raw(1, 0, 1)), (1030.0, raw(1, 1, 2))], 1200.0);
        assert_eq!(t.metrics(1200.0).loss_pct, None);
    }

    #[test]
    fn a_stuck_heartbeat_counts_against_the_link_and_silence_marks_it_bad() {
        let mut events = Vec::new();
        let mut at = 1000.0;
        for n in 0..6u32 {
            events.push((at, raw(n + 1, n, n * 2 + 1)));
            events.push((at + 30.0, raw(n + 1, n + 1, n * 2 + 2)));
            at += 4000.0;
        }
        // The seventh goes out and nothing comes back for 6 s.
        events.push((at, raw(7, 6, 13)));
        let t = run(&events, at + 6000.0);
        let m = t.metrics(at + 6000.0);
        assert!(m.waiting_ms.unwrap() > 5000.0);
        assert!(m.loss_pct.unwrap() > 10.0);
        assert!(m.silent_ms >= SILENT_MS);
        assert_eq!(quality(&m), Quality::Bad);
        assert!(tip(&m).contains("No packets for 6.0 s"));
    }

    #[test]
    fn nothing_measured_yet_is_no_data() {
        let t = run(&[], 100.0);
        let m = t.metrics(100.0);
        assert_eq!(quality(&m), Quality::NoData);
        assert_eq!(label(&m), "");
    }

    #[test]
    fn ping_picks_the_rating_and_loss_pulls_it_down() {
        let base = Metrics {
            ping_ms: Some(40.0),
            jitter_ms: Some(2.0),
            loss_pct: Some(0.0),
            silent_ms: 100.0,
            waiting_ms: None,
            samples: 8,
        };
        assert_eq!(quality(&base), Quality::Good);
        let with = |ping, loss| Metrics {
            ping_ms: Some(ping),
            loss_pct: Some(loss),
            ..base.clone()
        };
        assert_eq!(quality(&with(150.0, 0.0)), Quality::Fair);
        assert_eq!(quality(&with(300.0, 0.0)), Quality::Poor);
        assert_eq!(quality(&with(500.0, 0.0)), Quality::Bad);
        assert_eq!(quality(&with(40.0, 5.0)), Quality::Fair);
        assert_eq!(quality(&with(40.0, 12.0)), Quality::Poor);
        assert_eq!(quality(&with(300.0, 50.0)), Quality::Bad, "never below bad");
    }

    #[test]
    fn assessment_weighs_ping_jitter_and_loss_apart() {
        let m = |ping, jitter, loss| Metrics {
            ping_ms: Some(ping),
            jitter_ms: Some(jitter),
            loss_pct: Some(loss),
            silent_ms: 100.0,
            waiting_ms: None,
            samples: 10,
        };
        assert_eq!(assess(&m(40.0, 3.0, 0.0)).summary, "Smooth");
        assert_eq!(assess(&m(40.0, 3.0, 0.0)).quality, Quality::Good);
        // High but steady is laggy, not unstable.
        let steady = assess(&m(180.0, 4.0, 0.0));
        assert_eq!(steady.summary, "Steady, a little laggy");
        assert_eq!(steady.quality, Quality::Fair);
        // A low ping with spikes is worse than its ping suggests.
        let spiky = assess(&m(40.0, 90.0, 0.0));
        assert_eq!(spiky.summary, "Very uneven: lag spikes");
        assert_eq!(spiky.quality, Quality::Poor);
        assert_eq!(assess(&m(40.0, 3.0, 12.0)).summary, "Dropping packets: expect stutter");
        assert_eq!(assess(&m(40.0, 3.0, 12.0)).quality, Quality::Poor);
        assert_eq!(assess(&m(500.0, 200.0, 50.0)).quality, Quality::Bad, "never below bad");
    }

    #[test]
    fn assessment_without_data_is_measuring_and_silence_is_failing() {
        let none = Metrics {
            ping_ms: None,
            jitter_ms: None,
            loss_pct: None,
            silent_ms: 0.0,
            waiting_ms: None,
            samples: 0,
        };
        assert_eq!(assess(&none).summary, "Measuring");
        let silent = Metrics { silent_ms: 7000.0, ..none };
        assert_eq!(assess(&silent).summary, "Connection failing");
        assert_eq!(assess(&silent).quality, Quality::Bad);
    }

    #[test]
    fn bars_follow_the_rating() {
        assert_eq!(Quality::Good.bars(), 4);
        assert_eq!(Quality::Bad.bars(), 1);
        assert_eq!(Quality::NoData.bars(), 0);
    }

    #[test]
    fn the_label_is_the_rounded_ping_and_caps_at_999() {
        let m = |p| Metrics {
            ping_ms: Some(p),
            jitter_ms: None,
            loss_pct: None,
            silent_ms: 0.0,
            waiting_ms: None,
            samples: 1,
        };
        assert_eq!(label(&m(41.6)), "42");
        assert_eq!(label(&m(0.2)), "1");
        assert_eq!(label(&m(2500.0)), "999+");
    }

    #[test]
    fn the_tip_names_the_numbers() {
        let m = Metrics {
            ping_ms: Some(42.0),
            jitter_ms: Some(6.0),
            loss_pct: Some(0.0),
            silent_ms: 900.0,
            waiting_ms: None,
            samples: 9,
        };
        let text = tip(&m);
        assert!(text.starts_with("Good connection: ping 42 ms, jitter 6 ms, loss 0%"));
        let measuring = Metrics {
            loss_pct: None,
            ..m
        };
        assert!(tip(&measuring).contains("loss still being measured"));
    }

    #[test]
    fn marks_sit_inside_the_right_end_of_the_name_cell() {
        let cell = Rect::new(100.0, 50.0, 600.0, 40.0);
        let m = marks(cell, false).unwrap();
        for bar in m.bars {
            assert!(bar.x >= cell.x && bar.x + bar.w <= cell.x + cell.w);
            assert!(bar.y >= cell.y && bar.y + bar.h <= cell.y + cell.h);
        }
        assert!(m.bars[0].h < m.bars[3].h, "rising");
        assert!(m.bars[0].x < m.bars[3].x);
        assert!(m.label.x + m.label.w <= m.bars[0].x + 0.01);
        assert!(m.hover.x <= m.label.x && m.hover.x + m.hover.w >= m.bars[3].x + m.bars[3].w);
        // The name keeps well over half the cell.
        assert!(m.hover.w < cell.w * 0.4);
    }

    #[test]
    fn a_cramped_cell_gets_no_marks() {
        assert_eq!(marks(Rect::new(0.0, 0.0, 80.0, 40.0), false), None);
        assert_eq!(marks(Rect::new(0.0, 0.0, 0.0, 40.0), false), None);
        assert_eq!(marks(Rect::new(0.0, 0.0, 600.0, 6.0), false), None);
    }
}
