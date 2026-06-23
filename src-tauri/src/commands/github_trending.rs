//! commands/github_trending.rs — GitHub 周热门首页命令
//!
//! Business Logic（为什么需要这个模块）:
//!     首页需要展示 GitHub Trending Weekly 全语言 Top 25，并为每个项目提供中英文简介解说。
//!     GitHub 没有官方 Trending JSON API，因此后端抓取 `github.com/trending?since=weekly`
//!     HTML 后解析；Claude Code CLI 解说结果按 UTC 日期缓存，避免重复网络请求和重复 AI 消耗。
//!
//! Code Logic（这个模块做什么）:
//!     - `list_github_trending_repos`：读当天缓存；未命中则抓 GitHub、调用 Claude CLI、写缓存。
//!     - `get/default/update_github_trending_config`：设置页读写/恢复 CLI 路径、模型、缓存时长。
//!     - `test_claude_cli`：只执行 `claude --version` 验证本机 CLI 可用性。
//!     - 私有 helper 负责 HTML 解析、SQLite cache；Claude CLI 结构化执行复用 `claude_cli`。

use crate::claude_cli;
use crate::config::GithubTrendingConfig;
use crate::error::AppError;
use crate::state::AppState;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Row, SqlitePool};
use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;
use tauri::State;
use tokio::process::Command;

const TRENDING_URL: &str = "https://github.com/trending?since=weekly";
const CACHE_PREFIX: &str = "weekly:any:25";
const TOP_LIMIT: usize = 25;
const GITHUB_TIMEOUT_SECS: u64 = 20;
const CLAUDE_TIMEOUT_SECS: u64 = 180;
const CLAUDE_VERSION_TIMEOUT_SECS: u64 = 10;

/// GitHub Trending 配置 DTO（camelCase，对齐前端类型）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubTrendingConfigDto {
    pub ai_enabled: bool,
    pub claude_cli_path: String,
    pub claude_model: String,
    pub cache_ttl_hours: i64,
}

/// 单个 GitHub Trending 仓库 DTO。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubTrendingRepoDto {
    pub rank: u32,
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub url: String,
    pub description: String,
    pub language: Option<String>,
    pub stars: u64,
    pub forks: u64,
    pub stars_this_week: u64,
    pub explanation_zh: String,
    pub explanation_en: String,
}

/// GitHub Trending 首页响应 DTO。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubTrendingResponseDto {
    pub repos: Vec<GithubTrendingRepoDto>,
    pub fetched_at: String,
    pub expires_at: String,
    pub from_cache: bool,
    pub stale: bool,
    pub ai_status: String,
    pub ai_error: Option<String>,
}

/// Claude CLI 测试结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCliTestResult {
    pub ok: bool,
    pub version: Option<String>,
    pub error: Option<String>,
}

/// 缓存 payload：与响应主体一致，但不含本次读取态 fromCache/stale。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GithubTrendingPayload {
    repos: Vec<GithubTrendingRepoDto>,
    fetched_at: String,
    expires_at: String,
    ai_status: String,
    ai_error: Option<String>,
    #[serde(default)]
    ai_retry_attempted: bool,
}

/// Claude CLI 结构化输出外层。
#[derive(Debug, Clone, Deserialize)]
struct AiOutput {
    repos: Vec<AiRepoExplanation>,
}

/// Claude CLI 为单个仓库生成的中英文解说。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AiRepoExplanation {
    full_name: String,
    explanation_zh: String,
    explanation_en: String,
}

/// 将配置结构转成前端 DTO。
fn config_to_dto(config: &GithubTrendingConfig) -> GithubTrendingConfigDto {
    GithubTrendingConfigDto {
        ai_enabled: config.ai_enabled,
        claude_cli_path: config.claude_cli_path.clone(),
        claude_model: config.claude_model.clone(),
        cache_ttl_hours: config.cache_ttl_hours,
    }
}

/// 读取 GitHub Trending / Claude 解说配置。
///
/// Business Logic: 设置页初始化时展示当前 CLI 路径、模型、缓存策略。
#[tauri::command]
pub async fn get_github_trending_config(
    state: State<'_, AppState>,
) -> Result<GithubTrendingConfigDto, AppError> {
    let cfg = state.config.read().unwrap();
    Ok(config_to_dto(&cfg.github_trending))
}

