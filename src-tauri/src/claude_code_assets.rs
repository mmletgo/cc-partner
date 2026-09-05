//! claude_code_assets.rs — Claude Code 本地资产管理与局域网选择性拉取（N/N+1 façade）
//!
//! Business Logic（为什么需要这个模块）:
//!     用户已经在 Claude Code 中沉淀了 skills、plugins、MCP 配置。cc-partner 需要提供一个
//!     图形化入口来查看、启用/禁用、安装/卸载这些个人级资产，并能从局域网其它设备中选择性拉取。
//!     Gate B 起扫描/归一化语义逐步迁入 Claude adapter；本模块保留旧 DTO 与 mutation 路径，
//!     并对 P2P MCP 导出停止脱敏；旧 peer placeholder 导入标记 legacyLossy 且不得覆盖真凭据。
//!     Gate D Task 7：Hub 启用时旧 mutation 不得二次直接写 target（须走 Agent Hub）；Hub 关闭时
//!     继续读最后成功 target 文件，忽略未知 Hub 表，永不清理 CAS。
//!
//! Code Logic（这个模块做什么）:
//!     - 扫描 `~/.claude/skills`、`~/.claude/commands`、`claude plugin list --json` 和 user-scope MCP；
//!     - 对 plugin/MCP 优先调用 Claude Code CLI，skills/commands 通过安全移动目录实现启停；
//!     - P2P 导出 zip bundle 时 MCP 保留原文（env/headers/url）；
//!     - 导入时检测 `__REDACTED_BY_CLAUDE_PARTNER__` → legacyLossy/blocked，不把 placeholder 写入为凭据；
//!     - 诊断日志仍 value-redacted；
//!     - `require_legacy_direct_mutation_allowed`：Hub on 时 fail-closed。

use crate::agent_hub::migration::{legacy_facade_policy, LegacyFacadePolicy};
use crate::config;
use crate::error::AppError;
use crate::state::AppState;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{Cursor, Seek, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};
use tokio::process::Command;
use walkdir::WalkDir;
use zip::write::FileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// 旧版本 P2P 导出写入的 MCP 脱敏占位符（仅 import 检测，新导出不再写入）。
pub const REDACTED_PLACEHOLDER: &str = "__REDACTED_BY_CLAUDE_PARTNER__";
const CLI_TIMEOUT_SECS: u64 = 45;

/// 读取当前 Agent Hub 是否启用（配置缺失/锁失败 → false，兼容降级）。
///
/// Business Logic: N/N+1 façade 需要知道是否应禁止直接 mutation。
/// Code Logic: 读 config_runtime 快照的 `agent_hub.enabled`。
pub fn is_agent_hub_enabled(state: &AppState) -> bool {
    state
        .config
        .read()
        .map(|c| c.agent_hub.enabled)
        .unwrap_or(false)
}

/// 当前旧入口 façade 策略（Hub 开/关）。
///
/// Business Logic: commands/routes 在 mutation 前查询；Hub on 时 translate、禁直接写。
/// Code Logic: `legacy_facade_policy(is_agent_hub_enabled)`。
pub fn current_legacy_facade_policy(state: &AppState) -> LegacyFacadePolicy {
    legacy_facade_policy(is_agent_hub_enabled(state))
}

/// Hub 启用时拒绝旧入口对 target 的二次直接 mutation。
///
/// Business Logic: N/N+1 旧 DTO 只能从 Hub 翻译；mutation 必须走 Agent Hub 路径。
/// Code Logic: `allow_direct_target_mutation=false` → Validation。
pub fn require_legacy_direct_mutation_allowed(state: &AppState) -> Result<(), AppError> {
    let policy = current_legacy_facade_policy(state);
    if policy.allow_direct_target_mutation {
        return Ok(());
    }
    Err(AppError::validation(
        "agent_hub_legacy_mutation_disabled_use_agent_hub".to_string(),
    ))
}

/// 兼容 façade 永远不得清理 CAS（降级/旧路由合同）。
///
/// Business Logic: 关闭 Hub 后新表与 object 不自动删除；旧版本不得当空数据清理。
/// Code Logic: 恒返回 false（供测试与 checklist 引用）。
pub fn legacy_facade_allows_cas_gc() -> bool {
    false
}

/// MCP 导入时对旧 peer placeholder 的判定结果。
///
/// Business Logic（为什么需要这个枚举）:
///     混合版本局域网中旧 peer 可能仍发送脱敏占位；不得把占位当真实凭据覆盖本机。
///
/// Code Logic（这个枚举做什么）:
///     `Ok` / `LegacyLossy`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpImportCredentialStatus {
    /// 无 placeholder，可按 overwrite 策略安装
    Ok,
    /// 含旧脱敏占位，禁止当作凭据写入
    LegacyLossy,
}

/// Claude Code 资产类别。序列化为前端使用的小写字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClaudeCodeAssetKind {
    Skill,
    Command,
    Plugin,
    Mcp,
}

impl ClaudeCodeAssetKind {
    /// 返回类别的稳定字符串，用于路径分组、前端 key 与日志。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skill => "skill",
            Self::Command => "command",
            Self::Plugin => "plugin",
            Self::Mcp => "mcp",
        }
    }
}

/// 前端展示用 Claude Code 资产 DTO。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCodeAsset {
    pub kind: ClaudeCodeAssetKind,
    pub id: String,
    pub name: String,
    pub scope: String,
    pub enabled: bool,
    pub source: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub path: Option<String>,
    pub size_bytes: Option<u64>,
    pub updated_at: Option<String>,
    pub can_enable: bool,
    pub can_uninstall: bool,
    pub can_export: bool,
    pub warnings: Vec<String>,
}

/// invoke 安装入口的来源参数。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCodeInstallSource {
    pub kind: ClaudeCodeAssetKind,
    pub path: Option<String>,
    pub name: Option<String>,
    pub config: Option<Value>,
    #[serde(default)]
    pub overwrite: bool,
}

/// P2P 选择器：只导出/拉取用户勾选的资产。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCodeAssetSelector {
    pub kind: ClaudeCodeAssetKind,
    pub id: String,
}

/// 安装/拉取结果，供前端展示成功、跳过与失败数量。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCodeAssetInstallReport {
    pub ok: bool,
    pub installed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub note: String,
    pub items: Vec<ClaudeCodeAssetInstallItem>,
}

/// 单项安装结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCodeAssetInstallItem {
    pub kind: ClaudeCodeAssetKind,
    pub id: String,
    pub name: String,
    pub status: String,
    pub message: String,
}

/// bundle manifest，随 zip 一起传输。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetBundleManifest {
    pub assets: Vec<AssetBundleManifestItem>,
}

