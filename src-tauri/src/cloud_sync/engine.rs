//! cloud_sync/engine.rs — 云端同步流程编排
//!
//! Business Logic（为什么需要这个模块）:
//!     把 git_cli + snapshot 拼成完整的同步流程：detect_git → ensure_repo（clone/复用工作区）
//!     → 定分支 → fetch → reset --hard → import(merge 进本地) → export(写回工作区)
//!     → commit → push。push 被拒（多设备并发）时 fetch+reset+import+
//!     export+commit+push 再来一轮（最多 1 次重试 = 总共 2 轮）即可收敛。
//!     本地 SQLite + 向量时钟是权威源，git 只做传输，冲突解决完全复用 merge_*。
//!     CLAUDE.md 不参与云端自动同步，只由 CLAUDE.md 页面用户主动推送。
//!     所有正式工作区写流程经 CloudSyncRuntime 单飞，避免并发 reset/export 踩踏。
//!
//! Code Logic（这个模块做什么）:
//!     - `trigger_cloud_sync`：完整同步（经 exclusive gate），返回 CloudSyncResult。
//!     - `trigger_cloud_sync_with`：指定 trigger/policy 的入口（scheduler/manual 共用）。
//!     - `test_connection`：探测 git + 远端连通；复用正式 workdir 时取 gate。
//!     - `ensure_repo`：确保工作区存在（首次 clone + 设身份），解析同步分支。
//!     - `cloud_sync_workdir`：工作区路径 `~/.cc-partner/cloud-sync/`。

use crate::cloud_sync::git_cli::{self, PushError};
use crate::cloud_sync::runtime::{
    run_cloud_sync_exclusive, wait_policy, CloudSyncBusyPolicy, CloudSyncTrigger,
};
use crate::cloud_sync::snapshot::{export_from_db, import_to_db, ExportStats, ImportStats};
use crate::config::config_dir;
use crate::error::AppError;
use crate::models::claude_md::ClaudeMdRow;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// GitHub 工作区中 CLAUDE.md 单例目录名（仅用于用户主动推送，不参与 cloud auto sync）。
const CLAUDE_MD_DIR: &str = "claude_md";
/// GitHub 工作区中 CLAUDE.md 单例文件名。
const CLAUDE_MD_FILE: &str = "claude_md.json";
/// Git pathspec：只提交 CLAUDE.md 这一个云端快照文件。
const CLAUDE_MD_PATHSPEC: &str = "claude_md/claude_md.json";

/// 同步结果（返回前端，camelCase 对齐锁定契约）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudSyncResult {
    /// 整体是否成功。
    pub ok: bool,
    /// 本次 import 实际落库的条数总和（prompts + cc 历史 + ssh 目标）。
    pub pulled: u64,
    /// 本次 export 写出的文件数总和。
    pub pushed: u64,
    /// 友好中文说明（成功时给摘要，失败时给错误）。
    pub note: String,
    /// 同步完成时间（RFC3339）。
    pub synced_at: String,
}

/// 测试连通结果（返回前端，camelCase 对齐锁定契约）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestCloudSyncResult {
    /// 是否成功探测到 git 与远端连通。
    pub ok: bool,
    /// git 版本字符串（成功时回填）。
    pub git_version: Option<String>,
    /// 远端默认分支名（成功时回填）。
    pub default_branch: Option<String>,
    /// 失败原因（成功时为 None）。
    pub error: Option<String>,
}

/// CLAUDE.md 主动推送到 GitHub 的结果（内部用于合并前端提示）。
#[derive(Debug, Clone)]
pub struct CloudClaudeMdPushResult {
    /// 人类可读说明。
    pub note: String,
}

/// 返回云端同步工作区路径：`~/.cc-partner/cloud-sync/`。
///
/// Business Logic: cloud_sync 的 git 工作区集中放在应用数据根下，便于清理与定位。
/// Code Logic: 复用 config::config_dir()（与配置/数据库同根），追加 "cloud-sync"。
pub fn cloud_sync_workdir() -> PathBuf {
    config_dir()
        .expect("无法解析应用数据目录（检查 CC_PARTNER_DATA_DIR）")
        .join("cloud-sync")
}

