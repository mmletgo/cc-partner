# 健康习惯统计（饮水 / 休息）设计文档

- **日期**: 2026-07-07
- **关联历史**: `2026-06-22-health-reminder-design.md`（健康提醒基础）
- **状态**: 设计已与用户确认，待出实施计划

## 1. 背景与目标

健康提醒功能（M10）已落地「久坐提醒 + 喝水提醒 + 活动统计」三大块。其中**活动统计**有完整的写入 + 聚合查询 + 图表展示，但「饮水」和「休息」两类**只触发提醒、未做统计**：

- **饮水**：`water_records` 表只写入时间戳，**没有任何读取消费方**——数据躺在 SQLite 里，前端无法展示「今日喝了多少杯」。
- **休息**：状态机的 `reminder_closed_window`（转入 Resting 时报告的工作窗口起止时间）被标 `#[allow(dead_code)]`，**完全未持久化**——无法回答「今天休息了几次、共多久」。

用户在 Health 页看到活动统计，却看不到饮水/休息的反馈，**习惯养成闭环缺失**。

### 目标

在 Health 页新增「习惯统计」卡片，复用现有视觉风格，让用户一眼看到：

- 今日饮水次数 + 近 7 天趋势
- 今日休息次数 + 今日总休息时长 + 近 7 天休息次数趋势
- 支持手动 +1 杯水（误点可删除记录）

### 非目标（YAGNI）

- ❌ 不引入「每日目标 / 目标 ml」配置（与用户确认：只统计次数）
- ❌ 不记录每次饮水的容量 ml
- ❌ 不做眼保健 / 坐姿 / 伸展等其它健康子项（当前功能无此类提醒）
- ❌ 不做月视图 / 年视图，sparkline 固定展示近 7 天

## 2. 数据维度

| 展示指标 | 数据来源 | 展示形式 |
|---|---|---|
| 今日饮水次数 | `water_records` 当日计数 | 大数字（橙色） |
| 近 7 天饮水次数 | `water_records` 按本地日聚合 | 7 柱 sparkline（橙，今日高亮） |
| 今日休息次数 | 新表 `rest_records`，`kind='rest'` 当日计数 | 大数字（绿色） |
| 今日总休息时长 | `rest_records` 中 `kind='rest'` 的 `duration_seconds` 求和 | 小字 |
| 今日提醒次数 | `rest_records` 中 `kind='reminder'` 当日计数 | 小字 + tooltip |
| 近 7 天休息次数 | `rest_records` 中 `kind='rest'` 按本地日聚合 | 7 柱 sparkline（绿，今日高亮） |

### 关于「休息」的定义（与用户确认）

- **`kind='reminder'`**：状态机判定 `should_remind=true` 时记录一次（无论用户后续跳过 / 贪睡 / 开始休息）。即"久坐提醒触发次数"。
- **`kind='rest'`**：用户点击「开始休息」并完整完成倒计时（restLeft 归零）后记录一次，包含实际休息秒数。
- 「跳过」按钮不记录 rest（用户主动放弃休息）。
- 今日大数字 = **休息次数（`kind='rest'`）**，提醒次数仅作辅助小字展示。

## 3. 数据模型

### 3.1 `water_records` 表（已存在，无需改表）

```sql
-- 已在 src-tauri/src/lib.rs:175-177 建表
CREATE TABLE IF NOT EXISTS water_records (
  ts INTEGER PRIMARY KEY
);
```

保持不变。继续只存时间戳。`ts` 作为主键意味着同一秒内多次「+1」会冲突——前端用「连续点击节流」处理（见 §6.4）。

### 3.2 新增 `rest_records` 表

```sql
-- 在 src-tauri/src/lib.rs 新增 REST_SCHEMA 常量 + init_db 执行
CREATE TABLE IF NOT EXISTS rest_records (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts INTEGER NOT NULL,           -- 记录时间（Unix 秒）
  kind TEXT NOT NULL,            -- 'reminder' | 'rest'
  duration_seconds INTEGER NOT NULL DEFAULT 0  -- rest 时为实际秒数；reminder 为 0
);
-- 按时间查询索引（可选，数据量小，暂时不加）
```

