use serde_json::{json,Value};
use std::collections::{HashMap,HashSet};

const MAX_SAFE_INTEGER:u128=9_007_199_254_740_991;

#[derive(Clone)]
pub struct ManifestRecord {
    pub event_id:String,
    pub payload:Value,
}

#[derive(Clone)]
pub struct UsageRecord {
    pub event_id:String,
    pub decision_id:String,
    pub attempt_ref:String,
    pub payload:Value,
}

struct Candidate {
    event_id:String,
    run_id:String,
    host_root_ref:String,
    model:String,
    effort:Value,
    input_source:String,
    coverage:&'static str,
    segments:Vec<(String,String,String)>,
    baseline:[u128;3],
}

pub fn summarize(manifests:Vec<ManifestRecord>,usages:Vec<UsageRecord>)->Value {
    let manifests=latest_lifecycle_snapshots(manifests,&usages);
    let raw_collisions=raw_manifest_collisions(&manifests);
    let mut excluded=0u64;
    let mut candidates=Vec::new();
    for manifest in manifests.iter().cloned() {
        match candidate(manifest) {
            Some(candidate)=>candidates.push(candidate),
            None=>excluded+=1,
        }
    }
    let mut rejected=HashSet::new();
    reject_manifest_collisions(&candidates,&mut rejected);
    for (index,candidate) in candidates.iter().enumerate() {
        if raw_collisions.0.contains(&candidate.run_id)||raw_collisions.1.contains(&candidate.host_root_ref)||candidate.segments.iter().any(|(decision,attempt,_)|raw_collisions.2.contains(&(decision.clone(),attempt.clone()))) {rejected.insert(index);}
    }
    let by_event=usages.iter().map(|usage|(usage.event_id.as_str(),usage)).collect::<HashMap<_,_>>();
    let raw_stream_collisions=raw_manifest_stream_collisions(&manifests,&by_event);
    let mut by_owner:HashMap<(&str,&str),Vec<&UsageRecord>>=HashMap::new();
    let mut stream_owners:HashMap<&str,HashSet<(&str,&str)>>=HashMap::new();
    for usage in &usages {
        let owner=(usage.decision_id.as_str(),usage.attempt_ref.as_str());
        by_owner.entry(owner).or_default().push(usage);
        if let Some(stream)=usage.payload.get("usage_stream_id").and_then(Value::as_str) {
            stream_owners.entry(stream).or_default().insert(owner);
        }
    }
    for (index,candidate) in candidates.iter().enumerate() {
        if rejected.contains(&index){continue;}
        let mut streams=HashSet::new();
        for (decision,attempt,event_id) in &candidate.segments {
            let Some(usage)=by_event.get(event_id.as_str()).copied() else{rejected.insert(index);break;};
            if usage.decision_id!=*decision||usage.attempt_ref!=*attempt||!eligible(usage) {
                rejected.insert(index);break;
            }
            if !selected_report(usage,by_owner.get(&(decision.as_str(),attempt.as_str())).map(Vec::as_slice).unwrap_or(&[])) {
                rejected.insert(index);break;
            }
            if let Some(stream)=usage.payload.get("usage_stream_id").and_then(Value::as_str) {
                if !streams.insert(stream)||stream_owners.get(stream).is_some_and(|owners|owners.len()!=1)||raw_stream_collisions.contains(stream) {
                    rejected.insert(index);break;
                }
            }
        }
    }
    reject_cross_run_streams(&candidates,&by_event,&mut rejected);
    excluded+=rejected.len() as u64;
    let mut baseline=[0u128;3];
    let mut actual=0u128;
    let mut runs=Vec::new();
    let mut partial_coverage=false;
    let mut included=0u64;
    let mut overflow=false;
    for (index,candidate) in candidates.iter().enumerate() {
        if rejected.contains(&index){continue;}
        let mut run_actual=0u128;
        for (_,_,event_id) in &candidate.segments {
            let tokens=by_event[event_id.as_str()].payload["total_tokens"].as_u64().unwrap() as u128;
            run_actual=match run_actual.checked_add(tokens){Some(value)=>value,None=>{overflow=true;0}};
        }
        if overflow{break;}
        actual=match actual.checked_add(run_actual){Some(value)=>value,None=>{overflow=true;0}};
        for scenario in 0..3 {
            baseline[scenario]=match baseline[scenario].checked_add(candidate.baseline[scenario]){Some(value)=>value,None=>{overflow=true;0}};
        }
        if actual>MAX_SAFE_INTEGER||baseline.iter().any(|value|*value>MAX_SAFE_INTEGER){overflow=true;break;}
        included+=1;
        partial_coverage|=candidate.coverage=="partial";
        runs.push(json!({
            "run_id":candidate.run_id,
            "orchestrator_model":candidate.model,
            "reasoning_effort":candidate.effort,
            "input_source":candidate.input_source,
            "coverage":candidate.coverage,
            "manifest_event_ref":candidate.event_id
        }));
    }
    if overflow {
        excluded+=candidates.len() as u64-rejected.len() as u64;
        included=0;
        runs.clear();
    }
    let available=included>0&&!overflow;
    let baseline_value=available.then(||range(baseline));
    let saved=available.then(||json!({
        "low":signed(baseline[0],actual),
        "central":signed(baseline[1],actual),
        "high":signed(baseline[2],actual)
    }));
    let status=if !available{"unavailable"}else if excluded>0||partial_coverage{"partial"}else{"available"};
    json!({
        "status":status,
        "estimator_version":"scenario-v1",
        "saved":saved,
        "baseline":baseline_value,
        "actual_total":available.then_some(actual as u64),
        "included_runs":included,
        "excluded_runs":excluded,
        "assumptions":{"retention":[0.25,0.5,1.0],"output_multiplier":[0.8,1.0,1.2]},
        "scope":"host_only",
        "excluded_components":["local_laya_inference"],
        "runs":runs
    })
}

