//! workspace_layout — Workbench 工作现场结构元数据（不含终端字节/正文/命令）。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户重启或切换项目后需要恢复上次选中的 project/worktree/session/view/inspector/browser target。
//!     只能保存结构引用，禁止保存 terminal 输出、Prompt、文件正文、env、token、命令或 provider 配置。
//!
//! Code Logic（这个模块做什么）:
//!     定义 schemaVersion=1 的 WorkspaceLayout / Draft / 封闭枚举，校验 slot_key、
//!     命名 snapshot 与 loopback browser URL，并提供序列化字段白名单证明。

use crate::error::AppError;
use crate::workbench::browser::normalize_browser_target_url;
use serde::{Deserialize, Serialize};

/// auto slot 固定键：桌面端零配置自动保存最后工作现场。
pub const DESKTOP_AUTO_SLOT_KEY: &str = "desktop:auto";

/// 主窗 Tauri label。
pub const MAIN_WINDOW_LABEL: &str = "main";

/// 卫星窗 label 前缀；完整 label 为 `workbench-1`..`workbench-4`。
pub const WORKBENCH_WINDOW_LABEL_PREFIX: &str = "workbench-";

/// 桌面卫星工作台窗口上限。
pub const MAX_WORKBENCH_SATELLITE_WINDOWS: u8 = 4;

/// 卫星窗 auto slot 前缀：`desktop:auto:window:workbench-N`。
pub const DESKTOP_WINDOW_AUTO_SLOT_PREFIX: &str = "desktop:auto:window:";

/// layout schema 版本；未知版本 fail-closed。
pub const WORKSPACE_LAYOUT_SCHEMA_VERSION: u32 = 1;

/// 布局种类：自动 slot 或命名 snapshot。
///
/// Business Logic（为什么需要这个枚举）:
///     自动布局与用户命名 snapshot 共享同一表，但删除/列表语义不同。
///
/// Code Logic（这个枚举做什么）:
///     camelCase 序列化；仅允许 `auto` / `named`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceLayoutKind {
    /// 桌面自动 slot（`desktop:auto` 或 `desktop:auto:window:workbench-[1-4]`）。
    Auto,
    /// 命名 snapshot（`named:<uuid>`）。
    Named,
}

impl WorkspaceLayoutKind {
    /// Business Logic（为什么需要这个函数）:
    ///     SQLite 以 TEXT 存储 kind，读写需要稳定 token。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 `auto` / `named`。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Named => "named",
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     从数据库读出的字符串必须 fail-closed 解析，禁止猜测未知值。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅接受 `auto`/`named`，否则 Validation。
    pub fn parse(raw: &str) -> Result<Self, AppError> {
        match raw {
            "auto" => Ok(Self::Auto),
            "named" => Ok(Self::Named),
            _ => Err(AppError::validation(format!(
                "workspace_layout_invalid_kind:{raw}"
            ))),
        }
    }
}

/// Workbench 主工作区视图。
///
/// Business Logic（为什么需要这个枚举）:
///     恢复时需要还原用户停留的工作区层（终端/文件/浏览器/自动化）。
///
/// Code Logic（这个枚举做什么）:
///     camelCase 序列化；未知值 fail-closed。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceView {
    /// 终端工作区。
    Terminal,
    /// 文件工作区。
    Files,
    /// 浏览器预览工作区。
    Browser,
    /// 自动化/Orchestrator 工作区。
    Automation,
}

impl WorkspaceView {
    /// Business Logic（为什么需要这个函数）:
    ///     持久化与前端 DTO 需要稳定 token。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 snake/lower token 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Files => "files",
            Self::Browser => "browser",
            Self::Automation => "automation",
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     读库或入参解析必须拒绝未知 view，避免恢复到错误 UI。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅接受已知 token。
    pub fn parse(raw: &str) -> Result<Self, AppError> {
        match raw {
            "terminal" => Ok(Self::Terminal),
            "files" => Ok(Self::Files),
            "browser" => Ok(Self::Browser),
            "automation" => Ok(Self::Automation),
            _ => Err(AppError::validation(format!(
                "workspace_layout_invalid_view:{raw}"
            ))),
        }
    }
}