/// 读取 GitHub Trending / Claude 解说默认配置。
///
/// Business Logic: 设置页 AI tab 需要一键回到应用默认 CLI 路径、模型和缓存策略。
/// Code Logic: 不读取或写入当前配置，直接返回 GithubTrendingConfig::default() 对应 DTO。
#[tauri::command]
pub async fn get_default_github_trending_config() -> Result<GithubTrendingConfigDto, AppError> {
    Ok(config_to_dto(&GithubTrendingConfig::default()))
}

/// 更新 GitHub Trending / Claude 解说配置。
///
/// Business Logic: 用户在设置页应用配置后需落盘，下次首页刷新立即按新配置生效。
/// Code Logic: 取写锁应用 patch；对数值做保守下限/上限；保存 config.json；返回最新 DTO。
#[tauri::command]
pub async fn update_github_trending_config(
    state: State<'_, AppState>,
    ai_enabled: Option<bool>,
    claude_cli_path: Option<String>,
    claude_model: Option<String>,
    cache_ttl_hours: Option<i64>,
) -> Result<GithubTrendingConfigDto, AppError> {
    {
        let mut cfg = state.config.write().unwrap();
        if let Some(enabled) = ai_enabled {
            cfg.github_trending.ai_enabled = enabled;
        }
        if let Some(path) = claude_cli_path {
            cfg.github_trending.claude_cli_path = if path.trim().is_empty() {
                "claude".to_string()
            } else {
                path.trim().to_string()
            };
        }
        if let Some(model) = claude_model {
            cfg.github_trending.claude_model = if model.trim().is_empty() {
                "sonnet".to_string()
            } else {
                model.trim().to_string()
            };
        }
        if let Some(hours) = cache_ttl_hours {
            cfg.github_trending.cache_ttl_hours = hours.clamp(1, 168);
        }
        cfg.save()?;
    }

    let cfg = state.config.read().unwrap();
    Ok(config_to_dto(&cfg.github_trending))
}

/// 测试 Claude Code CLI 是否可用。
///
/// Business Logic: 设置页“测试 Claude CLI”只需要验证命令存在并能输出版本，不触发模型调用。
#[tauri::command]
pub async fn test_claude_cli(
    state: State<'_, AppState>,
    claude_cli_path: Option<String>,
) -> Result<ClaudeCliTestResult, AppError> {
    let mut cfg = state.config.read().unwrap().github_trending.clone();
    if let Some(path) = claude_cli_path {
        cfg.claude_cli_path = if path.trim().is_empty() {
            "claude".to_string()
        } else {
            path.trim().to_string()
        };
    }
    Ok(run_claude_version(&cfg).await)
}

/// 返回 GitHub Trending Weekly Top 25。
///
/// Business Logic: 首页打开时先读当天缓存；缓存未命中才抓取 GitHub 并生成 AI 解说。
///     若 GitHub 刷新失败但存在旧缓存，则回退旧缓存并标记 stale，保证首页尽量可用。
#[tauri::command]
pub async fn list_github_trending_repos(
    state: State<'_, AppState>,
) -> Result<GithubTrendingResponseDto, AppError> {
    let config = state.config.read().unwrap().github_trending.clone();
    let now = Utc::now();
    let key = cache_key(now);

    if let Some(payload) = load_cache(&state.db, &key).await? {
        if !is_expired(&payload.expires_at, now) {
            if should_retry_failed_ai_cache(&payload, &config) {
                return refresh_cached_ai_cache(&state.db, &key, &config, payload).await;
            }
            return Ok(payload_to_response(payload, true, false, None));
        }
    }

    let repos = match fetch_weekly_trending().await {
        Ok(repos) => repos,
        Err(err) => {
            if let Some(payload) = load_latest_cache(&state.db).await? {
                return Ok(payload_to_response(
                    payload,
                    true,
                    true,
                    Some(format!("GitHub refresh failed: {err}")),
                ));
            }
            return Err(err);
        }
    };

    let (repos, ai_status, ai_error) = if config.ai_enabled {
        match generate_explanations(&config, &repos).await {
            Ok(explanations) => (
                merge_explanations(repos, explanations),
                "ready".to_string(),
                None,
            ),
            Err(err) => (repos, "failed".to_string(), Some(err.to_string())),
        }
    } else {
        (repos, "disabled".to_string(), None)
    };

    let ttl_hours = config.cache_ttl_hours.clamp(1, 168);
    let fetched_at = now.to_rfc3339();
    let expires_at = (now + ChronoDuration::hours(ttl_hours)).to_rfc3339();
    let payload = GithubTrendingPayload {
        repos,
        fetched_at,
        expires_at,
        ai_status,
        ai_error,
        ai_retry_attempted: false,
    };
    store_cache(&state.db, &key, &payload).await?;
    Ok(payload_to_response(payload, false, false, None))
}

