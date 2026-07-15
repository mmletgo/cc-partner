//! storage/maintenance_gate.rs — 全局 SQLite 写事务维护屏障（进程内 + 跨进程）
//!
//! Business Logic（为什么需要这个模块）:
//!     恢复备份时必须独占数据库，防止本地/LAN/后台 writer 在 pre-restore 备份与
//!     replace/merge commit 之间静默覆盖数据。普通写路径用 shared lease 并发，
//!     restore 用 exclusive lease。GUI 与 sidecar 分属不同进程时，仅进程内 RwLock
//!     无法互斥——必须配合 data_dir 上的 OS 文件锁。
//!
//! Code Logic（这个模块做什么）:
//!     `DatabaseMaintenanceGate` 内部 `tokio::sync::RwLock` + 可选 `db-maintenance.lock`
//!     （fs4 exclusive/shared）；`DatabaseWritePermit` 贯穿 commit/rollback。

use crate::error::AppError;
use sqlx::sqlite::SqlitePool;
use sqlx::{Sqlite, Transaction};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

/// 全局 DB maintenance 读写屏障。
///
/// Business Logic: AppState 持有一份，所有生产 SQLite writer 共享。
/// Code Logic: 进程内 Arc<RwLock> + 可选跨进程锁路径。
#[derive(Clone, Debug)]
pub struct DatabaseMaintenanceGate {
    inner: Arc<RwLock<()>>,
    /// data_dir 下 `db-maintenance.lock`；None 时仅进程内互斥（测试 fixture）。
    cross_process_lock_path: Option<PathBuf>,
}

/// Shared 写租约：普通命令/调度/LAN 写路径持有。
///
/// Business Logic: 多 writer 可并存；restore exclusive 期间全部阻塞（含跨进程）。
/// Code Logic: OwnedRwLockReadGuard + 可选 OS shared lock。
#[derive(Debug)]
pub struct SharedWriteLease {
    _guard: OwnedRwLockReadGuard<()>,
    /// 跨进程 shared 锁文件（Drop 时 fd 关闭并 unlock）。
    _os_file: Option<File>,
}

/// Exclusive 维护租约：restore 从 pre-backup 到索引重建全程持有。
///
/// Business Logic: 独占期禁止任何 ordinary shared writer（含另一进程 GUI）。
/// Code Logic: OwnedRwLockWriteGuard + 可选 OS exclusive lock。
#[derive(Debug)]
pub struct ExclusiveMaintenanceLease {
    _guard: OwnedRwLockWriteGuard<()>,
    _os_file: Option<File>,
}

/// 写事务许可：必须覆盖 begin→commit/rollback 全生命周期。
///
/// Business Logic: begin_write_with_permit 接受任一变体；exclusive 不请求 nested shared。
/// Code Logic: Shared 内嵌 lease；MaintenanceExclusive 为标记（lease 由调用栈持有）。
#[derive(Debug)]
pub enum DatabaseWritePermit {
    /// 普通 writer 的 shared lease。
    Shared(SharedWriteLease),
    /// restore 已持有 exclusive 时转换的 maintenance permit。
    MaintenanceExclusive,
}

