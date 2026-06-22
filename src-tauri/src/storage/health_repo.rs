//! health_repo.rs — 健康提醒模块的 SQLite 存储
//!
//! Business Logic（为什么需要这个模块）:
//!     健康提醒功能需要持续记录用户每分钟的活动 / 闲置状态（用于久坐提醒、
//!     屏幕使用时长统计）以及喝水打卡记录。Task 6 的后台 daemon 每分钟采样一次
//!     前台窗口活动，把采样结果写入 `activity_records`；统计窗口内的活跃 / 闲置
//!     分钟数，并定期清理过期明细控制库体积；用户点击「喝水」按钮则写一条
//!     `water_records`。本模块封装这些读写。
//!
//! Code Logic（这个模块做什么）:
//!     持有共享 `SqlitePool`，用运行期 `sqlx::query`（非宏）执行 SQL。
//!     `activity_records` 以分钟级 unix 时间戳 `ts` 为主键，同分钟重采时用
//!     `INSERT OR REPLACE` 覆盖；`aggregate_minutes` 用 `SUM(CASE WHEN ...)` 在
//!     SQL 层完成活跃/闲置计数，避免把全量明细拉进内存。

use crate::error::AppError;
use sqlx::{Row, SqlitePool};

/// 单分钟活动采样行。
#[derive(Debug, Clone)]
pub struct ActivityRecord {
    /// 分钟级 unix 时间戳（主键，同一分钟重采会覆盖）。
    pub ts: i64,
    /// 该分钟内是否检测到用户活动（键鼠输入 / 非空闲）。
    pub is_active: bool,
    /// 该分钟内前台进程名（可空，闲置或采集失败时为 None）。
    pub process_name: Option<String>,
    /// 该分钟内前台窗口标题（可空，闲置或采集失败时为 None）。
    pub window_title: Option<String>,
}

/// health 模块数据库访问对象，封装 activity_records / water_records 的全部读写。
pub struct HealthRepo {
    /// SQLite 连接池（max_connections(1)，单连接语义，与其他 repo 共享同一池）。
    db: SqlitePool,
}

impl HealthRepo {
    /// 构造 HealthRepo，传入共享连接池。
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    /// 写入一条分钟级活动记录。
    ///
    /// Business Logic: daemon 每分钟采样一次前台活动，落库供后续久坐/屏幕时长统计。
    ///     同一分钟若重采（例如系统挂起恢复后补采），用 INSERT OR REPLACE 覆盖，
    ///     保证每个分钟桶只有一行最新结果。
    /// Code Logic: 绑定 (ts, is_active as i64, process_name, window_title) 执行
    ///     INSERT OR REPLACE，is_active 布尔转 0/1 存储。
    pub async fn insert_activity(&self, r: &ActivityRecord) -> Result<(), AppError> {
        sqlx::query(
            "INSERT OR REPLACE INTO activity_records (ts, is_active, process_name, window_title) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(r.ts)
        .bind(r.is_active as i64)
        .bind(r.process_name.as_deref())
        .bind(r.window_title.as_deref())
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// 取 [since_ts, +∞) 区间内的活动记录（按 ts 升序）。
    ///
    /// Business Logic: daemon 需要回看一个统计窗口（如最近 60 分钟）的全部明细，
    ///     用于触达判定或前端展示。
    /// Code Logic: SELECT 全字段 WHERE ts >= ? ORDER BY ts，逐行 try_get 还原为
    ///     ActivityRecord（is_active: i64 != 0）。
    #[allow(dead_code)]
    pub async fn get_activities_since(
        &self,
        since_ts: i64,
    ) -> Result<Vec<ActivityRecord>, AppError> {
        let rows = sqlx::query(
            "SELECT ts, is_active, process_name, window_title FROM activity_records WHERE ts >= ? ORDER BY ts",
        )
        .bind(since_ts)
        .fetch_all(&self.db)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(ActivityRecord {
                    ts: row.try_get("ts")?,
                    is_active: row.try_get::<i64, _>("is_active")? != 0,
                    process_name: row.try_get("process_name")?,
                    window_title: row.try_get("window_title")?,
                })
            })
            .collect()
    }

