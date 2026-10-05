use laya::store::Store;
use rusqlite::{params,Connection};
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use std::{fs,path::{Path,PathBuf}};

fn large(marker:&str)->String {marker.repeat(20*1024/marker.len()+1)}

fn snapshot(root:&Path,decision:&str,kind:&str)->(String,PathBuf,String,String) {
    let (id,relative,artifact_hash,payload_hash):(String,String,String,String)=Connection::open(root.join("laya.sqlite3")).unwrap().query_row(
        "SELECT a.id,a.relative_path,a.sha256,s.payload_hash FROM decision_snapshots s JOIN artifacts a ON a.id=s.artifact_id WHERE s.decision_id=?1 AND s.kind=?2",
        params![decision,kind],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
    (id,root.join(relative),artifact_hash,payload_hash)
}

fn assert_snapshot(detail:&Value,kind:&str,payload:&Value,id:&str,digest:&str) {
    let item=detail["snapshots"].as_array().unwrap().iter().find(|item|item["kind"]==kind).unwrap();
    assert_eq!(item["artifact_id"],id);
    assert_eq!(item["payload_hash"],digest);
    assert_eq!(&item["payload"],payload);
}

#[tokio::test]
async fn automatic_migration_backup_restores_external_evidence_without_reviving_privacy_deletion() {
    let root=tempfile::tempdir().unwrap();
    let request=json!({"state":large("migration-request-")});
    let output=json!({"advice":{"uncertain":false},"evidence":large("migration-output-")});
    {
        let store=Store::open(root.path()).unwrap();
        store.call("settings/update",json!({"recording_enabled":true})).await.unwrap();
        store.call("decisions/begin",json!({"request_id":"external","id":"external","request":request})).await.unwrap();
        store.call("decisions/finish",json!({"id":"external","result":output,"error":null,"context":{}})).await.unwrap();
    }

    let (request_id,request_path,request_hash,request_payload_hash)=snapshot(root.path(),"external","request");
    let (output_id,output_path,output_hash,output_payload_hash)=snapshot(root.path(),"external","output");
    assert_eq!(request_hash,request_payload_hash);
    assert_eq!(output_hash,output_payload_hash);
    let request_bytes=fs::read(&request_path).unwrap();
    let output_bytes=fs::read(&output_path).unwrap();
    assert_eq!(format!("{:x}",Sha256::digest(&request_bytes)),request_hash);
    assert_eq!(format!("{:x}",Sha256::digest(&output_bytes)),output_hash);

    let database=root.path().join("laya.sqlite3");
    Connection::open(&database).unwrap().execute_batch(
        "DROP INDEX IF EXISTS feedback_attempt_sequence_idx;
         DROP INDEX IF EXISTS feedback_usage_stream_idx;
         DROP INDEX IF EXISTS feedback_execution_scope_idx;
         PRAGMA user_version=4;
         UPDATE settings SET value_json=json_set(value_json,'$.schema_version',4);"
    ).unwrap();
    let store=Store::open(root.path()).unwrap();
    let listed=store.call("backup/list",json!({})).await.unwrap();
    let managed=listed["items"].as_array().unwrap().iter().find(|item|
        item["id"].as_str().is_some_and(|id|id.starts_with("pre-migration-"))).unwrap();
    let backup_id=managed["id"].as_str().unwrap().to_string();
    assert_eq!(managed["schema_version"],4);
    let backup_path=root.path().join(format!("backups/{backup_id}.sqlite3"));
    let archived=Connection::open(&backup_path).unwrap();
    for (id,payload) in [(&request_id,&request),(&output_id,&output)] {
        let text:String=archived.query_row("SELECT payload_json FROM backup_evidence WHERE artifact_id=?1",[id],|row|row.get(0)).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(),*payload);
    }
    drop(archived);

    fs::remove_file(&request_path).unwrap();
    fs::write(&output_path,b"damaged external output").unwrap();
    assert_eq!(store.call("backup/restore",json!({"id":backup_id})).await.unwrap()["restored"],true);
    assert_eq!(store.call("status",json!({})).await.unwrap()["schema_version"],5);
    let detail=store.call("decisions/get",json!({"id":"external"})).await.unwrap();
    assert_eq!(detail["request"],request);
    assert_eq!(detail["result"],output);
    assert_snapshot(&detail,"request",&request,&request_id,&request_hash);
    assert_snapshot(&detail,"output",&output,&output_id,&output_hash);
    assert_eq!(fs::read(&request_path).unwrap(),request_bytes);
    assert_eq!(fs::read(&output_path).unwrap(),output_bytes);
    let connection=Connection::open(&database).unwrap();
    for (id,digest) in [(&request_id,&request_hash),(&output_id,&output_hash)] {
        let restored:String=connection.query_row("SELECT sha256 FROM artifacts WHERE id=?1",[id],|row|row.get(0)).unwrap();
        assert_eq!(&restored,digest);
    }
    drop(connection);

    store.call("decisions/delete",json!({"id":"external"})).await.unwrap();
    assert!(!request_path.exists());
    assert!(!output_path.exists());
    assert_eq!(store.call("backup/restore",json!({"id":backup_id})).await.unwrap()["restored"],true);
    assert!(store.call("decisions/get",json!({"id":"external"})).await.unwrap_err().to_string().starts_with("not_found:"));
    assert_eq!(store.call("decisions/is_deleted",json!({"id":"external"})).await.unwrap()["deleted"],true);
    assert!(!request_path.exists());
    assert!(!output_path.exists());
    let remaining:i64=Connection::open(&database).unwrap().query_row(
        "SELECT COUNT(*) FROM artifacts WHERE id IN (?1,?2)",params![request_id,output_id],|row|row.get(0)).unwrap();
    assert_eq!(remaining,0);
}
