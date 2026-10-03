use crate::protocol::{hash, MAX_MESSAGE};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{fs::{self, File, OpenOptions}, io::{ErrorKind, Read, Write}, os::unix::fs::{OpenOptionsExt, PermissionsExt}, path::{Path, PathBuf}};

pub(crate) const INLINE_THRESHOLD_BYTES: usize = 16 * 1024;
pub(crate) const MAX_PAYLOAD_BYTES: usize = MAX_MESSAGE;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Evidence {
    pub(crate) id: String,
    pub(crate) relative_path: String,
    pub(crate) sha256: String,
    pub(crate) size_bytes: u64,
}

pub(crate) fn write(root:&Path,value:&Value)->Result<Evidence> {
    let bytes=serde_json::to_vec(value)?;
    if bytes.len()>MAX_PAYLOAD_BYTES {bail!("invalid: evidence payload exceeds 1 MiB");}
    let directory=evidence_directory(root,true)?.ok_or_else(||anyhow!("unavailable: evidence directory was not created"))?;
    let id=uuid::Uuid::new_v4().hyphenated().to_string();
    let path=directory.join(format!("{id}.json"));
    durable_create(&directory,&path,&bytes)?;
    Ok(Evidence {id:id.clone(),relative_path:format!("evidence/{id}.json"),sha256:hash(value),size_bytes:bytes.len() as u64})
}

pub(crate) fn read(root:&Path,evidence:&Evidence)->Result<Value> {
    validate_id(&evidence.id)?;
    validate_digest(&evidence.sha256)?;
    let expected_relative=format!("evidence/{}.json",evidence.id);
    if evidence.relative_path!=expected_relative {bail!("invalid: evidence relative path does not match id");}
    if evidence.size_bytes>MAX_PAYLOAD_BYTES as u64 {bail!("invalid: evidence size exceeds 1 MiB");}
    let directory=evidence_directory(root,false)?.ok_or_else(||anyhow!("not_found: evidence directory"))?;
    let path=directory.join(format!("{}.json",evidence.id));
    let metadata=secure_file_metadata(&path)?;
    if metadata.len()>MAX_PAYLOAD_BYTES as u64 {bail!("invalid: evidence file exceeds 1 MiB");}
    if metadata.len()!=evidence.size_bytes {bail!("invalid: evidence size mismatch");}
    let file=OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW|libc::O_NONBLOCK).open(&path).with_context(||format!("read evidence file {}",path.display()))?;
    let opened=file.metadata()?;
    if !opened.file_type().is_file()||opened.len()!=metadata.len() {bail!("invalid: evidence file changed during read");}
    let mut bytes=Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_PAYLOAD_BYTES as u64+1).read_to_end(&mut bytes)?;
    if bytes.len() as u64!=metadata.len() {bail!("invalid: evidence file changed during read");}
    let digest=format!("{:x}",Sha256::digest(&bytes));
    if digest!=evidence.sha256 {bail!("invalid: evidence hash mismatch");}
    let value:Value=serde_json::from_slice(&bytes).context("invalid: evidence JSON")?;
    if hash(&value)!=evidence.sha256 {bail!("invalid: evidence is not canonical JSON");}
    Ok(value)
}

pub(crate) fn restore(root:&Path,evidence:&Evidence,value:&Value)->Result<()> {
    validate_evidence(evidence)?;
    let bytes=serde_json::to_vec(value)?;
    if bytes.len()>MAX_PAYLOAD_BYTES {bail!("invalid: evidence payload exceeds 1 MiB");}
    if bytes.len() as u64!=evidence.size_bytes||hash(value)!=evidence.sha256 {bail!("invalid: restored evidence metadata mismatch");}
    let directory=evidence_directory(root,true)?.ok_or_else(||anyhow!("unavailable: evidence directory was not created"))?;
    let path=directory.join(format!("{}.json",evidence.id));
    match fs::symlink_metadata(&path) {
        Ok(_)=>{read(root,evidence)?;Ok(())}
        Err(error) if error.kind()==ErrorKind::NotFound=>durable_create(&directory,&path,&bytes),
        Err(error)=>Err(error).with_context(||format!("inspect evidence file {}",path.display())),
    }
}

