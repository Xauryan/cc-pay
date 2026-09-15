//! Atomic automatic-payment claims. A claim is intentionally never released.
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Shared across payment methods. `claim` must atomically return true exactly
/// once for an order, and persist the claim before returning. Errors stop payment.
/// Implement this trait with a database unique key for multiple application hosts.
pub trait AttemptStore: Send + Sync {
    fn contains(&self, order_id: &str) -> Result<bool>;
    fn claim(&self, order_id: &str) -> Result<bool>;
}

/// In-memory protection for custom integrations and tests. Not restart-persistent.
#[derive(Default)]
pub struct MemoryAttemptStore(Mutex<HashSet<String>>);
impl AttemptStore for MemoryAttemptStore {
    fn contains(&self, order_id: &str) -> Result<bool> {
        Ok(self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("付款防重锁不可用"))?
            .contains(order_id))
    }
    fn claim(&self, order_id: &str) -> Result<bool> {
        Ok(self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("付款防重锁不可用"))?
            .insert(order_id.to_owned()))
    }
}

/// Local durable claims using atomic create-new files. All processes must use
/// the same directory on a local filesystem. Files contain no payment secrets.
#[derive(Clone)]
pub struct FileAttemptStore {
    directory: PathBuf,
}
impl FileAttemptStore {
    pub fn new(directory: impl AsRef<Path>) -> Result<Self> {
        fs::create_dir_all(directory.as_ref()).context("无法创建付款防重目录")?;
        Ok(Self {
            directory: directory
                .as_ref()
                .canonicalize()
                .context("付款防重目录无效")?,
        })
    }
    fn path(&self, order_id: &str) -> PathBuf {
        self.directory
            .join(format!("{:x}.claim", Sha256::digest(order_id.as_bytes())))
    }
}
impl AttemptStore for FileAttemptStore {
    fn contains(&self, order_id: &str) -> Result<bool> {
        match fs::symlink_metadata(self.path(order_id)) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                anyhow::ensure!(self.directory.is_dir(), "付款防重目录不可用，停止付款请求");
                Ok(false)
            }
            Err(error) => Err(error).context("无法读取付款防重记录"),
        }
    }
    fn claim(&self, order_id: &str) -> Result<bool> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(self.path(order_id)) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => return Ok(false),
            Err(error) => return Err(error).context("无法保存付款防重记录，未提交扣款"),
        };
        // Leave even an incomplete marker in place after an I/O failure.
        file.write_all(b"cc-pay automatic payment claimed\n")
            .context("付款防重记录写入失败")?;
        file.sync_all().context("付款防重记录落盘失败")?;
        #[cfg(unix)]
        fs::File::open(&self.directory)?.sync_all()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn files_survive_reopening_and_only_one_concurrent_claim_wins() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileAttemptStore::new(dir.path()).unwrap();
        let results = std::thread::scope(|scope| {
            (0..12)
                .map(|_| scope.spawn(|| store.claim("../../order?secret=1").unwrap()))
                .collect::<Vec<_>>()
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(results.into_iter().filter(|claimed| *claimed).count(), 1);
        let reopened = FileAttemptStore::new(dir.path()).unwrap();
        assert!(reopened.contains("../../order?secret=1").unwrap());
        assert!(!reopened.claim("../../order?secret=1").unwrap());
        assert!(reopened.claim("other-order").unwrap());
        for file in fs::read_dir(dir.path()).unwrap() {
            let file = file.unwrap();
            assert_eq!(file.file_name().to_string_lossy().len(), 70);
            assert_eq!(
                fs::read(file.path()).unwrap(),
                b"cc-pay automatic payment claimed\n"
            );
        }
    }
    #[test]
    fn storage_failure_never_grants_permission_to_pay() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileAttemptStore::new(dir.path().join("claims")).unwrap();
        fs::remove_dir(&store.directory).unwrap();
        fs::write(&store.directory, b"not a directory").unwrap();
        assert!(store.claim("order").is_err());
        assert!(store.contains("order").is_err());
    }
}
