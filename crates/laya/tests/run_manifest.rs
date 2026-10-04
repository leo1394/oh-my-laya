use laya::store::Store;
use rusqlite::Connection;
use serde_json::{json,Value};

async fn recorded_store()->(tempfile::TempDir,Store) {
    let root=tempfile::tempdir().unwrap();
    let store=Store::open(root.path()).unwrap();
    store.call("settings/update",json!({"recording_enabled":true})).await.unwrap();
    (root,store)
}

async fn begin(store:&Store,id:&str) {
    store.call("decisions/begin",json!({"id":id,"request_id":id,"request":{"state":id}})).await.unwrap();
}

fn usage(event_id:&str,decision_id:&str,attempt_ref:&str)->Value {
    json!({"protocol_version":1,"event_id":event_id,"decision_id":decision_id,"attempt_ref":attempt_ref,"kind":"usage","source":{"host":"test","role":"worker","actor_type":"agent"},"payload":{"total_tokens":10,"source":"native-test","source_verified":true,"scope":"attempt","checkpoint":"terminal","overlap_status":"non_overlapping"}})
}

fn outcome(event_id:&str,decision_id:&str,attempt_ref:&str)->Value {
    json!({"protocol_version":1,"event_id":event_id,"decision_id":decision_id,"attempt_ref":attempt_ref,"kind":"outcome","source":{"host":"test","role":"worker","actor_type":"agent"},"payload":{"outcome":"success"}})
}

fn manifest()->Value {
    json!({
        "protocol_version":1,
        "event_id":"manifest-event",
        "decision_id":"root",
        "attempt_ref":"root-attempt",
        "kind":"run_manifest",
        "source":{"host":"test","role":"orchestrator","actor_type":"agent"},
        "payload":{
            "manifest_version":1,
            "run_id":"host-run-1",
            "host_root_ref":"host-root-1",
            "mode":"laya",
            "observed_at":"2026-10-04T00:00:00Z",
            "terminal_checkpoint":"host-terminal-1",
            "context":{
                "task_snapshot_hash":"task-hash",
                "code_revision":"revision-1",
                "test_snapshot_hash":null,
                "tool_environment_id":"tools-1",
                "external_inputs_hash":null,
                "acceptance_policy":"acceptance-1"
            },
            "meter":{"unit":"tokens","identity":"provider-meter-v1","scope":"host_only","excluded_components":["local_laya_inference"]},
            "segments":[
                {"decision_id":"root","attempt_ref":"root-attempt","usage_event_id":"root-usage"},
                {"decision_id":"child","attempt_ref":"child-attempt","usage_event_id":"child-usage"}
            ],
            "outcome_event_ids":["child-outcome"],
            "coverage":{"status":"complete","evidence_refs":["host-ledger-1"]}
        }
    })
}

async fn seed_references(store:&Store) {
    begin(store,"root").await;
    begin(store,"child").await;
    store.call("feedback",usage("root-usage","root","root-attempt")).await.unwrap();
    store.call("feedback",usage("child-usage","child","child-attempt")).await.unwrap();
    store.call("feedback",outcome("child-outcome","child","child-attempt")).await.unwrap();
}

#[tokio::test]
async fn manifest_persists_restarts_and_retries_idempotently() {
    let (root,store)=recorded_store().await;
    seed_references(&store).await;
    let event=manifest();
    let first=store.call("feedback",event.clone()).await.unwrap();
    assert_eq!(first["status"],"stored");
    drop(store);
    let reopened=Store::open(root.path()).unwrap();
    let replay=reopened.call("feedback",event).await.unwrap();
    assert_eq!(replay["idempotent"],true);
    let detail=reopened.call("decisions/get",json!({"id":"root"})).await.unwrap();
    let saved=detail["feedback"].as_array().unwrap().iter().find(|event|event["kind"]=="run_manifest").unwrap();
    assert_eq!(saved["payload"]["manifest_version"],1);
    assert_eq!(saved["payload"]["coverage"]["status"],"complete");
}

