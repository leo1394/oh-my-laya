//! Revisable read-only observations over immutable evidence, never training labels.
use crate::usage;
use anyhow::Result;
use rusqlite::{params,Connection};
use serde_json::{json,Value};
use std::collections::{BTreeMap,HashSet};

pub(crate) fn reused_streams(connection:&Connection)->Result<HashSet<String>> {
    let mut query=connection.prepare("SELECT stream FROM (SELECT json_extract(f.payload_json,'$.usage_stream_id') AS stream,f.decision_id,f.attempt_ref FROM feedback_events f JOIN decisions d ON d.id=f.decision_id WHERE f.kind='usage' AND d.deleted_at IS NULL AND json_type(f.payload_json,'$.usage_stream_id')='text' GROUP BY stream,f.decision_id,f.attempt_ref) GROUP BY stream HAVING COUNT(*)>1")?;
    let rows=query.query_map([],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<HashSet<_>>>()?;
    Ok(rows)
}

pub(crate) fn observe(connection:&Connection,decision:&Value,events:&[Value],reused:&HashSet<String>)->Result<Vec<Value>> {
    let mut groups:BTreeMap<&str,Vec<&Value>>=BTreeMap::new();
    for event in events {
        if event["kind"]=="run_manifest" {continue;}
        if let Some(attempt)=event["attempt_ref"].as_str() {groups.entry(attempt).or_default().push(event);}
    }
    let mut observations=Vec::new();
    for (attempt,group) in groups {
        let assignments=group.iter().copied().filter(|e|e["kind"]=="assignment").collect::<Vec<_>>();
        let latest=assignments.last().copied().unwrap_or(&Value::Null);
        let dispatch=assignments.iter().rev().find(|e|e["payload"]["execution"]["contract"]=="dispatch_receipt_v1").copied().unwrap_or(&Value::Null);
        let execution=&dispatch["payload"]["execution"];
        let versioned=!execution.is_null();
        let identity_conflict=if versioned {
            connection.query_row("SELECT COUNT(*)>1 FROM (SELECT f.decision_id,f.attempt_ref FROM feedback_events f JOIN decisions d ON d.id=f.decision_id WHERE f.kind='assignment' AND json_extract(f.payload_json,'$.execution.contract')='dispatch_receipt_v1' AND d.deleted_at IS NULL AND json_extract(f.payload_json,'$.execution.run_id')=?1 AND json_extract(f.payload_json,'$.execution.stage_id')=?2 AND json_extract(f.payload_json,'$.execution.ordinal')=?3 GROUP BY f.decision_id,f.attempt_ref)",params![execution["run_id"].as_str(),execution["stage_id"].as_str(),execution["ordinal"].as_u64()],|row|row.get::<_,bool>(0))?
        }else{false};
        let effective=&latest["payload"]["effective"];
        let verified=effective["model_observation"]["source"]=="host"&&effective["model_observation"]["verified"]==true;
        let pairs=assignments.iter().filter_map(|e|{
            let pair=&e["payload"]["effective"];
            pair["model"].as_str().map(|model|(model,pair["reasoning_effort"].to_string()))
        }).collect::<HashSet<_>>();
        let revisions=assignments.iter().filter_map(|e|e["payload"]["effective"]["model_observation"]["model_revision"].as_str()).collect::<HashSet<_>>();
        let mixed=pairs.len()>1||revisions.len()>1||assignments.iter().any(|e|e["payload"]["mixed_configuration"]==true);
        let mut initial_scores=Vec::new();
        let mut revised_scores=Vec::new();
        let mut quality_events=Vec::new();
        let flagged=group.iter().any(|event|decision["review_trigger_event_ids"].as_array().is_some_and(|ids|ids.contains(&event["event_id"])));
        for event in &group {
            for (ordinal,score) in event["payload"]["scores"].as_array().into_iter().flatten().enumerate() {
                let reference=json!({"event_id":event["event_id"],"ordinal":ordinal,"role":event["source"]["role"],
                    "dimension":score["dimension"],"value":score["value"],"phase":score["phase"],
                    "source_sequence":score["source_sequence"],"supersedes_event_id":score["supersedes_event_id"]});
                if score["phase"]=="initial" {initial_scores.push(reference);}else{revised_scores.push(reference);}
            }
            if matches!(event["kind"].as_str(),Some("test"|"review"|"outcome"|"user_choice")) {
                quality_events.push(json!({"event_id":event["event_id"],"kind":event["kind"]}));
            }
        }
        let outcomes=group.iter().filter(|e|e["kind"]=="outcome"&&e["payload"]["execution"]["contract"]=="attempt_outcome_v1").copied().collect::<Vec<_>>();
        let terminal=outcomes.last().copied().unwrap_or(&Value::Null);
        let usage_events=group.iter().copied().filter(|e|e["kind"]=="usage").collect::<Vec<_>>();
        let selection=usage::select(&usage_events.iter().map(|e|&e["payload"]).collect::<Vec<_>>(),reused);
        let selected_event=selection.report.and_then(|report|usage_events.iter().find(|e|&e["payload"]==report).copied());
        let usage_status=if selection.report.is_some(){"available"}else{"unavailable"};
        let estimate_refs=events.iter().filter(|e|e["kind"]=="run_manifest"&&e["payload"]["scenario"].is_object()
            &&e["payload"]["segments"].as_array().is_some_and(|segments|segments.iter().any(|s|s["decision_id"]==decision["id"]&&s["attempt_ref"]==attempt)))
            .map(|e|e["event_id"].clone()).collect::<Vec<_>>();
        observations.push(json!({
            "contract":"efficiency_observation_v1","decision_id":decision["id"],"attempt_ref":attempt,
            "observed_through_receive_sequence":group.iter().filter_map(|e|e["receive_sequence"].as_u64()).max(),
            "identity":{"status":if identity_conflict{"conflict"}else if versioned{"reported"}else{"unknown"},
                "run_id":execution["run_id"],"stage_id":execution["stage_id"],"ordinal":execution["ordinal"],
                "role":execution["role"],"attempt_kind":execution["attempt_kind"],"dispatch_status":execution["status"],
                "dispatch_event_id":dispatch["event_id"],"native_execution_ref":execution["native_execution_ref"]},
            "task_family":decision["request"]["advisor"]["task_family"],
            "policy":{"attempts":execution["policy_version"],"enforcement":execution["enforcement"],
                "rules_version":decision["result"]["meta"]["rules_version"],"memory_version":decision["result"]["meta"]["memory_version"],
                "selected_case_ids":decision["result"]["meta"]["case_ids"],"received_case_ids":decision["result"]["meta"]["worker_case_ids"],
                "memory_receipt":decision["result"]["meta"]["memory_receipt"]},
            "configuration":{"status":if mixed{"mixed"}else if verified{"reported_verified"}else{"unknown"},
                "effective":if verified{effective.clone()}else{Value::Null},"assignment_event_ids":assignments.iter().map(|e|e["event_id"].clone()).collect::<Vec<_>>()},
            "context":{"isolation":execution["context_isolation"],"evidence_ref":execution["context_evidence_ref"],"input_size":execution["input_size"]},
            "quality":{"flagged":flagged,"initial_scores":initial_scores,"revised_scores":revised_scores,"event_refs":quality_events},
            "outcome":{"event_id":terminal["event_id"],"result":terminal["payload"]["execution"]["result"],
                "failure_class":terminal["payload"]["execution"]["failure_class"],"duration_ms":terminal["payload"]["execution"]["duration_ms"]},
            "usage":{"status":usage_status,"event_id":selected_event.map(|e|e["event_id"].clone()),
                "report":selection.report,"exclusion_reasons":selection.exclusions,"scope":"verified_non_overlapping_attempt_streams"},
            "estimate_refs":estimate_refs,"estimate_coverage":"decision_local_references_only",
            "complete_task_coverage":false,"automatic_training_label":false
        }));
    }
    Ok(observations)
}

pub(crate) fn summary(connection:&Connection,reused:&HashSet<String>,after:Option<i64>,before:Option<i64>)->Result<Value> {
    let sql=format!("{} SELECT id,json_object('id',id,'review_trigger_event_ids',json(trigger_event_ids)) FROM review_queue WHERE deleted_at IS NULL AND (?1 IS NULL OR created_at>=?1) AND (?2 IS NULL OR created_at<=?2) AND EXISTS(SELECT 1 FROM feedback_events f WHERE f.decision_id=review_queue.id AND f.kind<>'run_manifest') ORDER BY id",crate::store::review_queue_cte());
    let mut decisions=connection.prepare(&sql)?;
    let rows=decisions.query_map(params![after,before],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?;
    let mut observed=0u64;
    let mut versioned=0u64;
    let mut conflicts=0u64;
    let mut delegated=0u64;
    let mut repairs=0u64;
    let mut upgrades=0u64;
    let mut initial_scored=0u64;
    let mut flagged=0u64;
    let mut usage_covered=0u64;
    let mut outcomes=BTreeMap::<String,u64>::new();
    let mut runs=HashSet::new();
    for row in rows {
        let (id,decision)=row?;
        let mut query=connection.prepare("SELECT event_id,attempt_ref,kind,source_json,payload_json,receive_sequence FROM feedback_events WHERE decision_id=?1 ORDER BY receive_sequence")?;
        let events=query.query_map([id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,u64>(5)?)))?
            .map(|row|{let (event_id,attempt,kind,source,payload,sequence)=row?;Ok(json!({"event_id":event_id,"attempt_ref":attempt,"kind":kind,"source":serde_json::from_str::<Value>(&source)?,"payload":serde_json::from_str::<Value>(&payload)?,"receive_sequence":sequence}))}).collect::<Result<Vec<_>>>()?;
        for observation in observe(connection,&serde_json::from_str::<Value>(&decision)?,&events,reused)? {
            observed+=1;
            if observation["identity"]["status"]=="reported" {
                versioned+=1;
                if let Some(run)=observation["identity"]["run_id"].as_str(){runs.insert(run.to_string());}
                if observation["identity"]["dispatch_status"]=="started"&&observation["identity"]["role"]!="orchestrator" {delegated+=1;}
                repairs+=u64::from(observation["identity"]["attempt_kind"]=="repair");
                upgrades+=u64::from(observation["identity"]["attempt_kind"]=="upgrade");
            }else if observation["identity"]["status"]=="conflict" {conflicts+=1;}
            initial_scored+=u64::from(observation["quality"]["initial_scores"].as_array().is_some_and(|a|a.iter().any(|s|s["value"].is_number())));
            flagged+=u64::from(observation["quality"]["flagged"]==true);
            usage_covered+=u64::from(observation["usage"]["status"]=="available");
            *outcomes.entry(observation["outcome"]["result"].as_str().unwrap_or("unknown").to_string()).or_default()+=1;
        }
    }
    Ok(json!({"contract":"efficiency_summary_v1","scope":"recorded_attempts","complete_task_coverage":false,
        "observed_attempts":observed,"versioned_attempts":versioned,"identity_conflicts":conflicts,
        "reported_runs":runs.len(),"delegated_attempts":delegated,"repair_attempts":repairs,"upgrade_attempts":upgrades,
        "initial_scored_attempts":initial_scored,"flagged_attempts":flagged,"reported_outcomes":outcomes,
        "usage_covered_attempts":usage_covered,"usage_missing_attempts":observed-usage_covered,
        "created_after":after,"created_before":before}))
}
