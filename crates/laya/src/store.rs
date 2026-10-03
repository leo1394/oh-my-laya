use crate::protocol::{hash, now, redact};
use crate::evidence::{self, Evidence};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{collections::{HashSet, VecDeque}, fs, io::Read, path::{Path, PathBuf}, sync::mpsc, thread, time::Duration};
use tokio::sync::oneshot;

const MIGRATION_1: &str = include_str!("../../../migrations/0001_workbench.sql");
const MIGRATION_2: &str = include_str!("../../../migrations/0002_evidence.sql");
const MIGRATION_3: &str = include_str!("../../../migrations/0003_case_applicability.sql");
const MAX_FEEDBACK_BYTES: usize = 16 * 1024;
const SCHEMA_VERSION: i64 = 3;
const MAX_UNRECORDED_IDS: usize = 4096;

type Reply = oneshot::Sender<Result<Value>>;

struct Work {
    method: String,
    params: Value,
    reply: Reply,
}

/// Cloneable asynchronous facade for the single SQLite owner thread.
#[derive(Clone)]
pub struct Store {
    sender: mpsc::SyncSender<Work>,
    unavailable: Option<std::sync::Arc<String>>,
}

struct Database {
    connection: Connection,
    root: PathBuf,
    unrecorded: HashSet<String>,
    unrecorded_order: VecDeque<String>,
    evidence_cleanup_error: Option<String>,
}

impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root).with_context(|| format!("create store directory {}", root.display()))?;
        fs::create_dir_all(root.join("backups")).context("create backup directory")?;
        set_private_directory(root)?;
        set_private_directory(&root.join("backups"))?;
        let root = root.to_path_buf();
        let (sender, receiver) = mpsc::sync_channel::<Work>(128);
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        thread::Builder::new().name("laya-sqlite".into()).spawn(move || {
            let database = Database::open(root);
            match database {
                Ok(mut database) => {
                    let _ = ready_sender.send(Ok(()));
                    while let Ok(work) = receiver.recv() {
                        let result = database.call(&work.method, work.params);
                        let _ = work.reply.send(result);
                    }
                }
                Err(error) => { let _ = ready_sender.send(Err(error)); }
            }
        }).context("start database thread")?;
        ready_receiver.recv().context("database thread stopped during startup")??;
        Ok(Self { sender, unavailable:None })
    }

    pub fn unavailable(reason:String)->Self {
        let (sender,_receiver)=mpsc::sync_channel(1);
        Self {sender,unavailable:Some(std::sync::Arc::new(reason))}
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        if let Some(reason)=&self.unavailable {bail!("unavailable: persistence disabled: {reason}; repair the database and restart service");}
        let (reply, receiver) = oneshot::channel();
        self.sender.try_send(Work { method: method.into(), params, reply })
            .map_err(|_| anyhow!("busy: database queue full or stopped"))?;
        receiver.await.map_err(|_| anyhow!("unavailable: database thread stopped"))?
    }
}

impl Database {
    fn open(root: PathBuf) -> Result<Self> {
        cleanup_restore_stages(&root)?;
        let database_path = root.join("laya.sqlite3");
        let had_database = database_path.metadata().map(|metadata|metadata.len()>0).unwrap_or(false);
        let connection = Connection::open(&database_path).context("open workbench database")?;
        set_private_file(&database_path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "secure_delete", "ON")?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION { bail!("invalid: database schema {} is newer than supported {}", version, SCHEMA_VERSION); }
        if version < SCHEMA_VERSION {
            if had_database {
                let migration_path = root.join("backups").join(format!("pre-migration-{}.sqlite3",uuid::Uuid::new_v4()));
                create_archive(&connection,&root,&migration_path)?;
            }
            migrate(&connection)?;
        }
        connection.execute("UPDATE settings SET value_json=json_set(value_json,'$.replay_enabled',json('true')) WHERE json_type(value_json,'$.replay_enabled') IS NULL",[])?;
        connection.execute("UPDATE jobs SET status='interrupted', updated_at=?1 WHERE status IN('running','queued')", [now()])?;
        let mut database=Self { connection, root, unrecorded: HashSet::new(), unrecorded_order: VecDeque::new(), evidence_cleanup_error:None };
        if let Err(error)=database.cleanup_evidence(true) {database.evidence_cleanup_error=Some(error.to_string());}
        Ok(database)
    }

    fn call(&mut self, method: &str, value: Value) -> Result<Value> {
        let result=match method {
            "settings/get" => self.settings_get(),
            "settings/update" => self.settings_update(value),
            "decisions/begin" => self.decisions_begin(value),
            "decisions/finish" => self.decisions_finish(value),
            "decisions/list" => self.decisions_list(value),
            "decisions/get" => self.decisions_get(value),
            "decisions/is_deleted" | "is_deleted" => self.decisions_is_deleted(value),
            "decisions/delete" => self.decisions_delete(value),
            "feedback" => self.feedback(value),
            "reviews/create" => self.reviews_create(value),
            "cases/list" => self.cases_list(value),
            "cases/delete" => self.cases_delete(value),
            "versions/create" => self.versions_create(value),
            "versions/list" => self.versions_list(value),
            "versions/get" => self.versions_get(value),
            "versions/report" => self.versions_report(value),
            "versions/activate" => self.versions_activate(value),
            "memory/retrieve" => self.memory_retrieve(value),
            "jobs/create" => self.jobs_create(value),
            "jobs/list" => self.jobs_list(value),
            "jobs/update" => self.jobs_update(value),
            "jobs/get" => self.jobs_get(value),
            "events" => self.events(value),
            "backup/create" => self.backup_create(value),
            "backup/list" => self.backup_list(),
            "backup/delete" => self.backup_delete(value),
            "backup/restore" => self.backup_restore(value),
            "retention" => self.retention(value),
            "status" => self.status(),
            _ => bail!("not_found: unknown store method {method}"),
        };
        if matches!(method,"decisions/begin"|"decisions/finish"|"decisions/delete"|"cases/delete"|"retention"|"backup/restore") {
            let cleanup=self.cleanup_evidence(result.is_err() || !matches!(method,"decisions/begin"|"decisions/finish") || self.evidence_cleanup_error.is_some());
            self.evidence_cleanup_error=cleanup.as_ref().err().map(ToString::to_string);
            if result.is_ok() {cleanup?;}
        }
        result
    }

    fn cleanup_evidence(&self,scan_orphans:bool)->Result<()> {
        if scan_orphans {evidence::cleanup_temporary(&self.root)?;}
        let ids=if scan_orphans {evidence::list(&self.root)?} else {
            let mut statement=self.connection.prepare("SELECT id FROM artifacts WHERE NOT EXISTS(SELECT 1 FROM decision_snapshots WHERE artifact_id=artifacts.id)")?;
            let ids=statement.query_map([],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            ids
        };
        for id in ids {
            let referenced:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM decision_snapshots WHERE artifact_id=?1)",[&id],|row|row.get(0))?;
            if !referenced {evidence::remove(&self.root,&id).context("evidence cleanup pending; retry the operation")?;}
        }
        self.connection.execute("DELETE FROM artifacts WHERE NOT EXISTS(SELECT 1 FROM decision_snapshots WHERE artifact_id=artifacts.id)",[])?;
        Ok(())
    }

