use anyhow::{bail, Context, Result};
use laya::{protocol::Request,runtime};
use serde_json::json;
use fs2::FileExt;
use std::os::unix::{fs::OpenOptionsExt,process::CommandExt};

#[tokio::main(flavor="multi_thread",worker_threads=2)]
async fn main() -> Result<()> {
    let args:Vec<String>=std::env::args().skip(1).collect();
    let root=runtime::root();
    match args.first().map(String::as_str).unwrap_or("--help") {
        "--version"=>println!("laya {}",env!("CARGO_PKG_VERSION")),
        "--help"|"-h"=>println!("Oh My Laya Decision Workbench\nUsage: laya dashboard [--port PORT] [--no-open] | status | stop | --snake [options]\nDefault: http://127.0.0.1:18686 (override with --port or LAYA_PORT; 0 requests a temporary port)\nInternal: laya mcp | service [--port PORT]"),
        "service"=> {
            let port=match command_port(&args[1..],false)? {Some(port)=>port,None=>runtime::configured_port()?};
            laya::service::run_with_port(root,port).await?;
        }
        "mcp"=>laya::mcp::run(&root).await?,
        "dashboard"=> {
            runtime::ensure_port(&root,command_port(&args[1..],true)?).await?;
            let result=runtime::rpc(&root,&Request::new("pair",json!({}))).await?;
            let url=result["url"].as_str().context("missing dashboard URL")?;
            if args.iter().any(|a|a=="--no-open") { println!("{url}"); }
            else {
                let status=std::process::Command::new(if cfg!(target_os="macos") {"open"}else{"xdg-open"}).arg(url).status()?;
                if !status.success() { bail!("could not open browser; use laya dashboard --no-open"); }
            }
        }
        "status"=>println!("{}",serde_json::to_string_pretty(&runtime::status(&root).await?)?),
        "stop"=>println!("{}",runtime::rpc(&root,&Request::new("stop",json!({}))).await?),
        "--snake"=> {
            if let Ok(status)=runtime::rpc(&root,&Request::new("status",json!({}))).await {
                if status["worker"]["busy"]==true || status["worker"]["queued"].as_u64().unwrap_or(0)>0 { bail!("Laya is busy; finish active tasks before starting the demo"); }
                runtime::rpc(&root,&Request::new("stop",json!({}))).await?;
            }
            eprintln!("Snake runs a separate model process; the idle workbench service has been stopped.");
            runtime::private_dir(&root)?;
            let lock_path=root.join("model.lock");
            if lock_path.is_symlink() {bail!("symlinked model lock is not allowed");}
            let model_lock=std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).mode(0o600).open(lock_path)?;
            model_lock.try_lock_exclusive().context("another Laya model is using this workbench; retry after it stops")?;
            let command=std::env::var("LAYA_SNAKE_BIN").context("demo command not configured; rerun Oh My Laya installer")?;
            let model=std::env::var("LAYA_MODEL_DIR").context("model directory not configured")?;
            let mut command=std::process::Command::new(command);
            command.args(["--model",&model]).args(&args[1..]);
            runtime::inherit_model_lock(&mut command,&model_lock);
            return Err(command.exec().into());
        }
        other=>bail!("unknown command: {other}"),
    }
    Ok(())
}

fn command_port(args:&[String],dashboard:bool)->Result<Option<u16>> {
    let mut port=None;
    let mut values=args.iter();
    while let Some(arg)=values.next() {
        if dashboard && arg=="--no-open" {continue;}
        let value=if arg=="--port" {values.next().context("--port requires a value")?.as_str()}
            else if let Some(value)=arg.strip_prefix("--port=") {value}
            else {bail!("unknown option: {arg}");};
        if port.is_some() {bail!("--port may only be supplied once");}
        port=Some(runtime::parse_port(value)?);
    }
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values:&[&str])->Vec<String> {values.iter().map(|v|v.to_string()).collect()}
    #[test]
    fn dashboard_ports_are_explicit_and_validated() {
        assert_eq!(command_port(&[],true).unwrap(),None);
        assert_eq!(command_port(&args(&["--no-open","--port","18686"]),true).unwrap(),Some(18686));
        assert_eq!(command_port(&args(&["--port=0"]),false).unwrap(),Some(0));
        for values in [vec!["--port"],vec!["--port=-1"],vec!["--port=65536"],vec!["--port=bad"],vec!["--port=1","--port=2"],vec!["--unknown"]] {
            assert!(command_port(&args(&values),true).is_err());
        }
        assert!(command_port(&args(&["--no-open"]),false).is_err());
        assert_eq!(runtime::DEFAULT_PORT,18686);
    }
}
