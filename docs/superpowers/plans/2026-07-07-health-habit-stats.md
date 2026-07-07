# 健康习惯统计（饮水 / 休息）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 Health 页新增「习惯统计」卡片，展示今日饮水次数、休息次数、总休息时长、近 7 天趋势，并支持手动 +1 杯和删除记录。

**Architecture:** 后端新增 `rest_records` 表存储休息/提醒事件，扩展 `health_repo.rs` 加聚合查询方法，新增 4 个 invoke 命令；前端新建 `HabitStatsCard` 组件用纯 div bar 渲染 sparkline（不依赖 recharts），接入 Health 页现有 30s 轮询。

**Tech Stack:** Rust + sqlx (SQLite in-memory 测试) + axum + tauri 命令；React 19 + TypeScript + CSS Modules + i18next；Node 原生 tsx 测试。

**关联设计文档:** `docs/superpowers/specs/2026-07-07-health-habit-stats-design.md`

---

## 文件结构

### 后端修改

| 文件 | 责任 | 动作 |
|---|---|---|
| `src-tauri/src/lib.rs` | 新增 `REST_SCHEMA` 常量 + init_db 执行 + 注册 4 个新命令 | 修改 |
| `src-tauri/src/storage/health_repo.rs` | 数据访问层，新增 9 个方法（water/rest 的 count/daily/delete/cleanup） | 修改 |
| `src-tauri/src/commands/health.rs` | 命令层，新增 4 命令 + `HabitStatsDto` + 修改 `record_water` 冲突处理 | 修改 |
| `src-tauri/src/health/mod.rs` | 在 `should_remind` 分支插入 reminder 记录 + 扩展跨天清理 | 修改 |

### 前端修改

| 文件 | 责任 | 动作 |
|---|---|---|
| `web/src/lib/types.ts` | 新增 `HabitStats` 类型 | 修改 |
| `web/src/api/health.ts` | 新增 4 个 API 方法 | 修改 |
| `web/src/pages/Health/HabitStatsCard.tsx` | 习惯统计卡片组件 | 新建 |
| `web/src/pages/Health/HabitStatsCard.module.css` | 卡片样式 | 新建 |
| `web/src/pages/Health/HabitStatsCard.test.ts` | 卡片渲染测试 | 新建 |
| `web/src/pages/Health/Health.tsx` | 接入数据 + UI 插入 | 修改 |
| `web/src/pages/Health/Health.module.css` | 卡片容器样式（如需） | 修改 |
| `web/src/pages/HealthOverlay.tsx` | `startRest` 归零后调用 `recordRestCompleted` | 修改 |
| `web/src/i18n/locales/zh/health.json` | 中文文案 | 修改 |
| `web/src/i18n/locales/en/health.json` | 英文文案 | 修改 |
| `web/src/styles/tokens.css` | 复用 `--success`，新增绿色变体 | 修改 |

### 测试文件

| 文件 | 内容 |
|---|---|
| `src-tauri/src/storage/health_repo.rs`（追加测试） | water/rest 聚合查询的单元测试 |
| `src-tauri/src/commands/health.rs`（追加测试） | HabitStatsDto 默认值测试 |
| `web/src/pages/Health/HabitStatsCard.test.ts` | 组件 SSR 渲染断言 |

---

## Task 1: 新增 `rest_records` 表 schema

**Files:**
- Modify: `src-tauri/src/lib.rs`（schema 常量区 166-177 行附近 + init_db 执行 290-292 行附近）

- [ ] **Step 1: 在 `src-tauri/src/lib.rs` 的 `WATER_SCHEMA` 常量后追加 `REST_SCHEMA`**

定位到 `const WATER_SCHEMA` 定义（约 174-177 行），在其后追加：

```rust
/// rest_records 表 schema:记录久坐提醒触发与完成的休息事件,用于习惯统计。
const REST_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS rest_records (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts INTEGER NOT NULL,
    kind TEXT NOT NULL,
    duration_seconds INTEGER NOT NULL DEFAULT 0
)";
```

- [ ] **Step 2: 在 `init_db` 函数的 `WATER_SCHEMA` 执行语句后追加 `REST_SCHEMA` 执行**

定位到 `init_db`（约 260 行起）内 `sqlx::query(WATER_SCHEMA).execute(&pool).await?;`（约 292 行），在其后追加一行：

```rust
    sqlx::query(REST_SCHEMA).execute(&pool).await?;
```

- [ ] **Step 3: 验证编译**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo check 2>&1 | tail -5`
Expected: 编译通过，无错误。

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "feat(health): 新增 rest_records 表 schema"
```

---

## Task 2: `health_repo.rs` 新增饮水统计方法（TDD）

**Files:**
- Modify: `src-tauri/src/storage/health_repo.rs`

参考现有方法位置：`insert_water`（行 194）/ `cleanup_older_than`（行 180）/ `get_hourly_activity`（行 155，本地桶聚合参考）。

- [ ] **Step 1: 先写失败测试 — `count_water_since`**

在 `health_repo.rs` 的 `#[cfg(test)] mod tests`（行 203 起）末尾追加测试：

```rust
    /// 验证 count_water_since 在空表/单条/多条情况下的计数。
    #[tokio::test]
    async fn test_count_water_since() {
        let repo = setup_db().await;
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
```

- [ ] **Step 2: 运行测试验证失败**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo test --lib storage::health_repo::tests::test_count_water_since 2>&1 | tail -10`
Expected: 编译失败，提示 `count_water_since` 方法不存在。

- [ ] **Step 3: 实现 `count_water_since`**

在 `insert_water` 方法（行 194）之后追加：

```rust
    /// 查询 since_ts 之后(含)的饮水次数。
    pub async fn count_water_since(&self, since_ts: i64) -> Result<i64, AppError> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM water_records WHERE ts >= ?")
            .bind(since_ts)
            .fetch_one(&self.db)
            .await?;
        Ok(row.0)
    }
```

- [ ] **Step 4: 运行测试验证通过**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo test --lib storage::health_repo::tests::test_count_water_since 2>&1 | tail -5`
Expected: PASS。

- [ ] **Step 5: 写失败测试 — `get_daily_water_counts`**

在 tests 模块追加：

```rust
    /// 验证 get_daily_water_counts 返回长度恒为 days 的桶,且按本地日正确分桶。
    #[tokio::test]
    async fn test_get_daily_water_counts() {
        let repo = setup_db().await;
        // 假设 since_ts = 0, days = 7
        // 插入若干条不同时间戳的记录
        // 桶 i 的时间范围: [since + i*86400, since + (i+1)*86400)
        repo.insert_water(0).await.unwrap();          // 桶 0
        repo.insert_water(86399).await.unwrap();      // 桶 0
        repo.insert_water(86400).await.unwrap();      // 桶 1
        repo.insert_water(6 * 86400).await.unwrap();  // 桶 6(今日)
        repo.insert_water(7 * 86400).await.unwrap();  // 超出范围,不计
        let counts = repo.get_daily_water_counts(0, 7).await.unwrap();
        assert_eq!(counts.len(), 7);
        assert_eq!(counts[0], 2);
        assert_eq!(counts[1], 1);
        assert_eq!(counts[6], 1);
    }
```

