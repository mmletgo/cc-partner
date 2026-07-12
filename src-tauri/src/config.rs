//! config.rs — 应用配置：加载/保存/默认值生成
//!
//! Business Logic（为什么需要这个模块）:
//!     应用需在多次运行间保持一致的设备标识（device_id）和用户偏好（设备名、端口、
//!     接收目录、快捷键）。首次运行要生成默认配置并持久化。此模块对照 Python
//!     `config.py`，直接读写 `~/.cc-partner/config.json`，并在首次更名后从旧
//!     `~/.claude-partner` 目录迁移，保证旧用户配置不丢失。
//!
//! Code Logic（这个模块做什么）:
//!     - 用 `dirs` crate 定位 home 目录，拼接配置文件路径。
//!     - `load()` 读 JSON；缺失则生成默认（uuid v4 设备 ID、hostname 设备名）。
//!     - `save()` 序列化为紧凑 JSON（UTF-8，中文不转义）写回。
//!     - macOS 下把旧配置里的 `<ctrl>` 快捷键迁移为 `<cmd>`（对齐 Python 行为）。

use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const CONFIG_DIR_NAME: &str = ".cc-partner";
const LEGACY_CONFIG_DIR_NAME: &str = ".claude-partner";
const APP_NAME: &str = "cc-partner";
/// CLI/测试隔离数据根目录环境变量。
///
/// Business Logic: smoke/CI 与并行 CLI 用例需要把 config/control/db/log 写到临时目录，
///     不能触碰用户真实 `~/.cc-partner`；`start` 子进程 detach 后也要继承同一 override。
const DATA_DIR_ENV: &str = "CC_PARTNER_DATA_DIR";

/// 把 cfg.db_path 中残留的旧 `~/.claude-partner/` 前缀改写为 `~/.cc-partner/`。
///
/// Business Logic: `config_dir()` 用 `fs::rename` 把整个旧目录搬到新目录，**不会**
///     改写任何文件内容——而 `db_path` 是绝对路径字段，旧 config.json 里残留
///     `~/.claude-partner/data.db` 会让 `init_db` 找不到文件触发 SQLITE_CANTOPEN
///     panic。必须在 load 时按 home 目录做一次字段级迁移并 save。
/// Code Logic: 仅当 `db_path` 以 `{home}/.claude-partner/` 开头时，把前缀替换成
///     `{home}/.cc-partner/`；其它情况（含新路径、第三方目录）原样保留。
///     返回 `true` 表示发生改写。
fn migrate_legacy_db_path(cfg: &mut AppConfig) -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    migrate_legacy_db_path_with_home(cfg, &home)
}

/// 同 `migrate_legacy_db_path`，但 home 由调用方传入，便于单测。
pub(crate) fn migrate_legacy_db_path_with_home(cfg: &mut AppConfig, home: &Path) -> bool {
    let legacy_prefix = format!("{}/{}", home.to_string_lossy(), LEGACY_CONFIG_DIR_NAME);
    if !cfg.db_path.starts_with(&legacy_prefix) {
        return false;
    }
    let new_prefix = format!("{}/{}", home.to_string_lossy(), CONFIG_DIR_NAME);
    let old = std::mem::take(&mut cfg.db_path);
    cfg.db_path = old.replacen(&legacy_prefix, &new_prefix, 1);
    true
}

/// 解析应用运行时数据根目录（配置/控制文件/数据库/日志的共同父目录）。
///
/// Business Logic（为什么需要这个函数）:
///     CLI 与集成 smoke 需要通过 `CC_PARTNER_DATA_DIR` 把后端状态隔离到临时目录，
///     避免污染用户真实 `~/.cc-partner`；未设置时必须保持现有 home 默认路径。
///
/// Code Logic（这个函数做什么）:
///     1) 读取 `CC_PARTNER_DATA_DIR`；空白/NUL/非绝对路径返回 Validation 错误；
///     2) 合法 override 则 `create_dir_all` 后返回该绝对路径；
///     3) 无 override 时复用 home 下 `.cc-partner`，并保留旧 `.claude-partner` 目录迁移。
pub fn data_dir() -> Result<PathBuf, AppError> {
    match std::env::var_os(DATA_DIR_ENV) {
        Some(raw) => resolve_data_dir_override(raw),
        None => Ok(default_home_data_dir()),
    }
}

/// 校验并物化 `CC_PARTNER_DATA_DIR` override。
///
/// Business Logic（为什么需要这个函数）:
///     非法 override（空串、相对路径、含 NUL）会让 detach 子进程写到不可预期位置，必须在入口拒绝。
///
/// Code Logic（这个函数做什么）:
///     拒绝含 NUL 字节、trim 后为空、非绝对路径的值；通过后递归创建目录并返回 PathBuf。
fn resolve_data_dir_override(raw: std::ffi::OsString) -> Result<PathBuf, AppError> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        if raw.as_bytes().contains(&0) {
            return Err(AppError::validation(
                "CC_PARTNER_DATA_DIR 不能包含 NUL 字节",
            ));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        if raw.encode_wide().any(|unit| unit == 0) {
            return Err(AppError::validation(
                "CC_PARTNER_DATA_DIR 不能包含 NUL 字节",
            ));
        }
    }

    let as_str = raw.to_string_lossy();
    let trimmed = as_str.trim();
    if trimmed.is_empty() {
        return Err(AppError::validation("CC_PARTNER_DATA_DIR 不能为空"));
    }
    if trimmed.contains('\0') {
        return Err(AppError::validation(
            "CC_PARTNER_DATA_DIR 不能包含 NUL 字节",
        ));
    }

    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        return Err(AppError::validation(
            "CC_PARTNER_DATA_DIR 必须是绝对路径",
        ));
    }

    fs::create_dir_all(&path)?;
    Ok(path)
}