/// 右侧 inspector 当前 tab。
///
/// Business Logic（为什么需要这个枚举）:
///     恢复 inspector 焦点（文件/Git/历史等）需要稳定枚举。
///
/// Code Logic（这个枚举做什么）:
///     camelCase 序列化；未知值 fail-closed。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InspectorTab {
    /// 文件 inspector。
    Files,
    /// Git 状态/变更。
    Git,
    /// Git 历史。
    History,
    /// 项目笔记。
    Notes,
    /// 自动化面板。
    Automation,
}

impl InspectorTab {
    /// Business Logic（为什么需要这个函数）:
    ///     持久化 token 必须与前端 inspector tab id 对齐。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Git => "git",
            Self::History => "history",
            Self::Notes => "notes",
            Self::Automation => "automation",
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     未知 inspector tab 不得猜测。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅接受已知 token。
    pub fn parse(raw: &str) -> Result<Self, AppError> {
        match raw {
            "files" => Ok(Self::Files),
            "git" => Ok(Self::Git),
            "history" => Ok(Self::History),
            "notes" => Ok(Self::Notes),
            "automation" => Ok(Self::Automation),
            _ => Err(AppError::validation(format!(
                "workspace_layout_invalid_inspector:{raw}"
            ))),
        }
    }
}

/// 写入 layout 的草稿（无 id/revision/时间戳；由 repo 填充）。
///
/// Business Logic（为什么需要这个结构体）:
///     前端 autosave 与命名 snapshot 只需提交稳定 selection，不携带 CAS 书页。
///
/// Code Logic（这个结构体做什么）:
///     仅含结构 metadata；无 command/content/prompt/env/provider 字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceLayoutDraft {
    /// slot 键：`desktop:auto`、`desktop:auto:window:workbench-[1-4]` 或 `named:<uuid>`。
    pub slot_key: String,
    /// auto / named。
    pub kind: WorkspaceLayoutKind,
    /// 命名 snapshot 展示名；auto 必须为 None。
    pub name: Option<String>,
    /// 当前 project id（本机 local 或控制设备上的 remote shortcut id）。
    pub project_id: String,
    /// 当前 active worktree id。
    pub active_worktree_id: Option<String>,
    /// 当前 active session id。
    pub active_session_id: Option<String>,
    /// 主工作区视图。
    pub workspace_view: WorkspaceView,
    /// inspector tab。
    pub inspector_tab: InspectorTab,
    /// 规范化后的 loopback browser target URL；不存 preview id。
    pub browser_target_url: Option<String>,
}

/// 持久化后的完整 layout 行。
///
/// Business Logic（为什么需要这个结构体）:
///     preflight/apply 需要 layout_id + revision 做 CAS 与对账。
///
/// Code Logic（这个结构体做什么）:
///     schema_version 固定为 1；revision 单调递增。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceLayout {
    /// schema 版本。
    pub schema_version: u32,
    /// 行 id（UUID）。
    pub id: String,
    /// slot 键。
    pub slot_key: String,
    /// auto / named。
    pub kind: WorkspaceLayoutKind,
    /// 命名 snapshot 名。
    pub name: Option<String>,
    /// project id。
    pub project_id: String,
    /// active worktree。
    pub active_worktree_id: Option<String>,
    /// active session。
    pub active_session_id: Option<String>,
    /// workspace view。
    pub workspace_view: WorkspaceView,
    /// inspector tab。
    pub inspector_tab: InspectorTab,
    /// browser target URL。
    pub browser_target_url: Option<String>,
    /// CAS revision。
    pub revision: u64,
    /// 创建时间 RFC3339。
    pub created_at: String,
    /// 更新时间 RFC3339。
    pub updated_at: String,
}

/// Business Logic（为什么需要这个函数）:
///     auto slot 键固定，防止前端/调用方拼写漂移。
///
/// Code Logic（这个函数做什么）:
///     返回 `desktop:auto`。
pub fn desktop_auto_slot_key() -> &'static str {
    DESKTOP_AUTO_SLOT_KEY
}

