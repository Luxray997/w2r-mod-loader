# Peer protocol

Everything the host sends to and accepts from other players. All of it rides the game's own
reliable, in-order lobby channel as one message type the base game does not use, and all of it
is **lobby-only**. The envelope and the plugin subsystem are specified in [abi.md](abi.md)
("Peer messages"); this document covers the host's own subsystems. The wire format is the one
wc2r-plus defines in its `PROTOCOL.md`, so the two mods see each other in a lobby.

```
[0]      0xE7            message type (unused by the base game)
[1..3]   'W' '2' 'R'     magic: this, not the type byte, marks a message as ours
[4]      subsystem       'H' handshake | 'M' map sharing | 'L' lobby settings | 'P' plugin
[5..]    the subsystem's own bytes; multi-byte integers are little-endian
```

Rules shared by every subsystem (`crates/w2r-core/src/peer.rs`):

- Our messages are removed from the stream before the game sees them. The stock `0x2A`
  message (a leftover file transfer) is dropped as well.
- A receiver uses the net slot the transport reports, never one named inside a message. The
  host of the lobby is always net slot 0.
- A net slot that sends more than 128 W2R messages per second has the excess dropped before
  any subsystem sees them, and one `GetNextPeerPacket` call consumes at most 64 of our messages
  before the game gets a turn, so the game's own traffic is never starved.
- Nothing is sent outside a lobby, and nothing received outside one is acted on.
- Messages that arrive on another thread, or while the host is inside a plugin callback, wait
  in a bounded queue (256) for the next frame; a flood beyond that is dropped and counted.

Subsystems `'M'` and `'L'` are consumed but have no handler yet (their ports are later Phase 3
steps). `'P'` is the only subsystem third-party plugins use.

## Handshake, subsystem `'H'`

Lets players running the host (or wc2r-plus) find each other, fills the roster that
`peer_send` and `W2R_EVT_PEER_MESSAGE` are gated on, and carries the simulation fingerprint
([load-policy.md](load-policy.md)). Implementation: `crates/w2r-core/src/handshake.rs` (pure
logic, tested without the game) driven by `session.rs`.

| Offset | Size | Field |
|---|---|---|
| 5 | 1 | protocol version, 1 |
| 6 | 1 | kind: 1 HELLO, 2 REPLY |
| 7 | 4 | nonce: chosen on each lobby entry, identifies this instance |
| 11 | 4 | capabilities: bit 0 handshake, bit 1 reserved (was "strict"; ignored), bit 2 map sharing |
| 15 | 4 | simulation fingerprint, the 4 bytes in fingerprint order (wc2r-plus sends zeros) |
| 19 | 16 | version text, ASCII, NUL-padded: `w2rpm <version>` (wc2r-plus sends its own version) |

35 bytes. Longer messages are accepted and the extra bytes ignored, so a later version can
append fields. Another protocol version, or fewer than 35 bytes, is ignored (one log line per
stay in the lobby).

- **HELLO** is broadcast when the local player enters a lobby, when another player appears in
  the transport's peer list, and when the local capabilities change (map sharing toggled). It is
  the only message a player without a mod receives; the base game ignores it.
- **REPLY** goes to the HELLO's sender only, once per nonce. A REPLY is never answered, so two
  instances cannot loop.
- A player's own HELLO comes back to it; the matching nonce tells it which net slot it occupies.
- The nonce only needs to be unique within a lobby. It is not random in a cryptographic sense
  and is not a secret. A net slot that presents a new nonce within 5 s of its last one is
  ignored, so cycling nonces cannot draw a REPLY or a log line each time; a change of
  capabilities within one nonce is logged at most once per 5 s.
- A net slot that disappears from the transport's peer list (checked once per second) is
  forgotten, so a newcomer in the same slot is not treated as announced.
- The version text is shown only after anything outside printable ASCII is replaced with `?`.

### Fingerprint policy

Decided per [load-policy.md](load-policy.md) and carried out here:

- The host compares each announced peer's fingerprint with its own and logs a mismatch once. A
  joiner does the same for the host (net slot 0) only. Nothing is shown to the user and nobody is
  removed; the fingerprint is used by features that need identical code on every machine.

It does not authenticate anyone and the fingerprint only detects mismatch (a collision at worst
misses a warning). It does not cover plugin settings; a simulation-affecting plugin must agree
those itself through peer messages (`'P'`). A player without the mod is simply not
checked.
