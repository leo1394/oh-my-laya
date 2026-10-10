pub mod protocol;
pub(crate) mod evidence;
pub mod worker;
pub mod outbox;
pub mod runtime;
pub mod mcp;
pub mod store;
pub(crate) mod activity;
pub mod service;
pub mod assets;
pub mod evaluation;
pub mod scenario;
pub(crate) mod execution;
pub(crate) mod usage;
pub(crate) mod efficiency;

#[cfg(test)]
mod fault_tests;

// Compiled only into the unit-test executable, never the shipped service.
#[cfg(test)]
pub(crate) fn test_fault(point:&str)->anyhow::Result<()> {
    if std::env::var("LAYA_TEST_FAULT_POINT").ok().as_deref()!=Some(point) {return Ok(());}
    let root=std::path::PathBuf::from(std::env::var("LAYA_TEST_FAULT_ROOT")?);
    let marker=root.join("reached");
    std::fs::write(&marker,point)?;
    std::fs::File::open(marker)?.sync_all()?;
    std::fs::File::open(&root)?.sync_all()?;
    match std::env::var("LAYA_TEST_FAULT_MODE").as_deref() {
        Ok("enospc")=>Err(std::io::Error::from_raw_os_error(libc::ENOSPC).into()),
        Ok("eio")=>Err(std::io::Error::from_raw_os_error(libc::EIO).into()),
        _=>{
            if unsafe {libc::kill(libc::getpid(),libc::SIGKILL)}!=0 {return Err(std::io::Error::last_os_error().into());}
            // Do not unwind and clean the faulted file before the signal arrives.
            loop {std::thread::park();}
        }
    }
}
