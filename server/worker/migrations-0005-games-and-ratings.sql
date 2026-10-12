-- Corroboration, ratings and the leaderboard: columns on reports and the derived tables.
-- Run once against a database made before this; a fresh schema.sql already has all of it.
ALTER TABLE reports ADD COLUMN me_team INTEGER;
ALTER TABLE reports ADD COLUMN game_id INTEGER;
ALTER TABLE reports ADD COLUMN ignored INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS reports_game ON reports (game_id);

-- The reporter's team, read from the stored envelope (reports made before this column existed).
UPDATE reports SET me_team = (
  SELECT json_extract(j.value, '$.team')
  FROM json_each(json_extract(reports.envelope, '$.body.players')) AS j
  WHERE json_extract(j.value, '$.slot') = reports.me
) WHERE me_team IS NULL;

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
  status    TEXT    NOT NULL DEFAULT 'unverified',  -- unverified | verified | disputed
  reason    TEXT,
  winner_team INTEGER,
  turns     INTEGER,                    -- median of the reports that back the winner
  reports   INTEGER NOT NULL DEFAULT 0, -- result reports that counted
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

-- Ask the scheduled job to build games and ratings from the reports already stored.
INSERT OR REPLACE INTO meta (k, v) VALUES ('rebuild', '1');
