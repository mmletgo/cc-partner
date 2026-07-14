//! S5 Backend Scale 集成测试：S5 后端规模化集成测试：claim 三阶段、慢 workflow、CAS 竞态、扫描公平性与 CC History mixed-version。
//!
//! Business Logic（为什么需要这个测试文件）:
//!     生产单连接池下，事务内 WORKFLOW.md IO 会饿死其它查询；CC History 分页协议
//!     必须在 new↔new / new↔legacy / 畸形响应下行为正确。本文件锁死这些边界。
//!
//! Code Logic（这个文件做什么）:
//!     1) orchestrator_claim_*：内存/文件 SQLite fixture 覆盖 preflight/CAS/cursor
//!     2) cc_history_mixed_version_*：委托 app_lib::mixed_version_harness
use app_lib::orchestrator::claim::{
    preflight_claim_candidates, preflight_claim_candidates_with_resolver, ClaimCandidate,
    ClaimScanCursor, CLAIM_CANDIDATE_LIMIT,
};
use app_lib::orchestrator::models::{
    OrchestratorRunState, OrchestratorTaskRow, OrchestratorTaskStatus, OrchestratorWorkflowState,
};
use app_lib::orchestrator::repo::OrchestratorRepo;
use app_lib::orchestrator::workflow::ResolvedWorkflow;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::Path;
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
///     在临时目录创建文件型 SQLite（多连接共享同一库），busy_timeout=5s + max_connections=2，
///     初始化 schema 后返回 TestDb。
async fn setup_repo() -> TestDb {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("scale.db");
    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
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
    create_workbench_projects_table(&pool).await;
    let dir = TempDir::new().unwrap();
    insert_workbench_project(&pool, "local-a", "local", dir.path()).await;

    let mut task = queued_task("task-1", "local-a", 10, "2026-07-05T00:00:01Z");
    repo.create_task(&task).await.unwrap();
    set_todo_idle(&pool, "task-1").await;
    task = repo.get_task("task-1").await.unwrap();

    let candidates = vec![ClaimCandidate {
        task,
        project_path: dir.path().to_path_buf(),
    }];

    let (release_tx, release_rx) = oneshot::channel::<()>();
    let release_rx = Arc::new(Mutex::new(Some(release_rx)));
    let resolver: Arc<
        dyn Fn(&Path) -> Result<ResolvedWorkflow, app_lib::error::AppError> + Send + Sync,
    > = Arc::new(move |_path: &Path| {
        if let Some(rx) = release_rx.lock().expect("lock").take() {
            let _ = rx.blocking_recv();
        }
        Ok(ResolvedWorkflow::built_in_default())
    });

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
    let preflight = preflight_handle
        .await
        .expect("join")
        .expect("preflight");
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
    create_workbench_projects_table(&pool).await;
    let dir = TempDir::new().unwrap();
    // 无 WORKFLOW.md → built-in Todo/Rework active。
    insert_workbench_project(&pool, "local-a", "local", dir.path()).await;

    for (id, created_at) in [
        ("task-a", "2026-07-05T00:00:01Z"),
        ("task-b", "2026-07-05T00:00:02Z"),
    ] {
        repo.create_task(&queued_task(id, "local-a", 10, created_at))
            .await
            .unwrap();
        set_todo_idle(&pool, id).await;
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

    let mut ids: Vec<String> = a
        .iter()
        .chain(b.iter())
        .map(|t| t.id.clone())
        .collect();
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
///     前 256 个无效 workflow 项目不得永久饿死后继合法任务；cursor 满窗推进，触尾回绕。
///
/// Code Logic（这个测试做什么）:
///     256 个无效项目各 1 任务 + 1 个合法项目任务；第一页 preflight eligible 为空且 exhausted；
///     用 next_cursor 再 list+preflight 必须拿到合法任务；再翻一页为空后 cursor 应回绕语义由调用方置 None。
#[tokio::test]
async fn orchestrator_claim_cursor_skips_invalid_window_then_wraps() {
    let db = setup_repo().await;
    let pool = &db.pool;
    let repo = &db.repo;
    create_workbench_projects_table(&pool).await;
    let root = TempDir::new().unwrap();

    // 256 无效项目（每个独立 path + 坏 WORKFLOW.md）
    for i in 0..CLAIM_CANDIDATE_LIMIT {
        let project_id = format!("bad-{i:04}");
        let path = root.path().join(&project_id);
        write_invalid_workflow(&path);
        insert_workbench_project(&pool, &project_id, "local", &path).await;
        let task_id = format!("bad-task-{i:04}");
        let created_at = format!(
            "2026-07-05T00:{:02}:{:02}Z",
            (i / 60) as u32,
            (i % 60) as u32
        );
        repo.create_task(&queued_task(
            &task_id,
            &project_id,
            100,
            &created_at,
        ))
        .await
        .unwrap();
        set_todo_idle(&pool, &task_id).await;
    }

    // 合法项目：无 WORKFLOW.md → built-in default
    let good_path = root.path().join("good");
    std::fs::create_dir_all(&good_path).unwrap();
    insert_workbench_project(&pool, "good", "local", &good_path).await;
    repo.create_task(&queued_task(
        "good-task",
        "good",
        1,
        "2026-07-05T01:00:00Z",
    ))
    .await
    .unwrap();
    set_todo_idle(&pool, "good-task").await;

    let page1 = repo
        .list_local_queued_claim_candidates(None, CLAIM_CANDIDATE_LIMIT)
        .await
        .unwrap();
    assert_eq!(page1.len(), CLAIM_CANDIDATE_LIMIT as usize);
    let preflight1 = preflight_claim_candidates(page1).await.unwrap();
    assert!(
        preflight1.eligible.is_empty(),
        "first window is all invalid projects"
    );
    assert!(preflight1.exhausted);
    let cursor = preflight1
        .next_cursor
        .expect("exhausted window must produce next_cursor");

    let page2 = repo
        .list_local_queued_claim_candidates(Some(&cursor), CLAIM_CANDIDATE_LIMIT)
        .await
        .unwrap();
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0].task.id, "good-task");
    let preflight2 = preflight_claim_candidates(page2).await.unwrap();
    assert_eq!(preflight2.eligible.len(), 1);
    assert_eq!(preflight2.eligible[0].task.id, "good-task");
    assert!(!preflight2.exhausted);

    let claimed = repo
        .claim_preflighted_candidates_with_global_capacity(1, &preflight2.eligible)
        .await
        .unwrap()
        .claimed;
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, "good-task");
    assert_eq!(claimed[0].status, OrchestratorTaskStatus::Preparing);

    // 触尾后再翻应为空（调用方应把 cursor 置 None 回绕）。
    let after = ClaimScanCursor::from_task(&claimed[0]);
    // claimed row is Preparing; list only Queued — use preflight2 cursor from good-task candidate.
    let tail_cursor = preflight2.next_cursor.expect("good page cursor");
    let page3 = repo
        .list_local_queued_claim_candidates(Some(&tail_cursor), CLAIM_CANDIDATE_LIMIT)
        .await
        .unwrap();
    assert!(page3.is_empty(), "after tail page must be empty for wrap");
    let _ = after; // silence if unused across refactors
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
    create_workbench_projects_table(&pool).await;
    let dir = TempDir::new().unwrap();
    insert_workbench_project(&pool, "local-a", "local", dir.path()).await;
    repo.create_task(&queued_task(
        "task-1",
        "local-a",
        5,
        "2026-07-05T00:00:01Z",
    ))
    .await
    .unwrap();
    set_todo_idle(&pool, "task-1").await;

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
