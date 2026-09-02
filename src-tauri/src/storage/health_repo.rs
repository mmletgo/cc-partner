//! health_repo.rs — 健康提醒模块的 SQLite 存储
//!
//! Business Logic（为什么需要这个模块）:
//!     健康提醒功能需要持续记录用户每分钟的活动 / 闲置 / 在场状态（用于久坐提醒、
//!     屏幕使用时长统计）以及喝水打卡记录。Task 6 的后台 daemon 每分钟采样一次
//!     前台窗口活动，把采样结果写入 `activity_records`；统计窗口内的在场 / 活跃 /
//!     离场分钟数，并定期清理过期明细控制库体积；用户点击「喝水」按钮则写一条
//!     `water_records`。本模块封装这些读写。
//!
//! Code Logic（这个模块做什么）:
//!     持有共享 `SqlitePool`，用运行期 `sqlx::query`（非宏）执行 SQL。
//!     `activity_records` 以分钟级 unix 时间戳 `ts` 为主键，同分钟重采时用
//!     `INSERT OR REPLACE` 覆盖；`aggregate_presence_minutes` 用 `SUM(CASE WHEN ...)` 在
//!     SQL 层完成在场/活跃/离场计数，避免把全量明细拉进内存。

use crate::error::AppError;
use crate::storage::maintenance_gate::{with_shared_write_lease, DatabaseMaintenanceGate};
use sqlx::{Row, SqlitePool};
use std::sync::Arc;

/// 按 process_name 聚合活跃分钟。
const APP_USAGE_SQL: &str = "SELECT process_name AS name, COUNT(*) AS mins FROM activity_records \
     WHERE ts >= ? AND is_active = 1 AND process_name IS NOT NULL AND process_name <> '' \
     GROUP BY process_name ORDER BY mins DESC";

/// 按 window_title 聚合活跃分钟。
const WINDOW_USAGE_SQL: &str =
    "SELECT window_title AS name, COUNT(*) AS mins FROM activity_records \
     WHERE ts >= ? AND is_active = 1 AND window_title IS NOT NULL AND window_title <> '' \
     GROUP BY window_title ORDER BY mins DESC";

/// 单分钟活动采样行。
#[derive(Debug, Clone)]
pub struct ActivityRecord {
    /// 分钟级 unix 时间戳（主键，同一分钟重采会覆盖）。
    pub ts: i64,
    /// 该分钟内是否检测到用户活动（键鼠输入 / 非空闲）。
    pub is_active: bool,
    /// 该分钟用户是否在场（Working 相位；采样降级时退化为 is_active）。
    pub is_present: bool,
    /// 该分钟内前台进程名（可空，闲置或采集失败时为 None）。
    pub process_name: Option<String>,
    /// 该分钟内前台窗口标题（可空，闲置或采集失败时为 None）。
    pub window_title: Option<String>,
}

