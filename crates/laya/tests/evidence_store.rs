use laya::{protocol::hash,store::Store};
use rusqlite::{params,Connection};
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use std::{fs,os::unix::fs::{symlink,PermissionsExt},path::Path};

async fn recorded_store()->(tempfile::TempDir,Store) {
    let root=tempfile::tempdir().unwrap();
    let store=Store::open(root.path()).unwrap();
    store.call("settings/update",json!({"recording_enabled":true})).await.unwrap();
    (root,store)
}

fn large(marker:&str)->String {marker.repeat(20*1024/marker.len()+1)}

fn snapshot_artifact(root:&Path,decision_id:&str,kind:&str)->(String,String,String,u64) {
    Connection::open(root.join("laya.sqlite3")).unwrap().query_row(
        "SELECT s.payload_json,a.id,a.relative_path,a.size_bytes FROM decision_snapshots s JOIN artifacts a ON a.id=s.artifact_id WHERE s.decision_id=?1 AND s.kind=?2",
        params![decision_id,kind],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap()
}

#[tokio::test]
async fn large_snapshot_is_external_private_and_round_trips_exactly() {
    let (root,store)=recorded_store().await;
    let request=json!({"state":large("request-evidence-")});
    store.call("decisions/begin",json!({"request_id":"r","id":"d","request":request})).await.unwrap();
    let (inline,artifact_id,relative,size)=snapshot_artifact(root.path(),"d","request");
    assert_eq!(inline,"null");
    assert!(size>16*1024&&size<=1024*1024);
    let path=root.path().join(relative);
    let bytes=fs::read(&path).unwrap();
    assert_eq!(fs::metadata(root.path().join("evidence")).unwrap().permissions().mode()&0o777,0o700);
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode()&0o777,0o600);
    let (catalog_hash,snapshot_hash):(String,String)=Connection::open(root.path().join("laya.sqlite3")).unwrap().query_row("SELECT a.sha256,s.payload_hash FROM artifacts a JOIN decision_snapshots s ON s.artifact_id=a.id WHERE a.id=?1",[artifact_id],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
    assert_eq!(catalog_hash,format!("{:x}",Sha256::digest(&bytes)));
    assert_eq!(catalog_hash,snapshot_hash);
    assert_eq!(catalog_hash,hash(&request));
    let detail=store.call("decisions/get",json!({"id":"d"})).await.unwrap();
    assert_eq!(detail["request"],request);
    assert_eq!(detail["snapshots"][0]["payload"],request);
}

#[tokio::test]
async fn uncertain_external_snapshot_survives_retention() {
    let (root,store)=recorded_store().await;
    store.call("decisions/begin",json!({"request_id":"r","id":"d","request":{"state":"task"}})).await.unwrap();
    let result=json!({"uncertain":true,"evidence":large("uncertain-")});
    store.call("decisions/finish",json!({"id":"d","result":result,"error":null,"context":{}})).await.unwrap();
    let (_,_,relative,_)=snapshot_artifact(root.path(),"d","output");
    Connection::open(root.path().join("laya.sqlite3")).unwrap().execute("UPDATE decisions SET finished_at=0 WHERE id='d'",[]).unwrap();
    assert_eq!(store.call("retention",json!({"days":1})).await.unwrap()["removed"],0);
    assert!(root.path().join(relative).is_file());
    assert_eq!(store.call("decisions/get",json!({"id":"d"})).await.unwrap()["result"],result);
}

#[tokio::test]
async fn aborted_snapshot_transaction_cleans_orphan_file() {
    let (root,store)=recorded_store().await;
    Connection::open(root.path().join("laya.sqlite3")).unwrap().execute_batch("CREATE TRIGGER reject_snapshot BEFORE INSERT ON decision_snapshots BEGIN SELECT RAISE(ABORT,'injected snapshot failure'); END;").unwrap();
    let error=store.call("decisions/begin",json!({"request_id":"r","id":"d","request":{"state":large("orphan-")}})).await.unwrap_err();
    assert!(error.to_string().contains("injected snapshot failure"));
    let connection=Connection::open(root.path().join("laya.sqlite3")).unwrap();
    assert_eq!(connection.query_row::<i64,_,_>("SELECT COUNT(*) FROM artifacts",[],|row|row.get(0)).unwrap(),0);
    assert_eq!(connection.query_row::<i64,_,_>("SELECT COUNT(*) FROM decisions WHERE id='d'",[],|row|row.get(0)).unwrap(),0);
    assert!(fs::read_dir(root.path().join("evidence")).unwrap().next().is_none());
}

#[tokio::test]
async fn privacy_delete_removes_external_context() {
    let (root,store)=recorded_store().await;
    store.call("decisions/begin",json!({"request_id":"r","id":"d","request":{"state":"task"}})).await.unwrap();
    let context=json!({"trace":large("context-"),"api_key":"private"});
    store.call("decisions/finish",json!({"id":"d","result":{"advice":{"uncertain":false}},"error":null,"context":context})).await.unwrap();
    let (_,artifact_id,relative,_)=snapshot_artifact(root.path(),"d","context");
    let detail=store.call("decisions/get",json!({"id":"d"})).await.unwrap();
    assert_eq!(detail["context"]["api_key"],"[REDACTED]");
    assert!(root.path().join(&relative).is_file());
    store.call("decisions/delete",json!({"id":"d"})).await.unwrap();
    assert!(!root.path().join(relative).exists());
    assert_eq!(Connection::open(root.path().join("laya.sqlite3")).unwrap().query_row::<i64,_,_>("SELECT COUNT(*) FROM artifacts WHERE id=?1",[artifact_id],|row|row.get(0)).unwrap(),0);
}

#[tokio::test]
async fn backup_contains_evidence_but_old_restore_respects_deletion() {
    let (root,store)=recorded_store().await;
    let request=json!({"state":large("backup-")});
    store.call("decisions/begin",json!({"request_id":"r","id":"d","request":request})).await.unwrap();
    let (_,artifact_id,relative,_)=snapshot_artifact(root.path(),"d","request");
    store.call("backup/create",json!({"id":"known"})).await.unwrap();
    let archived:String=Connection::open(root.path().join("backups/known.sqlite3")).unwrap().query_row("SELECT payload_json FROM backup_evidence WHERE artifact_id=?1",[&artifact_id],|row|row.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&archived).unwrap(),request);
    store.call("decisions/delete",json!({"id":"d"})).await.unwrap();
    assert!(!root.path().join(&relative).exists());
    store.call("backup/restore",json!({"id":"known"})).await.unwrap();
    assert!(store.call("decisions/get",json!({"id":"d"})).await.unwrap_err().to_string().starts_with("not_found:"));
    assert_eq!(store.call("decisions/is_deleted",json!({"id":"d"})).await.unwrap()["deleted"],true);
    assert!(!root.path().join(relative).exists());
}