impl DatabaseMaintenanceGate {
    /// 构造仅进程内 gate（测试 / 无 data_dir 场景）。
    ///
    /// Business Logic: fixture 不需要跨进程锁。
    /// Code Logic: cross_process_lock_path=None。
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(())),
            cross_process_lock_path: None,
        }
    }

    /// 构造带跨进程锁文件的 gate。
    ///
    /// Business Logic: 生产 AppState 必须让 GUI 与 sidecar 对 restore exclusive 互斥。
    /// Code Logic: 记录 `data_dir/db-maintenance.lock` 路径；目录不存在时在取锁时创建。
    pub fn with_cross_process_lock(lock_path: PathBuf) -> Self {
        Self {
            inner: Arc::new(RwLock::new(())),
            cross_process_lock_path: Some(lock_path),
        }
    }

    /// 获取 ordinary writer 的 shared lease。
    ///
    /// Business Logic: 普通写路径在 begin 前调用；restore exclusive（本进程或 sidecar）时挂起/失败。
    /// Code Logic: 进程内 `read_owned` + OS `try_lock_shared`（跨进程时）。
    pub async fn acquire_shared(&self) -> SharedWriteLease {
        let guard = self.inner.clone().read_owned().await;
        let os_file = self.acquire_os_shared_blocking().await;
        SharedWriteLease {
            _guard: guard,
            _os_file: os_file,
        }
    }

    /// 尝试非阻塞获取 shared lease。
    ///
    /// Business Logic: 测试可断言 restore 期间 ordinary writer 被挡。
    /// Code Logic: `try_read_owned` + OS try_lock_shared。
    pub fn try_acquire_shared(&self) -> Option<SharedWriteLease> {
        let guard = self.inner.clone().try_read_owned().ok()?;
        let os_file = match self.try_os_shared() {
            Ok(f) => f,
            Err(()) => return None,
        };
        Some(SharedWriteLease {
            _guard: guard,
            _os_file: os_file,
        })
    }

    /// 获取 restore 独占 lease。
    ///
    /// Business Logic: pre-restore 备份前取得，索引重建后释放；跨进程独占。
    /// Code Logic: `write_owned` + OS exclusive lock（阻塞轮询有界）。
    pub async fn acquire_exclusive(&self) -> ExclusiveMaintenanceLease {
        let guard = self.inner.clone().write_owned().await;
        let os_file = self.acquire_os_exclusive_blocking().await;
        ExclusiveMaintenanceLease {
            _guard: guard,
            _os_file: os_file,
        }
    }

    /// 尝试非阻塞获取 exclusive。
    ///
    /// Business Logic: 测试与诊断。
    /// Code Logic: `try_write_owned` + OS try_lock exclusive。
    pub fn try_acquire_exclusive(&self) -> Option<ExclusiveMaintenanceLease> {
        let guard = self.inner.clone().try_write_owned().ok()?;
        let os_file = match self.try_os_exclusive() {
            Ok(f) => f,
            Err(()) => return None,
        };
        Some(ExclusiveMaintenanceLease {
            _guard: guard,
            _os_file: os_file,
        })
    }

    /// 由 shared lease 生成写许可。
    pub fn shared_permit(lease: SharedWriteLease) -> DatabaseWritePermit {
        DatabaseWritePermit::Shared(lease)
    }

    /// 由已持有的 exclusive lease 生成 maintenance 写许可（不嵌套 shared）。
    pub fn exclusive_permit(_lease: &ExclusiveMaintenanceLease) -> DatabaseWritePermit {
        DatabaseWritePermit::MaintenanceExclusive
    }

    /// 阻塞获取 OS shared 锁（spawn_blocking）。
    async fn acquire_os_shared_blocking(&self) -> Option<File> {
        let path = self.cross_process_lock_path.clone()?;
        tokio::task::spawn_blocking(move || open_and_lock_shared(&path))
            .await
            .ok()
            .flatten()
    }

    /// 阻塞获取 OS exclusive 锁（spawn_blocking，短轮询）。
    async fn acquire_os_exclusive_blocking(&self) -> Option<File> {
        let path = self.cross_process_lock_path.clone()?;
        tokio::task::spawn_blocking(move || open_and_lock_exclusive_wait(&path))
            .await
            .ok()
            .flatten()
    }

    fn try_os_shared(&self) -> Result<Option<File>, ()> {
        let Some(path) = self.cross_process_lock_path.as_ref() else {
            return Ok(None);
        };
        open_and_try_lock_shared(path).map(Some).map_err(|_| ())
    }

    fn try_os_exclusive(&self) -> Result<Option<File>, ()> {
        let Some(path) = self.cross_process_lock_path.as_ref() else {
            return Ok(None);
        };
        open_and_try_lock_exclusive(path).map(Some).map_err(|_| ())
    }
}

impl Default for DatabaseMaintenanceGate {
    fn default() -> Self {
        Self::new()
    }
}

/// 打开锁文件并获取 shared 锁（阻塞）。
fn open_and_lock_shared(path: &Path) -> Option<File> {
    let file = open_lock_file(path).ok()?;
    // 阻塞 shared：exclusive 持有时挂起直至释放
    fs4::FileExt::lock_shared(&file).ok()?;
    Some(file)
}