/// 构造当天缓存 key。
fn cache_key(now: DateTime<Utc>) -> String {
    format!("{}:{}", CACHE_PREFIX, now.format("%Y-%m-%d"))
}

/// 判断缓存是否过期；解析失败时保守视作过期。
fn is_expired(expires_at: &str, now: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(expires_at)
        .map(|d| d.with_timezone(&Utc) <= now)
        .unwrap_or(true)
}

/// 将缓存 payload 转成前端响应，并叠加本次读取态。
fn payload_to_response(
    payload: GithubTrendingPayload,
    from_cache: bool,
    stale: bool,
    override_ai_error: Option<String>,
) -> GithubTrendingResponseDto {
    GithubTrendingResponseDto {
        repos: payload.repos,
        fetched_at: payload.fetched_at,
        expires_at: payload.expires_at,
        from_cache,
        stale,
        ai_status: payload.ai_status,
        ai_error: override_ai_error.or(payload.ai_error),
    }
}

/// 用缓存中的 GitHub 榜单重新生成 Claude 解说。
///
/// Business Logic（为什么需要这个函数）:
///     旧版本在 Claude CLI 非零退出且 stderr 为空时会把当天缓存标记成泛化失败，用户修复配置或升级后
///     仍会被未过期缓存挡住，所以需要对这类失败缓存做一次轻量重试。
///
/// Code Logic（这个函数做什么）:
///     复用缓存中的 repo 列表调用 Claude CLI，保留原 fetched_at/expires_at，写回同一缓存 key，
///     并以 fromCache=true 返回给前端，避免重新抓取 GitHub。
async fn refresh_cached_ai_cache(
    db: &SqlitePool,
    key: &str,
    config: &GithubTrendingConfig,
    payload: GithubTrendingPayload,
) -> Result<GithubTrendingResponseDto, AppError> {
    let (repos, ai_status, ai_error) = match generate_explanations(config, &payload.repos).await {
        Ok(explanations) => (
            merge_explanations(payload.repos, explanations),
            "ready".to_string(),
            None,
        ),
        Err(err) => (payload.repos, "failed".to_string(), Some(err.to_string())),
    };
    let refreshed = GithubTrendingPayload {
        repos,
        fetched_at: payload.fetched_at,
        expires_at: payload.expires_at,
        ai_status,
        ai_error,
        ai_retry_attempted: true,
    };
    store_cache(db, key, &refreshed).await?;
    Ok(payload_to_response(refreshed, true, false, None))
}

/// 判断未过期失败缓存是否应该重试 Claude 解说。
///
/// Business Logic（为什么需要这个函数）:
///     用户遇到的旧泛化错误没有可操作信息，且可能已经由新版命令形态修复；继续返回旧缓存会让问题看似未解决。
///
/// Code Logic（这个函数做什么）:
///     仅在 AI 仍启用、缓存状态为 failed、错误文本匹配旧的“命令返回非零状态”兜底文案时允许重试。
fn should_retry_failed_ai_cache(
    payload: &GithubTrendingPayload,
    config: &GithubTrendingConfig,
) -> bool {
    config.ai_enabled
        && payload.ai_status == "failed"
        && !payload.ai_retry_attempted
        && payload
            .ai_error
            .as_deref()
            .map(|error| error.contains("命令返回非零状态"))
            .unwrap_or(false)
}