/// Business Logic（为什么需要这个函数）:
///     卫星窗序号必须是稳定的 1..=4，建窗与 layout slot 共用同一上限。
///
/// Code Logic（这个函数做什么）:
///     解析精确 `workbench-1`..`workbench-4`，否则返回 None。
pub fn parse_satellite_window_slot(label: &str) -> Option<u8> {
    let rest = label.strip_prefix(WORKBENCH_WINDOW_LABEL_PREFIX)?;
    let slot = rest.parse::<u8>().ok()?;
    if rest == slot.to_string() && (1..=MAX_WORKBENCH_SATELLITE_WINDOWS).contains(&slot) {
        Some(slot)
    } else {
        None
    }
}

/// Business Logic（为什么需要这个函数）:
///     主窗与卫星窗各自保存独立工作现场，禁止 overlay 或越界 label 写入 auto slot。
///
/// Code Logic（这个函数做什么）:
///     `main` → `desktop:auto`；`workbench-N`(1..=4) → `desktop:auto:window:workbench-N`；其余 Validation。
pub fn window_auto_slot_key(label: &str) -> Result<String, AppError> {
    if label == MAIN_WINDOW_LABEL {
        return Ok(DESKTOP_AUTO_SLOT_KEY.to_string());
    }
    if parse_satellite_window_slot(label).is_some() {
        return Ok(format!("{DESKTOP_WINDOW_AUTO_SLOT_PREFIX}{label}"));
    }
    Err(AppError::validation(format!(
        "workspace_layout_invalid_window_label:{label}"
    )))
}

/// Business Logic（为什么需要这个函数）:
///     persist / delete 必须识别窗口 auto slot，不能把 named 或越界窗当成 auto。
///
/// Code Logic（这个函数做什么）:
///     仅 `desktop:auto` 与 `desktop:auto:window:workbench-[1-4]` 为真。
pub fn is_window_auto_slot_key(slot_key: &str) -> bool {
    if slot_key == DESKTOP_AUTO_SLOT_KEY {
        return true;
    }
    let Some(label) = slot_key.strip_prefix(DESKTOP_WINDOW_AUTO_SLOT_PREFIX) else {
        return false;
    };
    parse_satellite_window_slot(label).is_some()
}

/// Business Logic（为什么需要这个函数）:
///     命名 snapshot 的 slot 必须可预测且唯一。
///
/// Code Logic（这个函数做什么）:
///     生成 `named:<uuid>`。
#[allow(dead_code)] // named snapshot 创建 API surface
pub fn new_named_slot_key() -> String {
    format!("named:{}", uuid::Uuid::new_v4())
}

