use crate::{protocol::{hash, now}, service::{App,worker_cases}, store::{EVALUATOR_IDENTITY,RETRIEVAL_POLICY_VERSION}};
use anyhow::{bail, Context, Result};
use serde_json::{json,Value};
use std::{os::unix::fs::PermissionsExt,path::{Path,PathBuf}};
use tokio::io::AsyncWriteExt;

const BASELINE: &str=include_str!("../../../evals/advisor/baseline-v1.json");
const MAX_EXPORT_BYTES:usize=256*1024*1024;
const MAX_EXPORT_RECORDS:usize=100_000;

pub async fn start(app: App, input: Value) -> Result<Value> {
    let kind=input["kind"].as_str().context("invalid: job kind required")?;
    if !["evaluation","export"].contains(&kind) {bail!("invalid: unsupported job kind");}
    let job_guard=app.job_guard.clone().try_lock_owned().map_err(|_|anyhow::anyhow!("conflict: another job is running"))?;
    let id=uuid::Uuid::new_v4().to_string();
    let job=app.store.call("jobs/create",json!({"id":id,"kind":kind,"payload":input})).await?;
    let handle=app.clone();
    tokio::spawn(async move {
        let _job_guard=job_guard;
        let _maintenance=handle.maintenance.read().await;
        let _=handle.store.call("jobs/update",json!({"id":id,"status":"running"})).await;
        let outcome=if input["kind"]=="evaluation" {evaluate(&handle,&id,&input).await} else {export(&handle,&id).await};
        let change=match outcome {
            Ok(result)=>json!({"id":id,"status":"completed","result":result}),
            Err(error)=>json!({"id":id,"status":if error.to_string()=="cancelled" {"cancelled"}else{"failed"},"error":error.to_string()}),
        };
        let _=handle.store.call("jobs/update",change).await;
        handle.changed.notify_waiters();
    });
    Ok(job)
}

async fn checkpoint(app:&App,id:&str) -> Result<()> {
    if app.store.call("jobs/get",json!({"id":id})).await?["cancel_requested"]==true {bail!("cancelled");}
    while app.worker.busy() {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if app.store.call("jobs/get",json!({"id":id})).await?["cancel_requested"]==true {bail!("cancelled");}
    }
    Ok(())
}

async fn evaluate(app:&App,id:&str,input:&Value) -> Result<Value> {
    let version=input["version_id"].as_str().context("invalid: version_id required")?;
    let candidate=app.store.call("versions/get",json!({"id":version})).await?;
    let settings=app.store.call("settings/get",json!({})).await?;
    let current=settings["active_memory_version"].as_str().map(str::to_string);
    let baseline:Value=serde_json::from_str(BASELINE)?;
    let worker_info=app.worker.call("info",json!({})).await?;
    let mut rows=Vec::new();
    let cases=baseline["cases"].as_array().context("baseline cases")?;
    for (index,case) in cases.iter().enumerate() {
        checkpoint(app,id).await?;
        let mut runs=Vec::new();
        for version_id in [None,current.as_deref(),Some(version)] {
            checkpoint(app,id).await?;
            let configuration=json!({"task_family":case["family"],"task_lineage":case["lineage"],"language":case["language"]});
            let memory=if let Some(v)=version_id {
                app.store.call("memory/retrieve",json!({"query":case["state"],"version":v,"evaluation":true,"exclude_ids":[case["id"]],"configuration":configuration})).await?
            } else {json!({"version":null,"cases":[],"reason":"no_memory_version"})};
            let start=std::time::Instant::now();
            // Synthetic catalog is an evaluation fixture, not an available host model list.
            let result=app.worker.call("predict",json!({"state":case["state"],"advisor":{"models":[{"id":"evaluation-fixture","reasoning_efforts":["low","medium","high"]}],"current_model":"evaluation-fixture"},"memory_cases":worker_cases(&memory)})).await?;
            runs.push(json!({"result":result,"latency_ms":start.elapsed().as_millis(),"case_ids":memory["cases"].as_array().map(|a|a.iter().map(|v|v["id"].clone()).collect::<Vec<_>>()).unwrap_or_default(),"retrieval":{"configuration":configuration,"reason":memory["reason"],"version":memory["version"]}}));
        }
        rows.push(json!({"id":case["id"],"family":case["family"],"task_lineage":case["lineage"],"language":case["language"],"expected":case["labels"],"runs":runs}));
        app.store.call("jobs/update",json!({"id":id,"progress":{"completed":index+1,"total":cases.len()}})).await?;
        app.changed.notify_waiters();
    }
    let mut report=score(&rows,baseline["minimum_samples"].as_u64().unwrap_or(16) as usize);
    let final_identity=app.worker.call("info",json!({})).await?;
    let identity_verified=!final_identity["model"]["model_revision"].is_null() && final_identity["model"]["model_revision"]==worker_info["model"]["model_revision"];
    if !identity_verified {report["passed"]=json!(false);}
    report["identity_verified"]=json!(identity_verified);
    report["rows"]=json!(rows);
    report["baseline_version"]=baseline["version"].clone();
    report["baseline_hash"]=json!(hash(&baseline));
    report["candidate_hash"]=json!(hash(&candidate));
    report["worker_identity"]=final_identity;
    report["evaluated_at"]=json!(now());
    report["current_version"]=json!(current);
    report["scope"]=json!("fixed local regression set; not proof of general capability improvement");
    report["evaluator_identity"]=json!(EVALUATOR_IDENTITY);
    report["retrieval_policy_version"]=json!(RETRIEVAL_POLICY_VERSION);
    app.store.call("versions/report",json!({"id":version,"report":report})).await?;
    Ok(report)
}