/// 将本机 CLAUDE.md 版本主动推送到 GitHub 云端。
///
/// Business Logic: CLAUDE.md 不参与 cloud auto sync；用户在 CLAUDE.md 页面点击推送时，
///     GitHub 云端也必须被更新为触发设备的版本。这里不 import/merge 远端 CLAUDE.md，
///     只在远端最新工作树上覆盖写入本机 CLAUDE.md 快照并 push。
///     与完整 sync 共享 CloudSyncRuntime 门闸，避免并发写同一工作区。
///
/// Code Logic: 未配置 repo_url 则跳过（无需取锁）；否则经
///     `run_cloud_sync_exclusive(ClaudeMdPush, Wait{300s})` 后：detect_git → ensure_repo
///     （获锁后重读 config）→ fetch/reset → 写快照 → commit pathspec → push（可重试一轮）。
pub async fn push_claude_md_to_cloud(
    state: &AppState,
    row: &ClaudeMdRow,
) -> Result<CloudClaudeMdPushResult, AppError> {
    let repo_configured = {
        let cfg = state.config.read().unwrap();
        cfg.cloud_sync_repo_url
            .as_ref()
            .is_some_and(|url| !url.trim().is_empty())
    };
    if !repo_configured {
        return Ok(CloudClaudeMdPushResult {
            note: "GitHub 云端未配置，已跳过".to_string(),
        });
    }

    let outcome = run_cloud_sync_exclusive(
        &state.cloud_sync_runtime,
        CloudSyncTrigger::ClaudeMdPush,
        wait_policy(),
        || async {
            push_claude_md_to_cloud_locked(state, row).await
        },
    )
    .await?;

    // Wait 策略下 outcome 必为 Some；Timeout 已转 Err
    outcome.ok_or_else(|| AppError::generic("CLAUDE.md 推送到 GitHub 未完成（门闸异常）"))
}

/// 已持 CloudSyncRuntime gate 时执行 CLAUDE.md 云端推送。
///
/// Business Logic: 门闸外层保证单飞；本函数专注 Git 工作区写路径。
/// Code Logic: detect_git → ensure_repo（重读 config）→ fetch/reset → 写快照 →
///     commit pathspec → push；Rejected 重试一轮。
async fn push_claude_md_to_cloud_locked(
    state: &AppState,
    row: &ClaudeMdRow,
) -> Result<CloudClaudeMdPushResult, AppError> {
    let git = git_cli::detect_git()?;
    // 获锁后重读 repo URL/branch（ensure_repo 内部读 config）
    let (workdir, branch) = ensure_repo(state, &git).await?;

    for attempt in 0..2u8 {
        if attempt > 0 || has_remote_branch(&git, &workdir).await {
            git_cli::fetch_origin(&git, &workdir).await?;
        }
        if has_remote_branch(&git, &workdir).await {
            git_cli::reset_hard(&git, &workdir, &branch).await?;
        }

        write_claude_md_snapshot(&workdir, row)?;

        let commit_msg = format!(
            "push CLAUDE.md from {} @ {}",
            state.device_id.as_str(),
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
        );
        let committed =
            git_cli::commit_path(&git, &workdir, CLAUDE_MD_PATHSPEC, &commit_msg).await?;
        if !committed {
            return Ok(CloudClaudeMdPushResult {
                note: "GitHub 云端 CLAUDE.md 已是最新".to_string(),
            });
        }

        match git_cli::push(&git, &workdir, &branch).await {
            Ok(()) => {
                return Ok(CloudClaudeMdPushResult {
                    note: "GitHub 云端 CLAUDE.md 已推送".to_string(),
                });
            }
            Err(PushError::Rejected) if attempt == 0 => {
                tracing::warn!("CLAUDE.md GitHub 推送被远端拒绝，fetch/reset 后重试一轮");
                continue;
            }
            Err(PushError::Rejected) => {
                return Err(AppError::generic(
                    "CLAUDE.md 推送到 GitHub 被远端拒绝，重试后仍未成功，请稍后再试",
                ));
            }
            Err(PushError::Other(e)) => return Err(e),
        }
    }

    Err(AppError::generic("CLAUDE.md 推送到 GitHub 未完成"))
}

