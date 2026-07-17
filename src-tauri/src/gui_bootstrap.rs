//! gui_bootstrap.rs — GUI launcher-owned LAN disclosure bootstrap store。
//!
//! Business Logic（为什么需要这个模块）:
//!     GUI 在首次启动局域网 listener / 进入产品前必须获得用户对 LAN 风险的知情确认。
//!     该确认不得写入 sidecar-owned `AppConfig`，而应保存在 launcher 专属的 bootstrap
//!     文件（仅版本号 + 时间戳），以便在 sidecar 出生前原子读写。
//!     **开发壳与发布版分文件**：release → `gui-bootstrap.json`，dev → `gui-bootstrap.dev.json`，
//!     重置引导只清当前 flavor，互不影响。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `LAN_DISCLOSURE_VERSION` 与 `GuiBootstrapState`；按 `permissions::app_flavor`
//!     解析路径；提供加载、原子写入与“当前版本是否已确认”查询。Headless CLI 永不读取本文件。

use crate::config::data_dir;
use crate::error::AppError;
use crate::permissions::{app_flavor, AppFlavor};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// 当前 LAN 风险披露文案版本。
///
/// Business Logic（为什么需要这个常量）:
///     披露语义实质变化时需要用户重新确认；普通措辞微调不 bump 版本，避免打扰。
///
/// Code Logic（这个常量做什么）:
///     作为 bootstrap 文件中 `lanDisclosureVersion` 的权威当前值。
pub const LAN_DISCLOSURE_VERSION: u32 = 1;

/// 发布版 bootstrap 文件名（保持历史路径，兼容已确认用户）。
pub const GUI_BOOTSTRAP_FILE_RELEASE: &str = "gui-bootstrap.json";
/// 开发壳 bootstrap 文件名（与发布版隔离）。
pub const GUI_BOOTSTRAP_FILE_DEV: &str = "gui-bootstrap.dev.json";

/// Business Logic（为什么需要这个函数）:
///     Dev / Release 必须使用不同的 LAN 披露确认文件，重置一端不得清掉另一端。
///
/// Code Logic（这个函数做什么）:
///     Dev → `gui-bootstrap.dev.json`；Release → `gui-bootstrap.json`。
pub fn gui_bootstrap_file_name(flavor: AppFlavor) -> &'static str {
    match flavor {
        AppFlavor::Dev => GUI_BOOTSTRAP_FILE_DEV,
        AppFlavor::Release => GUI_BOOTSTRAP_FILE_RELEASE,
    }
}

/// 首选 HTTP/P2P TCP 端口（占用则递增，见 HTTP server 绑定策略）。
pub const PREFERRED_HTTP_PORT: u16 = 62116;

/// mDNS 发现使用的 UDP 端口。
pub const MDNS_PORT: u16 = 5353;

/// GUI launcher 拥有的 bootstrap 状态。
///
/// Business Logic（为什么需要这个结构）:
///     仅记录“用户确认了哪一版 LAN 风险披露、何时确认”，不得复制任何 sidecar 运行配置。
///
/// Code Logic（这个结构做什么）:
///     camelCase JSON：`lanDisclosureVersion` + 可选 `acknowledgedAt`（RFC3339）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiBootstrapState {
    pub lan_disclosure_version: u32,
    pub acknowledged_at: Option<String>,
}

impl Default for GuiBootstrapState {
    /// Business Logic（为什么需要这个函数）:
    ///     文件缺失或首次启动时需要明确“未确认”默认态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     版本 0、无时间戳，表示尚未确认当前披露。
    fn default() -> Self {
        Self {
            lan_disclosure_version: 0,
            acknowledged_at: None,
        }
    }
}

/// 返回当前进程 flavor 对应的 bootstrap 绝对路径。
///
/// Business Logic（为什么需要这个函数）:
///     launcher 与测试需要在同一 data_dir 根下定位 bootstrap，支持 `CC_PARTNER_DATA_DIR` 隔离；
///     开发壳与发布版分文件，避免共用确认状态。
///
/// Code Logic（这个函数做什么）:
///     `data_dir()` + `gui_bootstrap_file_name(app_flavor())`。
pub fn gui_bootstrap_path() -> Result<PathBuf, AppError> {
    Ok(data_dir()?.join(gui_bootstrap_file_name(app_flavor())))
}

/// 从磁盘加载 bootstrap 状态。
///
/// Business Logic（为什么需要这个函数）:
///     GUI 启动与状态查询需要判断用户是否已确认当前版本的 LAN 风险披露。
///
/// Code Logic（这个函数做什么）:
///     文件不存在返回 Default；存在则反序列化 camelCase JSON。损坏文件返回错误（fail-closed）。
pub fn load_gui_bootstrap() -> Result<GuiBootstrapState, AppError> {
    let path = gui_bootstrap_path()?;
    load_gui_bootstrap_from_path(&path)
}

