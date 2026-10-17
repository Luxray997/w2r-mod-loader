//! Link sharing: players who run the mod tell each other how good their own connection to each
//! other player is, so everyone can be warned about a link that will hold the game back. The game
//! measures only the links of the machine it runs on (`netquality.rs`); a joiner cannot see
//! whether two *other* players reach each other. Each mod user sends the figures it measured to
//! the other mod users every few seconds in the lobby, as one small message of our own subsystem
//! (`'Q'`), and every machine puts the figures together into a table of who reaches whom.
//!
//! A lockstep game runs at the pace of its slowest link, so the interesting question is which link
//! that is and who is at its end. `analyse` answers it: a player with poor links to two or more
//! people is named as the cause; a poor link on its own is named as a pair. A finding has to hold
//! for a few seconds before it is shown, and is shown once per episode.
//!
//! A player without the mod sends nothing, so links *between* two such players are unknown;
//! links from a mod user to anybody are always known to that user. Everything here is pure and
//! tested; [`frame`] and [`deliver`] are the thin live parts.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::netquality::{Metrics, Quality, SILENT_MS, quality};
use crate::peer;

/// The envelope's subsystem byte.
pub const SUBSYSTEM: u8 = b'Q';
const PROTOCOL: u8 = 1;
const ENTRY_LEN: usize = 6;
const NO_JITTER: u8 = u8::MAX;
const SLOTS: usize = 8;
const NO_PING: u16 = u16::MAX;
const NO_LOSS: u8 = u8::MAX;
/// A link is worth a warning above this ping, or at this loss.
pub const WARN_PING_MS: u16 = 250;
pub const WARN_LOSS_PCT: u8 = 5;
/// Steady jitter (see `netquality`) above this. The ping is read once per rendered frame, so
/// ordinary readings wobble by 15 to 30 ms on their own; this is well clear of that.
pub const WARN_JITTER_MS: u8 = 50;
/// Round trips needed before a ping is trusted (a first reading can be a spike at join time).
pub const MIN_SAMPLES_PING: usize = 3;
/// Round trips needed before jitter and loss are trusted.
pub const MIN_SAMPLES_STEADY: usize = 12;
/// Late heartbeats needed before loss counts: one is a hiccup, not a pattern.
pub const MIN_LATE: f32 = 2.0;
/// How often a player's figures are sent.
pub const SEND_EVERY_MS: u64 = 2_000;
/// Figures older than this are not used (the player left, or stopped sending).
pub const STALE_MS: u64 = 10_000;
/// A problem has to last this long before it is shown.
pub const CONFIRM_MS: u64 = 10_000;
/// Clear for this long and the problem is over; if it returns it is a new one.
pub const CLEAR_MS: u64 = 10_000;
/// The same problem is not shown again within this long.
pub const REPEAT_MS: u64 = 120_000;
/// No lobby frame for this long: the lobby was left, and what was gathered belongs to it.
const LOBBY_GAP_MS: u64 = 3_000;

/// What one player measured about the link to one other player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Link {
    pub ping_ms: Option<u16>,
    pub loss_pct: Option<u8>,
    pub jitter_ms: Option<u8>,
    /// No packets for a while (`netquality::SILENT_MS`).
    pub silent: bool,
}

impl Link {
    /// `None` when there is nothing worth saying yet (no round trip measured, not silent).
    pub fn from_metrics(m: &Metrics) -> Option<Link> {
        let silent = m.silent_ms >= SILENT_MS;
        let trusted = m.samples >= MIN_SAMPLES_PING;
        if !silent && (m.ping_ms.is_none() || !trusted) {
            return None;
        }
        let steady = m.samples >= MIN_SAMPLES_STEADY;
        // A loss figure is a share of the recent round trips; it is only a pattern when at least
        // `MIN_LATE` of them were late, whatever share one late heartbeat makes of a short run.
        let loss = m.loss_pct.filter(|_| steady).map(|l| {
            if l * m.samples as f32 / 100.0 < MIN_LATE - 0.5 { 0.0 } else { l }
        });
        Some(Link {
            ping_ms: m.ping_ms.filter(|_| trusted).map(|p| p.round().clamp(0.0, 60_000.0) as u16),
            loss_pct: loss.map(|l| l.round().clamp(0.0, 100.0) as u8),
            jitter_ms: m
                .jitter_ms
                .filter(|_| steady)
                .map(|j| j.round().clamp(0.0, f32::from(NO_JITTER - 1)) as u8),
            silent,
        })
    }

