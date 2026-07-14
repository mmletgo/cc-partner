//! storage/maintenance_gate.rs — 全局 SQLite 写事务维护屏障
//!
//! Business Logic（为什么需要这个模块）:
//!     恢复备份时必须独占数据库，防止本地/LAN/后台 writer 在 pre-restore 备份与
//!     replace/merge commit 之间静默覆盖数据。普通写路径用 shared lease 并发，
//!     restore 用 exclusive lease，且 exclusive 路径绝不重入 shared。
//!
//! Code Logic（这个模块做什么）:
//!     `DatabaseMaintenanceGate` 内部 `tokio::sync::RwLock`；
//!     `DatabaseWritePermit::{Shared,MaintenanceExclusive}` 贯穿 commit/rollback；
//!     生产路径唯一事务构造器为 `begin_write_with_permit`。

use crate::error::AppError;
use sqlx::sqlite::SqlitePool;
use sqlx::{Sqlite, Transaction};
use std::sync::Arc;
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

/// 全局 DB maintenance 读写屏障。
///
/// Business Logic: AppState 持有一份，所有生产 SQLite writer 共享。
/// Code Logic: Arc 包裹的 RwLock；shared=读锁，exclusive=写锁。
#[derive(Clone, Debug)]
pub struct DatabaseMaintenanceGate {
    inner: Arc<RwLock<()>>,
}

/// Shared 写租约：普通命令/调度/LAN 写路径持有。
///
/// Business Logic: 多 writer 可并存；restore exclusive 期间全部阻塞。
/// Code Logic: OwnedRwLockReadGuard，Drop 时释放。
#[derive(Debug)]
pub struct SharedWriteLease {
    _guard: OwnedRwLockReadGuard<()>,
}

/// Exclusive 维护租约：restore 从 pre-backup 到索引重建全程持有。
///
/// Business Logic: 独占期禁止任何 ordinary shared writer。
/// Code Logic: OwnedRwLockWriteGuard。
#[derive(Debug)]
pub struct ExclusiveMaintenanceLease {
    _guard: OwnedRwLockWriteGuard<()>,
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
    /// 构造空闲 gate。
    ///
    /// Business Logic: runtime 装配与测试 fixture 需要。
    /// Code Logic: 新建 Arc<RwLock<()>>。
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(())),
        }
    }

    /// 获取 ordinary writer 的 shared lease。
    ///
    /// Business Logic: 普通写路径在 begin 前调用；restore exclusive 时挂起。
    /// Code Logic: `read_owned().await`。
    pub async fn acquire_shared(&self) -> SharedWriteLease {
        let guard = self.inner.clone().read_owned().await;
        SharedWriteLease { _guard: guard }
    }

    /// 尝试非阻塞获取 shared lease。
    ///
    /// Business Logic: 测试可断言 restore 期间 ordinary writer 被挡。
    /// Code Logic: `try_read_owned`。
    pub fn try_acquire_shared(&self) -> Option<SharedWriteLease> {
        self.inner
            .clone()
            .try_read_owned()
            .ok()
            .map(|guard| SharedWriteLease { _guard: guard })
    }

    /// 获取 restore 独占 lease。
    ///
    /// Business Logic: pre-restore 备份前取得，索引重建后释放。
    /// Code Logic: `write_owned().await`。
    pub async fn acquire_exclusive(&self) -> ExclusiveMaintenanceLease {
        let guard = self.inner.clone().write_owned().await;
        ExclusiveMaintenanceLease { _guard: guard }
    }

    /// 尝试非阻塞获取 exclusive。
    ///
    /// Business Logic: 测试与诊断。
    /// Code Logic: `try_write_owned`。
    pub fn try_acquire_exclusive(&self) -> Option<ExclusiveMaintenanceLease> {
        self.inner
            .clone()
            .try_write_owned()
            .ok()
            .map(|guard| ExclusiveMaintenanceLease { _guard: guard })
    }

    /// 由 shared lease 生成写许可。
    ///
    /// Business Logic: 普通 writer 在 begin 前转换。
    /// Code Logic: 移动 lease 进 enum。
    pub fn shared_permit(lease: SharedWriteLease) -> DatabaseWritePermit {
        DatabaseWritePermit::Shared(lease)
    }

    /// 由已持有的 exclusive lease 生成 maintenance 写许可（不嵌套 shared）。
    ///
    /// Business Logic: restore 在独占期开事务时使用。
    /// Code Logic: 返回 MaintenanceExclusive 标记；lease 继续由调用栈持有。
    pub fn exclusive_permit(_lease: &ExclusiveMaintenanceLease) -> DatabaseWritePermit {
        DatabaseWritePermit::MaintenanceExclusive
    }
}

impl Default for DatabaseMaintenanceGate {
    fn default() -> Self {
        Self::new()
    }
}