/// 从 SQLite 读取指定 key 的缓存。
async fn load_cache(db: &SqlitePool, key: &str) -> Result<Option<GithubTrendingPayload>, AppError> {
    let row = sqlx::query("SELECT payload FROM github_trending_cache WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let payload: String = row.try_get("payload")?;
    let parsed = serde_json::from_str::<GithubTrendingPayload>(&payload)?;
    Ok(Some(parsed))
}

/// 读取最近一份缓存，用于 GitHub 刷新失败时兜底。
async fn load_latest_cache(db: &SqlitePool) -> Result<Option<GithubTrendingPayload>, AppError> {
    let row = sqlx::query(
        "SELECT payload FROM github_trending_cache
         WHERE key LIKE 'weekly:any:25:%'
         ORDER BY fetched_at DESC
         LIMIT 1",
    )
    .fetch_optional(db)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let payload: String = row.try_get("payload")?;
    let parsed = serde_json::from_str::<GithubTrendingPayload>(&payload)?;
    Ok(Some(parsed))
}

/// 写入/覆盖当天缓存。
async fn store_cache(
    db: &SqlitePool,
    key: &str,
    payload: &GithubTrendingPayload,
) -> Result<(), AppError> {
    let text = serde_json::to_string(payload)?;
    sqlx::query(
        "INSERT OR REPLACE INTO github_trending_cache
         (key, payload, fetched_at, expires_at, ai_status, ai_error)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(key)
    .bind(text)
    .bind(&payload.fetched_at)
    .bind(&payload.expires_at)
    .bind(&payload.ai_status)
    .bind(&payload.ai_error)
    .execute(db)
    .await?;
    Ok(())
}

/// 抓取 GitHub Trending Weekly HTML 并解析为 DTO。
async fn fetch_weekly_trending() -> Result<Vec<GithubTrendingRepoDto>, AppError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(GITHUB_TIMEOUT_SECS))
        .user_agent("ClaudePartner/0.5 GitHubTrending")
        .build()
        .map_err(|e| AppError::generic(format!("创建 GitHub 客户端失败: {e}")))?;
    let html = client
        .get(TRENDING_URL)
        .send()
        .await
        .map_err(|e| AppError::generic(format!("抓取 GitHub Trending 失败: {e}")))?
        .error_for_status()
        .map_err(|e| AppError::generic(format!("GitHub Trending 返回错误: {e}")))?
        .text()
        .await
        .map_err(|e| AppError::generic(format!("读取 GitHub Trending 响应失败: {e}")))?;

    let repos = parse_trending_html(&html);
    if repos.is_empty() {
        return Err(AppError::generic(
            "未能从 GitHub Trending 页面解析出项目列表",
        ));
    }
    Ok(repos.into_iter().take(TOP_LIMIT).collect())
}

