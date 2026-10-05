use laya::{protocol::hash,store::Store};
use rusqlite::{params,Connection};
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use std::{fs,time::Instant};

async fn recorded_store()->(tempfile::TempDir,Store) {
    let root=tempfile::tempdir().unwrap();
    let store=Store::open(root.path()).unwrap();
    store.call("settings/update",json!({"recording_enabled":true})).await.unwrap();
    (root,store)
}

async fn begin(store:&Store,id:&str) {
    store.call("decisions/begin",json!({"request_id":id,"id":id,"request":{"state":id,"advisor":{"task_family":"testing"}}})).await.unwrap();
}

fn source(role:&str)->Value {json!({"host":"codex","role":role,"actor_type":"agent"})}

fn pair(verified:bool)->Value {
    json!({"model":"fixture-model","reasoning_effort":"medium","model_observation":{
        "source":if verified{"host"}else{"policy"},"verified":verified,
        "observed_at":"2026-10-05T00:00:00Z","reference":"fixture"
    }})
}

fn dispatch(run:&str,stage:&str,ordinal:u64,attempt_kind:&str)->Value {
    json!({"contract":"dispatch_receipt_v1","run_id":run,"stage_id":stage,"ordinal":ordinal,
        "policy_version":"bounded-attempts-v1","enforcement":"advisory","role":"worker",
        "attempt_kind":attempt_kind,"status":"started","native_execution_ref":format!("native:{run}:{stage}:{ordinal}"),
        "context_isolation":"isolated","context_evidence_ref":format!("context:{run}:{stage}:{ordinal}"),
        "input_size":{"value":120,"unit":"native_tokens","source":"host-meter"}})
}

fn assignment(event:&str,decision:&str,attempt:&str,effective:Value,execution:Option<Value>)->Value {
    let mut payload=json!({"recommended":pair(false),"selected":pair(false),"requested":pair(false),
        "effective":effective,"reason":"dispatch","evidence_refs":[]});
    if let Some(execution)=execution {payload["execution"]=execution;}
    json!({"protocol_version":1,"event_id":event,"decision_id":decision,"attempt_ref":attempt,
        "kind":"assignment","source":source("orchestrator"),"payload":payload})
}

fn usage(event:&str,decision:&str,attempt:&str,stream:&str,tokens:u64)->Value {
    json!({"protocol_version":1,"event_id":event,"decision_id":decision,"attempt_ref":attempt,
        "kind":"usage","source":source("worker"),"payload":{"total_tokens":tokens,
        "source":"native-meter","source_verified":true,"scope":"attempt","checkpoint":"terminal",
        "overlap_status":"non_overlapping","aggregation":"cumulative","usage_stream_id":stream,"source_sequence":1}})
}

fn score(event:&str,decision:&str,attempt:&str,phase:&str,sequence:u64,value:u64,supersedes:Option<&str>)->Value {
    let mut item=json!({"rubric_version":"laya-feedback-v1","dimension":"judgment_quality","value":value,
        "reason":"quality evidence","evidence_refs":[],"phase":phase,"source_sequence":sequence,
        "observed_at":"2026-10-05T00:00:00Z"});
    if let Some(id)=supersedes {item["supersedes_event_id"]=json!(id);}
    json!({"protocol_version":1,"event_id":event,"decision_id":decision,"attempt_ref":attempt,
        "kind":"review","source":source("reviewer"),"payload":{"outcome":"approved","scores":[item]}})
}

fn outcome(event:&str,decision:&str,attempt:&str,dispatch_event:&str,result:&str,failure:Option<&str>)->Value {
    json!({"protocol_version":1,"event_id":event,"decision_id":decision,"attempt_ref":attempt,
        "kind":"outcome","source":source("worker"),"payload":{"outcome":result,"status":result,
        "execution":{"contract":"attempt_outcome_v1","run_id":"run-main","stage_id":"stage-main","ordinal":1,
            "policy_version":"bounded-attempts-v1","enforcement":"advisory","dispatch_event_id":dispatch_event,
            "result":result,"failure_class":failure,"first_feedback_event_id":null,"test_event_ids":[],
            "review_event_ids":[],"usage_event_ids":[],"duration_ms":250}}})
}

fn observation<'a>(detail:&'a Value,attempt:&str)->&'a Value {
    detail["efficiency_observations"].as_array().unwrap().iter().find(|item|item["attempt_ref"]==attempt).unwrap()
}

fn file_hash(path:&std::path::Path)->String {
    format!("{:x}",Sha256::digest(fs::read(path).unwrap()))
}