/// bundle 中的单项资产元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetBundleManifestItem {
    pub kind: ClaudeCodeAssetKind,
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub hash: String,
    pub asset_path: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginListItem {
    id: String,
    version: Option<String>,
    scope: Option<String>,
    enabled: bool,
    install_path: Option<String>,
    last_updated: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PluginManifest {
    name: Option<String>,
    description: Option<String>,
    version: Option<String>,
}

#[derive(Debug)]
struct LocalMcpServer {
    name: String,
    config: Value,
    enabled: bool,
    path: Option<PathBuf>,
}

/// 列出本机所有个人级 Claude Code assets。
pub async fn list_assets() -> Result<Vec<ClaudeCodeAsset>, AppError> {
    let mut assets = Vec::new();
    let mut plugin_install_paths = HashSet::new();

    match list_plugins_from_cli().await {
        Ok(items) => {
            for item in items {
                if let Some(path) = &item.install_path {
                    plugin_install_paths.insert(normalize_path_key(Path::new(path)));
                }
                assets.push(plugin_item_to_asset(item));
            }
        }
        Err(e) => tracing::warn!("Claude Code plugin list 失败，跳过 plugin CLI 列表: {e}"),
    }

    scan_skills_and_commands(&mut assets, &plugin_install_paths)?;
    scan_skills_dir_plugins(&mut assets, &plugin_install_paths)?;
    scan_mcp_servers(&mut assets)?;

    assets.sort_by(|a, b| {
        a.kind
            .as_str()
            .cmp(b.kind.as_str())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(assets)
}

/// 设置某个资产的启用状态（无 Hub 门闩；单测 / 内部 helper 用）。
///
/// Business Logic: Hub 启用时旧入口不得二次直接 mutation。
/// Code Logic: 委托 `set_asset_enabled_with_hub_guard(None, ...)`。
#[allow(dead_code)]
pub async fn set_asset_enabled(
    kind: ClaudeCodeAssetKind,
    id: String,
    enabled: bool,
) -> Result<ClaudeCodeAssetInstallReport, AppError> {
    set_asset_enabled_with_hub_guard(None, kind, id, enabled).await
}

/// 带 Hub 门闩的 set_asset_enabled（commands 层传入 AppState）。
pub async fn set_asset_enabled_with_hub_guard(
    hub_guard: Option<&AppState>,
    kind: ClaudeCodeAssetKind,
    id: String,
    enabled: bool,
) -> Result<ClaudeCodeAssetInstallReport, AppError> {
    if let Some(state) = hub_guard {
        require_legacy_direct_mutation_allowed(state)?;
    }
    match kind {
        ClaudeCodeAssetKind::Plugin => {
            let action = if enabled { "enable" } else { "disable" };
            run_claude(&["plugin", action, &id, "--scope", "user"]).await?;
            Ok(single_report(
                kind,
                &id,
                &id,
                "installed",
                "plugin 状态已更新",
            ))
        }
        ClaudeCodeAssetKind::Skill => {
            set_tree_enabled(
                kind,
                &id,
                enabled,
                &claude_skills_dir(),
                &disabled_skills_dir(),
            )?;
            Ok(single_report(
                kind,
                &id,
                &id,
                "installed",
                "skill 状态已更新",
            ))
        }
        ClaudeCodeAssetKind::Command => {
            set_command_enabled(&id, enabled)?;
            Ok(single_report(
                kind,
                &id,
                &id,
                "installed",
                "command 状态已更新",
            ))
        }
        ClaudeCodeAssetKind::Mcp => {
            set_mcp_enabled(&id, enabled).await?;
            Ok(single_report(kind, &id, &id, "installed", "MCP 状态已更新"))
        }
    }
}

/// 从本机路径或 JSON 配置安装一个资产（无 Hub 门闩；单测 / 内部 helper 用）。
#[allow(dead_code)]
pub async fn install_asset(
    source: ClaudeCodeInstallSource,
) -> Result<ClaudeCodeAssetInstallReport, AppError> {
    install_asset_with_hub_guard(None, source).await
}

/// 带 Hub 门闩的 install_asset。
pub async fn install_asset_with_hub_guard(
    hub_guard: Option<&AppState>,
    source: ClaudeCodeInstallSource,
) -> Result<ClaudeCodeAssetInstallReport, AppError> {
    if let Some(state) = hub_guard {
        require_legacy_direct_mutation_allowed(state)?;
    }
    let item = match source.kind {
        ClaudeCodeAssetKind::Skill => install_skill_from_source(&source)?,
        ClaudeCodeAssetKind::Command => install_command_from_source(&source)?,
        ClaudeCodeAssetKind::Plugin => install_plugin_from_source(&source)?,
        ClaudeCodeAssetKind::Mcp => install_mcp_from_source(&source).await?,
    };
    Ok(report_from_items(vec![item]))
}

/// 卸载一个本机资产。卸载前会备份到 cc-partner 自己的备份目录（无 Hub 门闩）。
#[allow(dead_code)]
pub async fn uninstall_asset(
    kind: ClaudeCodeAssetKind,
    id: String,
    keep_data: bool,
) -> Result<ClaudeCodeAssetInstallReport, AppError> {
    uninstall_asset_with_hub_guard(None, kind, id, keep_data).await
}

/// 带 Hub 门闩的 uninstall_asset。
pub async fn uninstall_asset_with_hub_guard(
    hub_guard: Option<&AppState>,
    kind: ClaudeCodeAssetKind,
    id: String,
    keep_data: bool,
) -> Result<ClaudeCodeAssetInstallReport, AppError> {
    if let Some(state) = hub_guard {
        require_legacy_direct_mutation_allowed(state)?;
    }
    match kind {
        ClaudeCodeAssetKind::Plugin => {
            let mut args = vec!["plugin", "uninstall", &id, "--scope", "user"];
            if keep_data {
                args.push("--keep-data");
            }
            run_claude(&args).await?;
            Ok(single_report(kind, &id, &id, "installed", "plugin 已卸载"))
        }
        ClaudeCodeAssetKind::Skill => {
            remove_tree_with_backup(
                kind,
                &id,
                &[
                    claude_skills_dir().join(&id),
                    disabled_skills_dir().join(&id),
                ],
            )?;
            Ok(single_report(kind, &id, &id, "installed", "skill 已卸载"))
        }
        ClaudeCodeAssetKind::Command => {
            let file = format!("{id}.md");
            remove_tree_with_backup(
                kind,
                &id,
                &[
                    claude_commands_dir().join(&file),
                    disabled_commands_dir().join(&file),
                ],
            )?;
            Ok(single_report(kind, &id, &id, "installed", "command 已卸载"))
        }
        ClaudeCodeAssetKind::Mcp => {
            backup_mcp_config(&id)?;
            let _ = run_claude(&["mcp", "remove", &id, "--scope", "user"]).await;
            let disabled = disabled_mcp_dir().join(format!("{id}.json"));
            if disabled.exists() {
                fs::remove_file(disabled)?;
            }
            Ok(single_report(kind, &id, &id, "installed", "MCP 已卸载"))
        }
    }
}

/// 列出局域网某设备暴露的 Claude Code assets。
pub async fn list_remote_assets(
    state: &AppState,
    device_id: String,
) -> Result<Vec<ClaudeCodeAsset>, AppError> {
    let device = {
        let devices = state.devices.read().expect("devices 读锁中毒");
        devices
            .get(&device_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("设备不存在或已离线"))?
    };
    let base_url = device.base_url();
    state.peer_client.claude_assets_inventory(&base_url).await
}

/// 从局域网某设备拉取用户选择的 assets，并按 overwrite 决策安装。
pub async fn pull_remote_assets(
    state: &AppState,
    device_id: String,
    items: Vec<ClaudeCodeAssetSelector>,
    overwrite: bool,
) -> Result<ClaudeCodeAssetInstallReport, AppError> {
    let device = {
        let devices = state.devices.read().expect("devices 读锁中毒");
        devices
            .get(&device_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("设备不存在或已离线"))?
    };
    let base_url = device.base_url();
    let bundle = state
        .peer_client
        .claude_assets_bundle(&base_url, &items)
        .await?;
    install_bundle(&bundle, overwrite).await
}

/// 为 P2P 路由构建仅包含所选资产的 zip bundle。
pub async fn build_bundle(items: Vec<ClaudeCodeAssetSelector>) -> Result<Vec<u8>, AppError> {
    let assets = list_assets().await?;
    let selected: HashSet<(ClaudeCodeAssetKind, String)> =
        items.into_iter().map(|i| (i.kind, i.id)).collect();
    let mcp_servers = read_all_mcp_servers()?;
    let mut manifest = AssetBundleManifest { assets: Vec::new() };
    let cursor = Cursor::new(Vec::<u8>::new());
    let mut zip = ZipWriter::new(cursor);

    for asset in assets {
        if !selected.contains(&(asset.kind, asset.id.clone())) || !asset.can_export {
            continue;
        }
        let segment = safe_name_segment(&format!("{}-{}", asset.kind.as_str(), asset.name))?;
        let prefix = format!("assets/{segment}");
        let mut warnings = asset.warnings.clone();

        match asset.kind {
            ClaudeCodeAssetKind::Skill | ClaudeCodeAssetKind::Plugin => {
                let path = match &asset.path {
                    Some(p) => PathBuf::from(p),
                    None => continue,
                };
                let hash = hash_path(&path)?;
                add_directory_to_zip(&mut zip, &path, &prefix)?;
                manifest.assets.push(AssetBundleManifestItem {
                    kind: asset.kind,
                    id: asset.id,
                    name: asset.name,
                    version: asset.version,
                    hash,
                    asset_path: prefix,
                    warnings,
                });
            }
            ClaudeCodeAssetKind::Command => {
                let path = match &asset.path {
                    Some(p) => PathBuf::from(p),
                    None => continue,
                };
                let filename = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("command.md");
                let asset_path = format!("{prefix}/{filename}");
                let hash = hash_path(&path)?;
                add_file_to_zip(&mut zip, &path, &asset_path)?;
                manifest.assets.push(AssetBundleManifestItem {
                    kind: asset.kind,
                    id: asset.id,
                    name: asset.name,
                    version: asset.version,
                    hash,
                    asset_path,
                    warnings,
                });
            }
            ClaudeCodeAssetKind::Mcp => {
                let Some(server) = mcp_servers.iter().find(|s| s.name == asset.id) else {
                    continue;
                };
                // Gate B：新构建导出保留 MCP 原文（env/headers/url），不再写入脱敏占位。
                // 诊断日志仍不得打印 secret；inventory 可用元数据，bundle 必须完整。
                if contains_sensitive_config(&server.config) {
                    warnings
                        .push("包含凭据字段；本版本局域网 bundle 按原文传输（非脱敏）".to_string());
                }
                let bytes = serde_json::to_vec_pretty(&server.config)?;
                let hash = sha256_hex(&bytes);
                let asset_path = format!("{prefix}/mcp.json");
                add_bytes_to_zip(&mut zip, &asset_path, &bytes)?;
                manifest.assets.push(AssetBundleManifestItem {
                    kind: asset.kind,
                    id: asset.id,
                    name: asset.name,
                    version: asset.version,
                    hash,
                    asset_path,
                    warnings,
                });
            }
        }
    }

    add_bytes_to_zip(
        &mut zip,
        "manifest.json",
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    let cursor = zip
        .finish()
        .map_err(|e| AppError::generic(format!("生成 assets bundle 失败: {e}")))?;
    Ok(cursor.into_inner())
}

/// 安装从局域网下载的 bundle。
pub async fn install_bundle(
    bytes: &[u8],
    overwrite: bool,
) -> Result<ClaudeCodeAssetInstallReport, AppError> {
    let staging = incoming_dir().join(format!("bundle-{}", Utc::now().timestamp_millis()));
    fs::create_dir_all(&staging)?;
    extract_zip_safe(bytes, &staging)?;
    let manifest_path = staging.join("manifest.json");
    let manifest_text = fs::read_to_string(&manifest_path)?;
    let manifest: AssetBundleManifest = serde_json::from_str(&manifest_text)?;
    let mut results = Vec::new();

    for item in manifest.assets {
        let result = install_manifest_item(&staging, &item, overwrite).await;
        match result {
            Ok(i) => results.push(i),
            Err(e) => results.push(ClaudeCodeAssetInstallItem {
                kind: item.kind,
                id: item.id.clone(),
                name: item.name.clone(),
                status: "failed".to_string(),
                message: e.to_string(),
            }),
        }
    }

    if let Err(e) = fs::remove_dir_all(&staging) {
        tracing::warn!("清理 assets staging 失败 {:?}: {e}", staging);
    }
    Ok(report_from_items(results))
}

async fn install_manifest_item(
    staging: &Path,
    item: &AssetBundleManifestItem,
    overwrite: bool,
) -> Result<ClaudeCodeAssetInstallItem, AppError> {
    let src = staging.join(&item.asset_path);
    match item.kind {
        ClaudeCodeAssetKind::Skill => install_tree(
            item.kind,
            &item.name,
            &src,
            &claude_skills_dir().join(&item.name),
            overwrite,
        ),
        ClaudeCodeAssetKind::Command => install_command_file(&item.name, &src, overwrite),
        ClaudeCodeAssetKind::Plugin => install_tree(
            item.kind,
            &item.name,
            &src,
            &claude_skills_dir().join(&item.name),
            overwrite,
        ),
        ClaudeCodeAssetKind::Mcp => {
            let config: Value = serde_json::from_str(&fs::read_to_string(&src)?)?;
            match classify_mcp_import_credentials(&config) {
                McpImportCredentialStatus::LegacyLossy => {
                    // 旧 peer placeholder：不得覆盖本机已有真实凭据，也不把 placeholder 当凭据写入。
                    if read_mcp_config(&item.name)?.is_some() {
                        return Ok(ClaudeCodeAssetInstallItem {
                            kind: item.kind,
                            id: item.id.clone(),
                            name: item.name.clone(),
                            status: "legacyLossy".to_string(),
                            message:
                                "旧端脱敏占位 MCP 不能覆盖本机已有凭据；请从具备原值的源重新推送"
                                    .to_string(),
                        });
                    }
                    Ok(ClaudeCodeAssetInstallItem {
                        kind: item.kind,
                        id: item.id.clone(),
                        name: item.name.clone(),
                        status: "legacyLossy".to_string(),
                        message: "旧端脱敏占位 MCP 已跳过安装（legacyLossy/blocked）".to_string(),
                    })
                }
                McpImportCredentialStatus::Ok => {
                    install_mcp_config(&item.name, config, overwrite).await
                }
            }
        }
    }
}

fn plugin_item_to_asset(item: PluginListItem) -> ClaudeCodeAsset {
    let path = item.install_path.clone().map(PathBuf::from);
    let manifest = path.as_deref().and_then(|p| read_plugin_manifest(p).ok());
    let warnings = manifest
        .as_ref()
        .map(|_| Vec::new())
        .unwrap_or_else(|| vec!["无法读取 plugin manifest，仅展示 CLI 元数据".to_string()]);
    ClaudeCodeAsset {
        kind: ClaudeCodeAssetKind::Plugin,
        id: item.id.clone(),
        name: item.id.split('@').next().unwrap_or(&item.id).to_string(),
        scope: item.scope.unwrap_or_else(|| "user".to_string()),
        enabled: item.enabled,
        source: "claude-plugin-cli".to_string(),
        version: item
            .version
            .or_else(|| manifest.as_ref().and_then(|m| m.version.clone())),
        description: manifest.and_then(|m| m.description),
        path: item.install_path,
        size_bytes: path.as_deref().and_then(|p| dir_size(p).ok()),
        updated_at: item.last_updated,
        can_enable: true,
        can_uninstall: true,
        can_export: path.as_deref().map(|p| p.exists()).unwrap_or(false),
        warnings,
    }
}

async fn list_plugins_from_cli() -> Result<Vec<PluginListItem>, AppError> {
    let out = run_claude(&["plugin", "list", "--json"]).await?;
    let list: Vec<PluginListItem> = serde_json::from_str(&out)?;
    Ok(list)
}

async fn run_claude(args: &[&str]) -> Result<String, AppError> {
    let mut command = Command::new("claude");
    command.args(args);
    let output = tokio::time::timeout(Duration::from_secs(CLI_TIMEOUT_SECS), command.output())
        .await
        .map_err(|_| AppError::generic("Claude Code CLI 执行超时"))?
        .map_err(|e| AppError::generic(format!("无法执行 claude CLI: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let msg = if stderr.is_empty() { stdout } else { stderr };
        let safe_args = args.iter().take(2).copied().collect::<Vec<_>>().join(" ");
        return Err(AppError::generic(format!("claude {safe_args} 失败: {msg}")));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn scan_skills_and_commands(
    assets: &mut Vec<ClaudeCodeAsset>,
    plugin_paths: &HashSet<String>,
) -> Result<(), AppError> {
    scan_skill_dir(assets, &claude_skills_dir(), true, plugin_paths)?;
    scan_skill_dir(assets, &disabled_skills_dir(), false, plugin_paths)?;
    scan_command_dir(assets, &claude_commands_dir(), true)?;
    scan_command_dir(assets, &disabled_commands_dir(), false)?;
    Ok(())
}

fn scan_skill_dir(
    assets: &mut Vec<ClaudeCodeAsset>,
    root: &Path,
    enabled: bool,
    plugin_paths: &HashSet<String>,
) -> Result<(), AppError> {
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.join(".claude-plugin").join("plugin.json").exists()
            || plugin_paths.contains(&normalize_path_key(&path))
        {
            continue;
        }
        let skill_file = path.join("SKILL.md");
        if !skill_file.exists() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("skill")
            .to_string();
        let (front_name, description) = read_skill_summary(&skill_file).unwrap_or((None, None));
        assets.push(ClaudeCodeAsset {
            kind: ClaudeCodeAssetKind::Skill,
            id: name.clone(),
            name: front_name.unwrap_or(name),
            scope: "user".to_string(),
            enabled,
            source: if enabled {
                "personal-skill"
            } else {
                "disabled-skill"
            }
            .to_string(),
            version: None,
            description,
            path: Some(path.to_string_lossy().to_string()),
            size_bytes: dir_size(&path).ok(),
            updated_at: metadata_updated_at(&path),
            can_enable: true,
            can_uninstall: true,
            can_export: true,
            warnings: Vec::new(),
        });
    }
    Ok(())
}

fn scan_command_dir(
    assets: &mut Vec<ClaudeCodeAsset>,
    root: &Path,
    enabled: bool,
) -> Result<(), AppError> {
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("command")
            .to_string();
        let description = fs::read_to_string(&path)
            .ok()
            .and_then(|text| first_non_empty_markdown_line(&text));
        assets.push(ClaudeCodeAsset {
            kind: ClaudeCodeAssetKind::Command,
            id: name.clone(),
            name,
            scope: "user".to_string(),
            enabled,
            source: if enabled {
                "personal-command"
            } else {
                "disabled-command"
            }
            .to_string(),
            version: None,
            description,
            path: Some(path.to_string_lossy().to_string()),
            size_bytes: fs::metadata(&path).ok().map(|m| m.len()),
            updated_at: metadata_updated_at(&path),
            can_enable: true,
            can_uninstall: true,
            can_export: true,
            warnings: Vec::new(),
        });
    }
    Ok(())
}

fn scan_skills_dir_plugins(
    assets: &mut Vec<ClaudeCodeAsset>,
    plugin_paths: &HashSet<String>,
) -> Result<(), AppError> {
    let root = claude_skills_dir();
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.join(".claude-plugin").join("plugin.json").exists()
            || plugin_paths.contains(&normalize_path_key(&path))
        {
            continue;
        }
        let manifest = read_plugin_manifest(&path).ok();
        let dir_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("plugin")
            .to_string();
        let name = manifest
            .as_ref()
            .and_then(|m| m.name.clone())
            .unwrap_or(dir_name.clone());
        assets.push(ClaudeCodeAsset {
            kind: ClaudeCodeAssetKind::Plugin,
            id: format!("{name}@skills-dir"),
            name,
            scope: "user".to_string(),
            enabled: true,
            source: "skills-dir".to_string(),
            version: manifest.as_ref().and_then(|m| m.version.clone()),
            description: manifest.and_then(|m| m.description),
            path: Some(path.to_string_lossy().to_string()),
            size_bytes: dir_size(&path).ok(),
            updated_at: metadata_updated_at(&path),
            can_enable: true,
            can_uninstall: true,
            can_export: true,
            warnings: vec![
                "此 plugin 来自 ~/.claude/skills，未出现在 plugin CLI 列表中".to_string(),
            ],
        });
    }
    Ok(())
}

fn scan_mcp_servers(assets: &mut Vec<ClaudeCodeAsset>) -> Result<(), AppError> {
    for server in read_all_mcp_servers()? {
        let mut warnings = Vec::new();
        if contains_sensitive_config(&server.config) {
            warnings.push("包含凭据字段；局域网导出按原文传输".to_string());
        }
        if contains_redacted_placeholder(&server.config) {
            warnings.push("包含旧版脱敏占位（legacyLossy），请从具备原值的源重新同步".to_string());
        }
        assets.push(ClaudeCodeAsset {
            kind: ClaudeCodeAssetKind::Mcp,
            id: server.name.clone(),
            name: server.name,
            scope: "user".to_string(),
            enabled: server.enabled,
            source: if server.enabled {
                "user-mcp"
            } else {
                "disabled-mcp"
            }
            .to_string(),
            version: None,
            description: describe_mcp(&server.config),
            path: server.path.map(|p| p.to_string_lossy().to_string()),
            size_bytes: None,
            updated_at: None,
            can_enable: true,
            can_uninstall: true,
            can_export: true,
            warnings,
        });
    }
    Ok(())
}

fn read_all_mcp_servers() -> Result<Vec<LocalMcpServer>, AppError> {
    let mut servers = Vec::new();
    let config = read_claude_json()?;
    if let Some(map) = config.get("mcpServers").and_then(|v| v.as_object()) {
        for (name, value) in map {
            servers.push(LocalMcpServer {
                name: name.clone(),
                config: value.clone(),
                enabled: true,
                path: None,
            });
        }
    }
    let disabled_dir = disabled_mcp_dir();
    if disabled_dir.exists() {
        for entry in fs::read_dir(disabled_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("mcp")
                .to_string();
            let config: Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
            servers.push(LocalMcpServer {
                name,
                config,
                enabled: false,
                path: Some(path),
            });
        }
    }
    Ok(servers)
}

fn read_claude_json() -> Result<Value, AppError> {
    let path = claude_json_path();
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

fn read_mcp_config(name: &str) -> Result<Option<Value>, AppError> {
    let config = read_claude_json()?;
    Ok(config
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .and_then(|m| m.get(name))
        .cloned())
}

async fn set_mcp_enabled(name: &str, enabled: bool) -> Result<(), AppError> {
    let disabled_path = disabled_mcp_dir().join(format!("{name}.json"));
    if enabled {
        let text = fs::read_to_string(&disabled_path)?;
        let config: Value = serde_json::from_str(&text)?;
        install_mcp_config(name, config, true).await?;
        fs::remove_file(disabled_path)?;
    } else {
        let config = read_mcp_config(name)?.ok_or_else(|| AppError::not_found("MCP 配置不存在"))?;
        fs::create_dir_all(disabled_mcp_dir())?;
        if disabled_path.exists() {
            backup_path(ClaudeCodeAssetKind::Mcp, name, &disabled_path)?;
        }
        fs::write(&disabled_path, serde_json::to_vec_pretty(&config)?)?;
        run_claude(&["mcp", "remove", name, "--scope", "user"]).await?;
    }
    Ok(())
}

fn set_tree_enabled(
    kind: ClaudeCodeAssetKind,
    id: &str,
    enabled: bool,
    active_root: &Path,
    disabled_root: &Path,
) -> Result<(), AppError> {
    let active = active_root.join(id);
    let disabled = disabled_root.join(id);
    if enabled {
        if !disabled.exists() {
            return Err(AppError::not_found("禁用区中没有该资产"));
        }
        if active.exists() {
            backup_path(kind, id, &active)?;
            remove_path(&active)?;
        }
        move_path(&disabled, &active)?;
    } else {
        if !active.exists() {
            return Err(AppError::not_found("启用区中没有该资产"));
        }
        fs::create_dir_all(disabled_root)?;
        if disabled.exists() {
            backup_path(kind, id, &disabled)?;
            remove_path(&disabled)?;
        }
        move_path(&active, &disabled)?;
    }
    Ok(())
}

fn set_command_enabled(id: &str, enabled: bool) -> Result<(), AppError> {
    let filename = format!("{id}.md");
    let active = claude_commands_dir().join(&filename);
    let disabled = disabled_commands_dir().join(&filename);
    if enabled {
        if active.exists() {
            return Ok(());
        }
        fs::create_dir_all(claude_commands_dir())?;
        if active.exists() {
            backup_path(ClaudeCodeAssetKind::Command, id, &active)?;
            remove_path(&active)?;
        }
        move_path(&disabled, &active)?;
    } else {
        fs::create_dir_all(disabled_commands_dir())?;
        if disabled.exists() {
            backup_path(ClaudeCodeAssetKind::Command, id, &disabled)?;
            remove_path(&disabled)?;
        }
        move_path(&active, &disabled)?;
    }
    Ok(())
}

fn install_skill_from_source(
    source: &ClaudeCodeInstallSource,
) -> Result<ClaudeCodeAssetInstallItem, AppError> {
    let path = source_path(source)?;
    let name = source
        .name
        .clone()
        .or_else(|| {
            path.file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
        .ok_or_else(|| AppError::generic("缺少 skill 名称"))?;
    install_tree(
        ClaudeCodeAssetKind::Skill,
        &name,
        &path,
        &claude_skills_dir().join(&name),
        source.overwrite,
    )
}

fn install_command_from_source(
    source: &ClaudeCodeInstallSource,
) -> Result<ClaudeCodeAssetInstallItem, AppError> {
    let path = source_path(source)?;
    let name = source
        .name
        .clone()
        .or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
        .ok_or_else(|| AppError::generic("缺少 command 名称"))?;
    install_command_file(&name, &path, source.overwrite)
}

fn install_plugin_from_source(
    source: &ClaudeCodeInstallSource,
) -> Result<ClaudeCodeAssetInstallItem, AppError> {
    let path = source_path(source)?;
    let source_dir = if path.extension().and_then(|s| s.to_str()) == Some("zip") {
        let staging = incoming_dir().join(format!("plugin-{}", Utc::now().timestamp_millis()));
        fs::create_dir_all(&staging)?;
        let bytes = fs::read(&path)?;
        extract_zip_safe(&bytes, &staging)?;
        detect_plugin_root(&staging)
    } else {
        path
    };
    let manifest = read_plugin_manifest(&source_dir).ok();
    let name = source
        .name
        .clone()
        .or_else(|| manifest.as_ref().and_then(|m| m.name.clone()))
        .or_else(|| {
            source_dir
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
        .ok_or_else(|| AppError::generic("缺少 plugin 名称"))?;
    install_tree(
        ClaudeCodeAssetKind::Plugin,
        &name,
        &source_dir,
        &claude_skills_dir().join(&name),
        source.overwrite,
    )
}

async fn install_mcp_from_source(
    source: &ClaudeCodeInstallSource,
) -> Result<ClaudeCodeAssetInstallItem, AppError> {
    let (name, config) = match (&source.name, &source.config, &source.path) {
        (Some(name), Some(config), _) => (name.clone(), config.clone()),
        (_, Some(config), _) => mcp_name_and_config(source.name.as_deref(), config.clone())?,
        (name, None, Some(path)) => {
            let value: Value = serde_json::from_str(&fs::read_to_string(path)?)?;
            mcp_name_and_config(name.as_deref(), value)?
        }
        _ => return Err(AppError::generic("安装 MCP 需要 name + JSON 配置")),
    };
    install_mcp_config(&name, config, source.overwrite).await
}

fn mcp_name_and_config(name: Option<&str>, value: Value) -> Result<(String, Value), AppError> {
    if let Some(name) = name {
        if let Some(config) = value
            .get("mcpServers")
            .and_then(|v| v.as_object())
            .and_then(|m| m.get(name))
        {
            return Ok((name.to_string(), config.clone()));
        }
        return Ok((name.to_string(), value));
    }
    if let Some(map) = value.get("mcpServers").and_then(|v| v.as_object()) {
        if map.len() == 1 {
            let (name, config) = map.iter().next().expect("len checked");
            return Ok((name.clone(), config.clone()));
        }
    }
    Err(AppError::generic("无法从 MCP JSON 中推断唯一名称"))
}

async fn install_mcp_config(
    name: &str,
    config: Value,
    overwrite: bool,
) -> Result<ClaudeCodeAssetInstallItem, AppError> {
    if read_mcp_config(name)?.is_some() {
        if !overwrite {
            return Ok(ClaudeCodeAssetInstallItem {
                kind: ClaudeCodeAssetKind::Mcp,
                id: name.to_string(),
                name: name.to_string(),
                status: "skipped".to_string(),
                message: "已存在，已跳过".to_string(),
            });
        }
        backup_mcp_config(name)?;
        let _ = run_claude(&["mcp", "remove", name, "--scope", "user"]).await;
    }
    let json = serde_json::to_string(&config)?;
    run_claude(&["mcp", "add-json", name, &json, "--scope", "user"]).await?;
    Ok(ClaudeCodeAssetInstallItem {
        kind: ClaudeCodeAssetKind::Mcp,
        id: name.to_string(),
        name: name.to_string(),
        status: "installed".to_string(),
        message: if contains_redacted_placeholder(&config) {
            "已导入脱敏配置，请补充凭据".to_string()
        } else {
            "MCP 已安装".to_string()
        },
    })
}

fn install_tree(
    kind: ClaudeCodeAssetKind,
    name: &str,
    src: &Path,
    dst: &Path,
    overwrite: bool,
) -> Result<ClaudeCodeAssetInstallItem, AppError> {
    validate_asset_name(name)?;
    if dst.exists() {
        if !overwrite {
            return Ok(ClaudeCodeAssetInstallItem {
                kind,
                id: name.to_string(),
                name: name.to_string(),
                status: "skipped".to_string(),
                message: "已存在，已跳过".to_string(),
            });
        }
        backup_path(kind, name, dst)?;
        remove_path(dst)?;
    }
    let parent = dst
        .parent()
        .ok_or_else(|| AppError::generic("目标路径无父目录"))?;
    fs::create_dir_all(parent)?;
    copy_dir_all(src, dst)?;
    Ok(ClaudeCodeAssetInstallItem {
        kind,
        id: name.to_string(),
        name: name.to_string(),
        status: "installed".to_string(),
        message: "已安装".to_string(),
    })
}

fn install_command_file(
    name: &str,
    src: &Path,
    overwrite: bool,
) -> Result<ClaudeCodeAssetInstallItem, AppError> {
    validate_asset_name(name)?;
    let dst = claude_commands_dir().join(format!("{name}.md"));
    if dst.exists() {
        if !overwrite {
            return Ok(ClaudeCodeAssetInstallItem {
                kind: ClaudeCodeAssetKind::Command,
                id: name.to_string(),
                name: name.to_string(),
                status: "skipped".to_string(),
                message: "已存在，已跳过".to_string(),
            });
        }
        backup_path(ClaudeCodeAssetKind::Command, name, &dst)?;
        fs::remove_file(&dst)?;
    }
    fs::create_dir_all(claude_commands_dir())?;
    fs::copy(src, dst)?;
    Ok(ClaudeCodeAssetInstallItem {
        kind: ClaudeCodeAssetKind::Command,
        id: name.to_string(),
        name: name.to_string(),
        status: "installed".to_string(),
        message: "command 已安装".to_string(),
    })
}

fn remove_tree_with_backup(
    kind: ClaudeCodeAssetKind,
    id: &str,
    candidates: &[PathBuf],
) -> Result<(), AppError> {
    let Some(path) = candidates.iter().find(|p| p.exists()) else {
        return Err(AppError::not_found("资产不存在"));
    };
    backup_path(kind, id, path)?;
    remove_path(path)?;
    Ok(())
}

fn backup_mcp_config(name: &str) -> Result<(), AppError> {
    if let Some(config) = read_mcp_config(name)? {
        let dst = backup_root()
            .join(timestamp_slug())
            .join("mcp")
            .join(format!("{name}.json"));
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(dst, serde_json::to_vec_pretty(&config)?)?;
    }
    Ok(())
}

fn backup_path(kind: ClaudeCodeAssetKind, name: &str, src: &Path) -> Result<(), AppError> {
    let dst = backup_root()
        .join(timestamp_slug())
        .join(kind.as_str())
        .join(safe_name_segment(name)?);
    if src.is_dir() {
        copy_dir_all(src, &dst)?;
    } else {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dst)?;
    }
    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<(), AppError> {
    if !src.is_dir() {
        return Err(AppError::generic("来源不是目录"));
    }
    fs::create_dir_all(dst)?;
    for entry in WalkDir::new(src).follow_links(false) {
        let entry = entry.map_err(|e| AppError::generic(format!("遍历目录失败: {e}")))?;
        let path = entry.path();
        let rel = path
            .strip_prefix(src)
            .map_err(|e| AppError::generic(format!("计算相对路径失败: {e}")))?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(path, target)?;
        }
    }
    Ok(())
}

fn move_path(src: &Path, dst: &Path) -> Result<(), AppError> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(_) => {
            if src.is_dir() {
                copy_dir_all(src, dst)?;
                fs::remove_dir_all(src)?;
            } else {
                fs::copy(src, dst)?;
                fs::remove_file(src)?;
            }
            Ok(())
        }
    }
}

fn remove_path(path: &Path) -> Result<(), AppError> {
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn source_path(source: &ClaudeCodeInstallSource) -> Result<PathBuf, AppError> {
    source
        .path
        .as_ref()
        .map(PathBuf::from)
        .ok_or_else(|| AppError::generic("缺少来源路径"))
}

fn read_plugin_manifest(root: &Path) -> Result<PluginManifest, AppError> {
    let path = root.join(".claude-plugin").join("plugin.json");
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

fn read_skill_summary(path: &Path) -> Result<(Option<String>, Option<String>), AppError> {
    let text = fs::read_to_string(path)?;
    let mut name = None;
    let mut description = None;
    if let Some(frontmatter) = yaml_frontmatter(&text) {
        for line in frontmatter.lines() {
            if let Some((k, v)) = line.split_once(':') {
                let key = k.trim();
                let value = v.trim().trim_matches('"').trim_matches('\'').to_string();
                if key == "name" && !value.is_empty() {
                    name = Some(value);
                } else if key == "description" && !value.is_empty() {
                    description = Some(value);
                }
            }
        }
    }
    if description.is_none() {
        description = first_non_empty_markdown_line(&text);
    }
    Ok((name, description))
}

fn yaml_frontmatter(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---\n")?;
    let (frontmatter, _) = rest.split_once("\n---")?;
    Some(frontmatter)
}

fn first_non_empty_markdown_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("---") && !line.starts_with('#'))
        .map(|s| s.chars().take(160).collect())
}

fn describe_mcp(config: &Value) -> Option<String> {
    let typ = config
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("stdio");
    if let Some(url) = config.get("url").and_then(|v| v.as_str()) {
        Some(format!("{typ} · {url}"))
    } else if let Some(command) = config.get("command").and_then(|v| v.as_str()) {
        Some(format!("{typ} · {command}"))
    } else {
        Some(typ.to_string())
    }
}

/// 判定 MCP 配置是否含旧 peer 脱敏占位。
///
/// Business Logic（为什么需要这个函数）:
///     导入路径必须识别 legacy placeholder，避免写入假凭据或覆盖真值。
///
/// Code Logic（这个函数做什么）:
///     递归检测 `__REDACTED_BY_CLAUDE_PARTNER__` 字符串。
pub fn classify_mcp_import_credentials(value: &Value) -> McpImportCredentialStatus {
    if contains_redacted_placeholder(value) {
        McpImportCredentialStatus::LegacyLossy
    } else {
        McpImportCredentialStatus::Ok
    }
}

/// 诊断用：将敏感配置值替换为占位（不得用于 P2P export）。
///
/// Business Logic: 日志/错误摘要 value-redacted；生产 P2P export 不再调用。
/// Code Logic: 对敏感 key 替换为 placeholder。
/// 保留供诊断 facade / 单测；生产 export 已改走 classify 路径，故允许 dead_code。
#[allow(dead_code)]
pub fn redact_mcp_config_for_diagnostics(value: &Value) -> Value {
    redact_value(value, None)
}

#[allow(dead_code)]
fn redact_value(value: &Value, key: Option<&str>) -> Value {
    let key_l = key.unwrap_or("").to_lowercase();
    if is_sensitive_key(&key_l) {
        return match value {
            Value::Object(map) => Value::Object(
                map.keys()
                    .map(|k| (k.clone(), Value::String(REDACTED_PLACEHOLDER.to_string())))
                    .collect(),
            ),
            Value::Array(values) => Value::Array(
                values
                    .iter()
                    .map(|_| Value::String(REDACTED_PLACEHOLDER.to_string()))
                    .collect(),
            ),
            Value::Null => Value::Null,
            _ => Value::String(REDACTED_PLACEHOLDER.to_string()),
        };
    }
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), redact_value(v, Some(k))))
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(|v| redact_value(v, key)).collect()),
        _ => value.clone(),
    }
}

fn contains_sensitive_config(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(k, v)| {
            let key = k.to_lowercase();
            is_sensitive_key(&key) || contains_sensitive_config(v)
        }),
        Value::Array(values) => values.iter().any(contains_sensitive_config),
        _ => false,
    }
}

