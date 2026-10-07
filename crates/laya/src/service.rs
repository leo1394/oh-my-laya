use crate::{outbox::Outbox, protocol::{now, redact, Request}, runtime::{self, ServiceInfo}, store::{canonical_task_family,validate_task_lineage,Store}, worker::Worker};
use anyhow::{bail, Context, Result};
use axum::{extract::{Path as UrlPath, Query, State}, http::{HeaderMap, StatusCode}, response::{IntoResponse, Response}, routing::{get, post}, Json, Router};
use fs2::FileExt;
use serde_json::{json, Value};
use std::{collections::HashMap, os::unix::fs::PermissionsExt, path::PathBuf, sync::{Arc,atomic::{AtomicBool,AtomicU64,Ordering}}, time::Duration};
use tokio::{io::{AsyncReadExt, AsyncWriteExt, BufReader}, net::{TcpListener, UnixListener}, sync::{Mutex, Notify, RwLock}};
use crate::assets::serve as assets;

pub fn worker_cases(memory: &Value) -> Value {
    json!(memory["cases"].as_array().into_iter().flatten().map(|case|json!({"id":case["id"],"summary":case["summary"].as_str().map(str::to_string).unwrap_or_else(||case["summary"].to_string()),"labels":case["labels"]})).collect::<Vec<_>>())
}

fn take_advisor_routing_scope(params:&mut Value)->Result<(Option<String>,Option<String>)> {
    let Some(advisor)=params.get_mut("advisor").and_then(Value::as_object_mut) else {return Ok((None,None));};
    let task_family=match advisor.remove("task_family") {
        None|Some(Value::Null)=>None,
        Some(Value::String(value))=>Some(canonical_task_family(&value)?),
        Some(_)=>bail!("invalid: advisor.task_family must be a string"),
    };
    let task_lineage=match advisor.remove("task_lineage") {
        None|Some(Value::Null)=>None,
        Some(Value::String(value))=>Some(validate_task_lineage(&value)?),
        Some(_)=>bail!("invalid: advisor.task_lineage must be a string"),
    };
    Ok((task_family,task_lineage))
}

async fn events(State(app): State<App>, Query(query): Query<HashMap<String,String>>, headers: HeaderMap) -> Response {
    if let Err(error)=app.authenticated(&headers,false).await {return failure(error);}
    let cursor=headers.get("last-event-id").and_then(|h|h.to_str().ok()).or_else(||query.get("last_event_id").map(String::as_str)).and_then(|s|s.parse::<i64>().ok()).unwrap_or(0).max(0);
    let stream=futures::stream::unfold((app,cursor,Vec::<Value>::new()),|(app,mut cursor,mut pending)|async move {
        loop {
            if app.stopping.load(Ordering::Acquire) {return None;}
            if !pending.is_empty() {
                let value=pending.remove(0);
                cursor=value["sequence"].as_i64().unwrap_or(cursor);
                let event=axum::response::sse::Event::default().id(cursor.to_string()).event("change").data(value.to_string());
                return Some((Ok::<_,std::convert::Infallible>(event),(app,cursor,pending)));
            }
            let notified=app.changed.notified();
            if let Ok(value)=app.store.call("events",json!({"after":cursor})).await { pending=value["items"].as_array().cloned().unwrap_or_default(); }
            if pending.is_empty() { let _=tokio::time::timeout(Duration::from_secs(30),notified).await; }
        }
    });
    axum::response::Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default()).into_response()
}

#[derive(Clone)]
pub struct App {
    pub store: Store,
    pub worker: Worker,
    pub root: PathBuf,
    pub info: ServiceInfo,
    pub privacy: Arc<Mutex<()>>,
    pub maintenance: Arc<RwLock<()>>,
    pub job_guard: Arc<Mutex<()>>,
    pub changed: Arc<Notify>,
    pub shutdown: Arc<Notify>,
    pub stopping: Arc<AtomicBool>,
    assessment_epoch: Arc<AtomicU64>,
    sessions: Arc<Mutex<HashMap<String,i64>>>,
    pairing: Arc<Mutex<HashMap<String,i64>>>,
}

impl App {
    async fn invalidate_assessments(&self, purge:bool) {
        self.assessment_epoch.fetch_add(1,Ordering::SeqCst);
        if purge && self.worker.status()["pid"].as_u64().unwrap_or(0)>0 {
            match self.worker.call("info",json!({})).await {
                Ok(info) if info["supported_contracts"].as_array().is_some_and(|items|items.iter().any(|item|item=="assessment_reuse_v1"))=> {
                    if self.worker.call("clear_assessment_cache",json!({})).await.is_err() {let _=self.worker.call("release",json!({})).await;}
                },
                Err(_)=>{let _=self.worker.call("release",json!({})).await;},
                _=>{},
            }
        }
    }

    pub async fn status(&self) -> Result<Value> {
        let mut value = self.store.call("status",json!({})).await.unwrap_or_else(|error|json!({"ok":false,"degraded":true,"persistence_error":error.to_string()}));
        value["service"] = serde_json::to_value(&self.info)?;
        value["supported_contracts"] = json!(["orchestration_plan_v1","dispatch_receipt_v1","attempt_outcome_v1"]);
        value["worker"] = self.worker.status();
        let worker_pid=value["worker"]["pid"].as_u64().unwrap_or(0);
        let pids=if worker_pid>0 {format!("{},{}",self.info.pid,worker_pid)} else {self.info.pid.to_string()};
        value["resources"]=match tokio::time::timeout(Duration::from_secs(1),tokio::process::Command::new("/bin/ps").args(["-p",&pids,"-o","pid=","-o","rss=","-o","%cpu="]).output()).await {
            Ok(Ok(output)) if output.status.success()=> {
                let processes=String::from_utf8_lossy(&output.stdout).lines().filter_map(|line| {
                    let fields=line.split_whitespace().collect::<Vec<_>>();
                    if fields.len()!=3 {return None;}
                    Some(json!({"pid":fields[0].parse::<u32>().ok()?,"rss_kib":fields[1].parse::<u64>().ok()?,"cpu_percent":fields[2].parse::<f64>().ok()?}))
                }).collect::<Vec<_>>();
                json!({"source":"ps","scope":"RSS per process, not additive unique physical or GPU memory; excludes browsers and MCP bridges","processes":processes,"observed_at":now()})
            },
            _=>json!({"available":false,"source":"ps"}),
        };
        let root = self.root.clone();
        value["outbox"] = match tokio::task::spawn_blocking(move || Outbox::open(&root)?.status()).await {
            Ok(Ok(status))=>status,
            other=>json!({"available":false,"error":format!("{other:?}")}),
        };
        Ok(value)
    }