/// 默认 home 数据目录（含旧目录迁移）。
///
/// Business Logic（为什么需要这个函数）:
///     未设置 `CC_PARTNER_DATA_DIR` 时生产路径必须与历史行为一致，并继续迁移旧 `.claude-partner`。
///
/// Code Logic（这个函数做什么）:
///     取 home 下 `.cc-partner`；若新目录不存在且旧目录存在则 rename 迁移，失败则回落旧路径。
fn default_home_data_dir() -> PathBuf {
    // dirs::config_dir 在各平台指向用户配置目录；历史 Python 版用的是 home 下的隐藏目录。
    // 更名后优先使用 ~/.cc-partner；若新目录不存在但旧 ~/.claude-partner 存在，首次启动时重命名迁移。
    let home = dirs::home_dir().expect("无法定位用户 home 目录，环境异常");
    let dir = home.join(CONFIG_DIR_NAME);
    let legacy = home.join(LEGACY_CONFIG_DIR_NAME);

    if !dir.exists() && legacy.exists() {
        match fs::rename(&legacy, &dir) {
            Ok(()) => tracing::info!("已迁移配置目录: {:?} -> {:?}", legacy, dir),
            Err(e) => {
                tracing::warn!("迁移配置目录失败，将继续使用旧目录 {:?}: {e}", legacy);
                return legacy;
            }
        }
    }

    dir
}

/// 配置文件和数据文件的根目录：`~/.cc-partner`（可被 `CC_PARTNER_DATA_DIR` 覆盖）。
///
/// Business Logic（为什么需要这个函数）:
///     既有模块（cloud_sync、control、load/save）统一通过此入口派生路径；测试/CLI 隔离时
///     必须与 `data_dir()` 指向同一根。
///
/// Code Logic（这个函数做什么）:
///     委托 `data_dir()`；解析失败时 panic（非法 override 属于环境配置错误，应尽早暴露）。
///
/// pub 供 cloud_sync 等模块复用同一根目录派生子路径（如 `~/.cc-partner/cloud-sync/`）。
pub fn config_dir() -> PathBuf {
    data_dir().expect("无法解析应用数据目录（检查 CC_PARTNER_DATA_DIR）")
}

/// 后端文件日志目录：`<data_dir>/logs`。
///
/// Business Logic（为什么需要这个函数）:
///     doctor/smoke 与后续 rotating logs 需要与 config/control 同一隔离根下的日志路径。
///
/// Code Logic（这个函数做什么）:
///     基于 `data_dir()` 拼接 `logs` 子目录路径（本函数不强制创建子目录）。
///
/// 当前生产路径尚未接线日志 writer；API 先落地供 data_dir 隔离契约与后续 logs plan 复用。
#[allow(dead_code)]
pub fn backend_log_dir() -> Result<PathBuf, AppError> {
    Ok(data_dir()?.join("logs"))
}

/// 后端当前日志文件路径：`<data_dir>/logs/backend.log`。
///
/// Business Logic（为什么需要这个函数）:
///     doctor 与日志读取需要固定文件名定位当前 backend 日志。
///
/// Code Logic（这个函数做什么）:
///     基于 `backend_log_dir()` 拼接 `backend.log`。
#[allow(dead_code)]
pub fn backend_log_path() -> Result<PathBuf, AppError> {
    Ok(backend_log_dir()?.join("backend.log"))
}

/// 配置文件完整路径：`~/.cc-partner/config.json`
fn config_file_path() -> PathBuf {
    config_dir().join("config.json")
}

/// 默认数据库路径：`~/.cc-partner/data.db`
pub fn default_db_path() -> PathBuf {
    config_dir().join("data.db")
}

/// 默认文件接收目录：`~/cc-partner-files`
fn default_receive_dir() -> PathBuf {
    dirs::home_dir()
        .expect("无法定位用户 home 目录，环境异常")
        .join("cc-partner-files")
}

/// 云端同步（GitHub 私有仓库）的默认轮询间隔（秒）= 10 分钟。
///
/// Business Logic: 自动同步的合理默认节奏：既不至于过于频繁（无谓 IO/git 操作），
///     也不至于太慢（用户切设备后等待过久）。10 分钟是一个保守默认，用户可在设置页调小。
fn default_cloud_sync_interval() -> u64 {
    600
}

/// GitHub Trending 缓存默认有效期（小时）= 24 小时。
///
/// Business Logic: 首页周热门每天刷新一次即可，避免用户频繁打开首页时重复抓取 GitHub
///     或反复调用本地 Claude Code CLI 生成解说。
fn default_trending_cache_ttl_hours() -> i64 {
    24
}

