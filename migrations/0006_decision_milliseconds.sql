ALTER TABLE decisions ADD COLUMN created_at_ms INTEGER;
CREATE INDEX decisions_activity_recent_idx ON decisions(COALESCE(created_at_ms,created_at*1000) DESC,id DESC) WHERE deleted_at IS NULL;
UPDATE settings SET schema_version=6;