/// 打开锁文件并 try shared。
fn open_and_try_lock_shared(path: &Path) -> Result<File, AppError> {
    let file = open_lock_file(path)?;
    match fs4::FileExt::try_lock_shared(&file) {
        Ok(()) => Ok(file),
        Err(fs4::TryLockError::WouldBlock) => Err(AppError::unavailable(
            "数据库处于 exclusive 维护中（恢复进行中），请稍后重试",
        )),
        Err(fs4::TryLockError::Error(err)) => Err(err.into()),
    }
}

/// 打开锁文件并 try exclusive。
fn open_and_try_lock_exclusive(path: &Path) -> Result<File, AppError> {
    let file = open_lock_file(path)?;
    match fs4::FileExt::try_lock(&file) {
        Ok(()) => Ok(file),
        Err(fs4::TryLockError::WouldBlock) => Err(AppError::unavailable(
            "无法获取数据库 exclusive 维护锁（其它 writer/restore 占用）",
        )),
        Err(fs4::TryLockError::Error(err)) => Err(err.into()),
    }
}

/// 打开锁文件并阻塞获取 exclusive（短轮询，最多 ~30s）。
fn open_and_lock_exclusive_wait(path: &Path) -> Option<File> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match open_and_try_lock_exclusive(path) {
            Ok(f) => return Some(f),
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

fn open_lock_file(path: &Path) -> Result<File, AppError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?)
}

/// 生产路径唯一写事务构造器。
pub async fn begin_write_with_permit<'c>(
    pool: &'c SqlitePool,
    _permit: &DatabaseWritePermit,
) -> Result<Transaction<'c, Sqlite>, AppError> {
    Ok(pool.begin().await?)
}

/// 普通 writer 便捷：acquire shared → permit → begin。
pub async fn begin_shared_write<'c>(
    pool: &'c SqlitePool,
    gate: &DatabaseMaintenanceGate,
) -> Result<(DatabaseWritePermit, Transaction<'c, Sqlite>), AppError> {
    let lease = gate.acquire_shared().await;
    let permit = DatabaseMaintenanceGate::shared_permit(lease);
    let tx = begin_write_with_permit(pool, &permit).await?;
    Ok((permit, tx))
}

/// 在 shared lease 下执行闭包写操作（用于单语句 execute 路径）。
///
/// Business Logic: 无事务的 INSERT/UPDATE/DELETE 也必须经 gate，避免 restore 中途被覆盖。
/// Code Logic: acquire_shared（含跨进程 shared 阻塞）→ 跑 future → drop lease。
pub async fn with_shared_write_lease<F, T>(gate: &DatabaseMaintenanceGate, f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let _lease = gate.acquire_shared().await;
    f.await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;
    use tempfile::tempdir;

    async fn memory_pool() -> SqlitePool {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn exclusive_blocks_ordinary_shared_try() {
        let gate = DatabaseMaintenanceGate::new();
        let exclusive = gate.acquire_exclusive().await;
        assert!(gate.try_acquire_shared().is_none());
        drop(exclusive);
        assert!(gate.try_acquire_shared().is_some());
    }

    #[tokio::test]
    async fn exclusive_permit_begins_without_nested_shared_deadlock() {
        let gate = DatabaseMaintenanceGate::new();
        let pool = memory_pool().await;
        sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        let exclusive = gate.acquire_exclusive().await;
        let permit = DatabaseMaintenanceGate::exclusive_permit(&exclusive);
        let mut tx = begin_write_with_permit(&pool, &permit).await.unwrap();
        sqlx::query("INSERT INTO t (id) VALUES (1)")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(exclusive);
    }

    /// 跨进程 exclusive 阻塞另一 gate 的 shared try。
    #[tokio::test]
    async fn cross_process_exclusive_blocks_shared_try() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db-maintenance.lock");
        let gate_a = DatabaseMaintenanceGate::with_cross_process_lock(path.clone());
        let gate_b = DatabaseMaintenanceGate::with_cross_process_lock(path);
        let exclusive = gate_a.acquire_exclusive().await;
        assert!(
            exclusive._os_file.is_some(),
            "exclusive must hold OS lock"
        );
        // 另一进程语义：不同 gate 实例
        assert!(gate_b.try_acquire_shared().is_none());
        drop(exclusive);
        assert!(gate_b.try_acquire_shared().is_some());
    }
}
