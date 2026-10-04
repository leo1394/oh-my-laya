ALTER TABLE cases ADD COLUMN task_lineage TEXT;

CREATE INDEX cases_task_scope_idx ON cases(task_family,language,task_lineage);
UPDATE settings SET schema_version=4;
