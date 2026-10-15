# How the game loads unit data (UDTA, UGRD, ALOW)

Ghidra trace of `Warcraft II.exe` build 2818 (project `WC2Remastered`, helpers in
`D:\Tools\ghidra_scripts`, new: `FindScalar.java` / `fs.sh`, `FindBytes.java` / `fb.sh`).
Names below are ours; the addresses are from the 2818 binary. Statements marked *inferred* were
not confirmed in game. Companion data dump: [unit-data-dump.md](unit-data-dump.md).

## Flow

1. `FUN_004c4280` (game/scenario start) calls `FUN_004d2b40(buf, len)` with the PUD in memory.
2. `FUN_004d2b40` sets `DAT_009225a8` (UDTA "use stock") and `DAT_009225ac` (UGRD "use stock") to 1,
   then calls the chunk walker `FUN_004d2ca0(&DAT_008c44e0, 0x10)`.
3. The walker reads each `tag + size` and dispatches through a table of 12-byte entries
   `{ tag[4], handler, required }` at `0x8c44e0`. Chunks must come in table order; a tag not in the
   table is skipped. Handlers: `UDTA` 0x4d2440, `UGRD` 0x4d2500, `ALOW` 0x4d19d0, `SIDE` 0x4d1a70,
   `SGLD` 0x4d2200, `SLBR` 0x4d22a0, `SOIL` 0x4d2340, `AIPL` 0x4d23e0, `MTXM` 0x4d20b0, `SQM ` 0x4d20e0,
   `OILM` 0x4d2110, `REGM` 0x4d21d0, `UNIT` 0x4d1d60, `ERA ` 0x4d1ce0, `DIM ` 0x4d1fe0, `OWNR` 0x4d1b60.
   `TYPE` is checked by `FUN_004d1820` (the "WAR2 MAP" string).
4. After the walk, if `DAT_009225a8 != 0` the game loads the **stock** table from the file
   `rez\unitdata.dat` (`FUN_004c4d10`) and patches four fields (`FUN_004c49a0`). Same for UGRD from
   `rez\upgrades.dat`.

So the stock rules are data files in the install, in exactly the PUD UDTA/UGRD field order, and a
PUD only overrides them when its section says so.

## UDTA handler (0x4d2440)

```
if (multiplayer && !FUN_004872f0(0) && !FUN_004873b0())   // see gate below
    skip the whole section           -> stock rules
else
    read u16 header  -> DAT_009225a8
    if header != 0: skip the rest    -> stock rules
    else: read fields straight into the live globals (descriptor 0x8c0150)
```

- The first u16 of the section is a "use stock" flag. It matches the files: `mutton thinned` and
  both `sheep tag` maps start `01 00` (stock), every custom map starts `00 00`.
- `multiplayer` is `DAT_00922f5b`, set by `FUN_004d65e0` to "connection count > 1".
- **Gate (multiplayer only).** Custom data is honoured when either
  - `FUN_004872f0`: `opts+0x155 == 0 && opts+0x159 == 0 && opts+0x15f != 1`, where `opts` is the lobby
    options struct at `0x916530`. Byte `0x155` is also tested by `FUN_00486f40` and is `== 7` in
    `FUN_004872b0`, so it looks like the game type, with 0 = "Use Map Settings" (*inferred*).
    `0x159` is the lobby flag `0x200` in `0x916560` (written in `FUN_0049c9f0`).
  - `FUN_004873b0`: `0x91667d == 0xf && 0x916689 == 0 && 0x91668a == 1`.
  Single-player always honours the section. UGRD (0x4d2500) has the identical gate.
- Each peer reads the UDTA from its **own copy of the PUD**, nothing is sent from the host. So rules
  stay in sync only because the map is identical (the lobby map hash / map download check).

## Where the data lands

Descriptor at `0x8c0150`: entries `{ dest, elem size, count }`. The globals are plain .data arrays, 110
entries each unless noted:

| global | content | | global | content |
|---|---|---|---|---|
| 0x917128 | overlap frames (u16) | | 0x917eb0 | react range, computer |
| 0x917608 | sight (u32) | | 0x917f20 | react range, human |
| 0x9177c0 | hit points (u16) | | 0x917f90 | armor |
| 0x9178a0 | mana | | 0x918000 | selectable |
| 0x917910 | build time | | 0x918070 | priority |
| 0x917980 | gold (/10) | | 0x9180e0 | basic damage |
| 0x9179f0 | lumber (/10) | | 0x918150 | piercing damage |
| 0x917a60 | oil (/10) | | 0x9181c0 / 0x918230 | weapons / armor upgradable |
| 0x917ad0 | unit size (u16 x2) | | 0x9182a0 | missile type |
| 0x917c88 | box size (u16 x2) | | 0x918310 | unit class |
| 0x917e40 | attack range | | 0x918380 / 0x9183f0 | decay / annoy |
| 0x918460 | mouse-right button (58) | | 0x9184a0 | point value (u16) |
| 0x918580 | can target | | 0x9185f0 | flags (u32) |

The ~1016 obsolete bytes (4 x 127 u16 at `0x917208 .. 0x917508`) and a 254-byte tail
(`0x9187a8`) are read and thrown away. UGRD fills `0x9188a8` (time, 52), then gold, lumber, oil, icon,
group, flags tables from `0x8c02e8`.

Saved games dump and restore the same arrays (`FUN_004c4c60` with the same two descriptors).

## ALOW (0x4d19d0)

Must be exactly `0x180` bytes, copied into six 0x40-byte global blocks, 16 players x u32 each:
`0x919210, 0x919250, 0x919290, 0x9192d0, 0x919310, 0x919350`. No gate: ALOW is honoured in all game
types. `FUN_004d2b40` pre-fills the blocks with per-map defaults (-1 / 0x4020 / 0) before parsing, so a
map without ALOW keeps those. What each of the six blocks means is still unknown (see the raw masks in
the data dump).

## What this means for a plugin

- Because the tables are ordinary globals, a plugin can **patch them after load** (hook the end of
  `FUN_004d2b40`) on every peer with the same inputs and get identical results, with no PUD needed.
- A "ruleset" can also be a replacement `unitdata.dat` / `upgrades.dat`, applied at the
  `FUN_004c4d10` call, or by forcing `DAT_009225a8 = 0` and injecting a UDTA buffer.
- To let a custom map's rules apply in non-"Use Map Settings" lobbies, hook `FUN_004872f0` to return 1.
- Peers must all apply the same ruleset: carry its hash in the lobby handshake (same capability-bit
  mechanism the observer uses) and refuse to start on mismatch, or the sim desyncs.
- Slots 20-25, 35, 44, 46, 47 are zero in stock data, so they are free unit types: the hero maps already
  use them. Whether they can be built or spawned depends on the other tables (UNIT chunk, build
  buttons), not covered here.

## Open

- Confirm in game that `0x916685` is the game type and that custom rules are ignored in melee lobbies.
- Find the hooks' safe point: end of `FUN_004d2b40`, before the first sim tick.
- Meaning of the six ALOW blocks and how they gate build buttons.