fn latest_lifecycle_snapshots(manifests:Vec<ManifestRecord>,usages:&[UsageRecord])->Vec<ManifestRecord> {
    let by_event=usages.iter().map(|usage|(usage.event_id.as_str(),usage)).collect::<HashMap<_,_>>();
    let sequences=|record:&ManifestRecord|->Option<HashMap<(String,String),(String,u64)>> {
        let p=&record.payload;
        if !p["scenario"]["input_source"].as_str()?.contains("; lifecycle-snapshot-v1;")||!p["run_id"].as_str()?.starts_with("codex-lifecycle:"){return None;}
        let mut values=HashMap::new();
        for segment in p["segments"].as_array()? {
            let usage=*by_event.get(segment["usage_event_id"].as_str()?)?;
            if !eligible(usage)||usage.decision_id!=segment["decision_id"].as_str()?||usage.attempt_ref!=segment["attempt_ref"].as_str()?{return None;}
            let key=(usage.decision_id.clone(),usage.attempt_ref.clone());
            if values.insert(key,(usage.payload["usage_stream_id"].as_str()?.to_string(),usage.payload["source_sequence"].as_u64()?)).is_some(){return None;}
        }
        (!values.is_empty()).then_some(values)
    };
    let versions=manifests.iter().map(sequences).collect::<Vec<_>>();
    let mut superseded=HashSet::new();
    for (i,old) in manifests.iter().enumerate() {
        let Some(old_versions)=&versions[i] else{continue;};
        for (j,new) in manifests.iter().enumerate() {
            if i==j||old.payload["run_id"]!=new.payload["run_id"]||old.payload["host_root_ref"]!=new.payload["host_root_ref"]{continue;}
            if ["orchestrator_model","reasoning_effort","input_source"].iter().any(|key|old.payload["scenario"][*key]!=new.payload["scenario"][*key]){continue;}
            let Some(new_versions)=&versions[j] else{continue;};
            let covers=old_versions.iter().all(|(key,(stream,seq))|new_versions.get(key).is_some_and(|(next_stream,next_seq)|stream==next_stream&&next_seq>=seq));
            let grows=new_versions.len()>old_versions.len()||old_versions.iter().any(|(key,(_,seq))|new_versions.get(key).is_some_and(|(_,next_seq)|next_seq>seq));
            if covers&&grows {superseded.insert(i);break;}
        }
    }
    manifests.into_iter().enumerate().filter_map(|(i,m)|(!superseded.contains(&i)).then_some(m)).collect()
}

