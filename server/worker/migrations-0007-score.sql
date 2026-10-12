-- Kills and the rest of the score tables: reports gain `combat`, games gain `score`, and every game
-- is judged again (a game with no fighting is no longer rated, and a leave needs proof of a fight).
-- Run once; a fresh schema.sql already has the columns.
ALTER TABLE reports ADD COLUMN combat INTEGER;
ALTER TABLE games ADD COLUMN score TEXT;
-- Reports sent after clients began sending scores carry them in the envelope.
UPDATE reports SET combat = (
  SELECT CASE WHEN SUM(COALESCE(json_extract(j.value, '$.kills'), 0) + COALESCE(json_extract(j.value, '$.razed'), 0)) > 0 THEN 1 ELSE 0 END
  FROM json_each(json_extract(reports.envelope, '$.body.result.score')) AS j
) WHERE phase = 'result' AND json_extract(envelope, '$.body.result.score') IS NOT NULL;
INSERT OR REPLACE INTO meta (k, v) VALUES ('rebuild', '1');
INSERT OR REPLACE INTO meta (k, v) VALUES ('last_rebuild', '0');