- [ ] **Step 6: 运行测试验证失败**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo test --lib storage::health_repo::tests::test_get_daily_water_counts 2>&1 | tail -10`
Expected: 编译失败，`get_daily_water_counts` 不存在。

- [ ] **Step 7: 实现 `get_daily_water_counts`**

在 `count_water_since` 方法后追加。算法：在 Rust 端用一条 SQL 取出所有 `ts >= since_ts` 的记录，按 `(ts - since_ts) / 86400` 算桶号，落入 `Vec<i64>` 长度 days，超界丢弃：

```rust
    /// 按本地日聚合 since_ts(本地当日 0 点秒级时间戳)之后每日饮水次数。
    /// 返回长度恒为 days 的 Vec,索引 0 = since_ts 当日,索引 days-1 = 今日。
    /// 超出 [since_ts, since_ts + days*86400) 的记录被丢弃。
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
```

- [ ] **Step 8: 运行测试验证通过**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo test --lib storage::health_repo::tests::test_get_daily_water_counts 2>&1 | tail -5`
Expected: PASS。

- [ ] **Step 9: 写失败测试 — `delete_water`**

```rust
    /// 验证 delete_water 删除存在/不存在的记录的返回值。
    #[tokio::test]
    async fn test_delete_water() {
        let repo = setup_db().await;
        repo.insert_water(100).await.unwrap();
        // 删除存在的记录
        let deleted = repo.delete_water(100).await.unwrap();
        assert!(deleted);
        // 再次删除应返回 false
        let deleted_again = repo.delete_water(100).await.unwrap();
        assert!(!deleted_again);
        // 计数为 0
        assert_eq!(repo.count_water_since(0).await.unwrap(), 0);
    }
```

- [ ] **Step 10: 运行测试验证失败**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo test --lib storage::health_repo::tests::test_delete_water 2>&1 | tail -10`
Expected: 编译失败，`delete_water` 不存在。

- [ ] **Step 11: 实现 `delete_water`**

```rust
    /// 删除指定 ts 的饮水记录。返回是否实际删除了记录(true=已删,false=不存在)。
    pub async fn delete_water(&self, ts: i64) -> Result<bool, AppError> {
        let result = sqlx::query("DELETE FROM water_records WHERE ts = ?")
            .bind(ts)
            .execute(&self.db)
            .await?;
        Ok(result.rows_affected() > 0)
    }
```

- [ ] **Step 12: 运行测试验证通过**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo test --lib storage::health_repo::tests::test_delete_water 2>&1 | tail -5`
Expected: PASS。

- [ ] **Step 13: 写失败测试 — `cleanup_water_older_than`**

```rust
    /// 验证 cleanup_water_older_than 只清理截止时间之前的记录。
    #[tokio::test]
    async fn test_cleanup_water_older_than() {
        let repo = setup_db().await;
        repo.insert_water(100).await.unwrap();
        repo.insert_water(200).await.unwrap();
        repo.insert_water(300).await.unwrap();
        let deleted = repo.cleanup_water_older_than(200).await.unwrap();
        assert_eq!(deleted, 1); // 只删 100
        assert_eq!(repo.count_water_since(0).await.unwrap(), 2);
    }
```

- [ ] **Step 14: 运行测试验证失败**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo test --lib storage::health_repo::tests::test_cleanup_water_older_than 2>&1 | tail -10`
Expected: 编译失败。

- [ ] **Step 15: 实现 `cleanup_water_older_than`**

```rust
    /// 清理 ts < cutoff_ts 的饮水记录,返回删除行数。
    pub async fn cleanup_water_older_than(&self, cutoff_ts: i64) -> Result<u64, AppError> {
        let result = sqlx::query("DELETE FROM water_records WHERE ts < ?")
            .bind(cutoff_ts)
            .execute(&self.db)
            .await?;
        Ok(result.rows_affected())
    }
```

- [ ] **Step 16: 运行测试验证通过**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo test --lib storage::health_repo::tests::test_cleanup_water_older_than 2>&1 | tail -5`
Expected: PASS。

- [ ] **Step 17: Commit**

```bash
git add src-tauri/src/storage/health_repo.rs
git commit -m "feat(health-repo): 新增饮水统计查询方法"
```

---

## Task 3: `health_repo.rs` 新增休息统计方法（TDD）

**Files:**
- Modify: `src-tauri/src/storage/health_repo.rs`

**前置:** Task 1（`rest_records` 表）+ Task 2（setup_db 需扩展建 rest_records 表）

- [ ] **Step 1: 扩展 `setup_db` 测试辅助函数,建 `rest_records` 表**

定位 `setup_db`（行 208-224），在 `water_records` 建表语句后追加：

```rust
        sqlx::query("CREATE TABLE IF NOT EXISTS rest_records (id INTEGER PRIMARY KEY AUTOINCREMENT, ts INTEGER NOT NULL, kind TEXT NOT NULL, duration_seconds INTEGER NOT NULL DEFAULT 0)")
            .execute(&pool)
            .await
            .unwrap();
```

- [ ] **Step 2: 写失败测试 — `insert_rest_record` + `count_rest_since`**

```rust
    /// 验证 insert_rest_record 写入不同 kind 的记录,count_rest_since 按 kind 过滤。
    #[tokio::test]
    async fn test_insert_and_count_rest() {
        let repo = setup_db().await;
        // 写入 2 条 reminder + 1 条 rest
        let id1 = repo.insert_rest_record(100, "reminder", 0).await.unwrap();
        let id2 = repo.insert_rest_record(200, "reminder", 0).await.unwrap();
        let id3 = repo.insert_rest_record(300, "rest", 300).await.unwrap();
        assert!(id1 < id2 && id2 < id3); // 自增 id
        // count 按 kind 过滤
        assert_eq!(repo.count_rest_since(0, "reminder").await.unwrap(), 2);
        assert_eq!(repo.count_rest_since(0, "rest").await.unwrap(), 1);
        // since 过滤
        assert_eq!(repo.count_rest_since(150, "reminder").await.unwrap(), 1);
    }
```

- [ ] **Step 3: 运行测试验证失败**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo test --lib storage::health_repo::tests::test_insert_and_count_rest 2>&1 | tail -10`
Expected: 编译失败。

- [ ] **Step 4: 实现 `insert_rest_record` 和 `count_rest_since`**

在 `cleanup_water_older_than` 方法后追加：

```rust
    /// 插入一条休息/提醒事件记录,返回自增 id。
    /// kind 取值: "reminder"(久坐提醒触发) | "rest"(用户完成休息倒计时)。
    /// duration_seconds 仅对 rest 有意义,reminder 传 0。
    pub async fn insert_rest_record(
        &self,
        ts: i64,
        kind: &str,
        duration_seconds: i64,
    ) -> Result<i64, AppError> {
        let row: (i64,) = sqlx::query_as(
            "INSERT INTO rest_records (ts, kind, duration_seconds) VALUES (?, ?, ?) RETURNING id",
        )
        .bind(ts)
        .bind(kind)
        .bind(duration_seconds)
        .fetch_one(&self.db)
        .await?;
        Ok(row.0)
    }

    /// 查询 since_ts 之后指定 kind 的事件次数。
    pub async fn count_rest_since(
        &self,
        since_ts: i64,
        kind: &str,
    ) -> Result<i64, AppError> {
        let row: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM rest_records WHERE ts >= ? AND kind = ?")
                .bind(since_ts)
                .bind(kind)
                .fetch_one(&self.db)
                .await?;
        Ok(row.0)
    }
```

- [ ] **Step 5: 运行测试验证通过**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo test --lib storage::health_repo::tests::test_insert_and_count_rest 2>&1 | tail -5`
Expected: PASS。

- [ ] **Step 6: 写失败测试 — `sum_rest_duration_since`**

