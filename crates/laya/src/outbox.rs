use crate::protocol::{hash, now, redact};
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{os::unix::fs::PermissionsExt, path::{Path, PathBuf}, time::Duration};

#[derive(Clone)]
pub struct Outbox { path: PathBuf }

impl Outbox {
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
        let this = Self { path: root.join("outbox.sqlite3") };
        let db = this.connection()?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS outbox(event_id TEXT PRIMARY KEY, decision_id TEXT NOT NULL, payload TEXT NOT NULL, hash TEXT NOT NULL, state TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, next_retry INTEGER NOT NULL DEFAULT 0, error TEXT, receipt TEXT, created_at INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS deleted_decisions(id TEXT PRIMARY KEY);")?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS snapshots(event_id TEXT PRIMARY KEY,decision_id TEXT NOT NULL,method TEXT NOT NULL,payload TEXT NOT NULL,hash TEXT NOT NULL,created_at INTEGER NOT NULL);")?;
        Ok(this)
    }

    fn connection(&self) -> Result<Connection> {
        if self.path.is_symlink() { bail!("symlinked outbox is not allowed"); }
        let db = Connection::open(&self.path)?;
        std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
        Ok(db)
    }

    pub fn enqueue(&self, event: &Value) -> Result<Value> {
        let id = event["event_id"].as_str().filter(|s| !s.is_empty() && s.len() <= 128).context("invalid event_id")?;
        let decision = event["decision_id"].as_str().context("missing decision_id")?;
        if serde_json::to_vec(event)?.len() > 16 * 1024 { bail!("feedback exceeds 16 KiB; provide a bounded evidence summary"); }
        let (clean, redacted) = redact(event);
        let digest = hash(&clean);
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if tx.query_row("SELECT 1 FROM deleted_decisions WHERE id=?", [decision], |_| Ok(())).optional()?.is_some() { bail!("deleted: feedback cannot resurrect deleted decision"); }
        if clean["kind"]=="run_manifest" {
            if let Some(segments)=clean["payload"]["segments"].as_array() {
                for segment in segments {
                    if let Some(reference)=segment["decision_id"].as_str() {
                        if tx.query_row("SELECT 1 FROM deleted_decisions WHERE id=?",[reference],|_|Ok(())).optional()?.is_some() {bail!("deleted: run manifest references deleted decision");}
                    }
                }
            }
        }
        let previous: Option<(String, String, Option<String>)> = tx.query_row("SELECT hash,state,receipt FROM outbox WHERE event_id=?", [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((old, state, receipt)) = previous {
            if old != digest { bail!("conflict: event_id already exists with different payload"); }
            return Ok(receipt.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({"event_id":id,"status":state,"payload_hash":digest})));
        }
        tx.execute("INSERT INTO outbox(event_id,decision_id,payload,hash,state,created_at) VALUES(?,?,?,?,'queued_local',?)", params![id,decision,clean.to_string(),digest,now()])?;
        tx.commit()?;
        Ok(json!({"event_id":id,"status":"queued_local","payload_hash":digest,"redacted":redacted}))
    }

    pub fn pending(&self) -> Result<Vec<Value>> {
        let db = self.connection()?;
        let mut stmt = db.prepare("SELECT payload FROM outbox WHERE state='queued_local' AND next_retry<=? ORDER BY created_at,rowid LIMIT 100")?;
        let rows = stmt.query_map([now()], |row| row.get::<_,String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn snapshot_enqueue(&self, method:&str, payload:&Value)->Result<Value> {
        if !["decisions/begin","decisions/finish"].contains(&method) {bail!("invalid: snapshot method");}
        if serde_json::to_vec(payload)?.len()>1024*1024 {bail!("invalid: snapshot exceeds 1 MiB");}
        let decision=payload["id"].as_str().context("invalid: snapshot decision id")?;
        let event_id=format!("{decision}:{method}");
        let clean=redact(payload).0;
        let digest=hash(&clean);
        let mut db=self.connection()?;
        let tx=db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if tx.query_row("SELECT 1 FROM deleted_decisions WHERE id=?",[decision],|_|Ok(())).optional()?.is_some() {bail!("deleted: snapshot cannot resurrect deleted decision");}
        let previous:Option<String>=tx.query_row("SELECT hash FROM snapshots WHERE event_id=?",[&event_id],|r|r.get(0)).optional()?;
        if let Some(previous)=previous {if previous!=digest {bail!("conflict: snapshot payload changed");}}
        else {tx.execute("INSERT INTO snapshots(event_id,decision_id,method,payload,hash,created_at) VALUES(?,?,?,?,?,?)",params![event_id,decision,method,clean.to_string(),digest,now()])?;}
        tx.commit()?;
        Ok(json!({"event_id":event_id,"method":method,"payload":clean,"payload_hash":digest}))
    }

    pub fn snapshot_pending(&self)->Result<Vec<Value>> {
        let db=self.connection()?;
        let mut statement=db.prepare("SELECT event_id,method,payload,hash FROM snapshots ORDER BY created_at,CASE method WHEN 'decisions/begin' THEN 0 ELSE 1 END,event_id LIMIT 32")?;
        let rows=statement.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?;
        rows.map(|row|{let(id,method,payload,digest)=row?;Ok(json!({"event_id":id,"method":method,"payload":serde_json::from_str::<Value>(&payload)?,"payload_hash":digest}))}).collect()
    }

    pub fn snapshot_ack(&self,event:&Value)->Result<()> {
        if hash(&event["payload"])!=event["payload_hash"] {bail!("invalid: snapshot receipt hash");}
        self.connection()?.execute("DELETE FROM snapshots WHERE event_id=? AND hash=?",params![event["event_id"].as_str(),event["payload_hash"].as_str()])?;
        Ok(())
    }

    pub fn acknowledge(&self, event: &Value, receipt: &Value) -> Result<()> {
        let id = event["event_id"].as_str().context("event id")?;
        if receipt["status"] != "stored" || receipt["event_id"] != id || receipt["payload_hash"] != hash(event) { bail!("invalid durable feedback receipt"); }
        self.connection()?.execute("UPDATE outbox SET state='acknowledged',receipt=?,payload='{}',error=NULL WHERE event_id=? AND hash=?", params![receipt.to_string(),id,hash(event)])?;
        Ok(())
    }

    pub fn failed(&self, id: &str, error: &str, permanent: bool) -> Result<()> {
        self.connection()?.execute("UPDATE outbox SET state=?,attempts=attempts+1,next_retry=?+min(300,5*(attempts+1)),error=? WHERE event_id=? AND state='queued_local'", params![if permanent {"quarantined"} else {"queued_local"},now(),error,id])?;
        Ok(())
    }

    pub fn delete_decision(&self, id: &str) -> Result<()> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute("INSERT OR IGNORE INTO deleted_decisions(id) VALUES(?)", [id])?;
        tx.execute("DELETE FROM outbox WHERE decision_id=?", [id])?;
        tx.execute("DELETE FROM outbox WHERE json_extract(payload,'$.kind')='run_manifest' AND EXISTS(SELECT 1 FROM json_each(outbox.payload,'$.payload.segments') segment WHERE CASE WHEN segment.type='object' THEN json_extract(segment.value,'$.decision_id') END=?1)",[id])?;
        tx.execute("DELETE FROM snapshots WHERE decision_id=?", [id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn withdraw_cases(&self,ids:&[String])->Result<()> {
        let withdrawn=ids.iter().cloned().collect();
        let mut db=self.connection()?;
        let tx=db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let entries={
            let mut statement=tx.prepare("SELECT event_id,payload FROM snapshots")?;
            let entries=statement.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            entries
        };
        for (id,text) in entries {
            let mut payload:Value=serde_json::from_str(&text)?;
            if crate::store::scrub_value(&mut payload,&withdrawn) {
                payload["capture_redacted"]=json!(true);
                if payload["context"].is_object() {payload["context"]["evidence_withdrawn"]=json!(ids);}
                tx.execute("UPDATE snapshots SET payload=?,hash=? WHERE event_id=?",params![payload.to_string(),hash(&payload),id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn status(&self) -> Result<Value> {
        let db = self.connection()?;
        let mut stmt = db.prepare("SELECT state,count(*),min(created_at) FROM outbox GROUP BY state")?;
        let rows = stmt.query_map([], |r| Ok(json!({"state":r.get::<_,String>(0)?,"count":r.get::<_,i64>(1)?,"oldest_at":r.get::<_,i64>(2)?})))?;
        let pending_snapshots:i64=db.query_row("SELECT count(*) FROM snapshots",[],|r|r.get(0))?;
        Ok(json!({"states":rows.collect::<rusqlite::Result<Vec<_>>>()?,"pending_snapshots":pending_snapshots}))
    }

    pub fn list(&self)->Result<Value> {
        let db=self.connection()?;
        let mut statement=db.prepare("SELECT event_id,decision_id,state,attempts,error,payload,hash,created_at FROM outbox WHERE state<>'acknowledged' ORDER BY created_at,rowid LIMIT 50")?;
        let items=statement.query_map([],|row|Ok(json!({"event_id":row.get::<_,String>(0)?,"decision_id":row.get::<_,String>(1)?,"state":row.get::<_,String>(2)?,"attempts":row.get::<_,i64>(3)?,"error":row.get::<_,Option<String>>(4)?,"payload":row.get::<_,String>(5)?,"payload_hash":row.get::<_,String>(6)?,"created_at":row.get::<_,i64>(7)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({"items":items,"limit":50}))
    }

    pub fn retry(&self,id:&str)->Result<Value> {
        let db=self.connection()?;
        let count=db.execute("UPDATE outbox SET state='queued_local',next_retry=0 WHERE event_id=? AND state IN('queued_local','quarantined')",[id])?;
        if count==0 {bail!("not_found: pending event");}
        Ok(json!({"event_id":id,"status":"queued_local","payload_changed":false}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event() -> Value { json!({"event_id":"e1","decision_id":"d1","payload":{"reason":"first judgment"}}) }
    #[test]
    fn survives_reopen_and_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let out = Outbox::open(dir.path()).unwrap();
        out.enqueue(&event()).unwrap();
        let reopened = Outbox::open(dir.path()).unwrap();
        assert_eq!(reopened.pending().unwrap(), vec![event()]);
        reopened.enqueue(&event()).unwrap();
        let mut changed = event(); changed["payload"]["reason"] = json!("rewritten");
        assert!(reopened.enqueue(&changed).is_err());
        assert_eq!(reopened.pending().unwrap().len(), 1);
    }
    #[test]
    fn ack_requires_matching_hash_and_delete_prevents_replay() {
        let dir = tempfile::tempdir().unwrap();
        let out = Outbox::open(dir.path()).unwrap();
        out.enqueue(&event()).unwrap();
        assert!(out.acknowledge(&event(),&json!({"event_id":"e1","status":"stored","payload_hash":"bad"})).is_err());
        out.acknowledge(&event(),&json!({"event_id":"e1","status":"stored","payload_hash":hash(&event())})).unwrap();
        assert!(out.pending().unwrap().is_empty());
        out.delete_decision("d1").unwrap();
        assert!(out.enqueue(&event()).is_err());
    }

    #[test]
    fn snapshots_survive_lost_ack_and_privacy_delete() {
        let dir=tempfile::tempdir().unwrap();
        let out=Outbox::open(dir.path()).unwrap();
        let payload=json!({"id":"d1","request_id":"d1","request":{"state":"task","api_key":"secret"}});
        let queued=out.snapshot_enqueue("decisions/begin",&payload).unwrap();
        assert!(!queued.to_string().contains("secret"));
        drop(out);
        let out=Outbox::open(dir.path()).unwrap();
        assert_eq!(out.snapshot_pending().unwrap(),vec![queued.clone()]);
        assert_eq!(out.snapshot_enqueue("decisions/begin",&payload).unwrap(),queued);
        let mut changed=payload.clone();changed["request"]["state"]=json!("changed");
        assert!(out.snapshot_enqueue("decisions/begin",&changed).is_err());
        out.snapshot_ack(&queued).unwrap();
        assert!(out.snapshot_pending().unwrap().is_empty());
        out.snapshot_enqueue("decisions/finish",&json!({"id":"d1","result":{"uncertain":true}})).unwrap();
        out.delete_decision("d1").unwrap();
        assert!(out.snapshot_pending().unwrap().is_empty());
        assert!(out.snapshot_enqueue("decisions/begin",&payload).is_err());
    }

    #[test]
    fn failed_commit_never_claims_queued() {
        let dir=tempfile::tempdir().unwrap();let out=Outbox::open(dir.path()).unwrap();
        out.connection().unwrap().execute_batch("CREATE TRIGGER simulate_full BEFORE INSERT ON outbox BEGIN SELECT RAISE(ABORT,'database or disk is full'); END;").unwrap();
        assert!(out.enqueue(&event()).is_err());
        assert!(out.pending().unwrap().is_empty());
    }

    #[test]
    fn manifest_privacy_delete_scrubs_cross_decision_queue_and_blocks_replay() {
        let dir=tempfile::tempdir().unwrap();
        let out=Outbox::open(dir.path()).unwrap();
        let manifest=json!({"event_id":"manifest","decision_id":"root","kind":"run_manifest","payload":{"segments":[{"decision_id":"root"},{"decision_id":"child"}]}});
        out.enqueue(&manifest).unwrap();
        out.enqueue(&event()).unwrap();
        out.failed("manifest","temporarily unavailable",true).unwrap();
        out.delete_decision("child").unwrap();
        assert!(out.retry("manifest").is_err());
        assert!(out.enqueue(&manifest).unwrap_err().to_string().contains("deleted"));
        drop(out);
        let out=Outbox::open(dir.path()).unwrap();
        assert_eq!(out.pending().unwrap(),vec![event()]);
        assert!(out.enqueue(&manifest).is_err());
    }

    #[test]
    fn manifest_acknowledgement_does_not_bypass_reference_tombstone() {
        let dir=tempfile::tempdir().unwrap();
        let out=Outbox::open(dir.path()).unwrap();
        let manifest=json!({"event_id":"manifest","decision_id":"root","kind":"run_manifest","payload":{"segments":[{"decision_id":"child"}]}});
        out.enqueue(&manifest).unwrap();
        out.acknowledge(&manifest,&json!({"event_id":"manifest","status":"stored","payload_hash":hash(&manifest)})).unwrap();
        out.delete_decision("child").unwrap();
        assert!(out.enqueue(&manifest).is_err());
    }

    #[test]
    fn malformed_manifest_segments_cannot_block_unrelated_privacy_delete() {
        let dir=tempfile::tempdir().unwrap();
        let out=Outbox::open(dir.path()).unwrap();
        for (index,segments) in [json!(["bad",null,17]),json!("bad"),json!({"bad":"text"})].into_iter().enumerate() {
            let id=format!("malformed-{index}");
            out.enqueue(&json!({"event_id":id,"decision_id":"other","kind":"run_manifest","payload":{"segments":segments}})).unwrap();
            out.failed(&id,"invalid: manifest",true).unwrap();
        }
        out.enqueue(&event()).unwrap();
        out.delete_decision("d1").unwrap();
        assert!(out.enqueue(&event()).is_err());
        assert!(out.pending().unwrap().is_empty());
    }
}