pub(crate) fn replace_verified(root:&Path,evidence:&Evidence,value:&Value)->Result<()> {
    validate_evidence(evidence)?;
    let bytes=serde_json::to_vec(value)?;
    if bytes.len()>MAX_PAYLOAD_BYTES {bail!("invalid: evidence payload exceeds 1 MiB");}
    if bytes.len() as u64!=evidence.size_bytes||hash(value)!=evidence.sha256 {bail!("invalid: replacement evidence metadata mismatch");}
    let directory=evidence_directory(root,true)?.ok_or_else(||anyhow!("unavailable: evidence directory was not created"))?;
    let target=directory.join(format!("{}.json",evidence.id));
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink()||!metadata.file_type().is_file()=>bail!("invalid: evidence replacement target is not a regular file"),
        Ok(_)=>{}
        Err(error) if error.kind()==ErrorKind::NotFound=>{}
        Err(error)=>return Err(error).with_context(||format!("inspect evidence file {}",target.display())),
    }
    let temporary=directory.join(format!(".replace-{}.tmp",uuid::Uuid::new_v4()));
    durable_create(&directory,&temporary,&bytes)?;
    if let Err(error)=fs::rename(&temporary,&target).with_context(||format!("replace evidence file {}",target.display())) {
        let _=fs::remove_file(&temporary);
        let _=open_directory(&directory).and_then(|directory|directory.sync_all().map_err(Into::into));
        return Err(error);
    }
    open_directory(&directory)?.sync_all().with_context(||format!("sync evidence directory {}",directory.display()))?;
    Ok(())
}

pub(crate) fn list(root:&Path)->Result<Vec<String>> {
    let Some(directory)=evidence_directory(root,false)? else{return Ok(Vec::new());};
    let mut ids=Vec::new();
    for entry in fs::read_dir(&directory).with_context(||format!("list evidence directory {}",directory.display()))? {
        let entry=entry?;
        let Some(name)=entry.file_name().to_str().map(str::to_string) else{continue;};
        let Some(id)=name.strip_suffix(".json") else{continue;};
        if validate_id(id).is_err(){continue;}
        let metadata=fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink()||!metadata.file_type().is_file(){bail!("invalid: managed evidence entry is not a regular file");}
        ids.push(id.to_string());
    }
    ids.sort();
    Ok(ids)
}

pub(crate) fn cleanup_temporary(root:&Path)->Result<()> {
    let Some(directory)=evidence_directory(root,false)? else{return Ok(());};
    let mut removed=false;
    for entry in fs::read_dir(&directory).with_context(||format!("list evidence directory {}",directory.display()))? {
        let entry=entry?;
        let Some(name)=entry.file_name().to_str().map(str::to_string) else{continue;};
        let Some(id)=name.strip_prefix(".replace-").and_then(|name|name.strip_suffix(".tmp")) else{continue;};
        if validate_id(id).is_err(){continue;}
        let metadata=fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink()||!metadata.file_type().is_file(){bail!("invalid: managed evidence temporary entry is not a regular file");}
        fs::remove_file(entry.path())?;
        removed=true;
    }
    if removed {open_directory(&directory)?.sync_all().with_context(||format!("sync evidence directory {}",directory.display()))?;}
    Ok(())
}