```rust
    /// 验证 sum_rest_duration_since 只累加 rest(kind='rest')的 duration,不累加 reminder。
    #[tokio::test]
    async fn test_sum_rest_duration_since() {
        let repo = setup_db().await;
        repo.insert_rest_record(100, "reminder", 0).await.unwrap();
        repo.insert_rest_record(200, "rest", 300).await.unwrap();
        repo.insert_rest_record(300, "rest", 120).await.unwrap();
        // 只累加 rest: 300 + 120 = 420
        assert_eq!(repo.sum_rest_duration_since(0).await.unwrap(), 420);
        // since 过滤
        assert_eq!(repo.sum_rest_duration_since(250).await.unwrap(), 120);
    }
```

- [ ] **Step 7: 运行测试验证失败**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo test --lib storage::health_repo::tests::test_sum_rest_duration_since 2>&1 | tail -10`
Expected: 编译失败。

- [ ] **Step 8: 实现 `sum_rest_duration_since`**

```rust
    /// 查询 since_ts 之后所有完成休息(kind='rest')的总时长秒数。
    pub async fn sum_rest_duration_since(&self, since_ts: i64) -> Result<i64, AppError> {
        let row: (i64,) = sqlx::query_as(
            "SELECT COALESCE(SUM(duration_seconds), 0) FROM rest_records WHERE ts >= ? AND kind = 'rest'",
        )
        .bind(since_ts)
        .fetch_one(&self.db)
        .await?;
        Ok(row.0)
    }
```

- [ ] **Step 9: 运行测试验证通过**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo test --lib storage::health_repo::tests::test_sum_rest_duration_since 2>&1 | tail -5`
Expected: PASS。

- [ ] **Step 10: 写失败测试 — `get_daily_rest_counts`**

```rust
    /// 验证 get_daily_rest_counts 按 kind 过滤并按本地日分桶。
    #[tokio::test]
    async fn test_get_daily_rest_counts() {
        let repo = setup_db().await;
        repo.insert_rest_record(0, "rest", 100).await.unwrap();          // 桶 0
        repo.insert_rest_record(86400, "rest", 100).await.unwrap();      // 桶 1
        repo.insert_rest_record(86400, "reminder", 0).await.unwrap();    // 不同 kind,不计
        repo.insert_rest_record(6 * 86400, "rest", 100).await.unwrap();  // 桶 6
        let counts = repo.get_daily_rest_counts(0, 7, "rest").await.unwrap();
        assert_eq!(counts.len(), 7);
        assert_eq!(counts[0], 1);
        assert_eq!(counts[1], 1);
        assert_eq!(counts[6], 1);
    }
```

- [ ] **Step 11: 运行测试验证失败**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo test --lib storage::health_repo::tests::test_get_daily_rest_counts 2>&1 | tail -10`
Expected: 编译失败。

- [ ] **Step 12: 实现 `get_daily_rest_counts`**

```rust
    /// 按本地日聚合 since_ts 之后每日指定 kind 的事件次数。
    /// 返回长度恒为 days 的 Vec,逻辑同 get_daily_water_counts。
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
```

- [ ] **Step 13: 运行测试验证通过**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo test --lib storage::health_repo::tests::test_get_daily_rest_counts 2>&1 | tail -5`
Expected: PASS。

- [ ] **Step 14: 写失败测试 — `delete_rest` + `cleanup_rest_older_than`**

```rust
    /// 验证 delete_rest 按 id 删除,cleanup_rest_older_than 按时间清理。
    #[tokio::test]
    async fn test_delete_and_cleanup_rest() {
        let repo = setup_db().await;
        let id1 = repo.insert_rest_record(100, "rest", 100).await.unwrap();
        let _id2 = repo.insert_rest_record(200, "rest", 100).await.unwrap();
        let _id3 = repo.insert_rest_record(300, "rest", 100).await.unwrap();
        // 按 id 删除
        let deleted = repo.delete_rest(id1).await.unwrap();
        assert!(deleted);
        assert_eq!(repo.count_rest_since(0, "rest").await.unwrap(), 2);
        // 删不存在的 id
        let deleted_missing = repo.delete_rest(99999).await.unwrap();
        assert!(!deleted_missing);
        // 按时间清理
        let cleaned = repo.cleanup_rest_older_than(250).await.unwrap();
        assert_eq!(cleaned, 1); // 只删 ts=200 那条
        assert_eq!(repo.count_rest_since(0, "rest").await.unwrap(), 1);
    }
```