/// 解析 GitHub Trending HTML。
fn parse_trending_html(html: &str) -> Vec<GithubTrendingRepoDto> {
    let document = Html::parse_document(html);
    let article_selector = Selector::parse("article.Box-row").expect("valid selector");
    let repo_link_selector = Selector::parse("h2 a").expect("valid selector");
    let description_selector = Selector::parse("p").expect("valid selector");
    let language_selector =
        Selector::parse(r#"span[itemprop="programmingLanguage"]"#).expect("valid selector");
    let stars_selector = Selector::parse(r#"a[href$="/stargazers"]"#).expect("valid selector");
    let forks_selector = Selector::parse(r#"a[href$="/forks"]"#).expect("valid selector");

    document
        .select(&article_selector)
        .filter_map(|article| {
            let link = article.select(&repo_link_selector).find_map(|a| {
                let href = a.value().attr("href")?;
                let mut parts = href.trim_start_matches('/').split('/');
                let owner = parts.next()?;
                let name = parts.next()?;
                if owner.is_empty() || name.is_empty() || parts.next().is_some() {
                    return None;
                }
                Some((href.to_string(), owner.to_string(), name.to_string()))
            })?;
            let rank = 0;
            let full_name = format!("{}/{}", link.1, link.2);
            let description = article
                .select(&description_selector)
                .next()
                .map(extract_text)
                .unwrap_or_default();
            let language = article.select(&language_selector).next().map(extract_text);
            let stars = article
                .select(&stars_selector)
                .next()
                .map(extract_text)
                .map(|s| parse_count(&s))
                .unwrap_or(0);
            let forks = article
                .select(&forks_selector)
                .next()
                .map(extract_text)
                .map(|s| parse_count(&s))
                .unwrap_or(0);
            let all_text = extract_text(article);
            let stars_this_week = all_text
                .split("Built by")
                .last()
                .map(parse_count)
                .unwrap_or(0);
            Some(GithubTrendingRepoDto {
                rank,
                owner: link.1,
                name: link.2,
                full_name,
                url: format!("https://github.com{}", link.0),
                description,
                language,
                stars,
                forks,
                stars_this_week,
                explanation_zh: String::new(),
                explanation_en: String::new(),
            })
        })
        .take(TOP_LIMIT)
        .enumerate()
        .map(|(index, mut repo)| {
            repo.rank = (index + 1) as u32;
            repo
        })
        .collect()
}

/// 提取节点文本并压缩空白。
fn extract_text(node: scraper::ElementRef<'_>) -> String {
    node.text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// 从包含逗号/文案的字符串里解析数字。
fn parse_count(text: &str) -> u64 {
    let digits = text
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect::<String>();
    digits.parse::<u64>().unwrap_or(0)
}

/// 调用 Claude Code CLI 批量生成双语解说。
async fn generate_explanations(
    config: &GithubTrendingConfig,
    repos: &[GithubTrendingRepoDto],
) -> Result<HashMap<String, AiRepoExplanation>, AppError> {
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["repos"],
        "properties": {
            "repos": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["fullName", "explanationZh", "explanationEn"],
                    "properties": {
                        "fullName": { "type": "string" },
                        "explanationZh": { "type": "string" },
                        "explanationEn": { "type": "string" }
                    }
                }
            }
        }
    });
    let input = repos
        .iter()
        .map(|r| {
            json!({
                "rank": r.rank,
                "fullName": r.full_name,
                "description": r.description,
                "language": r.language,
                "starsThisWeek": r.stars_this_week,
                "stars": r.stars,
                "forks": r.forks,
            })
        })
        .collect::<Vec<_>>();
    let prompt = format!(
        "You are writing concise bilingual explanations for a desktop app that shows GitHub weekly trending repositories.\n\
         Return only data matching the JSON schema.\n\
         For every input repo, preserve fullName exactly.\n\
         explanationZh: one useful Simplified Chinese sentence, 35-70 Chinese characters.\n\
         explanationEn: one useful English sentence, 18-32 words.\n\
         Explain what the project is and why it may be trending, based only on the provided metadata.\n\n\
         Input repos:\n{}",
        serde_json::to_string_pretty(&input)?
    );

    let parsed = claude_cli::run_structured_json::<AiOutput>(
        &config.claude_cli_path,
        &config.claude_model,
        &schema.to_string(),
        &prompt,
        CLAUDE_TIMEOUT_SECS,
        "生成解说",
    )
    .await?;

    Ok(ai_output_to_map(parsed))
}

/// 构造 Claude CLI 生成解说的参数列表。
///
/// Business Logic（为什么需要这个函数）:
///     GitHub Trending 解说是用户主动开启的本机 Claude CLI 能力，不应再人为设置单次预算上限，
///     避免长榜单生成时被预算参数中断。
///
/// Code Logic（这个函数做什么）:
///     根据配置返回 `claude --bare -p ...` 后续参数，保留模型和结构化输出约束，
///     但不包含 `--max-budget-usd`。
#[cfg(test)]
fn claude_generation_args(config: &GithubTrendingConfig, schema: &str) -> Vec<String> {
    claude_cli::build_pure_headless_args(&config.claude_model, schema)
}

/// 解析 Claude CLI 输出，兼容直接结构化 JSON 与 `--output-format json` 的 result 包装。
#[cfg(test)]
fn parse_ai_output(stdout: &str) -> Result<HashMap<String, AiRepoExplanation>, AppError> {
    let parsed = claude_cli::parse_structured_output::<AiOutput>(stdout)?;
    Ok(ai_output_to_map(parsed))
}

/// 将 Claude CLI 结构化输出按 fullName 建索引。
///
/// Business Logic（为什么需要这个函数）:
///     GitHub Trending 返回的是仓库列表，后续合并解说时需要按 fullName 快速找到对应条目。
///
/// Code Logic（这个函数做什么）:
///     跳过空 fullName 条目，其余条目放入 HashMap，后出现的同名条目覆盖前值。
fn ai_output_to_map(parsed: AiOutput) -> HashMap<String, AiRepoExplanation> {
    let mut map = HashMap::new();
    for item in parsed.repos {
        if item.full_name.trim().is_empty() {
            continue;
        }
        map.insert(item.full_name.clone(), item);
    }
    map
}