/// GitHub Trending 解说默认 Claude CLI 命令。
///
/// Business Logic: 大多数用户会把 Claude Code CLI 放入 PATH，默认使用 `claude` 最通用。
fn default_claude_cli_path() -> String {
    "claude".to_string()
}

/// GitHub Trending 解说默认 Claude 模型别名。
fn default_claude_model() -> String {
    "sonnet".to_string()
}

/// 平台相关默认截图快捷键：macOS 用 `<cmd>+<shift>+s`，其他平台 `<ctrl>+<shift>+s`
fn default_screenshot_hotkey() -> String {
    if cfg!(target_os = "macos") {
        "<cmd>+<shift>+s".to_string()
    } else {
        "<ctrl>+<shift>+s".to_string()
    }
}

/// Workbench Prompt 优化默认快捷键：轻按 Control 单键。
fn default_prompt_optimizer_hotkey() -> String {
    "<ctrl>".to_string()
}

/// Workbench Prompt 优化默认填入语言：中文优化版。
fn default_prompt_optimizer_fill_language() -> String {
    "zh".to_string()
}

/// 归一化 Workbench Prompt 优化填入语言。
///
/// Business Logic（为什么需要这个函数）:
///     配置文件或前端更新可能传入异常值，填入终端时只能在中文/英文优化版中二选一。
///
/// Code Logic（这个函数做什么）:
///     仅保留 "en"，其他值统一回退 "zh"。
pub(crate) fn normalize_prompt_optimizer_fill_language(value: &str) -> String {
    if value.trim().eq_ignore_ascii_case("en") {
        "en".to_string()
    } else {
        "zh".to_string()
    }
}

/// 获取本机 hostname 作为默认设备名（对应 Python 的 socket.gethostname()）
fn default_device_name() -> String {
    // 优先用系统 hostname；失败则回退到应用名。
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| APP_NAME.to_string())
}

/// 生成可由设置页恢复的偏好默认值。
///
/// Business Logic（为什么需要）:
///     设置页“恢复默认”需要使用后端环境感知默认值（hostname、home 下接收目录、
///     平台快捷键），避免前端用空字符串或硬编码路径覆盖真实基础设置。
///
/// Code Logic（这个函数做什么）:
///     调用现有默认值函数，返回 `(device_name, receive_dir, screenshot_hotkey,
///     prompt_optimizer_hotkey, prompt_optimizer_fill_language)`；
///     receive_dir 转成字符串以便命令层直接组装 DTO。
pub(crate) fn default_preference_values() -> (String, String, String, String, String) {
    (
        default_device_name(),
        default_receive_dir().to_string_lossy().to_string(),
        default_screenshot_hotkey(),
        default_prompt_optimizer_hotkey(),
        default_prompt_optimizer_fill_language(),
    )
}

/// 生成云端同步的默认偏好值。
///
/// Business Logic（为什么需要）:
///     设置页同步 tab 的“恢复默认”应与 AppConfig 首次生成的云同步默认值一致，
///     避免前端维护第二套默认常量导致漂移。
///
/// Code Logic（这个函数做什么）:
///     返回 repo_url/enabled/auto/interval_secs/branch 的默认元组，供命令层组装 DTO。
pub(crate) fn default_cloud_sync_values() -> (Option<String>, bool, bool, u64, Option<String>) {
    (None, false, false, default_cloud_sync_interval(), None)
}

/// Orchestrator 自动化并发上限默认值。
///
/// Business Logic（为什么需要这个函数）:
///     自动编排器默认只能同时推进一个任务，避免用户首次启用时对本机 Git/终端资源造成过大压力。
///
/// Code Logic（这个函数做什么）:
///     返回 serde 字段级默认值 1，供旧 config.json 缺少该字段时回退。
fn default_orchestrator_max_concurrent_tasks() -> i64 {
    1
}

/// Orchestrator 自动化全局配置。
///
/// Business Logic（为什么需要这个结构）:
///     自动化策略属于本设备运行偏好，不需要按项目分叉；Settings 自动化 tab 需要持久化
///     scheduler 开关、并发上限、验证命令和 full-auto delivery 开关。
///
/// Code Logic（这个结构做什么）:
///     纯 serde 配置载体，落盘在 AppConfig.orchestrator 下；所有字段有默认值，保证旧
///     config.json 缺少 orchestrator 字段时可正常反序列化。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorAutomationConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_orchestrator_max_concurrent_tasks")]
    pub max_concurrent_tasks: i64,
    #[serde(default)]
    pub verification_commands: Vec<String>,
    #[serde(default = "default_true")]
    pub auto_commit: bool,
    #[serde(default = "default_true")]
    pub auto_push_task_branch: bool,
    #[serde(default = "default_true")]
    pub auto_merge_to_main: bool,
    #[serde(default = "default_true")]
    pub auto_push_main: bool,
}