**设计说明**：

- `id` 自增：与 `water_records` 不同，因为同一秒内可能既触发 reminder 又完成 rest，且未来可能扩展更多事件，自增主键避免冲突。
- `kind` 字段用字符串而非 bool：未来可能扩展 `kind='snooze'` 等子类型，扩展性好。
- `duration_seconds` 默认 0：reminder 事件天然为 0，rest 事件填实际秒数。

### 3.3 清理策略

复用现有 `cfg.health.retain_days`（默认 90 天），在 `src-tauri/src/health/mod.rs:199-203` 已有的每日清理点扩展：

- 现有：`state.health_repo.cleanup_older_than(cutoff)` 清 `activity_records`
- 新增：`state.health_repo.cleanup_water_older_than(cutoff)` 清 `water_records`
- 新增：`state.health_repo.cleanup_rest_older_than(cutoff)` 清 `rest_records`

三条 DELETE 都在同一个跨天清理分支里跑，幂等、成本低。

## 4. 后端改动

### 4.1 `storage/health_repo.rs` 新增方法

在现有 `impl HealthRepo`（行 38-201）追加：

```rust
// ===== 饮水统计 =====

/// 查询 since_ts 之后的饮水次数
pub async fn count_water_since(&self, since_ts: i64) -> Result<i64, AppError>

/// 按本地日聚合 since_ts 之后每日饮水次数，返回长度恒为 days 的数组
/// days 参数决定桶数（前端固定传 7）
pub async fn get_daily_water_counts(
    &self,
    since_ts: i64,
    days: usize,
) -> Result<Vec<i64>, AppError>

/// 删除指定 ts 的饮水记录（用户撤销误点）
pub async fn delete_water(&self, ts: i64) -> Result<bool, AppError>

/// 清理 cutoff_ts 之前的饮水记录
pub async fn cleanup_water_older_than(&self, cutoff_ts: i64) -> Result<u64, AppError>

// ===== 休息统计 =====

/// 插入一条 rest_records
pub async fn insert_rest_record(
    &self,
    ts: i64,
    kind: &str,
    duration_seconds: i64,
) -> Result<i64, AppError>  // 返回自增 id

/// 查询 since_ts 之后指定 kind 的次数（kind 传 "rest" 或 "reminder"）
pub async fn count_rest_since(&self, since_ts: i64, kind: &str) -> Result<i64, AppError>

/// 查询 since_ts 之后所有 rest（kind='rest'）的总时长秒数
pub async fn sum_rest_duration_since(&self, since_ts: i64) -> Result<i64, AppError>

/// 按本地日聚合 since_ts 之后每日指定 kind 的次数
pub async fn get_daily_rest_counts(
    &self,
    since_ts: i64,
    days: usize,
    kind: &str,
) -> Result<Vec<i64>, AppError>

/// 删除指定 id 的 rest 记录（用户撤销误记）
pub async fn delete_rest(&self, id: i64) -> Result<bool, AppError>

/// 清理 cutoff_ts 之前的 rest 记录
pub async fn cleanup_rest_older_than(&self, cutoff_ts: i64) -> Result<u64, AppError>
```

**实现要点**：

- 移除现有 `insert_water` 上的 `#[allow(dead_code)]`（行 193），因为即将被 `record_water` 命令真正消费。
- 本地日聚合：参考现有 `get_hourly_activity`（行 155）的桶聚合写法，只是把 24 小时桶换成 N 天桶，桶边界用本地 0 点（前端统一传本地 0 点起的 since_ts）。
- 所有方法都按现有模式用 `sqlx::query` + `fetch_one/fetch_all`，错误转 `AppError`。

### 4.2 `commands/health.rs` 新增 / 修改命令

**新增命令**（在现有 `record_water` 之后，行 309 后追加）：

