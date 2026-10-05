# Create Multiplayer Game screen (Phase 3, lobby policy slice 4b)

Status: implemented and verified in the game (slices 1 to 4, 2026-10-04); on by default. Port of wc2r-plus `features/ui/lobby_creation_screen.cpp` and what it
stands on. Same parity rule as [chat-plugin.md](chat-plugin.md): the C++ is the reference. A host
feature (it hooks a game screen and persists in `host.json`), like the lock buttons it replaces
([lobby-policy.md](lobby-policy.md), slice 4a).

## What it is

The game's `LobbyCreationContent` (`char(__cdecl*)()`, drawn every frame of the screen, inside the
game's own full-window Nuklear window) is replaced, when the setting is on, by our own layout in
that same window: a banner and title, a left panel with the **map column** (map name, a square map
preview, a "Change map" button) and, on the right, the **options column** (lobby name, optional
password, resources, units, tileset, starting location, game speed, and the Teams and Slots lock
buttons), the **details** of the chosen map (size, maximum players, description) and the footer
(Cancel, Create). "Change map" opens the **map browser** in place of the screen: a folder list with
an up button, a reload button, a map-size filter, a search field over an index of every map under
the maps folder, and a row per map; picking one publishes it into the game's selection. The preview
is drawn from the map file itself (a PUD parsed by us, painted as coloured rectangles into a game
texture), not from the game's own preview.

The lock buttons first appeared as an overlay on the stock screen (slice 4a); they now live in the
options column here, and the overlay is gone.

## What it stands on, and what is new here

| Piece | wc2r-plus | Size | Notes |
|---|---|---|---|
| Screen kit: rectangle placement, label, heading, button, text edit, filter field, locked box, wrapped label, panel frame, scrolling list, icons, skin and font push/pop, window rect, `Localized`, game `std::string`/vector readers | `ui/screen_kit.cpp` | 497 | the host has `Nk` and `GameWidgets` already (`options.rs`, `lobbycreate.rs`); this adds the rectangle-placed set and the skin and layout helpers. Reused by the custom scenario screen and any later screen |
| Map browser: folder navigation, size filter, search index, live entries from the game's map list, choice and publish | `ui/map_browser.cpp`, `ui/map_search_cache.cpp` | 786 | the pure parts (path handling, filtering, the index, choice) are testable without the game |
| Map panel: name, preview box, details | `ui/map_panel.cpp` | 62 | |
| Map preview: read the map through the game, parse the PUD, tile colours per era, upload to a texture, draw | `map_preview.cpp`, `map_preview_pud.cpp` | 798 | the PUD parser is shared with map download's validator (step 4), so it is written once as `pud.rs` |
| The screen itself | `ui/lobby_creation_screen.cpp` | 376 | |

Almost every game symbol is already in `target/symbols.rs` (skins, fonts, the lobby option lists,
the map list, `LobbyCreationSelected`, `PendingGameSetup`, `CopyMapListEntry`,
`RefreshMapListFromFolder`, `ValidateMapFile`, `LobbyCreateButton`, `PreviousGameState`,
`TransitionGameState`, `SliderU32`, ...). Not in it: whatever `map_preview.cpp` calls to make and
draw a texture (`NkFillRect`, `NkDrawImage`, `CreateGameTexture`, `LoadMapFile`, `GameFree`, the GL
procedures); the slice that needs them adds them and the layout checks the way earlier slices did.

## Modules (all in `w2r-core`, x86 only where they touch the game)

- `screenkit.rs`: the kit above, on top of `options::Nk`. Pure geometry (the frame rectangle, the
  footer button width, flooring) is separate and unit-tested.
- `mapbrowser.rs`: state and pure logic (`SizeFilter`, last path component, folder stack, the
  search index and its filter, `Choice`), then a game-bound part that reads the live map list and
  publishes. Unit tests for everything pure.
- `pud.rs`: PUD sections (`TYPE`, `DIM `, `ERA `, `MTXM`, units for start locations and mines),
  bounds-checked, no panics on hostile files; reused by the validator in step 4. Tests with
  hand-built PUDs and fuzz-style truncations.
