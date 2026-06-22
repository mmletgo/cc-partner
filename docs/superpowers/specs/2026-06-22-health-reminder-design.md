# 健康提醒模块设计(融入 Catrace 久坐提醒)

## Context(为什么做)

将 [lanxiuyun/Catrace](https://github.com/lanxiuyun/Catrace)(久坐提醒 + 键盘鼠标轻量监测,React + TS + Rust + Tauri 2)的健康提醒能力融入 claude-partner,让这个局域网协作工具同时成为「开发者久坐健康管家」。

融入成本低的根本原因:**技术栈完全一致 + claude-partner 已具备久坐监测所需的几乎所有基础设施**(系统托盘、macOS 权限引导流程、SQLite、配置管理、常驻后台、tokio 后台任务、透明窗口技术、React Router 多页面、i18n)。不是「从零搬」,而是「往成熟骨架上加模块」。

基线:`13b7ce4`(master)。

## 需求(已与用户确认)

**功能范围(全量融入)**:
1. 核心久坐提醒:键盘鼠标活动监测 + 工作/休息状态判定 + 工作窗口满则提醒休息
2. 多种提醒方式:系统通知 / 应用内 toast / 全屏遮罩,支持推迟 5/10 分钟、跳过本次
3. 活动统计图表:当日活跃/休息分钟、应用使用时长排行、可视化图表
4. 喝水提醒:按间隔提醒补水,记录喝水时间轴

**6 个关键决策**:

| 决策点 | 选择 |
|---|---|
| 集成形态 | claude-partner 主界面**新增「健康提醒」页签**,与文件传输/截图/Prompt 平级 |
| 运行模式 | **开机自启 + 久坐监测默认开启**(装好即生效,首次引导 macOS 权限) |
| 统计粒度 | **最细**:活跃/非活跃 + 活动 app 进程名 + 窗口标题(全程本地,不上传) |
| 架构 | **后端主导**(久坐监测属「后台持续运行 + 定时判定」型,归 Rust 后端) |
| 交付节奏 | **一次性全做**(单计划交付全部功能) |
| 数据存储 | 复用 `~/.claude-partner/data.db`;**健康配置放 `config.json` 的 `AppConfig`**(跟随项目现有 config 惯例,不学 Catrace 放 DB) |

## 技术方案(后端主导)

久坐监测与现有 `transfer/`(后台传输 task)、`updater/`(后台下载 task)同类,属 Rust 后端职责。前端只负责展示与配置。

**键鼠监测 crate(复刻 Catrace)**:
- macOS:`device_query`(轮询查询键鼠当前状态)→ 需 **Accessibility(辅助功能)** 权限
- Windows / Linux:`rdev`(全局事件 hook 累计)→ 无需特殊权限
- 活动窗口(标题/进程名):`active-win-pos-rs`,三平台一致,支撑最细粒度统计

> macOS 选 `device_query` 轮询而非统一 `rdev`:只需 Accessibility 一项权限(而非 Accessibility + 输入监控两项),权限摩擦更小。具体 FFI 细节留 writing-plans 阶段定(device_query 内部已封装 IOKit 调用,大概率不需手写 FFI)。

## 详细设计

### 1. 后端模块与接入

新增 `src-tauri/src/health/`,与 `transfer/`、`sync/` 平级:

```
src-tauri/src/health/
├── mod.rs        — 对外门面:start_health_daemon(app,state) spawn 后台 tokio task;setup 时调用
├── monitor.rs    — 键鼠采样:每分钟查活动状态 + 活动窗口标题/进程名(跨平台分发)
├── state.rs      — 工作/休息状态机(纯算法,可单测):喂入分钟级活动序列 → 当前状态 + 是否触发提醒
├── reminder.rs   — 提醒生命周期:触发/推迟/跳过/免打扰判定
├── water.rs      — 喝水提醒:独立间隔计时 + 记录
└── db.rs         — sqlx:activity_records / water_records 增查 + 按天聚合 + 过期清理
```

**接入点**:
- `state.rs(AppState)` 加 `health: Arc<HealthState>`(运行时:当前工作窗口、上次提醒、推迟到何时、监测开关等)
- `config.rs(AppConfig)` 加 `health: HealthConfig`(见第 4 节)
- `lib.rs` setup:初始化 health → `health::start_health_daemon(app, state)`;`RunEvent::Exit` 停止 task(类比 mDNS 清理)
- `commands/health.rs` 新增 invoke 命令(见第 5 节),注册进 `invoke_handler`

### 2. 键鼠监测与权限

**每分钟采样流程**(后台 task,`tokio::time::interval(60s)`):
1. 查键鼠是否活跃 → `is_active: bool`(macOS device_query / Win·Linux rdev 累计判定)
2. 若活跃,取活动窗口标题 + 进程名(`active-win-pos-rs`)
3. 写 `activity_records` 一行
4. 喂入状态机 → 判定是否触发提醒 → `emit("health:reminder", {...})`

**权限**:扩展 `permissions/` 新增 `accessibility` 检测(macOS);复用现有 `OnboardingGuard`/`PermissionCard`/`PermissionNeededListener` 流程加该项引导(衔接 `2026-06-22-permission-auto-prompt-design.md`)。非 macOS 直接 granted。

### 3. 工作/休息状态机算法(纯逻辑,核心)

**关键概念**:
- **工作窗口**:从首次键鼠活动起的一段连续工作时段
- **有效休息**:连续无操作 ≥ `break_minutes`(默认 5)才中断工作窗口;短暂停歇(<5min,如倒水)不中断

```
状态流转(每分钟采样驱动):
  IDLE ──有活动──► WORKING(开始新工作窗口,记起点)
  WORKING ──连续无活动 ≥ break_minutes──► RESTING(关闭当前窗口,入库)
  RESTING ──有活动──► WORKING(开始新窗口)
  WORKING ──满足提醒条件──► REMINDER(等用户响应)
```

**提醒触发条件**(每分钟采样后判定;精确三条件语义实现时对照 Catrace `state.rs`,此处描述意图,满足任一):
- 当前 `WORKING` 且该**工作窗口自然时长**(从窗口起点至今,含 <`break_minutes` 的短暂停歇)** ≥ `work_window_minutes`(默认 45)**,且窗口内**未发生过有效休息** → 触发
- (Catrace 条件 C)上一刚结束的工作窗口本身已达过载(≥ `work_window_minutes`)且未被有效休息打断 → 触发

**用户响应**:
- 推迟 5/10 分钟 → 进入「推迟态」,到点重新判定
- 跳过本次 → 关闭当前工作窗口(视为已休息),回 `IDLE`/`WORKING`

参数默认:`work_window_minutes=45`(范围 1–120)、`break_minutes=5`,均可配。

### 4. 数据模型与存储

复用 `data.db`,新建表(`CREATE TABLE IF NOT EXISTS`,兼容旧库):

```sql
-- 每分钟活动采样(一行/分钟)
activity_records(
  ts INTEGER PRIMARY KEY,        -- 分钟级 unix 时间戳
  is_active INTEGER NOT NULL,    -- 0/1
  process_name TEXT,             -- 活跃时的进程名(可为空)
  window_title TEXT,             -- 活跃时的窗口标题(可为空)
  category TEXT                  -- 应用分类(可选,后置)
)
-- 喝水记录
water_records(ts INTEGER PRIMARY KEY)
```

**健康配置**放 `AppConfig.health`:
```rust
struct HealthConfig {
  enabled: bool,               // 默认 true
  work_window_minutes: u32,    // 45
  break_minutes: u32,          // 5
  reminder_styles: ReminderStyles, // 通知/弹窗/全屏 开关
  water_enabled: bool,         // true
  water_interval_minutes: u32, // 60
  dnd_start: Option<String>,   // 免打扰起 "22:00"
  dnd_end: Option<String>,     // 免打扰止 "07:00"(支持跨午夜)
  record_window_title: bool,   // 默认 true;false 时降级到「只记进程名」粒度(隐私退出)
  retain_days: u32,            // 明细保留天数,默认 90
}
```

**数据清理**(每分钟一条,1 年≈50 万行):后台 task 每日清理一次,删除 `ts < now - retain_days*86400` 的明细。避免无限增长。

### 5. 提醒机制

**后端 → 前端**:`emit("health:reminder", { reason, workMinutes, postponeOptions:[5,10] })`

**三种展示方式**(前端,均受 `reminder_styles` 开关控制):
1. **系统通知**:tauri notification(托盘区原生通知)
2. **应用内 toast**:React 浮层(右下角,堆叠,带「推迟5/10分」「跳过」「已喝水」按钮)
3. **全屏遮罩**:复用截图模块已验证的**透明窗口技术**(`WebviewWindowBuilder` + `transparent(true)` + `always_on_top`),路由 `/health-overlay`,倒计时自动关闭

**响应命令**(`commands/health.rs`):`snooze_reminder(minutes)` / `skip_reminder()` / `record_water()` / `get_health_status()` / `get_activity_stats(range)` / `update_health_config(cfg)` / `toggle_health_enabled(enabled)`

**免打扰**:触发前判 `dnd_start/dnd_end`(支持跨午夜),命中则静默累计、不弹。

### 6. 前端页签 `web/src/pages/Health/`

```
Health/
├── index.tsx          — 页面主体(今日概览 + 状态环 + 设置入口)
├── StatusRing.tsx     — 当前工作窗口倒计时环(SVG)
├── StatsChart.tsx     — 当日活跃/休息分钟、app 使用时长排行(图表库 recharts)
├── Settings.tsx       — 工作窗口/休息/提醒方式/喝水/免打扰/隐私 配置表单
└── ReminderToast.tsx  — 应用内 toast(监听 health:reminder)
```

- **路由**:`App.tsx` 加 `/health`;**Sidebar** 加导航项(与 Home/Prompts/Transfer 平级)
- **i18n**:新增 namespace `health`,`src/i18n/locales/{en,zh}/health.json`(规则:禁止组件内硬编码文案)
- **API**:`src/api/health.ts` 封装上述 invoke 命令
- 全屏遮罩页 `HealthOverlay.tsx` 独立于 AppShell(类比 `Screenshot/Overlay.tsx`),路由 `/health-overlay`,onMount 强制透明背景

### 7. 开机自启

新增 `tauri-plugin-autostart`(Catrace 同款)。setup 时按 `AppConfig` 开关注册/注销自启。用户选默认开启 → 首次启动默认注册自启(可在设置关)。capabilities 加 `autostart:default`。

### 8. 喝水提醒

独立子系统(`water.rs`):后台 task 按 `water_interval_minutes` 计时 → `emit("health:water")` → 前端 toast「该喝水了」+「已喝水」按钮 → `record_water()` 写 `water_records`。受 `dnd` 和 `water_enabled` 控制。统计页展示当日喝水次数/时间轴。

### 9. 错误处理与边界

- **权限缺失降级**:macOS 未授权 Accessibility → 监测 task 不采键鼠(全记为非活跃),主窗口弹引导(复用 `PermissionNeededListener` 范式);授权后自动恢复
- **采样线程容错**:`device_query`/`rdev`/`active-win-pos-rs` 单次失败不崩 task,记 `tracing::warn!` 跳过该分钟
- **Win rdev 被杀软拦截**:降级为「无活动」,记日志,不崩
- **数据清理失败**:不阻断主流程
- **窗口标题敏感**:本地不上传;设置页 `record_window_title` 开关提供退出机制(降级到「只记进程名」)
- **Send 边界**:health task 内跨 await 不持 `RwLockReadGuard`(参照 M5 传输入坑:先 clone 字段再 await)

## 文件改动清单

### 后端(`src-tauri/`)
- **新增** `src/health/{mod,monitor,state,reminder,water,db}.rs`
- **新增** `src/commands/health.rs`
- `src/state.rs`:加 `health: Arc<HealthState>`
- `src/config.rs`:`AppConfig` 加 `health: HealthConfig`
- `src/permissions/mod.rs` + `commands/permissions.rs`:加 `accessibility` 权限类型(检测 + 引导)
- `src/lib.rs`:setup 调 `start_health_daemon`;`RunEvent::Exit` 停 task;`invoke_handler` 注册 health 命令;`.plugin(tauri_plugin_autostart::init())`
- `migrations/` 或 lib.rs 内联:`activity_records` / `water_records` 建表
- `Cargo.toml`:`device_query` / `rdev` / `active-win-pos-rs` / `tauri-plugin-autostart` / `recharts`(前端)
- `capabilities/default.json`:`autostart:default`;全屏遮罩窗口 label `health-overlay-*` 通配 + `core:event`
- `tauri.conf.json`:如需 notification 权限

### 前端(`web/`)
- **新增** `src/pages/Health/{index,StatusRing,StatsChart,Settings,ReminderToast}.tsx` + `HealthOverlay.tsx`
- **新增** `src/api/health.ts`
- `src/App.tsx`:路由 `/health` + `/health-overlay`;Sidebar 加导航;`health:reminder`/`health:water` 顶层监听
- `src/i18n/locales/{en,zh}/health.json`:新 namespace
- `src/lib/types.ts`:health 相关类型(对齐 Rust camelCase)
- `web/package.json`:`recharts`

### 文档(规则 5)
- `src-tauri/CLAUDE.md`:新增 M10 健康模块节(监测/状态机/存储/权限/自启)
- `web/CLAUDE.md`:Health 页签 + i18n namespace 条目
- `docs/prd.md`:补充健康提醒功能(规则 10)

## 验证(规则 11/12,亲自跑)

1. `cd src-tauri && cargo build && cargo clippy && cargo test` —— 编译 + lint + 状态机/db 单测通过
2. `cd web && npx tsc --noEmit && npm run lint` —— 类型 + lint 通过
3. `./start.sh` dev 实测:
   - 首次启动 → 引导 Accessibility 权限 → 授权后监测自动开始
   - 键鼠活动累积 → 工作 45min 无有效休息 → 触发提醒(通知/toast/全屏三种分别验证)
   - 推迟 5/10 分钟、跳过本次 生效
   - 喝水提醒按间隔触发 →「已喝水」记录入库
   - 统计页展示当日活跃/休息分钟、app 使用时长排行、喝水时间轴
   - 开机自启注册成功;设置页可关
   - 免打扰时段内不弹提醒
   - `record_window_title=false` 时不再记窗口标题
4. 日志:主动读 `tracing` 输出分析监测/提醒行为(规则 12)

## 不做(YAGNI)

- 提醒文案风格库(温柔/搞笑/严肃)——先固定一套中性文案,风格库后置
- 视频流媒体检测(Catrace 有的特色功能)——与久坐核心无关,后置
- 跨设备健康数据同步(复用现有 sync 通道)——健康数据纯属个人本地,不进 P2P 同步
- 活动历史的周/月维度报表——先做「当日」,长周期后置
- 托盘图标实时倒计时文字(macOS)——先 tooltip,图标文字后置
- 应用分类(category)自动归类——字段预留,归类逻辑后置