    pub async fn feedback(&self, params: Value) -> Result<Value> {
        let _maintenance = self.maintenance.read().await;
        let _guard = self.privacy.lock().await;
        let receipt = self.store.call("feedback", redact(&params).0).await?;
        if receipt["status"]=="stored" && receipt["idempotent"]!=true && matches!(params["kind"].as_str(),Some("test"|"review"|"outcome"|"assignment")) {
            self.invalidate_assessments(false).await;
        }
        self.changed.notify_waiters();
        Ok(receipt)
    }

    async fn persist_snapshot(&self,method:&str,mut payload:Value)->Result<Value> {
        let _privacy=self.privacy.lock().await;
        let expected_epoch=payload.as_object_mut().and_then(|object|object.remove("assessment_epoch"));
        if method=="decisions/finish" && payload["result"]["meta"]["assessment_cache"]["status"]=="hit"
            && expected_epoch.and_then(|value|value.as_u64())!=Some(self.assessment_epoch.load(Ordering::SeqCst)) {
            bail!("stale_assessment: evidence or policy changed during reuse; retry with updated context");
        }
        let enabled=self.store.call("settings/get",json!({})).await.map(|v|v["recording_enabled"]==true).unwrap_or_else(|_| {
            std::fs::read_to_string(self.root.join("recording-consent.json")).ok().and_then(|s|serde_json::from_str::<Value>(&s).ok()).map(|v|v["recording_enabled"]==true).unwrap_or(false)
        });
        if !enabled {return self.store.call(method,payload).await;}
        payload["capture_redacted"]=json!(redact(&payload).1);
        let root=self.root.clone(); let owned=method.to_string();
        let event=tokio::task::spawn_blocking(move||Outbox::open(&root)?.snapshot_enqueue(&owned,&payload)).await??;
        match self.store.call(method,event["payload"].clone()).await {
            Ok(receipt) if receipt["recording_status"]=="stored"=> {
                let root=self.root.clone();
                tokio::task::spawn_blocking(move||Outbox::open(&root)?.snapshot_ack(&event)).await??;
                Ok(receipt)
            }
            Ok(receipt)=>Ok(receipt),
            Err(error) if ["conflict:","deleted:","invalid:"].iter().any(|prefix|error.to_string().starts_with(prefix))=>Err(error),
            Err(error)=>Ok(json!({"id":event["payload"]["id"],"recording_status":"queued_local","recording_error":error.to_string(),"existing":false})),
        }
    }

    async fn predict(&self, request: &Request) -> Result<Value> {
        let _maintenance = self.maintenance.read().await;
        if self.stopping.load(Ordering::Acquire) {bail!("unavailable: service is stopping");}
        if request.params.get("_cache_context").is_some()||request.params.get("memory_budget").is_some() {bail!("invalid: private assessment metadata is service-owned");}
        let assessment_epoch=self.assessment_epoch.load(Ordering::SeqCst);
        let advisor = request.params.get("advisor").filter(|v| !v.is_null()).is_some();
        let mut params = request.params.clone();
        let (task_family,task_lineage)=take_advisor_routing_scope(&mut params)?;
        let id = request.request_id.clone();
        let begin = match self.persist_snapshot("decisions/begin", json!({"id":id,"request_id":request.request_id,"request":request.params})).await {
            Ok(value)=>value,
            Err(error) if ["conflict:","deleted:","invalid:"].iter().any(|prefix|error.to_string().starts_with(prefix))=>return Err(error),
            Err(error)=>json!({"recording_status":"not_saved","recording_error":error.to_string(),"existing":false}),
        };
        if begin["existing"] == true {
            if let Some(result) = begin.get("result").filter(|v| !v.is_null()) { return Ok(result.clone()); }
            bail!("interrupted: existing request has no confirmed result; use a new attempt id");
        }
        let mut info = match self.worker.call("info",json!({})).await {
            Ok(info)=>info,
            Err(error)=> {
                let _=self.persist_snapshot("decisions/finish",json!({"id":id,"error":{"message":error.to_string(),"kind":"worker_unavailable"}})).await;
                return Err(error);
            }
        };
        let orchestration_unavailable=crate::protocol::negotiate_orchestration(&mut params,&info)?;
        let settings = self.store.call("settings/get",json!({})).await.unwrap_or(json!({"memory_enabled":false}));
        let budget_requested=params["orchestration"]["enabled"]==true;
        let budget_supported=info["supported_contracts"].as_array().is_some_and(|items|items.iter().any(|item|item=="memory_budget_v1"));
        let query = request.params["state"].as_str().map(str::to_string).unwrap_or_else(|| request.params["state"].to_string());
        let memory = if !advisor {json!({"cases":[],"reason":"not_advisor"})}
        else if settings["memory_enabled"] == true && task_family.is_some() {
            let language=if query.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {"zh"} else {"en"};
            // No verified execution configuration is known before model selection.
            // Preserve exclusion reasons in the snapshot, never infer identity from Laya's checkpoint.
            self.store.call("memory/retrieve",json!({"query":query,"memory_budget":budget_requested&&budget_supported,"configuration":{"task_family":task_family,"task_lineage":task_lineage,"language":language}})).await.unwrap_or(json!({"cases":[],"reason":"retrieval_failed"}))
        } else if settings["memory_enabled"] == true {json!({"cases":[],"reason":"missing_task_family_scope"})}
        else { json!({"cases":[],"reason":"disabled"}) };
        if advisor {
            params["memory_cases"] = worker_cases(&memory);
            params["model_tiers"] = settings.get("model_tiers").cloned().unwrap_or(json!({}));
        }
        if budget_requested&&budget_supported {params["memory_budget"]=json!(true);}
        if params["orchestration"]["enabled"]==true && info["supported_contracts"].as_array().is_some_and(|items|items.iter().any(|item|item=="assessment_reuse_v1")) {
            params["_cache_context"]=json!({"decision_id":id,"service_instance":self.info.instance,"epoch":assessment_epoch,"memory_version":memory["version"],"settings":settings});
        }
        let outcome = self.worker.call("predict",params).await;
        match outcome {
            Ok(mut result) => {
                if let Some(plan)=result.get_mut("orchestration_plan").and_then(Value::as_object_mut) {
                    plan.insert("decision_id".into(),json!(id));
                }
                info=self.worker.call("info",json!({})).await.unwrap_or_else(|_|json!({"model":{"model_revision":null},"identity_error":"post-inference identity unavailable","rules_version":info["rules_version"]}));
                let mut meta = result.get("meta").cloned().unwrap_or(json!({}));
                if orchestration_unavailable {meta["orchestration_status"]=json!("unsupported_worker");}
                if budget_requested&&!budget_supported {meta["memory_budget_status"]=json!("unsupported_worker");}
                // Retain worker-reported exposure separately from retrieval selection.
                // A missing worker report stays unknown, never inferred from retrieval.
                meta["worker_case_ids"] = meta.get("case_ids").cloned().unwrap_or(Value::Null);
                meta["decision_id"] = json!(id);
                meta["protocol_version"] = json!(1);
                meta["recording_status"] = begin.get("recording_status").cloned().unwrap_or(json!("not_recorded"));
                meta["model_revision"] = info["model"]["model_revision"].clone();
                meta["checkpoint_identity"] = info["model"].clone();
                meta["rules_version"] = info["rules_version"].clone();
                meta["memory_version"] = memory["version"].clone();
                meta["memory_reason"] = memory["reason"].clone();
                meta["case_ids"] = json!(memory["cases"].as_array().into_iter().flatten().map(|case|case["id"].clone()).collect::<Vec<_>>());
                result["meta"] = meta;
                match self.persist_snapshot("decisions/finish",json!({"id":id,"result":result,"assessment_epoch":assessment_epoch,"context":{"worker":info,"memory":memory}})).await {
                    Ok(receipt)=>{result["meta"]["recording_status"]=receipt["recording_status"].clone(); if let Some(error)=receipt.get("recording_error") {result["meta"]["recording_error"]=error.clone();}},
                    Err(error) if error.to_string().starts_with("stale_assessment:")=> {
                        let _=self.persist_snapshot("decisions/finish",json!({"id":id,"error":{"message":error.to_string(),"kind":"stale_assessment"}})).await;
                        self.changed.notify_waiters();
                        return Err(error);
                    },
                    Err(error)=>{result["meta"]["recording_status"]=json!("not_saved");result["meta"]["recording_error"]=json!(error.to_string());},
                }
                self.changed.notify_waiters();
                Ok(result)
            }
            Err(error) => {
                let _ = self.persist_snapshot("decisions/finish",json!({"id":id,"error":{"message":error.to_string()},"context":{"worker":info,"memory":memory}})).await;
                self.changed.notify_waiters();
                Err(error)
            }
        }
    }

