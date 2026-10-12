-- Adds the replays table. Run once against a database made before replay upload; a fresh
-- database made from schema.sql already has it.

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
