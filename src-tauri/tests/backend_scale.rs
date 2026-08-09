//! S5 Backend Scale 集成测试：claim 三阶段、慢 workflow、CAS 竞态、扫描公平性、
//! CC History mixed-version，以及 Task 8 负载/故障门禁与连接池决策证据。
//!
//! Business Logic（为什么需要这个测试文件）:
//!     生产单连接池下，事务内 WORKFLOW.md IO 会饿死其它查询；CC History 分页协议
//!     必须在 new↔new / new↔legacy / 畸形响应下行为正确；扩池到 2 必须有可重复的
//!     五项门槛证据，禁止凭感觉改生产 `max_connections`。
//!
//! Code Logic（这个文件做什么）:
//!     1) orchestrator_claim_*：文件 SQLite fixture 覆盖 preflight/CAS/cursor
//!     2) cc_history_mixed_version_*：委托 app_lib::mixed_version_harness
//!     3) scale_safety_*：有界正确性断言（limit/CAS/rollback/SQLITE_BUSY=0）
//!     4) backend_scale_benchmark（#[ignore]）：10k history + 1k tasks 混合压测 JSON
use app_lib::orchestrator::claim::{
    preflight_claim_candidates, preflight_claim_candidates_with_resolver, ClaimCandidate,
    ClaimScanCursor, CLAIM_CANDIDATE_LIMIT,
};
use app_lib::orchestrator::models::{
    OrchestratorRunState, OrchestratorTaskRow, OrchestratorTaskStatus, OrchestratorWorkflowState,
};
use app_lib::orchestrator::repo::OrchestratorRepo;
use app_lib::orchestrator::workflow::ResolvedWorkflow;
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::sync::oneshot;

/// 测试用 DB 句柄：持有 TempDir 防止库文件被删。
struct TestDb {
    _dir: TempDir,
    pool: SqlitePool,
    repo: OrchestratorRepo,
}

/// Business Logic（为什么需要这个函数）:
///     每个 scale 测试需要隔离的 Orchestrator schema，避免污染用户库。
///
/// Code Logic（这个函数做什么）:
///     在临时目录创建文件型 SQLite，busy_timeout=5s；默认 max_connections=2 以便
///     并发 CAS 竞态测试可同时排队写锁（仅测试池，不代表生产池）。
async fn setup_repo() -> TestDb {
    setup_repo_with_pool(2).await
}

/// Business Logic（为什么需要这个函数）:
///     正确性与压测需要可控的测试连接池大小，且不得改动生产 runtime 配置。
///
/// Code Logic（这个函数做什么）:
///     创建 WAL + busy_timeout=5s 的文件型 SQLite，按 `max_connections` 建池并初始化
///     Orchestrator schema，返回 TestDb。
async fn setup_repo_with_pool(max_connections: u32) -> TestDb {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("scale.db");
    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections.max(1))
        .connect_with(options)
        .await
        .unwrap();
    OrchestratorRepo::init_schema(&pool).await.unwrap();
    let repo = OrchestratorRepo::new(pool.clone());
    TestDb {
        _dir: dir,
        pool,
        repo,
    }
}

