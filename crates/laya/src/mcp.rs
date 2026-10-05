use crate::{outbox::Outbox, protocol::{Request, redact}, runtime};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{collections::HashMap,path::Path};
use tokio::io::{AsyncWriteExt, BufReader};

pub fn tools() -> Value {
    let mut tools=json!({"tools":[
        {"name":"laya_tell_me","description":"Run a local typed Laya decision. Advisor recommendations never authorize actions or change the current model.","inputSchema":{"type":"object","required":["state"],"additionalProperties":false,"properties":{"state":{"anyOf":[{"type":"string"},{"type":"object"},{"type":"array"}]},"questions":{"type":["object","null"]},"advisor":{"type":["object","null"]}}}},
        {"name":"laya_advisor_preferences","description":"Read settings; save only after user's Confirm and continue. Recommendation permissions do not authorize execution or recording.","inputSchema":{"type":"object","additionalProperties":false,"properties":{"policy":{"type":["string","null"],"enum":["always","conditional","auto",null]},"ceiling":{"type":["object","null"]},"models":{"type":["array","null"]},"squad":{"type":["object","null"]}}}},
        {"name":"laya_feedback","description":"Record structured Squad evidence with stable event_id. Preserve initial scores; retry the exact same event. stored means committed; queued_local means durably awaiting delivery. Does not approve labels or activate learning.","inputSchema":{"type":"object","additionalProperties":false,"required":["protocol_version","event_id","decision_id","attempt_ref","kind","source","payload"],"properties":{"protocol_version":{"type":"integer","const":1},"event_id":{"type":"string"},"decision_id":{"type":"string"},"attempt_ref":{"type":"string"},"kind":{"enum":["assignment","test","review","outcome","user_choice","usage","run_manifest"]},"source":{"type":"object"},"payload":{"type":"object"}}}}
    ]});
    tools["tools"][2]["inputSchema"]=serde_json::from_str(include_str!("../../../contracts/feedback.schema.json")).expect("bundled feedback schema is valid JSON");
    tools["tools"][0]["description"]=json!("Run a local typed Laya decision. Provide questions keyed by question id, each with type, instructions and criteria (not options). Alternatively omit questions and provide advisor. Advisor recommendations never authorize actions or change the current model.");
    tools["tools"][0]["inputSchema"]["properties"]["questions"]=json!({
        "type":["object","null"],"minProperties":1,"maxProperties":64,
        "description":"Required unless advisor is provided. Do not supply both. Example: {\"risk\":{\"type\":\"choice\",\"instructions\":\"Classify risk\",\"criteria\":{\"low\":\"Documentation edit\",\"high\":\"Data loss\"}}}",
        "additionalProperties":{
            "type":"object","required":["type","instructions"],
            "properties":{
                "type":{"type":"string","enum":["choice","score","noul"]},
                "instructions":{"description":"Question instructions; use a string."},
                "criteria":{"description":"choice: nonempty unique string labels or label-to-description object. score: nonempty list ordered lowest to highest. noul: optional false/true description object.","type":["array","object","null"]}
            },
            "allOf":[
                {"if":{"properties":{"type":{"const":"choice"}}},"then":{"required":["criteria"],"properties":{"criteria":{"anyOf":[{"type":"array","minItems":1,"uniqueItems":true,"items":{"type":"string"}},{"type":"object","minProperties":1}]}}}},
                {"if":{"properties":{"type":{"const":"score"}}},"then":{"required":["criteria"],"properties":{"criteria":{"type":"array","minItems":1}}}},
                {"if":{"properties":{"type":{"const":"noul"}}},"then":{"properties":{"criteria":{"type":["object","null"]}}}}
            ]
        }
    });
    tools["tools"][0]["inputSchema"]["properties"]["advisor"]["properties"]=json!({
        "task_family":{"type":"string","maxLength":64,"pattern":"^[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?$","description":"Optional reviewed-memory routing scope. Canonicalized to lowercase; docs aliases to documentation. Omit when unknown."},
        "task_lineage":{"type":"string","maxLength":128,"pattern":"^[A-Za-z0-9][A-Za-z0-9_.:/-]*$","description":"Optional independent task/holdout provenance. Same-lineage memory is excluded. Omit when unknown; it is never inferred."}
    });
    tools["tools"][0]["inputSchema"]["properties"]["advisor"]["description"]=json!("Advisor catalog and optional memory routing metadata. Retrieval language is derived from state; no advisor.language field is accepted for routing.");
    tools["tools"][0]["inputSchema"]["properties"]["orchestration"]=json!({
        "type":["object","null"],"additionalProperties":false,
        "description":"Opt-in minimal orchestration. Host-declared constraints are obligations, not execution permission. Omit for legacy behavior.",
        "required":["schema_version","enabled","run_id","stage_id","snapshot_revision","independent_work","dependencies_known","required_roles","constraint_refs"],
        "properties":{
            "schema_version":{"const":1,"type":"integer"},"enabled":{"type":"boolean"},
            "run_id":{"type":"string","minLength":1,"maxLength":128},
            "stage_id":{"type":"string","minLength":1,"maxLength":128},
            "snapshot_revision":{"type":"string","minLength":1,"maxLength":128},
            "independent_work":{"type":"boolean"},"dependencies_known":{"type":"boolean"},
            "required_roles":{"type":"array","uniqueItems":true,"maxItems":5,"items":{"enum":["explorer","worker","tester","researcher","reviewer"]}},
            "constraint_refs":{"type":"array","uniqueItems":true,"maxItems":32,"items":{"type":"string","minLength":1,"maxLength":256}}
        }
    });
    tools
}