#[tokio::test]
async fn overview_excludes_manifest_when_any_referenced_decision_is_outside_cohort() {
    let (root,store)=recorded_store().await;
    seed_references(&store).await;
    let mut event=manifest();
    event["payload"]["scenario"]=json!({"orchestrator_model":"gpt-test","reasoning_effort":"high","initial_context_tokens":100,"stages":[{"context_growth_tokens":50,"work_output_tokens":20,"passes":2}],"input_source":"declared-test"});
    store.call("feedback",event).await.unwrap();
    let connection=Connection::open(root.path().join("laya.sqlite3")).unwrap();
    connection.execute("UPDATE decisions SET created_at=200 WHERE id='root'",[]).unwrap();
    connection.execute("UPDATE decisions SET created_at=400 WHERE id='child'",[]).unwrap();
    let filtered=store.call("overview/get",json!({"created_after":200,"created_before":300})).await.unwrap();
    assert_eq!(filtered["dashboard"]["tokens"]["recorded_total"],10);
    assert_eq!(filtered["dashboard"]["tokens"]["scenario"]["included_runs"],0);
    assert_eq!(store.call("status",json!({})).await.unwrap()["dashboard"]["tokens"]["scenario"]["included_runs"],1);
}

#[tokio::test]
async fn manifest_rejects_malformed_contract_fields_and_duplicates() {
    let (_root,store)=recorded_store().await;
    seed_references(&store).await;
    let mut cases=Vec::new();
    let mut extra=manifest();
    extra["event_id"]=json!("bad-extra");
    extra["payload"]["total_tokens"]=json!(10);
    cases.push(extra);
    let mut missing=manifest();
    missing["event_id"]=json!("bad-missing");
    missing["payload"]["context"].as_object_mut().unwrap().remove("acceptance_policy");
    cases.push(missing);
    let mut meter=manifest();
    meter["event_id"]=json!("bad-meter");
    meter["payload"]["meter"]["excluded_components"]=json!([]);
    cases.push(meter);
    let mut duplicate=manifest();
    duplicate["event_id"]=json!("bad-duplicate");
    duplicate["payload"]["segments"][1]=duplicate["payload"]["segments"][0].clone();
    cases.push(duplicate);
    let mut empty_evidence=manifest();
    empty_evidence["event_id"]=json!("bad-evidence");
    empty_evidence["payload"]["coverage"]["evidence_refs"]=json!([]);
    cases.push(empty_evidence);
    let mut long_string=manifest();
    long_string["event_id"]=json!("bad-long-string");
    long_string["payload"]["run_id"]=json!("x".repeat(513));
    cases.push(long_string);
    let mut too_many=manifest();
    too_many["event_id"]=json!("bad-segment-count");
    too_many["payload"]["segments"]=Value::Array((0..129).map(|index|json!({"decision_id":"root","attempt_ref":format!("attempt-{index}"),"usage_event_id":format!("usage-{index}")})).collect());
    cases.push(too_many);
    let scenario=json!({"orchestrator_model":"gpt-test","reasoning_effort":null,"initial_context_tokens":100,"stages":[{"context_growth_tokens":50,"work_output_tokens":20,"passes":2}],"input_source":"declared-test"});
    let mut scenario_extra=manifest();
    scenario_extra["event_id"]=json!("bad-scenario-extra");
    scenario_extra["payload"]["scenario"]=scenario.clone();
    scenario_extra["payload"]["scenario"]["unexpected"]=json!(true);
    cases.push(scenario_extra);
    let mut scenario_limit=manifest();
    scenario_limit["event_id"]=json!("bad-scenario-limit");
    scenario_limit["payload"]["scenario"]=scenario.clone();
    scenario_limit["payload"]["scenario"]["initial_context_tokens"]=json!(1_000_000_001u64);
    cases.push(scenario_limit);
    let mut scenario_empty=manifest();
    scenario_empty["event_id"]=json!("bad-scenario-empty");
    scenario_empty["payload"]["scenario"]=scenario.clone();
    scenario_empty["payload"]["scenario"]["stages"]=json!([]);
    cases.push(scenario_empty);
    let mut scenario_passes=manifest();
    scenario_passes["event_id"]=json!("bad-scenario-passes");
    scenario_passes["payload"]["scenario"]=scenario;
    scenario_passes["payload"]["scenario"]["stages"][0]["passes"]=json!(0);
    cases.push(scenario_passes);
    for event in cases {
        assert!(store.call("feedback",event).await.unwrap_err().to_string().starts_with("invalid:"));
    }
}