- [ ] **Step 15: 运行测试验证失败**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo test --lib storage::health_repo::tests::test_delete_and_cleanup_rest 2>&1 | tail -10`
Expected: 编译失败。

- [ ] **Step 16: 实现 `delete_rest` 和 `cleanup_rest_older_than`**

```rust
    /// 按 id 删除一条 rest_records 记录。返回是否实际删除。
    pub async fn delete_rest(&self, id: i64) -> Result<bool, AppError> {
        let result = sqlx::query("DELETE FROM rest_records WHERE id = ?")
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 清理 ts < cutoff_ts 的休息记录,返回删除行数。
    pub async fn cleanup_rest_older_than(&self, cutoff_ts: i64) -> Result<u64, AppError> {
        let result = sqlx::query("DELETE FROM rest_records WHERE ts < ?")
            .bind(cutoff_ts)
            .execute(&self.db)
            .await?;
        Ok(result.rows_affected())
    }
```

- [ ] **Step 17: 运行测试验证通过**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo test --lib storage::health_repo::tests::test_delete_and_cleanup_rest 2>&1 | tail -5`
Expected: PASS。

- [ ] **Step 18: 运行全部 health_repo 测试,确保无回归**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo test --lib storage::health_repo 2>&1 | tail -15`
Expected: 所有测试通过（原有的 test_insert_and_aggregate / test_cleanup / test_water_record / test_hourly_activity_local_buckets 也应通过）。

- [ ] **Step 19: 移除 `insert_water` 上的 `#[allow(dead_code)]`**

定位 `insert_water`（约 193 行），删除其上方的 `#[allow(dead_code)]` 注解（该命令现在被 `record_water` 命令消费，不再是死代码）。

- [ ] **Step 20: Commit**

```bash
git add src-tauri/src/storage/health_repo.rs
git commit -m "feat(health-repo): 新增休息统计查询方法 + 9 个聚合方法"
```

---

## Task 4: `commands/health.rs` 新增 `HabitStatsDto` + 4 个命令

**Files:**
- Modify: `src-tauri/src/commands/health.rs`

参考现有命令：`record_water`（行 300）/ `get_activity_stats`（行 257）。

- [ ] **Step 1: 在 `commands/health.rs` 的 DTO 区追加 `HabitStatsDto`**

定位 `ActivityDetailDto`（行 117 附近），在其后追加：

```rust
/// 习惯统计返回:饮水 + 休息聚合,前端 HabitStatsCard 一次拉取所需数据。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HabitStatsDto {
    /// 今日饮水次数(本地当日 0 点起)。
    pub today_water_count: i64,
    /// 近 N 天(默认 7)每日饮水次数,索引 0 = N-1 天前,末位 = 今日。
    pub water_daily_counts: Vec<i64>,
    /// 距今最近一次饮水时间戳(Unix 秒),用于"距下次提醒"计算。无记录则 None。
    pub last_water_ts: Option<i64>,
    /// 今日完成休息次数。
    pub today_rest_count: i64,
    /// 今日完成休息总时长秒数。
    pub today_rest_total_seconds: i64,
    /// 今日久坐提醒触发次数。
    pub today_reminder_count: i64,
    /// 近 N 天每日完成休息次数。
    pub rest_daily_counts: Vec<i64>,
}
```

- [ ] **Step 2: 在 `commands/health.rs` 末尾(测试模块之前)追加 4 个命令**

定位 `close_health_overlay` 命令（约行 360），在其后、`#[cfg(test)]` 模块之前追加：

```rust
/// 用户在习惯统计卡片点「+1 杯」手动加计饮水。返回新插入的时间戳。
/// 与 reminder 触发的 record_water 共用 water_records 表,语义一致。
#[tauri::command]
pub async fn add_water_manual(state: State<'_, AppState>) -> Result<i64, AppError> {
    let now = chrono::Utc::now().timestamp();
    state.health_repo.insert_water(now).await?;
    // 重置喝水状态机的 last_drink_ts,避免紧接着弹喝水提醒
    {
        let mut w = state.health.water.lock().unwrap();
        w.last_drink_ts = now;
        w.pending_remind = false;
    }
    Ok(now)
}

/// 撤销误点的饮水记录。返回是否实际删除了记录。
#[tauri::command]
pub async fn delete_water_record(state: State<'_, AppState>, ts: i64) -> Result<bool, AppError> {
    state.health_repo.delete_water(ts).await
}

/// 用户完成休息倒计时后调用,记录一次完整休息(kind='rest')。
/// duration 取配置的 break_seconds(与前端倒计时口径一致)。
#[tauri::command]
pub async fn record_rest_completed(state: State<'_, AppState>) -> Result<(), AppError> {
    let now = chrono::Utc::now().timestamp();
    let duration = state.config.read().await.health.break_seconds;
    state.health_repo.insert_rest_record(now, "rest", duration).await?;
    Ok(())
}

/// 一次性返回习惯统计(饮水 + 休息聚合),减少前端多次 invoke。
/// days 参数控制趋势窗口,默认 7。
#[tauri::command]
pub async fn get_habit_stats(
    state: State<'_, AppState>,
    days: Option<i64>,
) -> Result<HabitStatsDto, AppError> {
    let days = days.unwrap_or(7).max(1) as usize;
    let now = chrono::Utc::now().timestamp();
    // 今日起点:UTC 当日 0 点(前端展示时按本地时区口径,后端只算近似 UTC 0 点)
    let today_start = now - now.rem_euclid(86400);
    let trend_start = today_start - ((days as i64) - 1) * 86400;

    let today_water_count = state.health_repo.count_water_since(today_start).await?;
    let water_daily_counts = state
        .health_repo
        .get_daily_water_counts(trend_start, days)
        .await?;
    let last_water_ts = state.health_repo.get_last_water_ts().await?;
    let today_rest_count = state.health_repo.count_rest_since(today_start, "rest").await?;
    let today_rest_total_seconds = state
        .health_repo
        .sum_rest_duration_since(today_start)
        .await?;
    let today_reminder_count = state
        .health_repo
        .count_rest_since(today_start, "reminder")
        .await?;
    let rest_daily_counts = state
        .health_repo
        .get_daily_rest_counts(trend_start, days, "rest")
        .await?;

    Ok(HabitStatsDto {
        today_water_count,
        water_daily_counts,
        last_water_ts,
        today_rest_count,
        today_rest_total_seconds,
        today_reminder_count,
        rest_daily_counts,
    })
}
```

> **注意**:`get_habit_stats` 里用了 `state.health_repo.get_last_water_ts()` 方法,该方法在 Step 4 创建。Step 顺序是:Step 1 加 DTO → Step 2 加命令(此时引用未定义方法) → Step 3 cargo check 报错 → Step 4 在 health_repo.rs 加方法 → 再次 check 通过。这是预期的 TDD 节奏。

- [ ] **Step 3: 验证编译(预期失败 — `get_last_water_ts` 未定义)**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo check 2>&1 | tail -10`
Expected: 编译失败,提示 `get_last_water_ts` 方法不存在。这是预期的——Step 4 会补上。

- [ ] **Step 4: 在 health_repo.rs 新增 `get_last_water_ts` 方法**

在 `cleanup_water_older_than` 方法后追加:

```rust
    /// 查询最近一次饮水时间戳(Unix 秒)。表空返回 None。
    pub async fn get_last_water_ts(&self) -> Result<Option<i64>, AppError> {
        let row: Option<(Option<i64>,)> =
            sqlx::query_as("SELECT MAX(ts) FROM water_records")
                .fetch_optional(&self.db)
                .await?;
        Ok(row.and_then(|(opt,)| opt))
    }
```

同时为该方法写一个单元测试,加在 tests 模块:

```rust
    /// 验证 get_last_water_ts 在空表/有记录时的返回。
    #[tokio::test]
    async fn test_get_last_water_ts() {
        let repo = setup_db().await;
        // 空表
        assert_eq!(repo.get_last_water_ts().await.unwrap(), None);
        repo.insert_water(100).await.unwrap();
        repo.insert_water(200).await.unwrap();
        // 返回最大 ts
        assert_eq!(repo.get_last_water_ts().await.unwrap(), Some(200));
    }
```

- [ ] **Step 5: 再次 cargo check + 跑新测试**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo check 2>&1 | tail -5 && cargo test --lib storage::health_repo::tests::test_get_last_water_ts 2>&1 | tail -5`
Expected: 编译通过,测试 PASS。

- [ ] **Step 6: 在 lib.rs invoke_handler 注册 4 新命令**

定位 `src-tauri/src/lib.rs` 的 health 注册块（行 656-670），把注释从「14 命令」改为「18 命令」,并在 `health_cmd::close_health_overlay,` 后追加:

```rust
            health_cmd::add_water_manual,
            health_cmd::delete_water_record,
            health_cmd::record_rest_completed,
            health_cmd::get_habit_stats,
```

- [ ] **Step 7: 验证编译 + 注册成功**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo check 2>&1 | tail -5`
Expected: 编译通过。

- [ ] **Step 8: 命令本体测试说明**

跳过命令本体的集成测试(需要 mock AppState,成本高)。聚合逻辑的正确性由 health_repo.rs 的单元测试保证,命令层只做组装,出错率低。

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/commands/health.rs src-tauri/src/lib.rs src-tauri/src/storage/health_repo.rs
git commit -m "feat(health-cmd): 新增习惯统计 4 命令 + HabitStatsDto"
```

---

## Task 5: `health/mod.rs` 在 reminder 触发点 + 跨天清理扩展

**Files:**
- Modify: `src-tauri/src/health/mod.rs`

参考现有代码：`should_remind` 消费分支（行 148-173）/ 跨天清理（行 199-203）。

- [ ] **Step 1: 在 `should_remind` 分支内追加 reminder 记录写入**

定位 `if should_remind {`（约行 151），在 `if !snoozed && !dnd {` 块**之前**追加（无论是否被 DND/贪睡静默都记录,因为提醒确实触发了,只是没弹给用户）:

```rust
    if should_remind {
        // 记录 reminder 触发事件(用于习惯统计),无论是否被静默都算一次触发。
        if let Err(e) = state.health_repo.insert_rest_record(now, "reminder", 0).await {
            tracing::warn!("写入 reminder 统计记录失败: {e}");
        }
        // 贪睡未到期则静默;免打扰时段静默 ...
        let snoozed = state.health.snooze_until.lock().unwrap().is_some_and(|t| t > now);
        // ... 原有逻辑保持不变
```

> **设计决策**:记录点放在 DND/snooze 静默门**之前**,即"提醒触发就计 1 次"。这与设计文档 §2「`kind='reminder'`:状态机判定 should_remind=true 时记录一次」一致。如果用户希望"DND 期间不计",可改为放在 `if !snoozed && !dnd` 块内——这是产品决策,默认按"触发即计"。

- [ ] **Step 2: 扩展跨天清理,加 water + rest 表清理**

定位跨天清理块（约行 199-203）：

```rust
            // 数据清理(DELETE 幂等,成本低;每次跑可优化为跨天清理)
            let cutoff = now - cfg.retain_days * 86400;
            if let Err(e) = state.health_repo.cleanup_older_than(cutoff).await {
                tracing::warn!("清理过期活动记录失败: {e}");
            }
```

在 `cleanup_older_than` 调用后追加两条:

```rust
            if let Err(e) = state.health_repo.cleanup_water_older_than(cutoff).await {
                tracing::warn!("清理过期饮水记录失败: {e}");
            }
            if let Err(e) = state.health_repo.cleanup_rest_older_than(cutoff).await {
                tracing::warn!("清理过期休息记录失败: {e}");
            }
```

- [ ] **Step 3: 验证编译**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo check 2>&1 | tail -5`
Expected: 编译通过。

- [ ] **Step 4: 运行全部 cargo test 确认无回归**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo test --lib 2>&1 | tail -15`
Expected: 所有测试通过。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/health/mod.rs
git commit -m "feat(health-daemon): reminder 触发记录入库 + 扩展跨天清理"
```

---

## Task 6: 前端类型 + API 封装

**Files:**
- Modify: `web/src/lib/types.ts`
- Modify: `web/src/api/health.ts`

- [ ] **Step 1: 在 `web/src/lib/types.ts` 追加 `HabitStats` 类型**

定位 `ActivityDetail`（行 1319 附近），在其后追加:

```ts
/** 习惯统计(饮水 + 休息)后端返回,对应 HabitStatsDto。 */
export interface HabitStats {
  /** 今日饮水次数。 */
  todayWaterCount: number;
  /** 近 N 天每日饮水次数,索引 0 = N-1 天前,末位 = 今日。 */
  waterDailyCounts: number[];
  /** 距今最近一次饮水时间戳(Unix 秒),无则 undefined。 */
  lastWaterTs?: number;
  /** 今日完成休息次数。 */
  todayRestCount: number;
  /** 今日完成休息总时长秒数。 */
  todayRestTotalSeconds: number;
  /** 今日久坐提醒触发次数。 */
  todayReminderCount: number;
  /** 近 N 天每日完成休息次数。 */
  restDailyCounts: number[];
}
```

- [ ] **Step 2: 在 `web/src/api/health.ts` 的 `healthApi` 对象追加 4 方法**

定位 `closeOverlay` 方法（行 66 附近），在其后、对象闭合 `}` 之前追加:

```ts
  getHabitStats: (days?: number) => invoke<HabitStats>('get_habit_stats', { days }),
  addWaterManual: () => invoke<number>('add_water_manual'),
  deleteWaterRecord: (ts: number) => invoke<boolean>('delete_water_record', { ts }),
  recordRestCompleted: () => invoke<void>('record_rest_completed'),