    pub fn quality(&self) -> Quality {
        quality(&Metrics {
            ping_ms: self.ping_ms.map(f32::from),
            jitter_ms: None,
            loss_pct: self.loss_pct.map(f32::from),
            silent_ms: if self.silent { SILENT_MS } else { 0.0 },
            waiting_ms: None,
            samples: 5,
        })
    }

    /// Bad enough to slow the game for everybody. Ping, loss and jitter are judged separately,
    /// any one of them is enough: a fast link that drops packets is as bad as a slow one.
    pub fn is_bad(&self) -> bool {
        self.reason().is_some()
    }

    /// What is wrong with the link, worst thing first; `None` when nothing is.
    pub fn reason(&self) -> Option<String> {
        if self.silent {
            return Some("no packets".to_owned());
        }
        if let Some(p) = self.ping_ms.filter(|&p| p > WARN_PING_MS) {
            return Some(format!("{p} ms"));
        }
        if let Some(l) = self.loss_pct.filter(|&l| l >= WARN_LOSS_PCT) {
            return Some(format!("{l}% packet loss"));
        }
        if let Some(j) = self.jitter_ms.filter(|&j| j > WARN_JITTER_MS) {
            return Some(format!("unsteady, jitter {j} ms"));
        }
        None
    }

    fn worse(self, other: Link) -> Link {
        let key = |l: &Link| (l.is_bad(), l.ping_ms.unwrap_or(0));
        if key(&other) > key(&self) { other } else { self }
    }
}

/// A whole message: the envelope, the protocol, a count, then per measured player its net slot,
/// ping (ms, `0xFFFF` = none), loss (percent, `0xFF` = none), jitter (ms, `0xFF` = none) and
/// flags (bit 0 = silent).
pub fn encode(links: &[(u8, Link)]) -> Vec<u8> {
    let links = &links[..links.len().min(SLOTS)];
    let mut out = vec![
        peer::TYPE,
        peer::MAGIC[0],
        peer::MAGIC[1],
        peer::MAGIC[2],
        SUBSYSTEM,
        PROTOCOL,
        links.len() as u8,
    ];
    for (net, l) in links {
        out.push(*net);
        out.extend_from_slice(&l.ping_ms.unwrap_or(NO_PING).to_le_bytes());
        out.push(l.loss_pct.unwrap_or(NO_LOSS));
        out.push(l.jitter_ms.unwrap_or(NO_JITTER));
        out.push(u8::from(l.silent));
    }
    out
}

