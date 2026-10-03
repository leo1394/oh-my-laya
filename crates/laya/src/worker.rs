use crate::protocol::{Request, MAX_MESSAGE};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::{path::{Path,PathBuf},os::unix::fs::OpenOptionsExt,process::Stdio, sync::{Arc, atomic::{AtomicBool, AtomicU32, Ordering}}, time::Duration};
use fs2::FileExt;
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, process::{Child, ChildStdin, ChildStdout, Command}, sync::{mpsc, oneshot}};

type Reply = oneshot::Sender<Result<Value>>;
struct Work { request: Request, reply: Reply, deadline: tokio::time::Instant }

#[derive(Clone)]
pub struct Worker {
    sender: mpsc::Sender<Work>,
    busy: Arc<AtomicBool>,
    pid: Arc<AtomicU32>,
}

struct Process { child: Child, input: ChildStdin, output: BufReader<ChildStdout>, _model_lock:std::fs::File }

impl Process {
    async fn start(python: &str,lock_path:&Path) -> Result<Self> {
        if lock_path.is_symlink() {bail!("invalid: symlinked model lock");}
        let model_lock=std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).mode(0o600).open(lock_path)?;
        model_lock.try_lock_exclusive().context("model resource is busy (Snake or another worker)")?;
        let mut command=Command::new(python);
        command.args(["-u", "-m", "laya_tell_me.worker"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).kill_on_drop(true);
        crate::runtime::inherit_model_lock(command.as_std_mut(),&model_lock);
        let mut child=command.spawn().context("start Laya Python worker")?;
        let input = child.stdin.take().context("worker stdin")?;
        let output = BufReader::new(child.stdout.take().context("worker stdout")?);
        Ok(Self { child, input, output, _model_lock:model_lock })
    }

    async fn call(&mut self, request: &Request) -> Result<Value> {
        let mut bytes = serde_json::to_vec(request)?;
        bytes.push(b'\n');
        self.input.write_all(&bytes).await?;
        self.input.flush().await?;
        let mut buffer = Vec::new();
        loop {
            let part = self.output.fill_buf().await?;
            if part.is_empty() { bail!("worker exited before replying"); }
            let count = part.iter().position(|b| *b == b'\n').map(|n| n + 1).unwrap_or(part.len());
            if buffer.len() + count > MAX_MESSAGE { bail!("worker reply too large"); }
            let done = part[count - 1] == b'\n';
            buffer.extend_from_slice(&part[..count]);
            self.output.consume(count);
            if done { break; }
        }
        let reply: Value = serde_json::from_slice(&buffer).context("invalid worker response")?;
        if reply["request_id"] != request.request_id || reply["protocol_version"] != 1 { bail!("worker response identity mismatch"); }
        if let Some(error) = reply.get("error") { bail!("worker: {}", error); }
        reply.get("result").cloned().context("worker response missing result")
    }