```

确认 `HabitStats` 已从 `@/lib/types` 导入（文件顶部应有 `import type { ... } from '@/lib/types'`,把 `HabitStats` 加入导入列表）。

- [ ] **Step 3: 类型检查**

Run: `cd /Users/hans/web_project/cc-partner/web && npx tsc --noEmit 2>&1 | tail -10`
Expected: 无错误。

- [ ] **Step 4: Commit**

```bash
git add web/src/lib/types.ts web/src/api/health.ts
git commit -m "feat(web): 新增 HabitStats 类型 + 4 个 healthApi 方法"
```

---

## Task 7: 新建 `HabitStatsCard` 组件 + 样式 + 测试

**Files:**
- Create: `web/src/pages/Health/HabitStatsCard.tsx`
- Create: `web/src/pages/Health/HabitStatsCard.module.css`
- Create: `web/src/pages/Health/HabitStatsCard.test.ts`

- [ ] **Step 1: 创建 `HabitStatsCard.module.css`**

```css
.card {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  padding: var(--space-5);
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  background: var(--surface);
}

.title {
  margin: 0;
  font-family: var(--font-display);
  font-size: var(--text-md);
  font-weight: var(--weight-semibold);
  color: var(--fg);
}

.row {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: var(--space-5);
}

.block {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
  padding: var(--space-4);
  border: 1px solid var(--border-soft);
  border-radius: var(--radius-md);
  background: color-mix(in oklab, var(--surface) 78%, transparent);
}

.blockHead {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.blockLabel {
  font-size: var(--text-xs);
  color: var(--meta);
}

.addBtn {
  background: transparent;
  border: 1px solid var(--accent);
  color: var(--accent);
  font-size: var(--text-xs);
  padding: var(--space-1) var(--space-3);
  border-radius: var(--radius-full);
  cursor: pointer;
  transition: all var(--motion-fast) var(--ease-standard);
}

.addBtn:hover:not(:disabled) {
  background: var(--accent);
  color: var(--accent-on);
}

.addBtn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.numLine {
  display: flex;
  align-items: baseline;
  gap: var(--space-2);
}

.numBig {
  font-family: var(--font-display);
  font-size: var(--text-3xl);
  font-weight: var(--weight-semibold);
  color: var(--accent);
  line-height: 1;
  font-variant-numeric: tabular-nums;
}

.numBig.rest {
  color: var(--success);
}

.numUnit {
  font-size: var(--text-xs);
  color: var(--meta);
}

.numSub {
  font-size: var(--text-xs);
  color: var(--meta);
}

.numSub b {
  color: var(--fg);
  font-weight: var(--weight-medium);
}

.numSub .pip {
  color: var(--accent);
  cursor: help;
  border-bottom: 1px dotted var(--meta);
}

.week {
  margin-top: var(--space-2);
}

.weekBars {
  display: flex;
  align-items: flex-end;
  gap: var(--space-1);
  height: 32px;
}

.bar {
  flex: 1;
  background: color-mix(in oklab, var(--accent) 50%, transparent);
  border-radius: var(--radius-xs) var(--radius-xs) 0 0;
  min-height: 3px;
  transition: height var(--motion-fast) var(--ease-standard);
}

.bar.today {
  background: var(--accent);
}

.bar.rest {
  background: color-mix(in oklab, var(--success) 50%, transparent);
}

.bar.rest.today {
  background: var(--success);
}

.weekLabels {
  display: flex;
  gap: var(--space-1);
  margin-top: var(--space-1);
}

.weekLabels span {
  flex: 1;
  text-align: center;
  font-size: 10px;
  color: var(--meta);
}

.weekLabels span.today {
  color: var(--accent);
  font-weight: var(--weight-medium);
}

.weekLabels span.today.rest {
  color: var(--success);
}

.footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding-top: var(--space-3);
  border-top: 1px dashed var(--border-soft);
  font-size: var(--text-xs);
  color: var(--meta);
}

.footerLink {
  color: var(--accent);
  text-decoration: none;
  cursor: pointer;
}

.footerLink:hover {
  text-decoration: underline;
}

@media (max-width: 880px) {
  .row {
    grid-template-columns: 1fr;
  }
}
```

- [ ] **Step 2: 创建 `HabitStatsCard.tsx`**

```tsx
import { useState, useCallback } from 'react';
import type { HabitStats } from '@/lib/types';
import { healthApi } from '@/api/health';
import { useTranslation } from 'react-i18next';
import styles from './HabitStatsCard.module.css';

/**
 * Business Logic(为什么需要):
 *   用户在 Health 页一眼看到今日饮水/休息次数 + 近 7 天趋势,
 *   形成"习惯打卡"反馈闭环,激励坚持喝水和定时休息。
 *
 * Code Logic(做什么):
 *   展示 HabitStats 数据:两栏(饮水/休息),每栏大数字 + 小字 + 7 柱 sparkline。
 *   饮水栏含"+1 杯"按钮(带 500ms 节流防同秒主键冲突)。
 *   底部"查看历史记录"链接(暂为占位,P1 增量做删除 UI)。
 */
