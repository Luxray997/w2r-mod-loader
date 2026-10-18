-- Diagnostics bundles sent from the manager (`PUT /v1/diagnostics`). The zip itself is in the R2
-- bucket `w2r-replays` under `diagnostics/<sha>.zip`; this table is the index and the rate limit.
-- A fresh schema.sql already has it.
CREATE TABLE IF NOT EXISTS diagnostics (
  sha      TEXT    PRIMARY KEY,
  code     TEXT    NOT NULL,
  key      TEXT    NOT NULL,
  received INTEGER NOT NULL,
  size     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS diagnostics_code ON diagnostics (code);
CREATE INDEX IF NOT EXISTS diagnostics_received ON diagnostics (received);
