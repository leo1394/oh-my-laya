CREATE TABLE artifacts (
    id TEXT PRIMARY KEY,
    relative_path TEXT NOT NULL UNIQUE,
    sha256 TEXT NOT NULL,
    size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0 AND size_bytes <= 1048576),
    decision_id TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

ALTER TABLE decision_snapshots ADD COLUMN artifact_id TEXT REFERENCES artifacts(id);
CREATE INDEX decision_snapshot_artifact ON decision_snapshots(artifact_id);
UPDATE settings SET schema_version=2;
ALTER TABLE backups ADD COLUMN complete INTEGER NOT NULL DEFAULT 1;
ALTER TABLE backups ADD COLUMN issues_json TEXT NOT NULL DEFAULT '[]';
