//! user_mirror/store_migration — 镜像前把 user-scope Skill/Command 收编进 portable-store
//!
//! Business Logic（为什么需要这个模块）:
//!     用户级镜像要求源端 Skill/Command 以本机 portable-store 真树为单一版本事实；
//!     「仓库真树 + Agent 根软链」直接 dereference 打包会让对端拿到无版本锚点的副本，
//!     本机也会长期保留逃逸形态。产品决策：镜像发送前先「确认版本并迁移进 store」，
//!     再 freeze 发送；断链保持 blocked 由只读 dereference 打包兜底。
//!
//! Code Logic（这个模块做什么）:
//!     强制重扫 user-scope portable inventory，对每条 (target, kind, native_id) 观测：
//!     `classify_store_link` 分 StoreLink（跳过）/ 可解析 EscapeLink（`MaterializeEscapeLink`
//!     复制真树进 store 并换链）/ Regular 真树（`MigrateToStore` move 进 store 并换链）；
//!     断链记 failed 跳过。单条失败不阻断整体；Plugin/MCP 及 plugin 组件完全不动。

use crate::agent_hub::models::ScopeKind;
use crate::agent_hub::portable_actions::models::PortableAssetActionKind;
use crate::agent_hub::portable_actions::targets::TargetActionRawOutcome;
use crate::agent_hub::portable_inventory::{
    inspect_portable_inventory_force_query, invalidate_portable_inventory_cache, PortableAssetKind,
    PortableInventoryItemDto, PortableInventoryQuery, PortableInventorySourceOrigin,
};
use crate::agent_hub::portable_store::{
    classify_store_link, current_portable_store_roots, execute_skill_or_command_store,
    is_under_portable_store, StoreLinkClass,
};
use crate::error::AppError;
use crate::state::AppState;
use std::fs;
use std::path::Path;

/// 迁移统计（不进 UI，仅 tracing/测试用）。
///
/// Business Logic（为什么需要这个结构体）:
///     镜像迁移是尽力而为的收编：调用方与运维需要知道哪些被收编、哪些跳过/失败，
///     但统计不得反向影响镜像主流程。
///
/// Code Logic（这个结构体做什么）:
///     `migrated` 是成功收编的资产标签（如 `skill:foo` / `command:bar`）；
///     `skipped_store` 计已是 store 软链的观测；`failed` 收集失败/跳过原因。
#[derive(Debug, Default, Clone)]
pub(crate) struct MirrorStoreMigrationStats {
    /// 成功收编的资产标签，如 "skill:foo" / "command:bar"
    pub migrated: Vec<String>,
    /// 已是 store 软链而跳过的观测次数
    pub skipped_store: usize,
    /// 失败/跳过的资产与原因，如 "skill:foo: <原因>"
    pub failed: Vec<String>,
}

/// 镜像冻结前把本机 user-scope Skill/Command 确认版本并收编进 portable-store。
///
/// Business Logic（为什么需要这个函数）:
///     Push/Pull 源端必须先归一「仓库真树 + Agent 软链」与散落真树两种形态，
///     让 freeze 打包的是 portable-store 内有 manifest 版本锚点的真树；
///     该步骤幂等（已收编的观测自然命中 StoreLink 跳过），可在 preview 与
///     apply/freeze 前重复执行而不改变结果。
///
/// Code Logic（这个函数做什么）:
///     失效缓存后强制重扫 user-scope portable inventory；过滤 Skill/Command 且
///     非 plugin 组件的条目，逐观测分类执行 `execute_skill_or_command_store`
///     （绕开 preview/apply 公共入口的 CLI/drift gates）；单条失败记入 stats 继续；
///     结束后再次失效缓存，返回统计。扫描级故障以 Err 传播。
pub(crate) async fn migrate_portable_assets_into_store(
    state: &AppState,
) -> Result<MirrorStoreMigrationStats, AppError> {
    invalidate_portable_inventory_cache();
    let snapshot = inspect_portable_inventory_force_query(
        state,
        PortableInventoryQuery {
            scope_kind: Some(ScopeKind::User),
            target: None,
            kind: None,
            local_project_id: None,
        },
    )
    .await?;
    let mut stats = MirrorStoreMigrationStats::default();
    for item in &snapshot.items {
        if item.scope_kind != ScopeKind::User {
            continue;
        }
        if !matches!(
            item.kind,
            PortableAssetKind::Skill | PortableAssetKind::Command
        ) {
            continue;
        }
        // Plugin 按原文件覆盖同步：组件 Skill/Command 不得被抽离 plugin 目录。
        if item.source_origin == PortableInventorySourceOrigin::PluginComponent
            || item.parent_plugin_inventory_item_id.is_some()
        {
            continue;
        }
        migrate_one_observation(&mut stats, item);
    }
    invalidate_portable_inventory_cache();
    tracing::info!(
        migrated = stats.migrated.len(),
        skipped_store = stats.skipped_store,
        failed = stats.failed.len(),
        "user mirror store migration finished"
    );
    Ok(stats)
}

