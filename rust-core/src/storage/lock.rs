use std::fs::{self, OpenOptions};
use std::path::PathBuf;

/// 文件锁（基于 fs2 crate 的 flock 封装）
/// Drop 时自动释放锁
pub struct FileLock {
    file: std::fs::File,
    #[allow(dead_code)]
    path: PathBuf,
}

impl FileLock {
    /// 获取排他锁（用于写入操作：init、uninit）
    pub fn exclusive(lock_path: PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)?;

        fs2::FileExt::lock_exclusive(&file)?;

        Ok(Self {
            file,
            path: lock_path,
        })
    }

    /// 获取共享锁（用于读取操作：search、callers、callees、status）
    pub fn shared(lock_path: PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        if !lock_path.exists() {
            return Err(format!("Lock file does not exist: {:?}", lock_path).into());
        }
        let file = OpenOptions::new()
            .read(true)
            .open(&lock_path)?;

        fs2::FileExt::lock_shared(&file)?;

        Ok(Self {
            file,
            path: lock_path,
        })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        fs2::FileExt::unlock(&self.file).ok();
    }
}