    /// 统计 [since_ts, +∞) 内活跃 / 非活跃分钟数。
    ///
    /// Business Logic: 久坐 / 屏幕时长提醒需要知道「最近 N 分钟内有多少分钟活跃、
    ///     多少分钟闲置」，由此判断是否触发提醒。
    /// Code Logic: 用 SUM(CASE WHEN is_active=1 THEN 1 ELSE 0 END) 在 SQL 层分别计
    ///     活跃 / 闲置行数；无任何记录时 SUM 返回 NULL，回退为 0。返回 (active, idle)。
    pub async fn aggregate_minutes(&self, since_ts: i64) -> Result<(i64, i64), AppError> {
        let row = sqlx::query(
            "SELECT \
                SUM(CASE WHEN is_active=1 THEN 1 ELSE 0 END) AS active, \
                SUM(CASE WHEN is_active=0 THEN 1 ELSE 0 END) AS idle \
             FROM activity_records WHERE ts >= ?",
        )
        .bind(since_ts)
        .fetch_optional(&self.db)
        .await?;
        match row {
            Some(r) => {
                let active: i64 = r.try_get("active").ok().unwrap_or(0);
                let idle: i64 = r.try_get("idle").ok().unwrap_or(0);
                Ok((active, idle))
            }
            None => Ok((0, 0)),
        }
    }

    /// 删除 ts < cutoff_ts 的活动明细。
    ///
    /// Business Logic: activity_records 会随时间无限增长，daemon 需定期清理超出
    ///     统计窗口（例如 24 小时）的旧数据以控制库体积。
    /// Code Logic: DELETE FROM activity_records WHERE ts < ?，返回受影响行数。
    pub async fn cleanup_older_than(&self, cutoff_ts: i64) -> Result<u64, AppError> {
        let res = sqlx::query("DELETE FROM activity_records WHERE ts < ?")
            .bind(cutoff_ts)
            .execute(&self.db)
            .await?;
        Ok(res.rows_affected())
    }

    /// 记录一次喝水打卡。
    ///
    /// Business Logic: 用户点击「喝水」按钮时记录该时刻，water_records 用于后续
    ///     喝水频率统计 / 提醒。以 ts 为主键，INSERT OR REPLACE 保证同一时间戳幂等。
    /// Code Logic: INSERT OR REPLACE INTO water_records (ts) VALUES (?)。
    #[allow(dead_code)]
    pub async fn insert_water(&self, ts: i64) -> Result<(), AppError> {
        sqlx::query("INSERT OR REPLACE INTO water_records (ts) VALUES (?)")
            .bind(ts)
            .execute(&self.db)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// 构造内存库并建表，供单测复用。
    async fn setup_db() -> SqlitePool {
        let options = sqlx::sqlite::SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS activity_records (ts INTEGER PRIMARY KEY, is_active INTEGER NOT NULL, process_name TEXT, window_title TEXT)").execute(&pool).await.unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS water_records (ts INTEGER PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    #[tokio::test]
    async fn test_insert_and_aggregate() {
        let pool = setup_db().await;
        let repo = HealthRepo::new(pool);
        repo.insert_activity(&ActivityRecord {
            ts: 1000,
            is_active: true,
            process_name: Some("code".into()),
            window_title: None,
        })
        .await
        .unwrap();
        repo.insert_activity(&ActivityRecord {
            ts: 1001,
            is_active: true,
            process_name: None,
            window_title: None,
        })
        .await
        .unwrap();
        repo.insert_activity(&ActivityRecord {
            ts: 1002,
            is_active: false,
            process_name: None,
            window_title: None,
        })
        .await
        .unwrap();
        let (active, idle) = repo.aggregate_minutes(0).await.unwrap();
        assert_eq!(active, 2);
        assert_eq!(idle, 1);
    }

    #[tokio::test]
    async fn test_cleanup() {
        let pool = setup_db().await;
        let repo = HealthRepo::new(pool);
        repo.insert_activity(&ActivityRecord {
            ts: 1,
            is_active: true,
            process_name: None,
            window_title: None,
        })
        .await
        .unwrap();
        repo.insert_activity(&ActivityRecord {
            ts: 100,
            is_active: true,
            process_name: None,
            window_title: None,
        })
        .await
        .unwrap();
        let n = repo.cleanup_older_than(50).await.unwrap();
        assert_eq!(n, 1);
        let recs = repo.get_activities_since(0).await.unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].ts, 100);
    }

    #[tokio::test]
    async fn test_water_record() {
        let pool = setup_db().await;
        let repo = HealthRepo::new(pool);
        repo.insert_water(9999).await.unwrap();
        // 不 panic 即通过（INSERT OR REPLACE 幂等）
        repo.insert_water(9999).await.unwrap();
    }
}
