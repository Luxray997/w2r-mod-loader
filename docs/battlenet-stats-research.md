# Battle.net stats: how StarCraft: Remastered gets them (research, 2026-10-12)

Goal: learn how SC:R fills "Players Online" / games, Career stats, Season stats (MMR, wins), History and
Summary, so similar data can be read for Warcraft II: Remastered. Source: StarCraft 1.23.10.13515
(`D:\Games\StarCraft\x86_64`), Ghidra 12.1.4, project `D:\Tools\ghidra_project_sc` (SC1).

## Method / gotchas
- `.text` of SC `StarCraft.exe`, SC `ClientSdk.dll` **and W2R's `ClientSdk.dll`** is encrypted on disk (entropy 8.00).
  W2R's `Warcraft II.exe` is plain. Static analysis of the on-disk files gives junk.
- `tools/research/dumppe.py <proc> <outdir> <module>...` copies the decrypted executable sections out of the
  running process (read-only ReadProcessMemory, no injection) into a copy of the on-disk PE; import that copy.
  Dumps: `SC_dump.exe`, `Sdk_dump.dll` in the SC1 Ghidra project.
- Full Ghidra auto-analysis of the decrypted SDK never finished (PE exception-handling analyzer loops on
  function repair). Import with that analyzer, Decompiler Parameter ID, Stack, Scalar refs etc. disabled
  (`LightAnalysis.java` pre-script) and it completes.
- The decrypted **exe** is control-flow obfuscated (stubs that never return, mis-bounded functions): UI-side
  logic is hard to read. The **SDK** decompiles cleanly (protobuf code), so the protocol is the reliable part.
- String VA = file offset + 0xc00 in SC `.rdata`; Ghidra finds no xrefs to exe strings reliably, scan RIP-relative
  displacements instead.

## Transport
- ClientSdk speaks Blizzard's "classic" protocol (Google protobuf 2.6 lite, `classic.protocol.v1.*`) to Aurora
  services over one authenticated connection (`Client.Aurora.DnsName`). The game talks to the SDK through
  `ClientSdk::I*V1` interfaces; exe classes `battlenet::*Service` / `*MessageReceiver` wrap them.
- Services seen in SC's SDK: gateway, authentication (OnlineStats), toon_profile (GetStats, GetAvatar), url
  (ResolveUrl), webapi (Get/Post), matchmaker, game_master, lobby, leaderboard-less.
