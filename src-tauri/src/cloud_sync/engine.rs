//! cloud_sync/engine.rs — 云端同步流程编排
//!
//! Business Logic（为什么需要这个模块）:
//!     把 git_cli + snapshot 拼成完整的同步流程：detect_git → ensure_repo（clone/复用工作区）
//!     → 定分支 → reconcile CLAUDE.md → fetch → reset --hard → import(merge 进本地) →
//!     export(写回工作区) → commit → push。push 被拒（多设备并发）时 fetch+reset+import+
//!     export+commit+push 再来一轮（最多 1 次重试 = 总共 2 轮）即可收敛。
//!     本地 SQLite + 向量时钟是权威源，git 只做传输，冲突解决完全复用 merge_*。
//!
//! Code Logic（这个模块做什么）:
//!     - `trigger_cloud_sync`：完整同步，返回 CloudSyncResult（pulled/pushed/note）。
//!     - `test_connection`：探测 git + 远端连通，返回 gitVersion/defaultBranch/error。
//!     - `ensure_repo`：确保工作区存在（首次 clone + 设身份），解析同步分支。
//!     - `cloud_sync_workdir`：工作区路径 `~/.claude-partner/cloud-sync/`。

use crate::cloud_sync::git_cli::{self, PushError};
use crate::cloud_sync::snapshot::{export_from_db, import_to_db, ExportStats, ImportStats};
use crate::config::config_dir;
use crate::error::AppError;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 同步结果（返回前端，camelCase 对齐锁定契约）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudSyncResult {
    /// 整体是否成功。
    pub ok: bool,
    /// 本次 import 实际落库的条数总和（prompts + cc 历史 + ssh 目标；CLAUDE.md 计 0 或 1）。
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

/// 返回云端同步工作区路径：`~/.claude-partner/cloud-sync/`。
///
/// Business Logic: cloud_sync 的 git 工作区集中放在应用数据根下，便于清理与定位。
/// Code Logic: 复用 config::config_dir()（与配置/数据库同根），追加 "cloud-sync"。
pub fn cloud_sync_workdir() -> PathBuf {
    config_dir().join("cloud-sync")
}

/// 触发一次完整的云端同步（手动按钮 / scheduler 均调此入口）。
///
/// Business Logic: 把 pull→import→export→commit→push 完整跑一遍。任一步骤失败返回
///     ok:false + 友好中文 note，绝不 panic。push 被拒时按"再 fetch+reset+import+
///     export+commit+push 一轮"收敛（总共最多 2 轮）。
///
/// Code Logic: 流程见模块顶部说明。pulled = 各轮 import 条数总和，pushed = 最后一次
///     export 的条数（export 是覆盖式，以最后一轮为准）。
pub async fn trigger_cloud_sync(state: &AppState) -> CloudSyncResult {
    let now = chrono::Utc::now().to_rfc3339();
    let ok_note = |pulled: u64, pushed: u64, cm: bool| {
        let mut parts: Vec<String> = Vec::new();
        parts.push(format!("拉取更新 {pulled} 条"));
        parts.push(format!("推送 {pushed} 条"));
        if cm {
            parts.push("CLAUDE.md 已同步".to_string());
        }
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

    // 2. 确保工作区就绪 + 定分支
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
        // 3. CLAUDE.md 文件↔DB 对账（纳入应用外编辑，import 内部会再做一次，这里保险）
        //    注：import_to_db 内部已含 reconcile_from_file，无需重复。

        // 4. fetch origin（首轮空仓库可能无 origin 引用，容错跳过）
        if attempt > 0 || has_remote_branch(&git, &workdir).await {
            if let Err(e) = git_cli::fetch_origin(&git, &workdir).await {
                // 首轮 fetch 失败（如全新空仓库无远端内容）容错继续；重试轮失败则记录
                if attempt > 0 {
                    tracing::warn!("cloud_sync: fetch 失败（继续尝试）: {e}");
                }
            }
        }

        // 5. reset --hard origin/<branch>（远端有分支时）
        if has_remote_branch(&git, &workdir).await {
            if let Err(e) = git_cli::reset_hard(&git, &workdir, &branch).await {
                tracing::warn!("cloud_sync: reset --hard 失败（继续）: {e}");
            }
        }

        // 6. import（远端 → 本地 merge）
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
        total_pulled += import_stats.prompts + import_stats.cc_history + import_stats.ssh_targets;
        if import_stats.claude_md_updated {
            total_pulled += 1;
        }
        tracing::info!(
            "cloud_sync: import 完成 prompts={} cc={} ssh={} claude_md_updated={}",
            import_stats.prompts,
            import_stats.cc_history,
            import_stats.ssh_targets,
            import_stats.claude_md_updated
        );

        // 7. export（本地权威 → 工作区）
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
            "cloud_sync: export 完成 prompts={} cc={} ssh={} claude_md={}",
            last_export.prompts,
            last_export.cc_history,
            last_export.ssh_targets,
            last_export.claude_md
        );

        // 8. commit（message 带设备 ID + 时间戳，便于多设备同步审计与回滚定位；无变化则跳过 push）
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
                note: ok_note(total_pulled, pushed, last_export.claude_md),
                synced_at: chrono::Utc::now().to_rfc3339(),
            };
        }

        // 9. push
        match git_cli::push(&git, &workdir, &branch).await {
            Ok(()) => {
                tracing::info!("cloud_sync: push 成功");
                let pushed = last_export.total();
                return CloudSyncResult {
                    ok: true,
                    pulled: total_pulled,
                    pushed,
                    note: ok_note(total_pulled, pushed, last_export.claude_md),
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
        pushed: last_export.prompts + last_export.cc_history,
        note: "同步未完成（未知原因）".to_string(),
        synced_at: chrono::Utc::now().to_rfc3339(),
    }
}

/// 测试云端同步连通性：探测 git 版本 + 远端默认分支。
///
/// Business Logic: 前端设置页"测试连接"按钮调用，让用户确认 git 可用、仓库可达、
///     拿到默认分支名供展示。不产生任何 commit/push 副作用。
/// Code Logic: detect_git → git_version；若已配 repo_url 且工作区已存在 → fetch 测连通 +
///     default_remote_branch；若配了 url 但无工作区 → clone 到临时目录测 + 解析默认分支。
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
    // 工作区已存在：fetch 测连通 + 解析默认分支
    if workdir.is_dir() && workdir.join(".git").exists() {
        match git_cli::fetch_origin(&git, &workdir).await {
            Ok(()) => {}
            Err(e) => {
                return TestCloudSyncResult {
                    ok: false,
                    git_version: Some(git_version),
                    default_branch: None,
                    error: Some(format!("fetch 远端失败: {e}")),
                };
            }
        }
        let branch = git_cli::default_remote_branch(&git, &workdir)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("cloud_sync test: 解析默认分支失败: {e}");
                "main".to_string()
            });
        return TestCloudSyncResult {
            ok: true,
            git_version: Some(git_version),
            default_branch: Some(branch),
            error: None,
        };
    }

    // 无工作区：clone 到临时目录测连通（测完删除）
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