#[tokio::test]
async fn observations_separate_revisions_and_accept_late_usage_and_outcome() {
    let (_root,store)=recorded_store().await;
    begin(&store,"decision").await;
    store.call("feedback",assignment("dispatch","decision","attempt",pair(true),Some(dispatch("run-main","stage-main",1,"repair")))).await.unwrap();
    store.call("feedback",score("initial","decision","attempt","initial",1,2,None)).await.unwrap();
    store.call("feedback",score("revision","decision","attempt","revision",2,1,Some("initial"))).await.unwrap();

    let before=store.call("decisions/get",json!({"id":"decision"})).await.unwrap();
    let before=observation(&before,"attempt");
    assert_eq!(before["contract"],"efficiency_observation_v1");
    assert_eq!(before["identity"]["status"],"reported");
    assert_eq!(before["configuration"]["status"],"reported_verified");
    assert_eq!(before["configuration"]["effective"],pair(true));
    assert_eq!(before["quality"]["initial_scores"].as_array().unwrap().len(),1);
    assert_eq!(before["quality"]["revised_scores"].as_array().unwrap().len(),1);
    assert_eq!(before["quality"]["revised_scores"][0]["supersedes_event_id"],"initial");
    assert_eq!(before["quality"]["flagged"],true);
    assert!(before["outcome"]["event_id"].is_null());
    assert_eq!(before["usage"]["status"],"unavailable");
    assert_eq!(before["complete_task_coverage"],false);

    store.call("feedback",usage("usage","decision","attempt","stream-main",42)).await.unwrap();
    store.call("feedback",outcome("terminal","decision","attempt","dispatch","failure",Some("acceptance"))).await.unwrap();
    let detail=store.call("decisions/get",json!({"id":"decision"})).await.unwrap();
    let observed=observation(&detail,"attempt");
    assert_eq!(observed["usage"]["status"],"available");
    assert_eq!(observed["usage"]["event_id"],"usage");
    assert_eq!(observed["usage"]["report"]["total_tokens"],42);
    assert_eq!(observed["outcome"],json!({"event_id":"terminal","result":"failure","failure_class":"acceptance","duration_ms":250}));

    let efficiency=&store.call("status",json!({})).await.unwrap()["dashboard"]["efficiency"];
    assert_eq!(efficiency["observed_attempts"],1);
    assert_eq!(efficiency["versioned_attempts"],1);
    assert_eq!(efficiency["reported_runs"],1);
    assert_eq!(efficiency["delegated_attempts"],1);
    assert_eq!(efficiency["repair_attempts"],1);
    assert_eq!(efficiency["upgrade_attempts"],0);
    assert_eq!(efficiency["initial_scored_attempts"],1);
    assert_eq!(efficiency["flagged_attempts"],1);
    assert_eq!(efficiency["reported_outcomes"]["failure"],1);
    assert_eq!(efficiency["usage_covered_attempts"],1);
    assert_eq!(efficiency["usage_missing_attempts"],0);
}

#[tokio::test]
async fn identity_and_usage_ambiguity_are_scoped_across_decisions() {
    let (_root,store)=recorded_store().await;
    for decision in ["one","two"] {begin(&store,decision).await;}
    for (decision,event,effective) in [("one","dispatch-one",Value::Null),("two","dispatch-two",pair(true))] {
        store.call("feedback",assignment(event,decision,"shared-attempt",effective,Some(dispatch("reused-run","reused-stage",1,"initial")))).await.unwrap();
        store.call("feedback",usage(&format!("usage-{decision}"),decision,"shared-attempt","reused-stream",10)).await.unwrap();
    }
    for decision in ["one","two"] {
        let detail=store.call("decisions/get",json!({"id":decision})).await.unwrap();
        assert_eq!(detail["efficiency_observations"].as_array().unwrap().len(),1);
        let item=observation(&detail,"shared-attempt");
        assert_eq!(item["decision_id"],decision);
        assert_eq!(item["identity"]["status"],"conflict");
        assert_eq!(item["usage"]["status"],"unavailable");
        assert_eq!(item["usage"]["exclusion_reasons"]["usage_stream_reused"],1);
    }
    let unknown=store.call("decisions/get",json!({"id":"one"})).await.unwrap();
    assert_eq!(observation(&unknown,"shared-attempt")["configuration"],json!({"status":"unknown","effective":null,"assignment_event_ids":["dispatch-one"]}));
    let efficiency=&store.call("status",json!({})).await.unwrap()["dashboard"]["efficiency"];
    assert_eq!(efficiency["observed_attempts"],2);
    assert_eq!(efficiency["identity_conflicts"],2);
    assert_eq!(efficiency["versioned_attempts"],0);
    assert_eq!(efficiency["reported_runs"],0);
    assert_eq!(efficiency["usage_covered_attempts"],0);
    assert_eq!(efficiency["usage_missing_attempts"],2);
}

