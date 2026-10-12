-- Lenient start: a single report can rate a game, and games both sides agree on ("corroborated")
-- earn their reporters weight in disputes. Run once; a fresh schema.sql already has the column.
ALTER TABLE games ADD COLUMN corroborated INTEGER NOT NULL DEFAULT 0;
-- Re-judge every stored game under the new rules (including those the first run left unverified).
INSERT OR REPLACE INTO meta (k, v) VALUES ('rebuild', '1');
INSERT OR REPLACE INTO meta (k, v) VALUES ('last_rebuild', '0');