pub(crate) fn remove(root:&Path,id:&str)->Result<()> {
    validate_id(id)?;
    let Some(directory)=evidence_directory(root,false)? else{return Ok(());};
    let path=directory.join(format!("{id}.json"));
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind()==ErrorKind::NotFound=>return Ok(()),
        Err(error)=>return Err(error).with_context(||format!("inspect evidence file {}",path.display())),
        Ok(metadata) if metadata.file_type().is_symlink()||!metadata.file_type().is_file()=>bail!("invalid: evidence path is not a regular file"),
        Ok(_)=>{}
    }
    fs::remove_file(&path).with_context(||format!("remove evidence file {}",path.display()))?;
    open_directory(&directory)?.sync_all().with_context(||format!("sync evidence directory {}",directory.display()))?;
    Ok(())
}

fn evidence_directory(root:&Path,create:bool)->Result<Option<PathBuf>> {
    let root_metadata=fs::symlink_metadata(root).with_context(||format!("inspect evidence root {}",root.display()))?;
    if root_metadata.file_type().is_symlink()||!root_metadata.file_type().is_dir(){bail!("invalid: evidence root is not a regular directory");}
    let directory=root.join("evidence");
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.file_type().is_symlink()||!metadata.file_type().is_dir()=>bail!("invalid: evidence directory is not a regular directory"),
        Ok(_)=>{}
        Err(error) if error.kind()==ErrorKind::NotFound&&create=>{
            fs::create_dir(&directory).with_context(||format!("create evidence directory {}",directory.display()))?;
            fs::set_permissions(&directory,fs::Permissions::from_mode(0o700)).with_context(||format!("set evidence directory permissions {}",directory.display()))?;
            open_directory(root)?.sync_all().with_context(||format!("sync evidence root {}",root.display()))?;
        }
        Err(error) if error.kind()==ErrorKind::NotFound=>return Ok(None),
        Err(error)=>return Err(error).with_context(||format!("inspect evidence directory {}",directory.display())),
    }
    let opened=open_directory(&directory)?;
    opened.set_permissions(fs::Permissions::from_mode(0o700)).with_context(||format!("set evidence directory permissions {}",directory.display()))?;
    Ok(Some(directory))
}

fn durable_create(directory:&Path,path:&Path,bytes:&[u8])->Result<()> {
    let mut created=false;
    let result=(||->Result<()> {
        let mut file=OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(path).with_context(||format!("create evidence file {}",path.display()))?;
        created=true;
        file.set_permissions(fs::Permissions::from_mode(0o600)).with_context(||format!("set evidence permissions {}",path.display()))?;
        #[cfg(test)] crate::test_fault("evidence.before_write")?;
        file.write_all(bytes).with_context(||format!("write evidence file {}",path.display()))?;
        #[cfg(test)] crate::test_fault("evidence.before_sync")?;
        file.sync_all().with_context(||format!("sync evidence file {}",path.display()))?;
        open_directory(directory)?.sync_all().with_context(||format!("sync evidence directory {}",directory.display()))?;
        #[cfg(test)] crate::test_fault("evidence.after_sync")?;
        Ok(())
    })();
    if let Err(error)=result {
        if created {
            let _=fs::remove_file(path);
            let _=open_directory(directory).and_then(|directory|directory.sync_all().map_err(Into::into));
        }
        return Err(error);
    }
    Ok(())
}

fn open_directory(path:&Path)->Result<File> {
    OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW|libc::O_DIRECTORY).open(path).with_context(||format!("open managed directory {}",path.display()))
}

fn secure_file_metadata(path:&Path)->Result<fs::Metadata> {
    let metadata=fs::symlink_metadata(path).with_context(||format!("inspect evidence file {}",path.display()))?;
    if metadata.file_type().is_symlink()||!metadata.file_type().is_file(){bail!("invalid: evidence path is not a regular file");}
    Ok(metadata)
}

fn validate_id(id:&str)->Result<()> {
    let parsed=uuid::Uuid::parse_str(id).map_err(|_|anyhow!("invalid: evidence id must be a UUIDv4"))?;
    if parsed.get_version_num()!=4||parsed.hyphenated().to_string()!=id {bail!("invalid: evidence id must be a canonical UUIDv4");}
    Ok(())
}