    async fn internal(&self, request: &Request) -> Result<Value> {
        request.validate()?;
        match request.method.as_str() {
            "status" => self.status().await,
            "predict" => self.predict(request).await,
            "preferences" => {
                let mutating=["policy","ceiling","squad"].iter().any(|key|request.params.get(*key).is_some_and(|v|!v.is_null()));
                if mutating {
                    let _privacy=self.privacy.lock().await;
                    self.invalidate_assessments(false).await;
                }
                // Never hold the feedback lock while queued behind model inference.
                let result=self.worker.call("preferences",request.params.clone()).await;
                if mutating {
                    let _privacy=self.privacy.lock().await;
                    self.invalidate_assessments(false).await;
                }
                result
            },
            "feedback" => self.feedback(request.params.clone()).await,
            "pair" => {
                let code = uuid::Uuid::new_v4().to_string();
                self.pairing.lock().await.insert(code.clone(),now()+60);
                Ok(json!({"url":format!("http://127.0.0.1:{}/#pair={}",self.info.port,code)}))
            }
            "stop" => {
                let _maintenance=self.maintenance.try_write().map_err(|_|anyhow::anyhow!("busy: active requests prevent stop"))?;
                let _jobs = self.job_guard.try_lock().map_err(|_|anyhow::anyhow!("busy: a background job is running"))?;
                if self.worker.busy() { bail!("busy: tasks are running; retry after they finish"); }
                self.worker.call("release",json!({})).await?;
                self.stopping.store(true,Ordering::Release);
                self.changed.notify_waiters();
                self.shutdown.notify_one();
                Ok(json!({"stopped":true}))
            }
            _ => bail!("invalid: method not exposed on agent transport"),
        }
    }

    async fn authenticated(&self, headers: &HeaderMap, write: bool) -> Result<()> {
        let host = format!("127.0.0.1:{}",self.info.port);
        if headers.get("host").and_then(|v| v.to_str().ok()) != Some(host.as_str()) { bail!("forbidden: invalid host"); }
        if write || headers.contains_key("origin") {
            let origin = format!("http://{host}");
            if headers.get("origin").and_then(|v| v.to_str().ok()) != Some(origin.as_str()) { bail!("forbidden: invalid origin"); }
        }
        let cookie = headers.get("cookie").and_then(|v| v.to_str().ok()).unwrap_or("");
        let session = cookie.split(';').find_map(|item| item.trim().strip_prefix("laya_session="));
        let sessions = self.sessions.lock().await;
        if session.and_then(|s| sessions.get(s)).copied().unwrap_or(0) < now() { bail!("unauthorized: open laya dashboard to pair this browser"); }
        Ok(())
    }
}

fn failure(error: anyhow::Error) -> Response {
    let message = error.to_string();
    let status = if message.starts_with("conflict:") { StatusCode::CONFLICT }
        else if message.starts_with("unauthorized:") { StatusCode::UNAUTHORIZED }
        else if message.starts_with("forbidden:") { StatusCode::FORBIDDEN }
        else if message.starts_with("not_found:") { StatusCode::NOT_FOUND }
        else { StatusCode::BAD_REQUEST };
    (status, Json(json!({"error":message}))).into_response()
}