/// 处理单条 (target, kind, native_id) 观测：分类并执行收编动作。
///
/// Business Logic（为什么需要这个函数）:
///     同一资产常被多个 Agent 观测；逐观测处理配合 store 动作的幂等裁决，
///     能把每个 Agent 根上的软链都换成 store 链，而不是只换第一个。
///
/// Code Logic（这个函数做什么）:
///     source_path 缺失记 failed；StoreLink 计 skipped_store；可解析 EscapeLink
///     走 `MaterializeEscapeLink`；Regular 真树走 `MigrateToStore`（但观测路径本身
///     已在 store 内的只计 skip）；断链跳过不建链。
fn migrate_one_observation(stats: &mut MirrorStoreMigrationStats, item: &PortableInventoryItemDto) {
    let label = format!("{}:{}", item.kind.as_str(), item.native_id);
    let Some(raw_path) = item
        .source_path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    else {
        stats
            .failed
            .push(format!("{label}: inventory source_path missing"));
        return;
    };
    let path = Path::new(raw_path);
    match classify_store_link(path) {
        StoreLinkClass::StoreLink { .. } => {
            stats.skipped_store += 1;
        }
        StoreLinkClass::EscapeLink => {
            if fs::canonicalize(path).is_err() {
                // 断链不迁移：保持原样，后续 freeze 仍按 blocked fail-closed 处理。
                stats
                    .failed
                    .push(format!("{label}: broken escape link left unmigrated"));
                return;
            }
            run_store_action(
                stats,
                item,
                &label,
                PortableAssetActionKind::MaterializeEscapeLink,
                path,
            );
        }
        StoreLinkClass::Regular => {
            // Codex/Gemini/Pi 等的运行时扫描根包含 portable-store 本身：source_path 直接
            // 指向 store 真树的观测不是 Agent 挂载点，绝不能对 store 真树再跑 MigrateToStore
            //（同名 SameContent 裁决会删掉 store 真树换成自引用断链）。
            if is_in_store_observation(path) {
                stats.skipped_store += 1;
                return;
            }
            run_store_action(
                stats,
                item,
                &label,
                PortableAssetActionKind::MigrateToStore,
                path,
            );
        }
    }
}

/// 该观测的 source_path 是否本身就是 portable-store 内的真树。
///
/// Business Logic（为什么需要这个函数）:
///     部分 Agent 把 store 目录当作运行时扫描根，store 真树会以 `Regular` 形态
///     进入 inventory；对它执行迁移会破坏 store，必须按「已在库」跳过。
///
/// Code Logic（这个函数做什么）:
///     canonicalize 后对每个 portable-store 根做 `is_under_portable_store` 前缀判断。
fn is_in_store_observation(path: &Path) -> bool {
    let Ok(canonical) = fs::canonicalize(path) else {
        return false;
    };
    current_portable_store_roots()
        .iter()
        .any(|root| is_under_portable_store(&canonical, root))
}