#[tokio::test]
async fn schema_one_database_migrates_and_reopens_with_evidence() {
    let root=tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("backups")).unwrap();
    let connection=Connection::open(root.path().join("laya.sqlite3")).unwrap();
    connection.execute_batch(include_str!(concat!(env!("CARGO_MANIFEST_DIR"),"/../../migrations/0001_workbench.sql"))).unwrap();
    connection.pragma_update(None,"user_version",1).unwrap();
    drop(connection);
    let store=Store::open(root.path()).unwrap();
    store.call("settings/update",json!({"recording_enabled":true})).await.unwrap();
    store.call("decisions/begin",json!({"request_id":"r","id":"d","request":{"state":large("migrated-")}})).await.unwrap();
    assert_eq!(store.call("status",json!({})).await.unwrap()["schema_version"],3);
    drop(store);
    let reopened=Store::open(root.path()).unwrap();
    assert!(reopened.call("decisions/get",json!({"id":"d"})).await.unwrap()["request"]["state"].as_str().unwrap().starts_with("migrated-"));
}

async fn assert_backup_repairs_live_evidence(damaged:Option<&[u8]>) {
    let (root,store)=recorded_store().await;
    let request=json!({"state":large("recover-")});
    store.call("decisions/begin",json!({"request_id":"r","id":"d","request":request})).await.unwrap();
    let (_,artifact_id,relative,_)=snapshot_artifact(root.path(),"d","request");
    store.call("backup/create",json!({"id":"known"})).await.unwrap();
    let path=root.path().join(&relative);
    match damaged {Some(bytes)=>fs::write(&path,bytes).unwrap(),None=>fs::remove_file(&path).unwrap()}
    let restored=store.call("backup/restore",json!({"id":"known"})).await.unwrap();
    assert_eq!(restored["safety_backup_complete"],false);
    assert_eq!(store.call("decisions/get",json!({"id":"d"})).await.unwrap()["snapshots"][0]["payload"],request);
    assert_eq!(fs::read(&path).unwrap(),serde_json::to_vec(&request).unwrap());
    let safety_id=restored["safety_backup_id"].as_str().unwrap();
    let backups=store.call("backup/list",json!({})).await.unwrap();
    let safety=backups["items"].as_array().unwrap().iter().find(|backup|backup["id"]==safety_id).unwrap();
    assert_eq!(safety["complete"],false);
    assert_eq!(safety["issues"][0]["artifact_id"],artifact_id);
    let archived:Option<Vec<u8>>=Connection::open(root.path().join(format!("backups/{safety_id}.sqlite3"))).unwrap().query_row("SELECT original_bytes FROM backup_evidence_damage WHERE artifact_id=?1",[artifact_id],|row|row.get(0)).unwrap();
    assert_eq!(archived.as_deref(),damaged);
}