#[tokio::test]
async fn manifest_rejects_mismatched_nonexistent_and_deleted_references() {
    let (_root,store)=recorded_store().await;
    seed_references(&store).await;
    let mut mismatch=manifest();
    mismatch["event_id"]=json!("mismatch");
    mismatch["payload"]["segments"][1]["attempt_ref"]=json!("wrong-attempt");
    assert!(store.call("feedback",mismatch).await.unwrap_err().to_string().contains("does not match its segment identity"));
    let mut missing=manifest();
    missing["event_id"]=json!("missing");
    missing["payload"]["segments"][1]["usage_event_id"]=json!("absent-usage");
    assert!(store.call("feedback",missing).await.unwrap_err().to_string().starts_with("waiting_dependency:"));
    store.call("feedback",outcome("other-outcome","root","unlisted-attempt")).await.unwrap();
    let mut wrong_outcome=manifest();
    wrong_outcome["event_id"]=json!("wrong-outcome");
    wrong_outcome["payload"]["outcome_event_ids"]=json!(["other-outcome"]);
    assert!(store.call("feedback",wrong_outcome).await.unwrap_err().to_string().contains("does not match a segment identity"));
    store.call("feedback",manifest()).await.unwrap();
    store.call("decisions/delete",json!({"id":"child"})).await.unwrap();
    let detail=store.call("decisions/get",json!({"id":"root"})).await.unwrap();
    assert!(detail["feedback"].as_array().unwrap().iter().all(|event|event["kind"]!="run_manifest"));
    assert!(store.call("feedback",manifest()).await.unwrap_err().to_string().starts_with("not_found:"));
}

#[tokio::test]
async fn manifest_recording_off_returns_existing_receipt_without_reference_lookup() {
    let root=tempfile::tempdir().unwrap();
    let store=Store::open(root.path()).unwrap();
    assert_eq!(store.call("decisions/begin",json!({"id":"root","request_id":"root","request":{"state":"private"}})).await.unwrap()["recording_status"],"not_recorded");
    let receipt=store.call("feedback",manifest()).await.unwrap();
    assert_eq!(receipt["status"],"not_recorded");
    assert_eq!(receipt["event_id"],"manifest-event");
}

#[test]
fn mcp_schema_exposes_the_exact_manifest_constants() {
    let tools=laya::mcp::tools();
    let schema=&tools["tools"][2]["inputSchema"];
    assert!(schema["properties"]["kind"]["enum"].as_array().unwrap().contains(&json!("run_manifest")));
    let payload=&schema["$defs"]["runManifestPayload"];
    assert_eq!(payload["properties"]["manifest_version"]["const"],1);
    assert_eq!(payload["properties"]["meter"]["properties"]["unit"]["const"],"tokens");
    assert_eq!(payload["properties"]["meter"]["properties"]["scope"]["const"],"host_only");
    assert_eq!(payload["properties"]["meter"]["properties"]["excluded_components"]["const"],json!(["local_laya_inference"]));
    assert_eq!(payload["properties"]["segments"]["maxItems"],128);
    assert_eq!(payload["properties"]["outcome_event_ids"]["maxItems"],128);
    assert_eq!(payload["properties"]["scenario"]["$ref"],"#/$defs/runScenario");
    assert_eq!(schema["$defs"]["runScenario"]["properties"]["stages"]["maxItems"],128);
    assert_eq!(schema["$defs"]["runScenario"]["properties"]["stages"]["items"]["properties"]["passes"]["maximum"],100);
}
