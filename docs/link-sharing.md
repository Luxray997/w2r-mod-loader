# Link sharing (conflict warnings between players)

The game measures only the links of the machine it runs on ([connection-quality-research.md](connection-quality-research.md)).
A joiner cannot see whether two *other* players reach each other, and a lockstep game runs at the pace of its
slowest link. So players who run the mod tell each other what they measured, and every mod user is warned in the
lobby chat about a link that will hold the game back.

Code: `crates/w2r-core/src/linkshare.rs` (pure and tested; `frame` and `deliver` are the thin live parts),
`netquality::game::latest_by_net` (the local figures per net slot), the call in `session.rs` (lobby frame) and the
chat line in `lobbyscreen.rs` (`draw_marks`). No setting; lobby only.

## Wire, subsystem `'Q'`

`E7 'W' '2' 'R' 'Q'`, then:

| Offset | Size | Field |
|---|---|---|
| 5 | 1 | protocol, 1 |
| 6 | 1 | count n (0 to 8) |
| 7 | 6 * n | per measured player: net slot (u8), ping ms (u16 LE, `0xFFFF` none), loss percent (u8, `0xFF` none), jitter ms (u8, `0xFF` none), flags (bit 0 = no packets for 5 s) |

Sent every 2 s to each net slot that announced the handshake (never to players without the mod), only when there
is something measured. A later version may append fields; a short body, another protocol or a slot above 7 refuses
the whole message. The sender is the net slot the transport reports, never one named inside. A report replaces the
sender's previous one; figures older than 10 s are ignored, and nothing is kept from an earlier lobby (no lobby
frame for 3 s) or accepted outside one. Net slots are the same on every machine (the host is 0), which is what makes
the figures comparable.

## What counts as a problem

A link is *poor* when any one of these holds (judged separately, so a fast link that drops packets counts):

- ping over 250 ms (`WARN_PING_MS`);
- loss of 5% or more (`WARN_LOSS_PCT`);
- steady jitter over 50 ms (`WARN_JITTER_MS`);
- no packets for 5 s.

The game sends one heartbeat per peer every 4 s and we cannot probe faster without the other player's mod, so
readings are sparse and the rules lean towards not crying wolf:

- A ping is shared only after 3 round trips (about 12 s in the lobby), and a loss or jitter figure after 12 (about
  48 s). The ping is the median of the last 5.
- Loss counts only when at least 2 heartbeats were late; one late heartbeat is a hiccup however big a share of a short
  run it is.
- Jitter is the *median* gap between consecutive round trips over the last 8, so a single resent heartbeat does not
  move it. The 50 ms limit is well above the 15 to 30 ms the one-frame ping accuracy produces on its own.
- When both ends report a link the worse one counts.

Findings:

- **Player(n)**: n has poor links to two or more players, so n is the cause ("Cat has a poor connection to you
  (400 ms) and Bob (380 ms)"). The player with the most poor links is taken first and its links are removed.
- **Pair(a, b)**: a poor link nobody else shares ("The connection between Bob and Cat is poor (420 ms)", "Your
  connection to Cat is poor ...").

Every message ends with "The game runs at the pace of its slowest link."

A finding has to hold for 10 s, is shown once per episode (clear for 10 s ends the episode), and the same finding is
not shown again within 2 minutes. It appears as a line in the lobby chat history on this machine only; nothing is
sent to other players for it (each mod user works it out from the same figures).

## Limits

- Two players without the mod: their link to each other is unknown (a mod user only knows its own links and what
  other mod users report). The more people run the mod, the fuller the picture.
- Ping accuracy is one frame, as for the bars; the thresholds are those of the bars.
- Lobby only: a link that degrades after the match starts is not reported.
- Figures are not authenticated; a modified client can claim a bad link between two honest players. The cost is a
  chat line on other mod users' screens.

## To check in game (three players, at least two with the mod)

1. Everyone good: no line. Host sees the bars as before.
2. Throttle one joiner's connection (or use a distant VPN): after about 10 s the other mod users get "X has a poor
   connection to ..." once, and the throttled player gets "You have ...".
3. Two joiners where only the pair between them is bad (needs a way to degrade just that link): "The connection
   between A and B ...". If joiners have no transport record for each other, this case never appears, because
   neither end can measure it; the host only sees links to itself.
4. Leave and re-enter a lobby: no stale line.