fn validate_digest(digest:&str)->Result<()> {
    if digest.len()!=64||!digest.bytes().all(|byte|byte.is_ascii_digit()||matches!(byte,b'a'..=b'f')) {bail!("invalid: evidence sha256");}
    Ok(())
}

fn validate_evidence(evidence:&Evidence)->Result<()> {
    validate_id(&evidence.id)?;
    validate_digest(&evidence.sha256)?;
    if evidence.relative_path!=format!("evidence/{}.json",evidence.id){bail!("invalid: evidence relative path does not match id");}
    if evidence.size_bytes>MAX_PAYLOAD_BYTES as u64{bail!("invalid: evidence size exceeds 1 MiB");}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::{symlink,PermissionsExt};

    #[test]
    fn round_trip_is_private_durable_and_idempotently_removed() {
        let root=tempfile::tempdir().unwrap();
        let value=json!({"state":"review evidence","items":[1,2,3]});
        let stored=write(root.path(),&value).unwrap();
        assert_eq!(stored.sha256,hash(&value));
        assert_eq!(stored.relative_path,format!("evidence/{}.json",stored.id));
        assert_eq!(fs::metadata(root.path().join("evidence")).unwrap().permissions().mode()&0o777,0o700);
        assert_eq!(fs::metadata(root.path().join(&stored.relative_path)).unwrap().permissions().mode()&0o777,0o600);
        assert_eq!(read(root.path(),&stored).unwrap(),value);
        remove(root.path(),&stored.id).unwrap();
        remove(root.path(),&stored.id).unwrap();
    }

    #[test]
    fn read_rejects_tampering_and_size_mismatch() {
        let root=tempfile::tempdir().unwrap();
        let value=json!({"state":"original"});
        let stored=write(root.path(),&value).unwrap();
        let mut wrong_size=stored.clone();
        wrong_size.size_bytes+=1;
        assert!(read(root.path(),&wrong_size).unwrap_err().to_string().starts_with("invalid:"));
        let path=root.path().join(&stored.relative_path);
        let bytes=fs::read(&path).unwrap();
        let mut tampered=bytes.clone();
        let index=tampered.iter().position(|byte|*byte==b'o').unwrap();
        tampered[index]=b'x';
        fs::write(&path,tampered).unwrap();
        assert!(read(root.path(),&stored).unwrap_err().to_string().starts_with("invalid:"));
    }

    #[test]
    fn rejects_oversize_traversal_and_symlinks() {
        let root=tempfile::tempdir().unwrap();
        assert!(write(root.path(),&Value::String("x".repeat(MAX_PAYLOAD_BYTES))).unwrap_err().to_string().starts_with("invalid:"));
        assert!(remove(root.path(),"../../outside").unwrap_err().to_string().starts_with("invalid:"));
        let outside=tempfile::tempdir().unwrap();
        symlink(outside.path(),root.path().join("evidence")).unwrap();
        assert!(write(root.path(),&json!({"safe":true})).unwrap_err().to_string().starts_with("invalid:"));

        let safe_root=tempfile::tempdir().unwrap();
        let stored=write(safe_root.path(),&json!({"safe":true})).unwrap();
        let path=safe_root.path().join(&stored.relative_path);
        fs::remove_file(&path).unwrap();
        let outside_file=outside.path().join("outside.json");
        fs::write(&outside_file,b"{}").unwrap();
        symlink(&outside_file,&path).unwrap();
        assert!(read(safe_root.path(),&stored).unwrap_err().to_string().starts_with("invalid:"));
        assert!(remove(safe_root.path(),&stored.id).unwrap_err().to_string().starts_with("invalid:"));
    }

    #[test]
    fn rejects_fifo_evidence_without_blocking() {
        use std::ffi::CString;
        let root=tempfile::tempdir().unwrap();
        let stored=write(root.path(),&json!({"safe":true})).unwrap();
        let path=root.path().join(&stored.relative_path);
        fs::remove_file(&path).unwrap();
        let encoded=CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe{libc::mkfifo(encoded.as_ptr(),0o600)},0);
        assert!(read(root.path(),&stored).unwrap_err().to_string().starts_with("invalid:"));
        assert!(remove(root.path(),&stored.id).unwrap_err().to_string().starts_with("invalid:"));
    }

    #[test]
    fn restore_preserves_catalog_identity_without_overwrite() {
        let source=tempfile::tempdir().unwrap();
        let target=tempfile::tempdir().unwrap();
        let value=json!({"snapshot":"backup"});
        let stored=write(source.path(),&value).unwrap();
        restore(target.path(),&stored,&value).unwrap();
        restore(target.path(),&stored,&value).unwrap();
        assert_eq!(read(target.path(),&stored).unwrap(),value);
        fs::write(target.path().join(&stored.relative_path),b"{\"snapshot\":\"broken\"}").unwrap();
        assert!(restore(target.path(),&stored,&value).unwrap_err().to_string().starts_with("invalid:"));
        assert_eq!(fs::read(target.path().join(&stored.relative_path)).unwrap(),b"{\"snapshot\":\"broken\"}");
        let mut wrong=stored.clone();wrong.sha256="0".repeat(64);
        assert!(restore(target.path(),&wrong,&value).unwrap_err().to_string().starts_with("invalid:"));
    }

    #[test]
    fn verified_replace_repairs_regular_file_and_rejects_symlink() {
        let root=tempfile::tempdir().unwrap();
        let value=json!({"snapshot":"verified"});
        let stored=write(root.path(),&value).unwrap();
        let path=root.path().join(&stored.relative_path);
        fs::write(&path,b"corrupt").unwrap();
        replace_verified(root.path(),&stored,&value).unwrap();
        assert_eq!(read(root.path(),&stored).unwrap(),value);
        fs::remove_file(&path).unwrap();
        let outside=tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(),&path).unwrap();
        assert!(replace_verified(root.path(),&stored,&value).unwrap_err().to_string().starts_with("invalid:"));
        assert!(path.is_symlink());
    }

    #[test]
    fn list_returns_only_canonical_managed_files() {
        let root=tempfile::tempdir().unwrap();
        let second=write(root.path(),&json!({"value":2})).unwrap();
        let first=write(root.path(),&json!({"value":1})).unwrap();
        fs::write(root.path().join("evidence/readme.txt"),b"ignored").unwrap();
        fs::write(root.path().join("evidence/not-a-uuid.json"),b"ignored").unwrap();
        let mut expected=vec![first.id.clone(),second.id.clone()];expected.sort();
        assert_eq!(list(root.path()).unwrap(),expected);
        let canonical=uuid::Uuid::new_v4().hyphenated().to_string();
        symlink(root.path().join("evidence/readme.txt"),root.path().join(format!("evidence/{canonical}.json"))).unwrap();
        assert!(list(root.path()).unwrap_err().to_string().starts_with("invalid:"));
    }

    #[test]
    fn temporary_cleanup_is_scoped_and_rejects_symlink() {
        let root=tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("evidence")).unwrap();
        let managed=root.path().join(format!("evidence/.replace-{}.tmp",uuid::Uuid::new_v4()));
        let unrelated=root.path().join("evidence/.replace-not-a-uuid.tmp");
        fs::write(&managed,b"partial").unwrap();
        fs::write(&unrelated,b"keep").unwrap();
        cleanup_temporary(root.path()).unwrap();
        assert!(!managed.exists());
        assert!(unrelated.exists());
        let outside=tempfile::NamedTempFile::new().unwrap();
        let linked=root.path().join(format!("evidence/.replace-{}.tmp",uuid::Uuid::new_v4()));
        symlink(outside.path(),&linked).unwrap();
        assert!(cleanup_temporary(root.path()).unwrap_err().to_string().starts_with("invalid:"));
        assert!(linked.is_symlink());
    }
}
