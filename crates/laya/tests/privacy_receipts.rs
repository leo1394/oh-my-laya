use laya::{outbox::Outbox,protocol::hash,store::Store};
use rusqlite::{params,Connection};
use serde_json::{json,Value};

async fn recorded_store()->(tempfile::TempDir,Store) {
    let root=tempfile::tempdir().unwrap();
    let store=Store::open(root.path()).unwrap();
    store.call("settings/update",json!({"recording_enabled":true})).await.unwrap();
    (root,store)
}

fn receipt(case_id:&str)->Value {
    json!({"contract":"memory_receipt_v1","policy_version":"whole-case-token-budget-v1",
        "serialization":"laya-state-mask-normalized-utf8","selected_case_ids":[case_id],
        "received_case_ids":[case_id],"excluded":[{"case_id":case_id,"reason":"token_budget","required_state_tokens":999}],
        "available_state_tokens":100,"base_state_tokens":10,"packed_state_tokens":90,
        "base_state_bytes":10,"packed_state_bytes":90,"unit":"checkpoint_tokens","max_cases":2,
        "complete_base_state":true,"input_fingerprint":"a".repeat(64),"usage_scope":"current_evaluation",
        "checkpoint_identity":{"checkpoint_digest":"fixture","identity_scope":"full_checkpoint_content"}})
}

fn assert_withdrawn(value:&Value) {
    assert!(value["selected_case_ids"].as_array().unwrap().is_empty());
    assert!(value["received_case_ids"].as_array().unwrap().is_empty());
    assert!(value["excluded"].as_array().unwrap().is_empty());
    assert_eq!(value["receipt_status"],"withdrawn");
    assert!(value["complete_base_state"].is_null());
    assert!(value["input_fingerprint"].is_null());
    assert_eq!(value["usage_scope"],"withdrawn");
}