interface HabitStatsCardProps {
  stats: HabitStats | null;
  waterEnabled: boolean;
  waterIntervalSeconds: number;
  retainDays: number;
  onWaterAdded: () => void;
}

/** 计算"距下次提醒"剩余分钟数。返回 null 表示无需展示(饮水禁用)。 */
function computeNextWaterMinutes(
  lastWaterTs: number | undefined,
  interval: number,
  nowSec: number,
): number | null {
  const base = lastWaterTs ?? nowSec;
  const remaining = base + interval - nowSec;
  return Math.max(0, Math.ceil(remaining / 60));
}

export function HabitStatsCard({
  stats,
  waterEnabled,
  waterIntervalSeconds,
  retainDays,
  onWaterAdded,
}: HabitStatsCardProps): JSX.Element {
  const { t } = useTranslation('health');
  const [adding, setAdding] = useState(false);

  const handleAddWater = useCallback(async () => {
    if (adding) return;
    setAdding(true);
    try {
      await healthApi.addWaterManual();
      onWaterAdded();
    } catch (e) {
      console.error('手动加计饮水失败', e);
    } finally {
      // 500ms 节流,防止同秒 ts 主键冲突
      setTimeout(() => setAdding(false), 500);
    }
  }, [adding, onWaterAdded]);

  if (!stats) {
    return (
      <section className={styles.card}>
        <p className={styles.blockLabel}>{t('habitStatsTitle')}</p>
        <p className={styles.blockLabel}>{t('noData')}</p>
      </section>
    );
  }

  const nowSec = Math.floor(Date.now() / 1000);
  const nextWaterMin = waterEnabled
    ? computeNextWaterMinutes(stats.lastWaterTs, waterIntervalSeconds, nowSec)
    : null;
  const restTotalMinutes = Math.round(stats.todayRestTotalSeconds / 60);

  return (
    <section className={styles.card}>
      <h3 className={styles.title}>{t('habitStatsTitle')}</h3>
      <div className={styles.row}>
        {/* 饮水栏 */}
        <div className={styles.block}>
          <div className={styles.blockHead}>
            <span className={styles.blockLabel}>💧 {t('todayWater')}</span>
            <button
              className={styles.addBtn}
              onClick={handleAddWater}
              disabled={adding}
              aria-label={t('addCup')}
            >
              {t('addCup')}
            </button>
          </div>
          <div className={styles.numLine}>
            <span className={styles.numBig}>{stats.todayWaterCount}</span>
            <span className={styles.numUnit}>{t('cup')}</span>
          </div>
          {nextWaterMin !== null && (
            <div className={styles.numSub}>
              {nextWaterMin > 0
                ? t('nextWaterIn', { n: nextWaterMin })
                : t('waterOverdue')}
            </div>
          )}
          <WeekBars
            counts={stats.waterDailyCounts}
            todayIndex={stats.waterDailyCounts.length - 1}
          />
        </div>

        {/* 休息栏 */}
        <div className={styles.block}>
          <div className={styles.blockHead}>
            <span className={styles.blockLabel}>🌿 {t('todayRest')}</span>
          </div>
          <div className={styles.numLine}>
            <span className={styles.numBig + ' ' + styles.rest}>{stats.todayRestCount}</span>
            <span className={styles.numUnit}>{t('times')}</span>
          </div>
          <div className={styles.numSub}>
            {t('totalRestMinutes', { n: restTotalMinutes })} ·{' '}
            <span className={styles.pip} title={t('reminderTimesToday', { n: stats.todayReminderCount })}>
              {t('reminderTimesToday', { n: stats.todayReminderCount })}
            </span>
          </div>
          <WeekBars
            counts={stats.restDailyCounts}
            todayIndex={stats.restDailyCounts.length - 1}
            variant="rest"
          />
        </div>
      </div>
      <div className={styles.footer}>
        <span>{t('habitFooter', { n: retainDays })}</span>
        <span className={styles.footerLink}>{t('viewHistory')}</span>
      </div>
    </section>
  );
}

/** 7 柱 sparkline 子组件,用纯 div bar 渲染(数据量小,不引入 recharts)。 */
interface WeekBarsProps {
  counts: number[];
  todayIndex: number;
  variant?: 'water' | 'rest';
}

function WeekBars({ counts, todayIndex, variant = 'water' }: WeekBarsProps): JSX.Element {
  const { t } = useTranslation('health');
  const max = Math.max(1, ...counts);
  const labels = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
  return (
    <div className={styles.week}>
      <div className={styles.weekBars}>
        {counts.map((c, i) => {
          const heightPct = c === 0 ? 0 : (c / max) * 100;
          const cls = [
            styles.bar,
            variant === 'rest' ? styles.rest : '',
            i === todayIndex ? styles.today : '',
          ]
            .filter(Boolean)
            .join(' ');
          return <div key={i} className={cls} style={{ height: `${heightPct}%` }} />;
        })}
      </div>
      <div className={styles.weekLabels}>
        {labels.map((label, i) => (
          <span
            key={label}
            className={
              i === todayIndex
                ? styles.today + (variant === 'rest' ? ' ' + styles.rest : '')
                : ''
            }
          >
            {i === todayIndex ? t('today') : t(label)}
          </span>
        ))}
      </div>
    </div>
  );
}
```

- [ ] **Step 3: 创建 `HabitStatsCard.test.ts`**

参考 `web/src/pages/Settings/HealthPanel.test.ts` 的 SSR + 正则断言模式。

```ts
import { register } from 'node:module';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import type { HabitStats } from '../../lib/types';

register('./css-stub.mjs', import.meta.url);

const { default: i18n } = await import('../../i18n');
await i18n.changeLanguage('zh');

const { HabitStatsCard } = await import('./HabitStatsCard');

const sampleStats: HabitStats = {
  todayWaterCount: 5,
  waterDailyCounts: [3, 6, 2, 5, 8, 4, 5],
  lastWaterTs: Math.floor(Date.now() / 1000) - 600,
  todayRestCount: 3,
  todayRestTotalSeconds: 720,
  todayReminderCount: 4,
  restDailyCounts: [2, 3, 1, 4, 5, 2, 3],
};

const rendered = renderToStaticMarkup(
  createElement(HabitStatsCard, {
    stats: sampleStats,
    waterEnabled: true,
    waterIntervalSeconds: 3600,
    retainDays: 90,
    onWaterAdded: () => undefined,
  }),
);

// 断言 1: 标题渲染
if (!rendered.includes('习惯统计')) {
  throw new Error('HabitStatsCard missing title');
}

// 断言 2: 今日饮水数字渲染
if (!rendered.includes('>5<')) {
  throw new Error('HabitStatsCard missing today water count');
}

// 断言 3: 7 柱 sparkline 渲染(应有 7 个 bar div)
const waterBars = rendered.match(/bar/g) ?? [];
if (waterBars.length < 14) {
  // 饮水 7 柱 + 休息 7 柱 = 至少 14 次 class 命中
  throw new Error(`HabitStatsCard expected >=14 bar classes, got ${waterBars.length}`);
}

// 断言 4: +1 杯按钮渲染
if (!rendered.includes('+1 杯')) {
  throw new Error('HabitStatsCard missing add cup button');
}

