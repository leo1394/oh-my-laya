use crate::store::Store;
use rusqlite::Connection;
use serde_json::json;
use sha2::{Digest,Sha256};
use std::{fs,path::{Path,PathBuf},process::{Command,Output}};
use std::os::unix::process::ExitStatusExt;

fn large()->String {"fault-evidence-".repeat(20*1024/15+1)}

fn run_child(root:&Path,point:&str,scenario:&str)->Output {
    let marker=root.join("reached");
    let _=fs::remove_file(&marker);
    let mut command=Command::new(std::env::current_exe().unwrap());
    command.args(["--ignored","--exact","fault_tests::child","--nocapture"])
        .env("LAYA_TEST_FAULT_ROOT",root).env("LAYA_TEST_FAULT_POINT",point).env("LAYA_TEST_SCENARIO",scenario).env("LAYA_TEST_FAULT_MODE","kill").env("RUST_BACKTRACE","0");
    if scenario=="io_failure" {command.env("LAYA_TEST_FAULT_MODE",if point=="evidence.before_write"{"enospc"}else{"eio"});}
    command.output().unwrap()
}

fn assert_reached(root:&Path,output:&Output) {
    assert!(root.join("reached").is_file(),"fault hook was not reached; status={:?}\nstdout={}\nstderr={}",output.status,String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr));
}

fn assert_storage_consistent(root:&Path) {
    let connection=Connection::open(root.join("laya.sqlite3")).unwrap();
    let missing:i64=connection.query_row("SELECT COUNT(*) FROM decision_snapshots s LEFT JOIN artifacts a ON a.id=s.artifact_id WHERE s.artifact_id IS NOT NULL AND a.id IS NULL",[],|row|row.get(0)).unwrap();
    let unreferenced:i64=connection.query_row("SELECT COUNT(*) FROM artifacts a WHERE NOT EXISTS(SELECT 1 FROM decision_snapshots s WHERE s.artifact_id=a.id)",[],|row|row.get(0)).unwrap();
    assert_eq!(missing,0);
    assert_eq!(unreferenced,0);
    let artifacts={
        let mut statement=connection.prepare("SELECT id,relative_path,sha256,size_bytes FROM artifacts ORDER BY id").unwrap();
        statement.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,u64>(3)?))).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap()
    };
    for (id,relative,digest,size) in &artifacts {
        assert_eq!(relative,&format!("evidence/{id}.json"));
        let bytes=fs::read(root.join(relative)).unwrap();
        assert_eq!(bytes.len() as u64,*size);
        assert_eq!(format!("{:x}",Sha256::digest(&bytes)),*digest);
        let mismatch:i64=connection.query_row("SELECT COUNT(*) FROM decision_snapshots WHERE artifact_id=?1 AND payload_hash<>?2",rusqlite::params![id,digest],|row|row.get(0)).unwrap();
        assert_eq!(mismatch,0);
    }
    let managed_files=fs::read_dir(root.join("evidence")).map(|entries|entries.filter_map(Result::ok).filter(|entry|entry.file_name().to_str().is_some_and(|name|name.ends_with(".json"))).count()).unwrap_or(0);
    assert_eq!(managed_files,artifacts.len());
    assert!(!fs::read_dir(root).unwrap().filter_map(Result::ok).any(|entry|entry.file_name().to_str().is_some_and(|name|name.starts_with(".restore-"))));
    assert!(!fs::read_dir(root.join("evidence")).map(|entries|entries.filter_map(Result::ok).any(|entry|entry.file_name().to_str().is_some_and(|name|name.starts_with(".replace-")&&name.ends_with(".tmp")))).unwrap_or(false));
}

fn restore_stage(root:&Path)->PathBuf {
    let stages=fs::read_dir(root).unwrap().filter_map(Result::ok).filter(|entry|entry.file_name().to_str().is_some_and(|name|name.starts_with(".restore-"))).map(|entry|entry.path()).collect::<Vec<_>>();
    assert_eq!(stages.len(),1);
    stages.into_iter().next().unwrap()
}