- W2R's SDK additionally has `leaderboard.GetRanks`, `toon_profile.GetGamemodeStats/GetMatchHistory/
  GetMatchResults/GetMatchmakingStats/...`, `matchmaker.GetRankedSeasonInfo/GetPlacementMatchesRequired/
  GetRankedDecayInfo`, `stats.*`. The W2R client can already ask for most of this; SC's SDK build does not.

## Players Online / games (gateway stats)
RPC `classic.protocol.v1.gateway.Gateway/GetGatewayStats` (also pushed `GatewayUpdate`).
- `GetGatewayStatsRequest`: field 1 uint64 (the gateway id; exe stores `gatewayId`).
- `GetGatewayStatsResponse`: fields 1, 2, 3 all optional **uint32**. Field meaning not yet pinned (expected:
  players online / games / third counter). SDK callback forwards (ctx, f1, f2, f3) to
  `IGatewayEventsV1` slot 3; exe handler `battlenet::GatewayStatsRequestHandler` -> `gateways::StatsSuccessNotification`.
- `GatewayStats` (pushed in GatewayUpdate): field 1 uint64, field 2 uint64.
- `GatewayInfo`: 1 uint64, 2 string, 3 string, 4 message, 5 bool, 6 repeated string, 7 string.
- `authentication.OnlineStats`: 1 uint32, 2 uint32, 3 uint64 (arrives with session info).
- UI: `SetGatewayInfo(gatewayTitle, gatewayDescription, gatewayNumPlayers)` (gateway picker).
- The "There are N games being played / N players online" chat lines are server text.

## Career / Season / History / Summary
Two mechanisms are present:
1. `toon_profile.GetStats` (legacy per-toon counters, used by `/stats` and profile popup):
   `GetStatsRequest` {1 u32, 2 u64, 3 string, 4 string, 5 u32}; `GetStatsResponse` {1 repeated Stat, 2 u32};
   `Stat` {1 string name, 2 u64 value, 3 u32 type}. Stat keys in exe: `wins losses draws disconnects`,
   `mm_wins mm_losses mm_disconnects` (ranked), `legacy_*`, `games_played`, `play_time`, `score_overall`,
   `units_*`, `structures_*`, `resources_*` (+ `_sum` suffix). Strings `bnet_profileStats_*` format them
   (all seasons / custom / season / overall).
2. Web pages: exe `UrlResolveProfileInfo`, `UrlResolveToonMatchMakingToast`, keys `Url.Profile`,
   `Url.Profile-Gateway`, `Url.Profile-MatchMaking`, `Url.Profile-Registry`, `Url.Leaderboard-Personal`
   (attribute `ToonName`, `MatchGuid`). `url.ResolveUrl` {1 string key, 2 repeated Attribute{string,string}}
   returns the URL; `webapi.Get/Post` {1 string, 2 repeated, 3 repeated, 4 bytes} -> {1 repeated header, 2 bytes}
   is an HTTP proxy through the RPC channel. MMR/season/history/summary are very likely served this way
   (CEF `WebBrowserView`, `glues::UserProfileScreen`, `LeaderboardScreen`). **Not yet confirmed**: needs the
   resolved URL from a running client.

## Open (superseded by the section below)
- Pin gateway response field meanings (live capture with known on-screen numbers).
- Capture resolved Profile/Profile-MatchMaking/Leaderboard URLs and response format from a running client.
- Same pass on W2R's decrypted ClientSdk (not dumped yet; the plugin host can dump from inside).

## RESOLVED with a live capture (2026-10-12/13): it is a local HTTP proxy to Battle.net's web API
The Players Online count, Career/Season/History/Summary and leaderboards are **not** binary RPC results the exe
decodes. The UI is CEF web panels (`SceneCefBrowser.exe`) served by the game itself:
`http://127.0.0.1:<port>/gamedata/webui/dist/<Panel>/<Panel>.html?locale=enUS&port=<port>&guid=<n>`
(panels seen: ProfilePanel, GatewayPanel, GatewayToonInfoPanel, LeaderboardPanel, MatchmakingSearchPanel,
ChatPanel, ParticipantsPanel, InfoPanel, ...). The port is an ephemeral listener of `StarCraft.exe`.
The panels call `fetch("/web-api/...")` against that origin. **ClientSdk.dll** (not the exe) owns a local route
`web-api/(.*)` (string in both the SC and W2R SDK) and forwards it to the Aurora `webapi.WebApi.Get/Post` RPC
(`WebApiRequest{1 string path, 2/3 repeated Attribute, 4 bytes body}` -> `WebApiResponse{1 repeated headers, 2 bytes body}`),
using the logged-in session. So any code in the process (or a local client) can read the data with a plain
`GET http://127.0.0.1:<port>/web-api/...` while the game is logged in. JSON responses:

| Path | Returns |
|---|---|
| `GET /web-api/v1/gateway` | `{"<gatewayId>":{"is_official":true,"name":"U.S. West","online_users":450,"region":"usw"},...}` ids 10 usw, 11 use, 20 eu, 30 kr, 45 asia. **This is "Players Online"** (matches the on-screen count within normal drift). |
| `GET /web-api/v1/leaderboard` | `gamemodes`, `gateways`, `leaderboards{id:{gateway_id,gamemode_id,season_id,season_name,name,last/next_update_time}}`, `matchmaked_current_season`, `team_leaderboard_info` |
| `GET /web-api/v1/leaderboard/{lbId}?offset=&length=` | `{columns:[rank,last_rank,gateway_id,points,wins,losses,disconnects,toon,battletag,avatar,feature_stat,rating,bucket], rows:[[...]]}` |
| `GET /web-api/v1/leaderboard-rank-by-toon/{lbId}/{toon}/{gatewayId}` | rank row + `total_rows`, `mingames` |
| `GET /web-api/v1/leaderboard-name-search/{lbId}/{text}`, `-by-friends/{gw}`, `-by-benefactor/{gw}` | search / friends / IGR boards |
| `GET /web-api/v2/aurora-profile-by-toon/{toon}/{gatewayId}?request_flags=scr_profile` | big profile: `aurora_id, battle_tag, country_code, toons[], stats[] (per gateway `raw` counters `<race>_<stat>_{sum,min,max}` plus legacy_*), matchmaked_stats[] (season, rating, points, wins, losses, disconnects, win/loss streaks, highest_*), game_results[] (recent games with players, race, result, per-player stats), avatars*, replays[]` -> Career/Season/Summary tabs |
| `GET /web-api/v1/matchmaker-gameinfo-by-toon/{toon}/{gw}/{gamemodeId}/{seasonId}` | match history (History tab): `[{ "MM-<guid>": {match_created, players:[{<toonguid>:{game_info, game_result, ...}}]}}]` |
| `GET /web-api/v1/map-stats-by-toon/{toon}/{gw}` | per-map, per-race games/wins (toon and global) |
| `GET /web-api/v1/file-set/classic.files.global.maps-1v1` | ranked map pool |
| `POST /web-api/v1/toon-profile` | set avatar/title/description (do not call casually) |