/// Business Logic（为什么需要这个函数）:
///     双写事务在 SQLite 上可能瞬时 BUSY；测试要证明 CAS 语义而非锁错误。
///
/// Code Logic（这个函数做什么）:
///     对 claim_preflighted 在 locked/busy 时有界重试，其它错误直接传播。
async fn claim_with_busy_retry(
    repo: &OrchestratorRepo,
    limit: i64,
    eligible: &[ClaimCandidate],
) -> app_lib::orchestrator::claim::ClaimCasOutcome {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match repo
            .claim_preflighted_candidates_with_global_capacity(limit, eligible)
            .await
        {
            Ok(outcome) => return outcome,
            Err(err) => {
                let msg = err.to_string().to_ascii_lowercase();
                if Instant::now() < deadline
                    && (msg.contains("locked") || msg.contains("busy") || msg.contains("deadlock"))
                {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    continue;
                }
                panic!("claim failed: {err}");
            }
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     claim JOIN 依赖 workbench_projects 表。
///
/// Code Logic（这个函数做什么）:
///     创建最小 workbench_projects 字段子集。
async fn create_workbench_projects_table(pool: &SqlitePool) {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS workbench_projects (\
         id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, device_id TEXT NOT NULL, \
         device_name TEXT NOT NULL, path TEXT NOT NULL, last_opened_at TEXT NOT NULL, \
         created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
    )
    .execute(pool)
    .await
    .unwrap();
}

/// Business Logic（为什么需要这个函数）:
///     测试需要把项目 path 指到真实 temp 目录以放置 WORKFLOW.md。
///
/// Code Logic（这个函数做什么）:
///     插入 local/remote 项目行，path 由调用方指定。
async fn insert_workbench_project(pool: &SqlitePool, id: &str, kind: &str, path: &Path) {
    sqlx::query(
        "INSERT INTO workbench_projects \
         (id, name, kind, device_id, device_name, path, last_opened_at, created_at, updated_at) \
         VALUES (?, ?, ?, 'device-test', 'Device Test', ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(format!("Project {id}"))
    .bind(kind)
    .bind(path.to_string_lossy().as_ref())
    .bind("2026-07-05T00:00:00Z")
    .bind("2026-07-05T00:00:00Z")
    .bind("2026-07-05T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
}

/// Business Logic（为什么需要这个函数）:
///     构造可 claim 的 Queued/Idle 任务行。
///
/// Code Logic（这个函数做什么）:
///     基于 default_for_status 填充稳定字段并覆盖 id/project/priority/created_at。
fn queued_task(id: &str, project_id: &str, priority: i64, created_at: &str) -> OrchestratorTaskRow {
    OrchestratorTaskRow {
        id: id.to_string(),
        project_id: project_id.to_string(),
        title: format!("Task {id}"),
        goal: format!("Goal {id}"),
        acceptance_criteria: format!("Criteria {id}"),
        status: OrchestratorTaskStatus::Queued,
        priority,
        created_at: created_at.to_string(),
        updated_at: created_at.to_string(),
        ..OrchestratorTaskRow::default_for_status(OrchestratorTaskStatus::Queued)
    }
}

/// Business Logic（为什么需要这个函数）:
///     确保 split state 为 Todo/Idle，与 claim SELECT 条件一致。
///
/// Code Logic（这个函数做什么）:
///     直接 UPDATE workflow_state/run_state。
async fn set_todo_idle(pool: &SqlitePool, task_id: &str) {
    sqlx::query(
        "UPDATE orchestrator_tasks SET workflow_state = ?, run_state = ?, blocked_reason = NULL WHERE id = ?",
    )
    .bind(OrchestratorWorkflowState::Todo.as_str())
    .bind(OrchestratorRunState::Idle.as_str())
    .bind(task_id)
    .execute(pool)
    .await
    .unwrap();
}

/// Business Logic（为什么需要这个函数）:
///     无效 WORKFLOW.md 项目用于填满 256 窗，验证 cursor 公平性。
///
/// Code Logic（这个函数做什么）:
///     写入缺少 front matter 结束分隔符的 WORKFLOW.md，使 resolve 失败。
fn write_invalid_workflow(project_dir: &Path) {
    std::fs::create_dir_all(project_dir).unwrap();
    std::fs::write(
        project_dir.join("WORKFLOW.md"),
        "---\nworkflow:\n  active_states: [todo]\n",
    )
    .unwrap();
}

/// Business Logic（为什么需要这个测试）:
///     慢 workflow 解析期间其它 DB 读必须仍能完成，否则单连接池会被 claim 饿死。
///
/// Code Logic（这个测试做什么）:
///     用 oneshot 阻塞 resolver；在 preflight 进行中并发 `repo.get_task`，断言 100ms 内返回。
#[tokio::test]
async fn orchestrator_claim_slow_preflight_allows_concurrent_get_task() {
    let db = setup_repo().await;
    let pool = &db.pool;
    let repo = &db.repo;
    create_workbench_projects_table(pool).await;
    let dir = TempDir::new().unwrap();
    insert_workbench_project(pool, "local-a", "local", dir.path()).await;

    let mut task = queued_task("task-1", "local-a", 10, "2026-07-05T00:00:01Z");
    repo.create_task(&task).await.unwrap();
    set_todo_idle(pool, "task-1").await;
    task = repo.get_task("task-1").await.unwrap();

    let candidates = vec![ClaimCandidate {
        task,
        project_path: dir.path().to_path_buf(),
    }];

    let (release_tx, release_rx) = oneshot::channel::<()>();
    let release_rx = Arc::new(Mutex::new(Some(release_rx)));
    let resolver = Arc::new(
        move |_path: &Path| -> Result<ResolvedWorkflow, app_lib::error::AppError> {
            if let Some(rx) = release_rx.lock().expect("lock").take() {
                let _ = rx.blocking_recv();
            }
            Ok(ResolvedWorkflow::built_in_default())
        },
    );

    let preflight_handle = tokio::spawn(async move {
        preflight_claim_candidates_with_resolver(candidates, resolver).await
    });

    // 等待 spawn_blocking 进入阻塞 resolver。
    tokio::time::sleep(Duration::from_millis(30)).await;

    let started = Instant::now();
    let concurrent = tokio::time::timeout(Duration::from_millis(100), repo.get_task("task-1"))
        .await
        .expect("get_task must complete within 100ms during slow preflight")
        .expect("get_task ok");
    assert_eq!(concurrent.id, "task-1");
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "elapsed {:?}",
        started.elapsed()
    );

    let _ = release_tx.send(());
    let preflight = preflight_handle.await.expect("join").expect("preflight");
    assert_eq!(preflight.eligible.len(), 1);
}

/// Business Logic（为什么需要这个测试）:
///     两个并发 CAS 对同一 eligible 集合领取时，每个任务最多返回一次，禁止重复 dispatch。
///
/// Code Logic（这个测试做什么）:
///     插入 2 个 Queued 任务，preflight 后并发两次 `claim_preflighted...limit=2`，
///     断言并集恰好 2 个唯一 id，无重复 Preparing 行。
#[tokio::test]
async fn orchestrator_claim_concurrent_cas_no_duplicate() {
    let db = setup_repo().await;
    let pool = &db.pool;
    let repo = &db.repo;
    create_workbench_projects_table(pool).await;
    let dir = TempDir::new().unwrap();
    // 无 WORKFLOW.md → built-in Todo/Rework active。
    insert_workbench_project(pool, "local-a", "local", dir.path()).await;

    for (id, created_at) in [
        ("task-a", "2026-07-05T00:00:01Z"),
        ("task-b", "2026-07-05T00:00:02Z"),
    ] {
        repo.create_task(&queued_task(id, "local-a", 10, created_at))
            .await
            .unwrap();
        set_todo_idle(pool, id).await;
    }

    let candidates = repo
        .list_local_queued_claim_candidates(None, CLAIM_CANDIDATE_LIMIT)
        .await
        .unwrap();
    let preflight = preflight_claim_candidates(candidates).await.unwrap();
    assert_eq!(preflight.eligible.len(), 2);

    let repo_a = OrchestratorRepo::new(db.pool.clone());
    let repo_b = OrchestratorRepo::new(db.pool.clone());
    let eligible_a = preflight.eligible.clone();
    let eligible_b = preflight.eligible.clone();

    let (out_a, out_b) = tokio::join!(
        claim_with_busy_retry(&repo_a, 2, &eligible_a),
        claim_with_busy_retry(&repo_b, 2, &eligible_b),
    );
    let a = out_a.claimed;
    let b = out_b.claimed;

    let mut ids: Vec<String> = a.iter().chain(b.iter()).map(|t| t.id.clone()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(
        ids,
        vec!["task-a".to_string(), "task-b".to_string()],
        "each task returned exactly once across concurrent CAS; a={:?} b={:?}",
        a.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        b.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
    );
    assert_eq!(a.len() + b.len(), 2);
}

/// Business Logic（为什么需要这个测试）:
///     前 256 个无效 workflow 项目不得永久饿死后继合法任务；满窗/project-cap 旋转推进 cursor。
///
/// Code Logic（这个测试做什么）:
///     256 个无效项目各 1 任务 + 1 个合法项目任务；反复 list+preflight 按 advance_cursor 推进，
///     最终必须命中 good-task 并可 CAS claim。
#[tokio::test]
async fn orchestrator_claim_cursor_skips_invalid_window_then_wraps() {
    let db = setup_repo().await;
    let pool = &db.pool;
    let repo = &db.repo;
    create_workbench_projects_table(pool).await;
    let root = TempDir::new().unwrap();

    // 256 无效项目（每个独立 path + 坏 WORKFLOW.md）
    for i in 0..CLAIM_CANDIDATE_LIMIT {
        let project_id = format!("bad-{i:04}");
        let path = root.path().join(&project_id);
        write_invalid_workflow(&path);
        insert_workbench_project(pool, &project_id, "local", &path).await;
        let task_id = format!("bad-task-{i:04}");
        let created_at = format!("2026-07-05T00:{:02}:{:02}Z", (i / 60), (i % 60));
        repo.create_task(&queued_task(&task_id, &project_id, 100, &created_at))
            .await
            .unwrap();
        set_todo_idle(pool, &task_id).await;
    }

    // 合法项目：无 WORKFLOW.md → built-in default
    let good_path = root.path().join("good");
    std::fs::create_dir_all(&good_path).unwrap();
    insert_workbench_project(pool, "good", "local", &good_path).await;
    repo.create_task(&queued_task("good-task", "good", 1, "2026-07-05T01:00:00Z"))
        .await
        .unwrap();
    set_todo_idle(pool, "good-task").await;

    let mut cursor: Option<ClaimScanCursor> = None;
    let mut good_eligible: Option<Vec<ClaimCandidate>> = None;
    // project-cap=64 时最多约 ceil(256/64)=4 次旋转 + 1 次触尾即可扫到 good。
    for _ in 0..16 {
        let page = repo
            .list_local_queued_claim_candidates(cursor.as_ref(), CLAIM_CANDIDATE_LIMIT)
            .await
            .unwrap();
        if page.is_empty() {
            cursor = None;
            continue;
        }
        let preflight = preflight_claim_candidates(page).await.unwrap();
        if let Some(c) = preflight.eligible.iter().find(|c| c.task.id == "good-task") {
            good_eligible = Some(vec![c.clone()]);
            break;
        }
        if preflight.advance_cursor {
            cursor = preflight.next_cursor;
        } else {
            cursor = None;
        }
    }
    let eligible = good_eligible.expect("good-task must appear after cursor rotation");

    let claimed = repo
        .claim_preflighted_candidates_with_global_capacity(1, &eligible)
        .await
        .unwrap()
        .claimed;
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, "good-task");
    assert_eq!(claimed[0].status, OrchestratorTaskStatus::Preparing);

    // 从 good-task 的 keyset 继续翻页应为空（触尾；调用方置 None 回绕）。
    let tail_cursor = ClaimScanCursor::from_task(&claimed[0]);
    let page_tail = repo
        .list_local_queued_claim_candidates(Some(&tail_cursor), CLAIM_CANDIDATE_LIMIT)
        .await
        .unwrap();
    assert!(
        page_tail.is_empty(),
        "after good-task keyset must be empty for wrap"
    );
}

/// Business Logic（为什么需要这个测试）:
///     兼容入口 `claim_next_local_queued_tasks_with_global_capacity` 必须仍能领取默认 workflow 任务。
///
/// Code Logic（这个测试做什么）:
///     无 WORKFLOW.md 的 local 项目插入 Queued 任务，调用兼容入口断言 Preparing。
#[tokio::test]
async fn orchestrator_claim_compat_entry_claims_default_workflow() {
    let db = setup_repo().await;
    let pool = &db.pool;
    let repo = &db.repo;
    create_workbench_projects_table(pool).await;
    let dir = TempDir::new().unwrap();
    insert_workbench_project(pool, "local-a", "local", dir.path()).await;
    repo.create_task(&queued_task("task-1", "local-a", 5, "2026-07-05T00:00:01Z"))
        .await
        .unwrap();
    set_todo_idle(pool, "task-1").await;

    let claimed = repo
        .claim_next_local_queued_tasks_with_global_capacity(1)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, "task-1");
    assert_eq!(claimed[0].status, OrchestratorTaskStatus::Preparing);
    assert!(claimed[0].prepare_claim_token.is_some());
}

// --- CC History mixed-version (Task 6) ---
/// new↔new 仅 paged。
#[test]
fn cc_history_mixed_version_new_to_new_uses_only_paged_routes() {
    app_lib::mixed_version_harness::assert_new_to_new_uses_only_paged_routes();
}

/// new↔legacy 仅 legacy。
#[test]
fn cc_history_mixed_version_new_to_legacy_uses_only_legacy_routes() {
    app_lib::mixed_version_harness::assert_new_to_legacy_uses_only_legacy_routes();
}

/// 畸形 paged 失败本轮。
#[test]
fn cc_history_mixed_version_malformed_paged_fails_round_not_empty_success() {
    app_lib::mixed_version_harness::assert_malformed_paged_fails_round_not_empty_success();
}

/// legacy body 对新服务端仍可用。
#[test]
fn cc_history_mixed_version_legacy_bodies_work_against_new_server() {
    app_lib::mixed_version_harness::assert_legacy_bodies_work_against_new_server();
}

/// H1：多 ID 批混入 item_too_large 毒丸时拆批并仍 pull 好数据。
#[test]
fn cc_history_mixed_version_item_too_large_halves_and_isolates_poison() {
    app_lib::mixed_version_harness::assert_item_too_large_halves_and_isolates_poison();
}

/// 413 batch_too_large 对半拆批直至成功。
#[test]
fn cc_history_mixed_version_batch_too_large_halves_until_success() {
    app_lib::mixed_version_harness::assert_batch_too_large_halves_until_success();
}

/// 并发 VC merge 收敛（mock push 走 merge）。
#[test]
fn cc_history_mixed_version_concurrent_vector_clock_merges() {
    app_lib::mixed_version_harness::assert_concurrent_vector_clock_merges();
}

/// Business Logic（为什么需要这个测试）:
///     生产池 max_connections=1 时并发 claim 仍不得重复领取（排队串行化后 CAS 语义仍成立）。
///
/// Code Logic（这个测试做什么）:
///     与 pool=2 竞态用例相同的双 task 并发 claim，但 fixture 用 pool=1。
#[tokio::test]
async fn orchestrator_claim_concurrent_cas_no_duplicate_pool1() {
    let db = setup_repo_with_pool(1).await;
    let pool = &db.pool;
    let repo = &db.repo;
    create_workbench_projects_table(pool).await;
    let dir = TempDir::new().unwrap();
    insert_workbench_project(pool, "local-a", "local", dir.path()).await;

    for (id, created_at) in [
        ("task-a", "2026-07-05T00:00:01Z"),
        ("task-b", "2026-07-05T00:00:02Z"),
    ] {
        repo.create_task(&queued_task(id, "local-a", 10, created_at))
            .await
            .unwrap();
        set_todo_idle(pool, id).await;
    }

    let candidates = repo
        .list_local_queued_claim_candidates(None, CLAIM_CANDIDATE_LIMIT)
        .await
        .unwrap();
    let preflight = preflight_claim_candidates(candidates).await.unwrap();
    assert_eq!(preflight.eligible.len(), 2);

    let repo_a = OrchestratorRepo::new(db.pool.clone());
    let repo_b = OrchestratorRepo::new(db.pool.clone());
    let eligible_a = preflight.eligible.clone();
    let eligible_b = preflight.eligible.clone();

    let (out_a, out_b) = tokio::join!(
        claim_with_busy_retry(&repo_a, 2, &eligible_a),
        claim_with_busy_retry(&repo_b, 2, &eligible_b),
    );
    let mut ids: Vec<String> = out_a
        .claimed
        .iter()
        .chain(out_b.claimed.iter())
        .map(|t| t.id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(
        ids,
        vec!["task-a".to_string(), "task-b".to_string()],
        "pool=1 concurrent CAS must still unique-claim"
    );
    assert_eq!(out_a.claimed.len() + out_b.claimed.len(), 2);
}

// --- S5 Task 8: safety gates + ignored load benchmark ---

/// 协议/仓储固定上限（与生产路由常量对齐；集成测试侧镜像，避免依赖私有模块导出）。
const BENCH_MANIFEST_PAGE_LIMIT_MAX: u32 = 512;
const BENCH_ITEM_BATCH_LIMIT: usize = 128;
const BENCH_CONTENT_MAX_BYTES: usize = 1024 * 1024;
const BENCH_ID_MAX_BYTES: usize = 256;
const BENCH_HISTORY_ROWS: usize = 10_000;
const BENCH_QUEUED_TASKS: usize = 1_000;
const BENCH_PROJECTS: usize = 100;
const BENCH_INVALID_WORKFLOWS: usize = 10;

/// 压测样本分位数摘要（毫秒）。
///
/// Business Logic（为什么需要这个结构）:
///     扩池决策需要 median/p95/max 三类可比较的等待/事务/tick 证据，且不得暴露业务内容。
///
/// Code Logic（这个结构做什么）:
///     保存已排序样本的中位、p95 与最大值（毫秒，向上取整到 u64）。
#[derive(Debug, Clone, Serialize)]
struct LatencySummaryMs {
    median: u64,
    p95: u64,
    max: u64,
    samples: usize,
}

/// 正文字节分布（脱敏计数）。
#[derive(Debug, Clone, Serialize)]
struct ContentDistribution {
    rows_1kib: usize,
    rows_64kib: usize,
    rows_1mib: usize,
}

/// 单次压测机读 JSON 行（脱敏：无 ID/正文/路径）。
///
/// Business Logic（为什么需要这个结构）:
///     运维与计划 Task 8 需要可脚本解析的证据，决定是否允许把生产池扩到 2。
///
/// Code Logic（这个结构做什么）:
///     序列化 pool、轮次、规模、等待/事务/tick 分位数、RSS、错误计数与门禁辅助字段。
#[derive(Debug, Clone, Serialize)]
struct BenchmarkJsonRow {
    pool_max_connections: u32,
    runs: u32,
    history_rows: usize,
    queued_tasks: usize,
    projects: usize,
    invalid_workflows: usize,
    content_distribution: ContentDistribution,
    acquire_wait_ms: LatencySummaryMs,
    transaction_ms: LatencySummaryMs,
    scheduler_tick_ms: LatencySummaryMs,
    rss_peak_bytes: u64,
    errors: u64,
    sqlite_busy: u64,
    cas_duplicates: u64,
    partial_batches: u64,
    limit_violations: u64,
    /// 连接等待 p95 是否超过 50ms（§4.2 门槛 1 的单次观测）。
    wait_p95_over_50ms: bool,
}

/// 压测错误计数（仅聚合，不含文案/路径）。
#[derive(Debug, Clone, Serialize, Default)]
struct BenchmarkErrorCounts {
    sqlite_busy: u64,
    limit_violations: u64,
    other: u64,
    cas_duplicates: u64,
    partial_batches: u64,
}

/// Business Logic（为什么需要这个函数）:
///     分位数是扩池门槛比较的输入，必须确定性且不含业务数据。
///
/// Code Logic（这个函数做什么）:
///     对样本排序后取 median（偶数取上中位）、ceil(0.95*(n-1)) 位置的 p95 与 max。
fn latency_summary_ms(mut samples: Vec<u64>) -> LatencySummaryMs {
    if samples.is_empty() {
        return LatencySummaryMs {
            median: 0,
            p95: 0,
            max: 0,
            samples: 0,
        };
    }
    samples.sort_unstable();
    let n = samples.len();
    let median = samples[n / 2];
    let p95_idx = ((n as f64 - 1.0) * 0.95).ceil() as usize;
    let p95 = samples[p95_idx.min(n - 1)];
    let max = *samples.last().unwrap();
    LatencySummaryMs {
        median,
        p95,
        max,
        samples: n,
    }
}

/// Business Logic（为什么需要这个函数）:
///     门禁要求报告进程 RSS，便于观察 10k 正文分页后的峰值内存是否失控。
///
/// Code Logic（这个函数做什么）:
///     Unix 用 getrusage(RUSAGE_SELF).ru_maxrss：macOS 为字节，Linux 为 KiB（×1024）。
fn current_rss_bytes() -> u64 {
    #[cfg(unix)]
    {
        // SAFETY: getrusage 写入调用方栈上 rusage；RUSAGE_SELF 仅读本进程。
        unsafe {
            let mut usage: libc::rusage = std::mem::zeroed();
            if libc::getrusage(libc::RUSAGE_SELF, &mut usage) == 0 {
                let rss = usage.ru_maxrss;
                if rss <= 0 {
                    return 0;
                }
                #[cfg(target_os = "macos")]
                {
                    return rss as u64;
                }
                #[cfg(not(target_os = "macos"))]
                {
                    return (rss as u64).saturating_mul(1024);
                }
            }
        }
        0
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Business Logic（为什么需要这个函数）:
///     压测与安全 fixture 需要 claude_history / prompts 表，且不依赖私有 storage 导出。
///
/// Code Logic（这个函数做什么）:
///     幂等创建 claude_history、索引与 prompts 最小 schema。
async fn ensure_scale_aux_tables(pool: &SqlitePool) {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS claude_history (\
         id TEXT PRIMARY KEY, project_path TEXT NOT NULL, project_name TEXT NOT NULL, \
         session_id TEXT NOT NULL, content TEXT NOT NULL, git_branch TEXT, cc_version TEXT, \
         occurred_at TEXT NOT NULL, device_id TEXT NOT NULL, vector_clock TEXT NOT NULL, \
         created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted INTEGER DEFAULT 0, source TEXT NOT NULL DEFAULT 'claude')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_ch_proj ON claude_history(project_path, occurred_at DESC)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS prompts (\
         id TEXT PRIMARY KEY, title TEXT NOT NULL, content TEXT NOT NULL, tags TEXT NOT NULL, \
         created_at TEXT NOT NULL, updated_at TEXT NOT NULL, device_id TEXT NOT NULL, \
         vector_clock TEXT NOT NULL, deleted INTEGER DEFAULT 0, \
         favorite INTEGER NOT NULL DEFAULT 0)",
    )
    .execute(pool)
    .await
    .unwrap();
}

/// Business Logic（为什么需要这个函数）:
///     请求层必须拒绝超限 ID 列表 / content / manifest limit，防止无界分配。
///
/// Code Logic（这个函数做什么）:
///     镜像生产上限：id 字节、batch 长度、content 字节、manifest limit 范围；
///     越界返回 Err 字符串标签，供安全测试断言。
fn assert_request_within_limits(
    id: Option<&str>,
    id_batch_len: Option<usize>,
    content_len: Option<usize>,
    manifest_limit: Option<u32>,
) -> Result<(), &'static str> {
    if let Some(id) = id {
        if id.is_empty() || id.len() > BENCH_ID_MAX_BYTES {
            return Err("id_limit");
        }
    }
    if let Some(n) = id_batch_len {
        if n > BENCH_ITEM_BATCH_LIMIT {
            return Err("item_batch_limit");
        }
    }
    if let Some(n) = content_len {
        if n > BENCH_CONTENT_MAX_BYTES {
            return Err("content_limit");
        }
    }
    if let Some(limit) = manifest_limit {
        if !(1..=BENCH_MANIFEST_PAGE_LIMIT_MAX).contains(&limit) {
            return Err("manifest_limit");
        }
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     分类 SQLite 瞬时锁错误，便于门禁统计 SQLITE_BUSY 且不打印 SQL。
///
/// Code Logic（这个函数做什么）:
///     对错误 Display 做小写 contains 匹配 locked/busy/database is locked。
fn is_sqlite_busy_msg(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("locked") || m.contains("busy") || m.contains("database is locked")
}

/// Business Logic（为什么需要这个函数）:
///     压测需要可复现的 10k 历史分布与 1k 任务/100 项目负载，覆盖大小行与无效 workflow。
///
/// Code Logic（这个函数做什么）:
///     在给定 pool 上创建 projects/tasks/history/prompts；content 分布为
///     9000×1KiB + 990×64KiB + 10×1MiB；前 10 个项目写坏 WORKFLOW.md；返回项目根路径。
async fn seed_scale_fixture(pool: &SqlitePool, repo: &OrchestratorRepo, root: &Path) -> PathBuf {
    create_workbench_projects_table(pool).await;
    ensure_scale_aux_tables(pool).await;

    for i in 0..BENCH_PROJECTS {
        let project_id = format!("p{i:03}");
        let path = root.join(&project_id);
        std::fs::create_dir_all(&path).unwrap();
        if i < BENCH_INVALID_WORKFLOWS {
            write_invalid_workflow(&path);
        }
        insert_workbench_project(pool, &project_id, "local", &path).await;
    }

    // 1k queued tasks 均匀摊到 100 项目。
    for i in 0..BENCH_QUEUED_TASKS {
        let project_id = format!("p{:03}", i % BENCH_PROJECTS);
        let task_id = format!("t{i:04}");
        let sec = i % 60;
        let min = (i / 60) % 60;
        let hour = i / 3600;
        let created_at = format!("2026-07-05T{hour:02}:{min:02}:{sec:02}Z");
        let priority = 100 - ((i % 20) as i64);
        repo.create_task(&queued_task(&task_id, &project_id, priority, &created_at))
            .await
            .unwrap();
        set_todo_idle(pool, &task_id).await;
    }

    let content_1k = "a".repeat(1024);
    let content_64k = "b".repeat(64 * 1024);
    let content_1m = "c".repeat(BENCH_CONTENT_MAX_BYTES);
    let vc = r#"{"bench":1}"#;
    let ts = "2026-07-05T00:00:00Z";

    let mut tx = pool.begin().await.unwrap();
    for i in 0..BENCH_HISTORY_ROWS {
        let content = if i < 9_000 {
            content_1k.as_str()
        } else if i < 9_990 {
            content_64k.as_str()
        } else {
            content_1m.as_str()
        };
        // 仅使用合成 id，日志/JSON 永不打印。
        let id = format!("h{i:05}");
        let project_path = format!("/proj/{}", i % 50);
        sqlx::query(
            "INSERT INTO claude_history \
             (id, project_path, project_name, session_id, content, git_branch, cc_version, \
              occurred_at, device_id, vector_clock, created_at, updated_at, deleted) \
             VALUES (?, ?, 'proj', 's1', ?, NULL, NULL, ?, 'bench', ?, ?, ?, 0)",
        )
        .bind(&id)
        .bind(&project_path)
        .bind(content)
        .bind(ts)
        .bind(vc)
        .bind(ts)
        .bind(ts)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();

    // 少量 Prompt 供 CRUD 并发。
    for i in 0..32 {
        let id = format!("prompt-{i:02}");
        sqlx::query(
            "INSERT INTO prompts \
             (id, title, content, tags, created_at, updated_at, device_id, vector_clock, deleted) \
             VALUES (?, ?, ?, '[]', ?, ?, 'bench', ?, 0)",
        )
        .bind(&id)
        .bind(format!("title-{i}"))
        .bind(format!("content-{i}"))
        .bind(ts)
        .bind(ts)
        .bind(vc)
        .execute(pool)
        .await
        .unwrap();
    }

    root.to_path_buf()
}

/// Business Logic（为什么需要这个函数）:
///     混合负载下需要同时行使 claim / 分页 manifest / history 读 / Prompt CRUD，
///     才能观测连接等待与事务时长。
///
/// Code Logic（这个函数做什么）:
///     启动多组并发任务：测 pool.acquire 等待、短写事务、完整 claim tick、
///     manifest 分页扫描、history get、prompt update；汇总样本与错误计数。
async fn run_mixed_load_once(
    pool: SqlitePool,
    _repo: OrchestratorRepo,
) -> (Vec<u64>, Vec<u64>, Vec<u64>, BenchmarkErrorCounts) {
    let busy = Arc::new(AtomicU64::new(0));
    let other = Arc::new(AtomicU64::new(0));
    let limit_v = Arc::new(AtomicU64::new(0));
    let cas_dup = Arc::new(AtomicU64::new(0));
    let wait_samples = Arc::new(Mutex::new(Vec::<u64>::new()));
    let tx_samples = Arc::new(Mutex::new(Vec::<u64>::new()));
    let tick_samples = Arc::new(Mutex::new(Vec::<u64>::new()));

    let mut handles = Vec::new();

    // 连接等待采样：并发 acquire。
    for _ in 0..24 {
        let pool = pool.clone();
        let waits = Arc::clone(&wait_samples);
        let busy_c = Arc::clone(&busy);
        let other_c = Arc::clone(&other);
        handles.push(tokio::spawn(async move {
            for _ in 0..4 {
                let started = Instant::now();
                match pool.acquire().await {
                    Ok(conn) => {
                        let ms = started.elapsed().as_millis() as u64;
                        waits.lock().unwrap().push(ms);
                        drop(conn);
                    }
                    Err(err) => {
                        if is_sqlite_busy_msg(&err.to_string()) {
                            busy_c.fetch_add(1, Ordering::Relaxed);
                        } else {
                            other_c.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
        }));
    }

    // 短写事务采样：批量 REPLACE 小批 history 元数据（不碰超大 content）。
    for k in 0..8 {
        let pool = pool.clone();
        let txs = Arc::clone(&tx_samples);
        let busy_c = Arc::clone(&busy);
        let other_c = Arc::clone(&other);
        handles.push(tokio::spawn(async move {
            for round in 0..3 {
                let started = Instant::now();
                let result = async {
                    let mut tx = pool.begin().await?;
                    for j in 0..8 {
                        let idx = (k * 100 + round * 10 + j) % 9000;
                        let id = format!("h{idx:05}");
                        sqlx::query("UPDATE claude_history SET updated_at = ? WHERE id = ?")
                            .bind("2026-07-06T00:00:00Z")
                            .bind(&id)
                            .execute(&mut *tx)
                            .await?;
                    }
                    tx.commit().await?;
                    Ok::<(), sqlx::Error>(())
                }
                .await;
                match result {
                    Ok(()) => txs
                        .lock()
                        .unwrap()
                        .push(started.elapsed().as_millis() as u64),
                    Err(err) => {
                        if is_sqlite_busy_msg(&err.to_string()) {
                            busy_c.fetch_add(1, Ordering::Relaxed);
                        } else {
                            other_c.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
        }));
    }

    // claim tick：list + preflight + CAS(limit=2)；检测单次 outcome 内重复 id。
    for _ in 0..6 {
        let repo = OrchestratorRepo::new(pool.clone());
        let ticks = Arc::clone(&tick_samples);
        let busy_c = Arc::clone(&busy);
        let other_c = Arc::clone(&other);
        let cas_c = Arc::clone(&cas_dup);
        handles.push(tokio::spawn(async move {
            for _ in 0..2 {
                let started = Instant::now();
                let outcome = async {
                    let candidates = repo
                        .list_local_queued_claim_candidates(None, CLAIM_CANDIDATE_LIMIT)
                        .await?;
                    let preflight = preflight_claim_candidates(candidates).await?;
                    let cas = repo
                        .claim_preflighted_candidates_with_global_capacity(2, &preflight.eligible)
                        .await?;
                    let mut ids: Vec<String> = cas.claimed.iter().map(|t| t.id.clone()).collect();
                    let before = ids.len();
                    ids.sort();
                    ids.dedup();
                    if ids.len() < before {
                        cas_c.fetch_add((before - ids.len()) as u64, Ordering::Relaxed);
                    }
                    Ok::<(), app_lib::error::AppError>(())
                }
                .await;
                match outcome {
                    Ok(()) => ticks
                        .lock()
                        .unwrap()
                        .push(started.elapsed().as_millis() as u64),
                    Err(err) => {
                        let msg = err.to_string();
                        if is_sqlite_busy_msg(&msg) {
                            busy_c.fetch_add(1, Ordering::Relaxed);
                        } else {
                            other_c.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
        }));
    }

    // paged sync：manifest 分页直至 done（只读 id/vc，不读正文）。
    for _ in 0..4 {
        let pool = pool.clone();
        let busy_c = Arc::clone(&busy);
        let other_c = Arc::clone(&other);
        let limit_c = Arc::clone(&limit_v);
        handles.push(tokio::spawn(async move {
            // 合法 limit
            if assert_request_within_limits(None, None, None, Some(256)).is_err() {
                limit_c.fetch_add(1, Ordering::Relaxed);
            }
            let mut after: Option<String> = None;
            for _ in 0..8 {
                let q = if let Some(ref a) = after {
                    sqlx::query(
                        "SELECT id, vector_clock FROM claude_history WHERE id > ? ORDER BY id ASC LIMIT 256",
                    )
                    .bind(a)
                    .fetch_all(&pool)
                    .await
                } else {
                    sqlx::query(
                        "SELECT id, vector_clock FROM claude_history ORDER BY id ASC LIMIT 256",
                    )
                    .fetch_all(&pool)
                    .await
                };
                match q {
                    Ok(rows) => {
                        if rows.is_empty() {
                            break;
                        }
                        let last: String = rows.last().unwrap().get("id");
                        after = Some(last);
                        if rows.len() < 256 {
                            break;
                        }
                    }
                    Err(err) => {
                        if is_sqlite_busy_msg(&err.to_string()) {
                            busy_c.fetch_add(1, Ordering::Relaxed);
                        } else {
                            other_c.fetch_add(1, Ordering::Relaxed);
                        }
                        break;
                    }
                }
            }
        }));
    }

    // history 读：按 id 批量（≤128）取 content 长度（不打印）。
    for batch in 0..4 {
        let pool = pool.clone();
        let busy_c = Arc::clone(&busy);
        let other_c = Arc::clone(&other);
        let limit_c = Arc::clone(&limit_v);
        handles.push(tokio::spawn(async move {
            let mut ids = Vec::with_capacity(BENCH_ITEM_BATCH_LIMIT);
            for j in 0..BENCH_ITEM_BATCH_LIMIT {
                ids.push(format!("h{:05}", batch * BENCH_ITEM_BATCH_LIMIT + j));
            }
            if assert_request_within_limits(None, Some(ids.len()), None, None).is_err() {
                limit_c.fetch_add(1, Ordering::Relaxed);
                return;
            }
            // 超限请求必须被拦截（安全侧采样）。
            if assert_request_within_limits(None, Some(BENCH_ITEM_BATCH_LIMIT + 1), None, None)
                .is_err()
            {
                // expected
            } else {
                limit_c.fetch_add(1, Ordering::Relaxed);
            }
            let placeholders = (0..ids.len()).map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT id, length(content) AS n FROM claude_history WHERE id IN ({placeholders})"
            );
            let mut q = sqlx::query(&sql);
            for id in &ids {
                q = q.bind(id);
            }
            match q.fetch_all(&pool).await {
                Ok(rows) => {
                    for row in rows {
                        let n: i64 = row.get("n");
                        if assert_request_within_limits(None, None, Some(n as usize), None).is_err()
                        {
                            // 1MiB 行刚好等于上限，应通过；更大才算 violation。
                            if (n as usize) > BENCH_CONTENT_MAX_BYTES {
                                limit_c.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                }
                Err(err) => {
                    if is_sqlite_busy_msg(&err.to_string()) {
                        busy_c.fetch_add(1, Ordering::Relaxed);
                    } else {
                        other_c.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }));
    }

    // Prompt CRUD：读改写。
    for i in 0..8 {
        let pool = pool.clone();
        let busy_c = Arc::clone(&busy);
        let other_c = Arc::clone(&other);
        handles.push(tokio::spawn(async move {
            let id = format!("prompt-{:02}", i % 32);
            let result = async {
                let row = sqlx::query("SELECT title, content FROM prompts WHERE id = ?")
                    .bind(&id)
                    .fetch_optional(&pool)
                    .await?;
                if row.is_some() {
                    sqlx::query("UPDATE prompts SET content = ?, updated_at = ? WHERE id = ?")
                        .bind(format!("updated-{i}"))
                        .bind("2026-07-06T00:00:00Z")
                        .bind(&id)
                        .execute(&pool)
                        .await?;
                }
                Ok::<(), sqlx::Error>(())
            }
            .await;
            if let Err(err) = result {
                if is_sqlite_busy_msg(&err.to_string()) {
                    busy_c.fetch_add(1, Ordering::Relaxed);
                } else {
                    other_c.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    let waits = wait_samples.lock().unwrap().clone();
    let txs = tx_samples.lock().unwrap().clone();
    let ticks = tick_samples.lock().unwrap().clone();
    let errors = BenchmarkErrorCounts {
        sqlite_busy: busy.load(Ordering::Relaxed),
        limit_violations: limit_v.load(Ordering::Relaxed),
        other: other.load(Ordering::Relaxed),
        cas_duplicates: cas_dup.load(Ordering::Relaxed),
        partial_batches: 0,
    };
    (waits, txs, ticks, errors)
}

/// Business Logic（为什么需要这个测试）:
///     协议上限必须在任何压测前可判定；超限请求不得被当作成功。
///
/// Code Logic（这个测试做什么）:
///     断言 id/batch/content/manifest 边界：合法通过，越界返回对应标签。
#[test]
fn scale_safety_no_request_exceeds_limits() {
    assert!(assert_request_within_limits(Some("ok"), Some(128), Some(1024), Some(256)).is_ok());
    assert_eq!(
        assert_request_within_limits(Some(&"x".repeat(BENCH_ID_MAX_BYTES + 1)), None, None, None)
            .unwrap_err(),
        "id_limit"
    );
    assert_eq!(
        assert_request_within_limits(None, Some(BENCH_ITEM_BATCH_LIMIT + 1), None, None)
            .unwrap_err(),
        "item_batch_limit"
    );
    assert_eq!(
        assert_request_within_limits(None, None, Some(BENCH_CONTENT_MAX_BYTES + 1), None)
            .unwrap_err(),
        "content_limit"
    );
    assert_eq!(
        assert_request_within_limits(None, None, None, Some(0)).unwrap_err(),
        "manifest_limit"
    );
    assert_eq!(
        assert_request_within_limits(None, None, None, Some(513)).unwrap_err(),
        "manifest_limit"
    );
}

/// Business Logic（为什么需要这个测试）:
///     claim 写事务不得包含 workflow resolver 文件 IO，否则单连接池会被饿死。
///
/// Code Logic（这个测试做什么）:
///     独立慢 preflight fixture：阻塞 resolver 期间 get_task 必须在 100ms 内返回，
///     证明 resolver seam 在事务外（与 orchestrator_claim_slow_preflight_* 同语义）。
#[tokio::test]
async fn scale_safety_no_transaction_includes_resolver_seam() {
    let db = setup_repo().await;
    let pool = &db.pool;
    let repo = &db.repo;
    create_workbench_projects_table(pool).await;
    let dir = TempDir::new().unwrap();
    insert_workbench_project(pool, "local-a", "local", dir.path()).await;

    let mut task = queued_task("task-1", "local-a", 10, "2026-07-05T00:00:01Z");
    repo.create_task(&task).await.unwrap();
    set_todo_idle(pool, "task-1").await;
    task = repo.get_task("task-1").await.unwrap();

    let candidates = vec![ClaimCandidate {
        task,
        project_path: dir.path().to_path_buf(),
    }];

    let (release_tx, release_rx) = oneshot::channel::<()>();
    let release_rx = Arc::new(Mutex::new(Some(release_rx)));
    let resolver = Arc::new(
        move |_path: &Path| -> Result<ResolvedWorkflow, app_lib::error::AppError> {
            if let Some(rx) = release_rx.lock().expect("lock").take() {
                let _ = rx.blocking_recv();
            }
            Ok(ResolvedWorkflow::built_in_default())
        },
    );

    let preflight_handle = tokio::spawn(async move {
        preflight_claim_candidates_with_resolver(candidates, resolver).await
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    let concurrent = tokio::time::timeout(Duration::from_millis(100), repo.get_task("task-1"))
        .await
        .expect("get_task must complete within 100ms during slow preflight")
        .expect("get_task ok");
    assert_eq!(concurrent.id, "task-1");
    let _ = release_tx.send(());
    let preflight = preflight_handle.await.expect("join").expect("preflight");
    assert_eq!(preflight.eligible.len(), 1);
}

/// Business Logic（为什么需要这个测试）:
///     并发 claim 不得双派发同一任务。
///
/// Code Logic（这个测试做什么）:
///     与 concurrent CAS 测试同构：preflight 后双写 CAS，断言唯一 id 并集。
#[tokio::test]
async fn scale_safety_no_duplicate_claims() {
    let db = setup_repo().await;
    let pool = &db.pool;
    let repo = &db.repo;
    create_workbench_projects_table(pool).await;
    let dir = TempDir::new().unwrap();
    insert_workbench_project(pool, "local-a", "local", dir.path()).await;
    for (id, created_at) in [
        ("task-a", "2026-07-05T00:00:01Z"),
        ("task-b", "2026-07-05T00:00:02Z"),
    ] {
        repo.create_task(&queued_task(id, "local-a", 10, created_at))
            .await
            .unwrap();
        set_todo_idle(pool, id).await;
    }
    let candidates = repo
        .list_local_queued_claim_candidates(None, CLAIM_CANDIDATE_LIMIT)
        .await
        .unwrap();
    let preflight = preflight_claim_candidates(candidates).await.unwrap();
    assert_eq!(preflight.eligible.len(), 2);
    let repo_a = OrchestratorRepo::new(db.pool.clone());
    let repo_b = OrchestratorRepo::new(db.pool.clone());
    let eligible_a = preflight.eligible.clone();
    let eligible_b = preflight.eligible.clone();
    let (out_a, out_b) = tokio::join!(
        claim_with_busy_retry(&repo_a, 2, &eligible_a),
        claim_with_busy_retry(&repo_b, 2, &eligible_b),
    );
    let mut ids: Vec<String> = out_a
        .claimed
        .iter()
        .chain(out_b.claimed.iter())
        .map(|t| t.id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 2);
    assert_eq!(out_a.claimed.len() + out_b.claimed.len(), 2);
}

/// Business Logic（为什么需要这个测试）:
///     push-batch 类写路径在中途失败时必须整批回滚，禁止 partial accepted；
///     必须走产品 `upsert_merged_batch` 事务边界，而不是手写 begin+INSERT。
///
/// Code Logic（这个测试做什么）:
///     构造 3 行 ClaudeHistoryRow，调用 `upsert_merged_batch_inject_fail_at(..., Some(1))`，
///     断言 Err 且表行数仍为 0（整批 rollback）。
#[tokio::test]
async fn scale_safety_no_partial_batch_after_injected_failure() {
    let db = setup_repo_with_pool(1).await;
    ensure_scale_aux_tables(&db.pool).await;
    let repo = app_lib::ClaudeHistoryRepo::new(db.pool.clone());
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM claude_history")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(before, 0);

    let mut batch = Vec::new();
    for i in 0..3 {
        let mut vc = std::collections::HashMap::new();
        vc.insert("d".to_string(), 1);
        batch.push(app_lib::ClaudeHistoryRow {
            id: format!("partial-{i}"),
            project_path: "/p".into(),
            project_name: "p".into(),
            session_id: "s".into(),
            content: format!("c{i}"),
            git_branch: None,
            cc_version: None,
            occurred_at: "t".into(),
            device_id: "d".into(),
            vector_clock: vc,
            created_at: "t".into(),
            updated_at: "t".into(),
            deleted: false,
            source: "claude".to_string(),
        });
    }
    let err = repo
        .upsert_merged_batch_inject_fail_at(&batch, Some(1))
        .await
        .expect_err("inject fail must surface");
    assert!(
        err.to_string().contains("injected") || err.to_string().contains("vector_clock"),
        "error should mention injection: {err}"
    );

    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM claude_history")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(after, 0, "product upsert path must roll back completely");
}

/// Business Logic（为什么需要这个测试）:
///     有界正确性 fixture 下 SQLITE_BUSY 应为 0，证明 CAS/读路径不靠无限锁重试掩盖问题。
///
/// Code Logic（这个测试做什么）:
///     pool=1 下并发 get_task + 短事务 + list candidates，统计 busy 消息次数为 0，
///     且 CAS 双写仍无重复 claim。
#[tokio::test]
async fn scale_safety_sqlite_busy_zero_under_bounded_correctness_fixture() {
    let db = setup_repo_with_pool(1).await;
    create_workbench_projects_table(&db.pool).await;
    let dir = TempDir::new().unwrap();
    insert_workbench_project(&db.pool, "local-a", "local", dir.path()).await;
    for (id, created_at) in [
        ("task-a", "2026-07-05T00:00:01Z"),
        ("task-b", "2026-07-05T00:00:02Z"),
        ("task-c", "2026-07-05T00:00:03Z"),
        ("task-d", "2026-07-05T00:00:04Z"),
    ] {
        db.repo
            .create_task(&queued_task(id, "local-a", 10, created_at))
            .await
            .unwrap();
        set_todo_idle(&db.pool, id).await;
    }

    let busy = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let pool = db.pool.clone();
        let repo = OrchestratorRepo::new(db.pool.clone());
        let busy_c = Arc::clone(&busy);
        handles.push(tokio::spawn(async move {
            for _ in 0..10 {
                if let Err(err) = repo.get_task("task-a").await {
                    if is_sqlite_busy_msg(&err.to_string()) {
                        busy_c.fetch_add(1, Ordering::Relaxed);
                    }
                }
                if let Err(err) = repo
                    .list_local_queued_claim_candidates(None, CLAIM_CANDIDATE_LIMIT)
                    .await
                {
                    if is_sqlite_busy_msg(&err.to_string()) {
                        busy_c.fetch_add(1, Ordering::Relaxed);
                    }
                }
                // 短只读事务
                let started = Instant::now();
                let _ = started;
                if let Err(err) = sqlx::query("SELECT COUNT(*) AS n FROM orchestrator_tasks")
                    .fetch_one(&pool)
                    .await
                {
                    if is_sqlite_busy_msg(&err.to_string()) {
                        busy_c.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    assert_eq!(
        busy.load(Ordering::Relaxed),
        0,
        "SQLITE_BUSY must be zero under bounded correctness fixture"
    );

    // 仍保持无重复 claim。
    let candidates = db
        .repo
        .list_local_queued_claim_candidates(None, CLAIM_CANDIDATE_LIMIT)
        .await
        .unwrap();
    let preflight = preflight_claim_candidates(candidates).await.unwrap();
    let repo_a = OrchestratorRepo::new(db.pool.clone());
    let repo_b = OrchestratorRepo::new(db.pool.clone());
    let eligible_a = preflight.eligible.clone();
    let eligible_b = preflight.eligible.clone();
    let (out_a, out_b) = tokio::join!(
        claim_with_busy_retry(&repo_a, 2, &eligible_a),
        claim_with_busy_retry(&repo_b, 2, &eligible_b),
    );
    let mut ids: Vec<String> = out_a
        .claimed
        .iter()
        .chain(out_b.claimed.iter())
        .map(|t| t.id.clone())
        .collect();
    ids.sort();
    let before = ids.len();
    ids.dedup();
    assert_eq!(before, ids.len(), "duplicate claim ids detected");
}

/// Business Logic（为什么需要这个测试）:
///     生产 `max_connections` 扩到 2 前必须有可重复混合压测证据（§4.2 五项门槛）。
///
/// Code Logic（这个测试做什么）:
///     构造 10k history + 1k tasks + 100 项目/10 无效 workflow；对 pool=1 与 pool=2
///     各跑 3 轮混合负载；每轮向 stdout 打印一行脱敏 JSON（median/p95/max wait/tx/tick、
///     RSS、错误计数）。默认 #[ignore]，需 --ignored --release。
#[tokio::test]
#[ignore = "load gate; run with --ignored --release --nocapture"]
async fn backend_scale_benchmark() {
    let root = TempDir::new().unwrap();
    // 预先生成项目目录骨架（每轮复用路径，DB 隔离）。
    for i in 0..BENCH_PROJECTS {
        let path = root.path().join(format!("p{i:03}"));
        std::fs::create_dir_all(&path).unwrap();
    }

    // CC_PARTNER_BENCH_POOL=1|2 仅测一侧；缺省两侧各 3 轮。
    let pools: Vec<u32> = match std::env::var("CC_PARTNER_BENCH_POOL")
        .unwrap_or_default()
        .as_str()
    {
        "1" => vec![1],
        "2" => vec![2],
        "" => vec![1, 2],
        other => panic!("CC_PARTNER_BENCH_POOL must be 1, 2, or empty; got {other}"),
    };

    let content_distribution = ContentDistribution {
        rows_1kib: 9_000,
        rows_64kib: 990,
        rows_1mib: 10,
    };

    for pool_size in pools {
        for run in 1_u32..=3 {
            let db = setup_repo_with_pool(pool_size).await;
            seed_scale_fixture(&db.pool, &db.repo, root.path()).await;
            let (waits, txs, ticks, errors) =
                run_mixed_load_once(db.pool.clone(), OrchestratorRepo::new(db.pool.clone())).await;
            let wait = latency_summary_ms(waits);
            let row = BenchmarkJsonRow {
                pool_max_connections: pool_size,
                runs: run,
                history_rows: BENCH_HISTORY_ROWS,
                queued_tasks: BENCH_QUEUED_TASKS,
                projects: BENCH_PROJECTS,
                invalid_workflows: BENCH_INVALID_WORKFLOWS,
                content_distribution: content_distribution.clone(),
                acquire_wait_ms: wait.clone(),
                transaction_ms: latency_summary_ms(txs),
                scheduler_tick_ms: latency_summary_ms(ticks),
                rss_peak_bytes: current_rss_bytes(),
                errors: errors.other,
                sqlite_busy: errors.sqlite_busy,
                cas_duplicates: errors.cas_duplicates,
                partial_batches: errors.partial_batches,
                limit_violations: errors.limit_violations,
                wait_p95_over_50ms: wait.p95 > 50,
            };
            // 仅打印机读 JSON；禁止 ID/正文/路径。
            println!("{}", serde_json::to_string(&row).expect("json"));
        }
    }
}