#[tokio::test]
async fn flagged_matches_existing_review_queue_triggers() {
    let (_root,store)=recorded_store().await;
    begin(&store,"flags").await;
    let attempts=["score-one","proposed-risk","blocked","modified","partial"];
    for attempt in attempts {
        store.call("feedback",assignment(&format!("assignment-{attempt}"),"flags",attempt,pair(true),None)).await.unwrap();
    }
    store.call("feedback",score("score-one-event","flags","score-one","initial",1,1,None)).await.unwrap();
    for (event,attempt,payload) in [
        ("risk-event","proposed-risk",json!({"outcome":"approved","proposed_labels":{"risk":"high"}})),
        ("blocked-event","blocked",json!({"outcome":"blocked"})),
    ] {store.call("feedback",json!({"protocol_version":1,"event_id":event,"decision_id":"flags","attempt_ref":attempt,"kind":"review","source":source("reviewer"),"payload":payload})).await.unwrap();}
    store.call("feedback",json!({"protocol_version":1,"event_id":"modified-event","decision_id":"flags","attempt_ref":"modified","kind":"user_choice","source":source("orchestrator"),"payload":{"choice":"modified"}})).await.unwrap();
    store.call("feedback",json!({"protocol_version":1,"event_id":"partial-event","decision_id":"flags","attempt_ref":"partial","kind":"outcome","source":source("worker"),"payload":{"outcome":"partial"}})).await.unwrap();
    let detail=store.call("decisions/get",json!({"id":"flags"})).await.unwrap();
    for attempt in attempts {assert_eq!(observation(&detail,attempt)["quality"]["flagged"],true,"{attempt}");}
    assert_eq!(store.call("status",json!({})).await.unwrap()["dashboard"]["efficiency"]["flagged_attempts"],5);
}

#[tokio::test]
async fn efficiency_cohorts_are_inclusive_and_exclude_deleted_decisions() {
    let (root,store)=recorded_store().await;
    for (id,created) in [("early",100),("start",200),("end",300)] {
        begin(&store,id).await;
        store.call("feedback",assignment(&format!("assignment-{id}"),id,id,pair(true),None)).await.unwrap();
        Connection::open(root.path().join("laya.sqlite3")).unwrap().execute("UPDATE decisions SET created_at=?2 WHERE id=?1",params![id,created]).unwrap();
    }
    let scoped=store.call("overview/get",json!({"created_after":200,"created_before":300})).await.unwrap();
    assert_eq!(scoped["dashboard"]["efficiency"]["observed_attempts"],2);
    store.call("decisions/delete",json!({"id":"end"})).await.unwrap();
    assert_eq!(store.call("overview/get",json!({"created_after":200,"created_before":300})).await.unwrap()["dashboard"]["efficiency"]["observed_attempts"],1);
    assert_eq!(store.call("status",json!({})).await.unwrap()["dashboard"]["efficiency"]["observed_attempts"],2);
    assert!(store.call("decisions/get",json!({"id":"end"})).await.unwrap_err().to_string().starts_with("not_found:"));
}