fn candidate(record:ManifestRecord)->Option<Candidate> {
    let payload=record.payload.as_object()?;
    if payload.get("mode")?.as_str()?!="laya"{return None;}
    if payload.get("meter")?.get("identity")?.as_str().is_none(){return None;}
    let scenario=payload.get("scenario")?.as_object()?;
    let model=scenario.get("orchestrator_model")?.as_str()?.to_string();
    let effort=scenario.get("reasoning_effort")?.clone();
    let input_source=scenario.get("input_source")?.as_str()?.to_string();
    let initial=scenario.get("initial_context_tokens")?.as_u64()? as u128;
    let stages=scenario.get("stages")?.as_array()?;
    let mut parsed=Vec::new();
    for stage in stages {
        parsed.push((stage.get("context_growth_tokens")?.as_u64()? as u128,stage.get("work_output_tokens")?.as_u64()? as u128,stage.get("passes")?.as_u64()? as u128));
    }
    let baseline=estimate(initial,&parsed)?;
    let segments=payload.get("segments")?.as_array()?.iter().map(|segment|Some((segment.get("decision_id")?.as_str()?.to_string(),segment.get("attempt_ref")?.as_str()?.to_string(),segment.get("usage_event_id")?.as_str()?.to_string()))).collect::<Option<Vec<_>>>()?;
    let coverage=if payload.get("coverage")?.get("status")?.as_str()?=="complete"{"reported"}else{"partial"};
    Some(Candidate {event_id:record.event_id,run_id:payload.get("run_id")?.as_str()?.to_string(),host_root_ref:payload.get("host_root_ref")?.as_str()?.to_string(),model,effort,input_source,coverage,segments,baseline})
}

fn estimate(initial:u128,stages:&[(u128,u128,u128)])->Option<[u128;3]> {
    let assumptions=[(20u128,5u128,16u128),(2,1,2),(5,5,6)];
    let mut numerators=[0u128;3];
    let mut prior=0u128;
    for &(growth,output,passes) in stages {
        let direct=initial.checked_add(growth)?;
        for (index,(denominator,retention_scaled,output_scaled)) in assumptions.into_iter().enumerate() {
            let direct=direct.checked_mul(passes)?.checked_mul(denominator)?;
            let retained=prior.checked_mul(passes)?.checked_mul(retention_scaled)?;
            let generated=output.checked_mul(output_scaled)?;
            numerators[index]=numerators[index].checked_add(direct)?.checked_add(retained)?.checked_add(generated)?;
        }
        prior=prior.checked_add(growth)?.checked_add(output)?;
    }
    let mut totals=[0u128;3];
    for (index,(denominator,_,_)) in assumptions.into_iter().enumerate() {totals[index]=numerators[index].checked_add(denominator/2)?/denominator;}
    Some(totals)
}

fn eligible(usage:&UsageRecord)->bool {
    let total=usage.payload.get("total_tokens").and_then(Value::as_u64);
    let components_consistent=match (total,usage.payload.get("input_tokens").and_then(Value::as_u64),usage.payload.get("output_tokens").and_then(Value::as_u64)) {
        (Some(total),Some(input),Some(output))=>input.checked_add(output)==Some(total),
        _=>true,
    };
    usage.payload.get("source_verified")==Some(&Value::Bool(true))
        &&usage.payload.get("scope").and_then(Value::as_str)==Some("attempt")
        &&usage.payload.get("overlap_status").and_then(Value::as_str)==Some("non_overlapping")
        &&usage.payload.get("checkpoint").and_then(Value::as_str).is_some()
        &&total.is_some()
        &&components_consistent
}

