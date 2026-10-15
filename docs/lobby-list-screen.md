# Multiplayer Lobbies screen (the list of hosted games)

Status: spec, nothing implemented. 2026-10-14. This is the screen between the main menu and a
lobby: Region, Search, the Lobby Name / Host / Players list, the info panel on the right and the
Previous / Create Multiplayer Game / Join Lobby buttons. It is **not** the in-lobby screen
([lobby-screen.md](lobby-screen.md)).

## What we have today

Only one thing touches this screen: `gameinfopatch.rs` fixes the Resources row of the info panel
(`FUN_0053bb50`). Everything else on it is stock. Nothing hooks the list's draw function, and its
window name is **not identified yet** (the in-lobby one is `multiplayer_lobby_screen`; the list's
is not in any of our notes or logs).

## What the data actually is

The list is not ours to query: Battle.net pushes it
([battlenet-stats-research.md](battlenet-stats-research.md), "How the Multiplayer lobby list is
fed" and the captures). Each hosted game reaches the exe as a `GameFound` event and sits in the
exe's `ClientGameMaster` list (object +0x14c: id at +4, name at +0x10, attribute array at
+0x34/+0x38). The attributes the host's own client set are all we get:

| Attribute | Shown today | Notes |
|---|---|---|
| game name | Lobby Name | |
| `szCreatorName` | Host | seen without a `#tag` ("BunterGnoll") |
| `szMapTitle` | Map | the `.pud` file title, not a hash |
| `nCurPlayers` / `nMaxPlayers` | Players | live; the host closing slots lowers max |
| `bInitialGameSpeed` | Game Speed | |
| `lFlags` | Resources, Peasants, Tileset... | 0, 0x3000, 0x3200, 0x7000 seen; bit meanings in `lobbylayout::resources_index` |
| `bStartPositions` | Starting Location | |
| `password` | lock icon | **sent in clear to every browsing client** |
| `gameReady` | none | always 0 |

Plus the game id (u64) and nothing else. A started game is removed from the list about 4 s after
Start; **there is no in-progress list**. The feed only exists while the Multiplayer screen is
open (subscribe slot 8, stop slot 9).

### Not available before joining

These came up in the design discussion and are **not** obtainable from the list as it is:

- **Ping or loss to the host.** `netquality.rs` reads the game's own peer records, which exist only
  after a connection. The list carries no host address. Ping on this screen would need a side
  channel (below) and a probe we do not have.
- **Who is in the game.** Only the count. No names, races, teams, ratings.
- **Mod, plugin set or version.** The mod handshake (`'L'` subsystem, the capability bit) happens
  after joining. Nothing in the advertisement says a host runs us.
- **Whether the map is installed.** Possible only by title: `szMapTitle` against the local map
  registry (`mapdownload::rescan_maps`). Same title does not mean same file (the hash
  check, [map-download.md](map-download.md), happens after joining).

## What we can do with only what is there (no network, no hooks beyond the screen)

Everything here is derived from the attributes above, so it works with or without the mod on the
host, and needs no stats-service call:

1. **Sortable columns and a wider list.** Click a header to sort by name, host, map, players,
   speed. Add columns: Map, Speed, Resources, Open slots (`max - cur`), Password (lock).
   The stock list shows three columns on a screen with a lot of empty space.
2. **Filters.** Hide locked, hide full, hide a map not installed, filter by player count, host and
   map in search (stock search is lobby-name only, if it searches anything beyond that).
3. **Map installed badge.** A tick if `szMapTitle` matches a local map by title, a download arrow
   if not. After the hash check this becomes exact; before it, label it "same name".
4. **Open-slot hint.** "2 open", or "full" greyed out. Full games get removed 6-20 s later, which
   is how the list looks for someone about to join a game that is already starting.
5. **A fuller info panel.** All `lFlags` rows presented (including ones the stock panel does not
   show), the creator's rating line if known (below), and the password row.
6. **Sorting that helps.** Default order: open slots first, then newest, games with a lock last.
   Newest needs the time we first saw each id (the feed has no create time we have found; the
   advertise code sets `game_create_time` but it is not in the `GameFound` we captured).
7. **A "my last games" row.** The local list of games joined recently (kept by us), to rejoin the
   same host.

## What needs the stats service (read API, no new host fields)

`GET /v1/ratings?tags=` is keyed by BattleTag and the list gives `szCreatorName`. That is a gap:

- If the creator name carries the tag (`Name#1234`), the existing client
  ([lobbyratings.rs](../crates/w2r-core/src/lobbyratings.rs), `tag_of`, `want`, the 5-minute
  cache) is reusable as is, asking for up to 16 hosts at a time. Respect the read limit (120 per
  minute per address) and the privacy note in that module's header: asking tells the service who
  is browsing, so it stays behind the match-sharing setting.
