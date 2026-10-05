use laya::{outbox::Outbox,protocol::hash,store::Store};
use serde_json::{json,Value};

async fn recorded_store()->(tempfile::TempDir,Store) {
    let root=tempfile::tempdir().unwrap();
    let store=Store::open(root.path()).unwrap();
    store.call("settings/update",json!({"recording_enabled":true})).await.unwrap();
    store.call("decisions/begin",json!({"request_id":"decision","id":"decision","request":{"state":"execution receipt"}})).await.unwrap();
    (root,store)
}

fn source(role:&str)->Value {json!({"host":"codex","role":role,"actor_type":"agent"})}

fn pair(stage:&str)->Value {
    json!({"model":"fixture-model","reasoning_effort":"medium","model_observation":{
        "source":stage,"verified":stage=="host","observed_at":"2026-10-05T00:00:00Z","reference":"fixture"
    }})
}

fn dispatch_receipt(ordinal:u64)->Value {
    json!({
        "contract":"dispatch_receipt_v1","run_id":"run-1","stage_id":"stage-1",
        "ordinal":ordinal,"policy_version":"bounded-attempts-v1","enforcement":"advisory",
        "role":"worker","attempt_kind":"initial","status":"started","native_execution_ref":"native:child-1",
        "context_isolation":"isolated","context_evidence_ref":"context:snapshot-1",
        "input_size":{"value":120,"unit":"native_tokens","source":"host-meter"}
    })
}

fn assignment(event_id:&str,attempt_ref:&str,execution:Option<Value>)->Value {
    let mut payload=json!({
        "recommended":pair("laya"),"selected":pair("policy"),"requested":pair("spawn_request"),
        "effective":pair("host"),"reason":"bounded dispatch","evidence_refs":[]
    });
    if let Some(execution)=execution {payload["execution"]=execution;}
    json!({"protocol_version":1,"event_id":event_id,"decision_id":"decision","attempt_ref":attempt_ref,
           "kind":"assignment","source":source("orchestrator"),"payload":payload})
}

fn initial_review(event_id:&str,attempt_ref:&str)->Value {
    json!({"protocol_version":1,"event_id":event_id,"decision_id":"decision","attempt_ref":attempt_ref,
        "kind":"review","source":source("reviewer"),"payload":{"outcome":"approved","scores":[{
            "rubric_version":"laya-feedback-v1","dimension":"judgment_quality","value":2,
            "reason":"initial acceptance evidence","evidence_refs":[],"phase":"initial",
            "source_sequence":1,"observed_at":"2026-10-05T00:00:00Z"
        }]}})
}

fn test_event(event_id:&str,attempt_ref:&str)->Value {
    json!({"protocol_version":1,"event_id":event_id,"decision_id":"decision","attempt_ref":attempt_ref,
           "kind":"test","source":source("tester"),"payload":{"result":"pass","summary":"tests pass"}})
}

fn usage_event(event_id:&str,attempt_ref:&str)->Value {
    json!({"protocol_version":1,"event_id":event_id,"decision_id":"decision","attempt_ref":attempt_ref,
        "kind":"usage","source":source("worker"),"payload":{"total_tokens":42,"source":"native-meter",
        "source_verified":true,"scope":"attempt","checkpoint":"terminal","overlap_status":"non_overlapping"}})
}

fn outcome_receipt(dispatch:&str,first_feedback:Option<&str>,tests:Vec<&str>,reviews:Vec<&str>,usage:Vec<&str>)->Value {
    json!({
        "contract":"attempt_outcome_v1","run_id":"run-1","stage_id":"stage-1","ordinal":4,
        "policy_version":"bounded-attempts-v1","enforcement":"advisory",
        "dispatch_event_id":dispatch,"result":"success","failure_class":null,
        "first_feedback_event_id":first_feedback,"test_event_ids":tests,
        "review_event_ids":reviews,"usage_event_ids":usage,"duration_ms":250
    })
}

