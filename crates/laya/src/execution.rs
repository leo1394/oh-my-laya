//! Versioned execution telemetry, not an authority to dispatch agents.
use anyhow::{anyhow,bail,Result};
use rusqlite::{Connection,OptionalExtension};
use serde_json::Value;
use std::collections::HashSet;

fn text<'a>(value:&'a Value,key:&str,max:usize)->Result<&'a str> {
    let value=value.as_str().ok_or_else(||anyhow!("invalid: execution {key} must be a string"))?;
    if value.trim().is_empty()||value.len()>max {bail!("invalid: execution {key} length");}
    Ok(value)
}

fn nullable_text(value:&Value,key:&str)->Result<()> {
    if !value.is_null() {text(value,key,512)?;}
    Ok(())
}

fn choice(value:&Value,key:&str,options:&[&str])->Result<()> {
    if !options.contains(&text(value,key,64)?) {bail!("invalid: execution {key} enum");}
    Ok(())
}

fn fields(value:&Value,required:&[&str])->Result<()> {
    let map=value.as_object().ok_or_else(||anyhow!("invalid: execution must be an object"))?;
    if map.len()!=required.len()||required.iter().any(|key|!map.contains_key(*key)) {
        bail!("invalid: execution fields must match the versioned contract");
    }
    Ok(())
}

fn refs(value:&Value)->Result<()> {
    let values=value.as_array().ok_or_else(||anyhow!("invalid: execution references must be an array"))?;
    if values.len()>32 {bail!("invalid: too many execution references");}
    let mut seen=HashSet::new();
    for value in values {if !seen.insert(text(value,"event reference",128)?) {bail!("invalid: duplicate execution reference");}}
    Ok(())
}

pub(crate) fn validate(kind:&str,payload:&Value)->Result<()> {
    let Some(value)=payload.get("execution") else{return Ok(());};
    let mut required=vec!["contract","run_id","stage_id","ordinal","policy_version","enforcement"];
    match kind {
        "assignment"=>required.extend(["role","attempt_kind","status","native_execution_ref","context_isolation","context_evidence_ref","input_size"]),
        "outcome"=>required.extend(["dispatch_event_id","result","failure_class","first_feedback_event_id","test_event_ids","review_event_ids","usage_event_ids","duration_ms"]),
        _=>bail!("invalid: execution contract is only valid on assignment/outcome"),
    }
    fields(value,&required)?;
    let contract=if kind=="assignment" {"dispatch_receipt_v1"}else{"attempt_outcome_v1"};
    if value["contract"]!=contract||value["policy_version"]!="bounded-attempts-v1"||value["enforcement"]!="advisory" {
        bail!("invalid: unsupported execution contract, policy or enforcement");
    }
    for key in ["run_id","stage_id"] {text(&value[key],key,128)?;}
    // Overspending is evidence to retain, not a reason to discard actual usage.
    if !value["ordinal"].as_u64().is_some_and(|v|(1..=1_000_000).contains(&v)) {bail!("invalid: execution ordinal");}
    if kind=="assignment" {
        choice(&value["role"],"role",&["orchestrator","explorer","worker","tester","researcher","reviewer"])?;
        choice(&value["attempt_kind"],"attempt_kind",&["initial","repair","upgrade","manual"])?;
        choice(&value["status"],"status",&["started","failed","unknown"])?;
        nullable_text(&value["native_execution_ref"],"native_execution_ref")?;
        if value["status"]=="started"&&value["native_execution_ref"].is_null() {bail!("invalid: started dispatch requires native execution reference");}
        choice(&value["context_isolation"],"context_isolation",&["isolated","unsupported","unknown"])?;
        nullable_text(&value["context_evidence_ref"],"context_evidence_ref")?;
        if value["context_isolation"]=="isolated"&&value["context_evidence_ref"].is_null() {bail!("invalid: isolated context requires host evidence reference");}
        let size=&value["input_size"];
        fields(size,&["value","unit","source"])?;
        choice(&size["unit"],"input size unit",&["native_tokens","bytes","characters","unknown"])?;
        nullable_text(&size["source"],"input size source")?;
        if !size["value"].is_null()&&(!size["value"].is_u64()||size["unit"]=="unknown"||size["source"].is_null()) {
            bail!("invalid: measured input size requires a nonnegative integer, known unit and source");
        }
    } else {
        text(&value["dispatch_event_id"],"dispatch_event_id",128)?;
        choice(&value["result"],"result",&["success","failure","partial","cancelled","unknown"])?;
        if payload.get("outcome").is_none()&&payload.get("status").is_none() {bail!("invalid: versioned outcome requires a compatible outcome/status alias");}
        for alias in ["outcome","status"] {if payload.get(alias).is_some_and(|v|v!=&value["result"]) {bail!("invalid: outcome alias contradicts execution result");}}
        if !value["failure_class"].is_null() {choice(&value["failure_class"],"failure_class",&["acceptance","capability","infrastructure","authorization","risk","user_decision","cancelled","unknown"])?;}
        if value["result"]=="success"&&!value["failure_class"].is_null() {bail!("invalid: successful outcome cannot have failure class");}
        nullable_text(&value["first_feedback_event_id"],"first_feedback_event_id")?;
        if !value["first_feedback_event_id"].is_null() {text(&value["first_feedback_event_id"],"first_feedback_event_id",128)?;}
        for key in ["test_event_ids","review_event_ids","usage_event_ids"] {refs(&value[key])?;}
        if !value["duration_ms"].is_null()&&!value["duration_ms"].is_u64() {bail!("invalid: execution duration_ms");}
    }
    Ok(())
}