fn contains_redacted_placeholder(value: &Value) -> bool {
    match value {
        Value::String(s) => s == REDACTED_PLACEHOLDER,
        Value::Object(map) => map.values().any(contains_redacted_placeholder),
        Value::Array(values) => values.iter().any(contains_redacted_placeholder),
        _ => false,
    }
}

fn is_sensitive_key(key: &str) -> bool {
    matches!(key, "env" | "headers" | "oauth" | "headershelper")
        || key.contains("token")
        || key.contains("secret")
        || key.contains("password")
        || key.contains("authorization")
        || key == "api_key"
        || key == "apikey"
}

fn add_directory_to_zip<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    root: &Path,
    prefix: &str,
) -> Result<(), AppError> {
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|e| AppError::generic(format!("遍历目录失败: {e}")))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .map_err(|e| AppError::generic(format!("计算 zip 相对路径失败: {e}")))?;
        let name = format!("{prefix}/{}", path_to_zip_name(rel)?);
        add_file_to_zip(zip, entry.path(), &name)?;
    }
    Ok(())
}

fn add_file_to_zip<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    path: &Path,
    name: &str,
) -> Result<(), AppError> {
    let bytes = fs::read(path)?;
    add_bytes_to_zip(zip, name, &bytes)
}

fn add_bytes_to_zip<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    name: &str,
    bytes: &[u8],
) -> Result<(), AppError> {
    let options = FileOptions::default().compression_method(CompressionMethod::Deflated);
    zip.start_file(name, options)
        .map_err(|e| AppError::generic(format!("写入 zip 文件失败: {e}")))?;
    zip.write_all(bytes)?;
    Ok(())
}