/// 从指定路径加载 bootstrap（可测入口）。
///
/// Business Logic（为什么需要这个函数）:
///     单测需在临时目录验证读写语义，不触碰真实 data_dir。
///
/// Code Logic（这个函数做什么）:
///     路径不存在 → Default；读取 UTF-8 JSON 反序列化为 `GuiBootstrapState`。
pub fn load_gui_bootstrap_from_path(path: &Path) -> Result<GuiBootstrapState, AppError> {
    if !path.exists() {
        return Ok(GuiBootstrapState::default());
    }
    let content = fs::read_to_string(path)?;
    let state: GuiBootstrapState = serde_json::from_str(&content).map_err(|e| {
        AppError::generic(format!(
            "读取 {} 失败: {e}",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("gui-bootstrap")
        ))
    })?;
    Ok(state)
}

/// 原子写入 bootstrap 状态。
///
/// Business Logic（为什么需要这个函数）:
///     确认操作必须持久化版本与时间戳，且不得半写入导致启动 gate 误判。
///
/// Code Logic（这个函数做什么）:
///     确保父目录存在 → 写同目录临时文件 → flush → rename 到目标。
pub fn save_gui_bootstrap(state: &GuiBootstrapState) -> Result<(), AppError> {
    let path = gui_bootstrap_path()?;
    save_gui_bootstrap_to_path(&path, state)
}

/// 原子写入到指定路径（可测入口）。
///
/// Business Logic（为什么需要这个函数）:
///     测试与生产共用同一原子写语义。
///
/// Code Logic（这个函数做什么）:
///     create_dir_all(parent) → `.gui-bootstrap.<pid>.tmp` → write/flush → rename。
pub fn save_gui_bootstrap_to_path(path: &Path, state: &GuiBootstrapState) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| AppError::generic(format!("序列化 gui-bootstrap 失败: {e}")))?;
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(json.as_bytes())?;
        file.flush()?;
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        AppError::generic(format!(
            "写入 {} 失败: {e}",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("gui-bootstrap")
        ))
    })?;
    Ok(())
}

/// 写入“已确认当前披露版本”状态。
///
/// Business Logic（为什么需要这个函数）:
///     用户点击确认后需原子记录版本与时间戳，供后续启动跳过 gate。
///
/// Code Logic（这个函数做什么）:
///     构造 `LAN_DISCLOSURE_VERSION` + 当前 UTC RFC3339，调用 `save_gui_bootstrap`。
pub fn acknowledge_current_lan_disclosure() -> Result<GuiBootstrapState, AppError> {
    let state = GuiBootstrapState {
        lan_disclosure_version: LAN_DISCLOSURE_VERSION,
        acknowledged_at: Some(chrono::Utc::now().to_rfc3339()),
    };
    save_gui_bootstrap(&state)?;
    Ok(state)
}

/// 判断当前披露版本是否已确认。
///
/// Business Logic（为什么需要这个函数）:
///     setup 与 status 命令需要快速判断是否应跳过 ensure/start。
///
/// Code Logic（这个函数做什么）:
///     load bootstrap；`lan_disclosure_version >= LAN_DISCLOSURE_VERSION` 且有 `acknowledged_at` 视为已确认。
pub fn is_current_lan_disclosure_acknowledged() -> Result<bool, AppError> {
    let state = load_gui_bootstrap()?;
    Ok(is_acknowledged_for_version(&state, LAN_DISCLOSURE_VERSION))
}

/// 重置 LAN 披露确认为未确认态。
///
/// Business Logic（为什么需要这个函数）:
///     用户在设置中「重置首次启动引导」时，需清除**当前 flavor** 的 LAN 风险确认，
///     使下次启动重新进入披露 gate；不得清掉另一端（Dev↔Release）的确认。
///
/// Code Logic（这个函数做什么）:
///     原子写入当前 `gui_bootstrap_path()` 的 `GuiBootstrapState::default()`，不触碰 data.db。
pub fn reset_lan_disclosure() -> Result<GuiBootstrapState, AppError> {
    let state = GuiBootstrapState::default();
    save_gui_bootstrap(&state)?;
    Ok(state)
}