/// 写出 CLAUDE.md GitHub 快照文件。
///
/// Business Logic: 用户主动推送时，云端保存的是触发设备当前 DB Row（含 vector_clock 元数据），
///     供其它设备或后续审计看到完整版本信息。
/// Code Logic: 确保 claude_md/ 存在，pretty JSON 写 claude_md.json。
fn write_claude_md_snapshot(workdir: &Path, row: &ClaudeMdRow) -> Result<(), AppError> {
    let dir = workdir.join(CLAUDE_MD_DIR);
    fs::create_dir_all(&dir)?;
    let path = dir.join(CLAUDE_MD_FILE);
    let text = serde_json::to_string_pretty(row)?;
    fs::write(path, text)?;
    Ok(())
}

/// 触发一次完整的云端同步（默认手动策略 Wait 300s）。
///
/// Business Logic: 前端「立即同步」按钮调用；与 scheduler 共享同一 gate。
/// Code Logic: 委托 `trigger_cloud_sync_with(Manual, Wait{300s})`。
pub async fn trigger_cloud_sync(state: &AppState) -> CloudSyncResult {
    trigger_cloud_sync_with(state, CloudSyncTrigger::Manual, wait_policy()).await
}

/// 按指定 trigger/policy 触发完整云端同步。
///
/// Business Logic: 手动 Wait；scheduler 用 ReturnBusy 忙则跳过本 tick。
///     获锁后重读 config（ensure_repo），覆盖 ensure→push 全流程。
///
/// Code Logic: `run_cloud_sync_exclusive` → 内层 `trigger_cloud_sync_locked`。
///     ReturnBusy 返回 ok=true note 标明跳过；Timeout 返回 ok=false。
pub async fn trigger_cloud_sync_with(
    state: &AppState,
    trigger: CloudSyncTrigger,
    policy: CloudSyncBusyPolicy,
) -> CloudSyncResult {
    let now = chrono::Utc::now().to_rfc3339();
    match run_cloud_sync_exclusive(
        &state.cloud_sync_runtime,
        trigger,
        policy,
        || async { Ok(trigger_cloud_sync_locked(state).await) },
    )
    .await
    {
        Ok(Some(result)) => result,
        Ok(None) => CloudSyncResult {
            ok: true,
            pulled: 0,
            pushed: 0,
            note: "云端同步繁忙，本轮已跳过".to_string(),
            synced_at: now,
        },
        Err(e) => CloudSyncResult {
            ok: false,
            pulled: 0,
            pushed: 0,
            note: e.to_string(),
            synced_at: now,
        },
    }
}