/// The figures in a message body (everything after the envelope). Longer bodies are accepted so a
/// later version can append fields; another protocol, a short body or a slot off the table is
/// refused as a whole.
pub fn decode(body: &[u8]) -> Option<Vec<(u8, Link)>> {
    let [PROTOCOL, count, rest @ ..] = body else {
        return None;
    };
    let count = usize::from(*count);
    if count > SLOTS || rest.len() < count * ENTRY_LEN {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for e in rest.chunks_exact(ENTRY_LEN).take(count) {
        let net = e[0];
        if usize::from(net) >= SLOTS {
            return None;
        }
        let ping = u16::from_le_bytes([e[1], e[2]]);
        out.push((
            net,
            Link {
                ping_ms: (ping != NO_PING).then_some(ping),
                loss_pct: (e[3] != NO_LOSS).then_some(e[3].min(100)),
                jitter_ms: (e[4] != NO_JITTER).then_some(e[4]),
                silent: e[5] & 1 != 0,
            },
        ));
    }
    Some(out)
}

/// Who reports what about whom: `reports[from][to]`, with when it arrived.
#[derive(Default)]
pub struct Board {
    reports: [[Option<(Link, u64)>; SLOTS]; SLOTS],
}

impl Board {
    /// Replaces everything `from` reported.
    pub fn set(&mut self, from: u8, links: &[(u8, Link)], now: u64) {
        let Some(row) = self.reports.get_mut(usize::from(from)) else {
            return;
        };
        *row = [None; SLOTS];
        for (to, link) in links {
            if to != &from && let Some(cell) = row.get_mut(usize::from(*to)) {
                *cell = Some((*link, now));
            }
        }
    }

    pub fn clear(&mut self) {
        *self = Board::default();
    }

    fn fresh(&self, from: usize, to: usize, now: u64) -> Option<Link> {
        let (link, at) = self.reports[from][to]?;
        (now.saturating_sub(at) <= STALE_MS).then_some(link)
    }

    /// The link between two players: what either end reports, the worse one if both do (one end
    /// is often the unlucky one, and the game waits for the slower).
    pub fn edge(&self, a: usize, b: usize, now: u64) -> Option<Link> {
        match (self.fresh(a, b, now), self.fresh(b, a, now)) {
            (Some(x), Some(y)) => Some(x.worse(y)),
            (x, y) => x.or(y),
        }
    }
}

/// A problem worth telling the players about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Finding {
    /// Poor links to two or more other players: this one is the cause.
    Player(u8),
    /// A poor link between two players, neither of whom has another.
    Pair(u8, u8),
}

/// The problems among the players in `present`. A player at the end of two or more poor links is
/// the cause and takes those links with them (the one with the most first); poor links left
/// over are pairs.
pub fn analyse(board: &Board, present: &[bool; SLOTS], now: u64) -> Vec<Finding> {
    let mut bad = [[false; SLOTS]; SLOTS];
    for a in 0..SLOTS {
        for b in a + 1..SLOTS {
            if present[a] && present[b] && board.edge(a, b, now).is_some_and(|l| l.is_bad()) {
                bad[a][b] = true;
                bad[b][a] = true;
            }
        }
    }
    let degree = |bad: &[[bool; SLOTS]; SLOTS], n: usize| bad[n].iter().filter(|&&x| x).count();
    let mut out = Vec::new();
    loop {
        let best = (0..SLOTS).max_by_key(|&n| (degree(&bad, n), std::cmp::Reverse(n)));
        match best {
            Some(n) if degree(&bad, n) >= 2 => {
                out.push(Finding::Player(n as u8));
                for m in 0..SLOTS {
                    bad[n][m] = false;
                    bad[m][n] = false;
                }
            }
            _ => break,
        }
    }
    for a in 0..SLOTS {
        for b in a + 1..SLOTS {
            if bad[a][b] {
                out.push(Finding::Pair(a as u8, b as u8));
            }
        }
    }
    out
}

#[derive(Clone, Copy)]
struct Episode {
    first: u64,
    last: u64,
    shown: Option<u64>,
}

/// Decides when a finding is worth showing: after it has held for [`CONFIRM_MS`], once per
/// episode, and not again within [`REPEAT_MS`] of the last time.
#[derive(Default)]
pub struct Notifier {
    seen: BTreeMap<Finding, Episode>,
}

impl Notifier {
    pub fn update(&mut self, now: u64, current: &[Finding]) -> Vec<Finding> {
        self.seen
            .retain(|_, e| now.saturating_sub(e.last) <= REPEAT_MS);
        let mut show = Vec::new();
        for f in current {
            let e = self.seen.entry(*f).or_insert(Episode {
                first: now,
                last: now,
                shown: None,
            });
            if now.saturating_sub(e.last) > CLEAR_MS {
                e.first = now;
            }
            e.last = now;
            let held = now.saturating_sub(e.first) >= CONFIRM_MS;
            let due = e.shown.is_none_or(|s| e.first > s && now.saturating_sub(s) >= REPEAT_MS);
            if held && due {
                e.shown = Some(now);
                show.push(*f);
            }
        }
        show
    }
}

fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// The chat line for a finding. `name` gives the display name of a net slot, `me` the local one.
pub fn describe(
    f: Finding,
    board: &Board,
    now: u64,
    me: Option<u8>,
    name: &dyn Fn(u8) -> String,
) -> String {
    let who = |n: u8| if Some(n) == me { "you".to_owned() } else { name(n) };
    let ping = |a: u8, b: u8| {
        board
            .edge(usize::from(a), usize::from(b), now)
            .and_then(|l| l.reason())
            .unwrap_or_default()
    };
    match f {
        Finding::Player(n) => {
            let others: Vec<String> = (0..SLOTS as u8)
                .filter(|&m| {
                    m != n
                        && board
                            .edge(usize::from(n), usize::from(m), now)
                            .is_some_and(|l| l.is_bad())
                })
                .map(|m| format!("{} ({})", who(m), ping(n, m)))
                .collect();
            let subject = if Some(n) == me { "You have".to_owned() } else { format!("{} has", name(n)) };
            format!(
                "{subject} a poor connection to {}. The game runs at the pace of its slowest link.",
                join_names(&others)
            )
        }
        Finding::Pair(a, b) => {
            let (x, y) = (who(a), who(b));
            let lead = if Some(a) == me || Some(b) == me {
                let other = if Some(a) == me { y } else { x };
                format!("Your connection to {other}")
            } else {
                format!("The connection between {x} and {y}")
            };
            format!("{lead} is poor ({}). The game runs at the pace of its slowest link.", ping(a, b))
        }
    }
}

struct State {
    board: Board,
    notifier: Notifier,
    last_frame: u64,
    last_sent: Option<u64>,
    me: Option<u8>,
    lobby_of_net: [Option<u8>; SLOTS],
    /// Findings confirmed and waiting for the lobby screen, which knows the names.
    due: Vec<Finding>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    let mut g = STATE.lock().ok()?;
    let s = g.get_or_insert_with(|| State {
        board: Board::default(),
        notifier: Notifier::default(),
        last_frame: 0,
        last_sent: None,
        me: None,
        lobby_of_net: [None; SLOTS],
        due: Vec::new(),
    });
    Some(f(s))
}

/// One lobby frame: files the figures this machine measured (`local`, per net slot), sends them
/// to the other mod users every [`SEND_EVERY_MS`] and looks for problems. `announced` has bit n set
/// for a net slot that runs the mod; `send` delivers one message to a net slot.
pub fn frame(
    now: u64,
    announced: u8,
    me: Option<u32>,
    lobby_of_net: [Option<u8>; SLOTS],
    local: &[Option<Metrics>; SLOTS],
    mut send: impl FnMut(u32, &[u8]) -> bool,
) {
    let Some(me) = me.filter(|&m| (m as usize) < SLOTS).map(|m| m as u8) else {
        return;
    };
    let msg = with(|s| {
        if now.saturating_sub(s.last_frame) > LOBBY_GAP_MS {
            // A different lobby than the one these figures came from.
            s.board.clear();
            s.notifier = Notifier::default();
            s.due.clear();
            s.last_sent = None;
        }
        s.last_frame = now;
        s.me = Some(me);
        s.lobby_of_net = lobby_of_net;
        let mine: Vec<(u8, Link)> = local
            .iter()
            .enumerate()
            .filter_map(|(n, m)| Some((n as u8, Link::from_metrics(m.as_ref()?)?)))
            .collect();
        s.board.set(me, &mine, now);
        let present: [bool; SLOTS] = core::array::from_fn(|n| lobby_of_net[n].is_some());
        let found = analyse(&s.board, &present, now);
        let shown = s.notifier.update(now, &found);
        s.due.extend(shown);
        let send_now = !mine.is_empty()
            && s.last_sent.is_none_or(|t| now.saturating_sub(t) >= SEND_EVERY_MS);
        if send_now {
            s.last_sent = Some(now);
        }
        send_now.then(|| encode(&mine))
    })
    .flatten();
    if let Some(msg) = msg {
        for n in 0..SLOTS as u32 {
            if n != u32::from(me) && announced & (1 << n) != 0 {
                send(n, &msg);
            }
        }
    }
}

