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

fn usage_event(id:&str,attempt:&str,tokens:Option<u64>,scope:&str,overlap:&str,ordered:Option<(&str,u64)>)->Value {
    let mut payload=json!({"total_tokens":tokens,"source":"host-test","source_verified":true,"scope":scope,"checkpoint":id,"overlap_status":overlap});
    if let Some((stream,sequence))=ordered {
        payload["aggregation"]=json!("cumulative");
        payload["usage_stream_id"]=json!(stream);
        payload["source_sequence"]=json!(sequence);
    }
    json!({"protocol_version":1,"event_id":id,"decision_id":"usage-d","attempt_ref":attempt,"kind":"usage","source":{"host":"test","role":"worker","actor_type":"agent"},"payload":payload})
}

#[tokio::test]
async fn dashboard_usage_deduplicates_checkpoints_and_excludes_inclusive_scopes() {
    let (_root,store)=recorded_store().await;
    assert!(store.call("status",json!({})).await.unwrap()["dashboard"]["tokens"]["recorded_total"].is_null());
    store.call("decisions/begin",json!({"id":"usage-d","request_id":"usage-d","request":{"state":"test"}})).await.unwrap();
    for event in [usage_event("latest","worker",Some(20),"attempt","non_overlapping",Some(("worker-stream",2))),usage_event("first","worker",Some(10),"attempt","non_overlapping",Some(("worker-stream",1))),usage_event("child","tester",Some(30),"attempt","non_overlapping",None),usage_event("parent","orchestrator",Some(100),"subtree","non_overlapping",None),usage_event("unknown","reviewer",Some(99),"attempt","unknown",None),usage_event("later-wrong-scope","worker",Some(999),"subtree","non_overlapping",None)] {
        store.call("feedback",event.clone()).await.unwrap();
        store.call("feedback",event).await.unwrap();
    }
    let status=store.call("status",json!({})).await.unwrap();
    assert_eq!(status["dashboard"]["tokens"]["recorded_total"],50);
    assert_eq!(status["dashboard"]["tokens"]["included_attempts"],2);
    assert_eq!(status["dashboard"]["tokens"]["excluded_reports"],3);
    assert_eq!(status["dashboard"]["tokens"]["aggregation_status"],"partial");
    assert_eq!(status["dashboard"]["tokens"]["exclusion_reasons"]["ineligible_scope"],2);
    assert!(status["dashboard"]["tokens"]["estimated_saved"].is_null());
}