async fn pair(State(app): State<App>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let host = format!("127.0.0.1:{}",app.info.port);
    let origin = format!("http://{host}");
    if headers.get("host").and_then(|v|v.to_str().ok()) != Some(host.as_str()) || headers.get("origin").and_then(|v|v.to_str().ok()) != Some(origin.as_str()) { return failure(anyhow::anyhow!("forbidden: invalid pairing origin")); }
    let code = body["code"].as_str().unwrap_or("");
    if app.pairing.lock().await.remove(code).unwrap_or(0) < now() { return failure(anyhow::anyhow!("unauthorized: pairing code expired or already used")); }
    let token = uuid::Uuid::new_v4().to_string();
    app.sessions.lock().await.insert(token.clone(),now()+8*3600);
    let mut response = Json(json!({"paired":true})).into_response();
    response.headers_mut().insert("set-cookie",format!("laya_session={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=28800").parse().unwrap());
    response
}

async fn api_get(State(app): State<App>, UrlPath(path): UrlPath<String>, Query(query): Query<HashMap<String,String>>, headers: HeaderMap) -> Response {
    if let Err(error) = app.authenticated(&headers,false).await { return failure(error); }
    let _maintenance=app.maintenance.read().await;
    let pieces: Vec<_> = path.split('/').collect();
    if let ["exports",id]=pieces.as_slice() {
        return match download_export(&app,id).await {Ok(response)=>response,Err(error)=>failure(error)};
    }
    let params = if pieces.as_slice()==["decisions"] {
        match decisions_list_params(&query) {Ok(params)=>params,Err(error)=>return failure(error)}
    } else if pieces.as_slice()==["overview"] {
        match time_filter_params(&query,false) {Ok(params)=>params,Err(error)=>return failure(error)}
    } else if pieces.as_slice()==["cases"] {
        match time_filter_params(&query,true) {Ok(params)=>params,Err(error)=>return failure(error)}
    } else if let Some(id)=pieces.get(1) {json!({"id":id})} else {json!({"limit":query.get("limit").and_then(|s|s.parse::<u64>().ok()).unwrap_or(50),"offset":query.get("offset").and_then(|s|s.parse::<u64>().ok()).unwrap_or(0)})};
    let result = match pieces.as_slice() {
        ["status"] => app.status().await,
        ["overview"] => app.store.call("overview/get",params).await,
        ["settings"] => app.store.call("settings/get",json!({})).await,
        ["advisor-preferences"] => app.worker.call("preferences",json!({})).await,
        ["decisions"] => app.store.call("decisions/list",params).await,
        ["decisions",_] => app.store.call("decisions/get",params).await,
        ["cases"] => app.store.call("cases/list",params).await,
        ["memory-versions"] => app.store.call("versions/list",params).await,
        ["memory-versions",_] => app.store.call("versions/get",params).await,
        ["jobs"] => app.store.call("jobs/list",params).await,
        ["jobs",_] => app.store.call("jobs/get",params).await,
        ["backups"] => app.store.call("backup/list",params).await,
        ["outbox"] => {
            let root=app.root.clone();
            match tokio::task::spawn_blocking(move||Outbox::open(&root)?.list()).await {Ok(result)=>result,Err(error)=>Err(error.into())}
        }
        _ => Err(anyhow::anyhow!("not_found: endpoint")),
    };
    match result { Ok(value)=>Json(value).into_response(), Err(error)=>failure(error) }
}

fn decisions_list_params(query:&HashMap<String,String>)->Result<Value> {
    if let Some(key)=query.keys().find(|key|!["limit","offset","filter","risk","source","created_after","created_before"].contains(&key.as_str())){bail!("invalid: unknown decision query filter {key}");}
    let limit=query.get("limit").map(|value|value.parse::<i64>().map_err(|_|anyhow::anyhow!("invalid: limit must be an integer"))).transpose()?.unwrap_or(50);
    let offset=query.get("offset").map(|value|value.parse::<i64>().map_err(|_|anyhow::anyhow!("invalid: offset must be an integer"))).transpose()?.unwrap_or(0);
    let mut filter=serde_json::Map::new();
    if let Some(value)=query.get("filter"){filter.insert("status".into(),json!(value));}
    if let Some(value)=query.get("risk"){filter.insert("risk".into(),json!(value));}
    if let Some(value)=query.get("source"){filter.insert("source".into(),json!(value));}
    for key in ["created_after","created_before"] {
        if let Some(value)=query.get(key){filter.insert(key.into(),json!(value.parse::<i64>().map_err(|_|anyhow::anyhow!("invalid: {key} must be UTC epoch seconds"))?));}
    }
    Ok(json!({"limit":limit,"offset":offset,"filter":filter}))
}

fn time_filter_params(query:&HashMap<String,String>,pagination:bool)->Result<Value> {
    if let Some(key)=query.keys().find(|key|!["created_after","created_before"].contains(&key.as_str())&&(!pagination||!["limit","offset"].contains(&key.as_str()))){bail!("invalid: unknown query filter {key}");}
    let mut params=serde_json::Map::new();
    for key in ["created_after","created_before"] {
        if let Some(value)=query.get(key){params.insert(key.into(),json!(value.parse::<i64>().map_err(|_|anyhow::anyhow!("invalid: {key} must be UTC epoch seconds"))?));}
    }
    if pagination {
        let limit=query.get("limit").map(|value|value.parse::<i64>().map_err(|_|anyhow::anyhow!("invalid: limit must be an integer"))).transpose()?.unwrap_or(50);
        let offset=query.get("offset").map(|value|value.parse::<i64>().map_err(|_|anyhow::anyhow!("invalid: offset must be an integer"))).transpose()?.unwrap_or(0);
        params.insert("limit".into(),json!(limit));params.insert("offset".into(),json!(offset));
    }
    Ok(Value::Object(params))
}