    fn settings_get(&self) -> Result<Value> {
        let (text, active): (String, Option<String>) = self.connection.query_row(
            "SELECT value_json, active_memory_version FROM settings WHERE singleton=1", [], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut value = parse_json(&text)?;
        value["active_memory_version"] = active.map(Value::String).unwrap_or(Value::Null);
        Ok(value)
    }

    fn settings_update(&mut self, value: Value) -> Result<Value> {
        let updates = object(&value, "settings update")?;
        allowed(updates, &["recording_enabled", "memory_enabled", "replay_enabled", "retention_days", "storage_soft_limit_bytes"])?;
        if let Some(v) = updates.get("recording_enabled") { require_bool(v, "recording_enabled")?; }
        if let Some(v) = updates.get("memory_enabled") { require_bool(v, "memory_enabled")?; }
        if let Some(v) = updates.get("replay_enabled") { require_bool(v, "replay_enabled")?; }
        if let Some(v) = updates.get("retention_days") {
            let n = require_i64(v, "retention_days")?;
            if !(1..=36500).contains(&n) { bail!("invalid: retention_days out of range"); }
        }
        if let Some(v) = updates.get("storage_soft_limit_bytes") {
            if require_i64(v, "storage_soft_limit_bytes")? < 1024 * 1024 { bail!("invalid: storage_soft_limit_bytes too small"); }
        }
        let current = self.settings_get()?;
        let mut merged = object(&current, "stored settings")?.clone();
        merged.remove("active_memory_version");
        for (key, value) in updates { merged.insert(key.clone(), value.clone()); }
        self.connection.execute("UPDATE settings SET value_json=?1 WHERE singleton=1", [Value::Object(merged).to_string()])?;
        emit(&self.connection, "settings.updated", &value)?;
        self.settings_get()
    }

    fn decisions_begin(&mut self, value: Value) -> Result<Value> {
        let map = object(&value, "decision begin")?;
        allowed(map, &["request_id", "id", "request", "capture_redacted"])?;
        let request_id = required_string(map, "request_id", 128)?;
        let id = required_string(map, "id", 128)?;
        let request = map.get("request").ok_or_else(|| anyhow!("invalid: request is required"))?;
        if !self.settings_get()?["recording_enabled"].as_bool().unwrap_or(false) {
            self.remember_unrecorded(id);
            return Ok(json!({"id":id,"status":"begun","request_id":request_id,"recording_status":"not_recorded","existing":false}));
        }
        let capture_redacted=map.get("capture_redacted").map(|value|require_bool(value,"capture_redacted")).transpose()?.unwrap_or(false);
        let (request, changed) = redact(request);
        let changed=changed||capture_redacted;
        let existing: Option<(String,String,Option<String>)> = self.connection.query_row(
            "SELECT id,request_json,result_json FROM decisions WHERE request_id=?1", [request_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
        if let Some((existing_id, existing_request, result)) = existing {
            if existing_id != id || existing_request != request.to_string() { bail!("conflict: request_id {request_id} was already used with different content"); }
            return Ok(json!({"id":id,"status":"begun","request_id":request_id,"recording_status":"stored","existing":true,"result":result.map(|text|parse_json(&text)).transpose()?}));
        }
        let timestamp = now();
        let transaction = self.connection.transaction()?;
        transaction.execute("INSERT INTO decisions(id,request_id,request_json,recording_status,created_at) VALUES(?1,?2,?3,'stored',?4)",
            params![id, request_id, request.to_string(), timestamp]).map_err(conflict)?;
        insert_snapshot(&self.root, &transaction, id, request_id, None, "request", &request, changed, false, timestamp)?;
        emit_tx(&transaction, "decision.created", &json!({"id":id}))?;
        transaction.commit()?;
        #[cfg(test)] crate::test_fault("snapshot.after_commit")?;
        Ok(json!({"id":id,"status":"begun","request_id":request_id,"recording_status":"stored","existing":false}))
    }

    fn decisions_finish(&mut self, value: Value) -> Result<Value> {
        let map = object(&value, "decision finish")?;
        allowed(map, &["id", "result", "error", "context", "capture_redacted"])?;
        let id = required_string(map, "id", 128)?;
        if self.unrecorded.remove(id) {
            self.unrecorded_order.retain(|stored|stored!=id);
            return Ok(json!({"id":id,"recording_status":"not_recorded"}));
        }
        let result = map.get("result").cloned().unwrap_or(Value::Null);
        let error = map.get("error").cloned().unwrap_or(Value::Null);
        let context = map.get("context").cloned().unwrap_or(Value::Null);
        let capture_redacted=map.get("capture_redacted").map(|value|require_bool(value,"capture_redacted")).transpose()?.unwrap_or(false);
        let (result, result_redacted) = redact(&result);
        let (error, error_redacted) = redact(&error);
        let (context, context_redacted) = redact(&context);
        let result_redacted=result_redacted||capture_redacted;
        let error_redacted=error_redacted||capture_redacted;
        let context_redacted=context_redacted||capture_redacted;
        let transaction = self.connection.transaction()?;
        let existing:Option<(String,Option<String>,Option<String>,Option<String>,Option<i64>)>=transaction.query_row("SELECT request_id,result_json,error_json,context_json,finished_at FROM decisions WHERE id=?1 AND deleted_at IS NULL",[id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional()?;
        let (request_id,stored_result,stored_error,stored_context,finished_at)=existing.ok_or_else(|| anyhow!("not_found: decision {id}"))?;
        if finished_at.is_some() {
            if stored_result.as_deref()==Some(&result.to_string())&&stored_error.as_deref()==Some(&error.to_string())&&stored_context.as_deref()==Some(&context.to_string()) {
                return Ok(json!({"id":id,"recording_status":"stored","existing":true}));
            }
            bail!("conflict: decision {id} was already finished with different content");
        }
        transaction.execute("UPDATE decisions SET result_json=?2,error_json=?3,context_json=?4,finished_at=?5 WHERE id=?1",
            params![id, result.to_string(), error.to_string(), context.to_string(), now()])?;
        if !result.is_null() { insert_snapshot(&self.root, &transaction, id, &request_id, None, "output", &result, result_redacted, false, now())?; }
        if !error.is_null() { insert_snapshot(&self.root, &transaction, id, &request_id, None, "error", &error, error_redacted, true, now())?; }
        if !context.is_null() { insert_snapshot(&self.root, &transaction, id, &request_id, None, "context", &context, context_redacted, false, now())?; }
        if is_uncertain(&result) || is_high_risk(&result) || is_invalid_result(&result) || !error.is_null() {
            transaction.execute("UPDATE decisions SET protected=1 WHERE id=?1", [id])?;
            transaction.execute("UPDATE decision_snapshots SET protected=1 WHERE decision_id=?1", [id])?;
        }
        emit_tx(&transaction, "decision.finished", &json!({"id":id}))?;
        transaction.commit()?;
        Ok(json!({"id":id,"recording_status":"stored"}))
    }

    fn remember_unrecorded(&mut self, id: &str) {
        if self.unrecorded.insert(id.to_string()) { self.unrecorded_order.push_back(id.to_string()); }
        while self.unrecorded.len() > MAX_UNRECORDED_IDS {
            if let Some(oldest) = self.unrecorded_order.pop_front() { self.unrecorded.remove(&oldest); } else { break; }
        }
    }

    fn decisions_list(&self, value: Value) -> Result<Value> {
        let map = object(&value, "decision list")?;
        allowed(map, &["limit", "offset", "filter"])?;
        let limit = bounded_limit(map.get("limit"), 50, 200)?;
        let offset = nonnegative(map.get("offset"), 0, "offset")?;
        let empty_filter=Map::new();
        let filter = match map.get("filter") { Some(value)=>object(value,"decision filter")?, None=>&empty_filter };
        allowed(filter,&["status","risk","source","created_after","created_before"])?;
        let status=filter.get("status").map(|value|required_string_value(value,"status",32)).transpose()?.unwrap_or("all");
        if !["all","pending","deleted","finished"].contains(&status){bail!("invalid: unknown decision status filter");}
        let risk=filter.get("risk").map(|value|required_string_value(value,"risk",16)).transpose()?;
        if risk.is_some_and(|value|!["low","medium","high"].contains(&value)){bail!("invalid: unknown risk filter");}
        let source=filter.get("source").map(|value|required_string_value(value,"source",128)).transpose()?;
        let created_after=filter.get("created_after").map(|value|require_i64(value,"created_after")).transpose()?;
        let created_before=filter.get("created_before").map(|value|require_i64(value,"created_before")).transpose()?;
        if created_after.is_some_and(|value|value<0)||created_before.is_some_and(|value|value<0){bail!("invalid: created time filters must be nonnegative UTC epoch seconds");}
        if matches!((created_after,created_before),(Some(after),Some(before)) if after>before){bail!("invalid: created_after must not exceed created_before");}
        let query=format!("{} SELECT {} FROM review_queue WHERE ((?1='deleted' AND deleted_at IS NOT NULL) OR (?1<>'deleted' AND deleted_at IS NULL AND (?1='all' OR (?1='finished' AND finished_at IS NOT NULL) OR (?1='pending' AND latest_review_status='pending' AND review_priority>0)))) AND (?2 IS NULL OR risk=?2) AND (?3 IS NULL OR EXISTS(SELECT 1 FROM feedback_events source_event WHERE source_event.decision_id=review_queue.id AND (json_extract(source_event.source_json,'$.host')=?3 OR json_extract(source_event.source_json,'$.role')=?3))) AND (?4 IS NULL OR created_at>=?4) AND (?5 IS NULL OR created_at<=?5) ORDER BY CASE WHEN ?1='pending' THEN review_priority END DESC,CASE WHEN ?1='pending' THEN created_at END ASC,CASE WHEN ?1='pending' THEN id END ASC,created_at DESC LIMIT ?6 OFFSET ?7",review_queue_cte(),review_queue_list_columns());
        let mut statement = self.connection.prepare(&query)?;
        let items = statement.query_map(params![status,risk,source,created_after,created_before,limit,offset], review_queue_list_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({"items":items,"limit":limit,"offset":offset}))
    }

    fn decisions_get(&self, value: Value) -> Result<Value> {
        let id = id_param(&value)?;
        let decision = self.connection.query_row(
            "SELECT id,request_id,request_json,result_json,error_json,context_json,recording_status,protected,created_at,finished_at FROM decisions WHERE id=?1 AND deleted_at IS NULL", [id], decision_row).optional()?
            .ok_or_else(|| anyhow!("not_found: decision {id}"))?;
        let feedback = json_column(&self.connection, "SELECT json_object('event_id',event_id,'kind',kind,'attempt_ref',attempt_ref,'source',json(source_json),'payload',json(payload_json),'payload_hash',event_hash,'receive_sequence',receive_sequence,'received_at',received_at) FROM feedback_events WHERE decision_id=?1 ORDER BY receive_sequence", id)?;
        let reviews = json_column(&self.connection, "SELECT json_object('revision',r.revision,'status',r.status,'labels',json(r.labels_json),'reason',r.reason,'actor',json(r.actor_json),'created_at',r.created_at,'task_family',c.task_family,'language',c.language,'applicability',c.applicability,'applicability_reason',c.applicability_reason,'validation_assignment_event_id',c.validation_assignment_event_id,'validation_context',json(c.validation_context_json),'verification_status',c.verification_status,'last_validated_at',c.last_validated_at) FROM reviews r LEFT JOIN cases c ON c.decision_id=r.decision_id AND c.review_revision=r.revision WHERE r.decision_id=?1 ORDER BY r.revision", id)?;
        let mut snapshots = json_column(&self.connection, "SELECT json_object('id',id,'request_id',request_id,'attempt_ref',attempt_ref,'kind',kind,'payload',json(payload_json),'artifact_id',artifact_id,'payload_hash',payload_hash,'capture_status',capture_status,'redaction_version',redaction_version,'uncertainty_reasons',json(uncertainty_reasons_json),'protected',json(protected),'created_at',created_at) FROM decision_snapshots WHERE decision_id=?1 ORDER BY id", id)?;
        for snapshot in &mut snapshots {
            if let Some(artifact_id)=snapshot["artifact_id"].as_str() {
                snapshot["payload"]=evidence::read(&self.root,&artifact_get(&self.connection,artifact_id)?)?;
            }
        }
        let execution_attempts = json_column(&self.connection, "SELECT json_object('id',id,'event_id',event_id,'attempt_ref',attempt_ref,'role',role,'recommended',json(recommended_json),'selected',json(selected_json),'requested',json(requested_json),'effective',json(effective_json),'parent_attempt_ref',parent_attempt_ref,'change_reason',change_reason,'mixed_configuration',CASE WHEN mixed_configuration=1 THEN json('true') ELSE json('false') END,'environment',json(environment_json),'created_at',created_at) FROM execution_attempts WHERE decision_id=?1 ORDER BY id", id)?;
        let model_observations = json_column(&self.connection, "SELECT json_object('id',id,'event_id',event_id,'attempt_ref',attempt_ref,'stage',stage,'observation',json(observation_json),'created_at',created_at) FROM model_observations WHERE decision_id=?1 ORDER BY id", id)?;
        let mut detail = decision.as_object().cloned().unwrap_or_default();
        detail.insert("feedback".into(), Value::Array(feedback));
        detail.insert("reviews".into(), Value::Array(reviews));
        detail.insert("snapshots".into(), Value::Array(snapshots));
        detail.insert("execution_attempts".into(), Value::Array(execution_attempts));
        detail.insert("model_observations".into(), Value::Array(model_observations));
        let mut detail = Value::Object(detail);
        attach_triage(&self.connection,&mut detail)?;
        Ok(detail)
    }

    fn decisions_is_deleted(&self, value: Value) -> Result<Value> {
        let id = id_param(&value)?;
        let deleted: Option<bool> = self.connection.query_row("SELECT deleted_at IS NOT NULL FROM decisions WHERE id=?1", [id], |row| row.get(0)).optional()?;
        Ok(json!({"id":id,"exists":deleted.is_some(),"deleted":deleted.unwrap_or(false)}))
    }

    fn decisions_delete(&mut self, value: Value) -> Result<Value> {
        let id = id_param(&value)?.to_string();
        let transaction = self.connection.transaction()?;
        let exists: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM decisions WHERE id=?1)", [&id], |row| row.get(0))?;
        if !exists { bail!("not_found: decision {id}"); }
        purge_decision(&self.root,&transaction,&id,now())?;
        repair_active_version(&transaction)?;
        emit_tx(&transaction, "decision.deleted", &json!({"id":id}))?;
        transaction.commit()?;
        Ok(json!({"id":id,"deleted":true,"tombstone":true}))
    }

    fn feedback(&mut self, event: Value) -> Result<Value> {
        validate_feedback(&event)?;
        let map = event.as_object().unwrap();
        let event_id = map["event_id"].as_str().unwrap();
        let decision_id = map["decision_id"].as_str().unwrap();
        if self.unrecorded.contains(decision_id) { return Ok(json!({"status":"not_recorded","event_id":event_id})); }
        let event_hash = hash(&event);
        if let Some((stored_hash, sequence)) = self.connection.query_row(
            "SELECT event_hash,receive_sequence FROM feedback_events WHERE event_id=?1", [event_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))).optional()? {
            if stored_hash != event_hash { bail!("conflict: event_id {event_id} was already used with different content"); }
            return Ok(json!({"status":"stored","event_id":event_id,"payload_hash":stored_hash,"receive_sequence":sequence,"idempotent":true}));
        }
        let decision: Option<(bool,String)> = self.connection.query_row("SELECT deleted_at IS NOT NULL,recording_status FROM decisions WHERE id=?1", [decision_id], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
        let source_expired=matches!(decision.as_ref(),Some((false,status)) if status=="source_expired");
        match decision {
            None => bail!("not_found: decision {decision_id}"),
            Some((true,_)) => bail!("not_found: decision {decision_id} was deleted"),
            _ => {}
        }
        let payload = map.get("payload").unwrap();
        let source = map.get("source").unwrap();
        let attempt_ref = map.get("attempt_ref").and_then(Value::as_str);
        let kind = map["kind"].as_str().unwrap();
        let scores = payload.get("scores").and_then(Value::as_array).cloned().unwrap_or_default();
        for score in &scores {
            if score.get("phase").and_then(Value::as_str)==Some("revision") {
                let supersedes=score.get("supersedes_event_id").and_then(Value::as_str).unwrap_or_default();
                let dependency_exists:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM feedback_events WHERE event_id=?1)",[supersedes],|row|row.get(0))?;
                if !dependency_exists { bail!("waiting_dependency: superseded event {supersedes} has not arrived"); }
            }
        }
        let primary = scores.first();
        let sequence: i64 = self.connection.query_row("SELECT COALESCE(MAX(receive_sequence),0)+1 FROM feedback_events", [], |row| row.get(0))?;
        let transaction = self.connection.transaction()?;
        transaction.execute("INSERT INTO feedback_events(event_id,decision_id,attempt_ref,kind,source_json,payload_json,event_hash,rubric_version,phase,source_sequence,supersedes_event_id,receive_sequence,received_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)", params![
            event_id, decision_id, attempt_ref, kind, source.to_string(), payload.to_string(), event_hash,
            primary.and_then(|v| v.get("rubric_version")).and_then(Value::as_str),
            primary.and_then(|v| v.get("phase")).and_then(Value::as_str),
            primary.and_then(|v| v.get("source_sequence")).and_then(Value::as_i64),
            primary.and_then(|v| v.get("supersedes_event_id")).and_then(Value::as_str), sequence, now()
        ]).map_err(conflict)?;
        for (ordinal, score) in scores.iter().enumerate() {
            insert_score(&transaction, event_id, decision_id, attempt_ref, source, score, ordinal as i64)?;
        }
        if kind == "assignment" { insert_assignment(&transaction, event_id, decision_id, attempt_ref, source, payload)?; }
        if !scores.is_empty() || ["review", "outcome", "user_choice", "test"].contains(&kind) {
            transaction.execute("UPDATE decisions SET protected=1 WHERE id=?1", [decision_id])?;
            transaction.execute("UPDATE decision_snapshots SET protected=1 WHERE decision_id=?1", [decision_id])?;
        }
        emit_tx(&transaction, "feedback.stored", &json!({"event_id":event_id,"decision_id":decision_id,"receive_sequence":sequence}))?;
        transaction.commit()?;
        Ok(json!({"status":"stored","event_id":event_id,"payload_hash":event_hash,"receive_sequence":sequence,"source_expired":source_expired}))
    }

    fn reviews_create(&mut self, value: Value) -> Result<Value> {
        let map = object(&value, "review")?;
        allowed(map, &["id", "expected_revision", "status", "labels", "reason", "actor", "task_family", "language", "applicability", "applicability_reason", "validation_assignment_event_id"])?;
        let id = required_string(map, "id", 128)?;
        let expected = map.get("expected_revision").map(|v| require_i64(v, "expected_revision")).transpose()?.unwrap_or(0);
        let status = required_string(map, "status", 32)?;
        if !["pending", "confirmed", "corrected", "insufficient", "excluded"].contains(&status) { bail!("invalid: review status"); }
        let labels = map.get("labels").cloned().unwrap_or_else(|| json!({}));
        if ["confirmed", "corrected"].contains(&status) {
            let labels_map = object(&labels, "review labels")?;
            allowed(labels_map,&["complexity","risk","certainty"])?;
            for key in ["complexity", "risk", "certainty"] {
                if !labels_map.contains_key(key) { bail!("invalid: completed review requires {key} label"); }
            }
            if !["low","medium","high"].contains(&labels_map["complexity"].as_str().unwrap_or("")) { bail!("invalid: complexity label"); }
            if !["low","medium","high"].contains(&labels_map["risk"].as_str().unwrap_or("")) { bail!("invalid: risk label"); }
            if !["clear","uncertain"].contains(&labels_map["certainty"].as_str().unwrap_or("")) { bail!("invalid: certainty label"); }
        }
        let reason = required_string(map, "reason", 8192)?;
        let transaction = self.connection.transaction()?;
        let current: i64 = transaction.query_row("SELECT COALESCE(MAX(revision),0) FROM reviews WHERE decision_id=?1", [id], |row| row.get(0))?;
        let exists: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM decisions WHERE id=?1 AND deleted_at IS NULL)", [id], |row| row.get(0))?;
        if !exists { bail!("not_found: decision {id}"); }
        if current != expected { bail!("conflict: expected review revision {expected}, current revision is {current}"); }
        let revision = current + 1;
        let revokes_previous=["confirmed","corrected","excluded","insufficient"].contains(&status)&&current>0;
        if revokes_previous {
            transaction.execute("UPDATE memory_versions SET invalidated=1,status=CASE WHEN status='active' THEN 'invalidated' ELSE status END,invalidation_reason=?2 WHERE id IN(SELECT mvc.version_id FROM memory_version_cases mvc JOIN cases c ON c.id=mvc.case_id WHERE c.decision_id=?1 AND c.active=1)",params![id,format!("review revised to {status}")])?;
            transaction.execute("DELETE FROM cases_fts WHERE case_id IN(SELECT id FROM cases WHERE decision_id=?1 AND active=1)",[id])?;
            transaction.execute("UPDATE cases SET active=0 WHERE decision_id=?1 AND active=1",[id])?;
        }
        let actor = map.get("actor").cloned().unwrap_or(Value::Null);
        transaction.execute("INSERT INTO reviews(decision_id,revision,status,labels_json,reason,actor_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![id, revision, status, labels.to_string(), reason, actor.to_string(), now()])?;
        let mut case_id = Value::Null;
        if ["confirmed", "corrected"].contains(&status) {
            let request: String = transaction.query_row("SELECT request_json FROM decisions WHERE id=?1", [id], |row| row.get(0))?;
            let request = parse_json(&request)?;
            let state = request.get("state").ok_or_else(||anyhow!("invalid: completed review requires request.state"))?;
            let summary = state.as_str().map(str::to_string).unwrap_or_else(||state.to_string());
            if summary.is_empty() { bail!("invalid: completed review requires a non-empty request.state summary"); }
            let content = Value::String(summary.chars().take(4096).collect());
            let task_family = optional_string(map,"task_family",128)?.unwrap_or("general");
            let inferred_language = if contains_cjk(&summary){"zh"}else{"en"};
            let language = optional_string(map,"language",32)?.unwrap_or(inferred_language);
            let applicability = optional_string(map,"applicability",64)?.unwrap_or("task-fact");
            if !["task-fact","configuration-dependent"].contains(&applicability){bail!("invalid: case applicability");}
            let requested_reason = optional_string(map,"applicability_reason",8192)?;
            let validation_assignment_event_id = optional_string(map,"validation_assignment_event_id",128)?;
            if applicability=="task-fact"&&validation_assignment_event_id.is_some(){bail!("invalid: task-fact case cannot bind a validation assignment");}
            let (verification_status,validation_context,last_validated_at,applicability_reason)=if applicability=="task-fact" {
                ("verified",None,Some(now()),requested_reason.map(str::to_string))
            } else {
                derive_case_validation(&transaction,id,validation_assignment_event_id,requested_reason)?
            };
            let cid = format!("{id}:{revision}");
            transaction.execute("INSERT INTO cases(id,decision_id,review_revision,content_json,labels_json,task_family,language,applicability,applicability_reason,content_hash,created_at,validation_assignment_event_id,validation_context_json,verification_status,last_validated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)", params![
                cid, id, revision, content.to_string(), labels.to_string(), task_family, language, applicability, applicability_reason, hash(&content), now(), validation_assignment_event_id, validation_context.map(|value|value.to_string()), verification_status, last_validated_at
            ])?;
            transaction.execute("INSERT INTO cases_fts(case_id,content) VALUES(?1,?2)", params![cid, searchable(&content)])?;
            case_id = Value::String(cid);
        }
        emit_tx(&transaction, "review.created", &json!({"id":id,"revision":revision,"status":status}))?;
        if revokes_previous { repair_active_version(&transaction)?; }
        transaction.commit()?;
        Ok(json!({"id":id,"revision":revision,"status":status,"case_id":case_id}))
    }

    fn cases_list(&self, value: Value) -> Result<Value> {
        let map = object(&value, "case list")?;
        allowed(map, &["limit", "offset", "include_deleted"])?;
        let limit = bounded_limit(map.get("limit"), 50, 200)?;
        let offset = nonnegative(map.get("offset"), 0, "offset")?;
        let include_deleted = map.get("include_deleted").and_then(Value::as_bool).unwrap_or(false);
        let mut statement = self.connection.prepare(if include_deleted {
            "SELECT id,decision_id,review_revision,content_json,labels_json,task_family,language,applicability,content_hash,active,deleted_at,created_at,applicability_reason,validation_assignment_event_id,validation_context_json,verification_status,last_validated_at FROM cases ORDER BY created_at DESC LIMIT ?1 OFFSET ?2"
        } else {
            "SELECT id,decision_id,review_revision,content_json,labels_json,task_family,language,applicability,content_hash,active,deleted_at,created_at,applicability_reason,validation_assignment_event_id,validation_context_json,verification_status,last_validated_at FROM cases WHERE deleted_at IS NULL AND active=1 ORDER BY created_at DESC LIMIT ?1 OFFSET ?2"
        })?;
        let items = statement.query_map(params![limit, offset], case_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({"items":items,"limit":limit,"offset":offset}))
    }

    fn cases_delete(&mut self, value: Value) -> Result<Value> {
        let id = id_param(&value)?.to_string();
        let transaction = self.connection.transaction()?;
        if transaction.execute("UPDATE cases SET active=0,deleted_at=COALESCE(deleted_at,?2) WHERE id=?1", params![id, now()])? == 0 { bail!("not_found: case {id}"); }
        scrub_case_references(&self.root,&transaction,&[id.clone()])?;
        transaction.execute("DELETE FROM cases_fts WHERE case_id=?1", [&id])?;
        transaction.execute("UPDATE memory_versions SET invalidated=1,status=CASE WHEN status='active' THEN 'invalidated' ELSE status END,invalidation_reason='referenced case deleted' WHERE id IN(SELECT version_id FROM memory_version_cases WHERE case_id=?1)", [&id])?;
        repair_active_version(&transaction)?;
        emit_tx(&transaction, "case.deleted", &json!({"id":id}))?;
        transaction.commit()?;
        Ok(json!({"id":id,"deleted":true}))
    }

    fn versions_create(&mut self, value: Value) -> Result<Value> {
        let map = object(&value, "version create")?;
        allowed(map, &["id", "parent_id", "case_ids", "configuration"])?;
        let id = required_string(map, "id", 128)?;
        let parent = optional_string(map, "parent_id", 128)?;
        let configuration = map.get("configuration").cloned().unwrap_or_else(|| json!({}));
        let case_ids = map.get("case_ids").map(string_array).transpose()?;
        let transaction = self.connection.transaction()?;
        if let Some(parent) = parent {
            let found: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM memory_versions WHERE id=?1)", [parent], |row| row.get(0))?;
            if !found { bail!("not_found: parent version {parent}"); }
        }
        transaction.execute("INSERT INTO memory_versions(id,parent_id,status,configuration_json,created_at) VALUES(?1,?2,'candidate',?3,?4)", params![id, parent, configuration.to_string(), now()]).map_err(conflict)?;
        let members = if let Some(ids) = case_ids { ids } else {
            let mut statement = transaction.prepare("SELECT id FROM cases WHERE active=1 AND deleted_at IS NULL ORDER BY id")?;
            let values = statement.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
            values
        };
        for case_id in &members {
            let case_hash: Option<String> = transaction.query_row("SELECT content_hash FROM cases WHERE id=?1 AND active=1 AND deleted_at IS NULL", [case_id], |row| row.get(0)).optional()?;
            let case_hash = case_hash.ok_or_else(|| anyhow!("not_found: active case {case_id}"))?;
            transaction.execute("INSERT INTO memory_version_cases(version_id,case_id,case_hash) VALUES(?1,?2,?3)", params![id, case_id, case_hash])?;
        }
        emit_tx(&transaction, "version.created", &json!({"id":id,"case_count":members.len()}))?;
        transaction.commit()?;
        Ok(json!({"id":id,"status":"candidate","evaluation_status":"pending","case_count":members.len()}))
    }

    fn versions_list(&self, value: Value) -> Result<Value> {
        let map = object(&value, "version list")?;
        allowed(map, &["limit", "offset"])?;
        let limit = bounded_limit(map.get("limit"), 50, 200)?;
        let offset = nonnegative(map.get("offset"), 0, "offset")?;
        let mut statement = self.connection.prepare("SELECT id,parent_id,status,configuration_json,evaluation_json,evaluation_status,invalidated,invalidation_reason,created_at,activated_at,(SELECT COUNT(*) FROM memory_version_cases c WHERE c.version_id=memory_versions.id) FROM memory_versions ORDER BY created_at DESC LIMIT ?1 OFFSET ?2")?;
        let items = statement.query_map(params![limit, offset], version_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({"items":items,"limit":limit,"offset":offset}))
    }

    fn versions_get(&self, value: Value) -> Result<Value> {
        let id = id_param(&value)?;
        let version = self.connection.query_row("SELECT id,parent_id,status,configuration_json,evaluation_json,evaluation_status,invalidated,invalidation_reason,created_at,activated_at,(SELECT COUNT(*) FROM memory_version_cases c WHERE c.version_id=memory_versions.id) FROM memory_versions WHERE id=?1", [id], version_row).optional()?
            .ok_or_else(|| anyhow!("not_found: version {id}"))?;
        let cases = json_column(&self.connection, "SELECT json_object('id',c.id,'summary',json(c.content_json),'content',json(c.content_json),'labels',json(c.labels_json),'content_hash',mvc.case_hash,'applicability',c.applicability,'applicability_reason',c.applicability_reason,'validation_assignment_event_id',c.validation_assignment_event_id,'validation_context',json(c.validation_context_json),'verification_status',c.verification_status,'last_validated_at',c.last_validated_at) FROM memory_version_cases mvc JOIN cases c ON c.id=mvc.case_id WHERE mvc.version_id=?1 ORDER BY c.id", id)?;
        let mut detail = version.as_object().cloned().unwrap_or_default();
        detail.insert("case_ids".into(), Value::Array(cases.iter().filter_map(|case| case.get("id").cloned()).collect()));
        detail.insert("cases".into(), Value::Array(cases));
        let report = detail.get("evaluation").cloned().unwrap_or(Value::Null);
        detail.insert("report".into(), report);
        Ok(Value::Object(detail))
    }

    fn versions_report(&mut self, value: Value) -> Result<Value> {
        let map = object(&value, "version report")?;
        allowed(map, &["id", "report"])?;
        let id = required_string(map, "id", 128)?;
        let report = map.get("report").ok_or_else(|| anyhow!("invalid: report is required"))?;
        let passed = report.get("passed").and_then(Value::as_bool).ok_or_else(|| anyhow!("invalid: report.passed must be boolean"))?;
        let status = if passed { "passed" } else { "failed" };
        let current: Option<(String,bool,bool)> = self.connection.query_row("SELECT status,invalidated,evaluation_json IS NOT NULL FROM memory_versions WHERE id=?1", [id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
        let (current_status,invalidated,reported) = current.ok_or_else(||anyhow!("not_found: version {id}"))?;
        if invalidated { bail!("invalid: version {id} is invalidated"); }
        if current_status == "active" || current_status == "superseded" { bail!("conflict: activated version reports are immutable"); }
        if reported { bail!("conflict: version {id} already has an evaluation report"); }
        self.connection.execute("UPDATE memory_versions SET evaluation_json=?2,evaluation_status=?3 WHERE id=?1", params![id, report.to_string(), status])?;
        emit(&self.connection, "version.reported", &json!({"id":id,"evaluation_status":status}))?;
        Ok(json!({"id":id,"evaluation_status":status}))
    }

    fn versions_activate(&mut self, value: Value) -> Result<Value> {
        let id = id_param(&value)?.to_string();
        let transaction = self.connection.transaction()?;
        let state: Option<(String, bool)> = transaction.query_row("SELECT evaluation_status,invalidated FROM memory_versions WHERE id=?1", [&id], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
        let (evaluation, invalidated) = state.ok_or_else(|| anyhow!("not_found: version {id}"))?;
        if invalidated { bail!("invalid: version {id} is invalidated"); }
        if evaluation != "passed" { bail!("invalid: version {id} has not passed evaluation"); }
        let missing: i64 = transaction.query_row("SELECT COUNT(*) FROM memory_version_cases mvc LEFT JOIN cases c ON c.id=mvc.case_id AND c.deleted_at IS NULL AND c.active=1 WHERE mvc.version_id=?1 AND c.id IS NULL", [&id], |row| row.get(0))?;
        if missing != 0 { bail!("invalid: version references deleted cases"); }
        transaction.execute("UPDATE memory_versions SET status='superseded' WHERE status='active' AND id<>?1", [&id])?;
        transaction.execute("UPDATE memory_versions SET status='active',activated_at=?2 WHERE id=?1", params![id, now()])?;
        transaction.execute("UPDATE settings SET active_memory_version=?1 WHERE singleton=1", [&id])?;
        emit_tx(&transaction, "version.activated", &json!({"id":id}))?;
        transaction.commit()?;
        Ok(json!({"id":id,"status":"active"}))
    }

    fn memory_retrieve(&self, value: Value) -> Result<Value> {
        let map = object(&value, "memory retrieve")?;
        allowed(map, &["query", "version", "exclude_ids", "exclude_task_families", "configuration", "evaluation"])?;
        let query = required_string(map, "query", 16 * 1024)?;
        let explicit_version = optional_string(map, "version", 128)?.map(str::to_string);
        let evaluation = map.get("evaluation").map(|v|require_bool(v,"evaluation")).transpose()?.unwrap_or(false);
        if evaluation && explicit_version.is_none() { bail!("invalid: evaluation retrieval requires an explicit version"); }
        if !evaluation && !self.settings_get()?["memory_enabled"].as_bool().unwrap_or(true) { return Ok(json!({"version":null,"cases":[],"reason":"memory_disabled"})); }
        let version = explicit_version.or_else(|| self.settings_get().ok()?.get("active_memory_version")?.as_str().map(str::to_string));
        let Some(version) = version else { return Ok(json!({"version":null,"cases":[],"reason":"no_active_version"})); };
        let excludes = map.get("exclude_ids").map(string_array).transpose()?.unwrap_or_default().into_iter().collect::<HashSet<_>>();
        let excluded_families = map.get("exclude_task_families").map(string_array).transpose()?.unwrap_or_default().into_iter().map(|family|family.to_lowercase()).collect::<HashSet<_>>();
        let configuration = map.get("configuration").and_then(Value::as_object);
        let missing_family = configuration.and_then(|c|c.get("task_family")).and_then(Value::as_str).is_none();
        if (!evaluation || excluded_families.is_empty()) && missing_family || configuration.and_then(|c|c.get("language")).and_then(Value::as_str).is_none() {
            return Ok(json!({"version":version,"cases":[],"reason":"missing_task_family_or_language_filter"}));
        }
        let valid: Option<(bool, String, String)> = self.connection.query_row("SELECT invalidated,evaluation_status,status FROM memory_versions WHERE id=?1", [&version], |row| Ok((row.get(0)?, row.get(1)?,row.get(2)?))).optional()?;
        let usable = matches!(valid.as_ref(),Some((false,report,_)) if report=="passed") || evaluation && matches!(valid.as_ref(),Some((false,_,status)) if status=="candidate");
        if !usable { bail!("invalid: memory version is not usable"); }
        let terms = search_terms(query);
        if terms.is_empty() { return Ok(json!({"version":version,"cases":[],"reason":"no_search_terms"})); }
        let match_query = terms.iter().take(32).map(|term|format!("\"{}\"",term.replace('"',"\"\""))).collect::<Vec<_>>().join(" OR ");
        let mut statement = self.connection.prepare("SELECT c.id,c.content_json,c.labels_json,c.task_family,c.language,c.applicability,c.content_hash,c.applicability_reason,c.validation_assignment_event_id,c.validation_context_json,c.verification_status,c.last_validated_at FROM cases_fts JOIN cases c ON c.id=cases_fts.case_id JOIN memory_version_cases mvc ON mvc.case_id=c.id WHERE cases_fts MATCH ?2 AND mvc.version_id=?1 AND c.active=1 AND c.deleted_at IS NULL ORDER BY bm25(cases_fts) LIMIT 128")?;
        let mut ranked = Vec::new();
        let rows = statement.query_map(params![version,match_query], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, Option<String>>(3)?, row.get::<_, Option<String>>(4)?, row.get::<_, String>(5)?, row.get::<_, String>(6)?, row.get::<_,Option<String>>(7)?, row.get::<_,Option<String>>(8)?, row.get::<_,Option<String>>(9)?, row.get::<_,String>(10)?, row.get::<_,Option<i64>>(11)?)))?;
        let mut applicability_exclusions=Vec::new();
        for row in rows {
            let (id, content, labels, task_family, language, applicability, content_hash, applicability_reason, validation_assignment_event_id, validation_context, verification_status, last_validated_at) = row?;
            if excludes.contains(&id) || task_family.as_ref().is_some_and(|family|excluded_families.contains(&family.to_lowercase())) { continue; }
            if configuration.and_then(|value|value.get("task_family")).and_then(Value::as_str).is_some_and(|expected|task_family.as_deref()!=Some(expected)) {continue;}
            if configuration.and_then(|value|value.get("language")).and_then(Value::as_str).is_some_and(|expected|language.as_deref()!=Some(expected)) {continue;}
            if configuration.and_then(|value|value.get("applicability")).and_then(Value::as_str).is_some_and(|expected|applicability!=expected) {continue;}
            let validation_context=validation_context.as_deref().map(parse_json).transpose()?;
            match configuration_matches(configuration,&applicability,&verification_status,validation_context.as_ref()) {
                ApplicabilityMatch::Match=>{},
                ApplicabilityMatch::Exclude(status,reason)=>{if applicability_exclusions.len()<128{applicability_exclusions.push(json!({"case_id":id,"status":status,"reason":reason}));}continue;}
            }
            if evaluation && near_duplicate(query,&parse_json(&content)?.as_str().unwrap_or_default()) { continue; }
            let haystack = format!("{} {}", content, labels).to_lowercase();
            let score = terms.iter().filter(|term| haystack.contains(term.as_str())).count();
            if score > 0 { ranked.push((score,id,content,labels,applicability,content_hash,applicability_reason,validation_assignment_event_id,validation_context,verification_status,last_validated_at)); }
        }
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let mut bytes = 2usize;
        let mut cases = Vec::new();
        for (score,id,content,labels,applicability,content_hash,applicability_reason,validation_assignment_event_id,validation_context,verification_status,last_validated_at) in ranked.into_iter().take(3) {
            let summary = parse_json(&content)?;
            let injected=json!({"id":id,"summary":summary,"labels":parse_json(&labels)?});
            let size=serde_json::to_vec(&injected)?.len()+usize::from(!cases.is_empty());
            if bytes + size > 4096 { continue; }
            bytes += size;
            cases.push(json!({"id":id,"summary":summary,"content":summary,"labels":parse_json(&labels)?,"applicability":applicability,"applicability_reason":applicability_reason,"validation_assignment_event_id":validation_assignment_event_id,"validation_context":validation_context,"verification_status":verification_status,"last_validated_at":last_validated_at,"content_hash":content_hash,"match_count":score}));
        }
        let reason=if !cases.is_empty(){"matched"}else if applicability_exclusions.iter().any(|item|item["status"]=="unknown"){"applicability_unknown"}else if !applicability_exclusions.is_empty(){"needs_revalidation"}else{"no_match"};
        Ok(json!({"version":version,"cases":cases,"reason":reason,"applicability_exclusions":applicability_exclusions}))
    }

    fn jobs_create(&mut self, value: Value) -> Result<Value> {
        let map = object(&value, "job create")?;
        allowed(map, &["id", "kind", "payload", "status"])?;
        let id = required_string(map, "id", 128)?;
        let kind = required_string(map, "kind", 64)?;
        let status = optional_string(map, "status", 32)?.unwrap_or("queued");
        validate_job_status(status)?;
        let payload = map.get("payload").cloned().unwrap_or_else(|| json!({}));
        self.connection.execute("INSERT INTO jobs(id,kind,status,payload_json,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?5)", params![id, kind, status, payload.to_string(), now()]).map_err(conflict)?;
        emit(&self.connection, "job.created", &json!({"id":id,"kind":kind,"status":status}))?;
        self.jobs_get(json!({"id":id}))
    }

    fn jobs_list(&self, value: Value) -> Result<Value> {
        let map = object(&value, "job list")?;
        allowed(map, &["limit", "offset", "status"])?;
        let limit = bounded_limit(map.get("limit"), 50, 200)?;
        let offset = nonnegative(map.get("offset"), 0, "offset")?;
        let status = optional_string(map, "status", 32)?;
        let mut statement = self.connection.prepare("SELECT id,kind,status,payload_json,progress_json,result_json,error_json,cancel_requested,created_at,updated_at FROM jobs WHERE (?1 IS NULL OR status=?1) ORDER BY updated_at DESC LIMIT ?2 OFFSET ?3")?;
        let items = statement.query_map(params![status, limit, offset], job_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({"items":items,"limit":limit,"offset":offset}))
    }

    fn jobs_update(&mut self, value: Value) -> Result<Value> {
        let map = object(&value, "job update")?;
        allowed(map, &["id", "status", "progress", "result", "error", "cancel_requested"])?;
        let id = required_string(map, "id", 128)?;
        let current = self.jobs_get(json!({"id":id}))?;
        let status = optional_string(map, "status", 32)?.unwrap_or(current["status"].as_str().unwrap());
        validate_job_status(status)?;
        let progress = map.get("progress").cloned().or_else(|| current.get("progress").cloned()).unwrap_or(Value::Null);
        let result = map.get("result").cloned().or_else(|| current.get("result").cloned()).unwrap_or(Value::Null);
        let error = map.get("error").cloned().or_else(|| current.get("error").cloned()).unwrap_or(Value::Null);
        let cancel = map.get("cancel_requested").and_then(Value::as_bool).unwrap_or_else(|| current["cancel_requested"].as_bool().unwrap_or(false));
        self.connection.execute("UPDATE jobs SET status=?2,progress_json=?3,result_json=?4,error_json=?5,cancel_requested=?6,updated_at=?7 WHERE id=?1", params![id,status,progress.to_string(),result.to_string(),error.to_string(),cancel,now()])?;
        emit(&self.connection, "job.updated", &json!({"id":id,"status":status}))?;
        self.jobs_get(json!({"id":id}))
    }

    fn jobs_get(&self, value: Value) -> Result<Value> {
        let id = id_param(&value)?;
        self.connection.query_row("SELECT id,kind,status,payload_json,progress_json,result_json,error_json,cancel_requested,created_at,updated_at FROM jobs WHERE id=?1", [id], job_row).optional()?
            .ok_or_else(|| anyhow!("not_found: job {id}"))
    }

    fn events(&self, value: Value) -> Result<Value> {
        let map = object(&value, "events")?;
        allowed(map, &["after", "limit"])?;
        let after = nonnegative(map.get("after"), 0, "after")?;
        let limit = bounded_limit(map.get("limit"), 100, 1000)?;
        let latest:i64=self.connection.query_row("SELECT COALESCE(MAX(sequence),0) FROM events",[],|row|row.get(0))?;
        if after>latest {return Ok(json!({"items":[{"sequence":latest,"kind":"stream.reset","payload":{"reason":"history_restored"},"created_at":now()}],"cursor":latest}));}
        let mut statement = self.connection.prepare("SELECT sequence,kind,payload_json,created_at FROM events WHERE sequence>?1 ORDER BY sequence LIMIT ?2")?;
        let items = statement.query_map(params![after,limit], |row| Ok(json!({"sequence":row.get::<_,i64>(0)?,"kind":row.get::<_,String>(1)?,"payload":parse_sql_json(row.get::<_,String>(2)?)?,"created_at":row.get::<_,i64>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let cursor = items.last().and_then(|v| v["sequence"].as_i64()).unwrap_or(after);
        Ok(json!({"items":items,"cursor":cursor}))
    }

    fn backup_create(&mut self, value: Value) -> Result<Value> {
        let map = object(&value, "backup create")?;
        allowed(map, &["id"])?;
        let id = optional_string(map, "id", 128)?.map(str::to_string).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        safe_component(&id)?;
        let relative = format!("backups/{id}.sqlite3");
        let path = self.root.join(&relative);
        if path.exists() { bail!("conflict: backup {id} already exists"); }
        create_archive(&self.connection,&self.root,&path)?;
        let digest = file_hash(&path)?;
        self.connection.execute("INSERT INTO backups(id,relative_path,sha256,schema_version,created_at) VALUES(?1,?2,?3,?4,?5)", params![id,relative,digest,SCHEMA_VERSION,now()])?;
        emit(&self.connection, "backup.created", &json!({"id":id}))?;
        Ok(json!({"id":id,"sha256":digest,"schema_version":SCHEMA_VERSION,"created":true}))
    }

    fn backup_list(&self) -> Result<Value> {
        let mut statement = self.connection.prepare("SELECT id,sha256,schema_version,created_at,complete,issues_json FROM backups ORDER BY created_at DESC")?;
        let items = statement.query_map([], |row| Ok(json!({"id":row.get::<_,String>(0)?,"sha256":row.get::<_,String>(1)?,"schema_version":row.get::<_,i64>(2)?,"created_at":row.get::<_,i64>(3)?,"complete":row.get::<_,bool>(4)?,"issues":parse_sql_json(row.get::<_,String>(5)?)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({"items":items}))
    }

    fn backup_delete(&mut self,value:Value)->Result<Value> {
        let id=id_param(&value)?;safe_component(id)?;
        let exists:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM backups WHERE id=?1)",[id],|row|row.get(0))?;
        if !exists {bail!("not_found: managed backup {id}");}
        let path=self.root.join("backups").join(format!("{id}.sqlite3"));
        if path.exists() || path.is_symlink() {fs::remove_file(&path)?;}
        fs::File::open(self.root.join("backups"))?.sync_all()?;
        self.connection.execute("DELETE FROM backups WHERE id=?1",[id])?;
        emit(&self.connection,"backup.deleted",&json!({"id":id}))?;
        Ok(json!({"id":id,"deleted":true}))
    }

    fn backup_restore(&mut self, value: Value) -> Result<Value> {
        let id = id_param(&value)?.to_string();
        safe_component(&id)?;
        let (relative, digest, schema): (String,String,i64) = self.connection.query_row("SELECT relative_path,sha256,schema_version FROM backups WHERE id=?1", [&id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?
            .ok_or_else(|| anyhow!("not_found: backup {id}"))?;
        if schema > SCHEMA_VERSION { bail!("invalid: backup schema is newer than this service"); }
        let complete:bool=self.connection.query_row("SELECT complete FROM backups WHERE id=?1",[&id],|row|row.get(0))?;
        if !complete {bail!("invalid: this safety archive preserves damaged state and cannot be restored automatically");}
        let path = self.root.join(&relative);
        if relative!=format!("backups/{id}.sqlite3") || path.is_symlink() {bail!("invalid: unsafe backup path");}
        if file_hash(&path)? != digest { bail!("invalid: backup hash mismatch"); }
        let backup_catalog = {
            let mut statement=self.connection.prepare("SELECT id,relative_path,sha256,schema_version,created_at,complete,issues_json FROM backups")?;
            let values=statement.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,i64>(3)?,row.get::<_,i64>(4)?,row.get::<_,bool>(5)?,row.get::<_,String>(6)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            values
        };
        let deleted_decisions = {
            let mut statement=self.connection.prepare("SELECT id,request_id,created_at,deleted_at FROM decisions WHERE deleted_at IS NOT NULL")?;
            let values=statement.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,i64>(2)?,row.get::<_,i64>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            values
        };
        let withdrawn_cases = {
            let mut statement=self.connection.prepare("SELECT id,deleted_at FROM cases WHERE deleted_at IS NOT NULL")?;
            let values=statement.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            values
        };
        let source = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let check: String = source.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if check != "ok" { bail!("invalid: backup integrity check failed: {check}"); }
        let stage=RestoreStage::new(&self.root)?;
        let staged_path=stage.0.join("restore.sqlite3");
        let mut staged=Connection::open(&staged_path)?;
        set_private_file(&staged_path)?;
        rusqlite::backup::Backup::new(&source,&mut staged)?.run_to_completion(64,Duration::from_millis(10),None)?;
        migrate(&staged)?;
        staged.pragma_update(None,"foreign_keys","ON")?;
        staged.pragma_update(None,"secure_delete","ON")?;
        restore_archive_evidence(&staged,&stage.0)?;
        let safety_id = format!("pre-restore-{}", uuid::Uuid::new_v4());
        let safety_path = self.root.join("backups").join(format!("{safety_id}.sqlite3"));
        let safety_issues=create_archive_inner(&self.connection,&self.root,&safety_path,true)?;
        let safety_hash = file_hash(&safety_path)?;
        self.connection.execute("INSERT INTO backups(id,relative_path,sha256,schema_version,created_at,complete,issues_json) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![safety_id,format!("backups/{safety_id}.sqlite3"),safety_hash,SCHEMA_VERSION,now(),safety_issues.is_empty(),json!(safety_issues).to_string()])?;
        let transaction=staged.transaction()?;
        transaction.execute("UPDATE jobs SET status='interrupted',updated_at=?1 WHERE status IN('running','queued')",[now()])?;
        for (backup_id,relative_path,sha256,schema_version,created_at,complete,issues_json) in backup_catalog {
            transaction.execute("INSERT OR REPLACE INTO backups(id,relative_path,sha256,schema_version,created_at,complete,issues_json) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![backup_id,relative_path,sha256,schema_version,created_at,complete,issues_json])?;
        }
        transaction.execute("INSERT OR REPLACE INTO backups(id,relative_path,sha256,schema_version,created_at,complete,issues_json) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![safety_id,format!("backups/{safety_id}.sqlite3"),safety_hash,SCHEMA_VERSION,now(),safety_issues.is_empty(),json!(safety_issues).to_string()])?;
        for (decision_id,request_id,created_at,deleted_at) in deleted_decisions {
            let exists:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM decisions WHERE id=?1)",[&decision_id],|row|row.get(0))?;
            if exists { purge_decision(&stage.0,&transaction,&decision_id,deleted_at)?; }
            else { transaction.execute("INSERT INTO decisions(id,request_id,request_json,recording_status,protected,deleted_at,created_at) VALUES(?1,?2,'null','deleted',0,?3,?4)",params![decision_id,request_id,deleted_at,created_at])?; }
        }
        for (case_id,deleted_at) in withdrawn_cases {
            transaction.execute("DELETE FROM cases_fts WHERE case_id=?1",[&case_id])?;
            transaction.execute("UPDATE cases SET active=0,deleted_at=?2 WHERE id=?1",params![case_id,deleted_at])?;
            transaction.execute("UPDATE memory_versions SET invalidated=1,status=CASE WHEN status='active' THEN 'invalidated' ELSE status END,invalidation_reason='referenced case deleted' WHERE id IN(SELECT version_id FROM memory_version_cases WHERE case_id=?1)",[&case_id])?;
            scrub_case_references(&stage.0,&transaction,&[case_id])?;
        }
        repair_active_version(&transaction)?;
        emit_tx(&transaction, "backup.restored", &json!({"id":id,"safety_backup_id":safety_id,"privacy_deletions_reapplied":true}))?;
        transaction.commit()?;
        staged.execute_batch("DROP TABLE IF EXISTS backup_evidence; DROP TABLE IF EXISTS backup_evidence_damage; DELETE FROM artifacts WHERE NOT EXISTS(SELECT 1 FROM decision_snapshots WHERE artifact_id=artifacts.id); VACUUM;")?;
        #[cfg(test)] crate::test_fault("restore.before_install")?;
        for artifact in artifact_manifest(&staged)? {
            let payload=evidence::read(&stage.0,&artifact)?;
            if let Ok(current)=artifact_get(&self.connection,&artifact.id) {
                if current!=artifact {bail!("invalid: immutable evidence identity changed across backup");}
            }
            evidence::replace_verified(&self.root,&artifact,&payload)?;
        }
        rusqlite::backup::Backup::new(&staged, &mut self.connection)?.run_to_completion(64, Duration::from_millis(10), None)?;
        self.connection.pragma_update(None, "foreign_keys", "ON")?;
        self.connection.pragma_update(None, "journal_mode", "WAL")?;
        self.connection.pragma_update(None, "synchronous", "FULL")?;
        self.connection.pragma_update(None, "secure_delete", "ON")?;
        #[cfg(test)] crate::test_fault("restore.after_install")?;
        staged.close().map_err(|(_,error)|error)?;
        stage.clean()?;
        Ok(json!({"id":id,"restored":true,"safety_backup_id":safety_id,"safety_backup_complete":safety_issues.is_empty(),"safety_backup_issues":safety_issues}))
    }

    fn retention(&mut self, value: Value) -> Result<Value> {
        let map = object(&value, "retention")?;
        allowed(map, &["days"])?;
        let days = map.get("days").map(|v| require_i64(v,"days")).transpose()?.unwrap_or(self.settings_get()?["retention_days"].as_i64().unwrap_or(30));
        if !(1..=36500).contains(&days) { bail!("invalid: retention days out of range"); }
        let cutoff = now().saturating_sub(days.saturating_mul(86400));
        let transaction=self.connection.transaction()?;
        let pending_query=format!("{} SELECT id FROM review_queue WHERE protected=0 AND deleted_at IS NULL AND latest_review_status='pending' AND review_priority>0",review_queue_cte());
        let pending={
            let mut statement=transaction.prepare(&pending_query)?;
            let values=statement.query_map([],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            values
        };
        for id in &pending {
            transaction.execute("UPDATE decisions SET protected=1 WHERE id=?1",[id])?;
            transaction.execute("UPDATE decision_snapshots SET protected=1 WHERE decision_id=?1",[id])?;
        }
        let candidates={
            let mut statement=transaction.prepare("SELECT id FROM decisions WHERE protected=0 AND deleted_at IS NULL AND recording_status='stored' AND finished_at IS NOT NULL AND finished_at<?1 AND NOT EXISTS(SELECT 1 FROM reviews r WHERE r.decision_id=decisions.id) AND NOT EXISTS(SELECT 1 FROM feedback_events f WHERE f.decision_id=decisions.id)")?;
            let values=statement.query_map([cutoff],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            values
        };
        for id in &candidates {
            transaction.execute("DELETE FROM decision_snapshots WHERE decision_id=?1",[id])?;
            transaction.execute("UPDATE decisions SET request_json='null',result_json=NULL,error_json=NULL,context_json=NULL,recording_status='source_expired',finished_at=NULL WHERE id=?1",[id])?;
        }
        let removed=candidates.len();
        emit_tx(&transaction, "retention.completed", &json!({"removed":removed,"cutoff":cutoff}))?;
        transaction.commit()?;
        Ok(json!({"removed":removed,"cutoff":cutoff}))
    }

    fn status(&self) -> Result<Value> {
        let decision_count: i64 = self.connection.query_row("SELECT COUNT(*) FROM decisions WHERE deleted_at IS NULL", [], |row| row.get(0))?;
        let feedback_count: i64 = self.connection.query_row("SELECT COUNT(*) FROM feedback_events", [], |row| row.get(0))?;
        let pending_query=format!("{} SELECT COUNT(*) FROM review_queue WHERE deleted_at IS NULL AND latest_review_status='pending' AND review_priority>0",review_queue_cte());
        let pending_reviews: i64 = self.connection.query_row(&pending_query, [], |row| row.get(0))?;
        let protected_count: i64 = self.connection.query_row("SELECT COUNT(*) FROM decisions WHERE protected=1 AND deleted_at IS NULL", [], |row| row.get(0))?;
        let latest_event: i64 = self.connection.query_row("SELECT COALESCE(MAX(sequence),0) FROM events", [], |row| row.get(0))?;
        let database_bytes = fs::metadata(self.root.join("laya.sqlite3")).map(|m| m.len()).unwrap_or(0);
        let wal_bytes=fs::metadata(self.root.join("laya.sqlite3-wal")).map(|m|m.len()).unwrap_or(0);
        let evidence_bytes:u64=self.connection.query_row("SELECT COALESCE(SUM(size_bytes),0) FROM artifacts",[],|row|row.get(0))?;
        let pending_evidence_cleanup:i64=self.connection.query_row("SELECT COUNT(*) FROM artifacts WHERE NOT EXISTS(SELECT 1 FROM decision_snapshots WHERE artifact_id=artifacts.id)",[],|row|row.get(0))?;
        let storage_bytes=database_bytes.saturating_add(wal_bytes).saturating_add(evidence_bytes);
        let settings=self.settings_get()?;
        let soft_limit=settings["storage_soft_limit_bytes"].as_u64().unwrap_or(524_288_000);
        let known_attempts:i64=self.connection.query_row("SELECT COUNT(DISTINCT attempt_ref) FROM execution_attempts",[],|row|row.get(0))?;
        let attempts_with_initial_scores:i64=self.connection.query_row("SELECT COUNT(DISTINCT e.attempt_ref) FROM execution_attempts e WHERE EXISTS(SELECT 1 FROM feedback_scores s WHERE s.attempt_ref=e.attempt_ref AND s.phase='initial')",[],|row|row.get(0))?;
        let known_attempts_missing_initial_scores:i64=self.connection.query_row("SELECT COUNT(DISTINCT e.attempt_ref) FROM execution_attempts e WHERE NOT EXISTS(SELECT 1 FROM feedback_scores s WHERE s.attempt_ref=e.attempt_ref AND s.phase='initial')",[],|row|row.get(0))?;
        Ok(json!({"ok":true,"schema_version":SCHEMA_VERSION,"settings":settings,"counts":{"decisions":decision_count,"feedback":feedback_count,"pending_reviews":pending_reviews,"protected_decisions":protected_count,"unrecorded_runtime":self.unrecorded.len()},"initial_score_coverage":{"known_attempts":known_attempts,"attempts_with_initial_scores":attempts_with_initial_scores,"known_attempts_missing_initial_scores":known_attempts_missing_initial_scores,"scope":"known_attempts_only","complete":Value::Null},"latest_event":latest_event,"database_bytes":database_bytes,"wal_bytes":wal_bytes,"evidence_bytes":evidence_bytes,"pending_evidence_cleanup":pending_evidence_cleanup,"evidence_cleanup_error":self.evidence_cleanup_error,"storage_bytes":storage_bytes,"storage_soft_limit_bytes":soft_limit,"storage_pressure":storage_bytes>soft_limit}))
    }
}

fn validate_feedback(value: &Value) -> Result<()> {
    if serde_json::to_vec(value)?.len() > MAX_FEEDBACK_BYTES { bail!("invalid: feedback payload exceeds 16 KiB"); }
    let map = object(value, "feedback event")?;
    allowed(map, &["protocol_version","event_id","decision_id","attempt_ref","kind","source","payload"])?;
    if map.get("protocol_version").and_then(Value::as_u64) != Some(1) { bail!("invalid: protocol_version must be 1"); }
    required_string(map,"event_id",128)?;
    required_string(map,"decision_id",128)?;
    required_string(map,"attempt_ref",128)?;
    let kind = required_string(map,"kind",32)?;
    if !["assignment","test","review","outcome","user_choice","usage"].contains(&kind) { bail!("invalid: feedback kind"); }
    let source = object(map.get("source").ok_or_else(|| anyhow!("invalid: source is required"))?, "feedback source")?;
    allowed(source,&["host","role","actor_type"])?;
    required_string(source,"host",128)?;
    required_string(source,"role",128)?;
    if required_string(source,"actor_type",32)? != "agent" { bail!("invalid: source.actor_type must be agent"); }
    let payload = object(map.get("payload").ok_or_else(|| anyhow!("invalid: payload is required"))?, "feedback payload")?;
    let common = ["scores","reason","evidence_refs"];
    let specific: &[&str] = match kind {
        "assignment" => &["recommended","selected","requested","effective","parent_attempt_ref","change_reason","mixed_configuration","environment"],
        "test" => &["result","status","scope","test_scope","code_revision","summary"],
        "review" => &["outcome","status","disposition","proposed_labels","summary"],
        "outcome" => &["outcome","status","summary","failure_reason"],
        "user_choice" => &["choice","accepted","selected","summary"],
        "usage" => &["total_tokens","input_tokens","output_tokens","source","source_verified","scope","checkpoint","parent_scope","overlap_status"],
        _ => &[],
    };
    let mut accepted = common.to_vec(); accepted.extend_from_slice(specific);
    allowed(payload,&accepted)?;
    validate_optional_common(payload)?;
    if let Some(scores) = payload.get("scores") {
        let scores = scores.as_array().ok_or_else(|| anyhow!("invalid: payload.scores must be an array"))?;
        for score in scores { validate_score(score)?; }
    }
    if kind == "assignment" {
        for field in ["recommended","selected","requested","effective","reason","evidence_refs"] {
            if !payload.contains_key(field) { bail!("invalid: assignment payload must preserve {field}, using null when unknown"); }
        }
        required_string(payload,"reason",8192)?;
        validate_evidence_refs(payload.get("evidence_refs").unwrap())?;
        for field in ["recommended","selected","requested"] { validate_assignment_pair(&payload[field],field,false)?; }
        validate_assignment_pair(&payload["effective"],"effective",true)?;
        optional_string(payload,"parent_attempt_ref",128)?;
        optional_string(payload,"change_reason",8192)?;
        if let Some(value)=payload.get("mixed_configuration"){require_bool(value,"mixed_configuration")?;}
        if let Some(value)=payload.get("environment"){if !value.is_null(){object(value,"assignment environment")?;}}
    } else {
        validate_kind_payload(kind,payload)?;
    }
    Ok(())
}

fn validate_score(value: &Value) -> Result<()> {
    let map = object(value,"score")?;
    allowed(map,&["rubric_version","dimension","value","reason","evidence_refs","phase","source_sequence","observed_at","supersedes_event_id"])?;
    if required_string(map,"rubric_version",64)? != "laya-feedback-v1" { bail!("invalid: score rubric_version"); }
    let dimension = required_string(map,"dimension",64)?;
    if !["judgment_quality","model_fit","outcome_quality"].contains(&dimension) { bail!("invalid: score dimension"); }
    match map.get("value") { Some(Value::Null) => {}, Some(v) if matches!(v.as_i64(),Some(0..=2)) => {}, _ => bail!("invalid: score value must be 0, 1, 2, or null") }
    required_string(map,"reason",8192)?;
    map.get("evidence_refs").ok_or_else(|| anyhow!("invalid: score evidence_refs is required")).and_then(validate_evidence_refs)?;
    let phase = required_string(map,"phase",16)?;
    if !["initial","revision"].contains(&phase) { bail!("invalid: score phase"); }
    let sequence = map.get("source_sequence").map(|v| require_i64(v,"source_sequence")).transpose()?.ok_or_else(|| anyhow!("invalid: source_sequence is required"))?;
    if sequence < 1 { bail!("invalid: source_sequence must be positive"); }
    validate_observed_at(required_string(map,"observed_at",128)?)?;
    let supersedes = optional_string(map,"supersedes_event_id",128)?;
    if phase == "revision" && supersedes.is_none() { bail!("invalid: revision score requires supersedes_event_id"); }
    if phase == "initial" && supersedes.is_some() { bail!("invalid: initial score cannot supersede an event"); }
    Ok(())
}

fn validate_optional_common(payload:&Map<String,Value>)->Result<()> {
    if let Some(value)=payload.get("reason"){if !value.is_null(){let reason=value.as_str().ok_or_else(||anyhow!("invalid: reason must be a string"))?;if reason.is_empty()||reason.len()>8192{bail!("invalid: reason length");}}}
    if let Some(value)=payload.get("evidence_refs"){validate_evidence_refs(value)?;}
    Ok(())
}

fn validate_kind_payload(kind:&str,payload:&Map<String,Value>)->Result<()> {
    match kind {
        "test"=>{
            optional_enum(payload,"result",&["pass","fail","unknown"])?;
            optional_enum(payload,"status",&["pass","fail","unknown"])?;
            optional_text(payload,"scope",8192)?;
            optional_text(payload,"test_scope",8192)?;
            optional_nullable_text(payload,"code_revision",512)?;
            optional_text(payload,"summary",8192)?;
        }
        "review"=>{
            let review_states=["approved","changes_requested","rejected","blocked","disagree","no_findings","ok","unknown"];
            optional_enum(payload,"disposition",&review_states)?;
            optional_enum(payload,"outcome",&review_states)?;
            optional_enum(payload,"status",&review_states)?;
            optional_text(payload,"summary",8192)?;
            if let Some(value)=payload.get("proposed_labels") {
                let labels=object(value,"proposed_labels")?;
                allowed(labels,&["complexity","risk","certainty"])?;
                if let Some(value)=labels.get("complexity"){enum_value(value,"complexity",&["low","medium","high"])?;}
                if let Some(value)=labels.get("risk"){enum_value(value,"risk",&["low","medium","high"])?;}
                if let Some(value)=labels.get("certainty"){enum_value(value,"certainty",&["clear","uncertain"])?;}
            }
        }
        "outcome"=>{
            let states=["success","failure","partial","cancelled","unknown"];
            optional_enum(payload,"outcome",&states)?;
            optional_enum(payload,"status",&states)?;
            optional_text(payload,"summary",8192)?;
            optional_nullable_text(payload,"failure_reason",8192)?;
        }
        "user_choice"=>{
            optional_enum(payload,"choice",&["accepted","declined","rejected","modified","unknown"])?;
            if let Some(value)=payload.get("accepted"){require_bool(value,"accepted")?;}
            if let Some(value)=payload.get("selected"){if !value.is_null(){object(value,"selected")?;}}
            optional_text(payload,"summary",8192)?;
        }
        "usage"=>{
            let known=["total_tokens","input_tokens","output_tokens"].iter().filter_map(|key|payload.get(*key)).any(|value|!value.is_null());
            for key in ["total_tokens","input_tokens","output_tokens"] {
                if let Some(value)=payload.get(key){if !value.is_null()&&value.as_u64().is_none(){bail!("invalid: {key} must be a nonnegative integer or null");}}
            }
            if known {
                required_string(payload,"source",1024)?;
                if payload.get("source_verified").and_then(Value::as_bool)!=Some(true){bail!("invalid: known usage requires source_verified true");}
                let scope=required_string(payload,"scope",64)?;
                if !["response","turn","attempt","task","subtree"].contains(&scope){bail!("invalid: usage scope");}
                if !payload.contains_key("checkpoint"){bail!("invalid: known usage requires explicit checkpoint");}
                optional_string(payload,"checkpoint",512)?;
                let overlap=required_string(payload,"overlap_status",64)?;
                if !["non_overlapping","overlapping","unknown"].contains(&overlap){bail!("invalid: usage overlap_status");}
            } else {
                optional_text(payload,"source",1024)?;
                if let Some(value)=payload.get("source_verified"){require_bool(value,"source_verified")?;}
                optional_enum(payload,"scope",&["response","turn","attempt","task","subtree"])?;
                optional_nullable_text(payload,"checkpoint",512)?;
                optional_enum(payload,"overlap_status",&["non_overlapping","overlapping","unknown"])?;
            }
            optional_nullable_text(payload,"parent_scope",512)?;
        }
        _=>{}
    }
    Ok(())
}

fn optional_enum(map:&Map<String,Value>,key:&str,values:&[&str])->Result<()>{if let Some(value)=map.get(key){enum_value(value,key,values)?;}Ok(())}
fn enum_value(value:&Value,key:&str,values:&[&str])->Result<()> {let value=value.as_str().ok_or_else(||anyhow!("invalid: {key} must be a string"))?;if !values.contains(&value){bail!("invalid: {key} enum");}Ok(())}
fn optional_text(map:&Map<String,Value>,key:&str,max:usize)->Result<()>{if map.contains_key(key){required_string(map,key,max)?;}Ok(())}
fn optional_nullable_text(map:&Map<String,Value>,key:&str,max:usize)->Result<()>{optional_string(map,key,max).map(|_|())}

fn validate_assignment_pair(value:&Value,field:&str,effective:bool)->Result<()> {
    if value.is_null(){return Ok(());}
    let pair=object(value,field)?;
    allowed(pair,&["model","reasoning_effort","model_observation"])?;
    required_string(pair,"model",256)?;
    if !pair.get("reasoning_effort").is_some_and(Value::is_null){required_string(pair,"reasoning_effort",64)?;}
    let observation=object(pair.get("model_observation").ok_or_else(||anyhow!("invalid: {field}.model_observation is required"))?,"model_observation")?;
    allowed(observation,&["source","verified","observed_at","reference","provider","model_revision","catalog_version"])?;
    let source=required_string(observation,"source",32)?;
    if !["laya","user","policy","spawn_request","host"].contains(&source){bail!("invalid: {field}.model_observation.source");}
    let verified=observation.get("verified").map(|value|require_bool(value,"verified")).transpose()?.ok_or_else(||anyhow!("invalid: {field}.model_observation.verified is required"))?;
    validate_observed_at(required_string(observation,"observed_at",128)?)?;
    optional_string(observation,"reference",1024)?;
    optional_string(observation,"provider",256)?;
    optional_string(observation,"model_revision",256)?;
    optional_string(observation,"catalog_version",256)?;
    if effective&&(source!="host"||!verified){bail!("invalid: effective requires verified host model_observation or null");}
    Ok(())
}

fn insert_score(transaction: &Transaction<'_>, event_id: &str, decision_id: &str, attempt_ref: Option<&str>, source: &Value, score: &Value, ordinal: i64) -> Result<()> {
    let map = score.as_object().unwrap();
    let role = source["role"].as_str().unwrap();
    let dimension = map["dimension"].as_str().unwrap();
    let phase = map["phase"].as_str().unwrap();
    let source_sequence = map["source_sequence"].as_i64().unwrap();
    let supersedes = map.get("supersedes_event_id").and_then(Value::as_str);
    if phase == "initial" {
        let duplicate: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM feedback_scores s JOIN feedback_events f ON f.event_id=s.event_id WHERE f.decision_id=?1 AND COALESCE(s.attempt_ref,'')=COALESCE(?2,'') AND s.role=?3 AND s.dimension=?4 AND s.phase='initial')", params![decision_id,attempt_ref,role,dimension], |row| row.get(0))?;
        if duplicate { bail!("conflict: initial score already exists for decision, attempt, role, and dimension"); }
    } else {
        let target: Option<(String,i64)> = transaction.query_row("SELECT f.decision_id,s.source_sequence FROM feedback_scores s JOIN feedback_events f ON f.event_id=s.event_id WHERE s.event_id=?1 AND s.role=?2 AND s.dimension=?3 AND COALESCE(s.attempt_ref,'')=COALESCE(?4,'')", params![supersedes.unwrap(),role,dimension,attempt_ref], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
        let (target,target_sequence) = target.ok_or_else(|| anyhow!("invalid: superseded event has no matching score for attempt, role, and dimension"))?;
        if target != decision_id { bail!("invalid: revision score does not match superseded decision"); }
        if source_sequence<=target_sequence {bail!("invalid: revision source_sequence must increase");}
    }
    let sequence_conflict: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM feedback_scores s JOIN feedback_events f ON f.event_id=s.event_id WHERE f.decision_id=?1 AND COALESCE(s.attempt_ref,'')=COALESCE(?2,'') AND s.role=?3 AND s.dimension=?4 AND s.source_sequence=?5)", params![decision_id,attempt_ref,role,dimension,source_sequence], |row| row.get(0))?;
    if sequence_conflict { bail!("conflict: score source_sequence already exists"); }
    transaction.execute("INSERT INTO feedback_scores(event_id,ordinal,attempt_ref,role,rubric_version,dimension,value,reason,evidence_refs_json,phase,source_sequence,observed_at,supersedes_event_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)", params![event_id,ordinal,attempt_ref,role,map["rubric_version"].as_str(),dimension,map["value"].as_i64(),map["reason"].as_str(),map["evidence_refs"].to_string(),phase,source_sequence,map["observed_at"].as_str(),supersedes])?;
    Ok(())
}

fn insert_assignment(transaction: &Transaction<'_>, event_id: &str, decision_id: &str, attempt_ref: Option<&str>, source: &Value, payload: &Value) -> Result<()> {
    let attempt_ref = attempt_ref.ok_or_else(|| anyhow!("invalid: assignment feedback requires attempt_ref"))?;
    transaction.execute("INSERT INTO execution_attempts(event_id,attempt_ref,decision_id,role,recommended_json,selected_json,requested_json,effective_json,parent_attempt_ref,change_reason,mixed_configuration,environment_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)", params![event_id,attempt_ref,decision_id,source["role"].as_str(),payload["recommended"].to_string(),payload["selected"].to_string(),payload["requested"].to_string(),payload["effective"].to_string(),payload.get("parent_attempt_ref").and_then(Value::as_str),payload.get("change_reason").and_then(Value::as_str),payload.get("mixed_configuration").and_then(Value::as_bool).unwrap_or(false),payload.get("environment").unwrap_or(&Value::Null).to_string(),now()]).map_err(conflict)?;
    for stage in ["recommended","selected","requested","effective"] {
        if let Some(observation)=payload[stage].get("model_observation") {
            transaction.execute("INSERT INTO model_observations(event_id,decision_id,attempt_ref,stage,observation_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![event_id,decision_id,attempt_ref,stage,observation.to_string(),now()])?;
        }
    }
    Ok(())
}

fn migrate(connection:&Connection)->Result<()> {
    let version:i64=connection.query_row("PRAGMA user_version",[],|row|row.get(0))?;
    if version>SCHEMA_VERSION {bail!("invalid: database schema is newer than supported");}
    let transaction=connection.unchecked_transaction()?;
    if version<1 {transaction.execute_batch(MIGRATION_1)?;}
    if version<2 {transaction.execute_batch(MIGRATION_2)?;}
    if version<3 {transaction.execute_batch(MIGRATION_3)?;}
    transaction.pragma_update(None,"user_version",SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}

fn artifact_get(connection:&Connection,id:&str)->Result<Evidence> {
    connection.query_row("SELECT id,relative_path,sha256,size_bytes FROM artifacts WHERE id=?1",[id],|row|Ok(Evidence{id:row.get(0)?,relative_path:row.get(1)?,sha256:row.get(2)?,size_bytes:row.get(3)?})).optional()?.ok_or_else(||anyhow!("unavailable: missing evidence catalog entry {id}"))
}

fn artifact_manifest(connection:&Connection)->Result<Vec<Evidence>> {
    let (count,bytes):(i64,i64)=connection.query_row("SELECT COUNT(*),COALESCE(SUM(size_bytes),0) FROM artifacts",[],|row|Ok((row.get(0)?,row.get(1)?)))?;
    if count>65536 || bytes>1024*1024*1024 {bail!("invalid: evidence archive exceeds 65536 files or 1 GiB");}
    let mut statement=connection.prepare("SELECT id,relative_path,sha256,size_bytes FROM artifacts ORDER BY id")?;
    let records=statement.query_map([],|row|Ok(Evidence{id:row.get(0)?,relative_path:row.get(1)?,sha256:row.get(2)?,size_bytes:row.get(3)?}))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(records)
}

fn create_archive(connection:&Connection,root:&Path,path:&Path)->Result<()> {
    create_archive_inner(connection,root,path,false).map(|_|())
}

fn create_archive_inner(connection:&Connection,root:&Path,path:&Path,allow_damage:bool)->Result<Vec<Value>> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    let result=(||->Result<Vec<Value>> {
        let mut issues=Vec::new();
        let mut destination=Connection::open(path)?;
        rusqlite::backup::Backup::new(connection,&mut destination)?.run_to_completion(64,Duration::from_millis(10),None)?;
        let version:i64=destination.query_row("PRAGMA user_version",[],|row|row.get(0))?;
        if version>=2 {
            destination.execute_batch("DROP TABLE IF EXISTS backup_evidence; DROP TABLE IF EXISTS backup_evidence_damage; CREATE TABLE backup_evidence(artifact_id TEXT PRIMARY KEY,payload_json TEXT NOT NULL); CREATE TABLE backup_evidence_damage(artifact_id TEXT PRIMARY KEY,error TEXT NOT NULL,original_bytes BLOB); DELETE FROM artifacts WHERE NOT EXISTS(SELECT 1 FROM decision_snapshots WHERE artifact_id=artifacts.id);")?;
            for artifact in artifact_manifest(&destination)? {
                match evidence::read(root,&artifact) {
                    Ok(payload)=>{destination.execute("INSERT INTO backup_evidence(artifact_id,payload_json) VALUES(?1,?2)",params![artifact.id,payload.to_string()])?;}
                    Err(error) if allow_damage=>{
                        let bytes=damaged_evidence_bytes(root,&artifact)?;
                        destination.execute("INSERT INTO backup_evidence_damage(artifact_id,error,original_bytes) VALUES(?1,?2,?3)",params![artifact.id,error.to_string(),bytes])?;
                        issues.push(json!({"artifact_id":artifact.id,"error":error.to_string(),"original_bytes_preserved":bytes.is_some()}));
                    }
                    Err(error)=>return Err(error),
                }
            }
        }
        destination.close().map_err(|(_,error)|error)?;
        fs::File::open(path)?.sync_all()?;
        fs::File::open(path.parent().context("backup parent")?)?.sync_all()?;
        Ok(issues)
    })();
    if result.is_err() {let _=fs::remove_file(path);}
    result
}

fn damaged_evidence_bytes(root:&Path,artifact:&Evidence)->Result<Option<Vec<u8>>> {
    use std::os::unix::fs::OpenOptionsExt;
    let id=uuid::Uuid::parse_str(&artifact.id)?;
    if id.get_version_num()!=4 || id.to_string()!=artifact.id || artifact.relative_path!=format!("evidence/{}.json",artifact.id) {bail!("invalid: unsafe damaged evidence identity");}
    let directory=root.join("evidence");
    if directory.is_symlink() {bail!("invalid: symlinked evidence directory");}
    let path=directory.join(format!("{}.json",artifact.id));
    match fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.file_type().is_file()=>bail!("invalid: damaged evidence is not a regular file"),
        Ok(_)=>{},
        Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(None),
        Err(error)=>return Err(error.into()),
    }
    let file=match fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW|libc::O_NONBLOCK).open(&path) {
        Ok(file)=>file,
        Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(None),
        Err(error)=>return Err(error.into()),
    };
    let metadata=file.metadata()?;
    if !metadata.is_file() || metadata.len()>evidence::MAX_PAYLOAD_BYTES as u64 {bail!("invalid: damaged evidence cannot be safely archived within its size bound");}
    let mut bytes=Vec::new();
    file.take(evidence::MAX_PAYLOAD_BYTES as u64+1).read_to_end(&mut bytes)?;
    if bytes.len()>evidence::MAX_PAYLOAD_BYTES {bail!("invalid: damaged evidence changed during archive");}
    Ok(Some(bytes))
}

fn restore_archive_evidence(connection:&Connection,root:&Path)->Result<()> {
    let manifest=artifact_manifest(connection)?;
    let has_blobs:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='backup_evidence')",[],|row|row.get(0))?;
    if !manifest.is_empty() && !has_blobs {bail!("invalid: backup lacks evidence payloads");}
    if has_blobs {
        let (count,largest,total):(i64,i64,i64)=connection.query_row("SELECT COUNT(*),COALESCE(MAX(length(CAST(payload_json AS BLOB))),0),COALESCE(SUM(length(CAST(payload_json AS BLOB))),0) FROM backup_evidence",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
        if count!=manifest.len() as i64 || largest>evidence::MAX_PAYLOAD_BYTES as i64 || total>1024*1024*1024 {bail!("invalid: backup evidence manifest mismatch or oversize");}
    }
    let mismatch:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM decision_snapshots s LEFT JOIN artifacts a ON a.id=s.artifact_id WHERE s.artifact_id IS NOT NULL AND (a.id IS NULL OR s.payload_hash<>a.sha256))",[],|row|row.get(0))?;
    if mismatch {bail!("invalid: backup snapshot evidence hash/reference mismatch");}
    for artifact in manifest {
        let payload:String=connection.query_row("SELECT payload_json FROM backup_evidence WHERE artifact_id=?1",[&artifact.id],|row|row.get(0))?;
        evidence::restore(root,&artifact,&parse_json(&payload)?)?;
    }
    Ok(())
}

struct RestoreStage(PathBuf);
fn cleanup_restore_stages(root:&Path)->Result<()> {
    for entry in fs::read_dir(root)? {
        let entry=entry?;
        let name=entry.file_name();
        let Some(id)=name.to_str().and_then(|name|name.strip_prefix(".restore-")) else{continue;};
        let Ok(uuid)=uuid::Uuid::parse_str(id) else{continue;};
        if uuid.get_version_num()!=4 || uuid.to_string()!=id {continue;}
        let metadata=fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {bail!("invalid: restore staging path is not a directory");}
        fs::remove_dir_all(entry.path()).context("clean interrupted private restore stage")?;
    }
    fs::File::open(root)?.sync_all()?;
    Ok(())
}
impl RestoreStage {
    fn new(root:&Path)->Result<Self> {
        let path=root.join(format!(".restore-{}",uuid::Uuid::new_v4()));
        fs::create_dir(&path)?;
        set_private_directory(&path)?;
        Ok(Self(path))
    }
    fn clean(&self)->Result<()> {
        if self.0.exists() {fs::remove_dir_all(&self.0).context("remove private restore staging directory")?;}
        fs::File::open(self.0.parent().context("restore stage parent")?)?.sync_all()?;
        Ok(())
    }
}
impl Drop for RestoreStage {fn drop(&mut self){let _=self.clean();}}

fn snapshot_storage(root:&Path,transaction:&Transaction<'_>,decision_id:&str,payload:&Value)->Result<(String,Option<String>)> {
    let text=payload.to_string();
    if text.len()>evidence::MAX_PAYLOAD_BYTES {bail!("invalid: snapshot exceeds 1 MiB; evidence not saved");}
    if text.len()<=evidence::INLINE_THRESHOLD_BYTES {return Ok((text,None));}
    let artifact=evidence::write(root,payload)?;
    transaction.execute("INSERT INTO artifacts(id,relative_path,sha256,size_bytes,decision_id,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![artifact.id,artifact.relative_path,artifact.sha256,artifact.size_bytes,decision_id,now()])?;
    Ok(("null".into(),Some(artifact.id)))
}

fn insert_snapshot(root:&Path,transaction: &Transaction<'_>, decision_id: &str, request_id: &str, attempt_ref: Option<&str>, kind: &str, payload: &Value, redacted: bool, protected: bool, timestamp: i64) -> Result<()> {
    let (inline,artifact_id)=snapshot_storage(root,transaction,decision_id,payload)?;
    transaction.execute("INSERT INTO decision_snapshots(decision_id,request_id,attempt_ref,kind,payload_json,payload_hash,capture_status,redaction_version,protected,created_at,artifact_id) VALUES(?1,?2,?3,?4,?5,?6,?7,'laya-redact-v1',?8,?9,?10)", params![decision_id,request_id,attempt_ref,kind,inline,hash(payload),if redacted{"redacted"}else{"complete"},protected,timestamp,artifact_id])?;
    Ok(())
}

fn invalidate_for_decision(transaction: &Transaction<'_>, decision_id: &str, reason: &str) -> Result<()> {
    transaction.execute("UPDATE memory_versions SET invalidated=1,status=CASE WHEN status='active' THEN 'invalidated' ELSE status END,invalidation_reason=?2 WHERE id IN(SELECT mvc.version_id FROM memory_version_cases mvc JOIN cases c ON c.id=mvc.case_id WHERE c.decision_id=?1)", params![decision_id,reason])?;
    Ok(())
}

fn purge_decision(root:&Path,transaction:&Transaction<'_>,id:&str,deleted_at:i64)->Result<()> {
    let case_ids={
        let mut statement=transaction.prepare("SELECT id FROM cases WHERE decision_id=?1")?;
        let values=statement.query_map([id],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        values
    };
    scrub_case_references(root,transaction,&case_ids)?;
    invalidate_for_decision(transaction,id,"decision deleted")?;
    transaction.execute("DELETE FROM cases_fts WHERE case_id IN (SELECT id FROM cases WHERE decision_id=?1)",[id])?;
    transaction.execute("DELETE FROM memory_version_cases WHERE case_id IN (SELECT id FROM cases WHERE decision_id=?1)",[id])?;
    transaction.execute("DELETE FROM cases WHERE decision_id=?1",[id])?;
    transaction.execute("DELETE FROM reviews WHERE decision_id=?1",[id])?;
    transaction.execute("DELETE FROM feedback_events WHERE decision_id=?1",[id])?;
    transaction.execute("DELETE FROM execution_attempts WHERE decision_id=?1",[id])?;
    transaction.execute("DELETE FROM model_observations WHERE decision_id=?1",[id])?;
    transaction.execute("DELETE FROM decision_snapshots WHERE decision_id=?1",[id])?;
    transaction.execute("UPDATE decisions SET request_json='null',result_json=NULL,error_json=NULL,context_json=NULL,recording_status='deleted',protected=0,deleted_at=?2 WHERE id=?1",params![id,deleted_at])?;
    Ok(())
}

fn scrub_case_references(root:&Path,transaction:&Transaction<'_>,case_ids:&[String])->Result<()> {
    if case_ids.is_empty(){return Ok(());}
    let withdrawn=case_ids.iter().cloned().collect::<HashSet<_>>();
    let contexts={
        let mut statement=transaction.prepare("SELECT id,context_json FROM decisions WHERE context_json IS NOT NULL AND deleted_at IS NULL")?;
        let values=statement.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        values
    };
    for (decision_id,text) in contexts {
        let mut context=parse_json(&text)?;
        if scrub_value(&mut context,&withdrawn) {
            if let Some(map)=context.as_object_mut(){map.insert("evidence_withdrawn".into(),json!(case_ids));}
            transaction.execute("UPDATE decisions SET context_json=?2 WHERE id=?1",params![decision_id,context.to_string()])?;
        }
    }
    let snapshots={
        let mut statement=transaction.prepare("SELECT id,payload_json,artifact_id,decision_id FROM decision_snapshots WHERE kind='context'")?;
        let values=statement.query_map([],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<String>>(2)?,row.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        values
    };
    for (snapshot_id,text,artifact_id,decision_id) in snapshots {
        let mut payload=match artifact_id {Some(id)=>evidence::read(root,&artifact_get(transaction,&id)?)?,None=>parse_json(&text)?};
        if scrub_value(&mut payload,&withdrawn) {
            if let Some(map)=payload.as_object_mut(){map.insert("evidence_withdrawn".into(),json!(case_ids));}
            let (inline,artifact_id)=snapshot_storage(root,transaction,&decision_id,&payload)?;
            transaction.execute("UPDATE decision_snapshots SET payload_json=?2,payload_hash=?3,capture_status='redacted',artifact_id=?4 WHERE id=?1",params![snapshot_id,inline,hash(&payload),artifact_id])?;
        }
    }
    Ok(())
}

pub(crate) fn scrub_value(value:&mut Value,withdrawn:&HashSet<String>)->bool {
    let mut changed=false;
    match value {
        Value::Object(map)=>{
            for (key,child) in map.iter_mut(){
                if key=="case_ids" {
                    if let Some(items)=child.as_array_mut(){let before=items.len();items.retain(|item|item.as_str().is_none_or(|id|!withdrawn.contains(id)));changed|=items.len()!=before;}
                } else if key=="cases" {
                    if let Some(items)=child.as_array_mut(){let before=items.len();items.retain(|item|item.get("id").or_else(||item.get("case_id")).and_then(Value::as_str).is_none_or(|id|!withdrawn.contains(id)));changed|=items.len()!=before;for item in items{changed|=scrub_value(item,withdrawn);}}
                } else {changed|=scrub_value(child,withdrawn);}
            }
        }
        Value::Array(items)=>for item in items{changed|=scrub_value(item,withdrawn);},
        _=>{}
    }
    changed
}

fn repair_active_version(transaction: &Transaction<'_>) -> Result<()> {
    let active: Option<String> = transaction.query_row("SELECT active_memory_version FROM settings WHERE singleton=1", [], |row| row.get(0))?;
    if let Some(active) = active {
        let invalid: bool = transaction.query_row("SELECT invalidated FROM memory_versions WHERE id=?1", [&active], |row| row.get(0)).unwrap_or(true);
        if invalid {
            let fallback: Option<String> = transaction.query_row("SELECT id FROM memory_versions WHERE invalidated=0 AND evaluation_status='passed' AND activated_at IS NOT NULL AND status IN('active','superseded') AND id<>?1 ORDER BY activated_at DESC LIMIT 1", [&active], |row| row.get(0)).optional()?;
            transaction.execute("UPDATE settings SET active_memory_version=?1 WHERE singleton=1", [fallback.as_deref()])?;
            if let Some(id) = fallback { transaction.execute("UPDATE memory_versions SET status='active',activated_at=?2 WHERE id=?1", params![id,now()])?; }
        }
    }
    Ok(())
}

fn emit(connection: &Connection, kind: &str, payload: &Value) -> Result<()> {
    connection.execute("INSERT INTO events(kind,payload_json,created_at) VALUES(?1,?2,?3)", params![kind,payload.to_string(),now()])?;
    Ok(())
}

fn emit_tx(transaction: &Transaction<'_>, kind: &str, payload: &Value) -> Result<()> {
    transaction.execute("INSERT INTO events(kind,payload_json,created_at) VALUES(?1,?2,?3)", params![kind,payload.to_string(),now()])?;
    Ok(())
}

fn decision_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({"id":row.get::<_,String>(0)?,"request_id":row.get::<_,String>(1)?,"request":parse_sql_json(row.get::<_,String>(2)?)?,"result":optional_sql_json(row.get::<_,Option<String>>(3)?)?,"error":optional_sql_json(row.get::<_,Option<String>>(4)?)?,"context":optional_sql_json(row.get::<_,Option<String>>(5)?)?,"recording_status":row.get::<_,String>(6)?,"protected":row.get::<_,bool>(7)?,"created_at":row.get::<_,i64>(8)?,"finished_at":row.get::<_,Option<i64>>(9)?}))
}

fn review_queue_cte()->&'static str {r#"WITH decision_signals AS (
SELECT d.*,
COALESCE(json_extract(d.result_json,'$.laya_result.risk.label'),json_extract(d.result_json,'$.advice.assessment.risk.choice'),json_extract(d.result_json,'$.answers.risk.choice'),CASE WHEN json_type(d.result_json,'$.risk')='text' THEN json_extract(d.result_json,'$.risk') END) AS risk,
COALESCE((COALESCE(json_extract(d.result_json,'$.uncertain'),0)=1 OR json_extract(d.result_json,'$.certainty')='uncertain' OR COALESCE(json_extract(d.result_json,'$.advice.uncertain'),0)=1 OR json_extract(d.result_json,'$.advice.certainty')='uncertain'),0) AS uncertain,
(d.finished_at IS NOT NULL AND (d.result_json IS NULL OR d.result_json='null' OR json_type(d.result_json)<>'object' OR (json_type(d.result_json,'$.laya_result') IS NULL AND json_type(d.result_json,'$.advice') IS NULL AND json_type(d.result_json,'$.answers') IS NULL AND json_type(d.result_json,'$.uncertain') IS NULL AND json_type(d.result_json,'$.certainty') IS NULL AND json_type(d.result_json,'$.risk') IS NULL))) AS invalid_result,
(d.error_json IS NOT NULL AND d.error_json<>'null') AS decision_error,
EXISTS(SELECT 1 FROM feedback_events f WHERE f.decision_id=d.id AND f.kind='review' AND json_extract(f.payload_json,'$.proposed_labels.risk')='high' AND COALESCE(json_extract(d.result_json,'$.laya_result.risk.label'),json_extract(d.result_json,'$.advice.assessment.risk.choice'),json_extract(d.result_json,'$.answers.risk.choice'),CASE WHEN json_type(d.result_json,'$.risk')='text' THEN json_extract(d.result_json,'$.risk') END,'')<>'high') AS reported_high_risk,
EXISTS(SELECT 1 FROM feedback_scores s JOIN feedback_events f ON f.event_id=s.event_id WHERE f.decision_id=d.id AND s.value IN(0,1)) AS problem_score,
EXISTS(SELECT 1 FROM feedback_events f WHERE f.decision_id=d.id AND f.kind='test' AND COALESCE(json_extract(f.payload_json,'$.result'),json_extract(f.payload_json,'$.status'))='fail') AS test_failed,
EXISTS(SELECT 1 FROM feedback_events f WHERE f.decision_id=d.id AND f.kind='outcome' AND COALESCE(json_extract(f.payload_json,'$.outcome'),json_extract(f.payload_json,'$.status')) IN('failure','partial')) AS outcome_failed,
EXISTS(SELECT 1 FROM feedback_events f WHERE f.decision_id=d.id AND f.kind='review' AND COALESCE(json_extract(f.payload_json,'$.disposition'),json_extract(f.payload_json,'$.outcome'),json_extract(f.payload_json,'$.status')) IN('changes_requested','rejected','blocked','disagree')) AS review_disagreement,
EXISTS(SELECT 1 FROM feedback_events f WHERE f.decision_id=d.id AND f.kind='user_choice' AND (json_extract(f.payload_json,'$.accepted')=0 OR json_extract(f.payload_json,'$.choice') IN('declined','rejected','modified'))) AS user_changed,
EXISTS(SELECT 1 FROM feedback_events f WHERE f.decision_id=d.id AND f.kind='assignment' AND lower(COALESCE(json_extract(f.payload_json,'$.change_reason'),'')) LIKE '%upgrade%') AS model_upgrade,
EXISTS(SELECT 1 FROM feedback_events f WHERE f.decision_id=d.id AND f.kind='review' AND ((json_type(f.payload_json,'$.proposed_labels.risk')='text' AND json_extract(f.payload_json,'$.proposed_labels.risk')<>COALESCE(json_extract(d.result_json,'$.laya_result.risk.label'),json_extract(d.result_json,'$.advice.assessment.risk.choice'),json_extract(d.result_json,'$.answers.risk.choice'),CASE WHEN json_type(d.result_json,'$.risk')='text' THEN json_extract(d.result_json,'$.risk') END,'')) OR (json_type(f.payload_json,'$.proposed_labels.complexity')='text' AND json_extract(f.payload_json,'$.proposed_labels.complexity')<>COALESCE(json_extract(d.result_json,'$.laya_result.complexity.label'),json_extract(d.result_json,'$.advice.assessment.complexity.choice'),json_extract(d.result_json,'$.answers.complexity.choice'),CASE WHEN json_type(d.result_json,'$.complexity')='text' THEN json_extract(d.result_json,'$.complexity') END,'')))) AS reported_label_change,
COALESCE((SELECT json_group_array(event_id) FROM (SELECT f.event_id FROM feedback_events f WHERE f.decision_id=d.id AND (EXISTS(SELECT 1 FROM feedback_scores s WHERE s.event_id=f.event_id AND s.value IN(0,1)) OR (f.kind='test' AND COALESCE(json_extract(f.payload_json,'$.result'),json_extract(f.payload_json,'$.status'))='fail') OR (f.kind='outcome' AND COALESCE(json_extract(f.payload_json,'$.outcome'),json_extract(f.payload_json,'$.status')) IN('failure','partial')) OR (f.kind='review' AND (COALESCE(json_extract(f.payload_json,'$.disposition'),json_extract(f.payload_json,'$.outcome'),json_extract(f.payload_json,'$.status')) IN('changes_requested','rejected','blocked','disagree') OR (json_type(f.payload_json,'$.proposed_labels.risk')='text' AND json_extract(f.payload_json,'$.proposed_labels.risk')<>COALESCE(json_extract(d.result_json,'$.laya_result.risk.label'),json_extract(d.result_json,'$.advice.assessment.risk.choice'),json_extract(d.result_json,'$.answers.risk.choice'),CASE WHEN json_type(d.result_json,'$.risk')='text' THEN json_extract(d.result_json,'$.risk') END,'')) OR (json_type(f.payload_json,'$.proposed_labels.complexity')='text' AND json_extract(f.payload_json,'$.proposed_labels.complexity')<>COALESCE(json_extract(d.result_json,'$.laya_result.complexity.label'),json_extract(d.result_json,'$.advice.assessment.complexity.choice'),json_extract(d.result_json,'$.answers.complexity.choice'),CASE WHEN json_type(d.result_json,'$.complexity')='text' THEN json_extract(d.result_json,'$.complexity') END,'')))) OR (f.kind='user_choice' AND (json_extract(f.payload_json,'$.accepted')=0 OR json_extract(f.payload_json,'$.choice') IN('declined','rejected','modified'))) OR (f.kind='assignment' AND lower(COALESCE(json_extract(f.payload_json,'$.change_reason'),'')) LIKE '%upgrade%')) ORDER BY f.receive_sequence)),'[]') AS trigger_event_ids,
COALESCE((SELECT json_group_array(source) FROM (SELECT DISTINCT json_extract(f.source_json,'$.host')||'/'||json_extract(f.source_json,'$.role') AS source FROM feedback_events f WHERE f.decision_id=d.id AND (EXISTS(SELECT 1 FROM feedback_scores s WHERE s.event_id=f.event_id AND s.value IN(0,1)) OR (f.kind='test' AND COALESCE(json_extract(f.payload_json,'$.result'),json_extract(f.payload_json,'$.status'))='fail') OR (f.kind='outcome' AND COALESCE(json_extract(f.payload_json,'$.outcome'),json_extract(f.payload_json,'$.status')) IN('failure','partial')) OR (f.kind='review' AND (COALESCE(json_extract(f.payload_json,'$.disposition'),json_extract(f.payload_json,'$.outcome'),json_extract(f.payload_json,'$.status')) IN('changes_requested','rejected','blocked','disagree') OR (json_type(f.payload_json,'$.proposed_labels.risk')='text' AND json_extract(f.payload_json,'$.proposed_labels.risk')<>COALESCE(json_extract(d.result_json,'$.laya_result.risk.label'),json_extract(d.result_json,'$.advice.assessment.risk.choice'),json_extract(d.result_json,'$.answers.risk.choice'),CASE WHEN json_type(d.result_json,'$.risk')='text' THEN json_extract(d.result_json,'$.risk') END,'')) OR (json_type(f.payload_json,'$.proposed_labels.complexity')='text' AND json_extract(f.payload_json,'$.proposed_labels.complexity')<>COALESCE(json_extract(d.result_json,'$.laya_result.complexity.label'),json_extract(d.result_json,'$.advice.assessment.complexity.choice'),json_extract(d.result_json,'$.answers.complexity.choice'),CASE WHEN json_type(d.result_json,'$.complexity')='text' THEN json_extract(d.result_json,'$.complexity') END,'')))) OR (f.kind='user_choice' AND (json_extract(f.payload_json,'$.accepted')=0 OR json_extract(f.payload_json,'$.choice') IN('declined','rejected','modified'))) OR (f.kind='assignment' AND lower(COALESCE(json_extract(f.payload_json,'$.change_reason'),'')) LIKE '%upgrade%')) ORDER BY source)),'[]') AS review_sources,
COALESCE((SELECT status FROM reviews r WHERE r.decision_id=d.id ORDER BY revision DESC LIMIT 1),'pending') AS latest_review_status
FROM decisions d
), review_queue AS (
SELECT decision_signals.*,
CASE WHEN reported_high_risk THEN 4 WHEN uncertain AND (problem_score OR test_failed OR outcome_failed OR review_disagreement OR user_changed OR reported_label_change) THEN 3 WHEN problem_score OR test_failed OR outcome_failed OR review_disagreement OR user_changed OR reported_label_change THEN 2 WHEN uncertain OR risk='high' OR invalid_result OR decision_error OR model_upgrade THEN 1 ELSE 0 END AS review_priority,
uncertain+COALESCE(risk='high',0)+invalid_result+decision_error+json_array_length(trigger_event_ids) AS review_trigger_count
FROM decision_signals
)"#}

fn review_queue_columns()->&'static str {"id,request_id,request_json,result_json,error_json,context_json,recording_status,protected,created_at,finished_at,risk,reported_high_risk,uncertain,invalid_result,decision_error,problem_score,test_failed,outcome_failed,review_disagreement,user_changed,model_upgrade,reported_label_change,review_priority,review_trigger_count,trigger_event_ids,review_sources"}

fn review_queue_list_columns()->&'static str {"id,request_id,CASE WHEN json_type(request_json,'$.state')='text' THEN substr(json_extract(request_json,'$.state'),1,280) ELSE id END,latest_review_status,recording_status,protected,created_at,finished_at,risk,reported_high_risk,uncertain,invalid_result,decision_error,problem_score,test_failed,outcome_failed,review_disagreement,user_changed,model_upgrade,reported_label_change,review_priority,review_trigger_count,trigger_event_ids,review_sources"}

fn review_queue_list_row(row:&rusqlite::Row<'_>)->rusqlite::Result<Value>{
    let mut decision=json!({"id":row.get::<_,String>(0)?,"request_id":row.get::<_,String>(1)?,"summary":row.get::<_,Option<String>>(2)?,"status":row.get::<_,String>(3)?,"recording_status":row.get::<_,String>(4)?,"protected":row.get::<_,bool>(5)?,"created_at":row.get::<_,i64>(6)?,"finished_at":row.get::<_,Option<i64>>(7)?});
    apply_review_queue_fields_at(row,&mut decision,8)?;
    Ok(decision)
}

fn apply_review_queue_fields(row:&rusqlite::Row<'_>,decision:&mut Value)->rusqlite::Result<()> {
    apply_review_queue_fields_at(row,decision,10)
}

fn apply_review_queue_fields_at(row:&rusqlite::Row<'_>,decision:&mut Value,risk_index:usize)->rusqlite::Result<()> {
    let mut reasons=Vec::new();
    if row.get::<_,bool>(risk_index+1)?{reasons.push("reported_high_risk_correction");}
    if row.get::<_,bool>(risk_index+2)?{reasons.push("uncertain");}
    if row.get::<_,Option<String>>(risk_index)?.as_deref()==Some("high"){reasons.push("high_risk");}
    if row.get::<_,bool>(risk_index+3)?{reasons.push("invalid_result");}
    if row.get::<_,bool>(risk_index+4)?{reasons.push("decision_error");}
    if row.get::<_,bool>(risk_index+5)?{reasons.push("problem_score");}
    if row.get::<_,bool>(risk_index+6)?{reasons.push("test_failed");}
    if row.get::<_,bool>(risk_index+7)?{reasons.push("outcome_failed");}
    if row.get::<_,bool>(risk_index+8)?{reasons.push("review_disagreement");}
    if row.get::<_,bool>(risk_index+9)?{reasons.push("user_choice_changed");}
    if row.get::<_,bool>(risk_index+10)?{reasons.push("model_upgrade");}
    if row.get::<_,bool>(risk_index+11)?{reasons.push("reported_label_change");}
    decision["risk"]=row.get::<_,Option<String>>(risk_index)?.map(Value::String).unwrap_or(Value::Null);
    decision["review_reasons"]=json!(reasons);
    decision["review_priority"]=json!(row.get::<_,i64>(risk_index+12)?);
    decision["review_trigger_count"]=json!(row.get::<_,i64>(risk_index+13)?);
    decision["review_trigger_event_ids"]=parse_sql_json(row.get::<_,String>(risk_index+14)?)?;
    decision["review_sources"]=parse_sql_json(row.get::<_,String>(risk_index+15)?)?;
    Ok(())
}

fn case_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let content = parse_sql_json(row.get::<_,String>(3)?)?;
    Ok(json!({"id":row.get::<_,String>(0)?,"decision_id":row.get::<_,String>(1)?,"review_revision":row.get::<_,i64>(2)?,"summary":content,"content":content,"labels":parse_sql_json(row.get::<_,String>(4)?)?,"task_family":row.get::<_,Option<String>>(5)?,"language":row.get::<_,Option<String>>(6)?,"applicability":row.get::<_,String>(7)?,"content_hash":row.get::<_,String>(8)?,"active":row.get::<_,bool>(9)?,"deleted_at":row.get::<_,Option<i64>>(10)?,"created_at":row.get::<_,i64>(11)?,"applicability_reason":row.get::<_,Option<String>>(12)?,"validation_assignment_event_id":row.get::<_,Option<String>>(13)?,"validation_context":optional_sql_json(row.get::<_,Option<String>>(14)?)?,"verification_status":row.get::<_,String>(15)?,"last_validated_at":row.get::<_,Option<i64>>(16)?}))
}

fn version_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({"id":row.get::<_,String>(0)?,"parent_id":row.get::<_,Option<String>>(1)?,"status":row.get::<_,String>(2)?,"configuration":parse_sql_json(row.get::<_,String>(3)?)?,"evaluation":optional_sql_json(row.get::<_,Option<String>>(4)?)?,"evaluation_status":row.get::<_,String>(5)?,"invalidated":row.get::<_,bool>(6)?,"invalidation_reason":row.get::<_,Option<String>>(7)?,"created_at":row.get::<_,i64>(8)?,"activated_at":row.get::<_,Option<i64>>(9)?,"case_count":row.get::<_,i64>(10)?}))
}

fn job_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({"id":row.get::<_,String>(0)?,"kind":row.get::<_,String>(1)?,"status":row.get::<_,String>(2)?,"payload":parse_sql_json(row.get::<_,String>(3)?)?,"progress":optional_sql_json(row.get::<_,Option<String>>(4)?)?,"result":optional_sql_json(row.get::<_,Option<String>>(5)?)?,"error":optional_sql_json(row.get::<_,Option<String>>(6)?)?,"cancel_requested":row.get::<_,bool>(7)?,"created_at":row.get::<_,i64>(8)?,"updated_at":row.get::<_,i64>(9)?}))
}

fn json_column(connection: &Connection, query: &str, id: &str) -> Result<Vec<Value>> {
    let mut statement = connection.prepare(query)?;
    let values = statement.query_map([id], |row| parse_sql_json(row.get::<_,String>(0)?))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(values)
}

fn attach_triage(connection:&Connection,decision:&mut Value)->Result<()> {
    let id=decision["id"].as_str().unwrap_or_default().to_string();
    let query=format!("{} SELECT {} FROM review_queue WHERE id=?1",review_queue_cte(),review_queue_columns());
    connection.query_row(&query,[id],|row|apply_review_queue_fields(row,decision))?;
    Ok(())
}

fn object<'a>(value: &'a Value, name: &str) -> Result<&'a Map<String,Value>> { value.as_object().ok_or_else(|| anyhow!("invalid: {name} must be an object")) }
fn allowed(map: &Map<String,Value>, fields: &[&str]) -> Result<()> { if let Some(key)=map.keys().find(|key| !fields.contains(&key.as_str())) { bail!("invalid: unknown field {key}"); } Ok(()) }
fn required_string<'a>(map: &'a Map<String,Value>, key: &str, max: usize) -> Result<&'a str> { let value=map.get(key).and_then(Value::as_str).ok_or_else(||anyhow!("invalid: {key} must be a string"))?; if value.is_empty()||value.len()>max { bail!("invalid: {key} length"); } Ok(value) }
fn required_string_value<'a>(value:&'a Value,key:&str,max:usize)->Result<&'a str>{let value=value.as_str().ok_or_else(||anyhow!("invalid: {key} must be a string"))?;if value.is_empty()||value.len()>max{bail!("invalid: {key} length");}Ok(value)}
fn optional_string<'a>(map: &'a Map<String,Value>, key: &str, max: usize) -> Result<Option<&'a str>> { match map.get(key) { None|Some(Value::Null)=>Ok(None), Some(Value::String(v)) if !v.is_empty()&&v.len()<=max=>Ok(Some(v)), _=>bail!("invalid: {key} must be a non-empty string or null") } }
fn require_bool(value: &Value,key:&str)->Result<bool>{value.as_bool().ok_or_else(||anyhow!("invalid: {key} must be boolean"))}
fn require_i64(value: &Value,key:&str)->Result<i64>{value.as_i64().ok_or_else(||anyhow!("invalid: {key} must be an integer"))}
fn id_param(value:&Value)->Result<&str>{required_string(object(value,"id parameter")?,"id",128)}
fn bounded_limit(value:Option<&Value>,default:i64,max:i64)->Result<i64>{let limit=value.map(|v|require_i64(v,"limit")).transpose()?.unwrap_or(default);if !(1..=max).contains(&limit){bail!("invalid: limit out of range");}Ok(limit)}
fn nonnegative(value:Option<&Value>,default:i64,key:&str)->Result<i64>{let n=value.map(|v|require_i64(v,key)).transpose()?.unwrap_or(default);if n<0{bail!("invalid: {key} must be nonnegative");}Ok(n)}
fn string_array(value:&Value)->Result<Vec<String>>{value.as_array().ok_or_else(||anyhow!("invalid: expected string array"))?.iter().map(|v|v.as_str().map(str::to_string).ok_or_else(||anyhow!("invalid: array items must be strings"))).collect()}
fn validate_evidence_refs(value:&Value)->Result<Vec<String>>{let refs=string_array(value)?;let mut unique=HashSet::new();if refs.iter().any(|reference|reference.is_empty()||!unique.insert(reference)){bail!("invalid: evidence_refs must contain unique non-empty strings");}Ok(refs)}
fn validate_observed_at(value:&str)->Result<()>{let pattern=regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$").expect("static RFC3339 regex");if !pattern.is_match(value){bail!("invalid: observed_at must be RFC3339 date-time");}Ok(())}
fn parse_json(text:&str)->Result<Value>{serde_json::from_str(text).context("invalid JSON in database")}
fn parse_sql_json(text:String)->rusqlite::Result<Value>{serde_json::from_str(&text).map_err(|e|rusqlite::Error::FromSqlConversionFailure(0,rusqlite::types::Type::Text,Box::new(e)))}
fn optional_sql_json(text:Option<String>)->rusqlite::Result<Value>{text.map(parse_sql_json).transpose().map(|v|v.unwrap_or(Value::Null))}
fn conflict(error:rusqlite::Error)->anyhow::Error{match error {rusqlite::Error::SqliteFailure(ref code,_) if code.extended_code==rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY||code.extended_code==rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE=>anyhow!("conflict: unique identifier already exists"),other=>other.into()}}
fn searchable(value:&Value)->String{search_terms(&value.to_string()).join(" ")}
fn is_uncertain(value:&Value)->bool{value.get("uncertain").and_then(Value::as_bool).unwrap_or(false)||value.get("certainty").and_then(Value::as_str)==Some("uncertain")||value.pointer("/advice/uncertain").and_then(Value::as_bool).unwrap_or(false)||value.pointer("/advice/certainty").and_then(Value::as_str)==Some("uncertain")}
fn is_high_risk(value:&Value)->bool{value.pointer("/laya_result/risk/label").and_then(Value::as_str)==Some("high")||value.pointer("/advice/assessment/risk/choice").and_then(Value::as_str)==Some("high")||value.pointer("/answers/risk/choice").and_then(Value::as_str)==Some("high")||value.get("risk").and_then(Value::as_str)==Some("high")}
fn is_invalid_result(value:&Value)->bool{value.as_object().is_none_or(|result|["laya_result","advice","answers","uncertain","certainty","risk"].iter().all(|key|!result.contains_key(*key)))}
fn validate_job_status(status:&str)->Result<()>{if !["queued","running","completed","failed","cancelled","interrupted"].contains(&status){bail!("invalid: job status");}Ok(())}
fn safe_component(value:&str)->Result<()>{if value.is_empty()||value.len()>128||value=="."||value==".."||!value.chars().all(|c|c.is_ascii_alphanumeric()||c=='-'||c=='_'){bail!("invalid: unsafe backup id");}Ok(())}
fn file_hash(path:&Path)->Result<String>{let mut file=fs::File::open(path).with_context(||format!("read backup {}",path.display()))?;let mut digest=Sha256::new();let mut buffer=[0u8;64*1024];loop{let count=file.read(&mut buffer)?;if count==0{break;}digest.update(&buffer[..count]);}Ok(format!("{:x}",digest.finalize()))}
#[cfg(unix)]
fn set_private_directory(path:&Path)->Result<()>{use std::os::unix::fs::PermissionsExt;fs::set_permissions(path,fs::Permissions::from_mode(0o700)).with_context(||format!("set private permissions on {}",path.display()))}
#[cfg(not(unix))]
fn set_private_directory(_path:&Path)->Result<()>{Ok(())}
#[cfg(unix)]
fn set_private_file(path:&Path)->Result<()>{use std::os::unix::fs::PermissionsExt;fs::set_permissions(path,fs::Permissions::from_mode(0o600)).with_context(||format!("set private permissions on {}",path.display()))}
#[cfg(not(unix))]
fn set_private_file(_path:&Path)->Result<()>{Ok(())}
fn search_terms(query:&str)->Vec<String>{let lower=query.to_lowercase();let mut terms=HashSet::new();for word in lower.split(|c:char|!c.is_alphanumeric()).filter(|s|s.chars().count()>=2){terms.insert(word.to_string());}for chars in lower.chars().filter(|c|c.is_alphanumeric()).collect::<Vec<_>>().windows(2){terms.insert(chars.iter().collect());}let mut terms=terms.into_iter().collect::<Vec<_>>();terms.sort();terms}
fn near_duplicate(left:&str,right:&str)->bool{let left=search_terms(left).into_iter().collect::<HashSet<_>>();let right=search_terms(right).into_iter().collect::<HashSet<_>>();if left.is_empty()||right.is_empty(){return false;}let intersection=left.intersection(&right).count();let union=left.union(&right).count();intersection==union||intersection*10>=union*8}
fn contains_cjk(value:&str)->bool{value.chars().any(|c|matches!(c,'\u{3400}'..='\u{4dbf}'|'\u{4e00}'..='\u{9fff}'|'\u{f900}'..='\u{faff}'))}
enum ApplicabilityMatch { Match, Exclude(&'static str,String) }

fn configuration_matches(configuration:Option<&Map<String,Value>>,applicability:&str,verification_status:&str,validation_context:Option<&Value>)->ApplicabilityMatch {
    let Some(configuration)=configuration else{return if applicability=="task-fact"{ApplicabilityMatch::Match}else{ApplicabilityMatch::Exclude("unknown","missing_retrieval_validation_context".into())}};
    if applicability=="task-fact" {return ApplicabilityMatch::Match;}
    if verification_status!="verified" {return ApplicabilityMatch::Exclude("unknown",format!("case_validation_{verification_status}"));}
    let Some(expected)=configuration.get("validation_context") else{return ApplicabilityMatch::Exclude("unknown","missing_retrieval_validation_context".into());};
    let Some(actual)=validation_context else{return ApplicabilityMatch::Exclude("unknown","missing_case_validation_context".into());};
    if expected!=actual {return ApplicabilityMatch::Exclude("needs_revalidation","validation_context_mismatch".into());}
    ApplicabilityMatch::Match
}

fn derive_case_validation(transaction:&Transaction<'_>,decision_id:&str,event_id:Option<&str>,requested_reason:Option<&str>)->Result<(&'static str,Option<Value>,Option<i64>,Option<String>)> {
    let Some(event_id)=event_id else{return Ok(("unknown",None,None,Some(append_reason(requested_reason,"validation_unknown:missing_validation_assignment"))))};
    let attempt:Option<(String,String,bool,String,String)>=transaction.query_row("SELECT a.attempt_ref,a.effective_json,a.mixed_configuration,a.environment_json,f.source_json FROM execution_attempts a JOIN feedback_events f ON f.event_id=a.event_id WHERE a.event_id=?1 AND a.decision_id=?2",params![event_id,decision_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional()?;
    let (attempt_ref,effective,mixed_configuration,environment,source)=attempt.ok_or_else(||anyhow!("invalid: validation assignment must belong to the reviewed decision"))?;
    let effective=parse_json(&effective)?;
    let environment=parse_json(&environment)?;
    let source=parse_json(&source)?;
    let observation=effective.get("model_observation").and_then(Value::as_object);
    let mut statement=transaction.prepare("SELECT DISTINCT s.rubric_version FROM feedback_scores s JOIN feedback_events f ON f.event_id=s.event_id WHERE f.decision_id=?1 AND s.attempt_ref=?2 ORDER BY s.rubric_version")?;
    let rubric_versions=statement.query_map(params![decision_id,attempt_ref],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let context=json!({
        "source":{"host":source.get("host").cloned().unwrap_or(Value::Null),"role":source.get("role").cloned().unwrap_or(Value::Null)},
        "effective":{"provider":observation.and_then(|value|value.get("provider")).cloned().unwrap_or(Value::Null),"model":effective.get("model").cloned().unwrap_or(Value::Null),"model_revision":observation.and_then(|value|value.get("model_revision")).cloned().unwrap_or(Value::Null),"reasoning_effort":effective.get("reasoning_effort").cloned().unwrap_or(Value::Null),"catalog_version":observation.and_then(|value|value.get("catalog_version")).cloned().unwrap_or(Value::Null)},
        "environment":environment,
        "rubric_versions":rubric_versions
    });
    let mut missing=Vec::new();
    if mixed_configuration {missing.push("mixed_configuration");}
    if observation.and_then(|value|value.get("source")).and_then(Value::as_str)!=Some("host")||observation.and_then(|value|value.get("verified")).and_then(Value::as_bool)!=Some(true){missing.push("unverified_effective_configuration");}
    for (pointer,reason) in [("/source/host","source_host"),("/source/role","source_role"),("/effective/provider","provider"),("/effective/model","model"),("/effective/model_revision","model_revision"),("/effective/reasoning_effort","reasoning_effort"),("/effective/catalog_version","catalog_version")] {
        if !context.pointer(pointer).and_then(Value::as_str).is_some_and(meaningful_identity){missing.push(reason);}
    }
    let stable_environment=context.get("environment").and_then(Value::as_object).is_some_and(|environment|["code_revision","task_snapshot_hash","test_snapshot_hash","tool_environment","tool_environment_id","skill_version","prompt_template_version"].iter().any(|key|environment.get(*key).and_then(Value::as_str).is_some_and(meaningful_identity)));
    if !stable_environment{missing.push("environment");}
    if context.get("rubric_versions").and_then(Value::as_array).is_none_or(Vec::is_empty){missing.push("rubric_versions");}
    if missing.is_empty(){Ok(("verified",Some(context),Some(now()),requested_reason.map(str::to_string)))}else{Ok(("unknown",Some(context),None,Some(append_reason(requested_reason,&format!("validation_unknown:{}",missing.join(","))))))}
}

fn append_reason(requested:Option<&str>,generated:&str)->String {requested.map(|reason|format!("{reason}; {generated}")).unwrap_or_else(||generated.to_string())}

fn meaningful_identity(value:&str)->bool {let normalized=value.trim().to_ascii_lowercase();!normalized.is_empty()&&!["unknown","null","none","unavailable","unspecified","n/a","?","-"].contains(&normalized.as_str())}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    async fn recorded_store() -> (tempfile::TempDir, Store) {
        let root = tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store.call("settings/update", json!({"recording_enabled":true})).await.unwrap();
        (root, store)
    }

    #[tokio::test]
    async fn recording_defaults_off_and_round_trips_decision() {
        let root = tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        assert_eq!(store.call("settings/get",json!({})).await.unwrap()["recording_enabled"],false);
        assert_eq!(store.call("decisions/begin",json!({"request_id":"r0","id":"d0","request":{"secret":"x"}})).await.unwrap()["recording_status"],"not_recorded");
        store.call("settings/update",json!({"recording_enabled":true})).await.unwrap();
        store.call("decisions/begin",json!({"request_id":"r1","id":"d1","request":{"state":"hello","api_key":"private"}})).await.unwrap();
        store.call("decisions/finish",json!({"id":"d1","result":{"uncertain":true},"error":null,"context":{"memory_version":null}})).await.unwrap();
        let found=store.call("decisions/get",json!({"id":"d1"})).await.unwrap();
        assert_eq!(found["request"]["api_key"],"[REDACTED]");
        assert_eq!(found["protected"],true);
        let snapshot_count=found["snapshots"].as_array().unwrap().len();
        let replay=store.call("decisions/finish",json!({"id":"d1","result":{"uncertain":true},"error":null,"context":{"memory_version":null}})).await.unwrap();
        assert_eq!(replay["existing"],true);
        assert_eq!(store.call("decisions/get",json!({"id":"d1"})).await.unwrap()["snapshots"].as_array().unwrap().len(),snapshot_count);
        assert!(store.call("decisions/finish",json!({"id":"d1","result":{"uncertain":false},"error":null,"context":{"memory_version":null}})).await.unwrap_err().to_string().starts_with("conflict:"));
    }

    #[tokio::test]
    async fn decision_list_projects_bounded_summaries_without_raw_evidence() {
        let (_root,store)=recorded_store().await;
        let request_marker="large-request-marker".repeat(2048);
        let result_marker="large-result-marker".repeat(2048);
        let error_marker="large-error-marker".repeat(2048);
        let context_marker="large-context-marker".repeat(2048);
        let state=format!("Bearer secret-token {}", "界".repeat(400));
        store.call("decisions/begin",json!({"request_id":"large-request","id":"large","request":{"state":state,"evidence":request_marker}})).await.unwrap();
        store.call("decisions/finish",json!({"id":"large","result":{"uncertain":true,"payload":result_marker},"error":{"payload":error_marker},"context":{"payload":context_marker}})).await.unwrap();
        store.call("decisions/begin",json!({"request_id":"object-request","id":"object","request":{"state":{"title":"object summary","payload":"x".repeat(400)}}})).await.unwrap();
        store.call("decisions/begin",json!({"request_id":"array-request","id":"array","request":{"state":["array summary","x".repeat(400)]}})).await.unwrap();
        store.call("decisions/begin",json!({"request_id":"null-request","id":"null","request":{"state":null}})).await.unwrap();

        let list=store.call("decisions/list",json!({"filter":{"status":"all"}})).await.unwrap();
        let items=list["items"].as_array().unwrap();
        let large=items.iter().find(|item|item["id"]=="large").unwrap();
        for field in ["request","result","error","context"] {assert!(large.get(field).is_none());}
        assert_eq!(large["status"],"pending");
        assert_eq!(large["recording_status"],"stored");
        assert_eq!(large["protected"],true);
        assert_eq!(large["summary"].as_str().unwrap().chars().count(),280);
        assert!(large["summary"].as_str().unwrap().starts_with("[REDACTED]"));
        let serialized=list.to_string();
        for marker in ["large-request-marker","large-result-marker","large-error-marker","large-context-marker"] {assert!(!serialized.contains(marker));}
        assert_eq!(items.iter().find(|item|item["id"]=="object").unwrap()["summary"],"object");
        assert_eq!(items.iter().find(|item|item["id"]=="array").unwrap()["summary"],"array");
        assert_eq!(items.iter().find(|item|item["id"]=="null").unwrap()["summary"],"null");

        let detail=store.call("decisions/get",json!({"id":"large"})).await.unwrap();
        assert!(detail["request"]["state"].as_str().unwrap().starts_with("[REDACTED]"));
        assert_eq!(detail["request"]["evidence"],request_marker);
        assert_eq!(detail["result"]["payload"],result_marker);
        assert_eq!(detail["error"]["payload"],error_marker);
        assert_eq!(detail["context"]["payload"],context_marker);
    }

    #[tokio::test]
    async fn review_queue_prioritizes_before_pagination_and_explains_triggers() {
        let (root,store)=recorded_store().await;
        for (id,result) in [
            ("tier-4",json!({"uncertain":false,"risk":"low"})),
            ("tier-3",json!({"advice":{"uncertain":true,"assessment":{"risk":{"choice":"medium"}}}})),
            ("tier-2",json!({"uncertain":false,"risk":"low"})),
            ("tier-1",json!({"uncertain":true,"risk":"low"})),
            ("high-alone",json!({"uncertain":false,"answers":{"risk":{"choice":"high"}}})),
            ("modified",json!({"uncertain":false,"risk":"low"})),
            ("upgrade",json!({"uncertain":false,"risk":"low"})),
            ("tie-a",json!({"certainty":"uncertain","risk":"low"})),
            ("tie-b",json!({"certainty":"uncertain","risk":"low"}))
        ] {
            store.call("decisions/begin",json!({"request_id":id,"id":id,"request":{}})).await.unwrap();
            store.call("decisions/finish",json!({"id":id,"result":result,"error":null,"context":{}})).await.unwrap();
        }
        let events=[
            json!({"protocol_version":1,"event_id":"missed-risk","decision_id":"tier-4","attempt_ref":"a","kind":"review","source":{"host":"codex","role":"reviewer","actor_type":"agent"},"payload":{"outcome":"approved","proposed_labels":{"risk":"high"}}}),
            json!({"protocol_version":1,"event_id":"failed-test","decision_id":"tier-3","attempt_ref":"a","kind":"test","source":{"host":"ci","role":"tester","actor_type":"agent"},"payload":{"result":"fail"}}),
            json!({"protocol_version":1,"event_id":"review-problem","decision_id":"tier-3","attempt_ref":"a","kind":"review","source":{"host":"codex","role":"reviewer","actor_type":"agent"},"payload":{"disposition":"changes_requested"}}),
            json!({"protocol_version":1,"event_id":"failed-outcome","decision_id":"tier-2","attempt_ref":"a","kind":"outcome","source":{"host":"pi","role":"worker","actor_type":"agent"},"payload":{"outcome":"failure"}}),
            json!({"protocol_version":1,"event_id":"modified-choice","decision_id":"modified","attempt_ref":"a","kind":"user_choice","source":{"host":"codex","role":"orchestrator","actor_type":"agent"},"payload":{"choice":"modified"}})
        ];
        for event in events {store.call("feedback",event).await.unwrap();}
        let replay=json!({"protocol_version":1,"event_id":"failed-test","decision_id":"tier-3","attempt_ref":"a","kind":"test","source":{"host":"ci","role":"tester","actor_type":"agent"},"payload":{"result":"fail"}});
        assert_eq!(store.call("feedback",replay).await.unwrap()["idempotent"],true);
        let source=json!({"host":"codex","role":"orchestrator","actor_type":"agent"});
        store.call("feedback",json!({"protocol_version":1,"event_id":"model-upgrade","decision_id":"upgrade","attempt_ref":"a","kind":"assignment","source":source,"payload":{"recommended":pair("model-a","laya",false),"selected":pair("model-b","policy",false),"requested":pair("model-b","spawn_request",false),"effective":null,"change_reason":"model upgrade","reason":"upgrade","evidence_refs":[]}})).await.unwrap();
        let connection=Connection::open(root.path().join("laya.sqlite3")).unwrap();
        for (id,created_at) in [("tier-4",50),("tier-3",10),("tier-2",20),("tier-1",30),("high-alone",40),("modified",60),("upgrade",70),("tie-a",80),("tie-b",80)] {connection.execute("UPDATE decisions SET created_at=?2 WHERE id=?1",params![id,created_at]).unwrap();}

        let first=store.call("decisions/list",json!({"limit":1,"filter":{"status":"pending"}})).await.unwrap();
        assert_eq!(first["items"][0]["id"],"tier-4");
        assert_eq!(first["items"][0]["review_priority"],4);
        assert_eq!(first["items"][0]["risk"],"low");
        assert!(first["items"][0]["review_reasons"].as_array().unwrap().iter().any(|reason|reason=="reported_high_risk_correction"));
        let queue=store.call("decisions/list",json!({"filter":{"status":"pending"}})).await.unwrap();
        let ids=queue["items"].as_array().unwrap().iter().map(|item|item["id"].as_str().unwrap()).collect::<Vec<_>>();
        assert_eq!(&ids[..4],["tier-4","tier-3","tier-2","modified"]);
        let tier3=queue["items"].as_array().unwrap().iter().find(|item|item["id"]=="tier-3").unwrap();
        assert_eq!(tier3["review_priority"],3);
        assert_eq!(tier3["review_trigger_count"],3);
        assert_eq!(tier3["review_trigger_event_ids"],json!(["failed-test","review-problem"]));
        assert_eq!(tier3["review_sources"],json!(["ci/tester","codex/reviewer"]));
        let high=queue["items"].as_array().unwrap().iter().find(|item|item["id"]=="high-alone").unwrap();
        assert_eq!(high["review_priority"],1);
        assert!(!high["review_reasons"].as_array().unwrap().iter().any(|reason|reason=="reported_high_risk_correction"));
        assert_eq!(queue["items"].as_array().unwrap().iter().find(|item|item["id"]=="upgrade").unwrap()["review_reasons"],json!(["model_upgrade"]));
        let ties=store.call("decisions/list",json!({"filter":{"status":"pending","created_after":75}})).await.unwrap();
        assert_eq!(ties["items"].as_array().unwrap().iter().map(|item|item["id"].as_str().unwrap()).collect::<Vec<_>>(),["tie-a","tie-b"]);
        let risk=store.call("decisions/list",json!({"filter":{"status":"pending","risk":"high"}})).await.unwrap();
        assert_eq!(risk["items"].as_array().unwrap().iter().map(|item|item["id"].as_str().unwrap()).collect::<Vec<_>>(),["high-alone"]);
        let reviewer=store.call("decisions/list",json!({"filter":{"status":"pending","source":"reviewer"}})).await.unwrap();
        assert_eq!(reviewer["items"].as_array().unwrap().iter().map(|item|item["id"].as_str().unwrap()).collect::<Vec<_>>(),["tier-4","tier-3"]);
        let detail=store.call("decisions/get",json!({"id":"tier-3"})).await.unwrap();
        assert_eq!(detail["review_priority"],tier3["review_priority"]);
        assert_eq!(detail["review_trigger_event_ids"],tier3["review_trigger_event_ids"]);
        assert!(store.call("decisions/list",json!({"filter":{"risk":"critical"}})).await.unwrap_err().to_string().starts_with("invalid:"));
        assert!(store.call("decisions/list",json!({"filter":{"source":1}})).await.unwrap_err().to_string().starts_with("invalid:"));
        assert!(store.call("decisions/list",json!({"filter":{"created_after":100,"created_before":10}})).await.unwrap_err().to_string().starts_with("invalid:"));
        assert!(store.call("decisions/list",json!({"filter":{"injected":"x"}})).await.unwrap_err().to_string().starts_with("invalid:"));
    }

    #[tokio::test]
    async fn changed_proposed_labels_are_reported_queue_triggers() {
        let (_root,store)=recorded_store().await;
        for id in ["complexity-change","risk-change"] {
            store.call("decisions/begin",json!({"request_id":id,"id":id,"request":{}})).await.unwrap();
            store.call("decisions/finish",json!({"id":id,"result":{"laya_result":{"complexity":{"label":"low"},"risk":{"label":"low"}}},"error":null,"context":{}})).await.unwrap();
        }
        store.call("feedback",json!({"protocol_version":1,"event_id":"complexity-proposal","decision_id":"complexity-change","attempt_ref":"a","kind":"review","source":{"host":"codex","role":"reviewer","actor_type":"agent"},"payload":{"proposed_labels":{"complexity":"high"}}})).await.unwrap();
        store.call("feedback",json!({"protocol_version":1,"event_id":"risk-proposal","decision_id":"risk-change","attempt_ref":"a","kind":"review","source":{"host":"pi","role":"reviewer","actor_type":"agent"},"payload":{"proposed_labels":{"risk":"medium"}}})).await.unwrap();
        for (id,event_id,source) in [("complexity-change","complexity-proposal","codex/reviewer"),("risk-change","risk-proposal","pi/reviewer")] {
            let detail=store.call("decisions/get",json!({"id":id})).await.unwrap();
            assert_eq!(detail["risk"],"low");
            assert_eq!(detail["review_priority"],2);
            assert_eq!(detail["review_reasons"],json!(["reported_label_change"]));
            assert_eq!(detail["review_trigger_count"],1);
            assert_eq!(detail["review_trigger_event_ids"],json!([event_id]));
            assert_eq!(detail["review_sources"],json!([source]));
        }
        let queue=store.call("decisions/list",json!({"filter":{"status":"pending"}})).await.unwrap();
        assert_eq!(queue["items"].as_array().unwrap().len(),2);
    }

    #[tokio::test]
    async fn invalid_finished_result_is_protected_from_retention() {
        let (root,store)=recorded_store().await;
        store.call("decisions/begin",json!({"request_id":"invalid","id":"invalid","request":{"state":"preserve original"}})).await.unwrap();
        store.call("decisions/finish",json!({"id":"invalid","result":{},"error":null,"context":{}})).await.unwrap();
        store.call("decisions/begin",json!({"request_id":"valid","id":"valid","request":{"state":"ordinary result"}})).await.unwrap();
        store.call("decisions/finish",json!({"id":"valid","result":{"advice":{"uncertain":false}},"error":null,"context":{}})).await.unwrap();
        let before=store.call("decisions/get",json!({"id":"invalid"})).await.unwrap();
        assert_eq!(before["protected"],true);
        assert_eq!(before["review_reasons"],json!(["invalid_result"]));
        assert!(before["snapshots"].as_array().unwrap().iter().all(|snapshot|snapshot["protected"]==1));
        assert_eq!(store.call("decisions/get",json!({"id":"valid"})).await.unwrap()["protected"],false);
        let connection=Connection::open(root.path().join("laya.sqlite3")).unwrap();
        connection.execute("UPDATE decisions SET protected=0,finished_at=0 WHERE id IN('invalid','valid')",[]).unwrap();
        connection.execute("UPDATE decision_snapshots SET protected=0 WHERE decision_id='invalid'",[]).unwrap();
        assert_eq!(store.call("retention",json!({"days":1})).await.unwrap()["removed"],1);
        let after=store.call("decisions/get",json!({"id":"invalid"})).await.unwrap();
        assert_eq!(after["protected"],true);
        assert!(after["snapshots"].as_array().unwrap().iter().all(|snapshot|snapshot["protected"]==1));
        assert_eq!(after["request"]["state"],"preserve original");
        assert_eq!(after["result"],json!({}));
        assert_eq!(after["snapshots"].as_array().unwrap().len(),3);
        assert_eq!(store.call("decisions/get",json!({"id":"valid"})).await.unwrap()["recording_status"],"source_expired");
    }

    #[tokio::test]
    async fn unrecorded_decision_tracking_is_bounded() {
        let root=tempdir().unwrap();
        let store=Store::open(root.path()).unwrap();
        for index in 0..MAX_UNRECORDED_IDS+8 {
            store.call("decisions/begin",json!({"request_id":format!("r{index}"),"id":format!("d{index}"),"request":{}})).await.unwrap();
        }
        assert_eq!(store.call("status",json!({})).await.unwrap()["counts"]["unrecorded_runtime"],MAX_UNRECORDED_IDS);
    }

    #[tokio::test]
    async fn late_feedback_is_kept_after_source_evidence_expires() {
        let (root,store)=recorded_store().await;
        store.call("decisions/begin",json!({"request_id":"old","id":"old","request":{"state":"ordinary task"}})).await.unwrap();
        store.call("decisions/finish",json!({"id":"old","result":{"advice":{"uncertain":false}},"error":null,"context":{}})).await.unwrap();
        Connection::open(root.path().join("laya.sqlite3")).unwrap().execute("UPDATE decisions SET finished_at=0 WHERE id='old'",[]).unwrap();
        assert_eq!(store.call("retention",json!({"days":1})).await.unwrap()["removed"],1);
        let event=json!({"protocol_version":1,"event_id":"late","decision_id":"old","attempt_ref":"a","kind":"review","source":{"host":"codex","role":"reviewer","actor_type":"agent"},"payload":{"disposition":"changes_requested","summary":"late evidence"}});
        let receipt=store.call("feedback",event).await.unwrap();
        assert_eq!(receipt["source_expired"],true);
        let detail=store.call("decisions/get",json!({"id":"old"})).await.unwrap();
        assert_eq!(detail["recording_status"],"source_expired");
        assert!(detail["request"].is_null());
        assert_eq!(detail["feedback"].as_array().unwrap().len(),1);
    }

    #[tokio::test]
    async fn realistic_advisor_uncertainty_protects_the_evidence_bundle() {
        let (_root,store)=recorded_store().await;
        let request=json!({"state":"Add a destructive database migration without a rollback plan","questions":["complexity","risk","certainty"],"criteria":{"models":["gpt-6-sol"],"role":"worker"},"language":"en","scope":"repository"});
        store.call("decisions/begin",json!({"request_id":"advisor-1","id":"decision-1","request":request})).await.unwrap();
        let result=json!({"laya_result":{"complexity":{"label":"high","score":0.71},"risk":{"label":"high","score":0.64},"certainty":{"label":"uncertain","score":0.41}},"advice":{"uncertain":true,"recommended":{"model":"gpt-6-sol","reasoning_effort":"high"},"reasons":["missing rollback context"]},"meta":{"model_revision":"checkpoint-digest","rules_version":"advisor-v1","memory_version":null,"case_ids":[]}});
        store.call("decisions/finish",json!({"id":"decision-1","result":result,"error":null,"context":{"checkpoint":"checkpoint-digest","rules_version":"advisor-v1","capture_status":"complete"}})).await.unwrap();
        let detail=store.call("decisions/get",json!({"id":"decision-1"})).await.unwrap();
        assert_eq!(detail["protected"],true);
        assert_eq!(detail["result"]["advice"]["recommended"]["model"],"gpt-6-sol");
        assert!(detail["review_reasons"].as_array().unwrap().iter().any(|reason|reason=="high_risk"));
        store.call("decisions/begin",json!({"request_id":"advisor-2","id":"decision-2","request":{"state":"README typo"}})).await.unwrap();
        store.call("decisions/finish",json!({"id":"decision-2","result":{"advice":{"uncertain":false,"assessment":{"risk":{"choice":"low"}}}},"error":null,"context":{}})).await.unwrap();
        let pending=store.call("decisions/list",json!({"filter":{"status":"pending"}})).await.unwrap();
        assert_eq!(pending["items"].as_array().unwrap().len(),1);
        assert_eq!(pending["items"][0]["id"],"decision-1");
    }

    fn score(phase:&str, sequence:i64, supersedes:Option<&str>)->Value {
        let mut value=json!({"rubric_version":"laya-feedback-v1","dimension":"judgment_quality","value":1,"reason":"evidence","evidence_refs":[],"phase":phase,"source_sequence":sequence,"observed_at":"2026-09-25T00:00:00Z"});
        if let Some(id)=supersedes { value["supersedes_event_id"]=json!(id); }
        value
    }

    #[tokio::test]
    async fn feedback_is_strict_idempotent_and_preserves_initial_score() {
        let (_root,store)=recorded_store().await;
        store.call("decisions/begin",json!({"request_id":"r","id":"d","request":{}})).await.unwrap();
        let event=json!({"protocol_version":1,"event_id":"e1","decision_id":"d","attempt_ref":"a","kind":"review","source":{"host":"codex","role":"reviewer","actor_type":"agent"},"payload":{"outcome":"changes_requested","scores":[score("initial",1,None)]}});
        let first=store.call("feedback",event.clone()).await.unwrap();
        let second=store.call("feedback",event).await.unwrap();
        assert_eq!(first["receive_sequence"],second["receive_sequence"]);
        assert_eq!(second["idempotent"],true);
        let duplicate=json!({"protocol_version":1,"event_id":"e2","decision_id":"d","attempt_ref":"a","kind":"review","source":{"host":"codex","role":"reviewer","actor_type":"agent"},"payload":{"outcome":"ok","scores":[score("initial",2,None)]}});
        assert!(store.call("feedback",duplicate).await.unwrap_err().to_string().starts_with("conflict:"));
    }

    fn pair(model:&str,source:&str,verified:bool)->Value {
        json!({"model":model,"reasoning_effort":"high","model_observation":{"source":source,"verified":verified,"observed_at":"2026-09-25T00:00:00Z","reference":"test"}})
    }

    #[tokio::test]
    async fn assignment_pairs_are_provenanced_and_later_observations_append() {
        let (_root,store)=recorded_store().await;
        store.call("decisions/begin",json!({"request_id":"r","id":"d","request":{"state":"task"}})).await.unwrap();
        let source=json!({"host":"codex","role":"orchestrator","actor_type":"agent"});
        let first=json!({"protocol_version":1,"event_id":"assignment-1","decision_id":"d","attempt_ref":"attempt","kind":"assignment","source":source,"payload":{"recommended":pair("model-a","laya",false),"selected":pair("model-a","policy",false),"requested":pair("model-a","spawn_request",false),"effective":null,"reason":"dispatch","evidence_refs":[]}});
        store.call("feedback",first).await.unwrap();
        let later=json!({"protocol_version":1,"event_id":"assignment-2","decision_id":"d","attempt_ref":"attempt","kind":"assignment","source":source,"payload":{"recommended":null,"selected":null,"requested":null,"effective":{"model":"model-a","reasoning_effort":null,"model_observation":{"source":"host","verified":true,"observed_at":"2026-09-25T00:00:00Z","reference":"host:1","provider":"openai","model_revision":null,"catalog_version":"catalog-1"}},"parent_attempt_ref":"parent","change_reason":"host observation arrived","mixed_configuration":true,"environment":{"code_revision":"abc"},"reason":"host observed child","evidence_refs":[]}});
        store.call("feedback",later).await.unwrap();
        let detail=store.call("decisions/get",json!({"id":"d"})).await.unwrap();
        assert_eq!(detail["execution_attempts"].as_array().unwrap().len(),2);
        assert_eq!(detail["model_observations"].as_array().unwrap().len(),4);
        assert_eq!(detail["execution_attempts"][1]["parent_attempt_ref"],"parent");
        assert_eq!(detail["execution_attempts"][1]["mixed_configuration"],true);
        assert_eq!(detail["execution_attempts"][1]["environment"]["code_revision"],"abc");
        assert!(detail["execution_attempts"][1]["effective"]["reasoning_effort"].is_null());
        assert_eq!(detail["model_observations"][3]["observation"]["catalog_version"],"catalog-1");
        let status=store.call("status",json!({})).await.unwrap();
        assert_eq!(status["settings"]["replay_enabled"],true);
        assert_eq!(status["initial_score_coverage"]["known_attempts"],1);
        assert_eq!(status["initial_score_coverage"]["known_attempts_missing_initial_scores"],1);
        assert!(status["initial_score_coverage"]["complete"].is_null());
        assert!(status.get("wal_bytes").is_some()&&status.get("storage_pressure").is_some());
        let invalid=json!({"protocol_version":1,"event_id":"assignment-3","decision_id":"d","attempt_ref":"attempt","kind":"assignment","source":source,"payload":{"recommended":null,"selected":null,"requested":null,"effective":pair("model-a","spawn_request",false),"reason":"copied request","evidence_refs":[]}});
        assert!(store.call("feedback",invalid).await.unwrap_err().to_string().starts_with("invalid:"));
    }

    #[tokio::test]
    async fn kind_specific_feedback_fields_reject_invalid_values() {
        let (_root,store)=recorded_store().await;
        store.call("decisions/begin",json!({"request_id":"r","id":"d","request":{}})).await.unwrap();
        let source=json!({"host":"codex","role":"worker","actor_type":"agent"});
        let invalid_payloads=[
            ("test",json!({"result":"maybe"})),
            ("review",json!({"disposition":"rubber_stamp"})),
            ("outcome",json!({"outcome":"great"})),
            ("user_choice",json!({"choice":"whatever"})),
            ("usage",json!({"total_tokens":-1})),
            ("usage",json!({"total_tokens":10}))
        ];
        for (index,(kind,payload)) in invalid_payloads.into_iter().enumerate() {
            let event=json!({"protocol_version":1,"event_id":format!("invalid-{index}"),"decision_id":"d","attempt_ref":"a","kind":kind,"source":source,"payload":payload});
            assert!(store.call("feedback",event).await.unwrap_err().to_string().starts_with("invalid:"));
        }
        let usage=json!({"protocol_version":1,"event_id":"usage","decision_id":"d","attempt_ref":"a","kind":"usage","source":source,"payload":{"total_tokens":10,"input_tokens":null,"output_tokens":null,"source":"host usage API","source_verified":true,"scope":"attempt","checkpoint":null,"parent_scope":null,"overlap_status":"unknown"}});
        assert_eq!(store.call("feedback",usage).await.unwrap()["status"],"stored");
    }

    #[tokio::test]
    async fn revision_selects_matching_score_inside_multi_score_event() {
        let (_root,store)=recorded_store().await;
        store.call("decisions/begin",json!({"request_id":"r","id":"d","request":{}})).await.unwrap();
        let judgment=score("initial",1,None);
        let mut model_fit=score("initial",1,None);
        model_fit["dimension"]=json!("model_fit");
        let initial=json!({"protocol_version":1,"event_id":"multi","decision_id":"d","attempt_ref":"a","kind":"review","source":{"host":"codex","role":"reviewer","actor_type":"agent"},"payload":{"disposition":"changes_requested","scores":[judgment,model_fit]}});
        let mut revision=score("revision",2,Some("multi"));
        revision["dimension"]=json!("model_fit");
        let revised=json!({"protocol_version":1,"event_id":"revision","decision_id":"d","attempt_ref":"a","kind":"review","source":{"host":"codex","role":"reviewer","actor_type":"agent"},"payload":{"disposition":"approved","scores":[revision]}});
        assert!(store.call("feedback",revised.clone()).await.unwrap_err().to_string().starts_with("waiting_dependency:"));
        store.call("feedback",initial).await.unwrap();
        store.call("feedback",revised).await.unwrap();
        let detail=store.call("decisions/get",json!({"id":"d"})).await.unwrap();
        assert_eq!(detail["feedback"].as_array().unwrap().len(),2);
    }

    #[tokio::test]
    async fn review_version_activation_and_deletion_invalidation_are_transactional() {
        let (_root,store)=recorded_store().await;
        store.call("decisions/begin",json!({"request_id":"r","id":"d","request":{"state":"rust migration"}})).await.unwrap();
        let review=store.call("reviews/create",json!({"id":"d","expected_revision":0,"status":"corrected","labels":{"complexity":"high","risk":"high","certainty":"clear"},"task_family":"migration","language":"en","reason":"migration risk"})).await.unwrap();
        let case_id=review["case_id"].as_str().unwrap();
        store.call("versions/create",json!({"id":"v1","case_ids":[case_id],"configuration":{}})).await.unwrap();
        assert!(store.call("versions/activate",json!({"id":"v1"})).await.unwrap_err().to_string().starts_with("invalid:"));
        store.call("versions/report",json!({"id":"v1","report":{"passed":true,"sample_count":10}})).await.unwrap();
        store.call("versions/activate",json!({"id":"v1"})).await.unwrap();
        store.call("decisions/begin",json!({"request_id":"r2","id":"d2","request":{"state":"readme cleanup"}})).await.unwrap();
        let review2=store.call("reviews/create",json!({"id":"d2","expected_revision":0,"status":"confirmed","labels":{"complexity":"low","risk":"low","certainty":"clear"},"reason":"confirmed"})).await.unwrap();
        store.call("versions/create",json!({"id":"never-activated","case_ids":[review2["case_id"]],"configuration":{}})).await.unwrap();
        store.call("versions/report",json!({"id":"never-activated","report":{"passed":true}})).await.unwrap();
        store.call("cases/delete",json!({"id":case_id})).await.unwrap();
        let settings=store.call("settings/get",json!({})).await.unwrap();
        assert!(settings["active_memory_version"].is_null());
        assert_eq!(store.call("versions/get",json!({"id":"v1"})).await.unwrap()["invalidated"],true);
        assert_eq!(store.call("versions/get",json!({"id":"never-activated"})).await.unwrap()["status"],"candidate");
    }

    #[tokio::test]
    async fn superseding_reviews_withdraw_old_cases_and_invalidate_versions() {
        let (_root,store)=recorded_store().await;
        store.call("decisions/begin",json!({"request_id":"r","id":"d","request":{"state":"migration"}})).await.unwrap();
        let first=store.call("reviews/create",json!({"id":"d","expected_revision":0,"status":"confirmed","labels":{"complexity":"medium","risk":"medium","certainty":"clear"},"reason":"first"})).await.unwrap();
        store.call("versions/create",json!({"id":"v","case_ids":[first["case_id"]],"configuration":{}})).await.unwrap();
        store.call("versions/report",json!({"id":"v","report":{"passed":true}})).await.unwrap();
        store.call("versions/activate",json!({"id":"v"})).await.unwrap();
        let corrected=store.call("reviews/create",json!({"id":"d","expected_revision":1,"status":"corrected","labels":{"complexity":"high","risk":"high","certainty":"clear"},"reason":"corrected"})).await.unwrap();
        let cases=store.call("cases/list",json!({"include_deleted":true})).await.unwrap();
        let old=cases["items"].as_array().unwrap().iter().find(|case|case["id"]==first["case_id"]).unwrap();
        assert_eq!(old["active"],false);
        assert!(cases["items"].as_array().unwrap().iter().any(|case|case["id"]==corrected["case_id"]&&case["active"]==true));
        assert_eq!(store.call("versions/get",json!({"id":"v"})).await.unwrap()["invalidated"],true);
        assert!(store.call("settings/get",json!({})).await.unwrap()["active_memory_version"].is_null());
        for (index,status) in ["excluded","insufficient"].iter().enumerate() {
            let id=format!("other-{index}");
            store.call("decisions/begin",json!({"request_id":id,"id":id,"request":{"state":"task"}})).await.unwrap();
            store.call("reviews/create",json!({"id":id,"expected_revision":0,"status":"confirmed","labels":{"complexity":"low","risk":"low","certainty":"clear"},"reason":"first"})).await.unwrap();
            store.call("reviews/create",json!({"id":id,"expected_revision":1,"status":status,"reason":"revoked"})).await.unwrap();
        }
        let cases=store.call("cases/list",json!({"include_deleted":true})).await.unwrap();
        for index in 0..2 { assert_eq!(cases["items"].as_array().unwrap().iter().find(|case|case["decision_id"]==format!("other-{index}")).unwrap()["active"],false); }
    }

    #[tokio::test]
    async fn candidate_evaluation_retrieval_uses_frozen_fts_cases_and_exclusions() {
        let (_root,store)=recorded_store().await;
        store.call("decisions/begin",json!({"request_id":"r","id":"d","request":{"state":"数据库迁移需要回滚计划"}})).await.unwrap();
        let review=store.call("reviews/create",json!({"id":"d","expected_revision":0,"status":"confirmed","labels":{"complexity":"high","risk":"high","certainty":"clear"},"task_family":"migration","language":"zh","reason":"confirmed"})).await.unwrap();
        let case_id=review["case_id"].as_str().unwrap();
        store.call("versions/create",json!({"id":"candidate","case_ids":[case_id],"configuration":{}})).await.unwrap();
        let found=store.call("memory/retrieve",json!({"query":"回滚 风险","version":"candidate","evaluation":true,"configuration":{"task_family":"migration","language":"zh"}})).await.unwrap();
        assert_eq!(found["cases"][0]["summary"],"数据库迁移需要回滚计划");
        let excluded=store.call("memory/retrieve",json!({"query":"回滚 风险","version":"candidate","evaluation":true,"exclude_task_families":["MIGRATION"],"configuration":{"language":"zh"}})).await.unwrap();
        assert_eq!(excluded["cases"].as_array().unwrap().len(),0);
    }

    #[tokio::test]
    async fn configuration_dependent_cases_require_exact_recorded_validation_context() {
        let (_root,store)=recorded_store().await;
        store.call("decisions/begin",json!({"request_id":"config","id":"config","request":{"state":"database rollback safety requirement"}})).await.unwrap();
        let source=json!({"host":"codex","role":"worker","actor_type":"agent"});
        let effective=json!({"model":"model-a","reasoning_effort":"high","model_observation":{"source":"host","verified":true,"observed_at":"2026-10-04T00:00:00Z","reference":"host:attempt","provider":"openai","model_revision":"revision-a","catalog_version":"catalog-a"}});
        let assignment=json!({"protocol_version":1,"event_id":"assignment-complete","decision_id":"config","attempt_ref":"attempt-complete","kind":"assignment","source":source,"payload":{"recommended":null,"selected":null,"requested":null,"effective":effective,"mixed_configuration":false,"environment":{"code_revision":"abc","tool_environment":"macos-arm64"},"reason":"recorded execution","evidence_refs":[]}});
        store.call("feedback",assignment).await.unwrap();
        let scored=json!({"protocol_version":1,"event_id":"score-complete","decision_id":"config","attempt_ref":"attempt-complete","kind":"review","source":{"host":"codex","role":"worker","actor_type":"agent"},"payload":{"disposition":"approved","scores":[score("initial",1,None)]}});
        store.call("feedback",scored).await.unwrap();
        let reviewed=store.call("reviews/create",json!({"id":"config","expected_revision":0,"status":"confirmed","labels":{"complexity":"high","risk":"high","certainty":"clear"},"task_family":"migration","language":"en","applicability":"configuration-dependent","applicability_reason":"validated on recorded attempt","validation_assignment_event_id":"assignment-complete","reason":"confirmed"})).await.unwrap();
        let case_id=reviewed["case_id"].as_str().unwrap();
        let cases=store.call("cases/list",json!({})).await.unwrap();
        let case=&cases["items"][0];
        assert_eq!(case["labels"],json!({"complexity":"high","risk":"high","certainty":"clear"}));
        assert_eq!(case["verification_status"],"verified");
        assert_eq!(case["validation_context"]["source"],json!({"host":"codex","role":"worker"}));
        assert_eq!(case["validation_context"]["effective"]["model_revision"],"revision-a");
        assert_eq!(case["validation_context"]["rubric_versions"],json!(["laya-feedback-v1"]));
        assert!(case["last_validated_at"].as_i64().is_some());
        let detail=store.call("decisions/get",json!({"id":"config"})).await.unwrap();
        assert_eq!(detail["reviews"][0]["applicability"],"configuration-dependent");
        assert_eq!(detail["reviews"][0]["validation_assignment_event_id"],"assignment-complete");
        store.call("versions/create",json!({"id":"config-version","case_ids":[case_id],"configuration":{}})).await.unwrap();
        let missing=store.call("memory/retrieve",json!({"query":"rollback safety","version":"config-version","evaluation":true,"configuration":{"task_family":"migration","language":"en"}})).await.unwrap();
        assert_eq!(missing["reason"],"applicability_unknown");
        assert_eq!(missing["applicability_exclusions"][0],json!({"case_id":case_id,"status":"unknown","reason":"missing_retrieval_validation_context"}));
        let mut mismatch=case["validation_context"].clone();
        mismatch["effective"]["model_revision"]=json!("revision-b");
        let stale=store.call("memory/retrieve",json!({"query":"rollback safety","version":"config-version","evaluation":true,"configuration":{"task_family":"migration","language":"en","validation_context":mismatch}})).await.unwrap();
        assert_eq!(stale["reason"],"needs_revalidation");
        assert_eq!(stale["applicability_exclusions"][0]["status"],"needs_revalidation");
        let matched=store.call("memory/retrieve",json!({"query":"rollback safety","version":"config-version","evaluation":true,"configuration":{"task_family":"migration","language":"en","validation_context":case["validation_context"].clone()}})).await.unwrap();
        assert_eq!(matched["reason"],"matched");
        assert_eq!(matched["cases"][0]["id"],case_id);
    }

    #[tokio::test]
    async fn incomplete_or_mixed_attempts_never_verify_configuration_cases() {
        let (_root,store)=recorded_store().await;
        let attempts=vec![
            ("unknown-revision",false,Value::Null,json!({"code_revision":"abc"})),
            ("mixed",true,Value::Null,json!({"code_revision":"abc"})),
            ("sentinel-identity",false,json!(" unknown "),json!({"code_revision":"abc"})),
            ("unknown-environment",false,json!("revision-a"),json!({"code_revision":null,"note":"arbitrary"})),
        ];
        for (id,mixed,model_revision,environment) in attempts {
            store.call("decisions/begin",json!({"request_id":id,"id":id,"request":{"state":"configuration result"}})).await.unwrap();
            let assignment=json!({"protocol_version":1,"event_id":format!("assignment-{id}"),"decision_id":id,"attempt_ref":format!("attempt-{id}"),"kind":"assignment","source":{"host":"codex","role":"worker","actor_type":"agent"},"payload":{"recommended":null,"selected":null,"requested":null,"effective":{"model":"model-a","reasoning_effort":"high","model_observation":{"source":"host","verified":true,"observed_at":"2026-10-04T00:00:00Z","reference":"host","provider":"openai","model_revision":model_revision,"catalog_version":"catalog-a"}},"mixed_configuration":mixed,"environment":environment,"reason":"recorded","evidence_refs":[]}});
            store.call("feedback",assignment).await.unwrap();
            let scored=json!({"protocol_version":1,"event_id":format!("score-{id}"),"decision_id":id,"attempt_ref":format!("attempt-{id}"),"kind":"review","source":{"host":"codex","role":"worker","actor_type":"agent"},"payload":{"disposition":"approved","scores":[score("initial",1,None)]}});
            store.call("feedback",scored).await.unwrap();
            store.call("reviews/create",json!({"id":id,"expected_revision":0,"status":"confirmed","labels":{"complexity":"medium","risk":"medium","certainty":"clear"},"applicability":"configuration-dependent","validation_assignment_event_id":format!("assignment-{id}"),"reason":"confirmed"})).await.unwrap();
        }
        let cases=store.call("cases/list",json!({})).await.unwrap();
        for case in cases["items"].as_array().unwrap() {
            assert_eq!(case["verification_status"],"unknown");
            assert!(case["last_validated_at"].is_null());
        }
        for id in ["unknown-revision","mixed","sentinel-identity"] {assert!(cases["items"].as_array().unwrap().iter().find(|case|case["decision_id"]==id).unwrap()["applicability_reason"].as_str().unwrap().contains("model_revision"));}
        assert!(cases["items"].as_array().unwrap().iter().find(|case|case["decision_id"]=="mixed").unwrap()["applicability_reason"].as_str().unwrap().contains("mixed_configuration"));
        assert!(cases["items"].as_array().unwrap().iter().find(|case|case["decision_id"]=="unknown-environment").unwrap()["applicability_reason"].as_str().unwrap().contains("environment"));
    }

    #[tokio::test]
    async fn schema_two_cases_migrate_without_promoting_legacy_configuration_evidence() {
        let root=tempdir().unwrap();
        {
            let connection=Connection::open(root.path().join("laya.sqlite3")).unwrap();
            connection.execute_batch(MIGRATION_1).unwrap();
            connection.execute_batch(MIGRATION_2).unwrap();
            connection.pragma_update(None,"user_version",2).unwrap();
            connection.execute("INSERT INTO decisions(id,request_id,request_json,recording_status,created_at) VALUES('task','task','{\"state\":\"task\"}','stored',1)",[]).unwrap();
            connection.execute("INSERT INTO reviews(decision_id,revision,status,labels_json,reason,actor_json,created_at) VALUES('task',1,'confirmed','{}','legacy','null',1)",[]).unwrap();
            connection.execute("INSERT INTO cases(id,decision_id,review_revision,content_json,labels_json,applicability,content_hash,created_at) VALUES('task:1','task',1,'\"task\"','{}','task-fact','task',1)",[]).unwrap();
            connection.execute("INSERT INTO decisions(id,request_id,request_json,recording_status,created_at) VALUES('config','config','{\"state\":\"config\"}','stored',1)",[]).unwrap();
            connection.execute("INSERT INTO reviews(decision_id,revision,status,labels_json,reason,actor_json,created_at) VALUES('config',1,'confirmed','{}','legacy','null',1)",[]).unwrap();
            connection.execute("INSERT INTO cases(id,decision_id,review_revision,content_json,labels_json,applicability,content_hash,created_at) VALUES('config:1','config',1,'\"config\"','{}','configuration-dependent','config',1)",[]).unwrap();
        }
        let store=Store::open(root.path()).unwrap();
        assert_eq!(store.call("status",json!({})).await.unwrap()["schema_version"],3);
        let cases=store.call("cases/list",json!({})).await.unwrap();
        let task=cases["items"].as_array().unwrap().iter().find(|case|case["id"]=="task:1").unwrap();
        let config=cases["items"].as_array().unwrap().iter().find(|case|case["id"]=="config:1").unwrap();
        assert_eq!(task["verification_status"],"verified");
        assert_eq!(config["verification_status"],"unknown");
        assert_eq!(config["applicability_reason"],"validation_unknown:legacy_missing_validation_metadata");
        assert!(root.path().join("backups").read_dir().unwrap().next().is_some());
    }

    #[tokio::test]
    async fn backup_restore_uses_verified_snapshot_and_safety_backup() {
        let (_root,store)=recorded_store().await;
        store.call("decisions/begin",json!({"request_id":"r1","id":"d1","request":{}})).await.unwrap();
        store.call("backup/create",json!({"id":"known"})).await.unwrap();
        store.call("decisions/delete",json!({"id":"d1"})).await.unwrap();
        store.call("decisions/begin",json!({"request_id":"r2","id":"d2","request":{}})).await.unwrap();
        let restored=store.call("backup/restore",json!({"id":"known"})).await.unwrap();
        assert_eq!(restored["restored"],true);
        assert!(store.call("decisions/get",json!({"id":"d1"})).await.unwrap_err().to_string().starts_with("not_found:"));
        assert_eq!(store.call("is_deleted",json!({"id":"d1"})).await.unwrap()["deleted"],true);
        assert!(store.call("decisions/get",json!({"id":"d2"})).await.unwrap_err().to_string().starts_with("not_found:"));
        let backups=store.call("backup/list",json!({})).await.unwrap();
        assert!(backups["items"].as_array().unwrap().iter().any(|backup|backup["id"]=="known"));
        assert!(backups["items"].as_array().unwrap().iter().any(|backup|backup["id"]==restored["safety_backup_id"]));
    }

    #[tokio::test]
    async fn backup_restore_does_not_resurrect_deleted_configuration_case_or_assignment() {
        let (_root,store)=recorded_store().await;
        store.call("decisions/begin",json!({"request_id":"private-config","id":"private-config","request":{"state":"private configuration evidence"}})).await.unwrap();
        let assignment=json!({"protocol_version":1,"event_id":"private-assignment","decision_id":"private-config","attempt_ref":"private-attempt","kind":"assignment","source":{"host":"codex","role":"worker","actor_type":"agent"},"payload":{"recommended":null,"selected":null,"requested":null,"effective":{"model":"model-a","reasoning_effort":"high","model_observation":{"source":"host","verified":true,"observed_at":"2026-10-04T00:00:00Z","reference":"host","provider":"openai","model_revision":"revision-a","catalog_version":"catalog-a"}},"mixed_configuration":false,"environment":{"code_revision":"abc"},"reason":"recorded","evidence_refs":[]}});
        store.call("feedback",assignment).await.unwrap();
        let scored=json!({"protocol_version":1,"event_id":"private-score","decision_id":"private-config","attempt_ref":"private-attempt","kind":"review","source":{"host":"codex","role":"worker","actor_type":"agent"},"payload":{"disposition":"approved","scores":[score("initial",1,None)]}});
        store.call("feedback",scored).await.unwrap();
        let review=store.call("reviews/create",json!({"id":"private-config","expected_revision":0,"status":"confirmed","labels":{"complexity":"medium","risk":"medium","certainty":"clear"},"applicability":"configuration-dependent","validation_assignment_event_id":"private-assignment","reason":"confirmed"})).await.unwrap();
        let case_id=review["case_id"].as_str().unwrap().to_string();
        assert_eq!(store.call("cases/list",json!({})).await.unwrap()["items"][0]["verification_status"],"verified");
        store.call("backup/create",json!({"id":"before-private-delete"})).await.unwrap();
        store.call("decisions/delete",json!({"id":"private-config"})).await.unwrap();
        let restored=store.call("backup/restore",json!({"id":"before-private-delete"})).await.unwrap();
        assert_eq!(restored["restored"],true);
        assert!(store.call("decisions/get",json!({"id":"private-config"})).await.unwrap_err().to_string().starts_with("not_found:"));
        let cases=store.call("cases/list",json!({"include_deleted":true})).await.unwrap();
        assert!(!cases["items"].as_array().unwrap().iter().any(|case|case["id"]==case_id));
    }

    #[tokio::test]
    async fn privacy_deletion_scrubs_derived_case_context_and_snapshots() {
        let (_root,store)=recorded_store().await;
        store.call("decisions/begin",json!({"request_id":"source","id":"source","request":{"state":"private example"}})).await.unwrap();
        let review=store.call("reviews/create",json!({"id":"source","expected_revision":0,"status":"confirmed","labels":{"complexity":"low","risk":"low","certainty":"clear"},"reason":"case"})).await.unwrap();
        let case_id=review["case_id"].as_str().unwrap();
        store.call("decisions/begin",json!({"request_id":"consumer","id":"consumer","request":{"state":"next task"}})).await.unwrap();
        store.call("decisions/finish",json!({"id":"consumer","result":{},"error":null,"context":{"memory":{"case_ids":[case_id],"cases":[{"id":case_id,"summary":"private example"}]}}})).await.unwrap();
        store.call("decisions/delete",json!({"id":"source"})).await.unwrap();
        let detail=store.call("decisions/get",json!({"id":"consumer"})).await.unwrap();
        assert!(detail["context"]["memory"]["case_ids"].as_array().unwrap().is_empty());
        assert!(detail["context"]["memory"]["cases"].as_array().unwrap().is_empty());
        assert_eq!(detail["context"]["evidence_withdrawn"][0],case_id);
        let snapshot=detail["snapshots"].as_array().unwrap().iter().find(|snapshot|snapshot["kind"]=="context").unwrap();
        assert_eq!(snapshot["capture_status"],"redacted");
        assert!(snapshot["payload"]["memory"]["cases"].as_array().unwrap().is_empty());
    }
}