#[tokio::test]
async fn overview_and_cases_use_inclusive_decision_created_cohorts_before_paging() {
    let (root,store)=recorded_store().await;
    let mut case_ids=Vec::new();
    for (index,(id,created_at,tokens)) in [("early",100,10),("start",200,20),("end",300,30)].into_iter().enumerate() {
        store.call("decisions/begin",json!({"id":id,"request_id":id,"request":{"state":id}})).await.unwrap();
        let review=store.call("reviews/create",json!({"id":id,"expected_revision":0,"status":"confirmed","labels":{"complexity":"low","risk":"low","certainty":"clear"},"reason":"confirmed"})).await.unwrap();
        case_ids.push(review["case_id"].as_str().unwrap().to_string());
        let mut usage=usage_event(&format!("usage-{id}"),id,Some(tokens),"attempt","non_overlapping",None);
        usage["decision_id"]=json!(id);
        store.call("feedback",usage).await.unwrap();
        let connection=Connection::open(root.path().join("laya.sqlite3")).unwrap();
        connection.execute("UPDATE decisions SET created_at=?2 WHERE id=?1",params![id,created_at]).unwrap();
        connection.execute("UPDATE cases SET created_at=?2 WHERE decision_id=?1",params![id,1000-index as i64]).unwrap();
    }
    let overview=store.call("overview/get",json!({"created_after":200,"created_before":300})).await.unwrap();
    assert_eq!(overview["dashboard"]["tokens"]["recorded_total"],50);
    assert_eq!(overview["dashboard"]["learning"]["reviewed_cases"],2);
    assert_eq!(overview["dashboard"]["time_scope"]["default"],"decision.created_at");
    assert_eq!(overview["counts"]["decisions"],2);
    assert_eq!(overview["counts"]["feedback"],2);
    assert_eq!(overview["counts"]["pending_reviews"],0);
    assert_eq!(overview["risk_counts"],json!({"low":0,"medium":0,"high":0,"unknown":2}));
    let status=store.call("status",json!({})).await.unwrap();
    assert_eq!(status["dashboard"]["tokens"]["recorded_total"],60);
    assert_eq!(status["counts"]["decisions"],3);
    assert_eq!(status["counts"]["feedback"],3);
    let first=store.call("cases/list",json!({"created_after":200,"created_before":300,"limit":1,"offset":0})).await.unwrap();
    let second=store.call("cases/list",json!({"created_after":200,"created_before":300,"limit":1,"offset":1})).await.unwrap();
    assert_eq!(first["items"][0]["decision_id"],"start");
    assert_eq!(second["items"][0]["decision_id"],"end");
    store.call("cases/delete",json!({"id":case_ids[2]})).await.unwrap();
    assert_eq!(store.call("cases/list",json!({"created_after":200,"created_before":300})).await.unwrap()["items"].as_array().unwrap().len(),1);
    assert_eq!(store.call("cases/list",json!({"created_after":200,"created_before":300,"include_deleted":true})).await.unwrap()["items"].as_array().unwrap().len(),2);
    assert!(store.call("overview/get",json!({"created_after":301,"created_before":300})).await.unwrap_err().to_string().starts_with("invalid:"));
    assert!(store.call("overview/get",json!({"unknown":1})).await.unwrap_err().to_string().starts_with("invalid:"));
}

#[tokio::test]
async fn dashboard_usage_excludes_conflicting_ordered_sequence() {
    let (_root,store)=recorded_store().await;
    store.call("decisions/begin",json!({"id":"usage-d","request_id":"usage-d","request":{}})).await.unwrap();
    store.call("feedback",usage_event("conflict-a","worker",Some(10),"attempt","non_overlapping",Some(("worker-stream",1)))).await.unwrap();
    store.call("feedback",usage_event("conflict-b","worker",Some(20),"attempt","non_overlapping",Some(("worker-stream",1)))).await.unwrap();
    let tokens=&store.call("status",json!({})).await.unwrap()["dashboard"]["tokens"];
    assert!(tokens["recorded_total"].is_null());
    assert_eq!(tokens["aggregation_status"],"unavailable");
    assert_eq!(tokens["exclusion_reasons"]["ordered_sequence_conflict"],2);
}

#[tokio::test]
async fn dashboard_usage_excludes_multiple_ordered_streams() {
    let (_root,store)=recorded_store().await;
    store.call("decisions/begin",json!({"id":"usage-d","request_id":"usage-d","request":{}})).await.unwrap();
    store.call("feedback",usage_event("stream-a","worker",Some(10),"attempt","non_overlapping",Some(("stream-a",1)))).await.unwrap();
    store.call("feedback",usage_event("stream-b","worker",Some(20),"attempt","non_overlapping",Some(("stream-b",1)))).await.unwrap();
    let status=store.call("status",json!({})).await.unwrap();
    assert!(status["dashboard"]["tokens"]["recorded_total"].is_null());
    assert_eq!(status["dashboard"]["tokens"]["exclusion_reasons"]["multiple_ordered_streams"],2);
}