fn extract_zip_safe(bytes: &[u8], dst: &Path) -> Result<(), AppError> {
    let reader = Cursor::new(bytes);
    let mut archive =
        ZipArchive::new(reader).map_err(|e| AppError::generic(format!("读取 zip 失败: {e}")))?;
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| AppError::generic(format!("读取 zip entry 失败: {e}")))?;
        if is_zip_symlink(&file) {
            return Err(AppError::generic("bundle 中包含 symlink，已拒绝"));
        }
        let enclosed = file
            .enclosed_name()
            .ok_or_else(|| AppError::generic("bundle 中包含不安全路径"))?
            .to_owned();
        let out = dst.join(enclosed);
        if file.name().ends_with('/') {
            fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out_file = fs::File::create(&out)?;
        std::io::copy(&mut file, &mut out_file)?;
    }
    Ok(())
}

fn is_zip_symlink(file: &zip::read::ZipFile<'_>) -> bool {
    file.unix_mode()
        .map(|mode| mode & 0o170000 == 0o120000)
        .unwrap_or(false)
}

fn path_to_zip_name(path: &Path) -> Result<String, AppError> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(s) => {
                let text = s
                    .to_str()
                    .ok_or_else(|| AppError::generic("路径包含非 UTF-8 片段"))?;
                parts.push(text.to_string());
            }
            _ => return Err(AppError::generic("路径包含不安全片段")),
        }
    }
    Ok(parts.join("/"))
}

