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
        "--help"|"-h"=>println!("Laya Decision & Learning Workbench\nUsage: laya dashboard | status | stop | --snake [options]\nInternal: laya mcp | service"),
        "service"=>laya::service::run(root).await?,
        "mcp"=>laya::mcp::run(&root).await?,
        "dashboard"=> {
            runtime::ensure(&root).await?;
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