fn outcome(event_id:&str,attempt_ref:&str,execution:Option<Value>)->Value {
    let mut payload=json!({"outcome":"success","status":"success","summary":"bounded attempt completed"});
    if let Some(execution)=execution {payload["execution"]=execution;}
    json!({"protocol_version":1,"event_id":event_id,"decision_id":"decision","attempt_ref":attempt_ref,
           "kind":"outcome","source":source("worker"),"payload":payload})
}

async fn seed_references(store:&Store,attempt_ref:&str) {
    for event in [
        assignment("dispatch",attempt_ref,Some(dispatch_receipt(4))),
        initial_review("initial-feedback",attempt_ref),
        test_event("test-pass",attempt_ref),
        usage_event("usage-terminal",attempt_ref),
    ] {store.call("feedback",event).await.unwrap();}
}

#[tokio::test]
async fn legacy_feedback_and_execution_receipts_persist_without_changing_provenance() {
    let (_root,store)=recorded_store().await;
    store.call("feedback",assignment("legacy-assignment","legacy",None)).await.unwrap();
    store.call("feedback",outcome("legacy-outcome","legacy",None)).await.unwrap();
    seed_references(&store,"attempt-1").await;
    let receipt=outcome("terminal","attempt-1",Some(outcome_receipt(
        "dispatch",Some("initial-feedback"),vec!["test-pass"],vec!["initial-feedback"],vec!["usage-terminal"]
    )));
    let stored=store.call("feedback",receipt.clone()).await.unwrap();
    assert_eq!(stored["status"],"stored");
    let replay=store.call("feedback",receipt.clone()).await.unwrap();
    assert_eq!(replay["idempotent"],true);
    assert_eq!(replay["payload_hash"],hash(&receipt));

    let detail=store.call("decisions/get",json!({"id":"decision"})).await.unwrap();
    let dispatch=detail["feedback"].as_array().unwrap().iter().find(|item|item["event_id"]=="dispatch").unwrap();
    let terminal=detail["feedback"].as_array().unwrap().iter().find(|item|item["event_id"]=="terminal").unwrap();
    assert_eq!(dispatch["payload"]["execution"]["ordinal"],4);
    assert_eq!(dispatch["source"]["role"],"orchestrator");
    assert_eq!(dispatch["payload"]["execution"]["role"],"worker");
    assert_eq!(terminal["payload"]["execution"],receipt["payload"]["execution"]);
    let attempt=detail["execution_attempts"].as_array().unwrap().iter().find(|item|item["event_id"]=="dispatch").unwrap();
    assert_eq!(attempt["requested"],pair("spawn_request"));
    assert_eq!(attempt["effective"],pair("host"));
    assert_eq!(attempt["role"],"orchestrator");
    assert_eq!(detail["feedback"].as_array().unwrap().iter().filter(|item|item["event_id"]=="initial-feedback").count(),1);

    let duplicate=outcome("second-terminal","attempt-1",Some(outcome_receipt(
        "dispatch",Some("initial-feedback"),vec!["test-pass"],vec!["initial-feedback"],vec!["usage-terminal"]
    )));
    assert!(store.call("feedback",duplicate).await.unwrap_err().to_string().starts_with("conflict:"));
}

