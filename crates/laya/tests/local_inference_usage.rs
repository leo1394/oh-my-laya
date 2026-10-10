use laya::store::Store;
use rusqlite::{params,Connection};
use serde_json::{json,Value};

async fn recorded_store()->(tempfile::TempDir,Store) {
    let root=tempfile::tempdir().unwrap();
    let store=Store::open(root.path()).unwrap();
    store.call("settings/update",json!({"recording_enabled":true})).await.unwrap();
    (root,store)
}

async fn finish(store:&Store,id:&str,result:Value,error:Value) {
    store.call("decisions/begin",json!({"request_id":id,"id":id,"request":{"state":id}})).await.unwrap();
    let payload=json!({"id":id,"result":result,"error":error});
    store.call("decisions/finish",payload.clone()).await.unwrap();
    assert_eq!(store.call("decisions/finish",payload).await.unwrap()["existing"],true);
}

async fn tokens(store:&Store,bounds:Value)->Value {
    store.call("overview/get",bounds).await.unwrap()["dashboard"]["tokens"].clone()
}

fn raw(input:Value,output:Value)->Value {
    json!({"answers":{"risk":{"choice":"low"}},"usage":{"input_tokens":input,"output_tokens":output}})
}

#[tokio::test]
async fn raw_and_advisor_predictions_count_once_without_host_usage() {
    let (_root,store)=recorded_store().await;
    finish(&store,"raw",raw(json!(12),json!(3)),Value::Null).await;
    finish(&store,"advisor",json!({"laya_result":raw(json!(20),json!(4)),"usage":{"input_tokens":999,"output_tokens":999}}),Value::Null).await;
    let tokens=tokens(&store,json!({})).await;
    assert_eq!(tokens["local_inference"],json!({"total_tokens":39,"input_tokens":32,"output_tokens":7,"known_decisions":2,"missing_decisions":0,"cached_decisions":0,"status":"available","scope":"stored_decisions_fresh_local_inference"}));
    assert!(tokens["recorded_total"].is_null());
    assert_eq!(tokens["included_attempts"],0);
    assert!(tokens["estimated_saved"].is_null());
    assert_eq!(store.call("status",json!({})).await.unwrap()["dashboard"]["tokens"],tokens);
}

#[tokio::test]
async fn caches_contribute_known_zero_and_do_not_repeat_source_usage() {
    let (_root,store)=recorded_store().await;
    for (id,meta) in [
        ("cache",json!({"assessment_cache":{"status":"hit","inference_input_tokens":0,"inference_output_tokens":0,"raw_usage_scope":"source_evaluation"}})),
        ("receipt",json!({"memory_receipt":{"usage_scope":"source_evaluation"}})),
        ("scope",json!({"assessment_cache":{"raw_usage_scope":"source_evaluation"}})),
    ] {
        finish(&store,id,json!({"laya_result":raw(json!(100),json!(10)),"meta":meta}),Value::Null).await;
    }
    let local=tokens(&store,json!({})).await["local_inference"].clone();
    assert_eq!(local["cached_decisions"],3);
    assert_eq!(local["known_decisions"],0);
    assert_eq!(local["total_tokens"],0);
    assert_eq!(local["status"],"available");
    finish(&store,"fresh",raw(json!(7),json!(2)),Value::Null).await;
    assert_eq!(tokens(&store,json!({})).await["local_inference"]["total_tokens"],9);
}

#[tokio::test]
async fn missing_invalid_failed_and_pending_usage_stays_unknown() {
    let (_root,store)=recorded_store().await;
    assert!(tokens(&store,json!({})).await["local_inference"]["total_tokens"].is_null());
    for (id,result,error) in [
        ("missing",json!({"answers":{}}),Value::Null),
        ("negative",raw(json!(-1),json!(2)),Value::Null),
        ("fractional",raw(json!(1),json!(2.5)),Value::Null),
        ("null",raw(Value::Null,json!(2)),Value::Null),
        ("string",raw(json!("1"),json!(2)),Value::Null),
        ("failure",raw(json!(1),json!(2)),json!({"message":"failed"})),
    ] {finish(&store,id,result,error).await;}
    store.call("decisions/begin",json!({"request_id":"pending","id":"pending","request":{}})).await.unwrap();
    let local=tokens(&store,json!({})).await["local_inference"].clone();
    assert_eq!(local["missing_decisions"],7);
    assert_eq!(local["status"],"unavailable");
    for key in ["total_tokens","input_tokens","output_tokens"] {assert!(local[key].is_null());}
    finish(&store,"zero",raw(json!(0),json!(0)),Value::Null).await;
    let local=tokens(&store,json!({})).await["local_inference"].clone();
    assert_eq!(local["status"],"partial");
    assert_eq!(local["known_decisions"],1);
    assert_eq!(local["total_tokens"],0);
}

#[tokio::test]
async fn decision_dates_are_inclusive_and_deleted_rows_are_excluded() {
    let (root,store)=recorded_store().await;
    for (id,time) in [("before",100),("start",200),("end",300),("after",400),("deleted",250)] {
        finish(&store,id,raw(json!(5),json!(1)),Value::Null).await;
        let connection=Connection::open(root.path().join("laya.sqlite3")).unwrap();
        connection.execute("UPDATE decisions SET created_at=?2,deleted_at=?3 WHERE id=?1",params![id,time,if id=="deleted"{Some(500)}else{None}]).unwrap();
    }
    let local=tokens(&store,json!({"created_after":200,"created_before":300})).await["local_inference"].clone();
    assert_eq!(local["known_decisions"],2);
    assert_eq!(local["total_tokens"],12);
    assert_eq!(tokens(&store,json!({})).await["local_inference"]["total_tokens"],24);
}

#[tokio::test]
async fn aggregate_overflow_never_exposes_inexact_javascript_numbers() {
    let (_root,store)=recorded_store().await;
    finish(&store,"boundary",raw(json!(9_007_199_254_740_990u64),json!(1)),Value::Null).await;
    assert_eq!(tokens(&store,json!({})).await["local_inference"]["total_tokens"],9_007_199_254_740_991u64);
    finish(&store,"extra",raw(json!(0),json!(1)),Value::Null).await;
    let local=tokens(&store,json!({})).await["local_inference"].clone();
    assert_eq!(local["status"],"overflow");
    assert_eq!(local["known_decisions"],2);
    for key in ["total_tokens","input_tokens","output_tokens"] {assert!(local[key].is_null());}
    let (_overflow_root,store)=recorded_store().await;
    finish(&store,"u64",raw(json!(u64::MAX),json!(u64::MAX)),Value::Null).await;
    assert_eq!(tokens(&store,json!({})).await["local_inference"]["status"],"overflow");
}