/// 生产路径唯一写事务构造器。
///
/// Business Logic: 所有 SQLite 写事务必须经此入口；inventory 拒绝旁路 `.begin()`。
/// Code Logic: permit 仅作生命周期/语义标记；实际 `pool.begin()`。
///
/// # Arguments
/// * `pool` — 共享 SqlitePool
/// * `permit` — Shared 或 MaintenanceExclusive，必须活到 commit/rollback
pub async fn begin_write_with_permit<'c>(
    pool: &'c SqlitePool,
    _permit: &DatabaseWritePermit,
) -> Result<Transaction<'c, Sqlite>, AppError> {
    Ok(pool.begin().await?)
}

/// 普通 writer 便捷：acquire shared → permit → begin。
///
/// Business Logic: 仓库方法一行开写事务，减少样板。
/// Code Logic: 返回 (permit, tx)；调用方必须持有 permit 至 commit。
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
/// Code Logic: acquire_shared → 跑 future → drop lease。
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
    use std::time::Duration;
    use tokio::time::timeout;

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

    /// exclusive 持有时 ordinary shared try 失败。
    #[tokio::test]
    async fn exclusive_blocks_ordinary_shared_try() {
        let gate = DatabaseMaintenanceGate::new();
        let exclusive = gate.acquire_exclusive().await;
        assert!(gate.try_acquire_shared().is_none());
        drop(exclusive);
        assert!(gate.try_acquire_shared().is_some());
    }

    /// restore exclusive + maintenance permit 开事务不自死锁。
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
        let mut tx = timeout(Duration::from_secs(2), begin_write_with_permit(&pool, &permit))
            .await
            .expect("must not deadlock waiting for shared")
            .expect("begin ok");
        sqlx::query("INSERT INTO t (id) VALUES (1)")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(permit);
        drop(exclusive);
    }

    /// exclusive 释放前 ordinary shared begin 超时（模拟阻塞）。
    #[tokio::test]
    async fn ordinary_writer_blocked_until_exclusive_release() {
        let gate = Arc::new(DatabaseMaintenanceGate::new());
        let exclusive = gate.acquire_exclusive().await;

        let gate2 = gate.clone();
        let blocked = tokio::spawn(async move {
            // 不应立即返回
            gate2.acquire_shared().await
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!blocked.is_finished());
        drop(exclusive);
        let lease = timeout(Duration::from_secs(2), blocked)
            .await
            .expect("join")
            .expect("task");
        drop(lease);
    }

    /// shared 与 begin_write_with_permit 可提交。
    #[tokio::test]
    async fn shared_permit_write_commits() {
        let gate = DatabaseMaintenanceGate::new();
        let pool = memory_pool().await;
        sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        let (permit, mut tx) = begin_shared_write(&pool, &gate).await.unwrap();
        sqlx::query("INSERT INTO t (id) VALUES (7)")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(permit);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM t")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    /// 生产 writer inventory：扫描源码，禁止旁路 pool.begin / conn.begin（测试与 gate 自身除外）。
    #[test]
    fn production_writer_inventory_rejects_raw_begin() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        walk_rs(&root, &mut |path, content| {
            let rel = path
                .strip_prefix(&root)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| path.display().to_string());
            // 白名单：gate 自身、schema bootstrap、纯测试模块
            if rel.contains("maintenance_gate.rs") {
                return;
            }
            if rel.ends_with("/tests.rs") || rel.contains("/tests/") {
                return;
            }
            // 允许文件内 #[cfg(test)] 区块中的 begin；启发式：行所在文件若含 begin 且不在注释
            for (idx, line) in content.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.starts_with("//") {
                    continue;
                }
                // raw begin 旁路：`.begin().await` 或 `pool.begin()` / `db.begin()` / `conn.begin()`
                let is_begin = trimmed.contains(".begin()")
                    && !trimmed.contains("begin_write_with_permit")
                    && !trimmed.contains("begin_shared_write");
                if !is_begin {
                    continue;
                }
                // 跳过明显测试辅助（文件名含 test 的路径已处理；行内 cfg test 难解析，靠模块划分）
                if rel.contains("mod.rs") && trimmed.contains("begin") {
                    // orchestrator helpers etc — still must migrate
                }
                offenders.push(format!("{rel}:{}: {trimmed}", idx + 1));
            }
        });

        // 初始迁移完成后应为空；若仍有残留则失败并列出
        if !offenders.is_empty() {
            // 允许 schema/ensure 路径中的 bootstrap？init_db 用 execute 不用 begin。
            panic!(
                "raw SQLite begin bypasses maintenance gate ({}):\n{}",
                offenders.len(),
                offenders.join("\n")
            );
        }
    }

    fn walk_rs(dir: &std::path::Path, f: &mut dyn FnMut(&std::path::Path, &str)) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk_rs(&path, f);
            } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    f(&path, &content);
                }
            }
        }
    }
}