#[tokio::test]
async fn case_deletion_scrubs_inline_and_external_receipts_and_restore_keeps_tombstones() {
    let (root,store)=recorded_store().await;
    store.call("decisions/begin",json!({"request_id":"source","id":"source","request":{"state":"private case"}})).await.unwrap();
    let review=store.call("reviews/create",json!({"id":"source","expected_revision":0,"status":"confirmed",
        "labels":{"complexity":"low","risk":"low","certainty":"clear"},"reason":"case"})).await.unwrap();
    let case_id=review["case_id"].as_str().unwrap().to_string();
    let memory_receipt=receipt(&case_id);
    store.call("decisions/begin",json!({"request_id":"consumer","id":"consumer","request":{"state":"consumer"}})).await.unwrap();
    store.call("decisions/finish",json!({"id":"consumer","error":null,
        "context":{"memory_receipt":memory_receipt,"padding":"context-padding-".repeat(2000)},
        "result":{"meta":{"case_ids":[case_id],"worker_case_ids":[case_id],"memory_receipt":receipt(&case_id)},
            "padding":"output-padding-".repeat(2000)}})).await.unwrap();
    store.call("versions/create",json!({"id":"receipt-version","case_ids":[case_id]})).await.unwrap();
    store.call("versions/create",json!({"id":"evaluation-only-version","case_ids":[]})).await.unwrap();
    store.call("jobs/create",json!({"id":"evaluation-job","kind":"evaluation","payload":{},"status":"running"})).await.unwrap();
    store.call("jobs/update",json!({"id":"evaluation-job","status":"completed","result":{"memory_receipt":receipt(&case_id)}})).await.unwrap();
    let connection=Connection::open(root.path().join("laya.sqlite3")).unwrap();
    connection.execute("UPDATE memory_versions SET status='active',evaluation_status='passed',evaluation_json=?1,activated_at=1 WHERE id='receipt-version'",
        params![json!({"passed":true,"memory_receipt":receipt(&case_id)}).to_string()]).unwrap();
    connection.execute("UPDATE memory_versions SET evaluation_status='passed',evaluation_json=?1 WHERE id='evaluation-only-version'",
        params![json!({"passed":true,"memory_receipt":receipt(&case_id)}).to_string()]).unwrap();
    connection.execute("UPDATE settings SET active_memory_version='receipt-version' WHERE singleton=1",[]).unwrap();
    drop(connection);
    let before=store.call("decisions/get",json!({"id":"consumer"})).await.unwrap();
    assert!(before["snapshots"].as_array().unwrap().iter().any(|item|item["kind"]=="context"&&item["artifact_id"].is_string()));
    assert!(before["snapshots"].as_array().unwrap().iter().any(|item|item["kind"]=="output"&&item["artifact_id"].is_string()));
    store.call("backup/create",json!({"id":"before-withdrawal"})).await.unwrap();

    store.call("cases/delete",json!({"id":case_id})).await.unwrap();
    let scrubbed=store.call("decisions/get",json!({"id":"consumer"})).await.unwrap();
    assert_eq!(scrubbed["context"]["evidence_withdrawn"],true);
    assert_eq!(scrubbed["result"]["evidence_withdrawn"],true);
    assert_withdrawn(&scrubbed["context"]["memory_receipt"]);
    assert_withdrawn(&scrubbed["result"]["meta"]["memory_receipt"]);
    assert!(scrubbed["result"]["meta"]["case_ids"].as_array().unwrap().is_empty());
    assert!(scrubbed["result"]["meta"]["worker_case_ids"].as_array().unwrap().is_empty());
    for snapshot in scrubbed["snapshots"].as_array().unwrap().iter().filter(|item|matches!(item["kind"].as_str(),Some("context"|"output"))) {
        assert_eq!(snapshot["capture_status"],"redacted");
        assert_eq!(snapshot["payload"]["evidence_withdrawn"],true);
    }
    let version=store.call("versions/get",json!({"id":"receipt-version"})).await.unwrap();
    assert_eq!(version["status"],"invalidated");
    assert_eq!(version["invalidated"],true);
    assert_eq!(version["invalidation_reason"],"referenced case deleted");
    assert_eq!(version["report"]["evidence_withdrawn"],true);
    assert_withdrawn(&version["report"]["memory_receipt"]);
    let evaluation_only=store.call("versions/get",json!({"id":"evaluation-only-version"})).await.unwrap();
    assert_eq!(evaluation_only["invalidated"],true);
    assert_eq!(evaluation_only["invalidation_reason"],"evaluation evidence withdrawn");
    assert_withdrawn(&evaluation_only["report"]["memory_receipt"]);
    assert!(store.call("versions/activate",json!({"id":"evaluation-only-version"})).await.unwrap_err().to_string().contains("invalidated"));
    assert!(store.call("settings/get",json!({})).await.unwrap()["active_memory_version"].is_null());
    let job=store.call("jobs/get",json!({"id":"evaluation-job"})).await.unwrap();
    assert_eq!(job["result"]["evidence_withdrawn"],true);
    assert_withdrawn(&job["result"]["memory_receipt"]);

    store.call("backup/restore",json!({"id":"before-withdrawal"})).await.unwrap();
    let restored=store.call("decisions/get",json!({"id":"consumer"})).await.unwrap();
    assert_withdrawn(&restored["context"]["memory_receipt"]);
    assert_withdrawn(&restored["result"]["meta"]["memory_receipt"]);
    assert_eq!(restored["context"]["evidence_withdrawn"],true);
    let restored_version=store.call("versions/get",json!({"id":"receipt-version"})).await.unwrap();
    assert_eq!(restored_version["status"],"invalidated");
    assert_withdrawn(&restored_version["report"]["memory_receipt"]);
    let restored_evaluation_only=store.call("versions/get",json!({"id":"evaluation-only-version"})).await.unwrap();
    assert_eq!(restored_evaluation_only["invalidation_reason"],"evaluation evidence withdrawn");
    assert_withdrawn(&restored_evaluation_only["report"]["memory_receipt"]);
    assert_withdrawn(&store.call("jobs/get",json!({"id":"evaluation-job"})).await.unwrap()["result"]["memory_receipt"]);
    assert_eq!(store.call("decisions/get",json!({"id":"source"})).await.unwrap()["id"],"source");
    let cases=store.call("cases/list",json!({"include_deleted":true})).await.unwrap();
    assert_eq!(cases["items"].as_array().unwrap().iter().find(|item|item["id"]==case_id).unwrap()["active"],false);
    assert!(root.path().join("backups/before-withdrawal.sqlite3").is_file());
}

#[test]
fn outbox_withdrawal_rehashes_snapshot_and_only_current_replay_is_acknowledged() {
    let root=tempfile::tempdir().unwrap();
    let outbox=Outbox::open(root.path()).unwrap();
    let case_id="case-private";
    let payload=json!({"id":"consumer","result":{"meta":{"case_ids":[case_id],"memory_receipt":receipt(case_id)}},
        "context":{"memory_receipt":receipt(case_id)}});
    let original=outbox.snapshot_enqueue("decisions/finish",&payload).unwrap();
    outbox.withdraw_cases(&[case_id.to_string()]).unwrap();
    let pending=outbox.snapshot_pending().unwrap();
    assert_eq!(pending.len(),1);
    let updated=&pending[0];
    assert_ne!(updated["payload_hash"],original["payload_hash"]);
    assert_eq!(updated["payload_hash"],hash(&updated["payload"]));
    assert_eq!(updated["payload"]["capture_redacted"],true);
    assert_eq!(updated["payload"]["context"]["evidence_withdrawn"],true);
    assert_withdrawn(&updated["payload"]["context"]["memory_receipt"]);
    assert_withdrawn(&updated["payload"]["result"]["meta"]["memory_receipt"]);
    outbox.snapshot_ack(&original).unwrap();
    assert_eq!(outbox.snapshot_pending().unwrap().len(),1);
    outbox.snapshot_ack(updated).unwrap();
    assert!(outbox.snapshot_pending().unwrap().is_empty());
}