```rust
/// 用户在习惯统计卡片点「+1 杯」手动加计饮水
#[tauri::command]
pub async fn add_water_manual(state: State<'_, AppState>) -> Result<i64, AppError>
// 返回新插入的 ts（前端可立即刷新，无需等轮询）

/// 撤销误点的饮水记录
#[tauri::command]
pub async fn delete_water_record(state: State<'_, AppState>, ts: i64) -> Result<bool, AppError>

/// 获取习惯统计（饮水 + 休息聚合一次性返回，减少前端多次 invoke）
#[tauri::command]
pub async fn get_habit_stats(
    state: State<'_, AppState>,
    days: Option<i64>,  // 默认 7
) -> Result<HabitStatsDto, AppError>
```

**`HabitStatsDto` 定义**（与现有 `ActivityStatsDto` 同位置，commands/health.rs 第 97 行附近）：

```rust
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HabitStatsDto {
    pub today_water_count: i64,
    pub water_daily_counts: Vec<i64>,      // 长度 7，[6天前, ..., 今日]
    pub last_water_ts: Option<i64>,        // 距今最近一次 water_records.ts，用于「距下次提醒」
    pub today_rest_count: i64,
    pub today_rest_total_seconds: i64,
    pub today_reminder_count: i64,
    pub rest_daily_counts: Vec<i64>,       // 长度 7
}
```

**修改 `skip_reminder` / 状态机消费点**：

reminder 的记录点放在 `src-tauri/src/health/mod.rs:148-163`（现有 emit `health:reminder` 的位置），在 `if should_remind {` 分支内追加：

```rust
if should_remind {
    // 新增：记录 reminder 触发
    if let Err(e) = state.health_repo.insert_rest_record(now, "reminder", 0).await {
        tracing::warn!("Failed to insert rest reminder record: {e}");
    }
    // ... 现有 emit / overlay 逻辑保持不变
}
```

**修改休息完成点**：

rest 的记录点放在 `web/src/pages/HealthOverlay.tsx:81-105` 的 `startRest` 倒计时归零回调里（行 92-99），在 `healthApi.skip()` 之后追加一个新命令调用：

```rust
/// 用户完成休息倒计时后调用，记录一次完整休息
#[tauri::command]
pub async fn record_rest_completed(state: State<'_, AppState>) -> Result<(), AppError> {
    let now = chrono::Utc::now().timestamp();
    // 从配置读 break_seconds 作为 duration（前端倒计时也是用这个值）
    let cfg = state.config.read().await;
    let duration = cfg.health.break_seconds;
    drop(cfg);
    state.health_repo.insert_rest_record(now, "rest", duration).await?;
    Ok(())
}
```

> **说明**：用配置的 `break_seconds` 作为 duration 而非前端实际倒计时秒数，因为前端倒计时恒等于 `break_seconds`（到点才触发），记录配置值语义清晰且不依赖前端状态同步。如果用户中途关闭 overlay 会被视为"跳过"，不记录。

### 4.3 命令注册

在 `src-tauri/src/lib.rs:656-670` 的 health 命令注册块追加 4 个新命令：

```
health_cmd::add_water_manual,
health_cmd::delete_water_record,
health_cmd::record_rest_completed,
health_cmd::get_habit_stats,
```

注释里的「14 命令」改为「18 命令」。

## 5. 前端改动

### 5.1 `web/src/lib/types.ts` 新增类型

在现有 `ActivityDetail`（行 1319）之后追加：

```ts
/** 习惯统计（饮水 + 休息）后端返回 */
export interface HabitStats {
  todayWaterCount: number;
  waterDailyCounts: number[];      // 长度 7
  todayRestCount: number;
  todayRestTotalSeconds: number;
  todayReminderCount: number;
  restDailyCounts: number[];       // 长度 7
}
```

### 5.2 `web/src/api/health.ts` 新增方法

在现有 `healthApi` 对象（行 18-67）追加：

```ts
getHabitStats: (days?: number) => invoke<HabitStats>('get_habit_stats', { days }),
addWaterManual: () => invoke<number>('add_water_manual'),           // 返回新 ts
deleteWaterRecord: (ts: number) => invoke<boolean>('delete_water_record', { ts }),
recordRestCompleted: () => invoke<void>('record_rest_completed'),
```