/// 纯函数：给定状态与目标版本是否已确认。
///
/// Business Logic（为什么需要这个函数）:
///     单测与 status 组装需在不碰磁盘的情况下判断确认语义。
///
/// Code Logic（这个函数做什么）:
///     version 达到目标且 `acknowledged_at` 非空。
pub fn is_acknowledged_for_version(state: &GuiBootstrapState, version: u32) -> bool {
    state.lan_disclosure_version >= version && state.acknowledged_at.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Business Logic: Dev/Release 文件名必须固定且互不相同。
    #[test]
    fn bootstrap_file_name_differs_by_flavor() {
        assert_eq!(
            gui_bootstrap_file_name(AppFlavor::Release),
            GUI_BOOTSTRAP_FILE_RELEASE
        );
        assert_eq!(gui_bootstrap_file_name(AppFlavor::Dev), GUI_BOOTSTRAP_FILE_DEV);
        assert_ne!(GUI_BOOTSTRAP_FILE_RELEASE, GUI_BOOTSTRAP_FILE_DEV);
    }

    #[test]
    fn missing_file_is_unacknowledged_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("gui-bootstrap.json");
        let state = load_gui_bootstrap_from_path(&path).unwrap();
        assert_eq!(state.lan_disclosure_version, 0);
        assert!(state.acknowledged_at.is_none());
        assert!(!is_acknowledged_for_version(&state, LAN_DISCLOSURE_VERSION));
    }

    #[test]
    fn save_and_load_roundtrip_records_version_and_timestamp() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("gui-bootstrap.json");
        let state = GuiBootstrapState {
            lan_disclosure_version: LAN_DISCLOSURE_VERSION,
            acknowledged_at: Some("2026-07-15T00:00:00Z".to_string()),
        };
        save_gui_bootstrap_to_path(&path, &state).unwrap();
        let loaded = load_gui_bootstrap_from_path(&path).unwrap();
        assert_eq!(loaded, state);
        assert!(is_acknowledged_for_version(&loaded, LAN_DISCLOSURE_VERSION));
    }

    #[test]
    fn version_bump_requires_new_acknowledgement() {
        let state = GuiBootstrapState {
            lan_disclosure_version: 0,
            acknowledged_at: Some("2026-07-15T00:00:00Z".to_string()),
        };
        assert!(!is_acknowledged_for_version(&state, 1));
        let acked = GuiBootstrapState {
            lan_disclosure_version: 1,
            acknowledged_at: Some("2026-07-15T00:00:00Z".to_string()),
        };
        assert!(is_acknowledged_for_version(&acked, 1));
        assert!(!is_acknowledged_for_version(&acked, 2));
    }

    #[test]
    fn bootstrap_json_only_contains_version_and_timestamp_keys() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("gui-bootstrap.json");
        let state = GuiBootstrapState {
            lan_disclosure_version: 1,
            acknowledged_at: Some("2026-07-15T00:00:00Z".to_string()),
        };
        save_gui_bootstrap_to_path(&path, &state).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let obj = value.as_object().unwrap();
        assert_eq!(obj.len(), 2);
        assert!(obj.contains_key("lanDisclosureVersion"));
        assert!(obj.contains_key("acknowledgedAt"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     重置后必须回到未确认态，否则「重置首次启动引导」无效。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入已确认 state → default 覆盖 → is_acknowledged 为 false。
    #[test]
    fn reset_state_is_unacknowledged_default() {
        let acked = GuiBootstrapState {
            lan_disclosure_version: LAN_DISCLOSURE_VERSION,
            acknowledged_at: Some("2026-07-15T00:00:00Z".to_string()),
        };
        assert!(is_acknowledged_for_version(&acked, LAN_DISCLOSURE_VERSION));
        let reset = GuiBootstrapState::default();
        assert_eq!(reset.lan_disclosure_version, 0);
        assert!(reset.acknowledged_at.is_none());
        assert!(!is_acknowledged_for_version(&reset, LAN_DISCLOSURE_VERSION));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     路径级重置必须持久化 default，供下次启动读取。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写已确认 → save default → load 断言未确认。
    #[test]
    fn save_default_clears_acknowledgement_on_disk() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("gui-bootstrap.json");
        save_gui_bootstrap_to_path(
            &path,
            &GuiBootstrapState {
                lan_disclosure_version: LAN_DISCLOSURE_VERSION,
                acknowledged_at: Some("2026-07-15T00:00:00Z".to_string()),
            },
        )
        .unwrap();
        save_gui_bootstrap_to_path(&path, &GuiBootstrapState::default()).unwrap();
        let loaded = load_gui_bootstrap_from_path(&path).unwrap();
        assert_eq!(loaded, GuiBootstrapState::default());
        assert!(!is_acknowledged_for_version(&loaded, LAN_DISCLOSURE_VERSION));
    }
}
