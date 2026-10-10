use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const BUCKET_SECONDS:i64=900;
const DAY_SECONDS:i64=86400;

fn text_field(column:&str,path:&str,max:usize)->String {
    format!("CASE WHEN json_type({column},'{path}')='text' THEN substr(json_extract({column},'{path}'),1,{max}) END")
}

fn roles(value:&str)->Option<Value> {
    let roles=serde_json::from_str::<Value>(value).ok()?;
    let items=roles.as_array()?;
    if items.len()>5||items.iter().any(|role|!role.as_str().is_some_and(|role|["explorer","worker","tester","researcher","reviewer"].contains(&role)))||items.iter().enumerate().any(|(index,role)|items[..index].contains(role)) {return None;}
    Some(roles)
}

pub(crate) fn privacy_revision(connection:&Connection)->Result<String> {
    let privacy_event:Option<(i64,String,i64,Option<String>)>=connection.query_row("SELECT sequence,kind,created_at,CASE WHEN kind='backup.restored' AND json_type(payload_json,'$.safety_backup_id')='text' THEN substr(json_extract(payload_json,'$.safety_backup_id'),1,128) END FROM events WHERE kind IN('decision.deleted','case.deleted','retention.completed','backup.restored') ORDER BY sequence DESC LIMIT 1",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional()?;
    Ok(privacy_event.map(|(sequence,kind,created_at,safety_backup_id)|format!("{:x}",Sha256::digest(json!([sequence,kind,created_at,safety_backup_id]).to_string().as_bytes()))).unwrap_or_else(||"none".into()))
}

pub(crate) fn query_with_revision(connection:&Connection, params:Value, privacy_revision:&str)->Result<Value> {
    let map=params.as_object().ok_or_else(||anyhow!("invalid: activity parameters must be an object"))?;
    if let Some(key)=map.keys().find(|key|!["created_after","created_before","limit","offset","q"].contains(&key.as_str())) {bail!("invalid: unknown field {key}");}
    let start=map.get("created_after").and_then(Value::as_i64).ok_or_else(||anyhow!("invalid: created_after must be a nonnegative integer"))?;
    let end=map.get("created_before").and_then(Value::as_i64).ok_or_else(||anyhow!("invalid: created_before must be a nonnegative integer"))?;
    if start<0||end<=start {bail!("invalid: activity interval must be positive");}
    let limit=match map.get("limit") {Some(value)=>value.as_i64().ok_or_else(||anyhow!("invalid: limit must be an integer"))?,None=>50};
    if !(1..=100).contains(&limit) {bail!("invalid: limit out of range");}
    let offset=match map.get("offset") {Some(value)=>value.as_i64().ok_or_else(||anyhow!("invalid: offset must be an integer"))?,None=>0};
    if offset<0 {bail!("invalid: offset out of range");}
    let search=match map.get("q") {Some(value)=>value.as_str().ok_or_else(||anyhow!("invalid: q must be text"))?.trim(),None=>""};
    if search.chars().count()>200 {bail!("invalid: q is too long");}

    let span=end-start;
    let bucket_seconds=if span<=31*DAY_SECONDS {BUCKET_SECONDS} else {((span-1)/DAY_SECONDS/400+1)*DAY_SECONDS};
    let bucket_count=((span-1)/bucket_seconds+1) as usize;
    let mut buckets=(0..bucket_count).map(|index| {
        let bucket_start=start+index as i64*bucket_seconds;
        json!({"start":bucket_start,"end":bucket_start.saturating_add(bucket_seconds).min(end),"count":0,"failed":0})
    }).collect::<Vec<_>>();
    let mut totals=json!({"decisions":0,"completed":0,"failed":0,"pending":0});
    let mut statement=connection.prepare("SELECT (created_at-?1)/?3,COUNT(*),SUM(CASE WHEN finished_at IS NOT NULL AND error_json IS NOT NULL AND error_json<>'null' THEN 1 ELSE 0 END),SUM(CASE WHEN finished_at IS NOT NULL AND (error_json IS NULL OR error_json='null') THEN 1 ELSE 0 END),SUM(CASE WHEN finished_at IS NULL THEN 1 ELSE 0 END) FROM decisions WHERE deleted_at IS NULL AND created_at>=?1 AND created_at<?2 GROUP BY 1")?;
    let rows=statement.query_map(params![start,end,bucket_seconds],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?,row.get::<_,i64>(2)?,row.get::<_,i64>(3)?,row.get::<_,i64>(4)?)))?;
    for row in rows {
        let (index,count,failed,completed,pending)=row?;
        let bucket=&mut buckets[index as usize];
        bucket["count"]=json!(count);
        bucket["failed"]=json!(failed);
        totals["decisions"]=json!(totals["decisions"].as_i64().unwrap()+count);
        totals["completed"]=json!(totals["completed"].as_i64().unwrap()+completed);
        totals["failed"]=json!(totals["failed"].as_i64().unwrap()+failed);
        totals["pending"]=json!(totals["pending"].as_i64().unwrap()+pending);
    }

    // This list and the buckets share the selected interval.
    let complexity=["$.laya_result.answers.complexity.choice","$.laya_result.complexity.label","$.advice.assessment.complexity.choice","$.answers.complexity.choice"].map(|path|text_field("result_json",path,16)).join(",");
    let risk=["$.laya_result.answers.risk.choice","$.laya_result.risk.label","$.advice.assessment.risk.choice","$.answers.risk.choice"].map(|path|text_field("result_json",path,16)).join(",");
    let model=["$.advice.recommendation.model","$.advice.recommended.model"].map(|path|text_field("result_json",path,256)).join(",");
    let effort=["$.advice.recommendation.reasoning_effort","$.advice.recommended.reasoning_effort"].map(|path|text_field("result_json",path,64)).join(",");
    let query=format!("SELECT id,created_at,finished_at,CASE WHEN error_json IS NOT NULL AND error_json<>'null' THEN 1 ELSE 0 END,CASE WHEN json_type(request_json,'$.state')='text' THEN substr(json_extract(request_json,'$.state'),1,600) ELSE id END,{},COALESCE({complexity}),COALESCE({risk}),CASE WHEN json_type(result_json,'$.advice.uncertain') IN('true','false') THEN json_extract(result_json,'$.advice.uncertain') WHEN json_type(result_json,'$.uncertain') IN('true','false') THEN json_extract(result_json,'$.uncertain') END,COALESCE({model}),COALESCE({effort}),{},CASE WHEN length(json_extract(result_json,'$.orchestration_plan.run_id'))<=128 THEN {} END,CASE WHEN length(json_extract(result_json,'$.orchestration_plan.stage_id'))<=128 THEN {} END,CASE WHEN json_type(result_json,'$.orchestration_plan.required_roles')='array' THEN substr(json_extract(result_json,'$.orchestration_plan.required_roles'),1,2048) END,CASE WHEN json_extract(result_json,'$.orchestration_plan.contract')='orchestration_plan_v1' AND json_extract(result_json,'$.orchestration_plan.schema_version')=1 THEN 1 ELSE 0 END,created_at_ms FROM decisions WHERE deleted_at IS NULL AND created_at>=?2 AND created_at<?3 ORDER BY COALESCE(created_at_ms,created_at*1000) DESC,id DESC LIMIT ?1",text_field("request_json","$.advisor.role",128),text_field("result_json","$.orchestration_plan.mode",32),text_field("result_json","$.orchestration_plan.run_id",128),text_field("result_json","$.orchestration_plan.stage_id",128));
    let search_filter="(?4='' OR instr(lower(id || ' ' || COALESCE(json_extract(request_json,'$.state'),'') || ' ' || COALESCE(json_extract(request_json,'$.advisor.role'),'')),lower(?4))>0)";
    let query=query.replace("ORDER BY COALESCE",&format!("AND {search_filter} ORDER BY COALESCE"))+" OFFSET ?5";
    let matched:i64=connection.query_row(&format!("SELECT COUNT(*) FROM decisions WHERE deleted_at IS NULL AND created_at>=?2 AND created_at<?3 AND {search_filter}"),params![limit,start,end,search],|row|row.get(0))?;
    let mut statement=connection.prepare(&query)?;
    let rows=statement.query_map(params![limit,start,end,search,offset],|row| {
        Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,Option<i64>>(2)?,row.get::<_,bool>(3)?,row.get::<_,String>(4)?,row.get::<_,Option<String>>(5)?,row.get::<_,Option<String>>(6)?,row.get::<_,Option<String>>(7)?,row.get::<_,Option<bool>>(8)?,row.get::<_,Option<String>>(9)?,row.get::<_,Option<String>>(10)?,row.get::<_,Option<String>>(11)?,row.get::<_,Option<String>>(12)?,row.get::<_,Option<String>>(13)?,row.get::<_,Option<String>>(14)?,row.get::<_,bool>(15)?,row.get::<_,Option<i64>>(16)?))
    })?;
    let mut items=Vec::new();
    for row in rows {
        let (id,created_at,finished_at,has_error,summary,role,complexity,risk,uncertain,model,reasoning_effort,mode,run_id,stage_id,required_roles,plan_contract,created_at_ms)=row?;
        let recommendation=model.map(|model|json!({"model":model,"reasoning_effort":reasoning_effort}));
        let orchestration=if plan_contract&&finished_at.is_some() {
            match (mode,run_id,stage_id,required_roles.as_deref().and_then(roles)) {
                (Some(mode),Some(run_id),Some(stage_id),Some(required_roles)) if ["direct","delegate","needs_context"].contains(&mode.as_str())&&!run_id.is_empty()&&!stage_id.is_empty()=>Some(json!({"mode":mode,"run_id":run_id,"stage_id":stage_id,"required_roles":required_roles})),
                _=>None,
            }
        } else {None};
        let mut assignments=Vec::new();
        let assignment_query=format!("SELECT CASE WHEN json_extract(f.payload_json,'$.execution.contract')='dispatch_receipt_v1' AND json_extract(f.payload_json,'$.execution.role') IN('orchestrator','explorer','worker','tester','researcher','reviewer') THEN json_extract(f.payload_json,'$.execution.role') END,substr(a.attempt_ref,1,128),a.created_at,{},{},CASE WHEN json_extract(a.effective_json,'$.model_observation.source')='host' AND json_extract(a.effective_json,'$.model_observation.verified')=1 THEN {} END,CASE WHEN json_extract(a.effective_json,'$.model_observation.source')='host' AND json_extract(a.effective_json,'$.model_observation.verified')=1 THEN {} END,CASE WHEN json_extract(f.payload_json,'$.execution.contract')='dispatch_receipt_v1' AND json_extract(f.payload_json,'$.execution.status') IN('started','failed','unknown') THEN json_extract(f.payload_json,'$.execution.status') END FROM execution_attempts a JOIN feedback_events f ON f.event_id=a.event_id WHERE a.decision_id=?1 ORDER BY a.id DESC LIMIT 6",text_field("a.requested_json","$.model",256),text_field("a.requested_json","$.reasoning_effort",64),text_field("a.effective_json","$.model",256),text_field("a.effective_json","$.reasoning_effort",64));
        let mut assignment_statement=connection.prepare(&assignment_query)?;
        let assignment_rows=assignment_statement.query_map([&id],|row| {
            let requested_model:Option<String>=row.get(3)?;
            let requested_effort:Option<String>=row.get(4)?;
            let effective_model:Option<String>=row.get(5)?;
            let effective_effort:Option<String>=row.get(6)?;
            Ok(json!({"role":row.get::<_,Option<String>>(0)?,"attempt_ref":row.get::<_,String>(1)?,"created_at":row.get::<_,i64>(2)?,"requested":requested_model.map(|model|json!({"model":model,"reasoning_effort":requested_effort})),"effective":effective_model.map(|model|json!({"model":model,"reasoning_effort":effective_effort})),"status":row.get::<_,Option<String>>(7)?}))
        })?;
        for assignment in assignment_rows {assignments.push(assignment?);}
        let assignments_truncated=assignments.len()>5;
        assignments.truncate(5);
        items.push(json!({"id":id,"created_at":created_at,"created_at_ms":created_at_ms,"finished_at":finished_at,"summary":summary,"status":if finished_at.is_none(){"pending"}else if has_error{"failed"}else{"completed"},"role":role,"complexity":complexity.filter(|choice|["low","medium","high"].contains(&choice.as_str())),"risk":risk.filter(|choice|["low","medium","high"].contains(&choice.as_str())),"uncertain":uncertain,"recommendation":recommendation,"orchestration":orchestration,"assignments":assignments,"assignments_truncated":assignments_truncated}));
    }
    let recording_enabled:bool=connection.query_row("SELECT json_extract(value_json,'$.recording_enabled') FROM settings WHERE singleton=1",[],|row|row.get(0))?;
    Ok(json!({"scope":"recorded_decisions","items_scope":"selected_recorded_decisions","recording_enabled":recording_enabled,"privacy_revision":privacy_revision,"start":start,"end":end,"bucket_seconds":bucket_seconds,"buckets":buckets,"totals":totals,"items":items,"pagination":{"offset":offset,"limit":limit,"total":matched,"q":search}}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(connection:&Connection, params:Value)->Result<Value> {
        query_with_revision(connection,params,&privacy_revision(connection)?)
    }

    fn database()->Connection {
        let connection=Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE settings(singleton INTEGER PRIMARY KEY,value_json TEXT NOT NULL); INSERT INTO settings VALUES(1,'{\"recording_enabled\":false}'); CREATE TABLE decisions(id TEXT PRIMARY KEY,created_at INTEGER NOT NULL,finished_at INTEGER,deleted_at INTEGER,request_json TEXT NOT NULL,result_json TEXT,error_json TEXT,created_at_ms INTEGER); CREATE TABLE feedback_events(event_id TEXT PRIMARY KEY,payload_json TEXT NOT NULL); CREATE TABLE execution_attempts(id INTEGER PRIMARY KEY,event_id TEXT NOT NULL,decision_id TEXT NOT NULL,role TEXT,attempt_ref TEXT NOT NULL,created_at INTEGER NOT NULL,requested_json TEXT,effective_json TEXT); CREATE TABLE events(sequence INTEGER PRIMARY KEY,kind TEXT NOT NULL,payload_json TEXT NOT NULL,created_at INTEGER NOT NULL);").unwrap();
        connection
    }

    fn decision(connection:&Connection,id:&str,time:i64,finished:Option<i64>,error:Option<&str>,deleted:bool,request:Value,result:Option<Value>) {
        connection.execute("INSERT INTO decisions(id,created_at,finished_at,deleted_at,request_json,result_json,error_json) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![id,time,finished,deleted.then_some(time),request.to_string(),result.map(|value|value.to_string()),error]).unwrap();
    }

    #[test]
    fn search_and_pagination_do_not_change_chart_totals() {
        let connection=database();
        for index in 0..65 {decision(&connection,&format!("id-{index}"),index,Some(index),None,false,json!({"state":if index==0 {"unique 100% 案例"} else {"ordinary"},"advisor":{"role":"worker"}}),None);}
        let first=query(&connection,json!({"created_after":0,"created_before":100,"limit":10})).unwrap();
        let second=query(&connection,json!({"created_after":0,"created_before":100,"limit":10,"offset":10})).unwrap();
        assert_eq!(first["pagination"]["total"],65);
        assert_eq!(second["items"][0]["id"],"id-54");
        let search=query(&connection,json!({"created_after":0,"created_before":100,"q":"100% 案例"})).unwrap();
        assert_eq!(search["items"][0]["id"],"id-0");
        assert_eq!(search["pagination"]["total"],1);
        assert_eq!(search["totals"],first["totals"]);
        assert_eq!(search["buckets"],first["buckets"]);
        assert!(query(&connection,json!({"created_after":0,"created_before":100,"offset":-1})).is_err());
        assert!(query(&connection,json!({"created_after":0,"created_before":100,"q":1})).is_err());
    }

    #[test]
    fn interval_is_exclusive_at_end_and_zero_filled_even_for_dst_days() {
        let connection=database();
        decision(&connection,"before",99,Some(100),None,false,json!({}),None);
        decision(&connection,"at-start",100,Some(101),None,false,json!({}),None);
        decision(&connection,"at-end",3700,Some(3701),Some("{}"),false,json!({}),None);
        decision(&connection,"failed",1000,Some(1001),Some("{}"),false,json!({}),None);
        decision(&connection,"pending",1900,None,None,false,json!({}),None);
        decision(&connection,"deleted",100,Some(101),None,true,json!({}),None);
        let activity=query(&connection,json!({"created_after":100,"created_before":3700})).unwrap();
        assert_eq!(activity["totals"],json!({"decisions":3,"completed":1,"failed":1,"pending":1}));
        assert_eq!(activity["buckets"],json!([{"start":100,"end":1000,"count":1,"failed":0},{"start":1000,"end":1900,"count":1,"failed":1},{"start":1900,"end":2800,"count":1,"failed":0},{"start":2800,"end":3700,"count":0,"failed":0}]));
        assert_eq!(activity["items"][0]["id"],"pending");
        assert_eq!(activity["items"].as_array().unwrap().len(),3);
        assert_eq!(activity["items"][0]["created_at_ms"],Value::Null);
        for hours in [23,25,26] {assert!(query(&connection,json!({"created_after":0,"created_before":hours*3600})).is_ok());}
        assert_eq!(query(&connection,json!({"created_after":0,"created_before":26*3600+1})).unwrap()["bucket_seconds"],BUCKET_SECONDS);
        assert_eq!(query(&connection,json!({"created_after":0,"created_before":31*DAY_SECONDS})).unwrap()["buckets"].as_array().unwrap().len(),31*96);
        let extreme=query(&connection,json!({"created_after":i64::MAX-100,"created_before":i64::MAX})).unwrap();
        assert_eq!(extreme["buckets"],json!([{"start":i64::MAX-100,"end":i64::MAX,"count":0,"failed":0}]));
    }

    #[test]
    fn missing_bounds_invalid_values_and_recording_off() {
        let connection=database();
        let empty=query(&connection,json!({"created_after":0,"created_before":900})).unwrap();
        assert_eq!(empty["recording_enabled"],false);
        assert_eq!(empty["totals"]["decisions"],0);
        assert!(empty["items"].as_array().unwrap().is_empty());
        for params in [json!({}),json!({"created_after":0}),json!({"created_after":-1,"created_before":1}),json!({"created_after":1,"created_before":1}),json!({"created_after":0,"created_before":1.5}),json!({"created_after":0,"created_before":1,"limit":101}),json!({"created_after":0,"created_before":1,"extra":true})] {
            assert!(query(&connection,params).unwrap_err().to_string().starts_with("invalid:"));
        }
    }

    #[test]
    fn latest_items_are_capped_and_project_small_fields_from_large_payloads() {
        let connection=database();
        for index in 0..105 {
            decision(&connection,&format!("id-{index:03}"),index,Some(index+1),None,false,json!({"state":"x".repeat(10000)}),Some(json!({"advice":{"uncertain":false,"recommendation":{"model":"model-a","reasoning_effort":"medium"}},"large":"z".repeat(100000)})));
        }
        let activity=query(&connection,json!({"created_after":0,"created_before":900,"limit":100})).unwrap();
        assert_eq!(activity["totals"]["decisions"],105);
        assert_eq!(activity["items"].as_array().unwrap().len(),100);
        assert_eq!(activity["items"][0]["id"],"id-104");
        assert_eq!(activity["items"][0]["summary"].as_str().unwrap().chars().count(),600);
        assert_eq!(activity["items"][0]["recommendation"],json!({"model":"model-a","reasoning_effort":"medium"}));
        assert!(activity.to_string().len()<150000);
    }

    #[test]
    fn latest_items_use_real_milliseconds_and_selected_range() {
        let connection=database();
        decision(&connection,"legacy",10,Some(11),None,false,json!({}),None);
        decision(&connection,"new-early",10,Some(11),None,false,json!({}),None);
        decision(&connection,"new-late",10,Some(11),None,false,json!({}),None);
        decision(&connection,"outside",11,Some(12),None,false,json!({}),None);
        connection.execute("UPDATE decisions SET created_at_ms=10001 WHERE id='new-early'",[]).unwrap();
        connection.execute("UPDATE decisions SET created_at_ms=10999 WHERE id='new-late'",[]).unwrap();
        let activity=query(&connection,json!({"created_after":10,"created_before":11})).unwrap();
        let items=activity["items"].as_array().unwrap();
        assert_eq!(items.iter().map(|item|item["id"].as_str().unwrap()).collect::<Vec<_>>(),vec!["new-late","new-early","legacy"]);
        assert_eq!(items[2]["created_at_ms"],Value::Null);
        assert_eq!(activity["items_scope"],"selected_recorded_decisions");
    }

    #[test]
    fn multiday_interval_uses_bounded_whole_day_buckets() {
        let connection=database();
        decision(&connection,"first",0,Some(1),None,false,json!({}),None);
        decision(&connection,"last",900*DAY_SECONDS-1,None,None,false,json!({}),None);
        let activity=query(&connection,json!({"created_after":0,"created_before":900*DAY_SECONDS})).unwrap();
        assert_eq!(activity["bucket_seconds"],3*DAY_SECONDS);
        assert_eq!(activity["buckets"].as_array().unwrap().len(),300);
        assert_eq!(activity["buckets"][0]["count"],1);
        assert_eq!(activity["buckets"][299]["count"],1);
        assert_eq!(activity["totals"]["decisions"],2);
    }

    #[test]
    fn result_plan_and_verified_assignments_are_distinct_from_request_context() {
        let connection=database();
        decision(&connection,"d",10,Some(20),None,false,json!({"state":"task","orchestration":{"enabled":true,"run_id":"requested-run","required_roles":["worker"]}}),Some(json!({"advice":{"assessment":{"complexity":{"choice":"high"},"risk":{"choice":"low"}},"uncertain":false},"orchestration_plan":{"contract":"orchestration_plan_v1","schema_version":1,"mode":"delegate","run_id":"planned-run","stage_id":"s","required_roles":["reviewer"]}})));
        decision(&connection,"pending",11,None,None,false,json!({"state":"pending","orchestration":{"enabled":true,"run_id":"requested-only"}}),None);
        for index in 0..6 {
            let event=format!("e{index}");
            let payload=if index==5 {json!({"execution":{"contract":"dispatch_receipt_v1","role":"worker","status":"started"}})}else{json!({})};
            connection.execute("INSERT INTO feedback_events VALUES(?1,?2)",params![event,payload.to_string()]).unwrap();
            let requested=json!({"model":"requested","reasoning_effort":"high"});
            let effective=if index==5 {json!({"model":"observed","reasoning_effort":"medium","model_observation":{"source":"host","verified":true}})}else{json!({"model":"unverified","reasoning_effort":"low","model_observation":{"source":"host","verified":false}})};
            connection.execute("INSERT INTO execution_attempts VALUES(?1,?2,'d','orchestrator',?3,?4,?5,?6)",params![index+1,event,format!("a{index}"),index,requested.to_string(),effective.to_string()]).unwrap();
        }
        let activity=query(&connection,json!({"created_after":0,"created_before":900})).unwrap();
        assert!(activity["items"][0]["orchestration"].is_null());
        assert_eq!(activity["items"][1]["orchestration"],json!({"mode":"delegate","run_id":"planned-run","stage_id":"s","required_roles":["reviewer"]}));
        assert_eq!(activity["items"][1]["assignments"].as_array().unwrap().len(),5);
        assert_eq!(activity["items"][1]["assignments_truncated"],true);
        assert_eq!(activity["items"][1]["assignments"][0]["status"],"started");
        assert_eq!(activity["items"][1]["assignments"][0]["role"],"worker");
        assert_eq!(activity["items"][1]["assignments"][0]["effective"],json!({"model":"observed","reasoning_effort":"medium"}));
        assert!(activity["items"][1]["assignments"][1]["effective"].is_null());
        assert!(activity["items"][1]["assignments"][1]["role"].is_null());
        assert!(activity["items"][1]["assignments"][1]["status"].is_null());
    }

    #[test]
    fn malformed_and_oversized_projection_fields_do_not_break_activity() {
        let connection=database();
        decision(&connection,"malformed",1,Some(2),None,false,json!({"advisor":{"role":12}}),Some(json!({"advice":{"uncertain":"perhaps","recommendation":{"model":"m".repeat(1000),"reasoning_effort":7}},"orchestration_plan":{"contract":"orchestration_plan_v1","schema_version":1,"mode":"delegate","run_id":"r","stage_id":"s","required_roles":["orchestrator"]}})));
        decision(&connection,"wrong-contract",2,Some(3),None,false,json!({}),Some(json!({"orchestration_plan":{"mode":"delegate","run_id":"r","stage_id":"s","required_roles":["worker"]}})));
        let activity=query(&connection,json!({"created_after":0,"created_before":900})).unwrap();
        assert!(activity["items"][0]["orchestration"].is_null());
        assert!(activity["items"][1]["role"].is_null());
        assert!(activity["items"][1]["uncertain"].is_null());
        assert!(activity["items"][1]["orchestration"].is_null());
        assert_eq!(activity["items"][1]["recommendation"]["model"].as_str().unwrap().chars().count(),256);
        assert!(activity["items"][1]["recommendation"]["reasoning_effort"].is_null());
    }

    #[tokio::test]
    async fn activity_runs_against_migrated_store_schema() {
        let root=tempfile::tempdir().unwrap();
        let store=crate::store::Store::open(root.path()).unwrap();
        let empty=store.call("activity",json!({"created_after":0,"created_before":900})).await.unwrap();
        assert_eq!(empty["recording_enabled"],false);
        store.call("settings/update",json!({"recording_enabled":true})).await.unwrap();
        store.call("decisions/begin",json!({"id":"real","request_id":"real","request":{"state":"real schema"}})).await.unwrap();
        store.call("decisions/finish",json!({"id":"real","result":{"advice":{"uncertain":false}},"error":null})).await.unwrap();
        let current=crate::protocol::now();
        let activity=store.call("activity",json!({"created_after":current-900,"created_before":current+900})).await.unwrap();
        assert_eq!(activity["recording_enabled"],true);
        assert_eq!(activity["items"][0]["id"],"real");
        assert_eq!(activity["items"][0]["status"],"completed");
        assert_eq!(activity["items"][0]["created_at_ms"].as_i64().unwrap()/1000,activity["items"][0]["created_at"].as_i64().unwrap());
        assert_eq!(activity["privacy_revision"],"none");
        store.call("decisions/delete",json!({"id":"real"})).await.unwrap();
        let after_delete=store.call("activity",json!({"created_after":0,"created_before":900})).await.unwrap();
        assert_ne!(after_delete["privacy_revision"],"none");
        store.call("decisions/begin",json!({"id":"next","request_id":"next","request":{"state":"next"}})).await.unwrap();
        let after_begin=store.call("activity",json!({"created_after":0,"created_before":900})).await.unwrap();
        assert_eq!(after_begin["privacy_revision"],after_delete["privacy_revision"]);
    }

    #[test]
    fn privacy_revision_changes_only_for_privacy_events() {
        let connection=database();
        let read=||query(&connection,json!({"created_after":0,"created_before":900})).unwrap()["privacy_revision"].clone();
        assert_eq!(read(),"none");
        connection.execute("INSERT INTO events VALUES(1,'decision.created','{}',10)",[]).unwrap();
        assert_eq!(read(),"none");
        connection.execute("INSERT INTO events VALUES(2,'decision.deleted','{\"id\":\"secret\"}',11)",[]).unwrap();
        let deleted=read();
        assert_ne!(deleted,"none");
        assert!(!deleted.as_str().unwrap().contains("secret"));
        connection.execute("INSERT INTO events VALUES(3,'decision.finished','{}',12)",[]).unwrap();
        assert_eq!(read(),deleted);
        connection.execute("INSERT INTO events VALUES(4,'backup.restored','{\"safety_backup_id\":\"backup-a\"}',13)",[]).unwrap();
        assert_ne!(read(),deleted);
    }
}