/// 已持 CloudSyncRuntime gate 时执行完整同步。
///
/// Business Logic: 本地 SQLite 是权威源，git 只做传输。
/// Code Logic: detect_git → ensure_repo（重读 config）→ 最多两轮
///     fetch/reset/import/export/commit/push；Rejected 重试一轮。
async fn trigger_cloud_sync_locked(state: &AppState) -> CloudSyncResult {
    let now = chrono::Utc::now().to_rfc3339();
    let ok_note = |pulled: u64, pushed: u64| {
        let mut parts: Vec<String> = Vec::new();
        parts.push(format!("拉取更新 {pulled} 条"));
        parts.push(format!("推送 {pushed} 条"));
        format!("同步成功：{}", parts.join("，"))
    };

    // 1. 探测 git
    let git = match git_cli::detect_git() {
        Ok(g) => g,
        Err(e) => {
            return CloudSyncResult {
                ok: false,
                pulled: 0,
                pushed: 0,
                note: e.to_string(),
                synced_at: now,
            };
        }
    };

    // 2. 确保工作区就绪 + 定分支（获锁后重读 repo URL/branch）
    let (workdir, branch) = match ensure_repo(state, &git).await {
        Ok(v) => v,
        Err(e) => {
            return CloudSyncResult {
                ok: false,
                pulled: 0,
                pushed: 0,
                note: format!("准备工作区失败: {e}"),
                synced_at: now,
            };
        }
    };

    let mut total_pulled: u64 = 0;
    let mut last_export: ExportStats = ExportStats::default();

    // 最多两轮（首轮 + 1 次重试收敛）
    for attempt in 0..2u8 {
        // 3. fetch origin（首轮空仓库可能无 origin 引用，容错跳过）
        if attempt > 0 || has_remote_branch(&git, &workdir).await {
            if let Err(e) = git_cli::fetch_origin(&git, &workdir).await {
                // 首轮 fetch 失败（如全新空仓库无远端内容）容错继续；重试轮失败则记录
                if attempt > 0 {
                    tracing::warn!("cloud_sync: fetch 失败（继续尝试）: {e}");
                }
            }
        }

        // 4. reset --hard origin/<branch>（远端有分支时）
        if has_remote_branch(&git, &workdir).await {
            if let Err(e) = git_cli::reset_hard(&git, &workdir, &branch).await {
                tracing::warn!("cloud_sync: reset --hard 失败（继续）: {e}");
            }
        }

        // 5. import（远端 → 本地 merge）
        let import_stats: ImportStats = match import_to_db(state, &workdir).await {
            Ok(s) => s,
            Err(e) => {
                return CloudSyncResult {
                    ok: false,
                    pulled: total_pulled,
                    pushed: 0,
                    note: format!("导入工作区数据失败: {e}"),
                    synced_at: chrono::Utc::now().to_rfc3339(),
                };
            }
        };
        total_pulled += import_stats.total();
        tracing::info!(
            "cloud_sync: import 完成 prompts={} cc={} ssh={} scratchpad={}",
            import_stats.prompts,
            import_stats.cc_history,
            import_stats.ssh_targets,
            import_stats.scratchpad
        );

        // 6. export（本地权威 → 工作区）
        last_export = match export_from_db(state, &workdir).await {
            Ok(s) => s,
            Err(e) => {
                return CloudSyncResult {
                    ok: false,
                    pulled: total_pulled,
                    pushed: 0,
                    note: format!("导出数据到工作区失败: {e}"),
                    synced_at: chrono::Utc::now().to_rfc3339(),
                };
            }
        };
        tracing::info!(
            "cloud_sync: export 完成 prompts={} cc={} ssh={} scratchpad={}",
            last_export.prompts,
            last_export.cc_history,
            last_export.ssh_targets,
            last_export.scratchpad
        );

        // 7. commit（message 带设备 ID + 时间戳，便于多设备同步审计与回滚定位；无变化则跳过 push）
        let commit_msg = format!(
            "cloud sync from {} @ {}",
            state.device_id.as_str(),
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
        );
        let committed = match git_cli::commit_all(&git, &workdir, &commit_msg).await {
            Ok(c) => c,
            Err(e) => {
                return CloudSyncResult {
                    ok: false,
                    pulled: total_pulled,
                    pushed: last_export.total(),
                    note: format!("提交工作区失败: {e}"),
                    synced_at: chrono::Utc::now().to_rfc3339(),
                };
            }
        };

        if !committed {
            // 无本地改动 → 无需 push，视为成功（pull 已吸收远端变化）
            tracing::info!("cloud_sync: 无本地改动，跳过 push");
            let pushed = last_export.total();
            return CloudSyncResult {
                ok: true,
                pulled: total_pulled,
                pushed,
                note: ok_note(total_pulled, pushed),
                synced_at: chrono::Utc::now().to_rfc3339(),
            };
        }

        // 8. push
        match git_cli::push(&git, &workdir, &branch).await {
            Ok(()) => {
                tracing::info!("cloud_sync: push 成功");
                let pushed = last_export.total();
                return CloudSyncResult {
                    ok: true,
                    pulled: total_pulled,
                    pushed,
                    note: ok_note(total_pulled, pushed),
                    synced_at: chrono::Utc::now().to_rfc3339(),
                };
            }
            Err(PushError::Rejected) => {
                if attempt == 0 {
                    tracing::warn!("cloud_sync: push 被远端拒绝，fetch 后重试一轮");
                    continue;
                }
                return CloudSyncResult {
                    ok: false,
                    pulled: total_pulled,
                    pushed: last_export.total(),
                    note: "推送被远端拒绝（其他设备刚更新），重试后仍未成功，请稍后再试"
                        .to_string(),
                    synced_at: chrono::Utc::now().to_rfc3339(),
                };
            }
            Err(PushError::Other(e)) => {
                return CloudSyncResult {
                    ok: false,
                    pulled: total_pulled,
                    pushed: last_export.total(),
                    note: format!("推送失败: {e}"),
                    synced_at: chrono::Utc::now().to_rfc3339(),
                };
            }
        }
    }

    // 理论上不可达（循环内必返回）
    CloudSyncResult {
        ok: false,
        pulled: total_pulled,
        pushed: last_export.total(),
        note: "同步未完成（未知原因）".to_string(),
        synced_at: chrono::Utc::now().to_rfc3339(),
    }
}

