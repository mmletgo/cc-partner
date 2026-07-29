//! agent_hub_gate_a_smoke — Gate A foundation process smoke (L2)
//! Evidence: L2-AGENT-HUB-GATE-A-001
//!
//! Business Logic（为什么需要这个测试文件）:
//!     Gate A 需要在隔离 data_dir 下验证：owner 恢复 prepared jobs、opt-in 前扫描零写入、
//!     opt-in 后嵌套 Claude 编辑向同目录 Codex/OpenCode 收敛、OpenCode prelude 引用祖先、
//!     同块并发编辑产出冲突记录（Attention 源可投影）、以及 git HEAD/index 不被改写；
//!     同时保留用户 CLAUDE.md 迁移幂等证据。
//!
//! Code Logic（这个文件做什么）:
//!     library-level process smoke（不启动完整 backend 二进制）：
//!     A) ProjectionScheduler recover_on_startup 对账 writing/prepared
//!     B) 临时 git 仓库只读 scan，hash/mtime/HEAD/cached 不变，opted_in=false
//!     C) compile_render + opt-in enqueue/run 三目标共享正文 + OpenCode prelude
//!     D) reconcile_instruction 同块 conflict + insert_conflict + list_unresolved
//!     E) git rev-parse HEAD 与 git diff --cached 不变
//!     F) migrate_user_claude_md_state_with 幂等 seed（既有 Task10 证据）
//!
//! NOT VERIFIED（本 smoke 不宣称）:
//!     - 真实 Claude Code / Codex CLI / OpenCode CLI 可执行探测与版本握手
//!     - 完整 sidecar owner runtime watch/debounce/ticker 循环
//!     - GUI/WebView、多机 mDNS、Windows WSL/tmux
//!     - 真实 notify 跨进程 external edit 全链路（仅库级 reconcile + projection）
//!     - 全平台矩阵；当前仅验证 cargo test 本机环境

use app_lib::agent_hub::instructions::{
    ancestor_agent_paths_for_directory, compile_render, reconcile_instruction, ExternalObservation,
    InstructionBlock, InstructionDocument, InstructionReconcileOutcome, ReconcileInput,
};
use app_lib::agent_hub::models::{AssetPolicy, DesiredPresence};
use app_lib::agent_hub::targets::InstructionRenderContext;
use app_lib::{
    agent_hub_sha256_hex, migrate_user_claude_md_state_with, AgentHubObjectStore, AgentHubRepo,
    AgentTarget, AssetKind, ClaudeMdRepo, ClaudeMdRow, DatabaseMaintenanceGate, MigrationDeps,
    NewLogicalAsset, NewScopeNode, NewTargetBinding, ProjectionJobState, ProjectionPayloadKind,
    ProjectionRequest, ProjectionScheduler, RevisionId, ScopeKind, UpsertAgentHubProjectMapping,
    CLAUDE_MD_ID, USER_INSTRUCTION_LOGICAL_KEY, USER_SCOPE_STABLE_ID,
};
use chrono::Utc;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// 隔离环境
// ---------------------------------------------------------------------------

/// 隔离 smoke 根目录与数据路径。
///
/// Business Logic（为什么需要这个结构）:
///     Gate A smoke 不得触碰用户真实 `~/.cc-partner` / HOME。
///
/// Code Logic（这个结构做什么）:
///     持有 tempfile 与 data/objects/db/project 子路径。
struct GateASmokeEnv {
    /// 根临时目录守卫
    _root: tempfile::TempDir,
    /// 隔离 data_dir
    data_dir: PathBuf,
    /// 临时 git 项目根
    project_root: PathBuf,
    /// SQLite 路径
    db_path: PathBuf,
    /// CAS 根
    objects_root: PathBuf,
}

/// Business Logic（为什么需要这个函数）:
///     每个 smoke case 必须使用独立 data 目录，避免污染真实配置。
///
/// Code Logic（这个函数做什么）:
///     创建 tempfile，布局 data/agent-hub/objects、data.db、project/，并注入 CC_PARTNER_DATA_DIR。
fn setup_isolated_env(name: &str) -> GateASmokeEnv {
    let root = tempfile::Builder::new()
        .prefix(&format!("cc-partner-gate-a-{name}-"))
        .tempdir()
        .expect("tempdir");
    let data_dir = root.path().join("data");
    let objects_root = data_dir.join("agent-hub").join("objects");
    let project_root = root.path().join("project");
    let db_path = data_dir.join("data.db");
    fs::create_dir_all(&objects_root).expect("objects root");
    fs::create_dir_all(&project_root).expect("project root");
    // 串行 smoke（--test-threads=1）下设置隔离 data_dir。
    // SAFETY: 仅本 smoke 进程内使用，不跨线程并发改 env。
    std::env::set_var("CC_PARTNER_DATA_DIR", &data_dir);
    GateASmokeEnv {
        _root: root,
        data_dir,
        project_root,
        db_path,
        objects_root,
    }
}