### 5.3 新建 `web/src/pages/Health/HabitStatsCard.tsx`

新组件，结构：

```tsx
export function HabitStatsCard({ stats }: { stats: HabitStats }) {
  // 渲染饮水 / 休息两栏 + 7 天 sparkline + 手动 +1 杯按钮
  // sparkline 用纯 div bar，不用 recharts（数据量小、保持轻）
}
```

**视觉规格**（参考已确认的 mockup）：

- 卡片标题：「💧🌿 习惯统计」
- 左栏（饮水）：大数字「5 杯」（橙 `--accent`）+ 小字「距下次提醒 · 还有 N 分钟」+ 7 柱 sparkline（橙）+ 右上角「+1 杯」按钮
- 右栏（休息）：大数字「3 次」（绿，新增 token 或复用现有绿色，见 §5.5）+ 小字「总时长 N 分钟 · 提醒 X 次 ⓘ」+ 7 柱 sparkline（绿）
- sparkline 今日柱高亮（饱和度高的主色），其它柱降饱和
- 底部：「数据保留 {retainDays} 天」+ 「查看历史记录 →」链接（点击展开历史记录抽屉/弹窗，可删除单条记录）

**与现有 `StatsChart.tsx` 的关系**：不复用 `StatsChart`，因为后者是 recharts 大图（app 使用时长 + 24h 分布），新卡片是迷你 sparkline，数据形态、视觉尺寸都不同。新建独立组件更清晰。

### 5.4 `web/src/pages/Health/Health.tsx` 接入

- **数据获取**：在 `refresh()` 函数（行 129-141）的 `Promise.all` 里追加 `healthApi.getHabitStats(7)`，新增 state `habitStats`。
- **UI 插入位置**：在指标网格（`metricGrid`，行 263-284）的 `Card.Body` 内或新建独立 Card，放在指标网格之后、活动统计图表（行 288-300）之前。倾向**新建独立 Card**，与活动图表面板并列，分区更清晰。
- **轮询**：复用现有 30s 轮询（行 144-148），习惯统计一起刷新。
- **"+1 杯"按钮**：点击调 `healthApi.addWaterManual()`，乐观更新本地 `habitStats.todayWaterCount++`，失败回滚。
- **「距下次提醒」小字**：需要从 `healthApi.getStatus()` 拿 `waterEnabled` + 配置 `waterIntervalSeconds` + 上次喝水 ts 推算。`HealthStatus` 当前不含上次喝水 ts——**需扩展 `HealthStatusDto` 增加 `lastWaterTs` 字段**（见 §5.6）。

### 5.5 新增 token

`web/src/styles/tokens.css`：休息用绿色与饮水橙色区分。先检查现有 token 是否已有"成功/健康"绿，若无则新增：

- 浅色：`--health-rest: #5b9a6b`、`--health-rest-on: #faf9f5`
- 深色：`--health-rest: #7ab98a`、`--health-rest-on: #1f1d1a`

如果项目已有绿色语义 token（如 `--success`）则复用，避免重复定义。决策点留给实施时确认。

### 5.6 `HealthStatusDto` 扩展字段

为支持「距下次提醒」展示，在 `HabitStatsDto`（§4.2）中追加一个字段，而不是改 `HealthStatusDto`（减少改动面、避免状态语义混淆）：

```rust
// 在 HabitStatsDto 追加
pub last_water_ts: Option<i64>,    // 距今最近一次 water_records.ts，无则 None
```

`get_habit_stats` 命令实现里查一次 `SELECT MAX(ts) FROM water_records` 填入。前端 `HabitStats` 类型同步加 `lastWaterTs?: number`。

> **决策**：放 `HabitStats` 而非 `HealthStatus`。理由：`HealthStatus` 是状态机的运行时镜像（idle/working/resting 相位），"上次喝水时间"是历史数据查询，混进去会污染状态语义。`HabitStats` 本就是"统计聚合返回"，放这里耦合度最低。