fn selected_report(selected:&UsageRecord,reports:&[&UsageRecord])->bool {
    let ordered=reports.iter().filter(|report|report.payload.get("usage_stream_id").is_some()).copied().collect::<Vec<_>>();
    let legacy=reports.iter().filter(|report|report.payload.get("usage_stream_id").is_none()&&report.payload.get("scope").and_then(Value::as_str)==Some("attempt")).copied().collect::<Vec<_>>();
    if !ordered.is_empty() {
        if !legacy.is_empty(){return false;}
        let streams=ordered.iter().filter_map(|report|report.payload.get("usage_stream_id").and_then(Value::as_str)).collect::<HashSet<_>>();
        if streams.len()!=1{return false;}
        let mut sequences=HashMap::new();
        for report in ordered {
            let Some(sequence)=report.payload.get("source_sequence").and_then(Value::as_u64) else{return false;};
            if let Some(existing)=sequences.insert(sequence,report) {
                if existing.payload!=report.payload{return false;}
            }
        }
        let Some((max_sequence,report))=sequences.into_iter().max_by_key(|(sequence,_)|*sequence) else{return false;};
        selected.payload.get("source_sequence").and_then(Value::as_u64)==Some(max_sequence)&&selected.payload==report.payload
    } else {
        !legacy.is_empty()&&legacy.iter().all(|report|legacy_signature(&report.payload)==legacy_signature(&selected.payload))
    }
}

fn legacy_signature(payload:&Value)->Value {
    json!({"total_tokens":payload.get("total_tokens"),"input_tokens":payload.get("input_tokens"),"output_tokens":payload.get("output_tokens"),"source":payload.get("source"),"source_verified":payload.get("source_verified"),"scope":payload.get("scope"),"checkpoint":payload.get("checkpoint"),"parent_scope":payload.get("parent_scope"),"overlap_status":payload.get("overlap_status")})
}

fn reject_manifest_collisions(candidates:&[Candidate],rejected:&mut HashSet<usize>) {
    let mut runs:HashMap<&str,Vec<usize>>=HashMap::new();
    let mut roots:HashMap<&str,Vec<usize>>=HashMap::new();
    let mut identities:HashMap<(&str,&str),Vec<usize>>=HashMap::new();
    for (index,candidate) in candidates.iter().enumerate() {
        runs.entry(&candidate.run_id).or_default().push(index);
        roots.entry(&candidate.host_root_ref).or_default().push(index);
        for (decision,attempt,_) in &candidate.segments {identities.entry((decision,attempt)).or_default().push(index);}
    }
    for indexes in runs.values().chain(roots.values()).chain(identities.values()) {
        if indexes.len()>1 {rejected.extend(indexes);}
    }
}

fn raw_manifest_collisions(manifests:&[ManifestRecord])->(HashSet<String>,HashSet<String>,HashSet<(String,String)>) {
    let mut runs:HashMap<String,u64>=HashMap::new();
    let mut roots:HashMap<String,u64>=HashMap::new();
    let mut identities:HashMap<(String,String),u64>=HashMap::new();
    for manifest in manifests {
        if manifest.payload.get("mode").and_then(Value::as_str)!=Some("laya"){continue;}
        if let Some(run)=manifest.payload.get("run_id").and_then(Value::as_str){*runs.entry(run.to_string()).or_default()+=1;}
        if let Some(root)=manifest.payload.get("host_root_ref").and_then(Value::as_str){*roots.entry(root.to_string()).or_default()+=1;}
        if let Some(segments)=manifest.payload.get("segments").and_then(Value::as_array) {
            for segment in segments {
                if let (Some(decision),Some(attempt))=(segment.get("decision_id").and_then(Value::as_str),segment.get("attempt_ref").and_then(Value::as_str)) {*identities.entry((decision.to_string(),attempt.to_string())).or_default()+=1;}
            }
        }
    }
    (runs.into_iter().filter_map(|(key,count)|(count>1).then_some(key)).collect(),roots.into_iter().filter_map(|(key,count)|(count>1).then_some(key)).collect(),identities.into_iter().filter_map(|(key,count)|(count>1).then_some(key)).collect())
}

fn raw_manifest_stream_collisions(manifests:&[ManifestRecord],by_event:&HashMap<&str,&UsageRecord>)->HashSet<String> {
    let mut owners:HashMap<String,HashSet<&str>>=HashMap::new();
    for manifest in manifests {
        if manifest.payload.get("mode").and_then(Value::as_str)!=Some("laya"){continue;}
        let event_ref=manifest.event_id.as_str();
        if let Some(segments)=manifest.payload.get("segments").and_then(Value::as_array) {
            for segment in segments {
                if let Some(stream)=segment.get("usage_event_id").and_then(Value::as_str).and_then(|event|by_event.get(event)).and_then(|usage|usage.payload.get("usage_stream_id")).and_then(Value::as_str) {owners.entry(stream.to_string()).or_default().insert(event_ref);}
            }
        }
    }
    owners.into_iter().filter_map(|(stream,owners)|(owners.len()>1).then_some(stream)).collect()
}

