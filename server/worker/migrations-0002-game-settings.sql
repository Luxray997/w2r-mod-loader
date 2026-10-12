-- Run once against a database created from the first schema.sql (before settings were sent).
-- Fresh databases already have these columns; do not run it on them.
ALTER TABLE reports ADD COLUMN map_sha TEXT;
ALTER TABLE reports ADD COLUMN speed INTEGER;
ALTER TABLE reports ADD COLUMN flags INTEGER;
ALTER TABLE reports ADD COLUMN resources TEXT;
ALTER TABLE reports ADD COLUMN tileset INTEGER;
ALTER TABLE reports ADD COLUMN peasants_only INTEGER;
ALTER TABLE reports ADD COLUMN fixed_start INTEGER;
CREATE INDEX IF NOT EXISTS reports_map_sha ON reports (map_sha);