/// Business Logic（为什么需要这个函数）:
///     校验 slot_key 与 kind 一致：auto 只能是主窗或卫星窗 auto slot；named 必须 named:<uuid>。
///
/// Code Logic（这个函数做什么）:
///     Auto 接受 `desktop:auto` 与 `desktop:auto:window:workbench-[1-4]`；named 解析 UUID 段。
pub fn validate_slot_key(slot_key: &str, kind: WorkspaceLayoutKind) -> Result<(), AppError> {
    match kind {
        WorkspaceLayoutKind::Auto => {
            if !is_window_auto_slot_key(slot_key) {
                return Err(AppError::validation(
                    "workspace_layout_invalid_auto_slot".to_string(),
                ));
            }
        }
        WorkspaceLayoutKind::Named => {
            let Some(rest) = slot_key.strip_prefix("named:") else {
                return Err(AppError::validation(
                    "workspace_layout_invalid_named_slot".to_string(),
                ));
            };
            if uuid::Uuid::parse_str(rest).is_err() {
                return Err(AppError::validation(
                    "workspace_layout_invalid_named_slot".to_string(),
                ));
            }
        }
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     命名 snapshot 必须有非空短名称；auto 禁止名称。
///
/// Code Logic（这个函数做什么）:
///     trim 后校验长度 1..=80。
pub fn validate_layout_name(
    kind: WorkspaceLayoutKind,
    name: &Option<String>,
) -> Result<Option<String>, AppError> {
    match kind {
        WorkspaceLayoutKind::Auto => {
            if name.as_ref().is_some_and(|n| !n.trim().is_empty()) {
                return Err(AppError::validation(
                    "workspace_layout_auto_must_not_have_name".to_string(),
                ));
            }
            Ok(None)
        }
        WorkspaceLayoutKind::Named => {
            let trimmed = name
                .as_ref()
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .ok_or_else(|| {
                    AppError::validation("workspace_layout_named_requires_name".to_string())
                })?;
            if trimmed.chars().count() > 80 {
                return Err(AppError::validation(
                    "workspace_layout_name_too_long".to_string(),
                ));
            }
            Ok(Some(trimmed))
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     browser target 必须经 loopback 归一化；禁止非本机 URL 与 preview id。
///
/// Code Logic（这个函数做什么）:
///     None 透传；Some 走 `normalize_browser_target_url`。
pub fn normalize_layout_browser_target(raw: Option<&str>) -> Result<Option<String>, AppError> {
    match raw {
        None => Ok(None),
        Some(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            Ok(Some(normalize_browser_target_url(trimmed)?))
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     写入前统一校验 draft，避免非法 slot/schema 进入 SQLite。
///
/// Code Logic（这个函数做什么）:
///     校验 project_id 非空、slot/name/browser，返回规范化后的 draft。
pub fn validate_and_normalize_draft(
    mut draft: WorkspaceLayoutDraft,
) -> Result<WorkspaceLayoutDraft, AppError> {
    if draft.project_id.trim().is_empty() {
        return Err(AppError::validation(
            "workspace_layout_project_required".to_string(),
        ));
    }
    draft.project_id = draft.project_id.trim().to_string();
    validate_slot_key(&draft.slot_key, draft.kind)?;
    draft.name = validate_layout_name(draft.kind, &draft.name)?;
    draft.browser_target_url =
        normalize_layout_browser_target(draft.browser_target_url.as_deref())?;
    if let Some(ref id) = draft.active_worktree_id {
        if id.trim().is_empty() {
            draft.active_worktree_id = None;
        }
    }
    if let Some(ref id) = draft.active_session_id {
        if id.trim().is_empty() {
            draft.active_session_id = None;
        }
    }
    Ok(draft)
}

/// Business Logic（为什么需要这个函数）:
///     读库时未知 schema 必须 fail-closed，禁止猜测字段。
///
/// Code Logic（这个函数做什么）:
///     仅接受 schema_version == 1。
pub fn ensure_known_schema_version(version: u32) -> Result<(), AppError> {
    if version != WORKSPACE_LAYOUT_SCHEMA_VERSION {
        return Err(AppError::validation(format!(
            "workspace_layout_unknown_schema:{version}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// Business Logic（为什么需要这个函数）:
    ///     构造合法 auto draft 供序列化与校验测试。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 project_id 可变的 auto draft。
    fn auto_draft(project_id: &str) -> WorkspaceLayoutDraft {
        WorkspaceLayoutDraft {
            slot_key: DESKTOP_AUTO_SLOT_KEY.to_string(),
            kind: WorkspaceLayoutKind::Auto,
            name: None,
            project_id: project_id.to_string(),
            active_worktree_id: Some("w1".to_string()),
            active_session_id: Some("s1".to_string()),
            workspace_view: WorkspaceView::Terminal,
            inspector_tab: InspectorTab::Git,
            browser_target_url: Some("http://localhost:5173/".to_string()),
        }
    }

    #[test]
    fn auto_slot_key_is_fixed() {
        assert_eq!(desktop_auto_slot_key(), "desktop:auto");
        validate_slot_key(DESKTOP_AUTO_SLOT_KEY, WorkspaceLayoutKind::Auto).unwrap();
        assert!(validate_slot_key("desktop:other", WorkspaceLayoutKind::Auto).is_err());
    }

    #[test]
    fn window_auto_slot_accepts_main_and_satellite_labels() {
        validate_slot_key(DESKTOP_AUTO_SLOT_KEY, WorkspaceLayoutKind::Auto).unwrap();
        validate_slot_key("desktop:auto:window:workbench-1", WorkspaceLayoutKind::Auto).unwrap();
        assert!(validate_slot_key("desktop:other", WorkspaceLayoutKind::Auto).is_err());
        assert!(
            validate_slot_key("desktop:auto:window:workbench-5", WorkspaceLayoutKind::Auto)
                .is_err()
        );
        assert_eq!(window_auto_slot_key("main").unwrap(), "desktop:auto");
        assert_eq!(
            window_auto_slot_key("workbench-2").unwrap(),
            "desktop:auto:window:workbench-2"
        );
        assert!(window_auto_slot_key("workbench-5").is_err());
        assert!(window_auto_slot_key("screenshot-overlay-0").is_err());
    }

    #[test]
    fn named_slot_requires_uuid() {
        let key = new_named_slot_key();
        assert!(key.starts_with("named:"));
        validate_slot_key(&key, WorkspaceLayoutKind::Named).unwrap();
        assert!(validate_slot_key("named:not-a-uuid", WorkspaceLayoutKind::Named).is_err());
        assert!(validate_slot_key(DESKTOP_AUTO_SLOT_KEY, WorkspaceLayoutKind::Named).is_err());
    }

    #[test]
    fn unknown_enums_fail_closed() {
        assert!(WorkspaceView::parse("unknown").is_err());
        assert_eq!(InspectorTab::parse("notes").unwrap(), InspectorTab::Notes);
        assert_eq!(InspectorTab::Notes.as_str(), "notes");
        assert!(InspectorTab::parse("mystery").is_err());
        assert!(WorkspaceLayoutKind::parse("legacy").is_err());
        assert!(ensure_known_schema_version(2).is_err());
        assert!(ensure_known_schema_version(1).is_ok());
    }

    #[test]
    fn invalid_browser_url_rejected() {
        assert!(normalize_layout_browser_target(Some("https://example.com")).is_err());
        let ok = normalize_layout_browser_target(Some("localhost:3000/app")).unwrap();
        assert_eq!(ok.as_deref(), Some("http://127.0.0.1:3000/app"));
    }

    #[test]
    fn named_requires_name_auto_forbids_name() {
        assert!(validate_layout_name(WorkspaceLayoutKind::Named, &None).is_err());
        assert!(validate_layout_name(WorkspaceLayoutKind::Auto, &Some("x".to_string())).is_err());
        assert_eq!(
            validate_layout_name(WorkspaceLayoutKind::Named, &Some("  snap  ".to_string()))
                .unwrap()
                .as_deref(),
            Some("snap")
        );
    }

    #[test]
    fn serialization_excludes_forbidden_fields() {
        let draft = validate_and_normalize_draft(auto_draft("p1")).unwrap();
        let layout = WorkspaceLayout {
            schema_version: WORKSPACE_LAYOUT_SCHEMA_VERSION,
            id: "id1".to_string(),
            slot_key: draft.slot_key.clone(),
            kind: draft.kind,
            name: draft.name.clone(),
            project_id: draft.project_id.clone(),
            active_worktree_id: draft.active_worktree_id.clone(),
            active_session_id: draft.active_session_id.clone(),
            workspace_view: draft.workspace_view,
            inspector_tab: draft.inspector_tab,
            browser_target_url: draft.browser_target_url.clone(),
            revision: 1,
            created_at: "t0".to_string(),
            updated_at: "t1".to_string(),
        };
        let value = serde_json::to_value(&layout).unwrap();
        let obj = value.as_object().unwrap();
        for forbidden in [
            "command",
            "content",
            "prompt",
            "env",
            "token",
            "provider",
            "previewId",
            "preview_id",
            "terminalBytes",
            "output",
        ] {
            assert!(
                !obj.contains_key(forbidden),
                "layout must not contain {forbidden}"
            );
        }
        // 整段 JSON 也不应出现这些敏感键名（防止嵌套泄漏）。
        let text = value.to_string();
        for needle in [
            "\"command\"",
            "\"prompt\"",
            "\"token\"",
            "\"env\"",
            "\"provider\"",
        ] {
            assert!(
                !text.contains(needle),
                "serialized layout must not contain {needle}"
            );
        }
        assert_eq!(obj.get("schemaVersion"), Some(&Value::from(1)));
        assert_eq!(
            obj.get("slotKey").and_then(|v| v.as_str()),
            Some("desktop:auto")
        );
    }

    #[test]
    fn empty_project_rejected() {
        let mut draft = auto_draft("  ");
        draft.project_id = "  ".to_string();
        assert!(validate_and_normalize_draft(draft).is_err());
    }
}