/// 从 Claude CLI 非零退出的输出中提取用户可读错误。
///
/// Business Logic（为什么需要这个函数）:
///     Claude CLI 的 `--output-format json` 在预算不足等失败场景会把错误 JSON 写到 stdout，
///     stderr 为空；只看 stderr 会让用户看不到真实原因。
///
/// Code Logic（这个函数做什么）:
///     优先返回 stderr；否则解析 stdout JSON 的 errors/result/subtype 字段；仍无可读内容时回退到
///     旧的“命令返回非零状态”兜底文案。
#[cfg(test)]
fn claude_failure_detail(stderr: &[u8], stdout: &[u8]) -> String {
    claude_cli::failure_detail(stderr, stdout)
}

/// 将 Claude 解说合并回仓库列表。
fn merge_explanations(
    mut repos: Vec<GithubTrendingRepoDto>,
    explanations: HashMap<String, AiRepoExplanation>,
) -> Vec<GithubTrendingRepoDto> {
    for repo in &mut repos {
        if let Some(item) = explanations.get(&repo.full_name) {
            repo.explanation_zh = item.explanation_zh.trim().to_string();
            repo.explanation_en = item.explanation_en.trim().to_string();
        }
    }
    repos
}

/// 运行 `claude --version` 测试 CLI。
async fn run_claude_version(config: &GithubTrendingConfig) -> ClaudeCliTestResult {
    let cli = claude_cli::normalize_cli_path(&config.claude_cli_path);
    let mut cmd = Command::new(cli);
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(
        Duration::from_secs(CLAUDE_VERSION_TIMEOUT_SECS),
        cmd.output(),
    )
    .await
    {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return ClaudeCliTestResult {
                ok: false,
                version: None,
                error: Some(format!("启动 Claude CLI 失败: {e}")),
            };
        }
        Err(_) => {
            return ClaudeCliTestResult {
                ok: false,
                version: None,
                error: Some(format!(
                    "Claude CLI 测试超时（{} 秒）",
                    CLAUDE_VERSION_TIMEOUT_SECS
                )),
            };
        }
    };
    if output.status.success() {
        ClaudeCliTestResult {
            ok: true,
            version: Some(String::from_utf8_lossy(&output.stdout).trim().to_string()),
            error: None,
        }
    } else {
        ClaudeCliTestResult {
            ok: false,
            version: None,
            error: Some(String::from_utf8_lossy(&output.stderr).trim().to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    const CACHE_SCHEMA: &str = "CREATE TABLE github_trending_cache (
        key TEXT PRIMARY KEY,
        payload TEXT NOT NULL,
        fetched_at TEXT NOT NULL,
        expires_at TEXT NOT NULL,
        ai_status TEXT NOT NULL,
        ai_error TEXT
    )";

    #[test]
    fn parses_trending_html_card_fields() {
        let html = r#"
        <div data-hpc>
          <article class="Box-row">
            <h2 class="h3 lh-condensed">
              <a href="/DeusData/codebase-memory-mcp">
                <span class="text-normal">DeusData /</span>
                codebase-memory-mcp
              </a>
            </h2>
            <p class="col-9 color-fg-muted my-1">
              High-performance code intelligence MCP server.
            </p>
            <div class="f6 color-fg-muted mt-2">
              <span><span itemprop="programmingLanguage">C</span></span>
              <a href="/DeusData/codebase-memory-mcp/stargazers">12,073</a>
              <a href="/DeusData/codebase-memory-mcp/forks">890</a>
              <span>Built by</span>
              <span>7,560 stars this week</span>
            </div>
          </article>
        </div>
        "#;
        let repos = parse_trending_html(html);
        assert_eq!(repos.len(), 1);
        let repo = &repos[0];
        assert_eq!(repo.rank, 1);
        assert_eq!(repo.full_name, "DeusData/codebase-memory-mcp");
        assert_eq!(repo.language.as_deref(), Some("C"));
        assert_eq!(repo.stars, 12_073);
        assert_eq!(repo.forks, 890);
        assert_eq!(repo.stars_this_week, 7_560);
    }

    #[test]
    fn parses_claude_direct_json_output() {
        let stdout = r#"{"repos":[{"fullName":"o/r","explanationZh":"中文解说","explanationEn":"English explanation."}]}"#;
        let map = parse_ai_output(stdout).expect("parse");
        assert_eq!(map["o/r"].explanation_zh, "中文解说");
        assert_eq!(map["o/r"].explanation_en, "English explanation.");
    }

    #[test]
    fn parses_claude_result_wrapped_json_output() {
        let stdout = r#"{"type":"result","result":"{\"repos\":[{\"fullName\":\"o/r\",\"explanationZh\":\"中文\",\"explanationEn\":\"English.\"}]}"}"#;
        let map = parse_ai_output(stdout).expect("parse");
        assert_eq!(map["o/r"].explanation_zh, "中文");
    }

    #[test]
    fn parses_claude_structured_output_field() {
        let stdout = r#"{"type":"result","result":"Generated explanations.","structured_output":{"repos":[{"fullName":"o/r","explanationZh":"结构化中文","explanationEn":"Structured English."}]}}"#;
        let map = parse_ai_output(stdout).expect("parse");
        assert_eq!(map["o/r"].explanation_zh, "结构化中文");
        assert_eq!(map["o/r"].explanation_en, "Structured English.");
    }

    #[test]
    fn extracts_claude_stdout_error_when_stderr_is_empty() {
        let stdout = r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"errors":["Reached maximum budget ($0.01)"]}"#;
        assert_eq!(
            claude_failure_detail(&[], stdout.as_bytes()),
            "Reached maximum budget ($0.01)"
        );
    }

    #[test]
    fn claude_generation_args_do_not_include_budget_limit() {
        let args = claude_generation_args(&GithubTrendingConfig::default(), "{}");

        assert!(!args.iter().any(|arg| arg == "--max-budget-usd"));
    }

    #[test]
    fn retries_legacy_generic_failed_ai_cache() {
        let payload = GithubTrendingPayload {
            repos: Vec::new(),
            fetched_at: Utc::now().to_rfc3339(),
            expires_at: (Utc::now() + ChronoDuration::hours(24)).to_rfc3339(),
            ai_status: "failed".to_string(),
            ai_error: Some("Claude CLI 生成解说失败: 命令返回非零状态".to_string()),
            ai_retry_attempted: false,
        };
        let config = GithubTrendingConfig::default();

        assert!(should_retry_failed_ai_cache(&payload, &config));
    }

    #[test]
    fn does_not_retry_failed_ai_cache_after_retry_attempted() {
        let mut payload = GithubTrendingPayload {
            repos: Vec::new(),
            fetched_at: Utc::now().to_rfc3339(),
            expires_at: (Utc::now() + ChronoDuration::hours(24)).to_rfc3339(),
            ai_status: "failed".to_string(),
            ai_error: Some("Claude CLI 生成解说失败: 命令返回非零状态".to_string()),
            ai_retry_attempted: false,
        };
        payload.ai_retry_attempted = true;
        let config = GithubTrendingConfig::default();

        assert!(!should_retry_failed_ai_cache(&payload, &config));
    }

    #[tokio::test]
    async fn cache_round_trip_same_key() {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("db");
        sqlx::query(CACHE_SCHEMA)
            .execute(&db)
            .await
            .expect("schema");
        let payload = GithubTrendingPayload {
            repos: vec![GithubTrendingRepoDto {
                rank: 1,
                owner: "owner".to_string(),
                name: "repo".to_string(),
                full_name: "owner/repo".to_string(),
                url: "https://github.com/owner/repo".to_string(),
                description: "desc".to_string(),
                language: Some("Rust".to_string()),
                stars: 10,
                forks: 2,
                stars_this_week: 3,
                explanation_zh: "中文".to_string(),
                explanation_en: "English.".to_string(),
            }],
            fetched_at: Utc::now().to_rfc3339(),
            expires_at: (Utc::now() + ChronoDuration::hours(24)).to_rfc3339(),
            ai_status: "ready".to_string(),
            ai_error: None,
            ai_retry_attempted: false,
        };
        store_cache(&db, "weekly:any:25:2026-06-23", &payload)
            .await
            .expect("store");
        let loaded = load_cache(&db, "weekly:any:25:2026-06-23")
            .await
            .expect("load")
            .expect("some");
        assert_eq!(loaded.repos[0].full_name, "owner/repo");
        assert_eq!(loaded.ai_status, "ready");
    }
}
