# Connection quality research (ping and loss in the lobby)

Requirement: it must work for players who do **not** run w2rpm, so nothing can depend on a plugin
message being answered. The measurement has to come from the game's own transport, on the machine
that shows it. Ghidra on `Warcraft II.exe` build 2818, read-only (`D:\Tools\ghidra_scripts\obs.sh`,
and `FindStrRefs.java` for string references). Addresses are static (pre-ASLR).

## The transport is a small reliable-UDP layer

`WRTSNet::MultiplayerImpl` (vtable `0x88934c`, net manager `DAT_0095bbd4`, transport at `+0x68`).
Each remote player has a **peer record** (`FUN_007d5fd0(netSlot)` returns it, 0 if none). Four
channels (0 to 3); the packet header is:

| offset | size | meaning |
|---|---|---|
| 0 | u16 | checksum (`FUN_007d27a0`) |
| 2 | u16 | length |
| 4 | u16 | sequence number on the channel |
| 6 | u16 | ack: the highest sequence received from the peer on the channel |
| 8 | u8 | channel |
| 9 | u8 | kind (1 and 2 are ack and resend-request packets) |
| 10 | u8 | sender's net slot |

Peer record fields seen so far (offsets from the record pointer):

| offset | meaning |
|---|---|
| `+0x30..+0x3f` | the peer's 16-byte address |
| `+0x40` | state flags (`8` = dropped, `4` = leaving) |
| `+0x44` | **time of the last packet received from the peer** (game clock, ms) |
| `+0x48` | time of the last resend request |
| `+0x4c` | **time the last heartbeat was sent** |
| `+0x50` | **heartbeats sent** (counter) |
| `+0x54` | **heartbeats echoed back by the peer** (counter) |
| `+0x80 + ch*8` | list of sent-but-unacked packets for channel `ch` (entry `+0x24` = last send time) |
| `+0xc8 + ch*2` | next sequence to send on channel `ch` |
| `+0xd0 + ch*2` | next sequence expected from the peer |
| `+0xda + ch*2` | highest sequence received (what we put in the ack field) |
| `+0xe2 + ch*2` | the ack the peer last sent us |
| `+0xec + ch*4` | time an ack became owed (0 = none) |
| `+0xfd` | non-zero = skip (loopback or unused) |
| `+0x10a` | the peer's net slot, `0xff` = gone |

The game clock is `FUN_00620b70()` (a singleton) with a virtual call at vtable `+8` returning
milliseconds.

## The heartbeat is a ping

`FUN_007d69a0(record, interval)`: if `record+0x54 == record+0x50` (the last heartbeat was echoed) and
`interval` ms have passed since `record+0x4c`, it stores the clock in `+0x4c`, increments `+0x50` and
sends the new value on channel 0 (`FUN_007d6640(record, 0, 4, &counter, 4)`). `FUN_007d48f0`
(the per-turn status builder) also calls it with interval 0 when no packet has arrived from the peer
for half the timeout. A peer that is silent for 8000 ms is dropped (`FUN_007d5700`).

So the round trip time to a peer is `now - record+0x4c` at the moment `record+0x54` catches up with
`record+0x50`. Because it is stop-and-wait, a heartbeat (or its echo) that is lost is resent by the
reliable layer, which shows up as a long round trip. Both ends do this, so **every peer is measured
by the game regardless of the mod**. We only need to read the fields.

Topology: the host has a record per joiner. A joiner has a record for the host and, going by
`FUN_007d48f0`, possibly for the other joiners. Show what each machine can see; the host's view is
the one that matters.

## Loss

No loss counter exists. Candidates, cheapest first:

1. **Late heartbeats.** A heartbeat whose round trip exceeds about 3 times the running median (and at
   least 250 ms) was almost certainly resent. Loss = late / total over the last N (say 30)
   heartbeats. Pure polling, no hook.
2. **Resend counter.** `FUN_007d6440(record, buffer, length)` is the resend of a queued packet
   (called from the tick `FUN_007d5700` when `entry+0x24 + timeout` passed). A detour that counts
   calls per record gives true retransmits; divide by packets sent (`+0xc8` deltas).
3. **Time since last packet** (`now - record+0x44`): not loss, but a stall indicator that shows a
   dying link before the 8 s drop.

Version 1 is polling only (ping, jitter, late-heartbeat loss, silence). The resend detour is version
2 if (1) proves too coarse.

## Implementation (2026-10-10, built and unit-tested, never run in game)

- The heartbeat interval is the `MultiplayerImpl` field at `+0x44`, set to **4000 ms** by the
  constructor, so a peer is measured **once every 4 s**. The ping is the median of the last 5
  round trips, the loss figure needs 5 and looks at the last 30 (two minutes).
- `netquality.rs` (pure, tested): `Tracker::feed(now, Raw{sent, echoed, heard})` times the
  counters between polls (an event is placed in the middle of the poll gap; a gap over 100 ms is a
  hitch and its timing is dropped), `Metrics` (ping, jitter, loss, silence, waiting), `quality`
  (ping picks Good <=100 / Fair <=200 / Poor <=350 / Bad; loss >=3% drops one step, >=10% two;
  5 s without a packet is Bad), `label`, `tip` and `marks` (four bars and a number at the right
  end of the name cell). Loss is "heartbeats that took over max(250 ms, 3x the median)", a proxy
  for resends; a heartbeat still unechoed past that limit counts too.
- `netquality::game::Reader` (x86): calls `TransportFindPeer` (`0x007d5fd0`, thiscall, new
  symbol) for net slots 0 to 7 on the object at `NetSession+0x68`, reads `+0x44/+0x50/+0x54`,
  skips records with `+0x10a == 0xff` or `+0xfd != 0`, resets a slot's tracker when its record
  pointer changes or the counter goes backwards, and maps net slots to lobby slots with
  `LobbySlotByNetSlot`. Everything is under the crash guard. Our own seat has no record, so it
  shows nothing.
- `lobbyscreen.rs` polls once per frame in `draw_marks` and calls `quality_marks` per seat;
  hovering the bars or number shows `tip`. No setting; a missing symbol only hides the bars.
- Accuracy: one render frame (about 16 ms) either side of a round trip, so pings under about 30 ms
  read as 10 to 30. Good enough for the rating; not a replacement for `ping`.

## To check in the game

1. Host a lobby, a second player joins: within 4 s of joining their row gets four bars and a
   number; the host's own row has none. Hover shows "Good connection: ping N ms, jitter ...".
2. The number should be near the real round trip (compare with `ping` to their address). Try with a
   second player **without the mod**: the bars must appear all the same.
3. Throttle or unplug the second player's network: the bars drop and "No packets for ..." shows
   before the game's 8 s drop.
4. A joiner sees the bars for the host (and maybe for other joiners, if the transport keeps those
   records): note which.
5. If nothing ever appears, `+0x50` may not move in a lobby; look in the log for the first
   sample, and read the three fields with a debugger.

## Open

- The record's list object layout (`+0x80`) is not needed for v1.
- The heartbeat interval (`MultiplayerImpl` field, set from the 'latency is' value in the
  constructor) decides the sample rate; check in game how often `+0x50` moves in a lobby.
- Clock: confirm vtable `+8` of `FUN_00620b70()` is milliseconds (the 8000 comparison says so).