fn referenced(connection:&Connection,id:&str,event:&Value,kind:Option<&str>)->Result<Value> {
    if event["event_id"]==id {bail!("invalid: execution event cannot reference itself");}
    let row:Option<(String,String,String,String)>=connection.query_row(
        "SELECT decision_id,attempt_ref,kind,payload_json FROM feedback_events WHERE event_id=?1",[id],
        |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional()?;
    let Some((decision,attempt,actual_kind,payload))=row else{bail!("waiting_dependency: execution reference {id} has not arrived");};
    if event["decision_id"]!=decision||event["attempt_ref"]!=attempt||kind.is_some_and(|kind|kind!=actual_kind) {
        bail!("invalid: execution reference does not match decision, attempt or kind");
    }
    Ok(serde_json::from_str(&payload)?)
}

pub(crate) fn validate_references(connection:&Connection,event:&Value)->Result<()> {
    let Some(value)=event["payload"].get("execution") else{return Ok(());};
    let decision=event["decision_id"].as_str().unwrap();
    let attempt=event["attempt_ref"].as_str().unwrap();
    let kind=event["kind"].as_str().unwrap();
    let mut query=connection.prepare("SELECT kind,payload_json FROM feedback_events WHERE decision_id=?1 AND attempt_ref=?2 AND json_type(payload_json,'$.execution')='object'")?;
    for row in query.query_map([decision,attempt],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))? {
        let (previous_kind,previous)=row?;
        let previous:Value=serde_json::from_str(&previous)?;
        for key in ["run_id","stage_id","ordinal","policy_version"] {
            if previous["execution"][key]!=value[key] {bail!("conflict: execution attempt identity changed");}
        }
        if kind=="assignment"&&previous_kind=="assignment"&&previous["execution"]["role"]!=value["role"] {bail!("conflict: execution attempt role changed");}
        if kind=="outcome"&&previous_kind=="outcome" {bail!("conflict: execution attempt already has terminal outcome");}
    }
    if kind=="outcome" {
        let dispatch=referenced(connection,value["dispatch_event_id"].as_str().unwrap(),event,Some("assignment"))?;
        if dispatch["execution"]["contract"]!="dispatch_receipt_v1" {bail!("invalid: outcome requires versioned dispatch receipt");}
        for key in ["run_id","stage_id","ordinal","policy_version"] {
            if dispatch["execution"][key]!=value[key] {bail!("invalid: outcome does not match dispatch identity");}
        }
        for (key,kind) in [("test_event_ids","test"),("review_event_ids","review"),("usage_event_ids","usage")] {
            for id in value[key].as_array().unwrap() {referenced(connection,id.as_str().unwrap(),event,Some(kind))?;}
        }
        if let Some(id)=value["first_feedback_event_id"].as_str() {
            let first=referenced(connection,id,event,None)?;
            if !first["scores"].as_array().is_some_and(|scores|scores.iter().any(|score|score["phase"]=="initial")) {
                bail!("invalid: first feedback reference must contain an initial score");
            }
        }
    }
    Ok(())
}