impl Default for OrchestratorAutomationConfig {
    /// 提供 Orchestrator 自动化配置全套默认值。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     旧配置升级和设置页恢复默认都需要一致的 full-auto-but-disabled 默认策略。
    ///
    /// Code Logic（这个函数做什么）:
    ///     enabled=false、max_concurrent_tasks=1、验证命令为空，四个 delivery flag=true。
    fn default() -> Self {
        Self {
            enabled: false,
            max_concurrent_tasks: default_orchestrator_max_concurrent_tasks(),
            verification_commands: Vec::new(),
            auto_commit: true,
            auto_push_task_branch: true,
            auto_merge_to_main: true,
            auto_push_main: true,
        }
    }
}

/// GitHub 周热门首页配置。
///
/// Business Logic（为什么需要这个结构）:
///     首页需要每日抓取 GitHub Trending Weekly，并可选调用本地 Claude Code CLI 生成中英文解说。
///     CLI 路径、模型和缓存时长属于用户环境偏好，必须持久化，且旧配置升级时需安全回退默认值。
///
/// Code Logic（这个结构做什么）:
///     纯配置载体，落盘在 AppConfig.github_trending 下。所有字段都有 serde default，
///     保证旧 config.json 缺字段时也能反序列化。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubTrendingConfig {
    /// 是否启用 Claude CLI 解说生成。关闭时仅展示 GitHub 原始描述。
    #[serde(default = "default_true")]
    pub ai_enabled: bool,
    /// Claude Code CLI 路径或命令名。
    #[serde(default = "default_claude_cli_path")]
    pub claude_cli_path: String,
    /// Claude Code CLI 模型别名或完整模型名。
    #[serde(default = "default_claude_model")]
    pub claude_model: String,
    /// 缓存有效期（小时），默认 24。
    #[serde(default = "default_trending_cache_ttl_hours")]
    pub cache_ttl_hours: i64,
}

impl Default for GithubTrendingConfig {
    fn default() -> Self {
        Self {
            ai_enabled: true,
            claude_cli_path: default_claude_cli_path(),
            claude_model: default_claude_model(),
            cache_ttl_hours: default_trending_cache_ttl_hours(),
        }
    }
}

/// 健康提醒配置(久坐监测 + 喝水提醒)。
///
/// Business Logic（为什么需要这个结构）:
///     M10 健康提醒功能需要可配置的久坐监测参数(工作窗口、有效休息时长、喝水间隔、明细保留天数)、
///     系统通知开关与免打扰时段。喝水提醒与全屏遮罩随健康监测固定启用,不再给用户独立开关。
///     这些偏好需跨多次运行持久化,且旧用户升级时其 config.json
///     尚无 health 字段,故每个字段均用 `#[serde(default = "...")]` 回退默认值,保证向后兼容。
///
/// Code Logic（这个结构做什么）:
///     纯数据载体(serde Serialize/Deserialize),字段 snake_case 落盘。`Default` 提供全套默认;
///     各 `default_*` 函数供 serde 在单字段缺失时回退(与 Default 字面值一致)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthConfig {
    /// 久坐监测总开关,默认开启(用户决策:装好即生效)
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 工作窗口长度(秒),默认 45 分钟
    #[serde(default = "default_work_window")]
    pub work_window_seconds: i64,
    /// 有效休息判定时长(秒),默认 5 分钟(连续无操作达此值才算休息)
    #[serde(default = "default_break")]
    pub break_seconds: i64,
    /// 是否记录窗口标题(最细粒度统计),默认开;关闭则降级到「只记进程名」
    #[serde(default = "default_true")]
    pub record_window_title: bool,
    /// 明细保留天数,默认 90;超期清理
    #[serde(default = "default_retain_days")]
    pub retain_days: i64,
    /// 系统通知提醒开关(Plan 1 唯一提醒方式)
    #[serde(default = "default_true")]
    pub notify_enabled: bool,
    /// 免打扰开始 "HH:MM"(含),None 表示无免打扰
    #[serde(default)]
    pub dnd_start: Option<String>,
    /// 免打扰结束 "HH:MM"(不含),支持跨午夜(如 22:00-07:00)
    #[serde(default)]
    pub dnd_end: Option<String>,
    /// 喝水提醒历史开关,运行时固定按健康监测总开关启用;保留字段用于读取旧配置。
    #[serde(default = "default_true")]
    pub water_enabled: bool,
    /// 喝水提醒间隔(秒),默认 1 小时(3600 秒)
    #[serde(default = "default_water_interval")]
    pub water_interval_seconds: i64,
    /// 全屏遮罩提醒历史开关,运行时固定启用;保留字段用于读取旧配置和 DTO 兼容。
    /// 缺字段时回退 true,确保升级后默认启动全屏遮罩提醒。
    #[serde(default = "default_true")]
    pub reminder_fullscreen: bool,
}

impl Default for HealthConfig {
    /// 提供健康提醒配置全套默认值。
    ///
    /// Business Logic: 久坐监测默认开启,45 分钟工作窗口 + 5 分钟有效休息,
    ///                  喝水提醒与全屏遮罩随健康监测启用,记录窗口标题,
    ///                  明细保留 90 天,通知开启,无免打扰。
    /// Code Logic: 返回各字段默认值常量,与 serde 单字段缺失时的 default_* 回退值一致。
    fn default() -> Self {
        Self {
            enabled: true,
            work_window_seconds: 45 * 60,
            break_seconds: 5 * 60,
            record_window_title: true,
            retain_days: 90,
            notify_enabled: true,
            dnd_start: None,
            dnd_end: None,
            water_enabled: true,
            water_interval_seconds: 60 * 60,
            reminder_fullscreen: true,
        }
    }
}