console.log('✓ HabitStatsCard renders correctly');
```

- [ ] **Step 4: 复制 CSS stub loader(若 Health 目录还没有)**

检查 `web/src/pages/Health/css-stub.mjs` 是否存在,不存在则从 `web/src/pages/Settings/css-stub.mjs` 复制:

Run: `cp /Users/hans/web_project/cc-partner/web/src/pages/Settings/css-stub.mjs /Users/hans/web_project/cc-partner/web/src/pages/Health/css-stub.mjs`

- [ ] **Step 5: 运行测试**

Run: `cd /Users/hans/web_project/cc-partner/web && npx tsx src/pages/Health/HabitStatsCard.test.ts 2>&1 | tail -10`
Expected: 输出 `✓ HabitStatsCard renders correctly`,退出码 0。

- [ ] **Step 6: 类型检查**

Run: `cd /Users/hans/web_project/cc-partner/web && npx tsc --noEmit 2>&1 | tail -10`
Expected: 无错误。

- [ ] **Step 7: Commit**

```bash
git add web/src/pages/Health/HabitStatsCard.tsx web/src/pages/Health/HabitStatsCard.module.css web/src/pages/Health/HabitStatsCard.test.ts web/src/pages/Health/css-stub.mjs
git commit -m "feat(web): 新增 HabitStatsCard 组件 + 测试"
```

---

## Task 8: `Health.tsx` 接入习惯统计

**Files:**
- Modify: `web/src/pages/Health/Health.tsx`
- Modify: `web/src/pages/Health/Health.module.css`（如需容器样式）

参考现有代码：`refresh`（行 129-141）/ `metricGrid`（行 263-284）/ 图表面板（行 288-300）/ `getLocalStartOfDayTs`（行 88-92）。

- [ ] **Step 1: 在 Health.tsx 导入 `HabitStatsCard` 和 `HabitStats` 类型**

在文件顶部 import 区追加:

```tsx
import { HabitStatsCard } from './HabitStatsCard';
import type { HabitStats } from '@/lib/types';
```

- [ ] **Step 2: 在 Health.tsx 的 state 区新增 `habitStats` state**

定位现有 state 声明(约行 80-100),追加:

```tsx
const [habitStats, setHabitStats] = useState<HabitStats | null>(null);
```

> **⚠️ 注意 React hooks 规则(AGENTS.md #20)**:`useState` 必须放在所有 early return(loading/error/空态守卫)**之前**。Health.tsx 现有代码里 state 声明都在 return 之前,把 `habitStats` 加到同一区域即可。实施时先 Read Health.tsx 顶部 50 行,找到现有 `useState` 集中区,追加进去。**禁止**把 `useState` 放到 `if (loading) return ...` 之后。

- [ ] **Step 3: 在 `refresh` 函数的 `Promise.all` 追加 `getHabitStats`**

定位 `refresh`(约行 129-141),把现有的 `Promise.all` 扩展为 4 路:

```tsx
  const refresh = useCallback(async () => {
    const startOfDay = getLocalStartOfDayTs();
    const [status, stats, detail, habit] = await Promise.all([
      healthApi.getStatus(),
      healthApi.getStats(startOfDay),
      healthApi.getDetail(startOfDay),
      healthApi.getHabitStats(7),
    ]);
    setStatus(status);
    setStats(stats);
    setDetail(detail);
    setHabitStats(habit);
  }, []);
```

> **注意**:现有 refresh 函数的依赖数组与 `Promise.all` 结构可能略有差异,需保留原有 try/catch 和错误处理逻辑,只在 Promise.all 内追加一路。Step 实施时先 Read 现有代码再合并。

- [ ] **Step 4: 在 Health.tsx 的 JSX 中插入 `<HabitStatsCard>`**

定位图表面板(`<div className={styles.chartCard}>` 约行 288)之前、指标网格所在的 Card(`overviewCard`)之后,插入:

```tsx
        <HabitStatsCard
          stats={habitStats}
          waterEnabled={config?.waterEnabled ?? true}
          waterIntervalSeconds={config?.waterIntervalSeconds ?? 3600}
          retainDays={config?.retainDays ?? 90}
          onWaterAdded={refresh}
        />
```

> `config` 来自现有 `getConfig` state。确认变量名与现有代码一致(可能是 `config` 或 `healthConfig`)。

- [ ] **Step 5: 类型检查**

Run: `cd /Users/hans/web_project/cc-partner/web && npx tsc --noEmit 2>&1 | tail -10`
Expected: 无错误。

- [ ] **Step 6: Commit**

```bash
git add web/src/pages/Health/Health.tsx web/src/pages/Health/Health.module.css
git commit -m "feat(web): Health 页接入习惯统计卡片"
```

---

## Task 9: `HealthOverlay.tsx` 完成休息后记录

**Files:**
- Modify: `web/src/pages/HealthOverlay.tsx`

参考现有代码：`startRest`（行 80-105），倒计时归零后的命令调用（行 92-99）。

- [ ] **Step 1: 在 `startRest` 的倒计时归零回调追加 `recordRestCompleted`**

定位 `startRest` 函数(行 80-105),把归零后的 async 块扩展:

```tsx
          (async () => {
            try {
              await healthApi.skip();
              await healthApi.recordRestCompleted();
            } catch (e) {
              console.error('休息结束记录失败', e);
            }
            await healthApi.closeOverlay();
          })();
```

> **说明**:`recordRestCompleted` 放在 `skip` 之后、`closeOverlay` 之前。失败仅 console.error 不阻断(与现有 `skip` 失败处理一致)。`skip` 重置状态机回 Idle,`recordRestCompleted` 记录一条 rest 事件,两者独立。

- [ ] **Step 2: 类型检查**

Run: `cd /Users/hans/web_project/cc-partner/web && npx tsc --noEmit 2>&1 | tail -10`
Expected: 无错误。

- [ ] **Step 3: Commit**

```bash
git add web/src/pages/Health/HealthOverlay.tsx
git commit -m "feat(web): 休息倒计时完成后记录 rest 事件"
```

---

## Task 10: i18n 文案

**Files:**
- Modify: `web/src/i18n/locales/zh/health.json`
- Modify: `web/src/i18n/locales/en/health.json`

- [ ] **Step 1: 在 `zh/health.json` 追加新 key**

在 JSON 对象末尾(`}` 之前)追加(注意逗号):

```json
  "habitStatsTitle": "习惯统计",
  "todayWater": "今日饮水",
  "todayRest": "今日休息",
  "cup": "杯",
  "times": "次",
  "totalRestMinutes": "总休息 {{n}} 分钟",
  "reminderTimesToday": "提醒 {{n}} 次",
  "nextWaterIn": "距下次提醒 · 还有 {{n}} 分钟",
  "waterOverdue": "已超时,等待提醒",
  "addCup": "+1 杯",
  "viewHistory": "查看历史记录",
  "habitFooter": "数据保留 {{n}} 天",
  "today": "今",
  "Mon": "一",
  "Tue": "二",
  "Wed": "三",
  "Thu": "四",
  "Fri": "五",
  "Sat": "六",
  "Sun": "日"
```

- [ ] **Step 2: 在 `en/health.json` 追加对应英文 key**

```json
  "habitStatsTitle": "Habit Stats",
  "todayWater": "Today's Water",
  "todayRest": "Today's Rest",
  "cup": "cups",
  "times": "times",
  "totalRestMinutes": "Total rest {{n}} min",
  "reminderTimesToday": "Reminders: {{n}}",
  "nextWaterIn": "Next reminder in {{n}} min",
  "waterOverdue": "Overdue, awaiting reminder",
  "addCup": "+1 cup",
  "viewHistory": "View history",
  "habitFooter": "Data retained for {{n}} days",
  "today": "Today",
  "Mon": "Mon",
  "Tue": "Tue",
  "Wed": "Wed",
  "Thu": "Thu",
  "Fri": "Fri",
  "Sat": "Sat",
  "Sun": "Sun"
