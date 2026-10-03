use laya::{outbox::Outbox,protocol::hash,store::Store};
use rusqlite::Connection;
use serde_json::{json,Value};

async fn recorded_store()->(tempfile::TempDir,Store) {
    let root=tempfile::tempdir().unwrap();
    let store=Store::open(root.path()).unwrap();
    store.call("settings/update",json!({"recording_enabled":true})).await.unwrap();
    (root,store)
}

fn score(value:i64,phase:&str,sequence:i64,supersedes:Option<&str>)->Value {
    let mut score=json!({"rubric_version":"laya-feedback-v1","dimension":"judgment_quality","value":value,"reason":"acceptance evidence","evidence_refs":[],"phase":phase,"source_sequence":sequence,"observed_at":"2026-10-04T00:00:00Z"});
    if let Some(event_id)=supersedes {score["supersedes_event_id"]=json!(event_id);}
    score
}

fn review(event_id:&str,decision_id:&str,value:i64,phase:&str,sequence:i64,supersedes:Option<&str>)->Value {
    json!({
        "protocol_version":1,
        "event_id":event_id,
        "decision_id":decision_id,
        "attempt_ref":"review-attempt",
        "kind":"review",
        "source":{"host":"codex","role":"reviewer","actor_type":"agent"},
        "payload":{"disposition":"changes_requested","scores":[score(value,phase,sequence,supersedes)]}
    })
}

#[tokio::test]
async fn lost_ack_replays_the_committed_feedback_without_duplication() {
    let (root,store)=recorded_store().await;
    store.call("decisions/begin",json!({"request_id":"lost-ack","id":"lost-ack","request":{"state":"review task"}})).await.unwrap();
    let event=review("initial-feedback","lost-ack",1,"initial",1,None);
    let outbox=Outbox::open(root.path()).unwrap();
    let queued=outbox.enqueue(&event).unwrap();
    assert_eq!(queued["status"],"queued_local");
    assert_eq!(queued["payload_hash"],hash(&event));

    let first=store.call("feedback",event.clone()).await.unwrap();
    assert_eq!(first["status"],"stored");
    assert_eq!(first["payload_hash"],hash(&event));
    drop(outbox);

    let reopened=Outbox::open(root.path()).unwrap();
    assert_eq!(reopened.pending().unwrap(),vec![event.clone()]);
    let replay=store.call("feedback",reopened.pending().unwrap().remove(0)).await.unwrap();
    assert_eq!(replay["idempotent"],true);
    assert_eq!(replay["payload_hash"],first["payload_hash"]);
    assert_eq!(replay["receive_sequence"],first["receive_sequence"]);
    reopened.acknowledge(&event,&replay).unwrap();
    assert!(reopened.pending().unwrap().is_empty());

    let detail=store.call("decisions/get",json!({"id":"lost-ack"})).await.unwrap();
    assert_eq!(detail["feedback"].as_array().unwrap().len(),1);
    assert_eq!(detail["feedback"][0]["payload_hash"],hash(&event));
}

#[tokio::test]
async fn retention_and_storage_pressure_preserve_initial_and_revision_chain() {
    let (root,store)=recorded_store().await;
    store.call("settings/update",json!({"storage_soft_limit_bytes":1024*1024})).await.unwrap();
    store.call("decisions/begin",json!({"request_id":"scored","id":"scored","request":{"state":"review task"}})).await.unwrap();
    store.call("decisions/finish",json!({"id":"scored","result":{"advice":{"uncertain":false}},"error":null,"context":{}})).await.unwrap();
    let initial=review("score-initial","scored",0,"initial",1,None);
    let revision_one=review("score-revision-1","scored",1,"revision",2,Some("score-initial"));
    let revision_two=review("score-revision-2","scored",2,"revision",3,Some("score-revision-1"));
    for event in [&initial,&revision_one,&revision_two] {
        let receipt=store.call("feedback",event.clone()).await.unwrap();
        assert_eq!(receipt["payload_hash"],hash(event));
    }

    let pressure="ordinary-pressure-".repeat(55_000);
    store.call("decisions/begin",json!({"request_id":"ordinary","id":"ordinary","request":{"state":pressure}})).await.unwrap();
    store.call("decisions/finish",json!({"id":"ordinary","result":{"advice":{"uncertain":false}},"error":null,"context":{}})).await.unwrap();
    assert_eq!(store.call("status",json!({})).await.unwrap()["storage_pressure"],true);

    // Retention has no public clock injection; only timestamps are aged directly.
    Connection::open(root.path().join("laya.sqlite3")).unwrap().execute("UPDATE decisions SET finished_at=0 WHERE id IN('scored','ordinary')",[]).unwrap();
    assert_eq!(store.call("retention",json!({"days":1})).await.unwrap()["removed"],1);
    assert_eq!(store.call("decisions/get",json!({"id":"ordinary"})).await.unwrap()["recording_status"],"source_expired");

    let detail=store.call("decisions/get",json!({"id":"scored"})).await.unwrap();
    assert_eq!(detail["recording_status"],"stored");
    assert_eq!(detail["feedback"].as_array().unwrap().len(),3);
    assert_eq!(detail["feedback"][0]["payload_hash"],hash(&initial));
    assert_eq!(detail["feedback"][0]["payload"]["scores"][0]["phase"],"initial");
    assert_eq!(detail["feedback"][1]["payload_hash"],hash(&revision_one));
    assert_eq!(detail["feedback"][1]["payload"]["scores"][0]["supersedes_event_id"],"score-initial");
    assert_eq!(detail["feedback"][2]["payload_hash"],hash(&revision_two));
    assert_eq!(detail["feedback"][2]["payload"]["scores"][0]["supersedes_event_id"],"score-revision-1");
}
