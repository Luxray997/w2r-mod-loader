-- Moves replay files out of D1 into the R2 bucket `w2r-replays`. The table is recreated without
-- the `data` column; the rows it held are not copied, so clients must send those replays again
-- (delete config\replay-upload-state.json on the uploading machine, then `w2rpm share upload`).
-- Run once against a database that has the migration-0003 table; a fresh schema.sql needs nothing.
DROP TABLE IF EXISTS replays;
CREATE TABLE replays (
  sha       TEXT    PRIMARY KEY,
  key       TEXT    NOT NULL,
  received  INTEGER NOT NULL,
  size      INTEGER NOT NULL,
  started   INTEGER NOT NULL,
  map       TEXT    NOT NULL,
  setup     TEXT    NOT NULL,
  build     TEXT,
  client    TEXT
);
CREATE INDEX replays_setup ON replays (setup, started);
CREATE INDEX replays_key ON replays (key, received);
