ALTER TABLE cases ADD COLUMN validation_assignment_event_id TEXT REFERENCES execution_attempts(event_id);
ALTER TABLE cases ADD COLUMN validation_context_json TEXT;
ALTER TABLE cases ADD COLUMN verification_status TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE cases ADD COLUMN last_validated_at INTEGER;

UPDATE cases
SET verification_status='verified', last_validated_at=created_at
WHERE applicability='task-fact';

UPDATE cases
SET applicability_reason=COALESCE(applicability_reason,'validation_unknown:legacy_missing_validation_metadata')
WHERE applicability='configuration-dependent';

CREATE INDEX cases_validation_assignment_idx ON cases(validation_assignment_event_id);
UPDATE settings SET schema_version=3;