async fn download_export(app:&App,id:&str)->Result<Response> {
    let guard=app.privacy.clone().lock_owned().await;
    let path=crate::evaluation::artifact_path(&app.root,id)?;
    let metadata=tokio::fs::symlink_metadata(&path).await.map_err(|_|anyhow::anyhow!("not_found: export unavailable or removed"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {bail!("invalid: export must be a regular file");}
    let file=tokio::fs::File::open(&path).await?;
    let stream=futures::stream::try_unfold((file,guard),|(mut file,guard)|async move {
        let mut buffer=vec![0u8;64*1024];
        let count=file.read(&mut buffer).await?;
        if count==0 {return Ok::<_,std::io::Error>(None);}
        buffer.truncate(count);
        Ok(Some((buffer,(file,guard))))
    });
    let mut response=Response::new(axum::body::Body::from_stream(stream));
    response.headers_mut().insert("content-type","application/x-ndjson".parse()?);
    response.headers_mut().insert("content-disposition",format!("attachment; filename=\"laya-export-{id}.ndjson\"").parse()?);
    response.headers_mut().insert("cache-control","no-store".parse()?);
    response.headers_mut().insert("x-content-type-options","nosniff".parse()?);
    Ok(response)
}

async fn api_write(State(app): State<App>, UrlPath(path): UrlPath<String>, headers: HeaderMap, Json(mut body): Json<Value>) -> Response {
    if let Err(error) = app.authenticated(&headers,true).await { return failure(error); }
    if !body.is_object() {return failure(anyhow::anyhow!("invalid: request body must be an object"));}
    let pieces: Vec<_> = path.split('/').collect();
    // Restore excludes every writer and background job, not just active inference.
    let restoring=matches!(pieces.as_slice(),["backups",_,"restore"]);
    let _exclusive=if restoring {match app.maintenance.try_write() {Ok(guard)=>Some(guard),Err(_)=>return failure(anyhow::anyhow!("busy: active work prevents restore"))}} else {None};
    let _shared=if !restoring && pieces.as_slice()!=["feedback"] {Some(app.maintenance.read().await)} else {None};
    if let Some(id) = pieces.get(1) { body["id"] = json!(id); }
    let result = match pieces.as_slice() {
        ["feedback"] => app.feedback(body).await,
        ["settings"] => {
            let _guard = app.privacy.lock().await;
            let result = app.store.call("settings/update",body).await;
            if result.is_ok() {app.invalidate_assessments(false).await;}
            match result {Ok(settings)=>write_private(&app.root.join("recording-consent.json"),&settings).map(|_|settings),Err(error)=>Err(error)}
        }
        ["decisions",_,"reviews"] => {
            let _guard = app.privacy.lock().await;
            body=redact(&body).0;
            body["actor"]=json!({"actor_type":"human","source":"authenticated_workbench"});
            let result=app.store.call("reviews/create",body).await;
            if result.is_ok() {app.invalidate_assessments(false).await;}
            result
        }
        ["memory-versions"] => {
            body["id"]=json!(uuid::Uuid::new_v4().to_string());
            app.store.call("versions/create",body).await
        }
        ["memory-versions",_,"activate"] => {
            let _guard = app.privacy.lock().await;
            let result=app.store.call("versions/activate",body).await;
            if result.is_ok() {app.invalidate_assessments(false).await;}
            result
        },
        ["jobs"] => crate::evaluation::start(app.clone(),body).await,
        ["jobs",id,"cancel"] => app.store.call("jobs/update",json!({"id":id,"cancel_requested":true})).await,
        ["outbox",id,"retry"] => {
            let root=app.root.clone();let id=id.to_string();
            match tokio::task::spawn_blocking(move||Outbox::open(&root)?.retry(&id)).await {Ok(result)=>result,Err(error)=>Err(error.into())}
        }
        ["backups"] => app.store.call("backup/create",body).await,
        ["backups",_,"restore"] => {
            if app.worker.busy() { Err(anyhow::anyhow!("busy: cannot restore during inference")) }
            else {
                let _guard=app.privacy.lock().await;
                match app.store.call("backup/restore",body).await {
                    Ok(value)=>match app.store.call("settings/get",json!({})).await {
                        Ok(settings)=>write_private(&app.root.join("recording-consent.json"),&settings).and_then(|_|clear_exports(&app.root)).map(|_|value),
                        Err(error)=>Err(error),
                    },
                    Err(error)=>Err(error),
                }
            }
        }
        _ => Err(anyhow::anyhow!("not_found: endpoint")),
    };
    if result.is_ok() && pieces.as_slice()!=["feedback"] {app.invalidate_assessments(true).await;}
    app.changed.notify_waiters();
    match result { Ok(value)=>Json(value).into_response(), Err(error)=>failure(error) }
}

async fn api_delete(State(app): State<App>, UrlPath(path): UrlPath<String>, headers: HeaderMap) -> Response {
    if let Err(error) = app.authenticated(&headers,true).await { return failure(error); }
    let _maintenance=app.maintenance.write().await;
    let _guard = app.privacy.lock().await;
    let pieces: Vec<_> = path.split('/').collect();
    if matches!(pieces.as_slice(),["decisions",_]|["cases",_]) {
        if let Err(error)=clear_exports(&app.root) {return failure(error);}
    }
    let result = match pieces.as_slice() {
        ["decisions",id] => {
            let root=app.root.clone(); let owned=id.to_string();
            let detail=match app.store.call("decisions/get",json!({"id":id})).await {Ok(detail)=>detail,Err(error)=>return failure(error)};
            let cases=detail["reviews"].as_array().into_iter().flatten().filter_map(|review|review["revision"].as_u64().map(|revision|format!("{id}:{revision}"))).collect::<Vec<_>>();
            match tokio::task::spawn_blocking(move || {let outbox=Outbox::open(&root)?;outbox.withdraw_cases(&cases)?;outbox.delete_decision(&owned)}).await {
                Ok(Ok(()))=>app.store.call("decisions/delete",json!({"id":id})).await,
                other=>Err(anyhow::anyhow!("could not clear pending feedback: {other:?}")),
            }
        }
        ["cases",id] => {
            let root=app.root.clone();let owned=id.to_string();
            match tokio::task::spawn_blocking(move||Outbox::open(&root)?.withdraw_cases(&[owned])).await {
                Ok(Ok(()))=>app.store.call("cases/delete",json!({"id":id})).await,
                other=>Err(anyhow::anyhow!("could not clear pending case copies: {other:?}")),
            }
        }
        ["backups",id] => app.store.call("backup/delete",json!({"id":id})).await,
        _=>Err(anyhow::anyhow!("not_found: endpoint")),
    };
    if result.is_ok() {app.invalidate_assessments(true).await;}
    app.changed.notify_waiters();
    match result { Ok(value)=>Json(value).into_response(), Err(error)=>failure(error) }
}

fn write_private(path: &std::path::Path, value: &Value) -> Result<()> {
    use std::io::Write;
    let temp=path.with_extension(format!("{}.tmp",uuid::Uuid::new_v4()));
    let mut file=std::fs::OpenOptions::new().create_new(true).write(true).open(&temp)?;
    std::fs::set_permissions(&temp,std::fs::Permissions::from_mode(0o600))?;
    file.write_all(&serde_json::to_vec(value)?)?; file.sync_all()?;
    std::fs::rename(temp,path)?;
    if let Some(parent)=path.parent() {std::fs::File::open(parent)?.sync_all()?;}
    Ok(())
}

fn clear_exports(root:&std::path::Path)->Result<()> {
    let directory=root.join("exports");
    if !directory.exists() {return Ok(());}
    if directory.is_symlink() {bail!("invalid: symlinked export directory");}
    for entry in std::fs::read_dir(&directory)? {
        let entry=entry?;
        let name=entry.file_name(); let name=name.to_string_lossy();
        if name.starts_with("laya-export-") && (name.ends_with(".ndjson") || name.ends_with(".partial")) {
            std::fs::remove_file(entry.path())?;
        }
    }
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

pub async fn run(root: PathBuf) -> Result<()> {
    run_with_port(root,runtime::configured_port()?).await
}

pub async fn run_with_port(root:PathBuf,port:u16)->Result<()> {
    runtime::private_dir(&root)?;
    let lock=std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(root.join("service.lock"))?;
    if lock.try_lock_exclusive().is_err() { return Ok(()); }
    let socket=root.join("service.sock");
    if socket.exists() { std::fs::remove_file(&socket)?; }
    let unix=UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket,std::fs::Permissions::from_mode(0o600))?;
    let tcp=TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,port)).await
        .with_context(||format!("cannot listen on 127.0.0.1:{port}; use laya dashboard --port <free-port> if this port is occupied"))?;
    let info=ServiceInfo {pid:std::process::id(),instance:uuid::Uuid::new_v4().to_string(),port:tcp.local_addr()?.port(),protocol_version:1};
    let store=match Store::open(&root) {
        Ok(store)=>store,
        Err(error) if error.to_string().starts_with("invalid:")=>return Err(error),
        Err(error)=>{eprintln!("Workbench persistence unavailable: {error}");Store::unavailable(error.to_string())},
    };
    if let Ok(settings)=store.call("settings/get",json!({})).await {write_private(&root.join("recording-consent.json"),&settings)?;}
    write_private(&root.join("service.json"),&serde_json::to_value(&info)?)?;
    let python=std::env::var("LAYA_PYTHON").unwrap_or_else(|_|"python3".into());
    let idle=std::env::var("LAYA_IDLE_SECONDS").ok().and_then(|s|s.parse().ok()).unwrap_or(300);
    let app=App {store,worker:Worker::start(python,Duration::from_secs(idle),root.join("model.lock")),root:root.clone(),info,privacy:Arc::new(Mutex::new(())),maintenance:Arc::new(RwLock::new(())),job_guard:Arc::new(Mutex::new(())),changed:Arc::new(Notify::new()),shutdown:Arc::new(Notify::new()),stopping:Arc::new(AtomicBool::new(false)),assessment_epoch:Arc::new(AtomicU64::new(0)),sessions:Arc::new(Mutex::new(HashMap::new())),pairing:Arc::new(Mutex::new(HashMap::new()))};
    let internal=app.clone();
    let unix_task=tokio::spawn(async move {
        while let Ok((stream,_))=unix.accept().await {
            let state=internal.clone();
            tokio::spawn(async move {
                let (read,mut write)=stream.into_split();
                let mut reader=BufReader::new(read);
                if let Ok(Some(frame))=runtime::read_frame(&mut reader).await {
                    if let Ok(request)=serde_json::from_slice::<Request>(&frame) {
                        let outcome=tokio::select! {
                            outcome=state.internal(&request)=>Some(outcome),
                            _=runtime::read_frame(&mut reader)=>None,
                        };
                        let Some(outcome)=outcome else {return;};
                        let response=match outcome {
                            Ok(value)=>json!({"request_id":request.request_id,"result":value}),
                            Err(error)=>json!({"request_id":request.request_id,"error":error.to_string()}),
                        };
                        let mut bytes=serde_json::to_vec(&response).unwrap(); bytes.push(b'\n'); let _=write.write_all(&bytes).await;
                    }
                }
            });
        }
    });
    let replay=app.clone();
    let replay_task=tokio::spawn(async move {
        let mut retained_at=0;
        loop {
            let replay_enabled=replay.store.call("settings/get",json!({})).await.map(|settings|settings["replay_enabled"]!=false).unwrap_or(false);
            if !replay_enabled {tokio::time::sleep(Duration::from_secs(10)).await;continue;}
            {
                let _maintenance=replay.maintenance.read().await;
                let _privacy=replay.privacy.lock().await;
                let root=replay.root.clone();
                if let Ok(Ok(snapshots))=tokio::task::spawn_blocking(move||Outbox::open(&root)?.snapshot_pending()).await {
                    for snapshot in snapshots {
                        if let Ok(receipt)=replay.store.call(snapshot["method"].as_str().unwrap_or(""),snapshot["payload"].clone()).await {
                            if receipt["recording_status"]=="stored" {
                                let root=replay.root.clone();
                                let _=tokio::task::spawn_blocking(move||Outbox::open(&root)?.snapshot_ack(&snapshot)).await;
                            }
                        }
                    }
                }
                if now()-retained_at>=3600 {
                    let _=replay.store.call("retention",json!({})).await;
                    retained_at=now();
                }
            }
            let root=replay.root.clone();
            if let Ok(Ok(events))=tokio::task::spawn_blocking(move||Outbox::open(&root)?.pending()).await {
                for event in events {
                    let response=replay.feedback(event.clone()).await;
                    let root=replay.root.clone();
                    let _=tokio::task::spawn_blocking(move||->Result<()> {
                        let out=Outbox::open(&root)?;
                        match response {
                            Ok(receipt) if receipt["status"]=="stored"=>out.acknowledge(&event,&receipt),
                            Ok(_)=>Ok(()),
                            Err(error)=> { let message=error.to_string(); let permanent=["conflict:","invalid:","not_found:","deleted:"].iter().any(|prefix|message.starts_with(prefix)); out.failed(event["event_id"].as_str().unwrap_or(""),&message,permanent) }
                        }
                    }).await;
                }
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    });
    let routes=Router::new().route("/api/v1/pair",post(pair))
        .route("/api/v1/events",get(events))
        .route("/api/v1/{*path}",get(api_get).post(api_write).patch(api_write).delete(api_delete))
        .fallback(assets)
        .layer(axum::middleware::from_fn(|request:axum::extract::Request,next:axum::middleware::Next|async move {
            let api=request.uri().path().starts_with("/api/");
            let mut response=next.run(request).await;
            if api {response.headers_mut().insert("cache-control","no-store".parse().unwrap());}
            response.headers_mut().insert("x-content-type-options","nosniff".parse().unwrap());
            response
        }))
        .layer(axum::extract::DefaultBodyLimit::max(1024*1024)).with_state(app.clone());
    let stop=app.shutdown.clone();
    let stopping=app.clone();
    axum::serve(tcp,routes).with_graceful_shutdown(async move {tokio::select!{_ = stop.notified()=>{},_ = tokio::signal::ctrl_c()=>{}} stopping.stopping.store(true,Ordering::Release); stopping.changed.notify_waiters();}).await?;
    unix_task.abort(); replay_task.abort();
    app.worker.call("release",json!({})).await?;
    std::fs::remove_file(socket)?;
    drop(lock);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_applicability_exclusions_are_not_injected_into_worker() {
        let memory=json!({"cases":[{"id":"task-fact","summary":"Keep rollback evidence","labels":{"risk":"high"}}],"applicability_exclusions":[{"case_id":"old-model-case","status":"needs_revalidation","reason":"catalog changed"}]});
        assert_eq!(worker_cases(&memory),json!([{"id":"task-fact","summary":"Keep rollback evidence","labels":{"risk":"high"}}]));
    }

    #[test]
    fn advisor_routing_scope_is_validated_canonicalized_and_removed_from_worker_input() {
        let mut params=json!({"state":"Update the README","advisor":{"task_family":"docs","task_lineage":"repo:readme/typo-1","models":[]}});
        assert_eq!(take_advisor_routing_scope(&mut params).unwrap(),(Some("documentation".into()),Some("repo:readme/typo-1".into())));
        assert!(params["advisor"].get("task_family").is_none());
        assert!(params["advisor"].get("task_lineage").is_none());
        let mut invalid=json!({"state":"task","advisor":{"task_family":"Authorization Boundary","models":[]}});
        assert!(take_advisor_routing_scope(&mut invalid).unwrap_err().to_string().starts_with("invalid:"));
        let mut missing=json!({"state":"task","advisor":{"models":[]}});
        assert_eq!(take_advisor_routing_scope(&mut missing).unwrap(),(None,None));
    }

    fn app(root:&std::path::Path)->App {
        App {store:Store::open(root).unwrap(),worker:Worker::start("nonexistent-python".into(),Duration::from_secs(1),root.join("model.lock")),root:root.into(),info:ServiceInfo{pid:1,instance:"test".into(),port:34567,protocol_version:1},privacy:Arc::new(Mutex::new(())),maintenance:Arc::new(RwLock::new(())),job_guard:Arc::new(Mutex::new(())),changed:Arc::new(Notify::new()),shutdown:Arc::new(Notify::new()),stopping:Arc::new(AtomicBool::new(false)),assessment_epoch:Arc::new(AtomicU64::new(0)),sessions:Arc::new(Mutex::new(HashMap::new())),pairing:Arc::new(Mutex::new(HashMap::new()))}
    }

    #[tokio::test]
    async fn typed_decisions_obey_recording_consent_and_preserve_results() {
        for recording in [false,true] {
            let dir=tempfile::tempdir().unwrap(); let mut app=app(dir.path());
            let fixture=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/workbench_worker.py");
            app.worker=Worker::start(fixture.to_string_lossy().into_owned(),Duration::ZERO,dir.path().join("model.lock"));
            app.store.call("settings/update",json!({"recording_enabled":recording})).await.unwrap();
            let request=Request::new("predict",json!({"state":"typed capture regression","questions":{"risk":{"type":"choice","instructions":"Classify risk","criteria":["low","high"]}}}));
            let result=app.predict(&request).await.unwrap();
            app.worker.call("release",json!({})).await.unwrap();
            assert_eq!(result["meta"]["decision_id"],request.request_id);
            assert_eq!(result["meta"]["recording_status"],if recording {"stored"}else{"not_recorded"});
            if recording {
                let stored=app.store.call("decisions/get",json!({"id":request.request_id})).await.unwrap();
                assert_eq!(stored["request"],request.params);
                assert_eq!(stored["result"],result);
                assert_eq!(app.predict(&request).await.unwrap(),result);
                let failed=Request::new("predict",json!({"state":"fixture:crash","questions":request.params["questions"]}));
                assert!(app.predict(&failed).await.is_err());
                let stored=app.store.call("decisions/get",json!({"id":failed.request_id})).await.unwrap();
                assert!(!stored["error"].is_null());
            } else {
                assert!(app.store.call("decisions/get",json!({"id":request.request_id})).await.is_err());
            }
        }
    }

    #[tokio::test]
    async fn browser_requires_cookie_host_and_write_origin() {
        let dir=tempfile::tempdir().unwrap(); let app=app(dir.path());
        let mut headers=HeaderMap::new(); headers.insert("host","127.0.0.1:34567".parse().unwrap());
        assert!(app.authenticated(&headers,false).await.is_err());
        app.sessions.lock().await.insert("test-cookie".into(),now()+60);
        headers.insert("cookie","laya_session=test-cookie".parse().unwrap());
        assert!(app.authenticated(&headers,false).await.is_ok());
        assert!(app.authenticated(&headers,true).await.is_err());
        headers.insert("origin","https://attacker.invalid".parse().unwrap());
        assert!(app.authenticated(&headers,true).await.is_err());
        headers.insert("origin","http://127.0.0.1:34567".parse().unwrap());
        assert!(app.authenticated(&headers,true).await.is_ok());
        assert_eq!(app.worker.status()["pid"],0);
    }

    #[tokio::test]
    async fn pairing_is_single_use_and_agent_cannot_publish() {
        let dir=tempfile::tempdir().unwrap(); let app=app(dir.path());
        app.pairing.lock().await.insert("once".into(),now()+60);
        let mut headers=HeaderMap::new(); headers.insert("host","127.0.0.1:34567".parse().unwrap()); headers.insert("origin","http://127.0.0.1:34567".parse().unwrap());
        assert_eq!(pair(State(app.clone()),headers.clone(),Json(json!({"code":"once"}))).await.status(),StatusCode::OK);
        assert_eq!(pair(State(app.clone()),headers,Json(json!({"code":"once"}))).await.status(),StatusCode::UNAUTHORIZED);
        assert!(app.internal(&Request::new("versions/activate",json!({"id":"arbitrary"}))).await.is_err());
    }

    #[tokio::test]
    async fn stale_assessment_finish_is_atomic_with_epoch_and_never_queues_result() {
        for recording in [false,true] {
            let dir=tempfile::tempdir().unwrap(); let app=app(dir.path());
            if recording {app.store.call("settings/update",json!({"recording_enabled":true})).await.unwrap();}
            let stale_id=if recording {"stale-recorded"}else{"stale-unrecorded"};
            app.persist_snapshot("decisions/begin",json!({"id":stale_id,"request_id":stale_id,"request":{"state":"stale"}})).await.unwrap();

            let privacy=app.privacy.lock().await;
            let gate=Arc::new(tokio::sync::Barrier::new(2));
            let task_app=app.clone(); let task_gate=gate.clone();
            let finish=tokio::spawn(async move {
                task_gate.wait().await;
                task_app.persist_snapshot("decisions/finish",json!({
                    "id":stale_id,"assessment_epoch":0,
                    "result":{"value":"must-not-persist","meta":{"assessment_cache":{"status":"hit"}}}
                })).await
            });
            gate.wait().await;
            app.assessment_epoch.fetch_add(1,Ordering::SeqCst);
            drop(privacy);

            let error=finish.await.unwrap().unwrap_err();
            assert!(error.to_string().starts_with("stale_assessment:"));
            assert_eq!(Outbox::open(dir.path()).unwrap().status().unwrap()["pending_snapshots"],0);
            app.persist_snapshot("decisions/finish",json!({"id":stale_id,"error":{"message":error.to_string(),"kind":"stale_assessment"}})).await.unwrap();
            assert_eq!(Outbox::open(dir.path()).unwrap().status().unwrap()["pending_snapshots"],0);
            if recording {
                let detail=app.store.call("decisions/get",json!({"id":stale_id})).await.unwrap();
                assert!(detail["result"].is_null());
                assert_eq!(detail["error"]["kind"],"stale_assessment");
            } else {
                assert_eq!(app.store.call("status",json!({})).await.unwrap()["counts"]["decisions"],0);
            }

            let matching_id=if recording {"matching-recorded"}else{"matching-unrecorded"};
            app.persist_snapshot("decisions/begin",json!({"id":matching_id,"request_id":matching_id,"request":{"state":"matching"}})).await.unwrap();
            let receipt=app.persist_snapshot("decisions/finish",json!({
                "id":matching_id,"assessment_epoch":1,
                "result":{"value":"fresh","meta":{"assessment_cache":{"status":"hit"}}}
            })).await.unwrap();
            assert_eq!(receipt["recording_status"],if recording {"stored"}else{"not_recorded"});
            assert_eq!(Outbox::open(dir.path()).unwrap().status().unwrap()["pending_snapshots"],0);
            if recording {
                let detail=app.store.call("decisions/get",json!({"id":matching_id})).await.unwrap();
                assert_eq!(detail["result"]["value"],"fresh");
            }
        }
    }

    #[test]
    fn decision_query_filters_are_strictly_parsed() {
        let query=HashMap::from([
            ("filter".into(),"pending".into()),
            ("risk".into(),"high".into()),
            ("source".into(),"reviewer".into()),
            ("created_after".into(),"10".into()),
            ("created_before".into(),"20".into())
        ]);
        let params=decisions_list_params(&query).unwrap();
        assert_eq!(params["filter"],json!({"status":"pending","risk":"high","source":"reviewer","created_after":10,"created_before":20}));
        assert!(decisions_list_params(&HashMap::from([("created_after".into(),"yesterday".into())])).unwrap_err().to_string().starts_with("invalid:"));
        assert!(decisions_list_params(&HashMap::from([("where".into(),"1=1".into())])).unwrap_err().to_string().starts_with("invalid:"));
    }

    #[test]
    fn overview_and_case_time_queries_are_strictly_parsed() {
        let overview=time_filter_params(&HashMap::from([("created_after".into(),"10".into()),("created_before".into(),"20".into())]),false).unwrap();
        assert_eq!(overview,json!({"created_after":10,"created_before":20}));
        let cases=time_filter_params(&HashMap::from([("created_after".into(),"10".into()),("limit".into(),"5".into()),("offset".into(),"2".into())]),true).unwrap();
        assert_eq!(cases,json!({"created_after":10,"limit":5,"offset":2}));
        assert!(time_filter_params(&HashMap::from([("created_after".into(),"yesterday".into())]),false).unwrap_err().to_string().starts_with("invalid:"));
        assert!(time_filter_params(&HashMap::from([("unknown".into(),"1".into())]),false).unwrap_err().to_string().starts_with("invalid:"));
        assert!(time_filter_params(&HashMap::from([("limit".into(),"1".into())]),false).unwrap_err().to_string().starts_with("invalid:"));
    }
}