```

- [ ] **Step 3: 验证 JSON 合法性**

Run: `cd /Users/hans/web_project/cc-partner && node -e "JSON.parse(require('fs').readFileSync('web/src/i18n/locales/zh/health.json','utf8')); JSON.parse(require('fs').readFileSync('web/src/i18n/locales/en/health.json','utf8')); console.log('✓ JSON valid')"`
Expected: 输出 `✓ JSON valid`。

- [ ] **Step 4: 重新跑前端测试确认 i18n key 解析正常**

Run: `cd /Users/hans/web_project/cc-partner/web && npx tsx src/pages/Health/HabitStatsCard.test.ts 2>&1 | tail -5`
Expected: 测试通过(标题"习惯统计"和"+1 杯"被正确解析)。

- [ ] **Step 5: Commit**

```bash
git add web/src/i18n/locales/zh/health.json web/src/i18n/locales/en/health.json
git commit -m "feat(i18n): 新增习惯统计中文/英文文案"
```

---

## Task 11: tokens.css 绿色变体(按需)

**Files:**
- Modify: `web/src/styles/tokens.css`

> **决策**:休息色复用现有 `--success`(浅色 `#17a34a` / 深色 `#4ade80`),**无需新增 token**。CSS Module 里 `color: var(--success)` 直接引用。
>
> 本任务**仅在视觉对比不足时**追加变体。如果实施时发现 `--success` 在浅色卡片底色上不够柔和(对比度过高),再追加 `--success-soft`。

- [ ] **Step 1: 实施时目视检查 Health 页**

启动 `./node_modules/.bin/tauri dev`,打开 `/health`,观察习惯统计卡片:
- 饮水大数字用 `--accent`(橙)
- 休息大数字 + sparkline 用 `--success`(绿)

如果绿色在浅色卡片底色上过亮/过暗,执行 Step 2;否则跳过本任务。

- [ ] **Step 2: (可选)追加 `--success-soft` token**

在 `tokens.css` 浅色 `:root`(行 23 的 `--success` 附近)追加:

```css
  --success-soft: color-mix(in oklab, var(--success) 14%, transparent);
```

在深色 `[data-theme="dark"]`(行 143 附近)追加:

```css
  --success-soft: color-mix(in oklab, var(--success) 22%, transparent);
```

CSS Module 中需要柔化的地方改用 `var(--success-soft)`。

- [ ] **Step 3: Commit(若有改动)**

```bash
git add web/src/styles/tokens.css
git commit -m "style(tokens): 新增 success-soft 绿色柔化变体"
```

---

## Task 12: 全量验证 + 手动测试清单

**Files:** 无（验证任务）

- [ ] **Step 1: 全量 Rust 测试**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo test --lib 2>&1 | tail -20`
Expected: 所有测试通过(原有 + 新增 health_repo 测试)。

- [ ] **Step 2: Rust clippy(无警告)**

Run: `cd /Users/hans/web_project/cc-partner/src-tauri && cargo clippy --lib 2>&1 | tail -10`
Expected: 无新警告。

- [ ] **Step 3: 前端类型检查**

Run: `cd /Users/hans/web_project/cc-partner/web && npx tsc --noEmit 2>&1 | tail -10`
Expected: 无错误。

- [ ] **Step 4: 前端测试**

Run: `cd /Users/hans/web_project/cc-partner/web && npx tsx src/pages/Health/HabitStatsCard.test.ts 2>&1 | tail -5`
Expected: 测试通过。

- [ ] **Step 5: 启动开发模式手动验证**

Run: `cd /Users/hans/web_project/cc-partner/web && ./node_modules/.bin/tauri dev`

手动测试清单:

- [ ] 打开 `/health` 页面,看到「习惯统计」卡片,显示两栏(饮水/休息),各有 7 柱 sparkline
- [ ] 点「+1 杯」→ 饮水大数字 +1,sparkline 今日柱升高;500ms 内按钮置灰
- [ ] 关闭健康监测(设置页)→ 卡片不消失但数据停止刷新(或卡片隐藏,看产品决策)
- [ ] 测试 reminder:把工作窗口设为 1 分钟(设置页),等 1 分钟后 reminder 触发 → 休息栏"提醒次数"+1
- [ ] 测试 rest:reminder 弹窗点「开始休息」,等倒计时归零 → 休息大数字 +1,总时长增加
- [ ] 跨天测试(可选):调整系统时间到次日 0 点后,等下一轮采样 → 今日数字归零,sparkline 滚动
- [ ] 关闭饮水提醒(设置页 `waterEnabled=false`)→ 饮水栏不显示"距下次提醒"小字
- [ ] 深色模式切换 → 颜色正确,无对比度问题

- [ ] **Step 6: 更新 CLAUDE.md(功能逻辑/需求文档,非代码)**

按 AGENTS.md 规则 #5,更新以下文件的功能描述(不是 changelog,而是反映新能力):

1. **`web/CLAUDE.md`** 第 60 行「健康提醒页」段落:
   - 在 `healthApi` 方法清单追加新 4 方法(`getHabitStats` / `addWaterManual` / `deleteWaterRecord` / `recordRestCompleted`)
   - 在页面结构描述追加「+ 习惯统计 Card(HabitStatsCard:今日饮水次数/休息次数/总休息时长/近 7 天 sparkline,支持手动 +1 杯)」
   - 说明 `refresh` 的 `Promise.all` 现在并行 4 路(增加 `getHabitStats(7)`)

2. **`src-tauri/CLAUDE.md`** 第 30 行「storage/health_repo.rs」:
   - 把「activity_records/water_records 读写」改为「activity_records/water_records/rest_records 读写 + 聚合统计」
   - 在第 41 行「health/」段落补充「reminder 触发写入 rest_records,跨天清理同时清 water_records/rest_records」

3. **`AGENTS.md`** §8.2「前端 invoke 命令」表:在 `record_water` 行后追加 4 新命令行。

> 注意:`web/src/pages/Health/` 目录下没有独立 CLAUDE.md,文档集中在 `web/CLAUDE.md`。

- [ ] **Step 7: 最终 Commit**

```bash
git add docs/ CLAUDE.md AGENTS.md 2>/dev/null || true
git commit -m "docs: 更新健康命令清单 + Health 页组件文档" --allow-empty
```

---

## Self-Review 检查(实施完成后)

实施完所有任务后,逐项核对:

1. **Spec 覆盖**:
   - [ ] §2 数据维度 6 个指标全部实现 → Task 4 `get_habit_stats`
   - [ ] §3.2 `rest_records` 表 → Task 1
   - [ ] §3.3 清理 → Task 5 Step 2
   - [ ] §4.1 health_repo 9 方法 → Task 2 + Task 3
   - [ ] §4.2 4 命令 + DTO → Task 4
   - [ ] §5 前端类型/API/组件/接入 → Task 6-9
   - [ ] §6 交互(+1 节流/距下次提醒计算) → Task 7
   - [ ] §7 i18n → Task 10
   - [ ] §8 测试 → 各任务 TDD 步骤

2. **类型一致性**:
   - [ ] `HabitStatsDto`(Rust)字段与 `HabitStats`(TS)一一对应(camelCase 序列化对齐)
   - [ ] `healthApi` 方法名与 Rust 命令名 snake_case 对齐
   - [ ] `record_rest_completed` 在前端 API 是 `recordRestCompleted`,在 Rust 是 `record_rest_completed`

3. **手动验证清单全部通过**。
