//! config.rs — 应用配置：加载/保存/默认值生成
//!
//! Business Logic（为什么需要这个模块）:
//!     应用需在多次运行间保持一致的设备标识（device_id）和用户偏好（设备名、端口、
//!     接收目录、快捷键）。首次运行要生成默认配置并持久化。此模块对照 Python
//!     `config.py`，直接读写旧的 `~/.claude-partner/config.json`，保证旧用户配置不丢失。
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

/// 配置文件和数据文件的根目录：`~/.claude-partner`
fn config_dir() -> PathBuf {
    // dirs::config_dir 在各平台指向用户配置目录；但 Python 用的是 home/.claude-partner
    // 为与旧数据兼容，这里统一用 home_dir 拼接，与 Python 完全一致
    dirs::home_dir()
        .expect("无法定位用户 home 目录，环境异常")
        .join(".claude-partner")
}

/// 配置文件完整路径：`~/.claude-partner/config.json`
fn config_file_path() -> PathBuf {
    config_dir().join("config.json")
}

/// 默认数据库路径：`~/.claude-partner/data.db`
pub fn default_db_path() -> PathBuf {
    config_dir().join("data.db")
}

/// 默认文件接收目录：`~/ClaudePartnerFiles`
fn default_receive_dir() -> PathBuf {
    dirs::home_dir()
        .expect("无法定位用户 home 目录，环境异常")
        .join("ClaudePartnerFiles")
}

/// 平台相关默认截图快捷键：macOS 用 `<cmd>+<shift>+s`，其他平台 `<ctrl>+<shift>+s`
fn default_screenshot_hotkey() -> String {
    if cfg!(target_os = "macos") {
        "<cmd>+<shift>+s".to_string()
    } else {
        "<ctrl>+<shift>+s".to_string()
    }
}

/// 获取本机 hostname 作为默认设备名（对应 Python 的 socket.gethostname()）
fn default_device_name() -> String {
    // 优先用系统 hostname；失败则回退到 "Claude Partner"
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "Claude Partner".to_string())
}

/// 健康提醒配置(久坐监测 + 喝水提醒)。
///
/// Business Logic（为什么需要这个结构）:
///     M10 健康提醒功能需要可配置的久坐监测参数(工作窗口、有效休息时长、明细保留天数)、
///     系统通知开关与免打扰时段。这些偏好需跨多次运行持久化,且旧用户升级时其 config.json
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
}

impl Default for HealthConfig {
    /// 提供健康提醒配置全套默认值。
    ///
    /// Business Logic: 久坐监测默认开启,45 分钟工作窗口 + 5 分钟有效休息,
    ///                  记录窗口标题,明细保留 90 天,通知开启,无免打扰。
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
        }
    }
}

/// serde 单字段缺失回退:布尔默认 true。
///
/// Business Logic: enabled/record_window_title/notify_enabled 三个开关默认开启。
/// Code Logic: 返回 `true` 字面量,供 `#[serde(default = "default_true")]` 调用。
fn default_true() -> bool { true }

/// serde 单字段缺失回退:工作窗口默认 45 分钟(2700 秒)。
///
/// Business Logic: 久坐监测以 45 分钟为标准工作窗口。
/// Code Logic: 返回 `45 * 60`,供 `#[serde(default = "default_work_window")]` 调用。
fn default_work_window() -> i64 { 45 * 60 }

/// serde 单字段缺失回退:有效休息默认 5 分钟(300 秒)。
///
/// Business Logic: 连续无操作达 5 分钟才判定为一次有效休息。
/// Code Logic: 返回 `5 * 60`,供 `#[serde(default = "default_break")]` 调用。
fn default_break() -> i64 { 5 * 60 }

/// serde 单字段缺失回退:明细保留默认 90 天。
///
/// Business Logic: 健康明细保留 90 天,超期清理避免无限增长。
/// Code Logic: 返回 `90`,供 `#[serde(default = "default_retain_days")]` 调用。
fn default_retain_days() -> i64 { 90 }

/// 应用全局配置。字段命名与 Python `AppConfig` dataclass 一致（snake_case 用于磁盘持久化）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// 设备唯一标识（UUID v4，首次运行生成）
    pub device_id: String,
    /// 设备显示名（默认为主机名）
    pub device_name: String,
    /// HTTP 服务端口，0 表示系统自动分配
    pub http_port: i64,
    /// 文件接收保存目录
    pub receive_dir: String,
    /// SQLite 数据库路径
    pub db_path: String,
    /// 截图快捷键
    pub screenshot_hotkey: String,
    /// 健康提醒配置(久坐监测 + 喝水提醒)。`#[serde(default)]` 保证旧 config.json
    /// (无 health 字段)反序列化时整体回退 `HealthConfig::default()`。
    #[serde(default)]
    pub health: HealthConfig,
}

impl AppConfig {
    /// 加载配置；文件不存在则生成默认配置并保存。
    ///
    /// Business Logic: 启动时读取上次配置；首次运行初始化默认值并落盘。
    /// Code Logic: 读 JSON 反序列化；若 macOS 旧配置含 `<ctrl>` 则迁移为 `<cmd>` 并保存；
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
                health: HealthConfig::default(),
            };
            cfg.save()?;
            Ok(cfg)
        }
    }

    /// 保存配置到 `~/.claude-partner/config.json`。
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

    #[test]
    fn test_health_config_defaults() {
        let h = HealthConfig::default();
        assert!(h.enabled);
        assert_eq!(h.work_window_seconds, 45 * 60);
        assert_eq!(h.break_seconds, 5 * 60);
        assert!(h.record_window_title);
        assert_eq!(h.retain_days, 90);
        assert!(h.dnd_start.is_none());
    }

    #[test]
    fn test_old_config_without_health_field_loads_with_defaults() {
        // 模拟迁移前无 health 字段的旧 config.json
        let old_json = r#"{
            "device_id":"dev_x","device_name":"mac","http_port":0,
            "receive_dir":"/tmp","db_path":"/tmp/data.db","screenshot_hotkey":"<cmd>+<shift>+s"
        }"#;
        let cfg: AppConfig = serde_json::from_str(old_json).unwrap();
        assert!(cfg.health.enabled, "旧 config 缺 health 字段时应回退默认 enabled=true");
        assert_eq!(cfg.health.work_window_seconds, 45 * 60);
    }

    #[test]
    fn test_health_config_roundtrip() {
        let cfg = AppConfig {
            device_id: "d".into(), device_name: "n".into(), http_port: 0,
            receive_dir: "/r".into(), db_path: "/db".into(), screenshot_hotkey: "<cmd>+s".into(),
            health: HealthConfig { enabled: false, work_window_seconds: 30*60, break_seconds: 3*60,
                record_window_title: false, retain_days: 30, notify_enabled: false,
                dnd_start: Some("22:00".into()), dnd_end: Some("07:00".into()) },
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.health.work_window_seconds, 30 * 60);
        assert!(!back.health.enabled);
        assert_eq!(back.health.dnd_start.as_deref(), Some("22:00"));
    }
}