## 6. 交互细节

### 6.1 「+1 杯」按钮

- 单击：调 `addWaterManual`，本地数字立即 +1，sparkline 今日柱同步升高。
- 节流：500ms 内只响应一次（防止连点产生同一秒 ts 主键冲突）。节流期间按钮置灰。
- 失败：toast 提示「记录失败，请重试」，本地数字回滚。

### 6.2 「查看历史记录」

点击展开一个抽屉/弹窗（具体形态实施时定），列出最近 50 条饮水 + 休息记录，每条带时间 + 类型图标 + 删除按钮。删除后立即从列表消失并刷新统计。

> 可作为 P1 增量，MVP 先做"只读 + 删除单条"，不做编辑。

### 6.3 「距下次提醒」计算

前端纯计算（不新增后端命令）：

```
remaining = (lastWaterTs ?? 程序启动时间) + waterIntervalSeconds - now
```

- `lastWaterTs` 来自 §5.6 的扩展字段。
- 如果 `remaining < 0`：显示「已超时，等待提醒」。
- 如果饮水提醒关闭（`waterEnabled=false`）：不显示这行。

### 6.4 同一秒 ts 冲突处理

`water_records.ts` 是主键。理论上「+1 杯」连续点击或同一秒内既有 reminder 又手动加水会冲突。处理方式：

- 前端节流（§6.1）降低概率。
- 后端 `insert_water` 改为 `INSERT OR IGNORE`，冲突时返回成功但前端检测到「行数没变」就提示「同一秒已记录过」。这个改动保留在 health_repo 层，对外语义清晰。

## 7. i18n 文案

在 `web/src/i18n/locales/zh/health.json` 和 `en/health.json` 新增 key（中英对齐）：

| key | zh | en |
|---|---|---|
| `habitStatsTitle` | 习惯统计 | Habit Stats |
| `todayWater` | 今日饮水 | Today's Water |
| `todayRest` | 今日休息 | Today's Rest |
| `cup` | 杯 | cups |
| `times` | 次 | times |
| `totalRestMinutes` | 总休息 {{n}} 分钟 | Total rest {{n}} min |
| `reminderTimesToday` | 提醒 {{n}} 次 | Reminders: {{n}} |
| `nextWaterIn` | 距下次提醒 · 还有 {{n}} 分钟 | Next reminder in {{n}} min |
| `waterOverdue` | 已超时，等待提醒 | Overdue, awaiting reminder |
| `addCup` | +1 杯 | +1 cup |
| `addCupFailed` | 记录失败，请重试 | Failed to record, please retry |
| `viewHistory` | 查看历史记录 | View history |
| `habitFooter` | 数据保留 {{n}} 天 | Data retained for {{n}} days |
| `deleteRecord` | 删除记录 | Delete |
| `deleteSuccess` | 已删除 | Deleted |
| `weekShortMon`/`Tue`/... | 一/二/三/四/五/六/今 | Mon/Tue/.../Today |

> 周几短标签如果 i18n 已有通用版可复用，避免重复。

## 8. 测试计划

### 8.1 Rust 单元测试（`health_repo.rs`）

- `count_water_since` 边界：空表、单条、跨天。
- `get_daily_water_counts` 桶数正确、跨天边界正确（00:00:00 落对桶）。
- `insert_rest_record` 不同 kind 都能写入并查回。
- `count_rest_since` 按 kind 过滤正确。
- `sum_rest_duration_since` 只累加 rest 不累加 reminder。
- `delete_water` / `delete_rest` 删除存在/不存在记录的返回值。
- cleanup 各表删除正确范围。

### 8.2 Rust 集成测试（in-memory sqlite）

- `record_rest_completed` 写入 rest 记录 + duration 正确。
- `add_water_manual` 写入 water 记录，连点节流。
- `get_habit_stats` 端到端：插入若干数据，返回聚合结构正确。
- `delete_water_record` 后 stats 数字减一。

### 8.3 前端组件测试

