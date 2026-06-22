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