/// 测试云端同步连通性：探测 git 版本 + 远端默认分支。
///
/// Business Logic: 前端设置页"测试连接"按钮调用，让用户确认 git 可用、仓库可达、
///     拿到默认分支名供展示。不产生任何 commit/push 副作用。
///     使用独立临时目录测连通时无需门闸；复用正式 workdir 做 fetch 时必须取 gate。
/// Code Logic: detect_git → git_version；若已配 repo_url 且工作区已存在 →
///     `run_cloud_sync_exclusive(Manual, Wait)` 后 fetch + default_remote_branch；
///     若配了 url 但无工作区 → clone 到临时目录测 + 解析默认分支（不取锁）。
pub async fn test_connection(state: &AppState) -> TestCloudSyncResult {
    // 1. 探测 git + 版本
    let git = match git_cli::detect_git() {
        Ok(g) => g,
        Err(e) => {
            return TestCloudSyncResult {
                ok: false,
                git_version: None,
                default_branch: None,
                error: Some(e.to_string()),
            };
        }
    };
    let git_version = match git_cli::git_version(&git).await {
        Ok(v) => v,
        Err(e) => {
            return TestCloudSyncResult {
                ok: false,
                git_version: None,
                default_branch: None,
                error: Some(format!("获取 git 版本失败: {e}")),
            };
        }
    };

    let repo_url = {
        let cfg = state.config.read().unwrap();
        cfg.cloud_sync_repo_url.clone()
    };

    // 未配仓库 URL：仅返回 git 可用（git_version），无远端可测
    let Some(url) = repo_url else {
        return TestCloudSyncResult {
            ok: true,
            git_version: Some(git_version),
            default_branch: None,
            error: Some("尚未配置云端仓库 URL（仅验证了 git 可用）".to_string()),
        };
    };
    if url.trim().is_empty() {
        return TestCloudSyncResult {
            ok: true,
            git_version: Some(git_version),
            default_branch: None,
            error: Some("尚未配置云端仓库 URL（仅验证了 git 可用）".to_string()),
        };
    }

    let workdir = cloud_sync_workdir();
    // 工作区已存在：必须取 gate 再 fetch（正式 workdir）
    if workdir.is_dir() && workdir.join(".git").exists() {
        let git_version_for_gate = git_version.clone();
        let gate_result = run_cloud_sync_exclusive(
            &state.cloud_sync_runtime,
            CloudSyncTrigger::Manual,
            wait_policy(),
            || {
                let git = git.clone();
                let workdir = workdir.clone();
                let git_version = git_version_for_gate.clone();
                async move {
                    match git_cli::fetch_origin(&git, &workdir).await {
                        Ok(()) => {}
                        Err(e) => {
                            return Ok(TestCloudSyncResult {
                                ok: false,
                                git_version: Some(git_version),
                                default_branch: None,
                                error: Some(format!("fetch 远端失败: {e}")),
                            });
                        }
                    }
                    let branch = git_cli::default_remote_branch(&git, &workdir)
                        .await
                        .unwrap_or_else(|e| {
                            tracing::warn!("cloud_sync test: 解析默认分支失败: {e}");
                            "main".to_string()
                        });
                    Ok(TestCloudSyncResult {
                        ok: true,
                        git_version: Some(git_version),
                        default_branch: Some(branch),
                        error: None,
                    })
                }
            },
        )
        .await;
        return match gate_result {
            Ok(Some(r)) => r,
            Ok(None) => TestCloudSyncResult {
                ok: false,
                git_version: Some(git_version),
                default_branch: None,
                error: Some("云端同步繁忙，测试连接已跳过".to_string()),
            },
            Err(e) => TestCloudSyncResult {
                ok: false,
                git_version: Some(git_version),
                default_branch: None,
                error: Some(e.to_string()),
            },
        };
    }

    // 无工作区：clone 到临时目录测连通（测完删除；独立 temp 无需锁）
    let tmp = std::env::temp_dir().join(format!("cp-cloud-sync-test-{}", uuid_str()));
    let clone_res = git_cli::clone(&git, &url, &tmp).await;
    let result = match clone_res {
        Ok(()) => {
            let _ = git_cli::set_local_identity(&git, &tmp).await;
            let branch = git_cli::default_remote_branch(&git, &tmp)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!("cloud_sync test: 解析默认分支失败: {e}");
                    "main".to_string()
                });
            TestCloudSyncResult {
                ok: true,
                git_version: Some(git_version),
                default_branch: Some(branch),
                error: None,
            }
        }
        Err(e) => TestCloudSyncResult {
            ok: false,
            git_version: Some(git_version),
            default_branch: None,
            error: Some(format!("clone 仓库失败（请检查 URL 与认证）: {e}")),
        },
    };
    // 清理临时目录（失败不阻断返回）
    if tmp.exists() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    result
}