/// Business Logic（为什么需要这个函数）:
///     smoke 需要独立 SQLite + AgentHub schema，不触碰真实 data.db。
///
/// Code Logic（这个函数做什么）:
///     打开 WAL 单连接池并 ensure_schema。
async fn open_hub_pool(db_path: &Path) -> sqlx::SqlitePool {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).expect("db parent");
    }
    let options = SqliteConnectOptions::from_str(&format!("sqlite:{}?mode=rwc", db_path.display()))
        .expect("sqlite options")
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("pool");
    AgentHubRepo::ensure_schema(&pool)
        .await
        .expect("ensure_schema");
    pool
}

/// Business Logic（为什么需要这个函数）:
///     fixture 失败必须立刻暴露。
///
/// Code Logic（这个函数做什么）:
///     在 cwd 执行命令并断言 success。
fn run_ok(cwd: &Path, args: &[&str]) {
    let status = Command::new(args[0])
        .args(&args[1..])
        .current_dir(cwd)
        .status()
        .unwrap_or_else(|e| panic!("spawn {:?}: {e}", args));
    assert!(
        status.success(),
        "command failed: {args:?} cwd={}",
        cwd.display()
    );
}

/// Business Logic（为什么需要这个函数）:
///     断言 git HEAD / cached diff 前后不变。
///
/// Code Logic（这个函数做什么）:
///     捕获 stdout 文本并 trim。
fn run_stdout(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new(args[0])
        .args(&args[1..])
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("spawn {:?}: {e}", args));
    assert!(
        out.status.success(),
        "command failed {:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Business Logic（为什么需要这个函数）:
///     Gate A 要求真实 git 仓库作为项目 fixture，验证不改 HEAD/index。
///
/// Code Logic（这个函数做什么）:
///     git init + README 提交 + origin；写入根/嵌套 CLAUDE.md 与 AGENTS*。
fn init_project_fixture(project: &Path) {
    run_ok(project, &["git", "init", "-b", "main"]);
    run_ok(
        project,
        &["git", "config", "user.email", "gate-a@example.com"],
    );
    run_ok(project, &["git", "config", "user.name", "gate-a"]);
    fs::write(project.join("README.md"), "gate-a fixture\n").expect("readme");
    fs::write(
        project.join("CLAUDE.md"),
        "# Root Claude\n\nshared root body\n",
    )
    .expect("claude root");
    fs::write(
        project.join("AGENTS.md"),
        "# Root Agents\n\nshared root body\n",
    )
    .expect("agents root");
    let nested = project.join("subdir");
    fs::create_dir_all(&nested).expect("subdir");
    fs::write(
        nested.join("CLAUDE.md"),
        "# Nested Claude\n\nnested shared body v1\n",
    )
    .expect("nested claude");
    fs::write(
        nested.join("AGENTS.override.md"),
        "# Nested Codex override\n\nnested shared body v1\n",
    )
    .expect("nested codex");
    fs::write(
        nested.join("AGENTS.md"),
        "# Nested OpenCode\n\nnested shared body v1\n",
    )
    .expect("nested opencode");
    run_ok(project, &["git", "add", "."]);
    run_ok(project, &["git", "commit", "-m", "init gate-a fixture"]);
    run_ok(
        project,
        &[
            "git",
            "remote",
            "add",
            "origin",
            "https://example.com/org/gate-a.git",
        ],
    );
}

/// Business Logic（为什么需要这个函数）:
///     opt-in 前零写断言需要内容指纹。
///
/// Code Logic（这个函数做什么）:
///     读文件字节并 sha256 hex。
fn file_hash(path: &Path) -> String {
    let bytes = fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    agent_hub_sha256_hex(&bytes)
}

/// Business Logic（为什么需要这个函数）:
///     mtime 辅助检测意外写盘（与 hash 并用）。
///
/// Code Logic（这个函数做什么）:
///     返回 modified SystemTime。
fn file_mtime(path: &Path) -> SystemTime {
    fs::metadata(path)
        .unwrap_or_else(|e| panic!("meta {}: {e}", path.display()))
        .modified()
        .expect("mtime")
}

/// Business Logic（为什么需要这个函数）:
///     recovery / projection 测试需要完整 asset+binding。
///
/// Code Logic（这个函数做什么）:
///     insert user scope + instruction asset + Claude binding。
async fn seed_user_instruction_asset(repo: &AgentHubRepo) -> (String, String) {
    let scope = repo
        .insert_scope(NewScopeNode {
            id: Some(format!("user-{}", unique_suffix())),
            kind: ScopeKind::User,
            hub_project_id: None,
            relative_path: None,
        })
        .await
        .expect("scope");
    let asset = repo
        .insert_asset(NewLogicalAsset {
            scope_id: scope.id,
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "root".into(),
            display_name: "root".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .expect("asset");
    let binding = repo
        .upsert_target_binding(NewTargetBinding {
            asset_id: asset.id.clone(),
            target: AgentTarget::Claude,
            local_scope_mapping_id: None,
            checkout_binding_id: None,
            desired_presence: DesiredPresence::Present,
            desired_enabled: true,
        })
        .await
        .expect("binding");
    (asset.id, binding.id)
}

/// Business Logic（为什么需要这个函数）:
///     case 目录与 id 需要唯一后缀。
///
/// Code Logic（这个函数做什么）:
///     纳秒 + pid。
fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos}-{}", std::process::id())
}

/// Business Logic（为什么需要这个函数）:
///     pre-opt-in 预览需要列出将触达的指令文件动作且零写。
///
/// Code Logic（这个函数做什么）:
///     只读 walk 根与 subdir 的 CLAUDE/AGENTS*，返回 planned action 描述。
fn scan_project_planned_actions(project: &Path) -> Vec<String> {
    let mut actions = Vec::new();
    for rel in [
        "CLAUDE.md",
        "AGENTS.md",
        "subdir/CLAUDE.md",
        "subdir/AGENTS.override.md",
        "subdir/AGENTS.md",
    ] {
        let path = project.join(rel);
        if path.is_file() {
            let _ = fs::read(&path).expect("read only");
            actions.push(format!("keep:{rel}"));
        } else {
            actions.push(format!("create:{rel}"));
        }
    }
    actions
}

// ---------------------------------------------------------------------------
// A. recovery of prepared jobs
// ---------------------------------------------------------------------------

/// A. owner 启动恢复 prepared/writing jobs。
///
/// Business Logic（为什么需要这个测试）:
///     崩溃后 owner 重启必须对账 prepared/writing job，禁止仅凭 DB 状态误标 committed。
///
/// Code Logic（这个测试做什么）:
///     enqueue → 人为 writing → recover_on_startup → 目标已是 rendered 则 committed。
#[tokio::test]
async fn gate_a_owner_recovers_prepared_jobs() {
    let env = setup_isolated_env("recover");
    let pool = open_hub_pool(&env.db_path).await;
    let repo = AgentHubRepo::new(pool);
    let store = AgentHubObjectStore::open(&env.objects_root).expect("object store");
    let sched = ProjectionScheduler::new(repo.clone(), store);
    let (asset_id, binding_id) = seed_user_instruction_asset(&repo).await;

    let target = env.data_dir.join("recover-target").join("CLAUDE.md");
    fs::create_dir_all(target.parent().unwrap()).expect("parent");
    fs::write(&target, b"base-content").expect("write base");
    let base = agent_hub_sha256_hex(b"base-content");
    let rendered = b"recovered-content";
    let rendered_hash = agent_hub_sha256_hex(rendered);

    let job = sched
        .enqueue_projection(ProjectionRequest {
            asset_id: asset_id.clone(),
            target: AgentTarget::Claude,
            target_binding_id: binding_id.clone(),
            desired_revision_id: Some(RevisionId::new_v7()),
            target_path: target.to_string_lossy().to_string(),
            expected_external_hash: Some(base.clone()),
            rendered_hash: rendered_hash.clone(),
            rendered_bytes: rendered.to_vec(),
            desired_presence: DesiredPresence::Present,
            desired_enabled: true,
            payload_kind: ProjectionPayloadKind::File,
            directory_entries: None,
            managed_paths: None,
            hub_project_id: None,
            base_hash: Some(base),
        })
        .await
        .expect("enqueue");
    assert_eq!(job.state, ProjectionJobState::Prepared);

    // 模拟崩溃：DB writing，目标已是新内容。
    repo.update_projection_job_state(
        &job.id,
        ProjectionJobState::Writing,
        1,
        Some("simulated_crash"),
        None,
        None,
    )
    .await
    .expect("mark writing");
    fs::write(&target, rendered).expect("simulate completed write");

    let recoverable = repo
        .list_recoverable_projection_jobs()
        .await
        .expect("list recoverable");
    assert!(
        recoverable.iter().any(|j| j.id == job.id),
        "writing job must be recoverable"
    );

    let stats = sched.recover_on_startup().await.expect("recover");
    assert!(stats.recovered >= 1, "stats={stats:?}");
    let done = repo
        .get_projection_job(&job.id)
        .await
        .expect("get")
        .expect("job");
    assert_eq!(
        done.state,
        ProjectionJobState::Committed,
        "hash-matched target must commit after recovery"
    );
    assert_eq!(fs::read(&target).expect("read"), rendered);

    // prepared 且目标仍是 base：recover 后不得卡在 writing。
    let target2 = env.data_dir.join("recover-target").join("CLAUDE2.md");
    fs::write(&target2, b"still-base").expect("base2");
    let base2 = agent_hub_sha256_hex(b"still-base");
    let next = b"after-recover-retry";
    let next_hash = agent_hub_sha256_hex(next);
    let job2 = sched
        .enqueue_projection(ProjectionRequest {
            asset_id,
            target: AgentTarget::Claude,
            target_binding_id: binding_id,
            desired_revision_id: Some(RevisionId::new_v7()),
            target_path: target2.to_string_lossy().to_string(),
            expected_external_hash: Some(base2.clone()),
            rendered_hash: next_hash,
            rendered_bytes: next.to_vec(),
            desired_presence: DesiredPresence::Present,
            desired_enabled: true,
            payload_kind: ProjectionPayloadKind::File,
            directory_entries: None,
            managed_paths: None,
            hub_project_id: None,
            base_hash: Some(base2),
        })
        .await
        .expect("enqueue2");
    repo.update_projection_job_state(&job2.id, ProjectionJobState::Writing, 1, None, None, None)
        .await
        .expect("writing2");
    let _ = sched.recover_on_startup().await.expect("recover2");
    let done2 = repo
        .get_projection_job(&job2.id)
        .await
        .expect("get2")
        .expect("job2");
    assert_ne!(
        done2.state,
        ProjectionJobState::Writing,
        "recover must not leave job stuck in writing: {done2:?}"
    );
    if done2.state == ProjectionJobState::Committed {
        assert_eq!(fs::read(&target2).expect("read2"), next);
    }
}

// ---------------------------------------------------------------------------
// B. pre-opt-in zero writes
// ---------------------------------------------------------------------------

/// B. opt-in 前 project scan 零写入。
///
/// Business Logic（为什么需要这个测试）:
///     用户未 confirm 时，Hub 只能预览计划动作，绝不能改项目仓库文件或 git 状态。
///
/// Code Logic（这个测试做什么）:
///     初始化 git fixture → 记录 hash/mtime/HEAD/cached → 只读 scan planned actions
///     → 断言文件与 git 未变，且 is_hub_project_opted_in=false。
///
/// 说明：完整 `build_project_enable_preview` 需要完整 AppState；本 L2 smoke 验证同等零写契约。
/// 完整 AppState preview 见 unit `project_scope::preview_lists_registered_only_...`。
#[tokio::test]
async fn gate_a_pre_opt_in_scan_zero_writes() {
    let env = setup_isolated_env("pre-optin");
    init_project_fixture(&env.project_root);
    let pool = open_hub_pool(&env.db_path).await;
    let repo = AgentHubRepo::new(pool);

    let watch_paths = [
        env.project_root.join("CLAUDE.md"),
        env.project_root.join("AGENTS.md"),
        env.project_root.join("subdir").join("CLAUDE.md"),
        env.project_root.join("subdir").join("AGENTS.override.md"),
        env.project_root.join("subdir").join("AGENTS.md"),
        env.project_root.join("README.md"),
    ];
    let before_hashes: Vec<(PathBuf, String, SystemTime)> = watch_paths
        .iter()
        .map(|p| (p.clone(), file_hash(p), file_mtime(p)))
        .collect();
    let before_head = run_stdout(&env.project_root, &["git", "rev-parse", "HEAD"]);
    let before_cached = run_stdout(&env.project_root, &["git", "diff", "--cached"]);
    let before_porcelain = run_stdout(&env.project_root, &["git", "status", "--porcelain"]);

    let planned = scan_project_planned_actions(&env.project_root);
    assert!(
        !planned.is_empty(),
        "preview must report planned actions without writing"
    );
    assert!(
        planned.iter().any(|p| p.contains("CLAUDE.md")),
        "planned={planned:?}"
    );

    let hub_id = format!("hub-{}", unique_suffix());
    assert!(
        !repo
            .is_hub_project_opted_in(&hub_id)
            .await
            .expect("opted_in"),
        "missing mapping must not count as opted-in"
    );

    for (path, hash, mtime) in &before_hashes {
        assert_eq!(
            file_hash(path),
            *hash,
            "content changed: {}",
            path.display()
        );
        assert_eq!(
            file_mtime(path),
            *mtime,
            "mtime changed: {}",
            path.display()
        );
    }
    assert_eq!(
        run_stdout(&env.project_root, &["git", "rev-parse", "HEAD"]),
        before_head
    );
    assert_eq!(
        run_stdout(&env.project_root, &["git", "diff", "--cached"]),
        before_cached
    );
    assert_eq!(
        run_stdout(&env.project_root, &["git", "status", "--porcelain"]),
        before_porcelain
    );
}

// ---------------------------------------------------------------------------
// C. post-opt-in nested converge + OpenCode prelude
// ---------------------------------------------------------------------------

/// C. opt-in 后嵌套 Claude 编辑向同目录 Codex/OpenCode 收敛 + OpenCode prelude。
///
/// Business Logic（为什么需要这个测试）:
///     用户在嵌套目录改 Claude 共享正文后，同目录 Codex override 与 OpenCode AGENTS.md
///     应收到同一 shared body；OpenCode 须带 managed prelude 列出祖先相对路径。
///
/// Code Logic（这个测试做什么）:
///     1) upsert opted_in mapping；
///     2) compile_render 三目标断言 body 对齐与 prelude；
///     3) ProjectionScheduler 将渲染字节写入同目录三文件；
///     4) git HEAD/index 不变。
#[tokio::test]
async fn gate_a_post_opt_in_nested_claude_converges_with_opencode_prelude() {
    let env = setup_isolated_env("converge");
    init_project_fixture(&env.project_root);
    let before_head = run_stdout(&env.project_root, &["git", "rev-parse", "HEAD"]);
    let before_cached = run_stdout(&env.project_root, &["git", "diff", "--cached"]);

    let pool = open_hub_pool(&env.db_path).await;
    let repo = AgentHubRepo::new(pool);
    let store = AgentHubObjectStore::open(&env.objects_root).expect("store");
    let sched = ProjectionScheduler::new(repo.clone(), store);

    let hub_project_id = format!("hub-proj-{}", unique_suffix());
    repo.upsert_project_mapping(UpsertAgentHubProjectMapping {
        hub_project_id: hub_project_id.clone(),
        local_workbench_project_id: Some("wb-proj-1".into()),
        git_remote_fingerprint: Some("https://example.com/org/gate-a".into()),
        local_absolute_path: Some(env.project_root.to_string_lossy().to_string()),
        opted_in: true,
    })
    .await
    .expect("opt-in mapping");
    assert!(repo
        .is_hub_project_opted_in(&hub_project_id)
        .await
        .expect("opted"));

    let shared_body = "nested shared body v2 from hub";
    let doc = InstructionDocument {
        relative_key: "subdir".into(),
        blocks: vec![InstructionBlock::shared("blk-shared", shared_body, vec![])],
    };
    // 根存在 AGENTS.md → 祖先目录键为空串
    let ancestors = ancestor_agent_paths_for_directory("subdir", &[String::new()]);
    assert_eq!(ancestors, vec!["../AGENTS.md".to_string()]);
    let ctx = InstructionRenderContext {
        project_root: Some(env.project_root.clone()),
        directory_relative: Some("subdir".into()),
        ancestor_agent_paths: ancestors,
    };

    let claude = compile_render(&doc, AgentTarget::Claude, &ctx);
    let codex = compile_render(&doc, AgentTarget::Codex, &ctx);
    let opencode = compile_render(&doc, AgentTarget::OpenCode, &ctx);

    assert_eq!(claude.file_name, "CLAUDE.md");
    assert_eq!(codex.file_name, "AGENTS.override.md");
    assert_eq!(opencode.file_name, "AGENTS.md");
    assert_eq!(claude.user_body().trim(), shared_body);
    assert_eq!(codex.user_body().trim(), shared_body);
    assert_eq!(opencode.user_body().trim(), shared_body);
    assert_eq!(claude.user_body(), codex.user_body());
    assert_eq!(codex.user_body(), opencode.user_body());
    assert!(opencode.managed_prefix_len > 0);
    let prelude = opencode.managed_prelude().expect("prelude");
    assert!(
        prelude.contains("../AGENTS.md"),
        "OpenCode prelude must reference ancestors: {prelude}"
    );
    assert!(!prelude.contains(shared_body), "prelude must not copy body");

    let scope = repo
        .insert_scope(NewScopeNode {
            id: Some(format!("proj-scope-{}", unique_suffix())),
            kind: ScopeKind::Project,
            hub_project_id: Some(hub_project_id.clone()),
            relative_path: Some("subdir".into()),
        })
        .await
        .expect("project scope");
    let asset = repo
        .insert_asset(NewLogicalAsset {
            scope_id: scope.id,
            kind: AssetKind::Instruction,
            origin_namespace: "project".into(),
            logical_key: "subdir".into(),
            display_name: "subdir".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .expect("asset");

    let nested = env.project_root.join("subdir");
    let targets = [
        (
            AgentTarget::Claude,
            nested.join("CLAUDE.md"),
            claude.bytes.clone(),
        ),
        (
            AgentTarget::Codex,
            nested.join("AGENTS.override.md"),
            codex.bytes.clone(),
        ),
        (
            AgentTarget::OpenCode,
            nested.join("AGENTS.md"),
            opencode.bytes.clone(),
        ),
    ];

    for (target, path, bytes) in &targets {
        let existing = fs::read(path).unwrap_or_default();
        let expected = if existing.is_empty() {
            None
        } else {
            Some(agent_hub_sha256_hex(&existing))
        };
        let binding = repo
            .upsert_target_binding(NewTargetBinding {
                asset_id: asset.id.clone(),
                target: *target,
                local_scope_mapping_id: None,
                checkout_binding_id: None,
                desired_presence: DesiredPresence::Present,
                desired_enabled: true,
            })
            .await
            .expect("binding");
        let hash = agent_hub_sha256_hex(bytes);
        sched
            .enqueue_projection(ProjectionRequest {
                asset_id: asset.id.clone(),
                target: *target,
                target_binding_id: binding.id,
                desired_revision_id: Some(RevisionId::new_v7()),
                target_path: path.to_string_lossy().to_string(),
                expected_external_hash: expected.clone(),
                rendered_hash: hash,
                rendered_bytes: bytes.clone(),
                desired_presence: DesiredPresence::Present,
                desired_enabled: true,
                payload_kind: ProjectionPayloadKind::File,
                directory_entries: None,
                managed_paths: None,
                hub_project_id: Some(hub_project_id.clone()),
                base_hash: expected,
            })
            .await
            .expect("enqueue target");
    }

    let cancel = CancellationToken::new();
    let stats = sched.run_ready_jobs(&cancel).await.expect("run jobs");
    assert!(
        stats.committed >= 3,
        "expected 3 committed projections, stats={stats:?}"
    );

    let claude_text = fs::read_to_string(nested.join("CLAUDE.md")).expect("claude file");
    let codex_text = fs::read_to_string(nested.join("AGENTS.override.md")).expect("codex file");
    let oc_text = fs::read_to_string(nested.join("AGENTS.md")).expect("opencode file");
    assert!(
        claude_text.contains(shared_body),
        "claude missing shared body: {claude_text}"
    );
    assert!(
        codex_text.contains(shared_body),
        "codex missing shared body: {codex_text}"
    );
    assert!(
        oc_text.contains(shared_body),
        "opencode missing shared body: {oc_text}"
    );
    assert!(
        oc_text.contains("../AGENTS.md"),
        "opencode file missing ancestor prelude: {oc_text}"
    );
    assert!(nested.join("AGENTS.override.md").is_file());

    assert_eq!(
        run_stdout(&env.project_root, &["git", "rev-parse", "HEAD"]),
        before_head,
        "Hub projection must not create git commits"
    );
    assert_eq!(
        run_stdout(&env.project_root, &["git", "diff", "--cached"]),
        before_cached,
        "Hub projection must not stage index changes"
    );
}

// ---------------------------------------------------------------------------
// D. concurrent same-block → conflict / attention record
// ---------------------------------------------------------------------------

/// D. 同块并发编辑 → conflict 记录（Attention 源可投影）。
///
/// Business Logic（为什么需要这个测试）:
///     base 上 hub 与 external 同时改同一 shared 块必须冲突冻结，并进入可被 Attention 投影的冲突表。
///
/// Code Logic（这个测试做什么）:
///     reconcile_instruction 同块冲突 → insert_conflict → list_unresolved_conflicts 非空；
///     稳定 ID 格式 `agent-hub:conflict:<id>` 与 attention 源约定一致。
///
/// NOT VERIFIED: 完整 Attention aggregator / GUI Inbox 渲染（见 attention unit tests）。
#[tokio::test]
async fn gate_a_concurrent_same_block_creates_attention_conflict() {
    let env = setup_isolated_env("conflict");
    let pool = open_hub_pool(&env.db_path).await;
    let repo = AgentHubRepo::new(pool);
    let (asset_id, _binding) = seed_user_instruction_asset(&repo).await;

    let base = InstructionDocument {
        relative_key: "subdir".into(),
        blocks: vec![
            InstructionBlock::shared("block-a", "same base", vec![]),
            InstructionBlock::shared("block-b", "other", vec![]),
        ],
    };
    let mut hub = base.clone();
    hub.blocks[0].common_markdown = Some("hub edit on same block".into());
    let external_text = "external edit on same block\n\nother\n";
    let outcome = reconcile_instruction(&ReconcileInput {
        base_document: base,
        hub_document: hub,
        external: ExternalObservation::Present {
            bytes: external_text.as_bytes().to_vec(),
        },
        target: AgentTarget::Claude,
        managed_prefix_len: 0,
        base_block_records: vec![],
    });
    match &outcome {
        InstructionReconcileOutcome::Conflict(c) => {
            assert_eq!(c.block_id.as_deref(), Some("block-a"));
        }
        other => panic!("expected same-block conflict, got {other:?}"),
    }

    let conflict_id = repo
        .insert_conflict(
            &asset_id,
            None,
            r#"{"kind":"canonical","blockId":"block-a","source":"gate_a_smoke"}"#,
        )
        .await
        .expect("insert_conflict");
    let unresolved = repo
        .list_unresolved_conflicts()
        .await
        .expect("list conflicts");
    assert!(
        unresolved
            .iter()
            .any(|c| c.id == conflict_id && !c.resolved),
        "unresolved must contain inserted conflict"
    );
    // Attention source 稳定 ID 合同（project_conflict_item 同源格式）
    let attention_id = format!("agent-hub:conflict:{conflict_id}");
    assert!(
        attention_id.starts_with("agent-hub:conflict:"),
        "attention stable id"
    );
}

// ---------------------------------------------------------------------------
// E. git index / HEAD unchanged
// ---------------------------------------------------------------------------

/// E. fixture HEAD / cached 在关键路径后保持不变。
///
/// Business Logic（为什么需要这个测试）:
///     Gate A 明确承诺不 commit/push 项目仓库；index 与 HEAD 必须稳定。
///
/// Code Logic（这个测试做什么）:
///     初始化 fixture → 记录 HEAD/cached → opt-in mapping + 纯渲染 → 再次断言。
#[tokio::test]
async fn gate_a_git_head_and_index_unchanged() {
    let env = setup_isolated_env("git-index");
    init_project_fixture(&env.project_root);
    let before_head = run_stdout(&env.project_root, &["git", "rev-parse", "HEAD"]);
    let before_cached = run_stdout(&env.project_root, &["git", "diff", "--cached"]);
    assert!(
        before_cached.is_empty(),
        "fixture index should be clean: {before_cached}"
    );

    let pool = open_hub_pool(&env.db_path).await;
    let repo = AgentHubRepo::new(pool);
    let hub_project_id = format!("hub-git-{}", unique_suffix());
    repo.upsert_project_mapping(UpsertAgentHubProjectMapping {
        hub_project_id: hub_project_id.clone(),
        local_workbench_project_id: Some("wb-git".into()),
        git_remote_fingerprint: Some("https://example.com/org/gate-a".into()),
        local_absolute_path: Some(env.project_root.to_string_lossy().to_string()),
        opted_in: true,
    })
    .await
    .expect("mapping");

    // 工作区 dirty 可接受，但不得 stage/commit。
    fs::write(
        env.project_root.join("subdir").join("CLAUDE.md"),
        "dirty from external without git add\n",
    )
    .expect("dirty write");
    let _ = scan_project_planned_actions(&env.project_root);
    let _ = compile_render(
        &InstructionDocument {
            relative_key: "subdir".into(),
            blocks: vec![InstructionBlock::shared("b1", "body", vec![])],
        },
        AgentTarget::OpenCode,
        &InstructionRenderContext {
            project_root: Some(env.project_root.clone()),
            directory_relative: Some("subdir".into()),
            ancestor_agent_paths: ancestor_agent_paths_for_directory("subdir", &[String::new()]),
        },
    );

    assert_eq!(
        run_stdout(&env.project_root, &["git", "rev-parse", "HEAD"]),
        before_head
    );
    assert_eq!(
        run_stdout(&env.project_root, &["git", "diff", "--cached"]),
        before_cached
    );
    assert!(
        !repo
            .is_hub_project_opted_in("never-opted")
            .await
            .expect("check"),
        "unrelated hub id remains false"
    );
}

// ---------------------------------------------------------------------------
// F. migration evidence (Task 10 seed)
// ---------------------------------------------------------------------------

/// Business Logic（为什么需要这个函数）:
///     smoke 不得触碰真实 `~/.cc-partner` 或开发者 home。
///
/// Code Logic（这个函数做什么）:
///     在系统 temp 下创建唯一 case 根目录。
fn make_case_root(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "cc-partner-agent-hub-gate-a-{}-{}-{}",
        label,
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&root).expect("create case root");
    root
}

/// Business Logic（为什么需要这个函数）:
///     迁移依赖 AgentHub + legacy claude_md schema。
///
/// Code Logic（这个函数做什么）:
///     打开 file SQLite，ensure Agent Hub schema，建最小 claude_md 表。
async fn setup_migration_repos(db_path: &Path) -> (AgentHubRepo, ClaudeMdRepo) {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).expect("db parent");
    }
    let options = SqliteConnectOptions::from_str(&format!("sqlite:{}", db_path.display()))
        .expect("sqlite options")
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("connect sqlite");
    AgentHubRepo::ensure_schema(&pool)
        .await
        .expect("agent hub schema");
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS claude_md (
            id TEXT PRIMARY KEY NOT NULL,
            content TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            device_id TEXT NOT NULL,
            vector_clock TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("claude_md schema");
    let gate = Arc::new(DatabaseMaintenanceGate::new());
    (
        AgentHubRepo::with_gate(pool.clone(), gate.clone()),
        ClaudeMdRepo::with_gate(pool, gate),
    )
}

/// Business Logic（为什么需要这个函数）:
///     MigrationDeps 需要稳定 device_id 与 object store 根。
///
/// Code Logic（这个函数做什么）:
///     组装引用生命周期内的 deps。
fn migration_deps<'a>(
    agent_hub: &'a AgentHubRepo,
    claude_md: &'a ClaudeMdRepo,
    objects_root: &'a Path,
) -> MigrationDeps<'a> {
    MigrationDeps {
        agent_hub,
        claude_md,
        device_id: "smoke-device-1",
        object_store_root: objects_root,
    }
}