fn choice<'a>(run:&'a Value,dimension:&str)->Option<&'a str> {
    run["result"]["laya_result"]["answers"][dimension]["choice"].as_str()
}

fn valid_suggestion(run:&Value)->bool {
    let recommendation=&run["result"]["advice"]["recommendation"];
    if recommendation.is_null() {return true;}
    recommendation["model"]=="evaluation-fixture"
        && matches!(recommendation["reasoning_effort"].as_str(),Some("low"|"medium"|"high"))
}

fn verified_case_ids(run:&Value)->Option<&Vec<Value>> {
    let selected=run["case_ids"].as_array()?;
    let received=run["result"]["meta"]["case_ids"].as_array()?;
    if selected==received && received.iter().all(Value::is_string) {Some(received)} else {None}
}

pub fn score(rows:&[Value],minimum:usize)->Value {
    let mut correct=[0usize;3];
    let mut denominator=0;
    let mut regressions=0;
    let mut invalid=0;
    let mut uncertain=[0usize;3];
    let mut uncertain_total=0;
    let mut run_shape_errors=0;
    let mut candidate_memory_exposure=0usize;
    let mut dimensions=serde_json::Map::new();
    for dimension in ["complexity","risk","certainty"] {dimensions.insert(dimension.into(),json!({"denominator":0,"correct":[0,0,0]}));}
    for row in rows {
        let runs=row["runs"].as_array();
        if runs.map(Vec::len)!=Some(3) {run_shape_errors+=1;}
        candidate_memory_exposure+=runs.and_then(|runs|runs.get(2)).and_then(verified_case_ids).map(Vec::len).unwrap_or(0);
        for (dimension,expected) in row["expected"].as_object().into_iter().flatten() {
            denominator+=1;
            if let Some(stats)=dimensions.get_mut(dimension) {stats["denominator"]=json!(stats["denominator"].as_u64().unwrap_or(0)+1);}
            for (i,run) in runs.into_iter().flatten().enumerate().take(3) {
                if choice(run,dimension)==expected.as_str() {
                    correct[i]+=1;
                    if let Some(stats)=dimensions.get_mut(dimension) {stats["correct"][i]=json!(stats["correct"][i].as_u64().unwrap_or(0)+1);}
                }
            }
        }
        if row["expected"]["certainty"]=="uncertain" {
            uncertain_total+=1;
            for (i,run) in runs.into_iter().flatten().enumerate().take(3) {
                if run["result"]["advice"]["ask_user"]==true {uncertain[i]+=1;}
            }
        }
        for run in runs.into_iter().flatten() {
            if verified_case_ids(run).is_none() {invalid+=1;}
            for key in ["complexity","risk","certainty"] {
                let permitted=if key=="certainty" {vec!["clear","uncertain"]}else{vec!["low","medium","high"]};
                if !choice(run,key).map(|c|permitted.contains(&c)).unwrap_or(false) {invalid+=1;}
            }
            if !valid_suggestion(run) {invalid+=1;}
            // Online advisor cannot claim to have switched or executed anything.
            if run["result"]["advice"]["delegation"]["parent_model_switched"]==true {invalid+=1;}
        }
        if runs.map(Vec::len)==Some(3) && row["expected"]["risk"]=="high" && choice(&row["runs"][2],"risk")==Some("low") && (choice(&row["runs"][0],"risk")!=Some("low") || choice(&row["runs"][1],"risk")!=Some("low")) {regressions+=1;}
    }
    let uncertain_regressed=uncertain[2]<uncertain[0] || uncertain[2]<uncertain[1];
    json!({"passed":rows.len()>=minimum && denominator>0 && candidate_memory_exposure>0 && invalid==0 && run_shape_errors==0 && regressions==0 && !uncertain_regressed && correct[2]>=correct[0] && correct[2]>=correct[1],"sample_count":rows.len(),"label_denominator":denominator,"candidate_memory_exposure":candidate_memory_exposure,"dimensions":dimensions,"run_order":["baseline","current","candidate"],"correct_labels":{"baseline":correct[0],"current":correct[1],"candidate":correct[2]},"new_high_to_low":regressions,"invalid_outputs":invalid,"run_shape_errors":run_shape_errors,"uncertain_total":uncertain_total,"uncertain_asks":uncertain,"uncertain_ask_regressed":uncertain_regressed})
}

