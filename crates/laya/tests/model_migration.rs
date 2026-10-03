use laya::{protocol::hash,store::Store};
use serde_json::{json,Value};

async fn recorded_store()->(tempfile::TempDir,Store) {
    let root=tempfile::tempdir().unwrap();
    let store=Store::open(root.path()).unwrap();
    store.call("settings/update",json!({"recording_enabled":true})).await.unwrap();
    (root,store)
}

fn observation(source:&str,verified:bool,catalog:&str,observed_at:&str)->Value {
    json!({"source":source,"verified":verified,"observed_at":observed_at,"reference":"migration-regression","provider":"openai","model_revision":null,"catalog_version":catalog})
}

fn pair(model:&str,effort:Option<&str>,source:&str,verified:bool,catalog:&str,observed_at:&str)->Value {
    json!({"model":model,"reasoning_effort":effort,"model_observation":observation(source,verified,catalog,observed_at)})
}

fn score(value:i64,reason:&str,sequence:i64,observed_at:&str)->Value {
    json!({"rubric_version":"laya-feedback-v1","dimension":"model_fit","value":value,"reason":reason,"evidence_refs":[],"phase":"initial","source_sequence":sequence,"observed_at":observed_at})
}

#[tokio::test]
async fn model_upgrade_preserves_attempt_configuration_scores_and_labels() {
    let (_root,store)=recorded_store().await;
    store.call("decisions/begin",json!({"request_id":"migration-request","id":"migration-decision","request":{"state":"migrate implementation without changing task risk"}})).await.unwrap();
    let original_labels=json!({"complexity":{"label":"high","score":0.86},"risk":{"label":"high","score":0.91},"certainty":{"label":"clear","score":0.93}});
    store.call("decisions/finish",json!({"id":"migration-decision","result":{"laya_result":original_labels,"advice":{"recommended":{"model":"model-a","reasoning_effort":null}}},"error":null,"context":{"model_catalog":"catalog-1"}})).await.unwrap();

    let source=json!({"host":"codex","role":"worker","actor_type":"agent"});
    let attempt_one=json!({
        "protocol_version":1,
        "event_id":"migration-assignment-1",
        "decision_id":"migration-decision",
        "attempt_ref":"attempt-1",
        "kind":"assignment",
        "source":source,
        "payload":{
            "recommended":pair("model-a",None,"laya",false,"catalog-1","2026-09-25T00:00:00Z"),
            "selected":null,
            "requested":null,
            "effective":pair("model-b",Some("high"),"host",true,"catalog-1","2026-09-25T00:00:00Z"),
            "mixed_configuration":false,
            "environment":{"code_revision":"base"},
            "scores":[score(1,"model B needed an upgrade",1,"2026-09-25T00:00:00Z")],
            "reason":"initial dispatch",
            "evidence_refs":[]
        }
    });
    let first_receipt=store.call("feedback",attempt_one.clone()).await.unwrap();

    let attempt_two=json!({
        "protocol_version":1,
        "event_id":"migration-assignment-2",
        "decision_id":"migration-decision",
        "attempt_ref":"attempt-2",
        "kind":"assignment",
        "source":source,
        "payload":{
            "recommended":null,
            "selected":null,
            "requested":null,
            "effective":pair("model-c",Some("high"),"host",true,"catalog-2","2027-03-25T00:00:00Z"),
            "parent_attempt_ref":"attempt-1",
            "change_reason":"model upgrade after the first attempt",
            "mixed_configuration":false,
            "environment":{"code_revision":"base"},
            "scores":[score(2,"model C completed the upgraded attempt",1,"2027-03-25T00:00:00Z")],
            "reason":"upgrade dispatch",
            "evidence_refs":[]
        }
    });
    let second_receipt=store.call("feedback",attempt_two.clone()).await.unwrap();
    let first_retry=store.call("feedback",attempt_one.clone()).await.unwrap();
    let second_retry=store.call("feedback",attempt_two.clone()).await.unwrap();

    assert_eq!(first_receipt["payload_hash"],hash(&attempt_one));
    assert_eq!(second_receipt["payload_hash"],hash(&attempt_two));
    assert_ne!(first_receipt["payload_hash"],second_receipt["payload_hash"]);
    assert_eq!(first_retry["idempotent"],true);
    assert_eq!(first_retry["payload_hash"],first_receipt["payload_hash"]);
    assert_eq!(first_retry["receive_sequence"],first_receipt["receive_sequence"]);
    assert_eq!(second_retry["idempotent"],true);
    assert_eq!(second_retry["payload_hash"],second_receipt["payload_hash"]);
    assert_eq!(second_retry["receive_sequence"],second_receipt["receive_sequence"]);

    let detail=store.call("decisions/get",json!({"id":"migration-decision"})).await.unwrap();
    assert_eq!(detail["result"]["laya_result"],original_labels);
    assert_eq!(detail["feedback"][0]["payload_hash"],hash(&attempt_one));
    assert_eq!(detail["feedback"][0]["attempt_ref"],"attempt-1");
    assert_eq!(detail["feedback"][0]["payload"]["scores"][0]["value"],1);
    assert_eq!(detail["feedback"][0]["payload"]["scores"][0]["observed_at"],"2026-09-25T00:00:00Z");
    assert_eq!(detail["feedback"][1]["payload_hash"],hash(&attempt_two));
    assert_eq!(detail["feedback"][1]["attempt_ref"],"attempt-2");
    assert_eq!(detail["feedback"][1]["payload"]["scores"][0]["value"],2);
    assert_eq!(detail["feedback"][1]["payload"]["scores"][0]["observed_at"],"2027-03-25T00:00:00Z");

    assert_eq!(detail["execution_attempts"].as_array().unwrap().len(),2);
    assert_eq!(detail["execution_attempts"][0]["recommended"]["model"],"model-a");
    assert!(detail["execution_attempts"][0]["recommended"]["reasoning_effort"].is_null());
    assert_eq!(detail["execution_attempts"][0]["effective"]["model"],"model-b");
    assert_eq!(detail["execution_attempts"][0]["effective"]["reasoning_effort"],"high");
    assert_eq!(detail["execution_attempts"][1]["effective"]["model"],"model-c");
    assert_eq!(detail["execution_attempts"][1]["effective"]["reasoning_effort"],"high");
    assert_eq!(detail["execution_attempts"][1]["parent_attempt_ref"],"attempt-1");

    let effective_observations=detail["model_observations"].as_array().unwrap().iter().filter(|observation|observation["stage"]=="effective").collect::<Vec<_>>();
    assert_eq!(effective_observations.len(),2);
    assert!(effective_observations[0]["observation"]["model_revision"].is_null());
    assert_eq!(effective_observations[0]["observation"]["catalog_version"],"catalog-1");
    assert_eq!(effective_observations[0]["observation"]["observed_at"],"2026-09-25T00:00:00Z");
    assert!(effective_observations[1]["observation"]["model_revision"].is_null());
    assert_eq!(effective_observations[1]["observation"]["catalog_version"],"catalog-2");
    assert_eq!(effective_observations[1]["observation"]["observed_at"],"2027-03-25T00:00:00Z");
}
