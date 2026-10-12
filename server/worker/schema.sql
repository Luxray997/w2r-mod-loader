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
  envelope  TEXT    NOT NULL,           -- the signed envelope exactly as received
  UNIQUE (key, rid)
);
CREATE INDEX IF NOT EXISTS reports_setup ON reports (setup, started);
CREATE INDEX IF NOT EXISTS reports_tag ON reports (me_tag);