#[tokio::test]
async fn malformed_dispatch_and_outcome_extensions_are_rejected() {
    let (_root,store)=recorded_store().await;
    let mut cases=Vec::new();
    for (field,value) in [
        ("contract",json!("wrong")),("run_id",json!("")),("stage_id",json!("x".repeat(129))),
        ("ordinal",json!(0)),("ordinal",json!(1_000_001)),("ordinal",json!(1.5)),
        ("policy_version",json!("unbounded")),("enforcement",json!("execute")),
        ("role",json!("parent")),("attempt_kind",json!("retry")),("status",json!("complete")),
        ("native_execution_ref",Value::Null),("context_isolation",json!("isolated")),
    ] {
        let mut execution=dispatch_receipt(1);
        execution[field]=value;
        if field=="context_isolation" {execution["context_evidence_ref"]=Value::Null;}
        cases.push(execution);
    }
    let mut bad_input=dispatch_receipt(1);
    bad_input["input_size"]=json!({"value":10,"unit":"unknown","source":null});
    cases.push(bad_input);
    let mut unknown=dispatch_receipt(1);
    unknown["extra"]=json!(true);
    cases.push(unknown);
    let mut missing_role=dispatch_receipt(1);
    missing_role.as_object_mut().unwrap().remove("role");
    cases.push(missing_role);
    for (index,execution) in cases.into_iter().enumerate() {
        let event=assignment(&format!("bad-dispatch-{index}"),&format!("bad-{index}"),Some(execution));
        assert!(store.call("feedback",event).await.unwrap_err().to_string().starts_with("invalid:"),"case {index}");
    }

    seed_references(&store,"attempt-1").await;
    let mut outcomes=Vec::new();
    let mut mismatch=outcome_receipt("dispatch",Some("initial-feedback"),vec![],vec![],vec![]);
    mismatch["result"]=json!("failure");
    outcomes.push(("alias",mismatch,json!({"outcome":"success","status":"success"})));
    let mut failure=outcome_receipt("dispatch",Some("initial-feedback"),vec![],vec![],vec![]);
    failure["failure_class"]=json!("logic");
    outcomes.push(("failure-class",failure,json!({"outcome":"success","status":"success"})));
    let duplicate=outcome_receipt("dispatch",Some("initial-feedback"),vec!["test-pass","test-pass"],vec![],vec![]);
    outcomes.push(("duplicate-ref",duplicate,json!({"outcome":"success","status":"success"})));
    let mut too_many=outcome_receipt("dispatch",Some("initial-feedback"),vec![],vec![],vec![]);
    too_many["usage_event_ids"]=json!((0..33).map(|index|format!("usage-{index}")).collect::<Vec<_>>());
    outcomes.push(("too-many",too_many,json!({"outcome":"success","status":"success"})));
    let mut duration=outcome_receipt("dispatch",Some("initial-feedback"),vec![],vec![],vec![]);
    duration["duration_ms"]=json!(-1);
    outcomes.push(("duration",duration,json!({"outcome":"success","status":"success"})));
    for (label,execution,aliases) in outcomes {
        let mut event=outcome(&format!("bad-outcome-{label}"),"attempt-1",Some(execution));
        event["payload"]["outcome"]=aliases["outcome"].clone();
        event["payload"]["status"]=aliases["status"].clone();
        assert!(store.call("feedback",event).await.unwrap_err().to_string().starts_with("invalid:"),"{label}");
    }
}

#[tokio::test]
async fn versioned_outcome_requires_a_matching_legacy_alias_and_failed_alias_reaches_review_queue() {
    let (_root,store)=recorded_store().await;
    store.call("feedback",assignment("dispatch-alias","attempt-alias",Some(dispatch_receipt(1)))).await.unwrap();
    let mut changed_role=dispatch_receipt(1);
    changed_role["role"]=json!("reviewer");
    assert!(store.call("feedback",assignment("dispatch-role-change","attempt-alias",Some(changed_role)))
        .await.unwrap_err().to_string().starts_with("conflict: execution attempt role changed"));
    let mut execution=outcome_receipt("dispatch-alias",None,vec![],vec![],vec![]);
    execution["ordinal"]=json!(1);
    execution["result"]=json!("failure");
    execution["failure_class"]=json!("acceptance");

    let mut no_alias=outcome("no-alias","attempt-alias",Some(execution.clone()));
    no_alias["payload"].as_object_mut().unwrap().remove("outcome");
    no_alias["payload"].as_object_mut().unwrap().remove("status");
    assert!(store.call("feedback",no_alias).await.unwrap_err().to_string().starts_with("invalid:"));

    let mut failed=outcome("failed-alias","attempt-alias",Some(execution));
    failed["payload"].as_object_mut().unwrap().remove("outcome");
    failed["payload"]["status"]=json!("failure");
    assert_eq!(store.call("feedback",failed).await.unwrap()["status"],"stored");
    let queue=store.call("decisions/list",json!({"filter":{"status":"pending"}})).await.unwrap();
    let decision=queue["items"].as_array().unwrap().iter().find(|item|item["id"]=="decision").unwrap();
    assert!(decision["review_reasons"].as_array().unwrap().iter().any(|reason|reason=="outcome_failed"));
    assert!(decision["review_trigger_event_ids"].as_array().unwrap().iter().any(|event|event=="failed-alias"));
}

