use crate::protocol::{Request, MAX_MESSAGE};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{os::unix::fs::PermissionsExt, path::{Path, PathBuf}, process::Stdio};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, net::UnixStream};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceInfo { pub pid: u32, pub instance: String, pub port: u16, pub protocol_version: u32 }

pub const DEFAULT_PORT: u16 = 18686;

pub fn configured_port() -> Result<u16> {
    match std::env::var("LAYA_PORT") {
        Ok(value)=>parse_port(&value),
        Err(std::env::VarError::NotPresent)=>Ok(DEFAULT_PORT),
        Err(error)=>Err(error).context("LAYA_PORT must be a valid port"),
    }
}

pub fn parse_port(value:&str)->Result<u16> {
    if value.is_empty() || !value.bytes().all(|c|c.is_ascii_digit()) {bail!("port must be an integer from 0 to 65535");}
    value.parse().context("port must be an integer from 0 to 65535")
}

/// The model-bearing child keeps the same flock open description after exec.
/// Parent SIGKILL must not release exclusivity while that child still exists.
pub fn inherit_model_lock(command:&mut std::process::Command,lock:&std::fs::File) {
    use std::os::{fd::AsRawFd,unix::process::CommandExt};
    let fd=lock.as_raw_fd();
    command.env("LAYA_MODEL_LOCK_FD",fd.to_string());
    // Only async-signal-safe fcntl runs between fork and exec.
    unsafe {command.pre_exec(move|| {
        if libc::fcntl(fd,libc::F_SETFD,0)==-1 {Err(std::io::Error::last_os_error())}else{Ok(())}
    });}
}

pub fn root() -> PathBuf {
    std::env::var_os("LAYA_WORKBENCH_DIR").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share/oh-my-laya/workbench")
    })
}

pub fn private_dir(root: &Path) -> Result<()> {
    for component in root.ancestors().take_while(|p| p.parent().is_some()) {
        if component.is_symlink() { bail!("symlinked runtime path is not allowed: {}", component.display()); }
    }
    std::fs::create_dir_all(root)?;
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

pub async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> Result<Option<Vec<u8>>> {
    let mut result = Vec::new();
    loop {
        let part = reader.fill_buf().await?;
        if part.is_empty() {
            if result.is_empty() { return Ok(None); }
            bail!("incomplete frame");
        }
        let n = part.iter().position(|b| *b == b'\n').map(|i| i+1).unwrap_or(part.len());
        if result.len() + n > MAX_MESSAGE { bail!("frame too large"); }
        let done = part[n-1] == b'\n';
        result.extend_from_slice(&part[..n]); reader.consume(n);
        if done { return Ok(Some(result)); }
    }
}

pub async fn rpc(root: &Path, request: &Request) -> Result<Value> {
    request.validate()?;
    let stream = UnixStream::connect(root.join("service.sock")).await?;
    exchange(stream,request).await
}

fn no_listener(error:&std::io::Error)->bool {
    matches!(error.kind(),std::io::ErrorKind::NotFound|std::io::ErrorKind::ConnectionRefused)
}

/// A failed observation is not proof that the service is stopped.
pub async fn status(root:&Path)->Result<Value> {
    let stream=match UnixStream::connect(root.join("service.sock")).await {
        Ok(stream)=>stream,
        Err(error) if no_listener(&error)=>return Ok(serde_json::json!({"running":false,"state":"not_listening","data_directory":root})),
        Err(error)=>return Err(error).context("service state unknown: unable to connect to status endpoint"),
    };
    exchange(stream,&Request::new("status",serde_json::json!({}))).await.context("service state unknown: status exchange failed")
}

async fn exchange(mut stream:UnixStream,request:&Request)->Result<Value> {
    let mut bytes = serde_json::to_vec(request)?; bytes.push(b'\n');
    stream.write_all(&bytes).await?;
    let mut reader = BufReader::new(stream);
    let frame = tokio::time::timeout(std::time::Duration::from_secs(125), read_frame(&mut reader)).await??.context("service closed")?;
    let response: Value = serde_json::from_slice(&frame)?;
    if response["request_id"] != request.request_id { bail!("service identity mismatch"); }
    if let Some(error) = response.get("error") { bail!("{}", error.as_str().unwrap_or("service error")); }
    response.get("result").cloned().context("missing service result")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_missing_or_refused_connect_means_no_listener() {
        assert!(no_listener(&std::io::Error::from_raw_os_error(libc::ENOENT)));
        assert!(no_listener(&std::io::Error::from_raw_os_error(libc::ECONNREFUSED)));
        for errno in [libc::EACCES,libc::EPERM,libc::ETIMEDOUT,libc::ECONNRESET,libc::ENOTDIR] {
            assert!(!no_listener(&std::io::Error::from_raw_os_error(errno)));
        }
    }

    #[tokio::test]
    async fn missing_status_socket_does_not_create_runtime() {
        let directory=tempfile::tempdir().unwrap();
        let root=directory.path().join("absent");
        let result=status(&root).await.unwrap();
        assert_eq!(result["running"],false);
        assert_eq!(result["state"],"not_listening");
        assert!(!root.exists());
    }

    #[tokio::test]
    async fn malformed_status_is_unknown_not_stopped() {
        let directory=tempfile::tempdir_in("/private/tmp").unwrap();
        let listener=tokio::net::UnixListener::bind(directory.path().join("service.sock")).unwrap();
        let server=tokio::spawn(async move {
            let (stream,_)=listener.accept().await.unwrap();
            let mut reader=BufReader::new(stream);
            read_frame(&mut reader).await.unwrap();
            reader.get_mut().write_all(b"not-json\n").await.unwrap();
        });
        let error=status(directory.path()).await.unwrap_err();
        assert!(error.to_string().contains("service state unknown"));
        server.await.unwrap();
    }
}

pub async fn ensure(root: &Path) -> Result<()> {
    ensure_port(root,None).await
}

pub async fn ensure_port(root:&Path,requested:Option<u16>)->Result<()> {
    let ping = Request::new("status", Value::Null);
    let port=match requested {Some(port)=>port,None=>configured_port()?};
    let verify=|status:&Value|->Result<()> {
        if requested.is_some() && port!=0 && status["service"]["port"].as_u64()!=Some(port as u64) {
            bail!("service already running on port {}; finish active tasks, run laya stop, then laya dashboard --port {port}",status["service"]["port"]);
        }
        Ok(())
    };
    if let Ok(status)=rpc(root, &ping).await { verify(&status)?; return Ok(()); }
    private_dir(root)?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(root.join("service.log"))?;
    std::fs::set_permissions(root.join("service.log"), std::fs::Permissions::from_mode(0o600))?;
    let mut child=std::process::Command::new(std::env::current_exe()?).args(["service","--port",&port.to_string()])
        .env("LAYA_WORKBENCH_DIR",root).stdin(Stdio::null()).stdout(Stdio::null()).stderr(log)
        .spawn().context("launch local service")?;
    for _ in 0..100 {
        if let Ok(status)=rpc(root,&ping).await { verify(&status)?; return Ok(()); }
        if let Some(status)=child.try_wait()? {
            if !status.success() {bail!("service failed to start on 127.0.0.1:{port}; the port may be occupied. Try laya dashboard --port <free-port>; inspect {}",root.join("service.log").display());}
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    bail!("service failed to start; inspect {}",root.join("service.log").display())
}