pub fn artifact_path(root:&Path,artifact_id:&str)->Result<PathBuf> {
    let id=uuid::Uuid::parse_str(artifact_id).context("invalid: artifact id")?.to_string();
    Ok(root.join("exports").join(format!("laya-export-{id}.ndjson")))
}

async fn cancelled(app:&App,id:&str)->Result<()> {
    if app.store.call("jobs/get",json!({"id":id})).await?["cancel_requested"]==true {bail!("cancelled");}
    Ok(())
}

async fn write_line(file:&mut tokio::fs::File,value:&Value,written:&mut usize)->Result<()> {
    let mut line=serde_json::to_vec(value)?;
    line.push(b'\n');
    if written.checked_add(line.len()).filter(|size|*size<=MAX_EXPORT_BYTES).is_none() {bail!("export exceeds 256 MiB limit");}
    file.write_all(&line).await?;
    *written+=line.len();
    Ok(())
}

async fn export(app:&App,id:&str)->Result<Value> {
    let _guard=app.privacy.lock().await;
    let exports=app.root.join("exports");
    if exports.is_symlink() {bail!("invalid: symlinked exports directory");}
    std::fs::create_dir_all(&exports)?;
    std::fs::set_permissions(&exports,std::fs::Permissions::from_mode(0o700))?;
    let artifact=artifact_path(&app.root,id)?;
    let partial=exports.join(format!(".laya-export-{id}.partial"));
    if partial.exists() {std::fs::remove_file(&partial)?;}
    if artifact.exists() {bail!("conflict: export artifact already exists");}
    let mut published=false;
    let outcome=async {
        cancelled(app,id).await?;
        let mut file=tokio::fs::OpenOptions::new().write(true).create_new(true).open(&partial).await?;
        std::fs::set_permissions(&partial,std::fs::Permissions::from_mode(0o600))?;
        let mut written=0usize;
        write_line(&mut file,&json!({"type":"manifest","schema_version":1,"exported_at":now()}),&mut written).await?;
        let mut offset=0usize;
        let mut count=0usize;
        loop {
            cancelled(app,id).await?;
            let page=app.store.call("decisions/list",json!({"limit":100,"offset":offset})).await?;
            let rows=page["items"].as_array().context("invalid decision page")?;
            if rows.is_empty() {break;}
            for row in rows {
                cancelled(app,id).await?;
                if count>=MAX_EXPORT_RECORDS {bail!("export exceeds 100000 record limit");}
                let decision=app.store.call("decisions/get",json!({"id":row["id"]})).await?;
                write_line(&mut file,&json!({"type":"decision","decision":decision}),&mut written).await?;
                count+=1;
            }
            offset+=rows.len();
            app.store.call("jobs/update",json!({"id":id,"progress":{"completed":count}})).await?;
            app.changed.notify_waiters();
        }
        file.flush().await?;
        file.sync_all().await?;
        cancelled(app,id).await?;
        tokio::fs::rename(&partial,&artifact).await?;
        published=true;
        cancelled(app,id).await?;
        Ok(json!({"schema_version":1,"artifact_id":id,"artifact_name":artifact.file_name().and_then(|v|v.to_str()).context("artifact name")?,"media_type":"application/x-ndjson","record_count":count}))
    }.await;
    if outcome.is_err() {
        let _=tokio::fs::remove_file(&partial).await;
        if published {let _=tokio::fs::remove_file(&artifact).await;}
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(ask:bool,model:&str,effort:&str)->Value {json!({"result":{"meta":{"case_ids":["eligible"]},"laya_result":{"answers":{"complexity":{"choice":"low"},"risk":{"choice":"low"},"certainty":{"choice":"uncertain"}}},"advice":{"ask_user":ask,"recommendation":{"model":model,"reasoning_effort":effort},"delegation":{"parent_model_switched":false}}},"case_ids":["eligible"]})}
    fn row(runs:Vec<Value>)->Value {json!({"expected":{"complexity":"low","risk":"low","certainty":"uncertain"},"runs":runs})}
    #[test]
    fn empty_evaluation_never_passes() {assert_eq!(score(&[],16)["passed"],false);}
    #[test]
    fn fixture_has_multilingual_families() {
        let set:Value=serde_json::from_str(BASELINE).unwrap();
        assert_eq!(set["cases"].as_array().unwrap().len(),16);
    }
    #[test]
    fn exactly_three_runs_are_required() {
        assert_eq!(score(&[row(vec![run(true,"evaluation-fixture","low");2])],1)["run_shape_errors"],1);
    }
    #[test]
    fn candidate_uncertain_ask_rate_cannot_regress() {
        let report=score(&[row(vec![run(true,"evaluation-fixture","low"),run(true,"evaluation-fixture","medium"),run(false,"evaluation-fixture","high")])],1);
        assert_eq!(report["uncertain_ask_regressed"],true); assert_eq!(report["passed"],false);
    }
    #[test]
    fn safe_three_run_fixture_can_pass() {
        let report=score(&[row(vec![run(true,"evaluation-fixture","low"),run(true,"evaluation-fixture","medium"),run(true,"evaluation-fixture","high")])],1);
        assert_eq!(report["passed"],true);
    }
    #[test]
    fn no_candidate_memory_exposure_never_passes() {
        let mut runs=vec![run(true,"evaluation-fixture","low"),run(true,"evaluation-fixture","medium"),run(true,"evaluation-fixture","high")];
        runs[2]["case_ids"]=json!([]);
        runs[2]["result"]["meta"]["case_ids"]=json!([]);
        let report=score(&[row(runs)],1);
        assert_eq!(report["candidate_memory_exposure"],0); assert_eq!(report["passed"],false);
    }
    #[test]
    fn missing_or_mismatched_worker_receipt_never_counts_as_exposure() {
        for receipt in [Value::Null,json!([]),json!(["another-case"])] {
            let mut runs=vec![run(true,"evaluation-fixture","low");3];
            runs[2]["result"]["meta"]["case_ids"]=receipt;
            let report=score(&[row(runs)],1);
            assert_eq!(report["candidate_memory_exposure"],0);
            assert_eq!(report["invalid_outputs"],1); assert_eq!(report["passed"],false);
        }
    }
    #[test]
    fn suggestions_are_limited_to_fixture_model_and_efforts() {
        for bad in [run(true,"host-model","low"),run(true,"evaluation-fixture","ultra")] {
            let report=score(&[row(vec![run(true,"evaluation-fixture","low"),run(true,"evaluation-fixture","medium"),bad])],1);
            assert_eq!(report["invalid_outputs"],1); assert_eq!(report["passed"],false);
        }
    }
    #[test]
    fn artifact_path_is_uuid_scoped() {
        let id="550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(artifact_path(Path::new("/private/workbench"),id).unwrap(),Path::new("/private/workbench/exports/laya-export-550e8400-e29b-41d4-a716-446655440000.ndjson"));
        assert!(artifact_path(Path::new("/private/workbench"),"../secret").is_err());
    }
}