- If it does not (the one capture shows a bare name), either add `GET /v1/ratings?names=` that
  returns a rating when exactly one tag has that name prefix (ambiguity returns nothing), or
  map names to tags from our own history. **First task: capture what `szCreatorName` is for a
  host with a BattleTag.** The advertise code also names `game_host_battletag`; whether that
  reaches `GameFound` is untested.

With ratings: a host-rating column, "rated 1210" in the info panel, and a rating range filter.

## Side channel: what the list cannot tell us

If we want player names, the plugin set, ping, observers allowed or the replay policy before
joining, the advertisement cannot carry them (we do not control what the server accepts) so the
options are:

**A. Extra attributes in the advertisement.** The host's client builds the attribute array in
`FUN_00630130`; a hook could add ones like `w2r_ver`, `w2r_obs`, `w2r_roster`. Unknown whether
the Aurora service stores unknown attributes or drops them (the SDK passes them through, the
server may validate). **A spike decides this**: add one attribute on a test host and look for it
in the lobby-log plugin's `GameFound` dump. If it survives, it is the best option: no extra
service, atomic with the game's life, and old clients just ignore it. Also size-limited and
visible to everyone, so nothing private goes in.

**B. A presence record on the stats service**, keyed by the game id the host got from
`AdvertiseSuccess`. The host posts `{game id, roster tags, plugin ids and versions, flags}` (signed
like a result report) on every change; clients look up the visible ids. Costs: new writes (30 per
minute per address limit, D1 limits on the free plan), a record per lobby change, cleanup, a
60 s read cache that would make a roster up to a minute stale, and the privacy question of storing
who is in which lobby. Only worth it if A fails.

**C. Join first, show later.** Everything that needs the host (ping, mod match, roster) appears in
the lobby, which already has them. This is the current state and the fallback.

Decision rule: do the spike for A before designing the roster column. If A fails, ship slices 1-3
below and treat B as optional.

## Replacing the screen: how

Same approach as the create screen and the Alliances dialog: a hook on the screen's draw function,
draw our version inside the game's own window, `Result::Suppress` for the stock one. Constraints
carried over from [lobby-screen.md](lobby-screen.md): never begin a Nuklear window inside the
game's frame, keep the game's own Previous and Join buttons (so Join, the password dialog and
`GameListPopup` messages keep working untouched), fall back to the stock screen on any fault or
missing symbol, behind a `host.json` setting (`enhanced_lobby_list`, off until verified).

Unknowns to resolve in Ghidra before writing any of this:

1. The list screen's window name and draw function (the lobby's is `multiplayer_lobby_screen`;
   this one likely follows the pattern).
2. Where the selected row and the Join action live, so our rows can select and join through the
   game's own paths. Join must reach the same handler (it sends the invite and opens the password
   dialog, `JoinPasswordContent`).
3. The `ClientGameMaster` entry layout, to read the list from memory instead of copying the stock
   screen's rows. The lobby-log plugin already hooks the events, so a mirror (id, attributes) can
   be kept without touching the game's list at all: update on `GameFound`, remove on
   `GameRemoved`.
4. The font and skin handles the stock list uses, so our rows match.

## Slices (each behind the setting, each checked in game)

1. **Read-only mirror.** A module that keeps the current list from the `GameFound` and
   `GameRemoved` events, first-seen time per id, with pure code (parse attributes, flag bits,
   open slots, title match) unit-tested. No drawing. Log it.
2. **Enhanced list.** Hook the screen. Sortable wider columns, filters, map-installed badge,
   full info panel. Stock Previous / Create / Join stay.
3. **Ratings.** Host rating column and info row via the existing client, once the creator-name
   question is answered.
4. **Extra attributes spike (A).** One test attribute, read back. Decides whether player names,
   plugin match, observers and replay policy go on this screen.
5. **If A works:** roster in the info panel, "needs plugin X" and observer/replay badges, balance
   estimate from the ratings (the in-lobby code already has `w2r_stats::balance`).

## Hostile data

Every string on this screen comes from other players or the service: names, map titles, lobby
names. Bound lengths and strip control characters before they reach the game's label functions
(the rule in the create screen). The password is visible in clear in the feed; **we must never log
it** (the lobby-log plugin dumps it, which is acceptable for research and not for shipping), and
we should not display it in the list. Sorting and filtering are local and never sent anywhere.
Anything asked of the stats service (ratings) is behind the sharing setting, rate-limited, and
cached; a service failure leaves the column blank and never blocks Join.

## Not doing

- A game "in progress" list: the feed never has one.
- Ping to a host before joining without a host address (the feed has none).
- Replacing the Create Multiplayer Game screen (done, [create-screen.md](create-screen.md)).
- Region switching: the realm is a single `us` ([battlenet-stats-research.md](battlenet-stats-research.md)).