fn hash_path(path: &Path) -> Result<String, AppError> {
    let mut hasher = Sha256::new();
    if path.is_file() {
        hasher.update(fs::read(path)?);
        return Ok(hex_encode(&hasher.finalize()));
    }
    let mut files = Vec::new();
    for entry in WalkDir::new(path).follow_links(false) {
        let entry = entry.map_err(|e| AppError::generic(format!("遍历目录失败: {e}")))?;
        if entry.file_type().is_file() {
            files.push(entry.path().to_path_buf());
        }
    }
    files.sort();
    for file in files {
        let rel = file
            .strip_prefix(path)
            .map_err(|e| AppError::generic(format!("计算 hash 相对路径失败: {e}")))?;
        hasher.update(path_to_zip_name(rel)?.as_bytes());
        hasher.update(fs::read(file)?);
    }
    Ok(hex_encode(&hasher.finalize()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn dir_size(path: &Path) -> Result<u64, AppError> {
    if path.is_file() {
        return Ok(fs::metadata(path)?.len());
    }
    let mut total = 0u64;
    for entry in WalkDir::new(path).follow_links(false) {
        let entry = entry.map_err(|e| AppError::generic(format!("遍历目录失败: {e}")))?;
        if entry.file_type().is_file() {
            total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0));
        }
    }
    Ok(total)
}

fn metadata_updated_at(path: &Path) -> Option<String> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(UNIX_EPOCH).ok()?;
    chrono::DateTime::<Utc>::from(UNIX_EPOCH + since)
        .to_rfc3339()
        .into()
}