/// serde 单字段缺失回退:布尔默认 true。
///
/// Business Logic: enabled/record_window_title/notify_enabled 等布尔偏好默认开启。
/// Code Logic: 返回 `true` 字面量,供 `#[serde(default = "default_true")]` 调用。
fn default_true() -> bool {
    true
}

/// serde 单字段缺失回退:工作窗口默认 45 分钟(2700 秒)。
///
/// Business Logic: 久坐监测以 45 分钟为标准工作窗口。
/// Code Logic: 返回 `45 * 60`,供 `#[serde(default = "default_work_window")]` 调用。
fn default_work_window() -> i64 {
    45 * 60
}

/// serde 单字段缺失回退:有效休息默认 5 分钟(300 秒)。
///
/// Business Logic: 连续无操作达 5 分钟才判定为一次有效休息。
/// Code Logic: 返回 `5 * 60`,供 `#[serde(default = "default_break")]` 调用。
fn default_break() -> i64 {
    5 * 60
}

/// serde 单字段缺失回退:明细保留默认 90 天。
///
/// Business Logic: 健康明细保留 90 天,超期清理避免无限增长。
/// Code Logic: 返回 `90`,供 `#[serde(default = "default_retain_days")]` 调用。
fn default_retain_days() -> i64 {
    90
}

/// serde 单字段缺失回退:喝水提醒默认间隔 1 小时(3600 秒)。
///
/// Business Logic: 久坐用户每小时提醒一次喝水,避免长时间忘饮水。
/// Code Logic: 返回 `60 * 60`,供 `#[serde(default = "default_water_interval")]` 调用。
fn default_water_interval() -> i64 {
    60 * 60
}

/// 应用全局配置。字段命名与 Python `AppConfig` dataclass 一致（snake_case 用于磁盘持久化）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// 设备唯一标识（UUID v4，首次运行生成）
    pub device_id: String,
    /// 设备显示名（默认为主机名）
    pub device_name: String,
    /// HTTP 服务首选端口；0 表示使用后端固定默认端口，端口被占时由 HTTP server 自动递增
    pub http_port: i64,
    /// 文件接收保存目录
    pub receive_dir: String,
    /// SQLite 数据库路径
    pub db_path: String,
    /// 截图快捷键
    pub screenshot_hotkey: String,
    /// Workbench Prompt 优化小组件快捷键（页面内生效，支持 `<ctrl>` 单修饰键）。
    #[serde(default = "default_prompt_optimizer_hotkey")]
    pub prompt_optimizer_hotkey: String,
    /// Workbench Prompt 优化结果自动填入语言：`zh` 或 `en`。
    #[serde(default = "default_prompt_optimizer_fill_language")]
    pub prompt_optimizer_fill_language: String,
    /// 云端同步（GitHub 私有仓库）的远端仓库 URL（如 git@github.com:user/repo.git）。
    /// None 表示未配置云端同步；配置后 scheduler 才会真正 clone/fetch/push。
    #[serde(default)]
    pub cloud_sync_repo_url: Option<String>,
    /// 云端同步总开关（前端设置页可切换）。false 时 scheduler 每 tick 仅空转不执行同步。
    #[serde(default)]
    pub cloud_sync_enabled: bool,
    /// 是否启用自动同步（scheduler 后台轮询）。false 时只支持手动触发 trigger_cloud_sync。
    #[serde(default)]
    pub cloud_sync_auto: bool,
    /// 自动同步轮询间隔（秒），默认 600（10 分钟）。scheduler 每 tick 重读此值实时生效。
    #[serde(default = "default_cloud_sync_interval")]
    pub cloud_sync_interval_secs: u64,
    /// 指定同步用分支（如 main）。None 时使用远端默认分支（origin/HEAD）。
    #[serde(default)]
    pub cloud_sync_branch: Option<String>,
    /// 健康提醒配置(久坐监测 + 喝水提醒)。`#[serde(default)]` 保证旧 config.json
    /// (无 health 字段)反序列化时整体回退 `HealthConfig::default()`。
    #[serde(default)]
    pub health: HealthConfig,
    /// Orchestrator 自动化全局配置。`#[serde(default)]` 兼容旧 config.json。
    #[serde(default)]
    pub orchestrator: OrchestratorAutomationConfig,
    /// GitHub 周热门首页与 Claude CLI 解说配置。`#[serde(default)]` 兼容旧 config.json。
    #[serde(default)]
    pub github_trending: GithubTrendingConfig,
}