#[tokio::test]
async fn backup_restore_repairs_missing_live_evidence_after_incomplete_safety_archive() {
    assert_backup_repairs_live_evidence(None).await;
}

#[tokio::test]
async fn backup_restore_repairs_tampered_live_evidence_and_preserves_damaged_bytes() {
    assert_backup_repairs_live_evidence(Some(b"damaged-live-evidence")).await;
}

#[tokio::test]
async fn schema_one_backup_is_migrated_during_restore() {
    let (root,store)=recorded_store().await;
    let path=root.path().join("backups/legacy.sqlite3");
    let legacy=Connection::open(&path).unwrap();
    legacy.execute_batch(include_str!(concat!(env!("CARGO_MANIFEST_DIR"),"/../../migrations/0001_workbench.sql"))).unwrap();
    legacy.pragma_update(None,"user_version",1).unwrap();
    legacy.execute("INSERT INTO decisions(id,request_id,request_json,recording_status,created_at) VALUES('legacy','legacy',?1,'stored',1)",[json!({"state":"legacy backup"}).to_string()]).unwrap();
    drop(legacy);
    let digest=format!("{:x}",Sha256::digest(fs::read(&path).unwrap()));
    Connection::open(root.path().join("laya.sqlite3")).unwrap().execute("INSERT INTO backups(id,relative_path,sha256,schema_version,created_at) VALUES('legacy','backups/legacy.sqlite3',?1,1,1)",[digest]).unwrap();
    store.call("backup/restore",json!({"id":"legacy"})).await.unwrap();
    assert_eq!(store.call("decisions/get",json!({"id":"legacy"})).await.unwrap()["request"]["state"],"legacy backup");
    assert_eq!(store.call("status",json!({})).await.unwrap()["schema_version"],3);
}

#[tokio::test]
async fn cleanup_failure_can_be_retried_without_duplicate_decision() {
    let (root,store)=recorded_store().await;
    store.call("decisions/begin",json!({"request_id":"r","id":"d","request":{"state":large("cleanup-")}})).await.unwrap();
    let (_,artifact_id,relative,_)=snapshot_artifact(root.path(),"d","request");
    let path=root.path().join(relative);
    fs::remove_file(&path).unwrap();
    let outside=tempfile::NamedTempFile::new().unwrap();
    fs::write(outside.path(),b"outside").unwrap();
    symlink(outside.path(),&path).unwrap();
    assert!(store.call("decisions/delete",json!({"id":"d"})).await.unwrap_err().to_string().starts_with("invalid:"));
    assert_eq!(store.call("status",json!({})).await.unwrap()["pending_evidence_cleanup"],1);
    assert_eq!(fs::read(outside.path()).unwrap(),b"outside");
    fs::remove_file(path).unwrap();
    assert_eq!(store.call("decisions/delete",json!({"id":"d"})).await.unwrap()["deleted"],true);
    assert_eq!(Connection::open(root.path().join("laya.sqlite3")).unwrap().query_row::<i64,_,_>("SELECT COUNT(*) FROM artifacts WHERE id=?1",[artifact_id],|row|row.get(0)).unwrap(),0);
    assert_eq!(fs::read(outside.path()).unwrap(),b"outside");
}