    async fn stop(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

impl Worker {
    pub fn start(python: String, idle: Duration, lock_path:PathBuf) -> Self {
        let (sender, mut receiver) = mpsc::channel::<Work>(32);
        let busy = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(AtomicU32::new(0));
        let worker = Self { sender, busy: busy.clone(), pid: pid.clone() };
        tokio::spawn(async move {
            let mut process: Option<Process> = None;
            loop {
                let next = if process.is_some() && !idle.is_zero() {
                    match tokio::time::timeout(idle, receiver.recv()).await {
                        Ok(next) => next,
                        Err(_) => {
                            if let Some(mut child) = process.take() { child.stop().await; }
                            pid.store(0, Ordering::Relaxed);
                            continue;
                        }
                    }
                } else { receiver.recv().await };
                let Some(mut work) = next else { break; };
                if work.reply.is_closed() { continue; }
                if work.request.method == "release" {
                    if let Some(mut child) = process.take() { child.stop().await; }
                    pid.store(0, Ordering::Relaxed);
                    let _ = work.reply.send(Ok(json!({"released":true})));
                    continue;
                }
                if tokio::time::Instant::now() >= work.deadline {
                    let _ = work.reply.send(Err(anyhow!("request expired in queue")));
                    continue;
                }
                busy.store(true, Ordering::Relaxed);
                if process.is_none() {
                    match Process::start(&python,&lock_path).await {
                        Ok(child) => { pid.store(child.child.id().unwrap_or(0), Ordering::Relaxed); process = Some(child); }
                        Err(error) => { let _ = work.reply.send(Err(error)); busy.store(false, Ordering::Relaxed); continue; }
                    }
                }
                let result = tokio::select! {
                    outcome=tokio::time::timeout_at(work.deadline, process.as_mut().unwrap().call(&work.request))=>match outcome {Ok(result)=>result,Err(_)=>Err(anyhow!("worker request timed out"))},
                    _=work.reply.closed()=>Err(anyhow!("worker request cancelled")),
                };
                // Failed/partial exchanges cannot safely be reused for the next request.
                if result.is_err() {
                    if let Some(mut child) = process.take() { child.stop().await; }
                    pid.store(0, Ordering::Relaxed);
                }
                busy.store(false, Ordering::Relaxed);
                let _ = work.reply.send(result);
            }
            if let Some(mut child) = process { child.stop().await; }
            pid.store(0, Ordering::Relaxed);
        });
        worker
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let request = Request::new(method, params);
        request.validate()?;
        let (reply, receiver) = oneshot::channel();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        self.sender.try_send(Work { request, reply, deadline }).map_err(|_| anyhow!("worker queue busy or stopped"))?;
        tokio::time::timeout_at(deadline + Duration::from_secs(1), receiver).await.context("worker reply timeout")?.context("worker stopped")?
    }

    pub fn status(&self) -> Value {
        json!({"busy":self.busy.load(Ordering::Relaxed), "pid":self.pid.load(Ordering::Relaxed), "queued":32-self.sender.capacity()})
    }

    pub fn busy(&self) -> bool { self.busy.load(Ordering::Relaxed) || self.sender.capacity() != 32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn deadline_timeout_stops_child_releases_lock_and_recovers() {
        let fixture=Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/workbench_worker.py").canonicalize().unwrap();
        let root=tempfile::tempdir().unwrap();
        let lock_path=root.path().join("model.lock");
        let worker=Worker::start(fixture.to_string_lossy().into_owned(),Duration::ZERO,lock_path.clone());
        let warm=worker.call("info",json!({})).await.unwrap();
        assert_eq!(warm["model"]["model_revision"],"fixture");
        let child_pid=worker.pid.load(Ordering::Relaxed);
        assert!(child_pid > 0);

        let request=Request::new("predict",json!({"state":"fixture:slow"}));
        let (reply,receiver)=oneshot::channel();
        worker.sender.send(Work { request, reply, deadline:tokio::time::Instant::now()+Duration::from_millis(250) }).await.unwrap();
        let error=tokio::time::timeout(Duration::from_secs(2),receiver).await.unwrap().unwrap().unwrap_err();
        assert_eq!(error.to_string(),"worker request timed out");
        assert_eq!(worker.status(),json!({"busy":false,"pid":0,"queued":0}));
        assert_eq!(unsafe { libc::kill(child_pid as i32,0) },-1);
        assert_eq!(std::io::Error::last_os_error().raw_os_error(),Some(libc::ESRCH));

        let lock=std::fs::OpenOptions::new().read(true).write(true).open(&lock_path).unwrap();
        lock.try_lock_exclusive().unwrap();
        FileExt::unlock(&lock).unwrap();

        let result=worker.call("info",json!({})).await.unwrap();
        assert_eq!(result["model"]["model_revision"],"fixture");
        assert!(worker.status()["pid"].as_u64().unwrap() > 0);
        assert_eq!(worker.call("release",json!({})).await.unwrap(),json!({"released":true}));
        assert_eq!(worker.status(),json!({"busy":false,"pid":0,"queued":0}));
    }
}