fn assert_staged_privacy(root:&Path) {
    let stage=restore_stage(root);
    let connection=Connection::open(stage.join("restore.sqlite3")).unwrap();
    let (deleted,request):(bool,String)=connection.query_row("SELECT deleted_at IS NOT NULL,request_json FROM decisions WHERE id='deleted'",[],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
    assert!(deleted);
    assert_eq!(request,"null");
    assert_eq!(connection.query_row::<i64,_,_>("SELECT COUNT(*) FROM decisions WHERE id='stable'",[],|row|row.get(0)).unwrap(),1);
    assert_eq!(connection.query_row::<i64,_,_>("SELECT COUNT(*) FROM decisions WHERE id='live-only'",[],|row|row.get(0)).unwrap(),0);
}

async fn prepare_root(root:&Path) {
    let store=Store::open(root).unwrap();
    store.call("settings/update",json!({"recording_enabled":true})).await.unwrap();
}

async fn prepare_restore(root:&Path) {
    let store=Store::open(root).unwrap();
    store.call("settings/update",json!({"recording_enabled":true})).await.unwrap();
    store.call("decisions/begin",json!({"request_id":"stable","id":"stable","request":{"state":large()}})).await.unwrap();
    store.call("decisions/begin",json!({"request_id":"deleted","id":"deleted","request":{"state":"private old state"}})).await.unwrap();
    store.call("backup/create",json!({"id":"known"})).await.unwrap();
    store.call("decisions/delete",json!({"id":"deleted"})).await.unwrap();
    store.call("decisions/begin",json!({"request_id":"live-only","id":"live-only","request":{"state":"new state"}})).await.unwrap();
}

fn assert_deleted(runtime:&tokio::runtime::Runtime,store:&Store,id:&str) {
    assert!(runtime.block_on(store.call("decisions/get",json!({"id":id}))).unwrap_err().to_string().starts_with("not_found:"));
    assert_eq!(runtime.block_on(store.call("decisions/is_deleted",json!({"id":id}))).unwrap()["deleted"],true);
}

#[test]
fn kill_after_evidence_sync_leaves_no_reference_or_orphan() {
    let root=tempfile::tempdir().unwrap();
    let runtime=tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(prepare_root(root.path()));
    let output=run_child(root.path(),"evidence.after_sync","begin");
    assert_reached(root.path(),&output);
    assert_eq!(output.status.signal(),Some(libc::SIGKILL));
    assert_eq!(Connection::open(root.path().join("laya.sqlite3")).unwrap().query_row::<i64,_,_>("SELECT COUNT(*) FROM decisions WHERE id='fault'",[],|row|row.get(0)).unwrap(),0);
    assert_eq!(fs::read_dir(root.path().join("evidence")).unwrap().filter_map(Result::ok).filter(|entry|entry.file_name().to_str().is_some_and(|name|name.ends_with(".json"))).count(),1);
    let store=Store::open(root.path()).unwrap();
    assert!(runtime.block_on(store.call("decisions/get",json!({"id":"fault"}))).is_err());
    assert_storage_consistent(root.path());
}

#[test]
fn kill_after_snapshot_commit_recovers_committed_reference() {
    let root=tempfile::tempdir().unwrap();
    let runtime=tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(prepare_root(root.path()));
    let output=run_child(root.path(),"snapshot.after_commit","begin");
    assert_reached(root.path(),&output);
    assert_eq!(output.status.signal(),Some(libc::SIGKILL));
    let connection=Connection::open(root.path().join("laya.sqlite3")).unwrap();
    assert_eq!(connection.query_row::<i64,_,_>("SELECT COUNT(*) FROM decisions WHERE id='fault'",[],|row|row.get(0)).unwrap(),1);
    assert_eq!(connection.query_row::<i64,_,_>("SELECT COUNT(*) FROM decision_snapshots WHERE decision_id='fault' AND artifact_id IS NOT NULL",[],|row|row.get(0)).unwrap(),1);
    drop(connection);
    let store=Store::open(root.path()).unwrap();
    let detail=runtime.block_on(store.call("decisions/get",json!({"id":"fault"}))).unwrap();
    assert_eq!(detail["request"]["state"],large());
    assert_storage_consistent(root.path());
}

#[test]
fn kill_before_restore_install_keeps_live_state_and_cleans_stage() {
    let root=tempfile::tempdir().unwrap();
    let runtime=tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(prepare_restore(root.path()));
    let output=run_child(root.path(),"restore.before_install","restore");
    assert_reached(root.path(),&output);
    assert_eq!(output.status.signal(),Some(libc::SIGKILL));
    assert_staged_privacy(root.path());
    let live=Connection::open(root.path().join("laya.sqlite3")).unwrap();
    assert_eq!(live.query_row::<i64,_,_>("SELECT COUNT(*) FROM decisions WHERE id='live-only' AND deleted_at IS NULL",[],|row|row.get(0)).unwrap(),1);
    drop(live);
    let store=Store::open(root.path()).unwrap();
    runtime.block_on(store.call("decisions/get",json!({"id":"stable"}))).unwrap();
    runtime.block_on(store.call("decisions/get",json!({"id":"live-only"}))).unwrap();
    assert_deleted(&runtime,&store,"deleted");
    assert_storage_consistent(root.path());
}

#[test]
fn kill_after_restore_install_recovers_backup_state_without_resurrection() {
    let root=tempfile::tempdir().unwrap();
    let runtime=tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(prepare_restore(root.path()));
    let output=run_child(root.path(),"restore.after_install","restore");
    assert_reached(root.path(),&output);
    assert_eq!(output.status.signal(),Some(libc::SIGKILL));
    assert_staged_privacy(root.path());
    let installed=Connection::open(root.path().join("laya.sqlite3")).unwrap();
    assert_eq!(installed.query_row::<i64,_,_>("SELECT COUNT(*) FROM decisions WHERE id='live-only'",[],|row|row.get(0)).unwrap(),0);
    assert_eq!(installed.query_row::<i64,_,_>("SELECT COUNT(*) FROM decisions WHERE id='deleted' AND deleted_at IS NOT NULL",[],|row|row.get(0)).unwrap(),1);
    drop(installed);
    let store=Store::open(root.path()).unwrap();
    runtime.block_on(store.call("decisions/get",json!({"id":"stable"}))).unwrap();
    assert!(runtime.block_on(store.call("decisions/get",json!({"id":"live-only"}))).is_err());
    assert_deleted(&runtime,&store,"deleted");
    assert_storage_consistent(root.path());
}

#[test]
fn injected_evidence_io_errors_roll_back_and_retry_cleanly() {
    for point in ["evidence.before_write","evidence.before_sync"] {
        let root=tempfile::tempdir().unwrap();
        let runtime=tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(prepare_root(root.path()));
        let output=run_child(root.path(),point,"io_failure");
        assert_reached(root.path(),&output);
        assert!(output.status.success(),"child failed; stdout={}\nstderr={}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr));
        let store=Store::open(root.path()).unwrap();
        let detail=runtime.block_on(store.call("decisions/get",json!({"id":"fault"}))).unwrap();
        assert_eq!(detail["request"]["state"],large());
        assert_storage_consistent(root.path());
    }
}

#[test]
#[ignore]
fn child() {
    let root=PathBuf::from(std::env::var_os("LAYA_TEST_FAULT_ROOT").expect("fault root"));
    let scenario=std::env::var("LAYA_TEST_SCENARIO").expect("fault scenario");
    let runtime=tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let store=Store::open(&root).unwrap();
        match scenario.as_str() {
            "begin"=>{let _=store.call("decisions/begin",json!({"request_id":"fault","id":"fault","request":{"state":large()}})).await;}
            "restore"=>{let _=store.call("backup/restore",json!({"id":"known"})).await;}
            "io_failure"=>{
                let request=json!({"request_id":"fault","id":"fault","request":{"state":large()}});
                let error=store.call("decisions/begin",request.clone()).await.unwrap_err();
                let expected=if std::env::var("LAYA_TEST_FAULT_MODE").as_deref()==Ok("enospc"){libc::ENOSPC}else{libc::EIO};
                let actual=error.chain().find_map(|cause|cause.downcast_ref::<std::io::Error>().and_then(std::io::Error::raw_os_error));
                assert_eq!(actual,Some(expected));
                let connection=Connection::open(root.join("laya.sqlite3")).unwrap();
                assert_eq!(connection.query_row::<i64,_,_>("SELECT COUNT(*) FROM decisions WHERE id='fault'",[],|row|row.get(0)).unwrap(),0);
                assert_eq!(connection.query_row::<i64,_,_>("SELECT COUNT(*) FROM artifacts",[],|row|row.get(0)).unwrap(),0);
                drop(connection);
                std::env::remove_var("LAYA_TEST_FAULT_POINT");
                std::env::remove_var("LAYA_TEST_FAULT_MODE");
                assert_eq!(store.call("decisions/begin",request).await.unwrap()["existing"],false);
            }
            other=>panic!("unknown fault scenario {other}"),
        }
    });
}