#[tokio::test]
async fn external_derived_case_context_stays_withdrawn_after_old_backup_restore() {
    let (root,store)=recorded_store().await;
    store.call("decisions/begin",json!({"request_id":"source","id":"source","request":{"state":"private reviewed example"}})).await.unwrap();
    let review=store.call("reviews/create",json!({"id":"source","expected_revision":0,"status":"confirmed","labels":{"complexity":"low","risk":"low","certainty":"clear"},"reason":"case"})).await.unwrap();
    let case_id=review["case_id"].as_str().unwrap();
    store.call("decisions/begin",json!({"request_id":"consumer","id":"consumer","request":{"state":"next task"}})).await.unwrap();
    let context=json!({"memory":{"case_ids":[case_id],"cases":[{"id":case_id,"summary":large("private-case-payload-")}]}});
    store.call("decisions/finish",json!({"id":"consumer","result":{"advice":{"uncertain":false}},"error":null,"context":context})).await.unwrap();
    let (_,old_artifact_id,old_relative,_)=snapshot_artifact(root.path(),"consumer","context");
    assert!(root.path().join(&old_relative).is_file());
    store.call("backup/create",json!({"id":"before-withdrawal"})).await.unwrap();
    store.call("decisions/delete",json!({"id":"source"})).await.unwrap();
    assert!(!root.path().join(&old_relative).exists());
    let scrubbed=store.call("decisions/get",json!({"id":"consumer"})).await.unwrap();
    assert!(scrubbed["context"]["memory"]["case_ids"].as_array().unwrap().is_empty());
    assert!(scrubbed["context"]["memory"]["cases"].as_array().unwrap().is_empty());
    assert_eq!(scrubbed["context"]["evidence_withdrawn"][0],case_id);
    let connection=Connection::open(root.path().join("laya.sqlite3")).unwrap();
    assert_eq!(connection.query_row::<i64,_,_>("SELECT COUNT(*) FROM artifacts WHERE id=?1",[&old_artifact_id],|row|row.get(0)).unwrap(),0);
    drop(connection);
    store.call("backup/restore",json!({"id":"before-withdrawal"})).await.unwrap();
    let restored=store.call("decisions/get",json!({"id":"consumer"})).await.unwrap();
    assert!(restored["context"]["memory"]["case_ids"].as_array().unwrap().is_empty());
    assert!(restored["context"]["memory"]["cases"].as_array().unwrap().is_empty());
    assert_eq!(restored["context"]["evidence_withdrawn"][0],case_id);
    assert!(store.call("decisions/get",json!({"id":"source"})).await.unwrap_err().to_string().starts_with("not_found:"));
    assert!(!root.path().join(old_relative).exists());
}

#[tokio::test]
async fn reopen_cleans_managed_crash_temps_and_preserves_normal_data() {
    let (root,store)=recorded_store().await;
    store.call("decisions/begin",json!({"request_id":"normal","id":"normal","request":{"state":"keep me"}})).await.unwrap();
    fs::create_dir(root.path().join("evidence")).unwrap();
    fs::set_permissions(root.path().join("evidence"),fs::Permissions::from_mode(0o700)).unwrap();
    let replace=root.path().join(format!("evidence/.replace-{}.tmp",uuid::Uuid::new_v4()));
    fs::write(&replace,b"private partial evidence").unwrap();
    fs::set_permissions(&replace,fs::Permissions::from_mode(0o600)).unwrap();
    let restore=root.path().join(format!(".restore-{}",uuid::Uuid::new_v4()));
    fs::create_dir(&restore).unwrap();
    fs::set_permissions(&restore,fs::Permissions::from_mode(0o700)).unwrap();
    let staged=restore.join("private.json");
    fs::write(&staged,b"private staged restore").unwrap();
    fs::set_permissions(&staged,fs::Permissions::from_mode(0o600)).unwrap();
    drop(store);
    let reopened=Store::open(root.path()).unwrap();
    assert!(!replace.exists());
    assert!(!restore.exists());
    assert_eq!(reopened.call("decisions/get",json!({"id":"normal"})).await.unwrap()["request"]["state"],"keep me");
}