fn detect_plugin_root(staging: &Path) -> PathBuf {
    if staging.join(".claude-plugin").join("plugin.json").exists() {
        return staging.to_path_buf();
    }
    if let Ok(entries) = fs::read_dir(staging) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.join(".claude-plugin").join("plugin.json").exists() {
                return path;
            }
        }
    }
    staging.to_path_buf()
}

fn safe_name_segment(name: &str) -> Result<String, AppError> {
    validate_asset_name(name)?;
    Ok(name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@') {
                c
            } else {
                '_'
            }
        })
        .collect())
}

fn validate_asset_name(name: &str) -> Result<(), AppError> {
    if name.trim().is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name
            .chars()
            .any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        || name == "."
        || name == ".."
    {
        return Err(AppError::generic("资产名称不合法"));
    }
    Ok(())
}

fn normalize_path_key(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

fn single_report(
    kind: ClaudeCodeAssetKind,
    id: &str,
    name: &str,
    status: &str,
    message: &str,
) -> ClaudeCodeAssetInstallReport {
    report_from_items(vec![ClaudeCodeAssetInstallItem {
        kind,
        id: id.to_string(),
        name: name.to_string(),
        status: status.to_string(),
        message: message.to_string(),
    }])
}

fn report_from_items(items: Vec<ClaudeCodeAssetInstallItem>) -> ClaudeCodeAssetInstallReport {
    let installed = items.iter().filter(|i| i.status == "installed").count();
    let skipped = items.iter().filter(|i| i.status == "skipped").count();
    let failed = items.iter().filter(|i| i.status == "failed").count();
    ClaudeCodeAssetInstallReport {
        ok: failed == 0,
        installed,
        skipped,
        failed,
        note: format!("installed={installed}, skipped={skipped}, failed={failed}"),
        items,
    }
}

fn timestamp_slug() -> String {
    Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string()
}

fn claude_dir() -> PathBuf {
    env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().expect("无法定位 home").join(".claude"))
}

fn claude_json_path() -> PathBuf {
    if let Some(dir) = env::var_os("CLAUDE_CONFIG_DIR") {
        PathBuf::from(dir).join(".claude.json")
    } else {
        dirs::home_dir()
            .expect("无法定位 home")
            .join(".claude.json")
    }
}