impl AppConfig {
    /// 加载配置；文件不存在则生成默认配置并保存。
    ///
    /// Business Logic: 启动时读取上次配置；首次运行初始化默认值并落盘。
    /// Code Logic: 读 JSON 反序列化；做两步迁移修复后 save()：
    ///             1) macOS 旧配置中 `<ctrl>` 快捷键替换为 `<cmd>`（对照 config.py）；
    ///             2) `db_path` 字段若仍指向已废弃的 `~/.claude-partner/`（目录迁移只
    ///                重命名目录、不改 JSON 字段），改写为 `~/.cc-partner/`。
    ///             文件缺失则用默认值构造并 save()。
    pub fn load() -> Result<Self, AppError> {
        let path = config_file_path();
        if path.exists() {
            let text = fs::read_to_string(&path)?;
            let mut cfg: AppConfig = serde_json::from_str(&text)?;
            // macOS 迁移：旧配置中 <ctrl> 快捷键自动替换为 <cmd>（对照 config.py）
            if cfg!(target_os = "macos") && cfg.screenshot_hotkey.contains("<ctrl>") {
                cfg.screenshot_hotkey = cfg.screenshot_hotkey.replace("<ctrl>", "<cmd>");
                cfg.save()?;
            }
            // 目录迁移补丁：config_dir() 把 ~/.claude-partner 整目录重命名成 ~/.cc-partner，
            // 但 config.json 里的 db_path 是绝对路径，目录迁移不会改 JSON 字段内容。
            // 残留的旧路径会让 init_db 找不到文件而 panic (SQLITE_CANTOPEN)。
            if migrate_legacy_db_path(&mut cfg) {
                tracing::info!("已迁移 db_path 字段到新配置目录: {}", cfg.db_path);
                cfg.save()?;
            }
            Ok(cfg)
        } else {
            // 首次运行，生成默认配置
            let cfg = AppConfig {
                device_id: Uuid::new_v4().to_string(),
                device_name: default_device_name(),
                http_port: 0,
                receive_dir: default_receive_dir().to_string_lossy().to_string(),
                db_path: default_db_path().to_string_lossy().to_string(),
                screenshot_hotkey: default_screenshot_hotkey(),
                prompt_optimizer_hotkey: default_prompt_optimizer_hotkey(),
                prompt_optimizer_fill_language: default_prompt_optimizer_fill_language(),
                cloud_sync_repo_url: None,
                cloud_sync_enabled: false,
                cloud_sync_auto: false,
                cloud_sync_interval_secs: default_cloud_sync_interval(),
                cloud_sync_branch: None,
                health: HealthConfig::default(),
                orchestrator: OrchestratorAutomationConfig::default(),
                github_trending: GithubTrendingConfig::default(),
            };
            cfg.save()?;
            Ok(cfg)
        }
    }

    /// 保存配置到 `~/.cc-partner/config.json`。
    ///
    /// Business Logic: 用户修改配置后需持久化，下次启动生效。
    /// Code Logic: 确保目录存在；序列化为 UTF-8 JSON（紧凑，中文不转义）写入。
    pub fn save(&self) -> Result<(), AppError> {
        let dir = config_dir();
        ensure_dir(&dir)?;
        let path = config_file_path();
        // serde_json::to_string 生成紧凑标准 JSON，与 Python json.dumps(ensure_ascii=False) 互通
        let text = serde_json::to_string(self)?;
        fs::write(&path, text)?;
        Ok(())
    }
}

/// 确保目录存在（递归创建），对应 Python 的 `Path.mkdir(parents=True, exist_ok=True)`。
fn ensure_dir(path: &Path) -> Result<(), AppError> {
    if !path.exists() {
        fs::create_dir_all(path)?;
    }
    Ok(())
}