#[tokio::test]
async fn dashboard_usage_excludes_ambiguous_legacy_reports() {
    let (_root,store)=recorded_store().await;
    store.call("decisions/begin",json!({"id":"usage-d","request_id":"usage-d","request":{}})).await.unwrap();
    store.call("feedback",usage_event("legacy-a","worker",Some(10),"attempt","non_overlapping",None)).await.unwrap();
    store.call("feedback",usage_event("legacy-b","worker",Some(20),"attempt","non_overlapping",None)).await.unwrap();
    let status=store.call("status",json!({})).await.unwrap();
    assert!(status["dashboard"]["tokens"]["recorded_total"].is_null());
    assert_eq!(status["dashboard"]["tokens"]["exclusion_reasons"]["legacy_reports_ambiguous"],2);
}

#[tokio::test]
async fn dashboard_usage_identical_legacy_replays_count_once_but_mixed_streams_do_not() {
    let (_root,store)=recorded_store().await;
    store.call("decisions/begin",json!({"id":"usage-d","request_id":"usage-d","request":{}})).await.unwrap();
    let legacy=usage_event("legacy","worker",Some(10),"attempt","non_overlapping",None);
    store.call("feedback",legacy.clone()).await.unwrap();
    let mut replay=legacy;
    replay["event_id"]=json!("legacy-new-envelope");
    store.call("feedback",replay).await.unwrap();
    assert_eq!(store.call("status",json!({})).await.unwrap()["dashboard"]["tokens"]["recorded_total"],10);
    store.call("feedback",usage_event("ordered","worker",Some(10),"attempt","non_overlapping",Some(("worker-stream",1)))).await.unwrap();
    let status=store.call("status",json!({})).await.unwrap();
    assert!(status["dashboard"]["tokens"]["recorded_total"].is_null());
    assert_eq!(status["dashboard"]["tokens"]["exclusion_reasons"]["mixed_ordered_and_legacy"],3);
}

#[tokio::test]
async fn dashboard_usage_latest_ordered_overlap_fails_closed() {
    let (_root,store)=recorded_store().await;
    store.call("decisions/begin",json!({"id":"usage-d","request_id":"usage-d","request":{}})).await.unwrap();
    store.call("feedback",usage_event("eligible","worker",Some(10),"attempt","non_overlapping",Some(("worker-stream",1)))).await.unwrap();
    store.call("feedback",usage_event("later-overlap","worker",None,"attempt","unknown",Some(("worker-stream",2)))).await.unwrap();
    let status=store.call("status",json!({})).await.unwrap();
    assert!(status["dashboard"]["tokens"]["recorded_total"].is_null());
    assert_eq!(status["dashboard"]["tokens"]["exclusion_reasons"]["overlap_not_non_overlapping"],1);
}

#[tokio::test]
async fn dashboard_usage_excludes_stream_reused_across_attempts() {
    let (_root,store)=recorded_store().await;
    store.call("decisions/begin",json!({"id":"usage-d","request_id":"usage-d","request":{}})).await.unwrap();
    store.call("feedback",usage_event("reuse-a","worker-a",Some(10),"attempt","non_overlapping",Some(("reused-stream",1)))).await.unwrap();
    store.call("feedback",usage_event("reuse-b","worker-b",Some(10),"attempt","non_overlapping",Some(("reused-stream",1)))).await.unwrap();
    let status=store.call("status",json!({})).await.unwrap();
    assert!(status["dashboard"]["tokens"]["recorded_total"].is_null());
    assert_eq!(status["dashboard"]["tokens"]["exclusion_reasons"]["usage_stream_reused"],2);
}

#[tokio::test]
async fn dashboard_usage_reports_display_overflow_without_rounding() {
    let (_root,store)=recorded_store().await;
    store.call("decisions/begin",json!({"id":"usage-d","request_id":"usage-d","request":{}})).await.unwrap();
    store.call("feedback",usage_event("max-safe","worker",Some(9_007_199_254_740_991),"attempt","non_overlapping",Some(("worker-stream",1)))).await.unwrap();
    store.call("feedback",usage_event("plus-one","tester",Some(1),"attempt","non_overlapping",Some(("tester-stream",1)))).await.unwrap();
    let status=store.call("status",json!({})).await.unwrap();
    assert!(status["dashboard"]["tokens"]["recorded_total"].is_null());
    assert_eq!(status["dashboard"]["tokens"]["aggregation_status"],"overflow");
    assert_eq!(status["dashboard"]["tokens"]["included_attempts"],2);
}