UI string/IDs: `scr.common.stats.mmr.label`, `...winRate...`, `...mostPlayedRace...`; Summary uses `<race>_` counters
(`overall|terran|zerg|protoss`), `totalTimePlayed`, `mostPlayedRace`.
Panel code (Blizzard's, not stored in the repo) can be re-fetched while SC runs from the same
`/gamedata/webui/dist/<Panel>/<Panel>.js` paths.

## Consequences for Warcraft II: Remastered
- W2R's `ClientSdk.dll` has the same `web-api/(.*)` local route, the `webapi` and `url` services and `ILocalWebsocketServerV1`
  (`Warcraft II.exe` references it), plus gateway stats and RPC-style `leaderboard.GetRanks`,
  `stats.GetAuroraProfileByToon`, `toon_profile.GetGamemodeStats/GetMatchHistory`, `matchmaker.GetRankedSeasonInfo`.
- Next test (needs W2R running and logged in): find the listening port of `Warcraft II.exe`
  (`netstat -ano | findstr <pid>`), try `GET /web-api/v1/gateway`, `/web-api/v1/leaderboard`; paths/program ids
  may differ for W2R. If the route works, a plugin or the manager can read stats with plain HTTP.
- Not verified: whether Blizzard's backend serves W2R data on these routes, auth limits, rate limits.

## Warcraft II findings (2026-10-13, game running, logged in)
- `Warcraft II.exe` opens **no listening TCP socket**: the SDK's local `web-api/(.*)` server is not started by the W2R exe
  (the exe only has empty base-class constructors for `ILocalWebsocketServerV1`). The StarCraft shortcut therefore does not work as-is.
- Single realm: default `Client.Aurora.DnsName` is `us.actual.battle.net` (exe also parses `-region`, `-csdk-aurora-dnsname`, `mp_region`, and
  contains dev79/dev80 fakeaurora test URLs). Decision: do not try other servers; all W2R data/players live on "us".
- Decrypted SDK dump (`W2_Sdk_dump.dll`) is in the SC1 Ghidra project for the next step: how the local server is started, and whether
  gateway stats / leaderboard / profile RPCs can be called from a plugin.

### Warcraft II SDK static findings (decrypted dump, `W2_Sdk_dump.dll`)
- Dumping a **32-bit** module needs `tools/research/derelocate.py <dump> <loaded_base_hex>` afterwards (the dump holds
  load-time relocated absolute addresses; without it string/vtable xrefs miss). Import with RTTI-only analysis
  (all other analyzers off, ~1 min; the full or "light" analysis of the SDK takes hours or never ends).
- The `web-api/(.*)` route is registered at `0x102e36aa` (VA, preferred base 0x10000000) inside a large, junk-instruction-obfuscated
  function (`~0x102e1ca0`): it builds the string and calls a router `AddRoute(this+0x50, pattern, handler)`. Neighbouring strings
  (`Not Authorized`, `Rate Limited`, `Not Supported`, `Not Found`, `Game returned error`, `Cache-Control: private, max-age=86400`)
  are the HTTP status/replies of that handler; "Game returned error" means some paths are answered by a game callback
  (`ILocalWebsocketHttpRequestV1`) instead of being proxied.
- Not yet determined: what creates and starts the SDK's local server object in W2R (it is not running; no listener), and whether
  a plugin in the game process can create it or call the underlying RPC interfaces (`IGatewayV1`, leaderboard, stats) directly.
- Not yet determined: whether Blizzard's backend returns data for the W2R program id on these routes/RPCs.

### Warcraft II: where the local server object lives (live read-only check, build 2818 exe, 2026-10-13)
- `Warcraft II.exe` has a function-local static singleton of SDK service wrappers (accessor `FUN_00594860`, object at `0x964d78`
  in the exe image, ImageBase 0x400000). Wrappers are `{vftable, sdkObject*}`; `GatewayService` at `+0xb8`,
  `LocalWebsocketServerService` at `+0xf8` (so the SDK `ILocalWebsocketServerV1*` is at image `0x964e74`).
  `tools/research/w2peek.py` reads it from outside: the object exists, vtable = SDK `LocalWebsocketServerV1` (RVA 0xab14b8 in the SDK), idle.
- SDK `LocalWebsocketServerV1` vtable (18 slots, slot 0 dtor); exe wrapper signatures: slot 4 `bool()`, slot 5 `void(u16)`,
  slot 6 `void()`, 7 `(a,b,c)`, 8 `(a,b)`, 9 and 10 `(5 args)`, 11 `(2)`, 12 `(3)`, 13 `(2)`, 14 and 15 `(5)`, 16 `(2)`, **17 `void()`**.
  Slot 14 (`0x10304a60`) is the HTTP reply path ("Game returned error", Cache-Control, Content-Length).
  **Slot 17 (`0x102e1ca0`, no arguments) registers the `web-api/(.*)` route.** In a running W2 the route string exists only as the
  literal in the DLL, so the registration has not run: the W2 exe never calls slot 17 (or whatever starts the server).
- Candidate experiment (not done): from a plugin, call slot 17 on the object (fastcall/thiscall `this` in ECX, no args) and see
  whether a listener appears for `Warcraft II.exe` (`netstat -ano`) and `GET /web-api/v1/gateway` answers. Unknown: whether more setup
  (port, event handler `ILocalWebsocketServerEventsV1`) is needed; risk is a crash of the game, not the account.

## RESULT: the local web server can be started in Warcraft II, and its backend has no W2R data (2026-10-13, build 2818)
Experiment with the throwaway plugin `w2r-sdk-probe` (in the w2r-plugins checkout, untracked): on a verified SDK object it called
vtable slots of `LocalWebsocketServerV1` once each. Findings:
- **Slot 5 with port 0 starts the server.** Two loopback listeners appear shortly after the call returns (asynchronously):
  one plain HTTP and one HTTPS/TLS, on ephemeral ports (57225 and 57226 in the test run). Slot 4 `bool()` returned 0 (not running).
- **Slot 17 registers the `web-api/(.*)` route** (returned 1). Before it, every path is `404 Not Found`; after it, `/web-api/...` is served.
  Order that worked: slot 5(0), then slot 17. (Slot 6 was not tried.) Both calls are from the main thread; the game kept running.
- The server is the same one StarCraft uses: `GET http://127.0.0.1:<port>/web-api/v1/gateway` returns the JSON for the 5 gateways.
- **Backend data for Warcraft II is empty.** `program_id` is `W2R` (the backend knows the program and uses the SC schema), but
  `online_users` is 0 on every gateway, `/v1/leaderboard` has no gamemodes, leaderboards or season (`matchmaked_current_season` 0),
  `aurora-profile-by-toon/<toon>/<gw>` returns a W2R-shaped but empty profile for a real toon, `leaderboard-rank-by-toon` and
  `map-stats-by-toon` are empty. `/v1/leaderboard/<id>?offset&length` returned a raw MySQL syntax error text and
  `/v1/file-set/classic.files.global.maps-1v1` a 500 (nothing configured for W2R). I did not probe further.
- Conclusion: Blizzard has the plumbing for W2R but no ranked ladder, leaderboards, player counts or profile stats are populated.
  Career/season-style stats for W2R must come from our own service (docs/stats-service.md), fed by the match reports the host already
  signs. The in-game "region us" and the single `us.actual.battle.net` realm match this.

### Is the endpoint list complete? (2026-10-13)
The community packages `bw-web-api` 3.0.2 (README: "typed API wrapper and documentation for the otherwise undocumented Brood War
Remastered API") and `gravatic-booster` 3.1.0 document the same local server and list 13 routes: the ones in the table above plus
`GET /v1/matchmaker-gameinfo-playerinfo/{matchId}` (ranked match info and replay link), the v2 profile `request_flags` values
`scr_profile`, `scr_tooninfo`, `scr_mmtooninfo`, `scr_mmgameloading`, and the old v1 `aurora-profile-by-toon` ("deprecated, useless").
No other routes are known; the SDK has no route list (it forwards any `/web-api/...` path to Aurora), so anything else would have to
be guessed against Blizzard's server, which we are not doing. Against W2R every one that needs no match id was tried (all four profile
flags, the v1 profile, name search, rank, map stats, gateway, leaderboard): all empty or an error. Reference only; no code copied.

## How the Multiplayer lobby list is fed (2026-10-13)
Not the web-api. It is the Aurora `game_master` service, driven through `ClientSdk::IInternetGameMasterV1` (exe wrapper
`InternetGameMasterService`, 19 methods) with events on `IInternetGameMasterEventsV1`; `ILocalGameMasterV1` is the LAN side.
- Client asks with `StartFindingGamesRequest` {1 u64, 2 u32, 3 u32, 4 repeated `GameCriteria`{1 u32, 2 u32}, 5 and 6 varint, 7 bool}
  and stops with `StopFindingGamesRequest`. The server then **pushes** `GamesFoundRequest` {1 u64 search id, 2 repeated `GameInfo`}
  and `GamesRemovedRequest` {1 u64, 2 repeated u64 game ids}. Nothing is polled.
- `GameInfo` {1 u64 game id, 2 string, 3 message, 4/5/6 u32, 7 bool, 8 repeated message (attributes), 9 u64, 10 repeated string,
  11 8 bytes, 12 bool, 13 u64, 14/15 u32}. In the exe the events arrive per game: `ClientGameMaster` events vtable slot 4 (found) builds or
  updates an entry in the list at object+0x14c (`FUN_00630b80` fills it), slot 5 removes it. The exe reads the id (+4), the name (+0x10)
  and the attribute key/value array (+0x34 count, +0x38 items).
- Everything shown comes from **attributes the hosting client set itself** (`FUN_00630130`, advertise): `players_num`, `players_max`,
  `game_secret`, `game_create_time` (`%FT%T`), `game_id`, `lobby_ready` (written as "false"), `game_host_battletag`, `is_private`,
  `game_speed`; plus the 6112 (0x17e0) game type. Joining uses `InviteToGame`/`GameInviteStatus` pairs (`Received invitation from ...`).
  Attribute updates (`FUN_0061f980`) only change `players_num` and `players_max`.
- `AdvertiseGame` / `StopAdvertisingGame` bracket a lobby's life. `StartGameRequest` and `EndGameRequest` (with `GamePlayerResult`)
  exist in the protocol, but W2R's `StartGameInfoImpl` and `EndGameInfoImpl` return 0 for every getter: **the game reports no
  start data and no results to Battle.net.** That alone explains the empty ladder.
- A game that has started is not marked anywhere we found (`lobby_ready` is never set true). Whether it just disappears
  (host stops advertising, server sends GamesRemoved) is unconfirmed. Needs a live look at the list while another game starts.
- The SDK side (`InternetGameMasterV1Impl`, vtable 0x10ab00fc, 20 slots) is control-flow flattened and does not decompile.

### Live capture of the lobby traffic (2026-10-13, w2r-lobby-log 0.1.0)
Captured while sitting in the lobby list, hosting a game, starting it and finishing it (`plugins/w2r-lobby-log`, patches the exe's
`InternetGameMasterService` and `ClientGameMaster` vtables; log in the plugin's data folder).
- **GameFound attributes are Storm-style names, not the ones in the advertise code:** `szCreatorName`, `szMapTitle`, `nCurPlayers`,
  `nMaxPlayers`, `bInitialGameSpeed`, `lFlags`, `bStartPositions`, `password`, `gameReady`, plus the game name. Examples: name
  "GARDEN", creator deAdeNdz, map "Garden of war BNE.pud". The `password` is sent in clear to every browsing client.
  (0.1.0 hid one-character values as `?`; fixed in 0.1.1.)
- The list is a small trickle: one `GameFound` at ~16 s, then the same game re-sent about every 2 s as a heartbeat/update.
- `GameRemoved` arrives in bursts every ~5 s with 4-7 ids at a time, nearly all ids this client never saw. The ids are one shared,
  increasing sequence (about 1.4 per second here), so game ids are probably shared with other Blizzard classic games and the server
  tells every client about every removal. Not yet confirmed.
- **Hosting:** the game calls a no-argument method (wrapper slot 18) and the service answers `AdvertiseSuccess` with the new game id.
- **Starting:** the game calls wrapper slot 15 with `(gameId u64, StartGameInfoImpl*)`, the service answers (`AdvertiseSuccess` again, an
  event in slot 10), and about 4 s later pushes `GameRemoved` for the host's own game. **A started game leaves the list**; there is no
  in-progress list for clients to read. An `AdvertiseFailure?` event (args 3,0,0xf,5) followed a second later.
- **Ending:** after the match the game calls wrapper slot 16 with `(gameId u64, EndGameInfoImpl*)` and gets an event in slot 12 back.
  The info object is the empty stub (all zero fields), confirmed live. So the game does report start and end to Battle.net, with
  no players, no result and no stats.

### Second capture (15 minutes in and out of lobbies, with other players, 0.1.1)
- Lobby entries now decode fully. Example: `GameFound id=0xa7bafdc name="dsfsdfds" szCreatorName=BunterGnoll szMapTitle="Rose Petal BNE.pud"
  nCurPlayers=4 nMaxPlayers=6 bInitialGameSpeed=8 lFlags=0 bStartPositions=1 password= gameReady=0`. `password` is empty for open games.
  `lFlags` seen: 0, 12288, 12800, 28672. `gameReady` stayed 0 for every game seen.
- **Player counts are live:** each join or leave is a new `GameFound` for the same id with the new `nCurPlayers`. A host that closes
  slots lowers `nMaxPlayers` (6, 5, 4) until it equals `nCurPlayers`; that full game is then removed 6-20 s later, which is what
  starting looks like from the outside. 5 lobbies seen this way, all removed after being full.
- `GameRemoved` came 854 times in ~15 minutes (about 1 per second) for ids that were never listed to us: only 5 of the 854 were games we had
  seen. So the removal stream covers many more games than the W2R lobby list shows, most likely other Blizzard classic titles sharing the
  id sequence. The first argument of `GameFound`/`GameRemoved` (1, then 4, then 6) rose after leaving and re-entering the list, so it is
  probably the find-subscription id.
- Leaving the lobby: the game calls wrapper slot 14 with `(gameId u64)` (stop advertising), the service answers with an
  `AdvertiseFailure?` (args 3,0,0xf,5) and an event in slot 8. Creating one: the no-argument slot 18.
- The host's own player-count updates produced `AdvertiseSuccess` events but **no call through the wrapper we patch**, so updates go
  out through another path (the advertise code also calls a method on the service at container+0x100 with the constant 0x17e0).
  Not yet logged.

### Third capture: who receives the feed (2026-10-13)
Logging from game start. About 5.5 minutes on the main menu produced no events at all (the log holds only its start marker). The
`GameRemoved` stream began, 341 s in, only once the Multiplayer lobby screen was opened. So the lobby feed is a subscription the
client opens by entering Multiplayer, not a broadcast to every connected client. The call that opens it did not pass through the
patched `InternetGameMasterService` slots either (no outgoing line), so it uses another path, probably the service at container+0x100.

### Fourth capture (0.2.0: every service wrapper patched, 192 slots)
Service wrappers in the container singleton, by offset: +0x50, +0x70, +0x94, +0xa0 (unnamed), +0xb8 Gateway, +0xc0 InternetGameMaster,
+0xe0 Lobby, +0xf0 LocalGameMaster, +0xf8 LocalWebsocketServer, +0x100 Network (24 slots), +0x11c and +0x124 (unnamed); +0x118 holds no vtable.
- At start-up the game makes about eight service calls within the first 0.3 s, including `svca0.slot7` with the toon name ("Beau")
  and `Gateway.slot6(2, ...)`. Nothing else goes out until the player does something.
- Opening the Multiplayer lobby screen produced **no outgoing call through any of the 192 patched exe-side slots**, yet the lobby feed
  (first `GameFound` at ~42 s, removals from ~45 s) started right then. So the subscription is opened by something that does not use
  these wrapper tables: a direct call into the SDK object, or the SDK itself. 0.2.1 therefore also patches the SDK's
  `InternetGameMasterV1Impl` vtable (0x10ab00fc, slots 1-19) in ClientSdk.dll.

### Fifth capture (0.2.4): the subscribe call
ClientSdk.dll's own vtable pages cannot be written (`VirtualProtect` fails with 87 on the execute-read page), so 0.2.4 swaps the vtable
pointer of the live `InternetGameMasterV1Impl` heap object (wrapper +0xc0, object `+4`) for a copy of the vtable holding logging stubs.
That works (19 slots).
- Opening the Multiplayer lobby list called **SDK game master slot 8** at t=61.2 s: `(1, 0, callback)`, where `1,0` is a u64 and the
  callback is an exe-side functor object (vtable 0xb17268). The first removal batch followed 2.6 s later. The `1` matches the first
  argument of every `GameFound`/`GameRemoved` that followed, so slot 8 is the find-games request and `1` the subscription id.
- It was **not** seen on the exe's `InternetGameMasterService` wrapper (static slot 8 stayed silent), so the game calls the SDK
  object directly for this one.
- A second call, **slot 9** `(2, 0, ...)`, came at t=127 s. Its role is not yet known (a second find request, or a stop).
- The removal stream stopped at t=103.7 s, before slot 9, so leaving the screen may end the feed after all. Earlier captures showed the
  feed continuing; to be checked against what the player did when.

### Sixth capture: slot 8 and slot 9 are a pair (0.2.4)
Sequence over one session: slot 8 `(1)` at 61 s (opened Multiplayer), slot 9 `(2)` at 127 s (after leaving at ~104 s), slot 8 `(3)` at 373.6 s
(opened it again), slot 9 `(4)` at 417.5 s (left again). The first argument is a running request counter. Every `GameFound` carries the
counter of the slot-8 call it answers (1 for the 185 s update, 3 for the 373.8 s one), and the full current list arrives about 0.17 s after
slot 8. So slot 8 is the find-games subscribe and slot 9 very likely its stop, sent some seconds after leaving the screen.
Caveats: a `GameFound` with counter 1 still arrived at 185 s, after the first slot 9 (counter 2), and in the minute at the main menu after the
second stop no event of any kind arrived, not even the usual removal bursts (which also did not appear in the second visit).
So when the stream ends is not settled; the stop may be delayed or may only end part of the feed.

### Seventh capture: hosting "TESTING" with a password, then starting (0.2.4)
SDK game master object (`InternetGameMasterV1Impl`), calls seen directly on it. Each carries a running request token (5, 6, 7...):
- slot 8 `(token)`: find games, sent on opening the lobby list; the current list came back 150 ms later with the same token.
- slot 9 `(token)`: sent 1.5 s later, when the player left the list for the Create Game screen (earlier visits: 23-44 s later).
- slot 6 `(token, GameInfoImpl*)`: **AdvertiseGame**. The exe object holds the lobby name inline ("TESTING"). The game calls it again, with the
  same token, for every change (an update is a re-advertise), which is why the wrapper's own slot 6 never fired. Answer: `AdvertiseSuccess`
  with the new game id.
- slot 18 (reached from wrapper slot 18, the no-argument create call): setup before the first advertise.
- slot 15 `(gameId, StartGameInfoImpl*)` = StartGame (wrapper and SDK both seen), answer: an event in slot 10.
- slot 7 `(token)` = stop advertising, sent **5 s after StartGame**; the service answers with the `AdvertiseFailure?` event (args token,0,0xf,5),
  which is just the stop's completion. This is what removes a started game from everyone's list.
- slot 16 `(gameId, EndGameInfoImpl*)` = EndGame, 90 s later at the end of the match, empty info; answer: an event in slot 12.
The host's own game came back as `GameFound` (token of the earlier find) with `password=testx` in clear and `nCurPlayers=2` while alone.
After the stop in slot 9, only echoes of the player's own game arrived: no removal bursts, no other player's lobby. During the 90-second match
nothing at all arrived. (The first capture showed lobbies arriving during a match; there the stop may not have been sent. Not resolved.)