// 依赖 hostname crate 取主机名（对照 Python socket.gethostname）
// 注意：该 crate 需加入 Cargo.toml

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// 进程级 `CC_PARTNER_DATA_DIR` 环境变量锁。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     多个 data_dir 测试会改写同一进程环境变量，必须串行避免互相污染。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用 OnceLock 初始化全局 Mutex，所有相关测试共享同一把锁。
    fn data_dir_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    /// 测试用 `CC_PARTNER_DATA_DIR` 环境隔离守卫。
    ///
    /// Business Logic（为什么需要这个结构）:
    ///     单元测试需要临时注入/清空 override，并在 panic 后仍恢复真实环境，避免污染其它用例。
    ///
    /// Code Logic（这个结构做什么）:
    ///     持有全局锁与原始 env 值；Drop 时按原值恢复或移除变量。
    struct DataDirEnvGuard {
        _lock: MutexGuard<'static, ()>,
        previous: Option<OsString>,
    }

    impl Drop for DataDirEnvGuard {
        /// 恢复测试前的 `CC_PARTNER_DATA_DIR`。
        ///
        /// Business Logic（为什么需要这个函数）:
        ///     即使断言失败或 panic，后续测试也不能继续看到错误的数据目录 override。
        ///
        /// Code Logic（这个函数做什么）:
        ///     有原值则 set_var，无原值则 remove_var。
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(DATA_DIR_ENV, value),
                None => std::env::remove_var(DATA_DIR_ENV),
            }
        }
    }

    /// 安装临时 `CC_PARTNER_DATA_DIR`（或清除）并返回 RAII 守卫。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     测试需要可控地模拟“有/无 override”和非法值，而不污染开发者真实环境。
    ///
    /// Code Logic（这个函数做什么）:
    ///     加锁后保存原值；`Some(path)` 时 set_var，`None` 时 remove_var。
    fn install_data_dir_env(value: Option<&str>) -> DataDirEnvGuard {
        let lock = data_dir_env_lock()
            .lock()
            .expect("CC_PARTNER_DATA_DIR 测试锁中毒");
        let previous = std::env::var_os(DATA_DIR_ENV);
        match value {
            Some(path) => std::env::set_var(DATA_DIR_ENV, path),
            None => std::env::remove_var(DATA_DIR_ENV),
        }
        DataDirEnvGuard {
            _lock: lock,
            previous,
        }
    }

    /// 验证合法绝对路径 override 会改写 config/control/database/log 派生路径。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     CLI/smoke 测试必须把运行时状态隔离到临时目录，不能碰真实 `~/.cc-partner`。
    ///
    /// Code Logic（这个测试做什么）:
    ///     设置绝对 `CC_PARTNER_DATA_DIR`，断言 data_dir/config_dir/db/log 都落在该目录下，
    ///     且目录被创建；退出时守卫恢复环境。
    #[test]
    fn data_dir_override_rewrites_config_control_db_and_log_paths() {
        let temp = tempfile::tempdir().expect("应能创建临时数据目录");
        let override_dir = temp.path().join("isolated-data");
        let _guard = install_data_dir_env(Some(override_dir.to_str().expect("临时路径应为 UTF-8")));

        let resolved = data_dir().expect("合法绝对路径 override 应成功");
        assert_eq!(resolved, override_dir);
        assert!(
            resolved.is_dir(),
            "data_dir 应确保 override 目录存在: {:?}",
            resolved
        );
        assert_eq!(config_dir(), override_dir);
        assert_eq!(default_db_path(), override_dir.join("data.db"));
        assert_eq!(
            backend_log_dir().expect("log 目录应可解析"),
            override_dir.join("logs")
        );
        assert_eq!(
            backend_log_path().expect("log 路径应可解析"),
            override_dir.join("logs").join("backend.log")
        );
        assert_eq!(
            crate::backend::control::control_file_path(),
            override_dir.join("backend-control.json")
        );
        assert_eq!(
            crate::backend::control::pid_file_path(),
            override_dir.join("backend.pid")
        );
    }

    /// 验证未设置 override 时仍使用 home 下默认目录。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     生产 GUI/CLI 默认路径不得因引入测试 override 而改变。
    ///
    /// Code Logic（这个测试做什么）:
    ///     清除 `CC_PARTNER_DATA_DIR` 后，断言 data_dir/config_dir 指向 `~/.cc-partner`（或已迁移路径）。
    #[test]
    fn data_dir_without_override_keeps_home_default() {
        let _guard = install_data_dir_env(None);
        let home = dirs::home_dir().expect("测试环境应有 home");
        let expected = home.join(CONFIG_DIR_NAME);
        let resolved = data_dir().expect("默认路径应成功");
        // 若旧目录迁移失败可能回落 legacy；生产路径仍应位于 home 下应用目录。
        assert!(
            resolved == expected || resolved == home.join(LEGACY_CONFIG_DIR_NAME),
            "默认 data_dir 应位于 home 应用目录，实际: {:?}",
            resolved
        );
        assert_eq!(config_dir(), resolved);
        assert_eq!(default_db_path(), resolved.join("data.db"));
    }

    /// 验证空白 override 被拒绝。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     空字符串会让路径解析落到相对/当前目录，破坏隔离与用户数据安全。
    ///
    /// Code Logic（这个测试做什么）:
    ///     设置空字符串与仅空白字符串，断言 data_dir 返回错误。
    #[test]
    fn data_dir_rejects_blank_override() {
        let _guard = install_data_dir_env(Some(""));
        assert!(
            data_dir().is_err(),
            "空字符串 override 应被拒绝"
        );

        drop(_guard);
        let _guard = install_data_dir_env(Some("   "));
        assert!(
            data_dir().is_err(),
            "纯空白 override 应被拒绝"
        );
    }

    /// 验证相对路径 override 被拒绝。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     相对路径依赖 cwd，CI 与 detach 子进程 cwd 不稳定，不能作为数据根。
    ///
    /// Code Logic（这个测试做什么）:
    ///     设置相对路径，断言 data_dir 返回错误。
    #[test]
    fn data_dir_rejects_relative_override() {
        let _guard = install_data_dir_env(Some("relative-data-dir"));
        let err = data_dir().expect_err("相对路径 override 应被拒绝");
        assert!(
            err.to_string().contains("绝对") || err.to_string().contains("absolute"),
            "错误应提示需要绝对路径，实际: {err}"
        );
    }

    #[test]
    fn test_health_config_defaults() {
        let h = HealthConfig::default();
        assert!(h.enabled);
        assert_eq!(h.work_window_seconds, 45 * 60);
        assert_eq!(h.break_seconds, 5 * 60);
        assert!(h.record_window_title);
        assert_eq!(h.retain_days, 90);
        assert!(h.dnd_start.is_none());
        assert!(h.water_enabled);
        assert!(h.reminder_fullscreen);
    }

    #[test]
    fn test_old_config_without_health_field_loads_with_defaults() {
        // 模拟迁移前无 health 字段的旧 config.json
        let old_json = r#"{
            "device_id":"dev_x","device_name":"mac","http_port":0,
            "receive_dir":"/tmp","db_path":"/tmp/data.db","screenshot_hotkey":"<cmd>+<shift>+s"
        }"#;
        let cfg: AppConfig = serde_json::from_str(old_json).unwrap();
        assert!(
            cfg.health.enabled,
            "旧 config 缺 health 字段时应回退默认 enabled=true"
        );
        assert_eq!(cfg.health.work_window_seconds, 45 * 60);
        assert!(cfg.github_trending.ai_enabled);
        assert_eq!(cfg.github_trending.claude_cli_path, "claude");
        assert_eq!(cfg.prompt_optimizer_hotkey, "<ctrl>");
        assert_eq!(cfg.prompt_optimizer_fill_language, "zh");
        assert!(!cfg.orchestrator.enabled);
        assert_eq!(cfg.orchestrator.max_concurrent_tasks, 1);
        assert!(cfg.orchestrator.auto_commit);
    }

    #[test]
    fn test_health_config_roundtrip() {
        let cfg = AppConfig {
            device_id: "d".into(),
            device_name: "n".into(),
            http_port: 0,
            receive_dir: "/r".into(),
            db_path: "/db".into(),
            screenshot_hotkey: "<cmd>+s".into(),
            prompt_optimizer_hotkey: "<ctrl>".into(),
            prompt_optimizer_fill_language: "en".into(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: default_cloud_sync_interval(),
            cloud_sync_branch: None,
            health: HealthConfig {
                enabled: false,
                work_window_seconds: 30 * 60,
                break_seconds: 3 * 60,
                record_window_title: false,
                retain_days: 30,
                notify_enabled: false,
                dnd_start: Some("22:00".into()),
                dnd_end: Some("07:00".into()),
                water_enabled: true,
                water_interval_seconds: 1800,
                reminder_fullscreen: true,
            },
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.health.work_window_seconds, 30 * 60);
        assert!(!back.health.enabled);
        assert_eq!(back.health.dnd_start.as_deref(), Some("22:00"));
        assert!(back.health.water_enabled);
        assert_eq!(back.health.water_interval_seconds, 1800);
        assert!(
            back.health.reminder_fullscreen,
            "reminder_fullscreen 应随配置 roundtrip"
        );
        assert_eq!(back.prompt_optimizer_hotkey, "<ctrl>");
        assert_eq!(back.prompt_optimizer_fill_language, "en");
    }

    /// 最小可用 cfg 工厂：db_path 由调用方指定，其余字段填空字符串/默认值。
    /// 仅供 `migrate_legacy_db_path_with_home` 系列单测使用。
    fn cfg_with_db_path(db_path: &str) -> AppConfig {
        AppConfig {
            device_id: "dev-test".into(),
            device_name: "n".into(),
            http_port: 0,
            receive_dir: "/r".into(),
            db_path: db_path.into(),
            screenshot_hotkey: "<cmd>+<shift>+s".into(),
            prompt_optimizer_hotkey: "<ctrl>".into(),
            prompt_optimizer_fill_language: "zh".into(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: default_cloud_sync_interval(),
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
        }
    }

    #[test]
    fn test_migrate_legacy_db_path_rewrites_old_prefix() {
        // 旧 config.json 残留 ~/.claude-partner/ 绝对路径时，迁移函数应改写为 ~/.cc-partner/
        let home = Path::new("/tmp/fake-home");
        let mut cfg = cfg_with_db_path("/tmp/fake-home/.claude-partner/data.db");
        assert!(migrate_legacy_db_path_with_home(&mut cfg, home));
        assert_eq!(cfg.db_path, "/tmp/fake-home/.cc-partner/data.db");
    }

    #[test]
    fn test_migrate_legacy_db_path_noop_when_already_new() {
        // 已是新路径时不应改写
        let home = Path::new("/tmp/fake-home");
        let mut cfg = cfg_with_db_path("/tmp/fake-home/.cc-partner/data.db");
        assert!(!migrate_legacy_db_path_with_home(&mut cfg, home));
        assert_eq!(cfg.db_path, "/tmp/fake-home/.cc-partner/data.db");
    }

    #[test]
    fn test_migrate_legacy_db_path_noop_when_unrelated_dir() {
        // 用户自定义 db_path（如外接 SSD）不应被改写
        let home = Path::new("/tmp/fake-home");
        let mut cfg = cfg_with_db_path("/Volumes/external/cc-partner.db");
        assert!(!migrate_legacy_db_path_with_home(&mut cfg, home));
        assert_eq!(cfg.db_path, "/Volumes/external/cc-partner.db");
    }

    #[test]
    fn test_migrate_legacy_db_path_does_not_match_substring_only() {
        // 仅含 `.claude-partner` 子串但不在 home 下时不应改写
        // （避免误伤路径里恰好出现该字符串的合法目录）
        let home = Path::new("/tmp/fake-home");
        let mut cfg = cfg_with_db_path("/data/.claude-partner-backup/data.db");
        assert!(!migrate_legacy_db_path_with_home(&mut cfg, home));
        assert_eq!(cfg.db_path, "/data/.claude-partner-backup/data.db");
    }
}
