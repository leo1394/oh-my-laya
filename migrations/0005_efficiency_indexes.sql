CREATE INDEX feedback_attempt_sequence_idx ON feedback_events(decision_id,attempt_ref,receive_sequence);
CREATE INDEX feedback_usage_stream_idx ON feedback_events(json_extract(payload_json,'$.usage_stream_id'),decision_id,attempt_ref) WHERE kind='usage';
CREATE INDEX feedback_execution_scope_idx ON feedback_events(json_extract(payload_json,'$.execution.run_id'),json_extract(payload_json,'$.execution.stage_id'),json_extract(payload_json,'$.execution.ordinal'),decision_id,attempt_ref) WHERE kind='assignment' AND json_extract(payload_json,'$.execution.contract')='dispatch_receipt_v1';
UPDATE settings SET schema_version=5;
