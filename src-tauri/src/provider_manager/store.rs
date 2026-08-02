//! provider_manager/store.rs — 对 cc-switch 的 SQLite 与 settings.json 只读访问。
//!
//! Business Logic（为什么需要这个模块）:
//!     Provider Manager 的"读"路径直接查询 cc-switch 的数据库展示 provider 列表，
//!     避免依赖 cc-switch CLI 的输出格式；"写"路径（切换）才委托给 CLI。
//!     只读访问使用 `mode=ro`，绝不与 cc-switch GUI 的写入竞争 WAL。
//!
//! Code Logic（这个模块做什么）:
//!     解析 cc-switch 目录（`CC_SWITCH_CONFIG_DIR` 或 `~/.cc-switch`）；
//!     以只读连接查询 `providers` 表并按 `app_type` 分组；
//!     读 `settings.json` 解析 `currentProvider*`（优先级高于 DB `is_current`）；
//!     组装前端 `AppProviders`（隐藏 0 provider 的 app，排除 `claude-desktop`）。

use crate::error::AppError;
use crate::provider_manager::models::{AgentApp, AppProviders, ProviderEntry};
use serde_json::Value;
use sqlx::{Connection, Row};
use std::collections::HashMap;
use std::path::PathBuf;

/// 一行 provider 的内部投影（仅取展示所需字段，绝不读 `settings_config`）。
#[derive(Debug, Clone)]
pub(super) struct ProviderRow {
    pub id: String,
    pub name: String,
    pub category: Option<String>,
    pub is_current: bool,
}

/// 解析 cc-switch 目录：`CC_SWITCH_CONFIG_DIR` 优先，否则 `~/.cc-switch`。
pub(super) fn cc_switch_dir() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("CC_SWITCH_CONFIG_DIR") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    dirs::home_dir().map(|h| h.join(".cc-switch"))
}

fn cc_switch_db_path() -> Option<PathBuf> {
    cc_switch_dir().map(|d| d.join("cc-switch.db"))
}

/// cc-switch 数据库文件是否存在。
pub(super) fn db_present() -> bool {
    cc_switch_db_path().map(|p| p.is_file()).unwrap_or(false)
}