/// 旧库幂等补 activity_records.is_present 列并按 is_active 近似回填。
///
/// Business Logic（为什么需要这个函数）:
///     「在场计时」改造给 activity_records 增加 is_present 列；旧用户库需无损升级，
///     用「键鼠活跃≈在场」近似回填历史数据，避免旧数据在场统计读出全 0。
/// Code Logic（这个函数做什么）:
///     PRAGMA table_info 检查无 is_present 列时执行 ALTER TABLE ADD COLUMN
///     `is_present INTEGER NOT NULL DEFAULT 0` + `UPDATE SET is_present = is_active`；
///     已有列或表不存在（PRAGMA 空集）时为 no-op。GUI 与 headless 共用的 init_db 调用。
pub async fn ensure_activity_present_column(pool: &SqlitePool) -> Result<(), AppError> {
    let columns = sqlx::query("PRAGMA table_info(activity_records)")
        .fetch_all(pool)
        .await?;
    // 表不存在时 PRAGMA 返回空集：单测/局部 schema 场景跳过，避免 ALTER 失败。
    if columns.is_empty() {
        return Ok(());
    }
    let has_col = columns.iter().any(|row| {
        row.try_get::<String, _>("name")
            .map(|name| name == "is_present")
            .unwrap_or(false)
    });
    if !has_col {
        sqlx::query(
            "ALTER TABLE activity_records ADD COLUMN is_present INTEGER NOT NULL DEFAULT 0",
        )
        .execute(pool)
        .await?;
        sqlx::query("UPDATE activity_records SET is_present = is_active")
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// health 模块数据库访问对象，封装 activity_records / water_records 的全部读写。
pub struct HealthRepo {
    /// SQLite 连接池（max_connections(1)，单连接语义，与其他 repo 共享同一池）。
    db: SqlitePool,
    /// 维护屏障：写路径持 shared lease，restore exclusive 时阻塞。
    gate: Arc<DatabaseMaintenanceGate>,
}

impl HealthRepo {
    /// 兼容构造：测试/局部 fixture 用独立 gate。
    ///
    /// Business Logic: 单测无需共享 AppState.maintenance_gate。
    /// Code Logic: 内部 `with_gate(db, Arc::new(DatabaseMaintenanceGate::new()))`。
    pub fn new(db: SqlitePool) -> Self {
        Self::with_gate(db, Arc::new(DatabaseMaintenanceGate::new()))
    }

    /// 生产构造：共享 AppState.maintenance_gate。
    ///
    /// Business Logic: 全部 ordinary writer 与 restore 共用同一 gate。
    /// Code Logic: 保存 pool + Arc gate。
    pub fn with_gate(db: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { db, gate }
    }

    /// 写入一条分钟级活动记录。
    ///
    /// Business Logic: daemon 每分钟采样一次前台活动，落库供后续久坐/在场统计。
    ///     同一分钟若重采（例如系统挂起恢复后补采），用 INSERT OR REPLACE 覆盖，
    ///     保证每个分钟桶只有一行最新结果。
    /// Code Logic: 持 shared write lease 后绑定 (ts, is_active, is_present, process_name,
    ///     window_title) 执行 INSERT OR REPLACE，布尔转 0/1 存储。
    pub async fn insert_activity(&self, r: &ActivityRecord) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT OR REPLACE INTO activity_records (ts, is_active, is_present, process_name, window_title) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(r.ts)
            .bind(r.is_active as i64)
            .bind(r.is_present as i64)
            .bind(r.process_name.as_deref())
            .bind(r.window_title.as_deref())
            .execute(&self.db)
            .await?;
            Ok(())
        })
        .await
    }

    /// 取 [since_ts, +∞) 区间内的活动记录（按 ts 升序）。
    ///
    /// Business Logic: daemon 需要回看一个统计窗口（如最近 60 分钟）的全部明细，
    ///     用于触达判定或前端展示。
    /// Code Logic: SELECT 全字段 WHERE ts >= ? ORDER BY ts，逐行 try_get 还原为
    ///     ActivityRecord（布尔: i64 != 0）。
    #[allow(dead_code)]
    pub async fn get_activities_since(
        &self,
        since_ts: i64,
    ) -> Result<Vec<ActivityRecord>, AppError> {
        let rows = sqlx::query(
            "SELECT ts, is_active, is_present, process_name, window_title FROM activity_records WHERE ts >= ? ORDER BY ts",
        )
        .bind(since_ts)
        .fetch_all(&self.db)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(ActivityRecord {
                    ts: row.try_get("ts")?,
                    is_active: row.try_get::<i64, _>("is_active")? != 0,
                    is_present: row.try_get::<i64, _>("is_present")? != 0,
                    process_name: row.try_get("process_name")?,
                    window_title: row.try_get("window_title")?,
                })
            })
            .collect()
    }

    /// 统计 [since_ts, +∞) 内在场 / 键鼠活跃 / 离场分钟数。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     活动统计改为「在场」口径：用户在设备前发呆（无键鼠输入但在场）也计入
    ///     在场时间；前端需要 present/active/away 三值分别展示。
    /// Code Logic（这个函数做什么）:
    ///     SQL 层聚合 present=SUM(is_present=1)、active=SUM(is_active=1)，
    ///     away=COUNT(*)-present（总桶数减在场）；无记录时 SUM 为 NULL 回退 0，
    ///     空表回 (0,0,0)。返回 (present, active, away)。
    pub async fn aggregate_presence_minutes(
        &self,
        since_ts: i64,
    ) -> Result<(i64, i64, i64), AppError> {
        let row = sqlx::query(
            "SELECT \
                COUNT(*) AS total, \
                SUM(CASE WHEN is_present=1 THEN 1 ELSE 0 END) AS present, \
                SUM(CASE WHEN is_active=1 THEN 1 ELSE 0 END) AS active \
             FROM activity_records WHERE ts >= ?",
        )
        .bind(since_ts)
        .fetch_optional(&self.db)
        .await?;
        match row {
            Some(r) => {
                let total: i64 = r.try_get("total").ok().unwrap_or(0);
                let present: i64 = r.try_get("present").ok().unwrap_or(0);
                let active: i64 = r.try_get("active").ok().unwrap_or(0);
                Ok((present, active, total.saturating_sub(present)))
            }
            None => Ok((0, 0, 0)),
        }
    }

    /// 按进程名聚合 [since_ts, +∞) 内的活跃分钟数,倒序返回(应用使用时长排行)。
    ///
    /// Business Logic: 统计页需要展示「今天在哪些 app 上花了多少分钟」,帮助用户
    ///     了解屏幕使用时长分布。仅统计活跃(is_active=1)且有 process_name 的行。
    /// Code Logic: 委托 `get_grouped_usage` + `APP_USAGE_SQL`。
    pub async fn get_app_usage(&self, since_ts: i64) -> Result<Vec<(String, i64)>, AppError> {
        self.get_grouped_usage(since_ts, APP_USAGE_SQL).await
    }

    /// 按窗口标题聚合 [since_ts, +∞) 内的活跃分钟数,倒序返回(窗口使用时长排行)。
    ///
    /// Business Logic: 统计页需要展示「今天在哪些窗口上花了多少分钟」。同一 app
    ///     下不同窗口标题(文件、网页、会话)应分开计时;仅统计活跃且有标题的行。
    /// Code Logic: 委托 `get_grouped_usage` + `WINDOW_USAGE_SQL`。
    pub async fn get_window_usage(&self, since_ts: i64) -> Result<Vec<(String, i64)>, AppError> {
        self.get_grouped_usage(since_ts, WINDOW_USAGE_SQL).await
    }

    /// 按固定列聚合活跃分钟,倒序返回 `(名称, 分钟)`。
    ///
    /// Business Logic: app 排行与窗口标题排行口径相同,只是分组列不同;
    ///     抽成一处避免两套 SQL 漂移。
    /// Code Logic: 执行调用方传入的静态 SQL(仅 process_name / window_title 两份常量),
    ///     过滤 is_active=1 且该列非空,GROUP BY 后按 mins DESC;逐行还原 (name, mins)。
    async fn get_grouped_usage(
        &self,
        since_ts: i64,
        sql: &'static str,
    ) -> Result<Vec<(String, i64)>, AppError> {
        let rows = sqlx::query(sql).bind(since_ts).fetch_all(&self.db).await?;
        rows.iter()
            .map(|r| {
                Ok((
                    r.try_get::<Option<String>, _>("name")?.unwrap_or_default(),
                    r.try_get("mins")?,
                ))
            })
            .collect()
    }

    /// 按本地小时(0-23)聚合 [since_ts, +∞) 内的在场分钟数,返回长度 24 的数组。
    ///
    /// Business Logic: 统计页需要展示「一天 24 小时每个时段的在场分布」,帮助用户
    ///     了解工作节奏(例如上午/下午/深夜的在场峰值)。口径为在场(is_present=1):
    ///     用户在设备前发呆(无键鼠输入但在场)也计入;用户体感中的「上午 10 点」
    ///     是本地时区的 10 点,用 UTC 桶在东八区会整体偏移 8 小时,图表错位。
    ///
    /// Code Logic: SQL 层用 SQLite `strftime('%H', datetime(ts,'unixepoch','localtime'))`
    ///     把时间戳按系统本地时区取小时(00-23 字符串),CAST INTEGER 后对 is_present=1
    ///     的行 GROUP BY;先初始化 24 个 0,再把查询结果填入对应桶(范围外忽略,保长度恒 24)。
    pub async fn get_hourly_activity(&self, since_ts: i64) -> Result<Vec<i64>, AppError> {
        let mut hours = vec![0i64; 24];
        let rows = sqlx::query(
            "SELECT CAST(strftime('%H', datetime(ts, 'unixepoch', 'localtime')) AS INTEGER) AS h, \
             COUNT(*) AS mins FROM activity_records \
             WHERE ts >= ? AND is_present = 1 GROUP BY h",
        )
        .bind(since_ts)
        .fetch_all(&self.db)
        .await?;
        for r in rows {
            let h: i64 = r.try_get("h")?;
            let mins: i64 = r.try_get("mins")?;
            if (0..24).contains(&h) {
                hours[h as usize] = mins;
            }
        }
        Ok(hours)
    }

    /// 删除 ts < cutoff_ts 的活动明细。
    ///
    /// Business Logic: activity_records 会随时间无限增长，daemon 需定期清理超出
    ///     统计窗口（例如 24 小时）的旧数据以控制库体积。
    /// Code Logic: 持 shared write lease 后 DELETE FROM activity_records WHERE ts < ?，返回受影响行数。
    pub async fn cleanup_older_than(&self, cutoff_ts: i64) -> Result<u64, AppError> {
        with_shared_write_lease(&self.gate, async {
            let res = sqlx::query("DELETE FROM activity_records WHERE ts < ?")
                .bind(cutoff_ts)
                .execute(&self.db)
                .await?;
            Ok(res.rows_affected())
        })
        .await
    }

    /// 记录一次喝水打卡。
    ///
    /// Business Logic: 用户点击「+1 杯」按钮时记录该时刻，water_records 用于后续喝水频率统计。
    ///     自增 id 主键（不再是 ts 主键），同秒连点也能各自成行，避免主键冲突丢计数。
    /// Code Logic: 持 shared write lease 后 INSERT ... RETURNING id，返回自增主键。
    pub async fn insert_water(&self, ts: i64) -> Result<i64, AppError> {
        with_shared_write_lease(&self.gate, async {
            let row: (i64,) =
                sqlx::query_as("INSERT INTO water_records (ts) VALUES (?) RETURNING id")
                    .bind(ts)
                    .fetch_one(&self.db)
                    .await?;
            Ok(row.0)
        })
        .await
    }

    /// Business Logic: 用户想看"今日喝了多少杯水",需要按时间范围统计饮水次数。
    /// Code Logic: SELECT COUNT(*) FROM water_records WHERE ts >= since_ts,返回单值。
    pub async fn count_water_since(&self, since_ts: i64) -> Result<i64, AppError> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM water_records WHERE ts >= ?")
            .bind(since_ts)
            .fetch_one(&self.db)
            .await?;
        Ok(row.0)
    }

    /// Business Logic: sparkline 展示近 7 天每日饮水次数,需要按本地日分桶聚合。
    /// Code Logic: 取出 since_ts 之后所有 ts,Rust 端按 (ts-since_ts)/86400 算桶号落入 Vec<i64>。
    /// 超出 [since_ts, since_ts + days*86400) 的记录丢弃。
    pub async fn get_daily_water_counts(
        &self,
        since_ts: i64,
        days: usize,
    ) -> Result<Vec<i64>, AppError> {
        let rows: Vec<(i64,)> =
            sqlx::query_as("SELECT ts FROM water_records WHERE ts >= ? ORDER BY ts ASC")
                .bind(since_ts)
                .fetch_all(&self.db)
                .await?;
        let mut buckets = vec![0i64; days];
        let span = (days as i64) * 86400;
        for (ts,) in rows {
            if ts < since_ts || ts >= since_ts + span {
                continue;
            }
            let idx = ((ts - since_ts) / 86400) as usize;
            if idx < days {
                buckets[idx] += 1;
            }
        }
        Ok(buckets)
    }

    /// Business Logic: 用户误点"+1 杯"后需要撤销,按自增 id 精准删除单条饮水记录。
    /// Code Logic: 持 shared write lease 后 DELETE FROM water_records WHERE id=?，返回 rows_affected > 0。
    pub async fn delete_water(&self, id: i64) -> Result<bool, AppError> {
        with_shared_write_lease(&self.gate, async {
            let result = sqlx::query("DELETE FROM water_records WHERE id = ?")
                .bind(id)
                .execute(&self.db)
                .await?;
            Ok(result.rows_affected() > 0)
        })
        .await
    }

    /// Business Logic: 保留 N 天数据避免数据库无限增长,定期清理过期饮水记录。
    /// Code Logic: 持 shared write lease 后 DELETE FROM water_records WHERE ts < cutoff_ts，返回删除行数。
    pub async fn cleanup_water_older_than(&self, cutoff_ts: i64) -> Result<u64, AppError> {
        with_shared_write_lease(&self.gate, async {
            let result = sqlx::query("DELETE FROM water_records WHERE ts < ?")
                .bind(cutoff_ts)
                .execute(&self.db)
                .await?;
            Ok(result.rows_affected())
        })
        .await
    }

    /// Business Logic: 前端"距下次提醒"需要知道上次喝水时间,推算剩余等待时长。
    /// Code Logic: SELECT MAX(ts) FROM water_records,表空返回 None。
    pub async fn get_last_water_ts(&self) -> Result<Option<i64>, AppError> {
        let row: Option<(Option<i64>,)> = sqlx::query_as("SELECT MAX(ts) FROM water_records")
            .fetch_optional(&self.db)
            .await?;
        Ok(row.and_then(|(opt,)| opt))
    }

    /// Business Logic: 久坐提醒触发或用户完成休息时,记录事件用于习惯统计。
    ///     kind 区分事件类型,reminder=提醒触发(用户可能跳过),rest=实际完成休息。
    /// Code Logic: 持 shared write lease 后 INSERT INTO rest_records ... RETURNING id，返回自增主键。
    pub async fn insert_rest_record(
        &self,
        ts: i64,
        kind: &str,
        duration_seconds: i64,
    ) -> Result<i64, AppError> {
        with_shared_write_lease(&self.gate, async {
            let row: (i64,) = sqlx::query_as(
                "INSERT INTO rest_records (ts, kind, duration_seconds) VALUES (?, ?, ?) RETURNING id",
            )
            .bind(ts)
            .bind(kind)
            .bind(duration_seconds)
            .fetch_one(&self.db)
            .await?;
            Ok(row.0)
        })
        .await
    }

    /// Business Logic: 统计今日提醒触发次数 / 完成休息次数,按 kind 过滤。
    /// Code Logic: SELECT COUNT(*) FROM rest_records WHERE ts >= ? AND kind = ?。
    pub async fn count_rest_since(&self, since_ts: i64, kind: &str) -> Result<i64, AppError> {
        let row: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM rest_records WHERE ts >= ? AND kind = ?")
                .bind(since_ts)
                .bind(kind)
                .fetch_one(&self.db)
                .await?;
        Ok(row.0)
    }

    /// Business Logic: 展示"今日总休息时长",只累加实际完成的休息(kind='rest')。
    /// Code Logic: SELECT COALESCE(SUM(duration_seconds),0) FROM rest_records
    ///     WHERE ts >= ? AND kind='rest'。COALESCE 保证空表返回 0 不返回 NULL。
    pub async fn sum_rest_duration_since(&self, since_ts: i64) -> Result<i64, AppError> {
        let row: (i64,) = sqlx::query_as(
            "SELECT COALESCE(SUM(duration_seconds), 0) FROM rest_records WHERE ts >= ? AND kind = 'rest'",
        )
        .bind(since_ts)
        .fetch_one(&self.db)
        .await?;
        Ok(row.0)
    }

    /// Business Logic: sparkline 展示近 7 天每日完成休息次数,按本地日分桶。
    /// Code Logic: 取出指定 kind 的所有 ts,Rust 端按 (ts-since_ts)/86400 分桶。
    pub async fn get_daily_rest_counts(
        &self,
        since_ts: i64,
        days: usize,
        kind: &str,
    ) -> Result<Vec<i64>, AppError> {
        let rows: Vec<(i64,)> = sqlx::query_as(
            "SELECT ts FROM rest_records WHERE ts >= ? AND kind = ? ORDER BY ts ASC",
        )
        .bind(since_ts)
        .bind(kind)
        .fetch_all(&self.db)
        .await?;
        let mut buckets = vec![0i64; days];
        let span = (days as i64) * 86400;
        for (ts,) in rows {
            if ts < since_ts || ts >= since_ts + span {
                continue;
            }
            let idx = ((ts - since_ts) / 86400) as usize;
            if idx < days {
                buckets[idx] += 1;
            }
        }
        Ok(buckets)
    }

    /// Business Logic: 用户撤销误记的休息事件,按自增 id 精准删除。
    /// Code Logic: 持 shared write lease 后 DELETE FROM rest_records WHERE id=?，返回 rows_affected > 0。
    /// NOTE: 暂无命令消费方(预留撤销误记的休息记录用),保留 dead_code 标注避免编译警告。
    #[allow(dead_code)]
    pub async fn delete_rest(&self, id: i64) -> Result<bool, AppError> {
        with_shared_write_lease(&self.gate, async {
            let result = sqlx::query("DELETE FROM rest_records WHERE id = ?")
                .bind(id)
                .execute(&self.db)
                .await?;
            Ok(result.rows_affected() > 0)
        })
        .await
    }

    /// 写入一条模板习惯事件（triggered / completed）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     自定义与内置模板都要按 template_id 聚合次数/时长，不能再只写死饮水/休息两张表。
    /// Code Logic（这个函数做什么）:
    ///     INSERT habit_records 返回自增 id。
    pub async fn insert_habit_record(
        &self,
        template_id: &str,
        ts: i64,
        kind: &str,
        duration_seconds: i64,
    ) -> Result<i64, AppError> {
        with_shared_write_lease(&self.gate, async {
            let row: (i64,) = sqlx::query_as(
                "INSERT INTO habit_records (template_id, ts, kind, duration_seconds) \
                 VALUES (?, ?, ?, ?) RETURNING id",
            )
            .bind(template_id)
            .bind(ts)
            .bind(kind)
            .bind(duration_seconds)
            .fetch_one(&self.db)
            .await?;
            Ok(row.0)
        })
        .await
    }

    /// 统计某模板自 since 起某 kind 的次数。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     习惯卡按模板展示今日完成/触发次数。
    /// Code Logic（这个函数做什么）:
    ///     COUNT habit_records WHERE template_id+kind+ts>=since。
    pub async fn count_habit_since(
        &self,
        template_id: &str,
        since_ts: i64,
        kind: &str,
    ) -> Result<i64, AppError> {
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM habit_records WHERE template_id = ? AND ts >= ? AND kind = ?",
        )
        .bind(template_id)
        .bind(since_ts)
        .bind(kind)
        .fetch_one(&self.db)
        .await?;
        Ok(row.0)
    }

    /// 累加某模板自 since 起 completed 时长。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     session 模板需要展示今日累计倒计时秒数。
    /// Code Logic（这个函数做什么）:
    ///     SUM duration_seconds WHERE completed。
    pub async fn sum_habit_duration_since(
        &self,
        template_id: &str,
        since_ts: i64,
    ) -> Result<i64, AppError> {
        let row: (i64,) = sqlx::query_as(
            "SELECT COALESCE(SUM(duration_seconds), 0) FROM habit_records \
             WHERE template_id = ? AND ts >= ? AND kind = 'completed'",
        )
        .bind(template_id)
        .bind(since_ts)
        .fetch_one(&self.db)
        .await?;
        Ok(row.0)
    }

    /// 近 N 日某模板 completed 次数分桶。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     sparkline 按模板画近 7 日完成次数。
    /// Code Logic（这个函数做什么）:
    ///     取 ts 后按 (ts-since)/86400 分桶，长度恒为 days。
    pub async fn get_daily_habit_counts(
        &self,
        template_id: &str,
        since_ts: i64,
        days: usize,
        kind: &str,
    ) -> Result<Vec<i64>, AppError> {
        let rows: Vec<(i64,)> = sqlx::query_as(
            "SELECT ts FROM habit_records WHERE template_id = ? AND ts >= ? AND kind = ? ORDER BY ts ASC",
        )
        .bind(template_id)
        .bind(since_ts)
        .bind(kind)
        .fetch_all(&self.db)
        .await?;
        let mut buckets = vec![0i64; days];
        let span = (days as i64) * 86400;
        for (ts,) in rows {
            if ts < since_ts || ts >= since_ts + span {
                continue;
            }
            let idx = ((ts - since_ts) / 86400) as usize;
            if idx < days {
                buckets[idx] += 1;
            }
        }
        Ok(buckets)
    }

    /// 最近一次某模板 completed 时间戳。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     interval 模板要算距下次还差多久。
    /// Code Logic（这个函数做什么）:
    ///     MAX(ts) WHERE completed。
    pub async fn get_last_habit_ts(
        &self,
        template_id: &str,
        kind: &str,
    ) -> Result<Option<i64>, AppError> {
        let row: (Option<i64>,) =
            sqlx::query_as("SELECT MAX(ts) FROM habit_records WHERE template_id = ? AND kind = ?")
                .bind(template_id)
                .bind(kind)
                .fetch_one(&self.db)
                .await?;
        Ok(row.0)
    }

    /// 清理过期习惯事件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     与活动/饮水/休息共用 retainDays，避免新表无限增长。
    /// Code Logic（这个函数做什么）:
    ///     DELETE habit_records WHERE ts < cutoff。
    pub async fn cleanup_habit_older_than(&self, cutoff_ts: i64) -> Result<u64, AppError> {
        with_shared_write_lease(&self.gate, async {
            let result = sqlx::query("DELETE FROM habit_records WHERE ts < ?")
                .bind(cutoff_ts)
                .execute(&self.db)
                .await?;
            Ok(result.rows_affected())
        })
        .await
    }

    /// Business Logic: 保留 N 天数据,定期清理过期休息记录。
    /// Code Logic: 持 shared write lease 后 DELETE FROM rest_records WHERE ts < cutoff_ts，返回删除行数。
    pub async fn cleanup_rest_older_than(&self, cutoff_ts: i64) -> Result<u64, AppError> {
        with_shared_write_lease(&self.gate, async {
            let result = sqlx::query("DELETE FROM rest_records WHERE ts < ?")
                .bind(cutoff_ts)
                .execute(&self.db)
                .await?;
            Ok(result.rows_affected())
        })
        .await
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
        sqlx::query("CREATE TABLE IF NOT EXISTS activity_records (ts INTEGER PRIMARY KEY, is_active INTEGER NOT NULL, is_present INTEGER NOT NULL DEFAULT 0, process_name TEXT, window_title TEXT)").execute(&pool).await.unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS water_records (id INTEGER PRIMARY KEY AUTOINCREMENT, ts INTEGER NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS rest_records (id INTEGER PRIMARY KEY AUTOINCREMENT, ts INTEGER NOT NULL, kind TEXT NOT NULL, duration_seconds INTEGER NOT NULL DEFAULT 0)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS habit_records (id INTEGER PRIMARY KEY AUTOINCREMENT, template_id TEXT NOT NULL, ts INTEGER NOT NULL, kind TEXT NOT NULL, duration_seconds INTEGER NOT NULL DEFAULT 0)")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    #[tokio::test]
    async fn test_insert_and_aggregate() {
        let pool = setup_db().await;
        let repo = HealthRepo::new(pool);
        // 活跃且在场
        repo.insert_activity(&ActivityRecord {
            ts: 1000,
            is_active: true,
            is_present: true,
            process_name: Some("code".into()),
            window_title: None,
        })
        .await
        .unwrap();
        // 不活跃但在场(设备前发呆)
        repo.insert_activity(&ActivityRecord {
            ts: 1001,
            is_active: false,
            is_present: true,
            process_name: None,
            window_title: None,
        })
        .await
        .unwrap();
        // 离场
        repo.insert_activity(&ActivityRecord {
            ts: 1002,
            is_active: false,
            is_present: false,
            process_name: None,
            window_title: None,
        })
        .await
        .unwrap();
        let (present, active, away) = repo.aggregate_presence_minutes(0).await.unwrap();
        assert_eq!(present, 2);
        assert_eq!(active, 1);
        assert_eq!(away, 1);
    }

    /// 旧库(无 is_present 列)跑迁移后补列,is_present 按 is_active 回填,且迁移幂等。
    #[tokio::test]
    async fn test_migration_backfills_is_present_from_is_active() {
        let options = sqlx::sqlite::SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        // 手工建旧版(无 is_present)表并插入两行历史数据
        sqlx::query("CREATE TABLE activity_records (ts INTEGER PRIMARY KEY, is_active INTEGER NOT NULL, process_name TEXT, window_title TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO activity_records (ts, is_active) VALUES (1, 1)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO activity_records (ts, is_active) VALUES (2, 0)")
            .execute(&pool)
            .await
            .unwrap();

        ensure_activity_present_column(&pool).await.unwrap();
        // 幂等:重复执行不报错、不重复回填
        ensure_activity_present_column(&pool).await.unwrap();

        let repo = HealthRepo::new(pool);
        let recs = repo.get_activities_since(0).await.unwrap();
        assert_eq!(recs.len(), 2);
        assert!(recs[0].is_active);
        assert!(recs[0].is_present, "旧数据回填: 键鼠活跃≈在场");
        assert!(!recs[1].is_active);
        assert!(!recs[1].is_present);
        let (present, active, away) = repo.aggregate_presence_minutes(0).await.unwrap();
        assert_eq!((present, active, away), (1, 1, 1));
    }

    #[tokio::test]
    async fn test_cleanup() {
        let pool = setup_db().await;
        let repo = HealthRepo::new(pool);
        repo.insert_activity(&ActivityRecord {
            ts: 1,
            is_active: true,
            is_present: true,
            process_name: None,
            window_title: None,
        })
        .await
        .unwrap();
        repo.insert_activity(&ActivityRecord {
            ts: 100,
            is_active: true,
            is_present: true,
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
        let id1 = repo.insert_water(9999).await.unwrap();
        let id2 = repo.insert_water(9999).await.unwrap();
        // 自增 id:同秒 ts 各自成行,两条记录都保留
        assert!(id2 > id1, "同秒连点应各自生成不同自增 id");
        assert_eq!(repo.count_water_since(0).await.unwrap(), 2);
    }

    /// 验证同秒连点「+1 杯」每条都被记录(回归 ts 主键冲突丢计数 bug)。
    #[tokio::test]
    async fn test_insert_water_same_second_keeps_all() {
        let pool = setup_db().await;
        let repo = HealthRepo::new(pool);
        let same_ts = 1234567890;
        repo.insert_water(same_ts).await.unwrap();
        repo.insert_water(same_ts).await.unwrap();
        repo.insert_water(same_ts).await.unwrap();
        assert_eq!(repo.count_water_since(0).await.unwrap(), 3);
    }

    /// 按本地小时桶聚合在场分钟:插入「本地今天指定小时」的若干分钟记录,
    /// 断言只计 is_present=1 的行落在正确的本地小时桶。用 chrono 构造本地某小时的 ts,
    /// 使测试与时区无关。
    #[tokio::test]
    async fn test_hourly_activity_local_buckets() {
        use chrono::{Datelike, Local, NaiveDate, TimeZone};
        let pool = setup_db().await;
        let repo = HealthRepo::new(pool);

        let today = Local::now().naive_local().date();
        let date =
            NaiveDate::from_ymd_opt(today.year(), today.month(), today.day()).expect("valid today");
        let mk_ts = |h: u32, m: u32| -> i64 {
            Local
                .from_local_datetime(&date.and_hms_opt(h, m, 0).unwrap())
                .single()
                .unwrap()
                .timestamp()
        };
        // 本地 9:00/9:01 在场活跃 + 9:02 离场(不计入) + 9:03 在场但无键鼠输入(计入)
        let ts_900 = mk_ts(9, 0);
        let ts_901 = mk_ts(9, 1);
        let ts_902 = mk_ts(9, 2);
        let ts_903 = mk_ts(9, 3);
        for (ts, is_active) in [(ts_900, true), (ts_901, true)] {
            repo.insert_activity(&ActivityRecord {
                ts,
                is_active,
                is_present: true,
                process_name: None,
                window_title: None,
            })
            .await
            .unwrap();
        }
        repo.insert_activity(&ActivityRecord {
            ts: ts_902,
            is_active: false,
            is_present: false,
            process_name: None,
            window_title: None,
        })
        .await
        .unwrap();
        repo.insert_activity(&ActivityRecord {
            ts: ts_903,
            is_active: false,
            is_present: true,
            process_name: None,
            window_title: None,
        })
        .await
        .unwrap();

        let hours = repo.get_hourly_activity(0).await.unwrap();
        assert_eq!(hours.len(), 24);
        // 本地 9 点桶应只含在场分钟:活跃 2 + 在场发呆 1 = 3(离场 9:02 不计入)
        assert_eq!(hours[9], 3);
        let sum: i64 = hours.iter().sum();
        assert_eq!(sum, 3);
    }

    /// 验证 count_water_since 在空表/单条/多条情况下的计数。
    #[tokio::test]
    async fn test_count_water_since() {
        let pool = setup_db().await;
        let repo = HealthRepo::new(pool);
        // 空表
        assert_eq!(repo.count_water_since(0).await.unwrap(), 0);
        // 插入 3 条
        repo.insert_water(100).await.unwrap();
        repo.insert_water(200).await.unwrap();
        repo.insert_water(300).await.unwrap();
        // since=150 只计 200/300
        assert_eq!(repo.count_water_since(150).await.unwrap(), 2);
        // since=0 计全部
        assert_eq!(repo.count_water_since(0).await.unwrap(), 3);
    }

    /// 验证 get_daily_water_counts 返回长度恒为 days 的桶,且按本地日正确分桶。
    #[tokio::test]
    async fn test_get_daily_water_counts() {
        let pool = setup_db().await;
        let repo = HealthRepo::new(pool);
        repo.insert_water(0).await.unwrap(); // 桶 0
        repo.insert_water(86399).await.unwrap(); // 桶 0
        repo.insert_water(86400).await.unwrap(); // 桶 1
        repo.insert_water(6 * 86400).await.unwrap(); // 桶 6(今日)
        repo.insert_water(7 * 86400).await.unwrap(); // 超出范围,不计
        let counts = repo.get_daily_water_counts(0, 7).await.unwrap();
        assert_eq!(counts.len(), 7);
        assert_eq!(counts[0], 2);
        assert_eq!(counts[1], 1);
        assert_eq!(counts[6], 1);
    }

    /// 验证 delete_water 按自增 id 删除存在/不存在的记录的返回值。
    #[tokio::test]
    async fn test_delete_water() {
        let pool = setup_db().await;
        let repo = HealthRepo::new(pool);
        let id = repo.insert_water(100).await.unwrap();
        let deleted = repo.delete_water(id).await.unwrap();
        assert!(deleted);
        let deleted_again = repo.delete_water(id).await.unwrap();
        assert!(!deleted_again);
        assert_eq!(repo.count_water_since(0).await.unwrap(), 0);
    }

    /// 验证 cleanup_water_older_than 只清理截止时间之前的记录。
    #[tokio::test]
    async fn test_cleanup_water_older_than() {
        let pool = setup_db().await;
        let repo = HealthRepo::new(pool);
        repo.insert_water(100).await.unwrap();
        repo.insert_water(200).await.unwrap();
        repo.insert_water(300).await.unwrap();
        let deleted = repo.cleanup_water_older_than(200).await.unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(repo.count_water_since(0).await.unwrap(), 2);
    }

    /// 验证 insert_rest_record 写入不同 kind 的记录,count_rest_since 按 kind 过滤。
    #[tokio::test]
    async fn test_insert_and_count_rest() {
        let pool = setup_db().await;
        let repo = HealthRepo::new(pool);
        let id1 = repo.insert_rest_record(100, "reminder", 0).await.unwrap();
        let id2 = repo.insert_rest_record(200, "reminder", 0).await.unwrap();
        let id3 = repo.insert_rest_record(300, "rest", 300).await.unwrap();
        assert!(id1 < id2 && id2 < id3); // 自增 id
        assert_eq!(repo.count_rest_since(0, "reminder").await.unwrap(), 2);
        assert_eq!(repo.count_rest_since(0, "rest").await.unwrap(), 1);
        assert_eq!(repo.count_rest_since(150, "reminder").await.unwrap(), 1);
    }

    /// 验证 sum_rest_duration_since 只累加 rest(kind='rest')的 duration,不累加 reminder。
    #[tokio::test]
    async fn test_sum_rest_duration_since() {
        let pool = setup_db().await;
        let repo = HealthRepo::new(pool);
        repo.insert_rest_record(100, "reminder", 0).await.unwrap();
        repo.insert_rest_record(200, "rest", 300).await.unwrap();
        repo.insert_rest_record(300, "rest", 120).await.unwrap();
        assert_eq!(repo.sum_rest_duration_since(0).await.unwrap(), 420);
        assert_eq!(repo.sum_rest_duration_since(250).await.unwrap(), 120);
    }

    /// 验证 get_daily_rest_counts 按 kind 过滤并按本地日分桶。
    #[tokio::test]
    async fn test_get_daily_rest_counts() {
        let pool = setup_db().await;
        let repo = HealthRepo::new(pool);
        repo.insert_rest_record(0, "rest", 100).await.unwrap();
        repo.insert_rest_record(86400, "rest", 100).await.unwrap();
        repo.insert_rest_record(86400, "reminder", 0).await.unwrap();
        repo.insert_rest_record(6 * 86400, "rest", 100)
            .await
            .unwrap();
        let counts = repo.get_daily_rest_counts(0, 7, "rest").await.unwrap();
        assert_eq!(counts.len(), 7);
        assert_eq!(counts[0], 1);
        assert_eq!(counts[1], 1);
        assert_eq!(counts[6], 1);
    }

    /// 验证 delete_rest 按 id 删除,cleanup_rest_older_than 按时间清理。
    #[tokio::test]
    async fn test_delete_and_cleanup_rest() {
        let pool = setup_db().await;
        let repo = HealthRepo::new(pool);
        let id1 = repo.insert_rest_record(100, "rest", 100).await.unwrap();
        let _id2 = repo.insert_rest_record(200, "rest", 100).await.unwrap();
        let _id3 = repo.insert_rest_record(300, "rest", 100).await.unwrap();
        let deleted = repo.delete_rest(id1).await.unwrap();
        assert!(deleted);
        assert_eq!(repo.count_rest_since(0, "rest").await.unwrap(), 2);
        let deleted_missing = repo.delete_rest(99999).await.unwrap();
        assert!(!deleted_missing);
        let cleaned = repo.cleanup_rest_older_than(250).await.unwrap();
        assert_eq!(cleaned, 1);
        assert_eq!(repo.count_rest_since(0, "rest").await.unwrap(), 1);
    }

    /// 验证 get_last_water_ts 在空表/有记录时的返回。
    #[tokio::test]
    async fn test_get_last_water_ts() {
        let pool = setup_db().await;
        let repo = HealthRepo::new(pool);
        assert_eq!(repo.get_last_water_ts().await.unwrap(), None);
        repo.insert_water(100).await.unwrap();
        repo.insert_water(200).await.unwrap();
        assert_eq!(repo.get_last_water_ts().await.unwrap(), Some(200));
    }

    /// 验证按窗口标题聚合只计活跃且非空标题,并按分钟倒序。
    #[tokio::test]
    async fn test_get_window_usage_groups_active_titles() {
        let pool = setup_db().await;
        let repo = HealthRepo::new(pool);
        repo.insert_activity(&ActivityRecord {
            ts: 1000,
            is_active: true,
            is_present: true,
            process_name: Some("Code".into()),
            window_title: Some("main.rs — cc-partner".into()),
        })
        .await
        .unwrap();
        repo.insert_activity(&ActivityRecord {
            ts: 1060,
            is_active: true,
            is_present: true,
            process_name: Some("Code".into()),
            window_title: Some("main.rs — cc-partner".into()),
        })
        .await
        .unwrap();
        repo.insert_activity(&ActivityRecord {
            ts: 1120,
            is_active: true,
            is_present: true,
            process_name: Some("Safari".into()),
            window_title: Some("GitHub".into()),
        })
        .await
        .unwrap();
        repo.insert_activity(&ActivityRecord {
            ts: 1180,
            is_active: false,
            is_present: false,
            process_name: Some("Code".into()),
            window_title: Some("main.rs — cc-partner".into()),
        })
        .await
        .unwrap();
        repo.insert_activity(&ActivityRecord {
            ts: 1240,
            is_active: true,
            is_present: true,
            process_name: Some("Finder".into()),
            window_title: Some("".into()),
        })
        .await
        .unwrap();
        repo.insert_activity(&ActivityRecord {
            ts: 1300,
            is_active: true,
            is_present: true,
            process_name: Some("Finder".into()),
            window_title: None,
        })
        .await
        .unwrap();
        repo.insert_activity(&ActivityRecord {
            ts: 10,
            is_active: true,
            is_present: true,
            process_name: Some("Code".into()),
            window_title: Some("old".into()),
        })
        .await
        .unwrap();

        let usage = repo.get_window_usage(1000).await.unwrap();
        assert_eq!(
            usage,
            vec![("main.rs — cc-partner".into(), 2), ("GitHub".into(), 1),]
        );

        let app_usage = repo.get_app_usage(1000).await.unwrap();
        assert_eq!(app_usage.len(), 3);
        assert!(app_usage.contains(&("Code".into(), 2)));
        assert!(app_usage.contains(&("Finder".into(), 2)));
        assert_eq!(app_usage[2], ("Safari".into(), 1));
        assert!(app_usage[0].1 >= app_usage[1].1);
    }

    #[tokio::test]
    async fn test_habit_records_count_daily_and_cleanup() {
        let pool = setup_db().await;
        let repo = HealthRepo::new(pool);
        repo.insert_habit_record("kegel", 100, "triggered", 0)
            .await
            .unwrap();
        repo.insert_habit_record("kegel", 200, "completed", 30)
            .await
            .unwrap();
        repo.insert_habit_record("water", 200, "completed", 0)
            .await
            .unwrap();
        assert_eq!(
            repo.count_habit_since("kegel", 0, "completed")
                .await
                .unwrap(),
            1
        );
        assert_eq!(repo.sum_habit_duration_since("kegel", 0).await.unwrap(), 30);
        assert_eq!(
            repo.get_last_habit_ts("kegel", "completed").await.unwrap(),
            Some(200)
        );
        let daily = repo
            .get_daily_habit_counts("kegel", 0, 7, "completed")
            .await
            .unwrap();
        assert_eq!(daily.len(), 7);
        assert_eq!(daily[0], 1);
        assert_eq!(repo.cleanup_habit_older_than(150).await.unwrap(), 1);
        assert_eq!(
            repo.count_habit_since("kegel", 0, "triggered")
                .await
                .unwrap(),
            0
        );
    }
}