fn claude_skills_dir() -> PathBuf {
    claude_dir().join("skills")
}

fn claude_commands_dir() -> PathBuf {
    claude_dir().join("commands")
}

fn assets_root() -> PathBuf {
    config::config_dir()
        .expect("无法解析应用数据目录（检查 CC_PARTNER_DATA_DIR）")
        .join("claude-assets")
}

fn disabled_skills_dir() -> PathBuf {
    assets_root().join("disabled").join("skills")
}

fn disabled_commands_dir() -> PathBuf {
    assets_root().join("disabled").join("commands")
}

fn disabled_mcp_dir() -> PathBuf {
    assets_root().join("disabled").join("mcp")
}

fn backup_root() -> PathBuf {
    assets_root().join("backups")
}

fn incoming_dir() -> PathBuf {
    assets_root().join("incoming")
}

/// portable executor 使用的 Claude 路径根（可注入，便于单测隔离）。
///
/// Business Logic: B4 必须在临时 CLAUDE_CONFIG_DIR / data_dir 下执行，不污染开发者机器。
/// Code Logic: 汇总 skills/commands/mcp/disabled/backup 路径。
#[derive(Debug, Clone)]
pub struct PortableClaudeRoots {
    /// CLAUDE_CONFIG_DIR 或 ~/.claude
    #[allow(dead_code)]
    pub config_dir: PathBuf,
    /// .claude.json 路径
    pub claude_json_path: PathBuf,
    /// skills 启用区
    pub skills_dir: PathBuf,
    /// commands 启用区
    pub commands_dir: PathBuf,
    /// disabled skills
    pub disabled_skills_dir: PathBuf,
    /// disabled commands
    pub disabled_commands_dir: PathBuf,
    /// disabled mcp 快照
    pub disabled_mcp_dir: PathBuf,
    /// 可恢复备份根
    pub backup_root: PathBuf,
}

/// 解析 portable Claude 根（优先注入路径）。
///
/// Business Logic: executor 与 legacy 命令共享同一目录语义。
/// Code Logic: claude_config_dir / data_dir 覆盖环境默认。
pub fn portable_claude_roots(
    claude_config_dir: Option<&std::path::Path>,
    data_dir: Option<&std::path::Path>,
) -> Result<PortableClaudeRoots, AppError> {
    let config_dir = claude_config_dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(claude_dir);
    let claude_json_path = if let Some(dir) = claude_config_dir {
        dir.join(".claude.json")
    } else {
        claude_json_path()
    };
    let assets = if let Some(data) = data_dir {
        data.join("claude-assets")
    } else {
        assets_root()
    };
    Ok(PortableClaudeRoots {
        skills_dir: config_dir.join("skills"),
        commands_dir: config_dir.join("commands"),
        disabled_skills_dir: assets.join("disabled").join("skills"),
        disabled_commands_dir: assets.join("disabled").join("commands"),
        disabled_mcp_dir: assets.join("disabled").join("mcp"),
        backup_root: assets.join("backups"),
        config_dir,
        claude_json_path,
    })
}

/// crate-visible：安全 move（跨卷 fallback copy+remove）。
pub fn portable_move_path(src: &Path, dst: &Path) -> Result<(), AppError> {
    move_path(src, dst)
}

/// crate-visible：删除文件或目录。
pub fn portable_remove_path(path: &Path) -> Result<(), AppError> {
    remove_path(path)
}

/// crate-visible：备份到 backup_root。
pub fn portable_backup_path(
    kind: ClaudeCodeAssetKind,
    name: &str,
    src: &Path,
    backup_root: &Path,
) -> Result<(), AppError> {
    let dst = backup_root
        .join(timestamp_slug())
        .join(kind.as_str())
        .join(safe_name_segment(name)?);
    if src.is_dir() {
        copy_dir_all(src, &dst)?;
    } else {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dst)?;
    }
    Ok(())
}

/// crate-visible：删除前备份（Skill/Command uninstall）。
pub fn portable_remove_tree_with_backup(
    kind: ClaudeCodeAssetKind,
    id: &str,
    candidates: &[PathBuf],
    backup_root: &Path,
) -> Result<(), AppError> {
    let Some(path) = candidates.iter().find(|p| p.exists()) else {
        return Err(AppError::not_found("资产不存在"));
    };
    portable_backup_path(kind, id, path, backup_root)?;
    remove_path(path)?;
    Ok(())
}

/// crate-visible：Skill 树启停（active ↔ disabled move）。
pub fn portable_set_tree_enabled(
    kind: ClaudeCodeAssetKind,
    id: &str,
    enabled: bool,
    active_root: &Path,
    disabled_root: &Path,
    backup_root: &Path,
) -> Result<(), AppError> {
    let active = active_root.join(id);
    let disabled = disabled_root.join(id);
    if enabled {
        if !disabled.exists() {
            return Err(AppError::not_found("禁用区中没有该资产"));
        }
        if active.exists() {
            portable_backup_path(kind, id, &active, backup_root)?;
            remove_path(&active)?;
        }
        move_path(&disabled, &active)?;
    } else {
        if !active.exists() {
            return Err(AppError::not_found("启用区中没有该资产"));
        }
        fs::create_dir_all(disabled_root)?;
        if disabled.exists() {
            portable_backup_path(kind, id, &disabled, backup_root)?;
            remove_path(&disabled)?;
        }
        move_path(&active, &disabled)?;
    }
    Ok(())
}

/// crate-visible：Command 文件启停。
pub fn portable_set_command_enabled(
    id: &str,
    enabled: bool,
    active_root: &Path,
    disabled_root: &Path,
    backup_root: &Path,
) -> Result<(), AppError> {
    let filename = format!("{id}.md");
    let active = active_root.join(&filename);
    let disabled = disabled_root.join(&filename);
    if enabled {
        if active.exists() {
            return Ok(());
        }
        fs::create_dir_all(active_root)?;
        if !disabled.exists() {
            return Err(AppError::not_found("禁用区中没有该 command"));
        }
        move_path(&disabled, &active)?;
    } else {
        fs::create_dir_all(disabled_root)?;
        if disabled.exists() {
            portable_backup_path(ClaudeCodeAssetKind::Command, id, &disabled, backup_root)?;
            remove_path(&disabled)?;
        }
        if !active.exists() {
            return Err(AppError::not_found("启用区中没有该 command"));
        }
        move_path(&active, &disabled)?;
    }
    Ok(())
}

/// 稳定 content/tree hash（文件或目录），供 executor changed-source 校验与测试。
///
/// 注：portable action 源 hash 已统一到 inventory 域（Skill=SKILL.md-only）；
/// 本 helper 仍保留给需要整树 walk 的非 portable 路径。
#[allow(dead_code)]
pub fn portable_content_hash_path(path: &Path) -> Result<String, AppError> {
    use crate::agent_hub::object_store::sha256_hex;
    use sha2::{Digest, Sha256};
    if path.is_file() {
        return Ok(sha256_hex(&fs::read(path)?));
    }
    let mut hasher = Sha256::new();
    let mut entries: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(path).follow_links(false) {
        let entry = entry.map_err(|e| AppError::generic(format!("walk: {e}")))?;
        if entry.file_type().is_file() {
            entries.push(entry.path().to_path_buf());
        }
    }
    entries.sort();
    for f in entries {
        let rel = f.strip_prefix(path).unwrap_or(&f);
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update(fs::read(&f)?);
    }
    Ok(sha256_hex(hasher.finalize().as_slice()))
}

/// 测试串行锁（供 portable_actions executor 测试复用）。
#[cfg(test)]
#[allow(dead_code)]
pub fn claude_assets_env_lock_for_tests() -> std::sync::MutexGuard<'static, ()> {
    claude_assets_env_lock()
}

/// 串行化依赖 HOME/CLAUDE_CONFIG_DIR 的测试（模块级，供 harness + 单测共用）。
#[cfg(test)]
fn claude_assets_env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