/// 读取 `~/.cc-switch/settings.json` 为 `serde_json::Value`（缺失/损坏返回 `None`）。
pub(super) fn read_settings() -> Option<Value> {
    let path = cc_switch_dir()?.join("settings.json");
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 从 settings.json 取 `currentProvider<Pascal>` 的值。
pub(super) fn settings_current(settings: &Value, app: AgentApp) -> Option<String> {
    settings
        .get(app.settings_current_key())?
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 一次性只读查询全部 provider，按 `app_type` 分组。
///
/// 数据库缺失/表不存在/读取失败时返回空 map（而非错误），保证 UI 始终可渲染。
pub(super) async fn fetch_all_providers() -> Result<HashMap<String, Vec<ProviderRow>>, AppError> {
    let path = match cc_switch_db_path() {
        Some(p) if p.is_file() => p,
        _ => return Ok(HashMap::new()),
    };
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path.clone())
        .read_only(true);
    let mut conn = match sqlx::sqlite::SqliteConnection::connect_with(&opts).await {
        Ok(c) => c,
        Err(e) => {
            // 只读打开失败（例如 WAL sidecar 缺失）不应阻断整个特性。
            tracing::warn!("以只读方式打开 cc-switch 数据库失败: {e}");
            return Ok(HashMap::new());
        }
    };
    let result = sqlx::query(
        "SELECT id, app_type, name, category, is_current \
         FROM providers ORDER BY app_type, sort_index, name",
    )
    .fetch_all(&mut conn)
    .await;
    let _ = conn.close().await;
    let rows = match result {
        Ok(r) => r,
        Err(e) => {
            // 表不存在（更老的 cc-switch）等情况下当作无 provider。
            tracing::warn!("读取 cc-switch providers 表失败: {e}");
            return Ok(HashMap::new());
        }
    };
    let mut grouped: HashMap<String, Vec<ProviderRow>> = HashMap::new();
    for r in &rows {
        let app_type: String = r.try_get::<String, _>("app_type").unwrap_or_default();
        let row = ProviderRow {
            id: r.try_get::<String, _>("id").unwrap_or_default(),
            name: r.try_get::<String, _>("name").unwrap_or_default(),
            category: r.try_get::<Option<String>, _>("category").ok().flatten(),
            is_current: r.try_get::<bool, _>("is_current").unwrap_or(false),
        };
        grouped.entry(app_type).or_default().push(row);
    }
    Ok(grouped)
}

/// 计算某 agent 的当前 provider id：settings.json 优先，否则回落 DB `is_current`。
fn resolve_current_id(
    settings: Option<&Value>,
    app: AgentApp,
    rows: &[ProviderRow],
) -> Option<String> {
    settings
        .and_then(|s| settings_current(s, app))
        .or_else(|| rows.iter().find(|r| r.is_current).map(|r| r.id.clone()))
}

/// 由分组数据 + settings.json 组装单个 `AppProviders`。
fn build_app_providers(
    app: AgentApp,
    rows: Vec<ProviderRow>,
    settings: Option<&Value>,
) -> AppProviders {
    let current_id = resolve_current_id(settings, app, &rows);
    let providers = rows
        .into_iter()
        .map(|r| {
            let is_current = Some(&r.id) == current_id.as_ref();
            ProviderEntry {
                id: r.id,
                name: r.name,
                category: r.category,
                is_current,
            }
        })
        .collect();
    AppProviders {
        app,
        providers,
        current_provider_id: current_id,
    }
}

/// 组装所有受支持 agent 的 provider 列表（隐藏 0 provider 的 app）。
pub(super) async fn list_apps() -> Result<Vec<AppProviders>, AppError> {
    let grouped = fetch_all_providers().await?;
    let settings = read_settings();
    let mut out = Vec::new();
    for &app in AgentApp::all() {
        let rows = match grouped.get(app.as_str()) {
            Some(r) if !r.is_empty() => r.clone(),
            _ => continue,
        };
        out.push(build_app_providers(app, rows, settings.as_ref()));
    }
    Ok(out)
}

/// 切换后重读单个 agent，返回更新后的 `AppProviders`。
pub(super) async fn refresh_app(app: AgentApp) -> Result<AppProviders, AppError> {
    let grouped = fetch_all_providers().await?;
    let settings = read_settings();
    let rows = grouped.get(app.as_str()).cloned().unwrap_or_default();
    Ok(build_app_providers(app, rows, settings.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn seed_db(dir: &std::path::Path) {
        // 用可写连接建表并插入测试数据；生产读路径用 mode=ro 打开同一文件。
        let url = format!("sqlite://{}", dir.join("cc-switch.db").to_string_lossy());
        let opts = SqliteConnectOptions::from_str(&url)
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE providers (\
               id TEXT NOT NULL, app_type TEXT NOT NULL, name TEXT NOT NULL, \
               settings_config TEXT NOT NULL DEFAULT '{}', \
               category TEXT, sort_index INTEGER, is_current BOOLEAN NOT NULL DEFAULT 0, \
               PRIMARY KEY (id, app_type))",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO providers (id, app_type, name, category, sort_index, is_current) VALUES \
             ('default','claude','智谱','custom',0,1),\
             ('off','claude','Claude Official','official',1,0),\
             ('openai','codex','OpenAI Official','official',0,1),\
             ('minimax','codex','MiniMax','cn_official',1,0),\
             ('cd1','claude-desktop','Desktop','official',0,1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }

    #[tokio::test]
    async fn list_apps_filters_empty_and_excludes_claude_desktop() {
        let dir = tempfile::tempdir().unwrap();
        seed_db(dir.path()).await;
        // CC_SWITCH_CONFIG_DIR 指向临时目录，避免读到真实 ~/.cc-switch。
        std::env::set_var("CC_SWITCH_CONFIG_DIR", dir.path());
        let apps = list_apps().await.unwrap();
        std::env::remove_var("CC_SWITCH_CONFIG_DIR");

        let kinds: Vec<&str> = apps.iter().map(|a| a.app.as_str()).collect();
        assert_eq!(kinds, vec!["claude", "codex"]); // gemini/opencode/hermes/openclaw/claude-desktop 无数据
    }

    #[tokio::test]
    async fn current_provider_resolution_prefers_settings_json_over_db() {
        let dir = tempfile::tempdir().unwrap();
        seed_db(dir.path()).await;
        // settings.json 把 claude 当前 provider 指向 'off'，覆盖 DB 的 'default'。
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::json!({"currentProviderClaude": "off"}).to_string(),
        )
        .unwrap();
        std::env::set_var("CC_SWITCH_CONFIG_DIR", dir.path());
        let apps = list_apps().await.unwrap();
        std::env::remove_var("CC_SWITCH_CONFIG_DIR");

        let claude = apps.iter().find(|a| a.app == AgentApp::Claude).unwrap();
        assert_eq!(claude.current_provider_id.as_deref(), Some("off"));
        assert!(claude
            .providers
            .iter()
            .any(|p| p.id == "off" && p.is_current));
        assert!(claude
            .providers
            .iter()
            .any(|p| p.id == "default" && !p.is_current));

        // codex 没有设置 settings.json，回落到 DB is_current='openai'。
        let codex = apps.iter().find(|a| a.app == AgentApp::Codex).unwrap();
        assert_eq!(codex.current_provider_id.as_deref(), Some("openai"));
    }

    #[test]
    fn settings_current_ignores_blank_values() {
        let v = serde_json::json!({"currentProviderClaude": "  "});
        assert_eq!(settings_current(&v, AgentApp::Claude), None);
        let v = serde_json::json!({"currentProviderCodex": "abc"});
        assert_eq!(
            settings_current(&v, AgentApp::Codex),
            Some("abc".to_string())
        );
        assert_eq!(settings_current(&v, AgentApp::Claude), None);
    }
}
