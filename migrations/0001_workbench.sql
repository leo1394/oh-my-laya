PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS settings (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    value_json TEXT NOT NULL,
    active_memory_version TEXT,
    schema_version INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS decisions (
    id TEXT PRIMARY KEY,
    request_id TEXT NOT NULL UNIQUE,
    request_json TEXT NOT NULL,
    result_json TEXT,
    error_json TEXT,
    context_json TEXT,
    recording_status TEXT NOT NULL,
    protected INTEGER NOT NULL DEFAULT 0,
    deleted_at INTEGER,
    created_at INTEGER NOT NULL,
    finished_at INTEGER
);

CREATE TABLE IF NOT EXISTS decision_snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    decision_id TEXT NOT NULL REFERENCES decisions(id) ON DELETE CASCADE,
    request_id TEXT NOT NULL,
    attempt_ref TEXT,
    kind TEXT NOT NULL CHECK (kind IN ('request','output','context','error')),
    payload_json TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    capture_status TEXT NOT NULL,
    redaction_version TEXT NOT NULL,
    uncertainty_reasons_json TEXT NOT NULL DEFAULT '[]',
    protected INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS model_observations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL REFERENCES feedback_events(event_id) ON DELETE CASCADE,
    decision_id TEXT NOT NULL REFERENCES decisions(id) ON DELETE CASCADE,
    attempt_ref TEXT NOT NULL,
    stage TEXT NOT NULL CHECK (stage IN ('recommended','selected','requested','effective')),
    observation_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS execution_attempts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE REFERENCES feedback_events(event_id) ON DELETE CASCADE,
    attempt_ref TEXT NOT NULL,
    decision_id TEXT NOT NULL REFERENCES decisions(id) ON DELETE CASCADE,
    role TEXT,
    recommended_json TEXT,
    selected_json TEXT,
    requested_json TEXT,
    effective_json TEXT,
    parent_attempt_ref TEXT,
    change_reason TEXT,
    mixed_configuration INTEGER NOT NULL DEFAULT 0,
    environment_json TEXT,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS feedback_events (
    event_id TEXT PRIMARY KEY,
    decision_id TEXT NOT NULL REFERENCES decisions(id) ON DELETE CASCADE,
    attempt_ref TEXT,
    kind TEXT NOT NULL,
    source_json TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    event_hash TEXT NOT NULL,
    rubric_version TEXT,
    phase TEXT,
    source_sequence INTEGER,
    supersedes_event_id TEXT REFERENCES feedback_events(event_id),
    receive_sequence INTEGER NOT NULL UNIQUE,
    received_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS feedback_scores (
    event_id TEXT NOT NULL REFERENCES feedback_events(event_id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    attempt_ref TEXT,
    role TEXT NOT NULL,
    rubric_version TEXT NOT NULL,
    dimension TEXT NOT NULL,
    value INTEGER,
    reason TEXT NOT NULL,
    evidence_refs_json TEXT NOT NULL,
    phase TEXT NOT NULL,
    source_sequence INTEGER NOT NULL,
    observed_at TEXT NOT NULL,
    supersedes_event_id TEXT REFERENCES feedback_events(event_id),
    PRIMARY KEY (event_id, ordinal),
    CHECK (value IS NULL OR value IN (0,1,2))
);

CREATE TABLE IF NOT EXISTS reviews (
    decision_id TEXT NOT NULL REFERENCES decisions(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL,
    status TEXT NOT NULL,
    labels_json TEXT NOT NULL,
    reason TEXT NOT NULL,
    actor_json TEXT,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (decision_id, revision)
);

CREATE TABLE IF NOT EXISTS cases (
    id TEXT PRIMARY KEY,
    decision_id TEXT NOT NULL REFERENCES decisions(id) ON DELETE CASCADE,
    review_revision INTEGER NOT NULL,
    content_json TEXT NOT NULL,
    labels_json TEXT NOT NULL,
    task_family TEXT,
    language TEXT,
    applicability TEXT NOT NULL DEFAULT 'task-fact',
    applicability_reason TEXT,
    content_hash TEXT NOT NULL,
    active INTEGER NOT NULL DEFAULT 1,
    deleted_at INTEGER,
    created_at INTEGER NOT NULL,
    FOREIGN KEY (decision_id, review_revision) REFERENCES reviews(decision_id, revision)
);

CREATE VIRTUAL TABLE IF NOT EXISTS cases_fts USING fts5(case_id UNINDEXED, content);

CREATE TABLE IF NOT EXISTS memory_versions (
    id TEXT PRIMARY KEY,
    parent_id TEXT REFERENCES memory_versions(id),
    status TEXT NOT NULL,
    configuration_json TEXT NOT NULL,
    evaluation_json TEXT,
    evaluation_status TEXT NOT NULL DEFAULT 'pending',
    invalidated INTEGER NOT NULL DEFAULT 0,
    invalidation_reason TEXT,
    created_at INTEGER NOT NULL,
    activated_at INTEGER
);

CREATE TABLE IF NOT EXISTS memory_version_cases (
    version_id TEXT NOT NULL REFERENCES memory_versions(id) ON DELETE CASCADE,
    case_id TEXT NOT NULL REFERENCES cases(id),
    case_hash TEXT NOT NULL,
    PRIMARY KEY (version_id, case_id)
);

CREATE TABLE IF NOT EXISTS jobs (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    progress_json TEXT,
    result_json TEXT,
    error_json TEXT,
    cancel_requested INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS backups (
    id TEXT PRIMARY KEY,
    relative_path TEXT NOT NULL UNIQUE,
    sha256 TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS decisions_created_idx ON decisions(created_at DESC);
CREATE INDEX IF NOT EXISTS decisions_finished_idx ON decisions(finished_at);
CREATE INDEX IF NOT EXISTS snapshots_decision_idx ON decision_snapshots(decision_id, id);
CREATE INDEX IF NOT EXISTS feedback_decision_kind_idx ON feedback_events(decision_id, kind, receive_sequence);
CREATE INDEX IF NOT EXISTS attempts_decision_ref_idx ON execution_attempts(decision_id, attempt_ref, id);
CREATE INDEX IF NOT EXISTS observations_decision_ref_idx ON model_observations(decision_id, attempt_ref, id);
CREATE INDEX IF NOT EXISTS scores_initial_idx ON feedback_scores(attempt_ref, role, dimension, phase, source_sequence);
CREATE INDEX IF NOT EXISTS reviews_status_idx ON reviews(status, created_at);
CREATE INDEX IF NOT EXISTS cases_decision_idx ON cases(decision_id, review_revision);
CREATE INDEX IF NOT EXISTS jobs_status_idx ON jobs(status, updated_at);

INSERT OR IGNORE INTO settings(singleton, value_json, active_memory_version, schema_version)
VALUES (1, '{"recording_enabled":false,"memory_enabled":true,"replay_enabled":true,"retention_days":30,"storage_soft_limit_bytes":524288000}', NULL, 1);