pub async fn call(root: &Path, name: &str, mut arguments: Value) -> Result<Value> {
    match name {
        "laya_tell_me" | "laya_advisor_preferences" => {
            runtime::ensure(root).await?;
            let mut unsupported=false;
            if name=="laya_tell_me" && arguments.get("orchestration").is_some() {
                let peer=runtime::rpc(root,&Request::new("status",json!({}))).await?;
                unsupported=crate::protocol::negotiate_orchestration(&mut arguments,&peer)?;
            }
            let mut result=runtime::rpc(root,&Request::new(if name == "laya_tell_me" {"predict"} else {"preferences"},arguments)).await?;
            if unsupported {result["meta"]["orchestration_status"]=json!("unsupported_service");}
            Ok(result)
        }
        "laya_feedback" => {
            // Offline consent comes only from the service-owned durable consent marker.
            // Missing marker is fail-closed: model policy approval is not recording consent.
            let enabled = std::fs::read_to_string(root.join("recording-consent.json"))
                .ok().and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .map(|v| v["recording_enabled"] == true).unwrap_or(false);
            if !enabled { return Ok(json!({"status":"not_recorded","event_id":arguments["event_id"],"reason":"recording_disabled"})); }
            let out = Outbox::open(root)?;
            let receipt = out.enqueue(&arguments)?;
            if receipt["status"] == "stored" { return Ok(receipt); }
            let replay_enabled=std::fs::read_to_string(root.join("recording-consent.json")).ok().and_then(|s|serde_json::from_str::<Value>(&s).ok()).map(|settings|settings["replay_enabled"]!=false).unwrap_or(false);
            if !replay_enabled {return Ok(receipt);}
            let (clean, _) = redact(&arguments);
            let request = Request::new("feedback",clean.clone());
            let outcome = runtime::ensure(root).await;
            if outcome.is_ok() {
                match runtime::rpc(root,&request).await {
                    Ok(response) if response["status"] == "stored" => { out.acknowledge(&clean,&response)?; return Ok(response); }
                    Ok(response) => return Ok(response),
                    Err(error) => {
                        let text = error.to_string();
                        let permanent = ["invalid:","conflict:","not_found:","deleted:"].iter().any(|s| text.starts_with(s));
                        out.failed(clean["event_id"].as_str().context("event id")?, &text, permanent)?;
                        if permanent { return Ok(json!({"status":"quarantined","event_id":clean["event_id"],"error":text})); }
                    }
                }
            }
            Ok(receipt)
        }
        _ => anyhow::bail!("unknown tool"),
    }
}