- `mappreview.rs`: tile palette per era, the rectangle painter, texture upload and draw.
- `createscreen.rs`: the hook and the layout; replaces `lobbycreate.rs`'s overlay while on.

## Setting and fallback

`host.json` gets `enhanced_create_screen` (default **true** since slice 4; it was false while the
screen was being verified, and a file that says false keeps it off), shown as a checkbox on the plugin screen. The detour takes over only
when the setting is on, every required symbol resolved, the window rectangle is sane and no fault
has occurred; otherwise it calls the original (and the 4a buttons draw). A fault under the screen
draws the stock screen from then on for the session and logs once. This is the same shape as the
chat panel and the lesson of the lobby banner: a draw that goes wrong must not strand the user on a
screen they cannot leave. The map browser closes itself when the screen has not been drawn for
250 ms (wc2r-plus `kReentryGapMs`).

## Known review findings to fix while porting

- The browser indexes the maps folder recursively (`IndexDir`, depth-limited); cap the entries, the
  depth and the total bytes read, never follow reparse points, and treat every PUD as hostile
  (this is the same validator work as step 4).
- Map names, paths and descriptions come from files: bound them and strip control characters before
  they reach the game's label functions.
- The preview reads a whole map file through the game's loader; bound the size and free through the
  game's allocator, as wc2r-plus does.

## Slices and exit criteria

Each slice ends with an in-game check by the user, behind the setting.

1. **Kit and the options column.** `screenkit.rs`, `createscreen.rs` with the hook, the setting, the
   banner, frames, the options column, the footer and the locks. The map column shows the current
   map's name and details and keeps the game's own map selection (no browser yet), so the screen
   can already create a game. Exit: create a game on the replacement screen with each option, the
   locks flip and persist, Cancel returns, the setting off gives the stock screen.
2. **Map browser.** `mapbrowser.rs`: folders, up, reload, size filter, search, publish. Exit: pick a
   map from a subfolder and from search, create the game with it, the lobby shows that map.
3. **Preview and details.** `pud.rs`, `mappreview.rs`, the details block. Exit: the preview matches
   the chosen map for each era; a malformed PUD in the folder shows no preview and no crash.
4. **Default on.** Done: the default is true. The 4a overlay is removed: the lock buttons exist only
   on the replacement screen (and as checkboxes on the plugin screen), so with the replacement off
   the game draws its own screen untouched.

## Open questions

1. Is the custom scenario screen (`ui/custom_scenario_screen.cpp`, the single-player screen built on
   the same kit and browser) wanted too? It is cheap once slices 1 to 3 exist, and not part of lobby
   policy.
2. Translations: the screen uses the game's own localized keys (`mp_lobby_name`,
   `customscenario_*`) where they exist and English literals elsewhere ("Password (Optional)",
   "Teams", "Slots", "Change map"). Keep that.

## Reload and the search-index cache (2026-10-04)

The two things slice 2 left out are in, written and not yet checked in the game:

- **Reload** (the button left of the folder path in the browser) now makes the game rescan the whole
  maps folder into its map registry (`ScanDirectoryIntoMapRegistry`, through
  `mapdownload::rescan_maps`), then re-reads the current folder and drops the search index, so a map
  copied in while the game runs shows up. The next index after a reload is always walked afresh.
- **The search index is cached** in `<host root>\cache\map_search_index.txt` (`mapsearchcache.rs`,
  wc2r-plus's text format). The first search of a launch reads it instead of walking every folder
  through the game's list, but only if the maps root is the same and every folder and map file still
  has the size and modification time it had when it was saved; anything else, or any doubt about the
  file, rebuilds and re-saves it. Bounded (16 MiB, 4000 maps, 20,000 folders) and sanitised on
  load. `w2r.log` says `mapbrowser: search index read from the cache` or `built by walking the
  folders`, with the count and the time.
- To check: search once (it is walked and saved), close the game, search again after a restart (read
  from the cache, much faster with a big collection); copy a map in, press reload, search for it.
