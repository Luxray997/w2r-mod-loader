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
  me_team   INTEGER,                    -- the reporter's team
  game_id   INTEGER,                    -- games.id once grouped
  ignored   INTEGER NOT NULL DEFAULT 0, -- 1 when the key reported under a tag it is not bound to
  envelope  TEXT    NOT NULL,           -- the signed envelope exactly as received
  UNIQUE (key, rid)
);
CREATE INDEX IF NOT EXISTS reports_setup ON reports (setup, started);
CREATE INDEX IF NOT EXISTS reports_tag ON reports (me_tag);
CREATE INDEX IF NOT EXISTS reports_game ON reports (game_id);

-- Replay metadata. The file itself is in the R2 bucket `w2r-replays` under the same `sha`.
-- A replay is accepted only when the same key has reported the same game (reports.setup /
-- started), so every row here has at least one report behind it.
CREATE TABLE IF NOT EXISTS replays (
  sha       TEXT    PRIMARY KEY,        -- SHA-256 of the file, 64 hex
  key       TEXT    NOT NULL,           -- uploader's ed25519 public key, 64 hex
  received  INTEGER NOT NULL,
  size      INTEGER NOT NULL,
  started   INTEGER NOT NULL,           -- unix seconds, from the replay's header
  map       TEXT    NOT NULL,
  setup     TEXT    NOT NULL,           -- roster hash, as in reports.setup
  build     TEXT,
  client    TEXT
);
CREATE INDEX IF NOT EXISTS replays_setup ON replays (setup, started);
CREATE INDEX IF NOT EXISTS replays_key ON replays (key, received);

-- Everything below is derived from `reports` and can be rebuilt (docs/stats-service.md).
-- A game: the reports that share a roster hash and start within a few minutes of each other.
CREATE TABLE IF NOT EXISTS games (
  id        INTEGER PRIMARY KEY,
  setup     TEXT    NOT NULL,           -- roster hash, as in reports.setup
  started   INTEGER NOT NULL,           -- earliest report, unix seconds
  map       TEXT    NOT NULL,
  map_sha   TEXT,
  resources TEXT,
  speed     INTEGER,
  flags     INTEGER,
  peasants_only INTEGER,
  fixed_start   INTEGER,
  roster    TEXT    NOT NULL,           -- JSON players: tag, name, kind, team, race
  status    TEXT    NOT NULL DEFAULT 'unverified',  -- verified | disputed | unverified (no result)
  reason    TEXT,
  winner_team INTEGER,
  turns     INTEGER,                    -- median of the reports that back the winner
  reports   INTEGER NOT NULL DEFAULT 0, -- result reports that counted
  corroborated INTEGER NOT NULL DEFAULT 0, -- 1 when both sides reported and agree
  rated     INTEGER NOT NULL DEFAULT 0, -- 1 once it has moved the ratings
  changes   TEXT,                       -- JSON: who gained or lost how much
  updated   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS games_setup ON games (setup, started);
CREATE INDEX IF NOT EXISTS games_status ON games (status, started);

CREATE TABLE IF NOT EXISTS ratings (
  tag       TEXT    PRIMARY KEY,        -- BattleTag
  mu        REAL    NOT NULL,
  sigma     REAL    NOT NULL,
  skill     REAL    NOT NULL,           -- mu - 3 * sigma, what the leaderboard sorts by
  games     INTEGER NOT NULL,
  wins      INTEGER NOT NULL,
  losses    INTEGER NOT NULL,
  last_game INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ratings_skill ON ratings (skill DESC);

-- A key speaks for the first BattleTag it reported under; reports under another tag are ignored.
CREATE TABLE IF NOT EXISTS bindings (
  key        TEXT PRIMARY KEY,
  tag        TEXT NOT NULL,
  first_seen INTEGER NOT NULL
);

-- Keys that can verify a game they played in on their own (maintainer-managed).
CREATE TABLE IF NOT EXISTS trusted_keys (
  key  TEXT PRIMARY KEY,
  note TEXT
);

CREATE TABLE IF NOT EXISTS meta (
  k TEXT PRIMARY KEY,
  v TEXT
);