/// 执行单个 store 收编动作并把结果折算进统计。
///
/// Business Logic（为什么需要这个函数）:
///     镜像迁移是尽力而为：单条失败必须可观测但不得中断其余资产的收编。
///
/// Code Logic（这个函数做什么）:
///     直接调底层 `execute_skill_or_command_store`（含同名版本裁决与 manifest
///     记账）；`Applied` 计 migrated，其它 outcome 与 Err 记 failed 并 warn。
fn run_store_action(
    stats: &mut MirrorStoreMigrationStats,
    item: &PortableInventoryItemDto,
    label: &str,
    action: PortableAssetActionKind,
    path: &Path,
) {
    match execute_skill_or_command_store(
        item.target,
        action,
        item.kind,
        &item.native_id,
        path,
        Some(item),
    ) {
        Ok(TargetActionRawOutcome::Applied) => {
            stats.migrated.push(label.to_string());
        }
        Ok(outcome) => {
            let reason = describe_outcome(&outcome);
            tracing::warn!(
                asset = label,
                action = action.as_str(),
                reason = reason.as_str(),
                "user mirror store migration did not apply"
            );
            stats.failed.push(format!("{label}: {reason}"));
        }
        Err(error) => {
            tracing::warn!(
                asset = label,
                action = action.as_str(),
                error = %error,
                "user mirror store migration failed"
            );
            stats.failed.push(format!("{label}: {error}"));
        }
    }
}