#[tokio::test]
async fn missing_dependencies_retry_exact_durable_payload_then_validate_identity_and_first_score() {
    let (root,store)=recorded_store().await;
    let event=outcome("terminal","attempt-1",Some(outcome_receipt(
        "dispatch",Some("initial-feedback"),vec!["test-pass"],vec!["initial-feedback"],vec!["usage-terminal"]
    )));
    let outbox=Outbox::open(root.path()).unwrap();
    assert_eq!(outbox.enqueue(&event).unwrap()["status"],"queued_local");
    assert!(store.call("feedback",event.clone()).await.unwrap_err().to_string().starts_with("waiting_dependency:"));
    assert_eq!(outbox.pending().unwrap(),vec![event.clone()]);

    seed_references(&store,"attempt-1").await;
    let receipt=store.call("feedback",outbox.pending().unwrap().remove(0)).await.unwrap();
    outbox.acknowledge(&event,&receipt).unwrap();
    assert!(outbox.pending().unwrap().is_empty());
    assert_eq!(receipt["payload_hash"],hash(&event));

    store.call("feedback",test_event("wrong-kind-ref","attempt-4")).await.unwrap();
    let wrong_kind=outcome("wrong-kind","attempt-4",Some(outcome_receipt(
        "wrong-kind-ref",None,vec![],vec![],vec![]
    )));
    assert!(store.call("feedback",wrong_kind).await.unwrap_err().to_string().contains("kind"));
    store.call("feedback",assignment("dispatch-2","attempt-2",Some(dispatch_receipt(1)))).await.unwrap();
    let no_score=test_event("feedback-without-score","attempt-2");
    store.call("feedback",no_score).await.unwrap();
    let mut missing_initial_execution=outcome_receipt(
        "dispatch-2",Some("feedback-without-score"),vec![],vec![],vec![]
    );
    missing_initial_execution["ordinal"]=json!(1);
    let missing_initial=outcome("missing-initial","attempt-2",Some(missing_initial_execution));
    assert!(store.call("feedback",missing_initial).await.unwrap_err().to_string().contains("initial score"));

    store.call("decisions/begin",json!({"request_id":"other","id":"other","request":{}})).await.unwrap();
    let mut other_dispatch=assignment("other-dispatch","attempt-3",Some(dispatch_receipt(1)));
    other_dispatch["decision_id"]=json!("other");
    store.call("feedback",other_dispatch).await.unwrap();
    let mut wrong_decision_execution=outcome_receipt("other-dispatch",None,vec![],vec![],vec![]);
    wrong_decision_execution["ordinal"]=json!(1);
    let wrong_decision=outcome("wrong-decision","attempt-3",Some(wrong_decision_execution));
    assert!(store.call("feedback",wrong_decision).await.unwrap_err().to_string().contains("does not match"));
}

#[tokio::test]
async fn recording_off_returns_not_recorded_without_dependency_lookup() {
    let root=tempfile::tempdir().unwrap();
    let store=Store::open(root.path()).unwrap();
    assert_eq!(store.call("decisions/begin",json!({"request_id":"decision","id":"decision","request":{"state":"private"}})).await.unwrap()["recording_status"],"not_recorded");
    let event=outcome("offline-terminal","offline-attempt",Some(outcome_receipt(
        "missing-dispatch",Some("missing-feedback"),vec!["missing-test"],vec!["missing-review"],vec!["missing-usage"]
    )));
    let receipt=store.call("feedback",event).await.unwrap();
    assert_eq!(receipt,json!({"status":"not_recorded","event_id":"offline-terminal"}));
}