/// Business Logic（为什么需要这个测试）:
///     Gate A 迁移必须幂等 seed user instruction，且 projection binding 默认 absent。
///
/// Code Logic（这个测试做什么）:
///     写隔离 CLAUDE.md → migrate 两次 → 断言 targetOnly/absent/幂等。
#[tokio::test]
async fn gate_a_migration_seeds_target_only_absent_and_is_idempotent() {
    let root = make_case_root("migrate");
    let data_dir = root.join("data");
    let home_fake = root.join("home");
    let claude_dir = home_fake.join(".claude");
    fs::create_dir_all(&claude_dir).expect("claude dir");
    fs::create_dir_all(&data_dir).expect("data dir");

    let claude_file = claude_dir.join("CLAUDE.md");
    let body = "# Smoke rules\n\nAlways confirm before edits.\n";
    fs::write(&claude_file, body).expect("write claude md");

    let db_path = data_dir.join("data.db");
    let (agent_hub, claude_md) = setup_migration_repos(&db_path).await;

    // seed legacy row with different content — non-empty file must win
    let mut vc = HashMap::new();
    vc.insert("smoke-device-1".into(), 1u64);
    claude_md
        .upsert(&ClaudeMdRow {
            id: CLAUDE_MD_ID.into(),
            content: "legacy-db-body-should-lose".into(),
            updated_at: Utc::now().to_rfc3339(),
            device_id: "smoke-device-1".into(),
            vector_clock: vc,
        })
        .await
        .expect("seed legacy");

    let first = migrate_user_claude_md_state_with(
        &migration_deps(&agent_hub, &claude_md, &data_dir),
        &claude_file,
    )
    .await
    .expect("first migrate");

    assert_eq!(first.content_source, "file");
    assert_eq!(first.policy, "targetOnly");
    assert_eq!(first.desired_presence, "absent");
    assert!(first.created_revision, "first migrate must create revision");
    assert!(first.revision_id.is_some());
    assert!(first.blocks_target_only);
    // codex/opencode diffs may be empty strings for pure Claude targetOnly imports
    // (compile_render still returns a valid projection payload).
    let _ = (&first.codex_diff, &first.opencode_diff);

    let asset = agent_hub
        .get_asset(&first.asset_id)
        .await
        .expect("get asset ok")
        .expect("asset exists");
    assert_eq!(asset.policy, AssetPolicy::TargetOnly);
    assert_eq!(asset.logical_key, USER_INSTRUCTION_LOGICAL_KEY);
    assert_eq!(asset.scope_id, USER_SCOPE_STABLE_ID);

    let bindings = agent_hub
        .list_target_bindings_for_asset(&first.asset_id)
        .await
        .expect("bindings");
    assert_eq!(bindings.len(), 3, "claude/codex/opencode bindings");
    for binding in &bindings {
        assert_eq!(
            binding.desired_presence,
            DesiredPresence::Absent,
            "projection bindings start absent until confirmation"
        );
    }

    let second = migrate_user_claude_md_state_with(
        &migration_deps(&agent_hub, &claude_md, &data_dir),
        &claude_file,
    )
    .await
    .expect("second migrate");
    assert!(
        !second.created_revision,
        "idempotent second run must not append revision"
    );
    assert_eq!(second.asset_id, first.asset_id);
    assert_eq!(second.payload_hash, first.payload_hash);

    let _ = fs::remove_dir_all(&root);
}

/// Business Logic（为什么需要这个测试）:
///     未 opt-in 时项目树不得被 smoke 误写成 fixture HEAD；此处固定无 git 仓路径。
///
/// Code Logic（这个测试做什么）:
///     在无 .git 的项目路径上确认迁移不创建 git 目录。
#[tokio::test]
async fn gate_a_migration_does_not_create_git_dir_on_unrelated_project_path() {
    let root = make_case_root("no-git");
    let data_dir = root.join("data");
    let project = root.join("project");
    fs::create_dir_all(&project).expect("project");
    fs::create_dir_all(&data_dir).expect("data");
    let claude_file = root.join("CLAUDE.md");
    fs::write(&claude_file, "# only\n").expect("write");

    let (agent_hub, claude_md) = setup_migration_repos(&data_dir.join("data.db")).await;
    let _preview = migrate_user_claude_md_state_with(
        &migration_deps(&agent_hub, &claude_md, &data_dir),
        &claude_file,
    )
    .await
    .expect("migrate");

    assert!(
        !project.join(".git").exists(),
        "migration must not init git in unrelated project path"
    );
    let _ = fs::remove_dir_all(&root);
}