#[tokio::test]
async fn dashboard_usage_preserves_unknown_data_as_null() {
    let (_root,store)=recorded_store().await;
    store.call("decisions/begin",json!({"id":"usage-d","request_id":"usage-d","request":{}})).await.unwrap();
    store.call("feedback",usage_event("unknown-total","worker",None,"attempt","non_overlapping",None)).await.unwrap();
    let status=store.call("status",json!({})).await.unwrap();
    assert!(status["dashboard"]["tokens"]["recorded_total"].is_null());
    assert_eq!(status["dashboard"]["tokens"]["aggregation_status"],"unavailable");
    assert_eq!(status["dashboard"]["tokens"]["exclusion_reasons"]["unknown_total_tokens"],1);
}

#[tokio::test]
async fn ordered_usage_rejects_partial_or_total_less_stream_metadata() {
    let (_root,store)=recorded_store().await;
    store.call("decisions/begin",json!({"id":"usage-d","request_id":"usage-d","request":{}})).await.unwrap();
    let mut partial=usage_event("partial","worker",Some(10),"attempt","non_overlapping",None);
    partial["payload"]["usage_stream_id"]=json!("worker-stream");
    assert!(store.call("feedback",partial).await.unwrap_err().to_string().contains("requires aggregation"));
    let mut total_less=usage_event("total-less","worker",None,"attempt","non_overlapping",Some(("worker-stream",1)));
    total_less["payload"].as_object_mut().unwrap().remove("total_tokens");
    total_less["payload"]["input_tokens"]=json!(10);
    assert!(store.call("feedback",total_less).await.unwrap_err().to_string().contains("explicit scope=attempt total"));
}

#[tokio::test]
async fn workbench_tiers_and_human_preference_preserve_risk_and_original_evidence() {
    let (_root,store)=recorded_store().await;
    let tiers=json!({"low":{"model":"test-model","reasoning_effort":"low"}});
    let settings=store.call("settings/update",json!({"model_tiers":tiers})).await.unwrap();
    assert_eq!(settings["model_tiers"],tiers);
    assert!(store.call("settings/update",json!({"model_tiers":{"unexpected":{}}})).await.is_err());
    assert!(store.call("settings/update",json!({"model_tiers":{"low":{"model":"test-model"}}})).await.is_err());
    store.call("decisions/begin",json!({"id":"tier-review","request_id":"tier-review","request":{"state":"Production migration"}})).await.unwrap();
    let original=json!({"advice":{"assessment":{"risk":{"choice":"high"}}}});
    store.call("decisions/finish",json!({"id":"tier-review","result":original})).await.unwrap();
    store.call("reviews/create",json!({"id":"tier-review","expected_revision":0,"status":"confirmed","labels":{"complexity":"high","risk":"high","certainty":"clear","model_tier":"low"},"reason":"Human preference; not risk evidence"})).await.unwrap();
    let detail=store.call("decisions/get",json!({"id":"tier-review"})).await.unwrap();
    assert_eq!(detail["result"],original);
    assert_eq!(detail["reviews"][0]["labels"]["model_tier"],"low");
    assert_eq!(detail["reviews"][0]["labels"]["risk"],"high");
    let cases=store.call("cases/list",json!({})).await.unwrap();
    assert_eq!(cases["items"][0]["labels"]["model_tier"],"low");
    let status=store.call("status",json!({})).await.unwrap();
    assert_eq!(status["risk_counts"]["high"],1);
    assert_eq!(status["risk_counts"]["low"],0);
}

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
    assert_eq!(store.call("status",json!({})).await.unwrap()["schema_version"],4);
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
    assert_eq!(store.call("status",json!({})).await.unwrap()["schema_version"],4);
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