/// 把非 Applied 的原始 outcome 转成一行原因。
///
/// Business Logic: 统计要能定位失败类别，方便运维与测试断言。
/// Code Logic: Blocked/Failed/OutcomeUnknown 拼 code+message；Skipped 描述幂等跳过。
fn describe_outcome(outcome: &TargetActionRawOutcome) -> String {
    match outcome {
        TargetActionRawOutcome::Applied => "applied".to_string(),
        TargetActionRawOutcome::Skipped => "skipped (state already satisfied)".to_string(),
        TargetActionRawOutcome::Blocked { code, message }
        | TargetActionRawOutcome::Failed { code, message }
        | TargetActionRawOutcome::OutcomeUnknown { code, message } => {
            format!("{code}: {message}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::migrate_portable_assets_into_store;
    use crate::backend::runtime::build_app_state;
    use crate::backend::ui::RecordingBackendUi;
    use crate::config::{install_data_dir_env, install_env_var};
    use crate::state::AppState;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    struct MirrorEnv {
        _tmp: tempfile::TempDir,
        _guards: Vec<Box<dyn std::any::Any>>,
        state: AppState,
        home: PathBuf,
        data: PathBuf,
    }

    /// Business Logic（为什么需要这个函数）:
    ///     迁移会真实 move/换链磁盘文件，必须隔离 HOME 与 data_dir，不得碰开发者配置。
    ///
    /// Code Logic（这个函数做什么）:
    ///     tempfile 下建 home/data；注入 `CC_PARTNER_DATA_DIR` 与 `HOME`；构造 AppState。
    async fn seed_mirror_env() -> MirrorEnv {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let data = tmp.path().join("data");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&data).expect("data");
        let data_guard = install_data_dir_env(Some(data.to_str().expect("utf8 data dir")));
        let home_guard = install_env_var("HOME", Some(home.to_str().expect("utf8 home")));
        let ui = Arc::new(RecordingBackendUi::default());
        let state = build_app_state(ui).await.expect("app state");
        MirrorEnv {
            _tmp: tmp,
            _guards: vec![Box::new(data_guard), Box::new(home_guard)],
            state,
            home,
            data,
        }
    }

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(path, text).expect("write");
    }

    /// Business Logic: 软链相关断言只对 unix 有意义，非 unix 直接跳过。
    fn unix_only() -> bool {
        cfg!(unix)
    }

    /// Business Logic（为什么需要这个测试）:
    ///     镜像前收编的核心承诺：散落真树 move 进 portable-store 并在原位换 store 软链。
    ///
    /// Code Logic（这个测试做什么）:
    ///     真树 `~/.claude/skills/foo` → 迁移 → store 真树存在、原路径是软链且
    ///     canonicalize 等于 store 目录、stats 记 `skill:foo` 迁移成功。
    #[tokio::test]
    async fn migrate_moves_real_tree_skill_into_store_and_relinks() {
        if !unix_only() {
            return;
        }
        let env = seed_mirror_env().await;
        write(
            env.home.join(".claude/skills/foo/SKILL.md").as_path(),
            "---\nname: foo\ndescription: d\n---\nBODY\n",
        );

        let stats = migrate_portable_assets_into_store(&env.state)
            .await
            .expect("migrate");

        let store_skill = env.data.join("portable-store/skills/foo");
        assert!(
            store_skill.join("SKILL.md").is_file(),
            "store tree must exist, stats={stats:?}"
        );
        let native = env.home.join(".claude/skills/foo");
        assert!(
            fs::symlink_metadata(&native)
                .expect("native meta")
                .file_type()
                .is_symlink(),
            "native must become a store symlink"
        );
        assert_eq!(
            fs::canonicalize(&native).expect("canonicalize native"),
            fs::canonicalize(&store_skill).expect("canonicalize store"),
            "native link must point at the store tree"
        );
        assert!(
            stats.migrated.iter().any(|label| label == "skill:foo"),
            "stats={stats:?}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     逃逸软链（仓库真树在 store 外）必须材料化：复制进 store、原软链换成 store 链，
    ///     仓库真树原地保留。
    ///
    /// Code Logic（这个测试做什么）:
    ///     `<home>/repo/skills/bar` 真树 + `~/.claude/skills/bar` 软链 → 迁移 →
    ///     store 有内容、原软链指向 store、repo 真树仍在。
    #[tokio::test]
    async fn migrate_materializes_resolvable_escape_link() {
        if !unix_only() {
            return;
        }
        let env = seed_mirror_env().await;
        let repo_skill = env.home.join("repo/skills/bar");
        write(
            repo_skill.join("SKILL.md").as_path(),
            "---\nname: bar\ndescription: repo\n---\nREPO-BODY\n",
        );
        fs::create_dir_all(env.home.join(".claude/skills")).expect("claude skills root");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&repo_skill, env.home.join(".claude/skills/bar"))
            .expect("escape link");
        #[cfg(not(unix))]
        let _ = &repo_skill;

        let stats = migrate_portable_assets_into_store(&env.state)
            .await
            .expect("migrate");

        let store_skill = env.data.join("portable-store/skills/bar");
        assert!(
            store_skill.join("SKILL.md").is_file(),
            "store copy must exist, stats={stats:?}"
        );
        assert_eq!(
            fs::read_to_string(store_skill.join("SKILL.md")).expect("store body"),
            "---\nname: bar\ndescription: repo\n---\nREPO-BODY\n"
        );
        let native = env.home.join(".claude/skills/bar");
        assert!(fs::symlink_metadata(&native)
            .expect("native meta")
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::canonicalize(&native).expect("canonicalize native"),
            fs::canonicalize(&store_skill).expect("canonicalize store")
        );
        assert!(
            repo_skill.join("SKILL.md").is_file(),
            "repo real tree must remain untouched"
        );
        assert!(
            stats.migrated.iter().any(|label| label == "skill:bar"),
            "stats={stats:?}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     断链与已在库两种观测都不得产生副作用：不建链、不重复迁移、不得 panic。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断链软链 + 手工就位的 store 软链 → 迁移 → migrated 为空、skipped_store ≥ 1、
    ///     store 无 broken 真树、断链原样（若 scanner 列出则只允许 broken 进 failed）。
    #[tokio::test]
    async fn migrate_skips_broken_escape_link_and_store_links() {
        if !unix_only() {
            return;
        }
        let env = seed_mirror_env().await;
        fs::create_dir_all(env.home.join(".claude/skills")).expect("skills root");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                env.home.join("no-such-target"),
                env.home.join(".claude/skills/broken"),
            )
            .expect("broken link");
        }
        let store_skill = env.data.join("portable-store/skills/already");
        write(
            store_skill.join("SKILL.md").as_path(),
            "---\nname: already\n---\nok\n",
        );
        #[cfg(unix)]
        std::os::unix::fs::symlink(&store_skill, env.home.join(".claude/skills/already"))
            .expect("store link");

        let stats = migrate_portable_assets_into_store(&env.state)
            .await
            .expect("migrate must not fail on broken links");

        assert!(
            stats.migrated.is_empty(),
            "nothing should be migrated, stats={stats:?}"
        );
        assert!(
            stats.skipped_store >= 1,
            "existing store link must be counted, stats={stats:?}"
        );
        assert!(
            !env.data.join("portable-store/skills/broken").exists(),
            "broken link must not be materialized, stats={stats:?}"
        );
        assert!(
            stats
                .failed
                .iter()
                .all(|entry| entry.starts_with("skill:broken")),
            "only the broken observation may fail, stats={stats:?}"
        );
        let broken = env.home.join(".claude/skills/broken");
        assert!(
            fs::symlink_metadata(&broken)
                .expect("broken meta")
                .file_type()
                .is_symlink(),
            "broken link must stay a symlink"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Plugin 与其内部组件按原文件覆盖同步，迁移不得抽走 plugin 目录里的任何内容。
    ///
    /// Code Logic（这个测试做什么）:
    ///     放 plugin 目录与 plugin 内 skill 组件 → 迁移 → plugin 目录原样、
    ///     组件仍是真树、store 无对应条目。
    #[tokio::test]
    async fn migrate_leaves_plugin_and_mcp_untouched() {
        if !unix_only() {
            return;
        }
        let env = seed_mirror_env().await;
        write(
            env.home
                .join(".claude/plugins/p1/.claude-plugin/plugin.json")
                .as_path(),
            r#"{"name":"p1","version":"1.0.0"}"#,
        );
        let inner = env.home.join(".claude/plugins/p1/skills/inner");
        write(
            inner.join("SKILL.md").as_path(),
            "---\nname: inner\n---\ninner\n",
        );

        let stats = migrate_portable_assets_into_store(&env.state)
            .await
            .expect("migrate");

        assert!(
            env.home
                .join(".claude/plugins/p1/.claude-plugin/plugin.json")
                .is_file(),
            "plugin directory must stay untouched, stats={stats:?}"
        );
        assert!(
            !fs::symlink_metadata(&inner)
                .expect("inner meta")
                .file_type()
                .is_symlink(),
            "plugin inner skill must stay a real tree, stats={stats:?}"
        );
        assert!(inner.join("SKILL.md").is_file());
        assert!(
            !env.data.join("portable-store/skills/inner").exists()
                && !env.data.join("portable-store/skills/p1").exists(),
            "store must not gain plugin content, stats={stats:?}"
        );
        assert!(
            stats
                .migrated
                .iter()
                .all(|label| !label.contains("inner") && !label.contains("p1")),
            "stats={stats:?}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     同一仓库真树被多个 Agent 软链观测时，迁移必须把每个 Agent 根都换成
    ///     指向同一 store 真树的软链（幂等逐观测处理）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     `~/.claude/skills/foo` 与 `~/.codex/skills/foo` 都软链 repo 真树 → 迁移 →
    ///     两个根都 canonicalize 到同一 store 目录，repo 真树仍在。
    #[tokio::test]
    async fn migrate_relinks_second_agent_observation() {
        if !unix_only() {
            return;
        }
        let env = seed_mirror_env().await;
        let repo_skill = env.home.join("repo/skills/foo");
        write(
            repo_skill.join("SKILL.md").as_path(),
            "---\nname: foo\ndescription: shared\n---\nSHARED\n",
        );
        fs::create_dir_all(env.home.join(".claude/skills")).expect("claude skills root");
        fs::create_dir_all(env.home.join(".codex/skills")).expect("codex skills root");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&repo_skill, env.home.join(".claude/skills/foo"))
                .expect("claude escape link");
            std::os::unix::fs::symlink(&repo_skill, env.home.join(".codex/skills/foo"))
                .expect("codex escape link");
        }
        #[cfg(not(unix))]
        let _ = &repo_skill;

        let stats = migrate_portable_assets_into_store(&env.state)
            .await
            .expect("migrate");

        let store_skill = env.data.join("portable-store/skills/foo");
        assert!(
            store_skill.join("SKILL.md").is_file(),
            "store tree must exist once, stats={stats:?}"
        );
        for root in [".claude", ".codex"] {
            let link = env.home.join(root).join("skills/foo");
            assert!(
                fs::symlink_metadata(&link)
                    .unwrap_or_else(|_| panic!("{root} link meta"))
                    .file_type()
                    .is_symlink(),
                "{root} link must stay a symlink, stats={stats:?}"
            );
            assert_eq!(
                fs::canonicalize(&link).unwrap_or_else(|_| panic!("{root} link canonicalize")),
                fs::canonicalize(&store_skill).expect("store canonicalize"),
                "{root} link must point at the shared store tree"
            );
        }
        assert!(
            repo_skill.join("SKILL.md").is_file(),
            "repo real tree must remain"
        );
        assert!(
            stats
                .migrated
                .iter()
                .filter(|label| label.as_str() == "skill:foo")
                .count()
                >= 1,
            "at least the first observation must migrate, stats={stats:?}"
        );
    }
}