/// Figures arrived from net slot `sender` (the packet hook; `body` follows the envelope). Used
/// only while the lobby is being shown, and only from the slot the transport names.
pub fn deliver(sender: u32, body: &[u8], now: u64) {
    if sender as usize >= SLOTS {
        return;
    }
    let Some(links) = decode(body) else { return };
    with(|s| {
        if now.saturating_sub(s.last_frame) <= LOBBY_GAP_MS && s.me != Some(sender as u8) {
            s.board.set(sender as u8, &links, now);
        }
    });
}

/// The warnings that became due, as chat lines. `names` is indexed by lobby slot.
pub fn take_notices(names: &[String; SLOTS], now: u64) -> Vec<String> {
    with(|s| {
        let due = std::mem::take(&mut s.due);
        let lobby = s.lobby_of_net;
        let name = |n: u8| {
            lobby
                .get(usize::from(n))
                .copied()
                .flatten()
                .and_then(|l| names.get(usize::from(l)))
                .filter(|t| !t.is_empty())
                .cloned()
                .unwrap_or_else(|| format!("Player {}", n + 1))
        };
        due.into_iter()
            .map(|f| describe(f, &s.board, now, s.me, &name))
            .collect()
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(ping: u16) -> Link {
        Link {
            ping_ms: Some(ping),
            loss_pct: Some(0),
            jitter_ms: Some(5),
            silent: false,
        }
    }

    fn all(n: usize) -> [bool; SLOTS] {
        core::array::from_fn(|i| i < n)
    }

    #[test]
    fn a_message_round_trips_after_its_envelope() {
        let links = vec![
            (1, link(40)),
            (
                3,
                Link {
                    ping_ms: None,
                    loss_pct: Some(12),
                    jitter_ms: None,
                    silent: true,
                },
            ),
        ];
        let msg = encode(&links);
        assert_eq!(&msg[..5], &[peer::TYPE, b'W', b'2', b'R', SUBSYSTEM]);
        assert_eq!(decode(&msg[5..]), Some(links));
    }

    #[test]
    fn bad_messages_are_refused_whole() {
        assert_eq!(decode(&[]), None);
        assert_eq!(decode(&[2, 0]), None, "another protocol");
        assert_eq!(decode(&[1, 2, 0, 0, 0, 0, 0]), None, "short");
        assert_eq!(decode(&[1, 9]), None, "too many");
        assert_eq!(decode(&[1, 1, 8, 10, 0, 0, 0, 0]), None, "slot off the table");
        // A later version may append fields.
        assert_eq!(decode(&[1, 1, 2, 10, 0, 255, 255, 0, 99, 99]).unwrap()[0].1.ping_ms, Some(10));
    }

    #[test]
    fn a_measurement_with_nothing_to_say_is_not_shared() {
        let m = Metrics {
            ping_ms: None,
            jitter_ms: None,
            loss_pct: None,
            silent_ms: 100.0,
            waiting_ms: None,
            samples: 0,
        };
        assert_eq!(Link::from_metrics(&m), None);
        let silent = Metrics { silent_ms: SILENT_MS + 1.0, ..m };
        assert!(Link::from_metrics(&silent).unwrap().silent);
        assert!(Link::from_metrics(&silent).unwrap().is_bad());
    }

    fn metrics_with(samples: usize, ping: f32, loss: Option<f32>, jitter: Option<f32>) -> Metrics {
        Metrics {
            ping_ms: Some(ping),
            jitter_ms: jitter,
            loss_pct: loss,
            silent_ms: 0.0,
            waiting_ms: None,
            samples,
        }
    }

    #[test]
    fn early_readings_are_not_trusted() {
        // Two round trips: the ping could be a join-time spike.
        assert_eq!(Link::from_metrics(&metrics_with(2, 900.0, None, None)), None);
        // Enough for a ping, not for loss or jitter.
        let l = Link::from_metrics(&metrics_with(5, 40.0, Some(20.0), Some(90.0))).unwrap();
        assert_eq!((l.loss_pct, l.jitter_ms), (None, None));
        assert!(!l.is_bad());
    }

    #[test]
    fn one_late_heartbeat_is_a_hiccup_two_are_a_pattern() {
        // 1 late of 14 is 7%: over the threshold as a share, but only one event.
        let one = Link::from_metrics(&metrics_with(14, 40.0, Some(100.0 / 14.0), Some(5.0))).unwrap();
        assert_eq!(one.loss_pct, Some(0));
        assert!(!one.is_bad());
        let two = Link::from_metrics(&metrics_with(14, 40.0, Some(200.0 / 14.0), Some(5.0))).unwrap();
        assert_eq!(two.loss_pct, Some(14));
        assert!(two.is_bad());
        assert_eq!(two.reason().unwrap(), "14% packet loss");
    }

    #[test]
    fn ping_loss_and_jitter_are_judged_separately() {
        let fast_but_lossy = Link { loss_pct: Some(8), ..link(40) };
        assert!(fast_but_lossy.is_bad());
        assert_eq!(fast_but_lossy.reason().unwrap(), "8% packet loss");
        let fast_but_unsteady = Link { jitter_ms: Some(70), ..link(40) };
        assert!(fast_but_unsteady.is_bad());
        assert!(fast_but_unsteady.reason().unwrap().contains("jitter 70"));
        assert!(!Link { jitter_ms: Some(50), ..link(40) }.is_bad(), "ordinary wobble");
        assert_eq!(link(400).reason().unwrap(), "400 ms");
    }

    #[test]
    fn only_poor_links_count() {
        assert!(!link(150).is_bad());
        assert!(!link(250).is_bad());
        assert!(link(251).is_bad());
        assert!(!Link { loss_pct: Some(4), ..link(40) }.is_bad());
        assert!(Link { loss_pct: Some(5), ..link(40) }.is_bad());
    }

    fn board(edges: &[(u8, u8, u16)]) -> Board {
        let mut b = Board::default();
        for from in 0..SLOTS as u8 {
            let row: Vec<(u8, Link)> = edges
                .iter()
                .filter(|e| e.0 == from)
                .map(|&(_, y, p)| (y, link(p)))
                .collect();
            b.set(from, &row, 0);
        }
        b
    }

    #[test]
    fn a_player_with_two_poor_links_is_the_cause() {
        // Player 2 is slow to everybody; the rest reach each other fine.
        let b = board(&[(0, 1, 40), (0, 2, 400), (1, 2, 380), (0, 3, 50), (3, 2, 420)]);
        assert_eq!(analyse(&b, &all(4), 0), vec![Finding::Player(2)]);
    }

    #[test]
    fn one_poor_link_is_a_pair() {
        let b = board(&[(0, 1, 40), (1, 2, 400), (0, 2, 30)]);
        assert_eq!(analyse(&b, &all(3), 0), vec![Finding::Pair(1, 2)]);
    }

    #[test]
    fn a_good_lobby_has_no_findings() {
        let b = board(&[(0, 1, 40), (1, 2, 90), (0, 2, 30)]);
        assert!(analyse(&b, &all(3), 0).is_empty());
    }

    #[test]
    fn either_end_can_report_a_link_and_the_worse_one_counts() {
        let mut b = Board::default();
        b.set(1, &[(2, link(60))], 0);
        b.set(2, &[(1, link(500))], 0);
        assert_eq!(b.edge(1, 2, 0).unwrap().ping_ms, Some(500));
        assert_eq!(b.edge(2, 1, 0).unwrap().ping_ms, Some(500));
    }

    #[test]
    fn stale_figures_and_absent_players_are_ignored() {
        let b = board(&[(0, 1, 400), (0, 2, 400)]);
        assert!(analyse(&b, &all(3), STALE_MS + 1).is_empty());
        let mut present = all(3);
        present[2] = false;
        assert_eq!(analyse(&b, &present, 0), vec![Finding::Pair(0, 1)]);
    }

    #[test]
    fn a_report_replaces_the_previous_one() {
        let mut b = Board::default();
        b.set(1, &[(2, link(400))], 0);
        b.set(1, &[], 5);
        assert!(b.edge(1, 2, 5).is_none());
    }

    #[test]
    fn a_finding_is_shown_after_it_holds_and_only_once() {
        let mut n = Notifier::default();
        let f = [Finding::Pair(1, 2)];
        assert!(n.update(0, &f).is_empty());
        assert!(n.update(CONFIRM_MS - 1, &f).is_empty());
        assert_eq!(n.update(CONFIRM_MS, &f), f);
        assert!(n.update(CONFIRM_MS + 2_000, &f).is_empty(), "once");
    }

    #[test]
    fn a_blip_is_never_shown_and_a_return_inside_the_repeat_window_is_quiet() {
        let mut n = Notifier::default();
        let f = [Finding::Player(3)];
        assert!(n.update(0, &f).is_empty());
        assert!(n.update(2_000, &[]).is_empty());
        // Gone for longer than CLEAR_MS: the clock starts over.
        assert!(n.update(20_000, &f).is_empty());
        assert!(n.update(24_000, &f).is_empty());
        assert_eq!(n.update(30_000, &f), f);
        // Fixed, then broken again soon: not repeated.
        assert!(n.update(40_000, &[]).is_empty());
        assert!(n.update(50_000, &f).is_empty());
        assert!(n.update(61_000, &f).is_empty());
        // After the repeat window it is news again.
        assert!(n.update(150_000, &f).is_empty());
        assert_eq!(n.update(160_000, &f), f);
    }

    #[test]
    fn the_text_names_the_players_and_says_you() {
        let b = board(&[(0, 2, 400), (1, 2, 380)]);
        let name = |n: u8| ["Ann", "Bob", "Cat"][usize::from(n)].to_owned();
        let t = describe(Finding::Player(2), &b, 0, Some(0), &name);
        assert_eq!(
            t,
            "Cat has a poor connection to you (400 ms) and Bob (380 ms). The game runs at the pace of its slowest link."
        );
        let t = describe(Finding::Player(2), &b, 0, Some(2), &name);
        assert!(t.starts_with("You have a poor connection to Ann (400 ms) and Bob (380 ms)"), "{t}");
        let b = board(&[(1, 2, 400)]);
        let t = describe(Finding::Pair(1, 2), &b, 0, Some(1), &name);
        assert!(t.starts_with("Your connection to Cat is poor (400 ms)"), "{t}");
        let t = describe(Finding::Pair(1, 2), &b, 0, Some(0), &name);
        assert!(t.starts_with("The connection between Bob and Cat is poor (400 ms)"), "{t}");
    }

    fn metrics(ping: f32) -> Metrics {
        Metrics {
            ping_ms: Some(ping),
            jitter_ms: None,
            loss_pct: Some(0.0),
            silent_ms: 0.0,
            waiting_ms: None,
            samples: 8,
        }
    }

    /// Everything together on one machine (net slot 0): its own figures, one message from net slot
    /// 1 about the link to net slot 2, and a warning that comes out as a chat line.
    #[test]
    fn a_joiners_report_completes_the_picture() {
        let lobby = [Some(0), Some(1), Some(2), None, None, None, None, None];
        let mut local: [Option<Metrics>; SLOTS] = Default::default();
        local[1] = Some(metrics(40.0));
        local[2] = Some(metrics(45.0));
        let mut sent = Vec::new();
        let names: [String; SLOTS] = core::array::from_fn(|i| ["Ann", "Bob", "Cat", "", "", "", "", ""][i].to_owned());
        let _ = take_notices(&names, 0);
        let mut now = 1_000;
        frame(now, 0b110, Some(0), lobby, &local, |n, m| {
            sent.push((n, m.to_vec()));
            true
        });
        assert_eq!(sent.len(), 2, "to both mod users");
        assert_eq!(decode(&sent[0].1[5..]).unwrap().len(), 2);
        let report = encode(&[(2, link(420))]);
        for _ in 0..14 {
            now += 1_000;
            deliver(1, &report[5..], now);
            frame(now, 0b110, Some(0), lobby, &local, |_, _| true);
        }
        let lines = take_notices(&names, now);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].starts_with("The connection between Bob and Cat is poor (420 ms)"));
        assert!(take_notices(&names, now).is_empty(), "taken once");
    }
}