/// 确保同步工作区就绪并返回 (workdir, branch)。
///
/// Business Logic: 首次同步需 clone 远端到工作区；后续复用。分支优先用 config 显式配置，
///     否则用远端默认分支，再否则用当前 HEAD 分支。未配 repo_url 时报错（无法 clone）。
///
/// Code Logic:
/// 1. workdir = cloud_sync_workdir()；
/// 2. 不存在且配了 url → clone + set_local_identity；
/// 3. 存在 → 复用；
/// 4. branch：config.cloud_sync_branch > default_remote_branch > current_branch；
///    全都拿不到则回退 "main"。
async fn ensure_repo(state: &AppState, git: &Path) -> Result<(PathBuf, String), AppError> {
    let workdir = cloud_sync_workdir();
    let (repo_url, configured_branch) = {
        let cfg = state.config.read().unwrap();
        (
            cfg.cloud_sync_repo_url.clone(),
            cfg.cloud_sync_branch.clone(),
        )
    };

    let repo_url = repo_url
        .ok_or_else(|| AppError::generic("未配置云端同步仓库 URL，请在设置页填写后再同步"))?;
    if repo_url.trim().is_empty() {
        return Err(AppError::generic(
            "云端同步仓库 URL 为空，请在设置页填写后再同步",
        ));
    }

    if !workdir.is_dir() || !workdir.join(".git").exists() {
        // 首次：clone（若残留非 git 目录，先清理避免 clone 到非空目录失败）
        if workdir.exists() {
            let _ = std::fs::remove_dir_all(&workdir);
        }
        if let Some(parent) = workdir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        git_cli::clone(git, &repo_url, &workdir).await?;
        git_cli::set_local_identity(git, &workdir).await?;
    }

    // 解析分支
    let branch = if let Some(b) = configured_branch {
        b
    } else {
        match git_cli::default_remote_branch(git, &workdir).await {
            Ok(b) => b,
            Err(_) => {
                // 远端默认分支解析失败时尝试本地当前分支，再回退 "main"
                local_current_branch(git, &workdir).unwrap_or_else(|| "main".to_string())
            }
        }
    };

    Ok((workdir, branch))
}

/// 同步取当前 HEAD 分支名（兜底，ensure_repo 内 default_remote_branch 失败时用）。
///
/// Business Logic: 全新 clone 的空仓库 origin/HEAD 可能未设置，default_remote_branch 会失败，
///     此时退而求其次取本地当前分支名。
/// Code Logic: std::process::Command 跑 `git symbolic-ref --short HEAD`，成功返回分支名。
fn local_current_branch(git: &Path, workdir: &Path) -> Option<String> {
    let out = std::process::Command::new(git)
        .current_dir(workdir)
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
        .ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    } else {
        None
    }
}