fn reject_cross_run_streams(candidates:&[Candidate],by_event:&HashMap<&str,&UsageRecord>,rejected:&mut HashSet<usize>) {
    let mut owners:HashMap<&str,HashSet<usize>>=HashMap::new();
    for (index,candidate) in candidates.iter().enumerate() {
        for (_,_,event_id) in &candidate.segments {
            if let Some(stream)=by_event.get(event_id.as_str()).and_then(|usage|usage.payload.get("usage_stream_id")).and_then(Value::as_str) {owners.entry(stream).or_default().insert(index);}
        }
    }
    for indexes in owners.values() {if indexes.len()>1 {rejected.extend(indexes);}}
}

fn range(values:[u128;3])->Value {json!({"low":values[0] as u64,"central":values[1] as u64,"high":values[2] as u64})}

fn signed(baseline:u128,actual:u128)->i64 {
    if baseline>=actual{(baseline-actual) as i64}else{-((actual-baseline) as i64)}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(event:&str,run:&str,root:&str,decision:&str,attempt:&str,usage:&str,coverage:&str,scenario:bool)->ManifestRecord {
        let mut payload=json!({
            "run_id":run,"host_root_ref":root,"mode":"laya","terminal_checkpoint":"run-terminal",
            "meter":{"identity":"meter"},
            "segments":[{"decision_id":decision,"attempt_ref":attempt,"usage_event_id":usage}],
            "coverage":{"status":coverage}
        });
        if scenario {payload["scenario"]=json!({
            "orchestrator_model":"gpt-test","reasoning_effort":"high","initial_context_tokens":100,
            "stages":[{"context_growth_tokens":50,"work_output_tokens":20,"passes":2},{"context_growth_tokens":30,"work_output_tokens":10,"passes":1}],
            "input_source":"declared-test"
        });}
        ManifestRecord {event_id:event.to_string(),payload}
    }

    fn usage(event:&str,decision:&str,attempt:&str,total:Value,sequence:Option<u64>)->UsageRecord {
        let mut payload=json!({"total_tokens":total,"source_verified":true,"scope":"attempt","checkpoint":event,"overlap_status":"non_overlapping"});
        if let Some(sequence)=sequence {
            payload["aggregation"]=json!("cumulative");
            payload["usage_stream_id"]=json!(format!("stream-{decision}-{attempt}"));
            payload["source_sequence"]=json!(sequence);
        }
        UsageRecord {event_id:event.to_string(),decision_id:decision.to_string(),attempt_ref:attempt.to_string(),payload}
    }

    #[test]
    fn lifecycle_snapshots_follow_native_sequence_not_delivery_order() {
        let mut old=manifest("m1","codex-lifecycle:r","h","d","a","u1","partial",true);
        old.payload["scenario"]["input_source"]=json!("native-envelope-v1; lifecycle-snapshot-v1; test");
        let mut new=old.clone();
        new.event_id="m2".into();
        new.payload["segments"][0]["usage_event_id"]=json!("u2");
        let reports=vec![usage("u1","d","a",json!(100),Some(1)),usage("u2","d","a",json!(495),Some(2))];
        let result=summarize(vec![new.clone(),old.clone()],reports.clone());
        assert_eq!(result["actual_total"],495);
        assert_eq!(result["saved"]["central"],0);
        assert_eq!(result["included_runs"],1);
        assert_eq!(result["excluded_runs"],0);
        let mut conflict=new.clone();
        conflict.event_id="conflict".into();
        conflict.payload["scenario"]["initial_context_tokens"]=json!(999);
        assert_eq!(summarize(vec![new,old,conflict],reports)["status"],"unavailable");
    }

    #[test]
    fn estimate_uses_exact_versioned_rationals() {
        assert_eq!(estimate(100,&[(20,50,2),(30,40,1)]),Some([460,495,548]));
    }

    #[test]
    fn estimate_checked_arithmetic_does_not_wrap() {
        assert!(estimate(u128::MAX,&[(1,1,1)]).is_none());
    }

    #[test]
    fn summary_preserves_positive_negative_and_partial_results() {
        let positive=summarize(vec![manifest("m","r","h","d","a","u","partial",true)],vec![usage("u","d","a",json!(200),None)]);
        assert_eq!(positive["status"],"partial");
        assert_eq!(positive["baseline"],json!({"low":472,"central":495,"high":536}));
        assert_eq!(positive["saved"],json!({"low":272,"central":295,"high":336}));
        assert_eq!(positive["runs"][0]["coverage"],"partial");
        assert_eq!(positive["runs"][0]["orchestrator_model"],"gpt-test");
        assert_eq!(positive["runs"][0]["reasoning_effort"],"high");
        let negative=summarize(vec![manifest("m","r","h","d","a","u","complete",true)],vec![usage("u","d","a",json!(600),None)]);
        assert_eq!(negative["saved"],json!({"low":-128,"central":-105,"high":-64}));
        assert_eq!(negative["runs"][0]["coverage"],"reported");
    }

    #[test]
    fn summary_excludes_missing_unknown_duplicate_and_stale_inputs() {
        let missing=summarize(vec![manifest("m","r","h","d","a","u","complete",false)],vec![usage("u","d","a",json!(1),None)]);
        assert_eq!(missing["status"],"unavailable");
        assert_eq!(missing["excluded_runs"],1);
        let unknown=summarize(vec![manifest("m","r","h","d","a","u","complete",true)],vec![usage("u","d","a",Value::Null,None)]);
        assert_eq!(unknown["included_runs"],0);
        assert_eq!(unknown["excluded_runs"],1);
        let duplicate=summarize(vec![manifest("m1","same","h1","d1","a","u1","complete",true),manifest("m2","same","h2","d2","a","u2","complete",true)],vec![usage("u1","d1","a",json!(1),None),usage("u2","d2","a",json!(1),None)]);
        assert_eq!(duplicate["excluded_runs"],2);
        let stale=summarize(vec![manifest("m","r","h","d","a","old","complete",true)],vec![usage("old","d","a",json!(10),Some(1)),usage("new","d","a",json!(20),Some(2))]);
        assert_eq!(stale["included_runs"],0);
        assert_eq!(stale["excluded_runs"],1);
    }

    #[test]
    fn summary_overflow_excludes_every_suppressed_run() {
        let mut manifests=Vec::new();
        let mut usages=Vec::new();
        for index in 0..2 {
            manifests.push(manifest(&format!("m{index}"),&format!("r{index}"),&format!("h{index}"),&format!("d{index}"),"a",&format!("u{index}"),"complete",true));
            usages.push(usage(&format!("u{index}"),&format!("d{index}"),"a",json!(u64::MAX),None));
        }
        let summary=summarize(manifests,usages);
        assert_eq!(summary["status"],"unavailable");
        assert_eq!(summary["included_runs"],0);
        assert_eq!(summary["excluded_runs"],2);
        assert!(summary["actual_total"].is_null());
    }

    #[test]
    fn summary_accepts_identical_terminal_envelopes_and_rejects_overlap_or_stream_reuse() {
        let first=usage("first","d","a",json!(20),Some(1));
        let mut replay=first.clone();
        replay.event_id="replay".to_string();
        let identical=summarize(vec![manifest("m","r","h","d","a","replay","complete",true)],vec![first,replay]);
        assert_eq!(identical["included_runs"],1);
        let mut overlapping=usage("u","d","a",json!(20),None);
        overlapping.payload["overlap_status"]=json!("overlapping");
        let overlap=summarize(vec![manifest("m","r","h","d","a","u","complete",true)],vec![overlapping]);
        assert_eq!(overlap["excluded_runs"],1);
        let mut first=usage("u1","d1","a",json!(20),Some(1));
        let mut second=usage("u2","d2","a",json!(20),Some(1));
        first.payload["usage_stream_id"]=json!("reused");
        second.payload["usage_stream_id"]=json!("reused");
        let reused=summarize(vec![manifest("m1","r1","h1","d1","a","u1","complete",true),manifest("m2","r2","h2","d2","a","u2","complete",true)],vec![first,second]);
        assert_eq!(reused["included_runs"],0);
        assert_eq!(reused["excluded_runs"],2);
    }
}