/// Gate C N/N+1 runtime proof：旧 peer `legacyLossy` placeholder 不得覆盖本机 canonical credential。
///
/// Business Logic: mixed-version harness 与 Gate B 单测共用同一 fail-closed 合同，避免仅靠源码盘点。
/// Code Logic: 隔离 CLAUDE_CONFIG_DIR，预置真实 MCP 凭据，classify 为 LegacyLossy 且文件内容不变。
#[cfg(test)]
pub fn prove_legacy_lossy_placeholder_never_overwrites_canonical_credential() {
    let _g = claude_assets_env_lock();

    let dir = tempfile::tempdir().expect("tempdir");
    let claude = dir.path().join("claude-home");
    fs::create_dir_all(&claude).expect("claude home");
    fs::write(
        claude.join(".claude.json"),
        r#"{"mcpServers":{"private-api":{"env":{"API_TOKEN":"real-local"}}}}"#,
    )
    .expect("seed mcp");
    let data = dir.path().join("cc-data");
    fs::create_dir_all(&data).expect("data");
    // SAFETY: 串行 env_lock 持有期间修改测试 env。
    // 锁序约定：DATA → 其它 env key（与 user_mirror seed_dual_env 一致）；
    // HOME 先于 DATA 会与其构成 AB-BA 死锁（并行 lib 测试挂死根因）。
    let _data_dir_guard =
        crate::config::install_data_dir_env(Some(data.to_str().expect("utf8 data dir")));
    let _claude_env_guard =
        crate::config::install_env_var("CLAUDE_CONFIG_DIR", Some(claude.to_str().expect("utf8")));
    let _home_env_guard =
        crate::config::install_env_var("HOME", Some(dir.path().to_str().expect("utf8")));

    let lossy = serde_json::json!({
        "env": { "API_TOKEN": REDACTED_PLACEHOLDER },
        "headers": { "Authorization": REDACTED_PLACEHOLDER }
    });
    assert_eq!(
        classify_mcp_import_credentials(&lossy),
        McpImportCredentialStatus::LegacyLossy,
        "old-peer placeholder must classify as legacyLossy"
    );

    let before = read_mcp_config("private-api")
        .expect("read before")
        .expect("canonical mcp present");
    assert_eq!(before["env"]["API_TOKEN"], "real-local");

    // 生产 install_manifest_item 在 LegacyLossy 分支不调用 install_mcp_config。
    if classify_mcp_import_credentials(&lossy) == McpImportCredentialStatus::LegacyLossy {
        // no-op write — fail-closed
    } else {
        panic!("expected legacy lossy classification");
    }

    let after = read_mcp_config("private-api")
        .expect("read after")
        .expect("canonical still present");
    assert_eq!(
        after["env"]["API_TOKEN"], "real-local",
        "legacyLossy must never overwrite canonical credential"
    );
    assert_ne!(after["env"]["API_TOKEN"], REDACTED_PLACEHOLDER);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 串行化依赖 HOME/CLAUDE_CONFIG_DIR 的测试，避免污染并行用例。
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        claude_assets_env_lock()
    }

    #[test]
    fn diagnostic_redaction_still_masks_secrets() {
        let input = serde_json::json!({
            "type": "http",
            "url": "https://example.com/mcp",
            "headers": { "Authorization": "Bearer real-token" },
            "env": { "API_KEY": "secret" }
        });
        let out = redact_mcp_config_for_diagnostics(&input);
        assert_eq!(out["url"], "https://example.com/mcp");
        assert_eq!(out["headers"]["Authorization"], REDACTED_PLACEHOLDER);
        assert_eq!(out["env"]["API_KEY"], REDACTED_PLACEHOLDER);
    }

    #[test]
    fn list_assets_sorts_by_kind_then_name_case_insensitive() {
        let _g = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude-home");
        fs::create_dir_all(claude.join("skills/Zeta")).unwrap();
        fs::write(
            claude.join("skills/Zeta/SKILL.md"),
            "---\nname: Zeta\n---\n",
        )
        .unwrap();
        fs::create_dir_all(claude.join("skills/alpha")).unwrap();
        fs::write(
            claude.join("skills/alpha/SKILL.md"),
            "---\nname: alpha\n---\n",
        )
        .unwrap();
        fs::create_dir_all(claude.join("commands")).unwrap();
        fs::write(claude.join("commands/beta.md"), "beta cmd\n").unwrap();
        fs::write(
            claude.join(".claude.json"),
            r#"{"mcpServers":{"m1":{"command":"echo"}}}"#,
        )
        .unwrap();

        // 隔离：CLAUDE_CONFIG_DIR + 假 home 避免读真实环境
        // 锁序约定：DATA → 其它 env key，避免与 user_mirror 的 DATA→HOME 逆序死锁。
        // assets_root 走 CC_PARTNER_DATA_DIR
        let data = dir.path().join("cc-data");
        fs::create_dir_all(&data).unwrap();
        let _data_dir_guard =
            crate::config::install_data_dir_env(Some(data.to_str().expect("utf8 data dir")));
        let _claude_env_guard = crate::config::install_env_var(
            "CLAUDE_CONFIG_DIR",
            Some(claude.to_str().expect("utf8")),
        );
        let _home_env_guard =
            crate::config::install_env_var("HOME", Some(dir.path().to_str().expect("utf8")));

        let assets = tauri::async_runtime::block_on(list_assets()).expect("list");
        // plugin CLI 可能失败被跳过；skills/commands/mcp 应在
        let kinds: Vec<_> = assets.iter().map(|a| a.kind.as_str()).collect();
        assert!(
            kinds.windows(2).all(|w| w[0] <= w[1]),
            "kinds not sorted: {kinds:?}"
        );
        let skill_names: Vec<_> = assets
            .iter()
            .filter(|a| a.kind == ClaudeCodeAssetKind::Skill)
            .map(|a| a.name.to_lowercase())
            .collect();
        assert!(
            skill_names.windows(2).all(|w| w[0] <= w[1]),
            "skill names not sorted: {skill_names:?}"
        );
    }

    #[test]
    fn build_bundle_preserves_exact_mcp_secrets_no_placeholder() {
        let _g = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude-home");
        fs::create_dir_all(&claude).unwrap();
        let mcp = serde_json::json!({
            "mcpServers": {
                "private-api": {
                    "type": "http",
                    "url": "https://example.invalid/mcp?token=plain-fixture",
                    "headers": { "Authorization": "Bearer plain-fixture" },
                    "env": { "API_TOKEN": "plain-fixture" }
                }
            }
        });
        fs::write(
            claude.join(".claude.json"),
            serde_json::to_vec_pretty(&mcp).unwrap(),
        )
        .unwrap();
        let data = dir.path().join("cc-data");
        fs::create_dir_all(&data).unwrap();
        // 锁序约定：DATA → 其它 env key，避免与 user_mirror 的 DATA→HOME 逆序死锁。
        let _data_dir_guard =
            crate::config::install_data_dir_env(Some(data.to_str().expect("utf8 data dir")));
        let _claude_env_guard = crate::config::install_env_var(
            "CLAUDE_CONFIG_DIR",
            Some(claude.to_str().expect("utf8")),
        );
        let _home_env_guard =
            crate::config::install_env_var("HOME", Some(dir.path().to_str().expect("utf8")));

        let bytes = tauri::async_runtime::block_on(build_bundle(vec![ClaudeCodeAssetSelector {
            kind: ClaudeCodeAssetKind::Mcp,
            id: "private-api".into(),
        }]))
        .expect("bundle");
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains(REDACTED_PLACEHOLDER),
            "bundle must not contain redaction placeholder"
        );
        // zip 内是压缩数据；解压断言
        let staging = dir.path().join("unz");
        fs::create_dir_all(&staging).unwrap();
        extract_zip_safe(&bytes, &staging).unwrap();
        let mut found = false;
        for entry in WalkDir::new(&staging) {
            let entry = entry.unwrap();
            if entry.file_name() == "mcp.json" {
                let body = fs::read_to_string(entry.path()).unwrap();
                assert!(body.contains("plain-fixture"));
                assert!(body.contains("Bearer plain-fixture"));
                assert!(body.contains("token=plain-fixture"));
                assert!(!body.contains(REDACTED_PLACEHOLDER));
                found = true;
            }
        }
        assert!(found, "mcp.json missing from bundle");
    }

    #[test]
    fn legacy_placeholder_import_is_legacy_lossy_and_does_not_overwrite() {
        // Gate B stable name — runtime fail-closed for old-peer placeholder vs canonical credential.
        // Shared body is also exercised by sync::mixed_version harness (Gate C N/N+1).
        // prove_* takes the shared env lock internally (do not nest env_lock here).
        prove_legacy_lossy_placeholder_never_overwrites_canonical_credential();
    }

    #[test]
    fn rejects_unsafe_asset_names() {
        assert!(validate_asset_name("good-name").is_ok());
        assert!(validate_asset_name("../bad").is_err());
        assert!(validate_asset_name("bad/name").is_err());
    }

    #[test]
    fn path_to_zip_name_rejects_parent_components() {
        assert_eq!(path_to_zip_name(Path::new("a/b.txt")).unwrap(), "a/b.txt");
        assert!(path_to_zip_name(Path::new("../b.txt")).is_err());
    }
}