/// 判断工作区是否有 origin 的远端分支引用（决定是否需 fetch/reset）。
///
/// Business Logic: 全新空仓库 clone 下来时 origin/HEAD 可能尚未建立，此时 fetch/reset
///     会失败，需识别为 false 容错跳过。
/// Code Logic: `git rev-parse --verify origin/HEAD` 成功 → true；失败 → false。
async fn has_remote_branch(git: &Path, workdir: &Path) -> bool {
    git_cli::run(
        git,
        workdir,
        &["rev-parse", "--verify", "origin/HEAD"],
        std::time::Duration::from_secs(30),
    )
    .await
    .is_ok()
}

/// 生成一个临时 uuid 字符串（用于临时 clone 目录名，避免并发冲突）。
fn uuid_str() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_sync::runtime::{
        run_cloud_sync_exclusive, wait_policy, CloudSyncRuntime, CloudSyncTrigger,
    };
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// 记录一次工作区写步骤（模拟 reset/write）。
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum WorkStep {
        Begin(String),
        Reset(String),
        Write(String),
        End(String),
    }

    /// Business Logic: 手动 sync 与 CLAUDE.md push 不得在同一 workdir 交错 reset/write。
    /// Code Logic: 两任务经同一 CloudSyncRuntime Wait 门闸，记录步骤序列，断言无交错。
    #[tokio::test]
    async fn writers_do_not_overlap() {
        let runtime = Arc::new(CloudSyncRuntime::new());
        let log: Arc<Mutex<Vec<WorkStep>>> = Arc::new(Mutex::new(Vec::new()));
        let first_entered = Arc::new(tokio::sync::Notify::new());

        let run_flow = |name: &'static str,
                        trigger: CloudSyncTrigger,
                        rt: Arc<CloudSyncRuntime>,
                        log: Arc<Mutex<Vec<WorkStep>>>,
                        notify_enter: Option<Arc<tokio::sync::Notify>>| async move {
            run_cloud_sync_exclusive(&rt, trigger, wait_policy(), || {
                let log = log.clone();
                let notify_enter = notify_enter.clone();
                async move {
                    {
                        let mut g = log.lock().unwrap();
                        g.push(WorkStep::Begin(name.into()));
                    }
                    if let Some(n) = notify_enter {
                        n.notify_one();
                    }
                    // 模拟 reset
                    {
                        let mut g = log.lock().unwrap();
                        g.push(WorkStep::Reset(name.into()));
                    }
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    // 模拟 write
                    {
                        let mut g = log.lock().unwrap();
                        g.push(WorkStep::Write(name.into()));
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    {
                        let mut g = log.lock().unwrap();
                        g.push(WorkStep::End(name.into()));
                    }
                    Ok::<(), AppError>(())
                }
            })
            .await
        };

        let rt1 = runtime.clone();
        let log1 = log.clone();
        let n1 = first_entered.clone();
        let t1 = tokio::spawn(async move {
            run_flow(
                "manual",
                CloudSyncTrigger::Manual,
                rt1,
                log1,
                Some(n1),
            )
            .await
        });

        first_entered.notified().await;

        let rt2 = runtime.clone();
        let log2 = log.clone();
        let t2 = tokio::spawn(async move {
            run_flow(
                "claude_md",
                CloudSyncTrigger::ClaudeMdPush,
                rt2,
                log2,
                None,
            )
            .await
        });

        let (r1, r2) = tokio::join!(t1, t2);
        assert!(r1.unwrap().unwrap().is_some());
        assert!(r2.unwrap().unwrap().is_some());

        let steps = log.lock().unwrap().clone();
        // 必须整段完成后再开始下一段：Begin..End 成对不交错
        assert_eq!(
            steps,
            vec![
                WorkStep::Begin("manual".into()),
                WorkStep::Reset("manual".into()),
                WorkStep::Write("manual".into()),
                WorkStep::End("manual".into()),
                WorkStep::Begin("claude_md".into()),
                WorkStep::Reset("claude_md".into()),
                WorkStep::Write("claude_md".into()),
                WorkStep::End("claude_md".into()),
            ],
            "manual 与 claude_md 写流程不得交错: {steps:?}"
        );
    }
}