- `HabitStatsCard` 渲染：mock `HabitStats`，断言大数字、sparkline 柱数（7）、今日柱高亮 class。
- 「+1 杯」点击：mock `addWaterManual`，断言乐观更新 + 失败回滚。
- 「距下次提醒」计算逻辑单元测试（纯函数）。

### 8.4 手动验证清单

- [ ] 开启健康提醒，工作窗口设 1 分钟（测试用），等 reminder 触发 → rest_records 出现一条 reminder → 习惯卡片"提醒次数" +1。
- [ ] 点击「开始休息」，等倒计时归零 → 出现一条 rest → 大数字 +1，总时长增加。
- [ ] 点「+1 杯」→ 饮水数字 +1，sparkline 今日柱升高。
- [ ] 跨天：调整系统时间到次日 0 点后，确认今日数字归零、sparkline 滚动。
- [ ] 关闭饮水提醒 → 「距下次提醒」小字消失。
- [ ] 删除一条记录 → 数字减一。

## 9. 实施顺序（粗粒度，细化由 writing-plans 出）

1. **DB schema**：lib.rs 加 `REST_SCHEMA`，init_db 执行。
2. **health_repo.rs**：新增 9 个方法 + 移除 dead_code 标注 + 单元测试。
3. **commands/health.rs**：新增 4 命令 + 修改 `get_health_status`（加 lastWaterTs）+ 修改 `record_rest_completed`。
4. **health/mod.rs**：在 should_remind 分支插入 reminder 记录 + 跨天清理扩展。
5. **lib.rs**：注册 4 新命令。
6. **types.ts + api/health.ts**：新增 `HabitStats` 类型 + 4 方法。
7. **HabitStatsCard.tsx**：新组件 + CSS Module。
8. **Health.tsx**：接入数据 + UI 插入。
9. **HealthOverlay.tsx**：`startRest` 归零回调加 `recordRestCompleted`。
10. **i18n**：zh + en 文案。
11. **tokens.css**：绿色 token（如需）。
12. **手动验证清单**跑一遍。

## 10. 风险与权衡

- **同一秒 ts 冲突**：用 `INSERT OR IGNORE` + 前端节流兜底，足够。
- **状态机重启丢失**：`HealthRuntime` 是内存状态机，重启后 Idle。rest_records 已落库不受影响；但 reminder 的「本窗口是否已提醒」标志会丢，可能导致重启后短期内重复 reminder——这是**现有行为**，本次不引入新问题。
- **break_seconds 取配置值 vs 实际倒计时**：取配置值简化实现，与前端倒计时语义一致。代价：如果未来支持「用户中途暂停倒计时」会不准——但当前无此功能，YAGNI。
- **lastWaterTs 放哪**：见 §5.6，已决策放 `HabitStats`。

## 11. 相关文件索引

后端：
- `src-tauri/src/lib.rs`（schema 定义 166-177 行 / init_db 290-292 行 / 命令注册 656-670 行）
- `src-tauri/src/storage/health_repo.rs`（impl 38-201 行）
- `src-tauri/src/commands/health.rs`（record_water 299-309 / skip_reminder 218-223 / DTO 28-117）
- `src-tauri/src/health/mod.rs`（should_remind 消费 148-163 / 跨天清理 199-203）
- `src-tauri/src/health/state.rs`（advance 85-137 / reminder_closed_window 104-113）
- `src-tauri/src/config.rs`（retain_days 317-318 / break_seconds）

前端：
- `web/src/lib/types.ts`（HealthConfig 1245 / HealthStatus 1277 / ActivityDetail 1319）
- `web/src/api/health.ts`（healthApi 18-67）
- `web/src/pages/Health/Health.tsx`（refresh 129-141 / metricGrid 263-284 / 图表面板 288-300）
- `web/src/pages/Health/StatsChart.tsx`（现有活动图表，不复用但参考风格）
- `web/src/pages/HealthOverlay.tsx`（startRest 81-105）
- `web/src/i18n/locales/zh/health.json` + `en/health.json`
- `web/src/styles/tokens.css`
