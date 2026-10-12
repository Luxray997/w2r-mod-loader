-- D1 schema. `reports` is the append-only log: everything else is derived from it and can be
-- dropped and rebuilt (docs/stats-reporting.md). Nothing here stores an IP address.
CREATE TABLE IF NOT EXISTS reports (
  id        INTEGER PRIMARY KEY,
  key       TEXT    NOT NULL,           -- reporter's ed25519 public key, 64 hex
  rid       TEXT    NOT NULL,           -- the report's random id, 16 hex
  phase     TEXT    NOT NULL CHECK (phase IN ('start', 'result')),
  setup     TEXT    NOT NULL,           -- hash of map + roster, 64 hex
  started   INTEGER NOT NULL,           -- unix seconds on the reporter's clock
  received  INTEGER NOT NULL,           -- unix seconds on the server's clock
  map       TEXT    NOT NULL,
  me        INTEGER NOT NULL,           -- the reporter's slot
  me_tag    TEXT    NOT NULL,           -- the reporter's BattleTag (required)
  outcome   TEXT,                       -- win | loss | left | other, result reports only
  code      INTEGER,
  turns     INTEGER,
  sync      INTEGER,
  map_sha   TEXT,                       -- SHA-256 of the map file played, 64 hex
  speed     INTEGER,                    -- game speed setting, as stored
  flags     INTEGER,                    -- match flags word, as stored (undecoded bits kept)
  resources TEXT,                       -- default | random | low | medium | high
  tileset   INTEGER,                    -- index into the game's tileset list, 0 = the map's own
  peasants_only INTEGER,                -- 0 or 1
  fixed_start   INTEGER,                -- 0 or 1
  envelope  TEXT    NOT NULL,           -- the signed envelope exactly as received
  UNIQUE (key, rid)
);
CREATE INDEX IF NOT EXISTS reports_setup ON reports (setup, started);
CREATE INDEX IF NOT EXISTS reports_tag ON reports (me_tag);

-- Replay files, stored untouched. A replay is accepted only when the same key has reported the
-- same game (reports.setup / started), so every row here has at least one report behind it.
CREATE TABLE IF NOT EXISTS replays (
  sha       TEXT    PRIMARY KEY,        -- SHA-256 of the file, 64 hex
  key       TEXT    NOT NULL,           -- uploader's ed25519 public key, 64 hex
  received  INTEGER NOT NULL,
  size      INTEGER NOT NULL,
  started   INTEGER NOT NULL,           -- unix seconds, from the replay's header
  map       TEXT    NOT NULL,
  setup     TEXT    NOT NULL,           -- roster hash, as in reports.setup
  build     TEXT,
  client    TEXT,
  data      BLOB    NOT NULL
);
CREATE INDEX IF NOT EXISTS replays_setup ON replays (setup, started);
CREATE INDEX IF NOT EXISTS replays_key ON replays (key, received);