pub async fn run(root: &Path) -> Result<()> {
    let (sender,mut input)=tokio::sync::mpsc::channel(32);
    let reader=tokio::spawn(async move {
        let mut source=BufReader::new(tokio::io::stdin());
        loop {
            match runtime::read_frame(&mut source).await {
                Ok(Some(line))=>if sender.send(Ok(line)).await.is_err() {break;},
                Ok(None)=>break,
                Err(error)=>{let _=sender.send(Err(error)).await;break;},
            }
        }
    });
    let mut output = tokio::io::stdout();
    let mut jobs=tokio::task::JoinSet::new();
    let mut pending:HashMap<String,tokio::task::AbortHandle>=HashMap::new();
    let mut eof=false;
    loop {
        let line=tokio::select! {
            completed=jobs.join_next(),if !jobs.is_empty()=> {
                if let Some(Ok((key,response)))=completed {
                    pending.remove(&key);
                    write_response(&mut output,&response).await?;
                }
                continue;
            }
            line=input.recv(),if !eof=>line,
            else=>break,
        };
        let Some(line)=line else {eof=true;continue;};
        let line=line?;
        let request: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(_) => {
                output.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32700,\"message\":\"Parse error\"}}\n").await?;
                continue;
            }
        };
        if request["method"]=="notifications/cancelled" {
            let key=request["params"]["requestId"].to_string();
            if let Some(handle)=pending.remove(&key) {handle.abort();}
            continue;
        }
        let Some(id) = request.get("id") else { continue; };
        let method = request["method"].as_str().unwrap_or("");
        if method=="tools/call" {
            let key=id.to_string();
            if pending.len()>=32 || pending.contains_key(&key) {
                write_response(&mut output,&json!({"jsonrpc":"2.0","id":id,"error":{"code":-32600,"message":"Duplicate request id or connection is busy"}})).await?;
                continue;
            }
            let root=root.to_path_buf();let key_for_job=key.clone();
            let handle=jobs.spawn(async move {
                let name=request["params"]["name"].as_str().unwrap_or("");
                let args=request["params"].get("arguments").cloned().unwrap_or(json!({}));
                let response=match call(&root,name,args).await {
                    Ok(result)=>json!({"jsonrpc":"2.0","id":request["id"],"result":{"content":[{"type":"text","text":result.to_string()}],"structuredContent":result,"isError":false}}),
                    Err(error)=>json!({"jsonrpc":"2.0","id":request["id"],"result":{"content":[{"type":"text","text":error.to_string()}],"isError":true}}),
                };
                (key_for_job,response)
            });
            pending.insert(key,handle);
            continue;
        }
        let response = match method {
            "initialize" => {
                let proposed=request["params"]["protocolVersion"].as_str().unwrap_or("");
                let version=if ["2024-11-05","2025-03-26","2025-06-18","2025-11-25"].contains(&proposed) {proposed}else{"2025-11-25"};
                json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":version,"capabilities":{"tools":{},"experimental":{"oh-my-laya":{"contracts":["orchestration_plan_v1"],"requires_runtime_negotiation":true}}},"serverInfo":{"name":"oh-my-laya","title":"Oh My Laya","version":env!("CARGO_PKG_VERSION")}}})
            },
            "ping" => json!({"jsonrpc":"2.0","id":id,"result":{}}),
            "tools/list" => json!({"jsonrpc":"2.0","id":id,"result":tools()}),
            _ => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}),
        };
        write_response(&mut output,&response).await?;
    }
    reader.abort();
    Ok(())
}

async fn write_response(output:&mut tokio::io::Stdout,response:&Value)->Result<()> {
    let mut bytes=serde_json::to_vec(response)?;bytes.push(b'\n');
    output.write_all(&bytes).await?;output.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_schema_explains_question_fields_and_modes() {
        let catalog=tools();
        let tool=&catalog["tools"][0];
        let questions=&tool["inputSchema"]["properties"]["questions"];
        let definition=&questions["additionalProperties"];
        assert_eq!(definition["required"],json!(["type","instructions"]));
        assert_eq!(definition["properties"]["type"]["enum"],json!(["choice","score","noul"]));
        assert_eq!(definition["allOf"][0]["then"]["required"],json!(["criteria"]));
        assert_eq!(definition["allOf"][1]["then"]["properties"]["criteria"]["minItems"],1);
        assert_eq!(questions["maxProperties"],64);
        assert!(questions["description"].as_str().unwrap().contains("Example:"));
        assert!(tool["description"].as_str().unwrap().contains("omit questions and provide advisor"));
        let advisor=&tool["inputSchema"]["properties"]["advisor"];
        assert_eq!(advisor["properties"]["task_family"]["maxLength"],64);
        assert_eq!(advisor["properties"]["task_lineage"]["maxLength"],128);
        assert!(advisor["description"].as_str().unwrap().contains("language is derived"));
    }
}