#[tokio::test]
async fn schema_four_migrates_with_event_hash_and_pre_migration_backup_preserved() {
    let root=tempfile::tempdir().unwrap();
    let event=assignment("legacy-event","legacy","attempt",pair(true),None);
    let expected_hash=hash(&event);
    {
        let store=Store::open(root.path()).unwrap();
        store.call("settings/update",json!({"recording_enabled":true})).await.unwrap();
        begin(&store,"legacy").await;
        assert_eq!(store.call("feedback",event).await.unwrap()["payload_hash"],expected_hash);
    }
    let path=root.path().join("laya.sqlite3");
    let connection=Connection::open(&path).unwrap();
    connection.execute_batch("DROP INDEX IF EXISTS feedback_attempt_sequence_idx; DROP INDEX IF EXISTS feedback_usage_stream_idx; DROP INDEX IF EXISTS feedback_execution_scope_idx; PRAGMA user_version=4; UPDATE settings SET value_json=json_set(value_json,'$.schema_version',4);").unwrap();
    drop(connection);

    let store=Store::open(root.path()).unwrap();
    assert_eq!(store.call("status",json!({})).await.unwrap()["schema_version"],5);
    let detail=store.call("decisions/get",json!({"id":"legacy"})).await.unwrap();
    assert_eq!(detail["feedback"][0]["payload_hash"],expected_hash);
    let indexes=Connection::open(&path).unwrap().query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name IN('feedback_attempt_sequence_idx','feedback_usage_stream_idx','feedback_execution_scope_idx')",[],|row|row.get::<_,u64>(0)).unwrap();
    assert_eq!(indexes,3);
    let backups=fs::read_dir(root.path().join("backups")).unwrap().map(|entry|entry.unwrap().path()).collect::<Vec<_>>();
    assert_eq!(backups.len(),1);
    let listed=store.call("backup/list",json!({})).await.unwrap();
    assert_eq!(listed["items"].as_array().unwrap().len(),1);
    let managed=&listed["items"][0];
    let id=managed["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("pre-migration-"));
    assert_eq!(managed["schema_version"],4);
    assert_eq!(managed["complete"],true);
    assert_eq!(managed["sha256"],file_hash(&backups[0]));
    let (relative,digest,schema):(String,String,u64)=Connection::open(&path).unwrap().query_row(
        "SELECT relative_path,sha256,schema_version FROM backups WHERE id=?1",[&id],
        |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
    assert_eq!(relative,format!("backups/{id}.sqlite3"));
    assert_eq!(root.path().join(&relative),backups[0]);
    assert_eq!(digest,file_hash(&backups[0]));
    assert_eq!(schema,4);
    let events=store.call("events",json!({"after":0,"limit":1000})).await.unwrap();
    assert!(events["items"].as_array().unwrap().iter().any(|event|
        event["kind"]=="backup.created"&&event["payload"]==json!({"id":id,"reason":"pre_migration"})));
    let backup=Connection::open(&backups[0]).unwrap();
    assert_eq!(backup.query_row::<u64,_,_>("PRAGMA user_version",[],|row|row.get(0)).unwrap(),4);
    assert_eq!(backup.query_row::<String,_,_>("SELECT event_hash FROM feedback_events WHERE event_id='legacy-event'",[],|row|row.get(0)).unwrap(),expected_hash);
    drop(backup);

    begin(&store,"after-migration").await;
    assert_eq!(store.call("backup/restore",json!({"id":id})).await.unwrap()["id"],id);
    assert_eq!(store.call("status",json!({})).await.unwrap()["schema_version"],5);
    let restored=store.call("decisions/get",json!({"id":"legacy"})).await.unwrap();
    assert_eq!(restored["feedback"][0]["payload_hash"],expected_hash);
    assert!(store.call("decisions/get",json!({"id":"after-migration"})).await.unwrap_err().to_string().starts_with("not_found:"));
    assert_eq!(file_hash(&backups[0]),digest);

    assert_eq!(store.call("backup/delete",json!({"id":id})).await.unwrap()["deleted"],true);
    assert!(!backups[0].exists());
    assert!(root.path().join("backups").is_dir());
    let remaining=store.call("backup/list",json!({})).await.unwrap();
    assert!(remaining["items"].as_array().unwrap().iter().all(|backup|backup["id"]!=id));
}

#[tokio::test]
async fn fresh_database_does_not_create_a_pre_migration_backup() {
    let root=tempfile::tempdir().unwrap();
    let store=Store::open(root.path()).unwrap();
    assert_eq!(store.call("status",json!({})).await.unwrap()["schema_version"],5);
    assert!(store.call("backup/list",json!({})).await.unwrap()["items"].as_array().unwrap().is_empty());
    assert_eq!(fs::read_dir(root.path().join("backups")).unwrap().count(),0);
    let events=store.call("events",json!({"after":0,"limit":1000})).await.unwrap();
    assert!(events["items"].as_array().unwrap().iter().all(|event|event["kind"]!="backup.created"));
}

#[tokio::test]
#[ignore = "diagnostic scalability measurement"]
async fn efficiency_summary_scalability_measurement() {
    for count in [100usize,1000] {
        let (_root,store)=recorded_store().await;
        for index in 0..count {
            let id=format!("decision-{index}");
            begin(&store,&id).await;
            store.call("feedback",assignment(&format!("assignment-{index}"),&id,"attempt",pair(true),None)).await.unwrap();
        }
        let started=Instant::now();
        assert_eq!(store.call("status",json!({})).await.unwrap()["dashboard"]["efficiency"]["observed_attempts"],count);
        eprintln!("efficiency summary {count} decisions: {:?}",started.elapsed());
    }
}
