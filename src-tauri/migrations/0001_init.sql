-- 0001_init.sql — 初始化 schema（对照 Python storage/database.py）
-- 全部 CREATE TABLE IF NOT EXISTS，对已有旧库是无操作，保证用户数据兼容。

-- prompts 表：Prompt 实体
-- tags / vector_clock 为 JSON TEXT（与 Python json.dumps(ensure_ascii=False) 互通）
-- created_at / updated_at 为 ISO 字符串（可能带/不带时区偏移，读取时透传）
CREATE TABLE IF NOT EXISTS prompts (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    tags TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    device_id TEXT NOT NULL,
    vector_clock TEXT NOT NULL,
    deleted INTEGER DEFAULT 0
);

-- transfer_history 表：文件传输历史记录（M5 完整使用，M1 先建表保兼容）
CREATE TABLE IF NOT EXISTS transfer_history (
    id TEXT PRIMARY KEY,
    filename TEXT NOT NULL,
    file_path TEXT NOT NULL,
    size INTEGER NOT NULL,
    sha256 TEXT NOT NULL,
    direction TEXT NOT NULL,
    peer_device_id TEXT NOT NULL,
    status TEXT NOT NULL,
    transferred_bytes INTEGER DEFAULT 0,
    created_at TEXT NOT NULL,
    completed_at TEXT
);

-- scratchpad 表：速记本多页面文本
-- 旧默认页 id 恒为 "scratchpad"，新页面使用 UUID；清空内容是 content=""，删除页面是 deleted=1。
CREATE TABLE IF NOT EXISTS scratchpad (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL DEFAULT '速记本',
    content TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    device_id TEXT NOT NULL,
    vector_clock TEXT NOT NULL,
    deleted INTEGER DEFAULT 0
);

-- workbench_projects 表：工作台最近项目记录
CREATE TABLE IF NOT EXISTS workbench_projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    device_id TEXT NOT NULL,
    device_name TEXT NOT NULL,
    path TEXT NOT NULL,
    last_opened_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- workbench_worktrees 表：工作台项目下的 Git worktree 元数据；Git 状态运行期查询，不落库
CREATE TABLE IF NOT EXISTS workbench_worktrees (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    branch TEXT,
    base_branch TEXT,
    path TEXT NOT NULL,
    is_main INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- workbench_sessions 表：工作台终端 tab 元数据，运行期 PTY/tmux attach 在启动时重建
CREATE TABLE IF NOT EXISTS workbench_sessions (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    worktree_id TEXT,
    name TEXT NOT NULL,
    command TEXT NOT NULL,
    cwd TEXT,
    status TEXT NOT NULL,
    cols INTEGER NOT NULL,
    rows INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    exited_at TEXT,
    exit_code INTEGER,
    backend TEXT NOT NULL,
    backend_id TEXT,
    backend_window_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- workbench_browser_targets 表：项目/worktree 最近一次浏览器预览目标 URL
CREATE TABLE IF NOT EXISTS workbench_browser_targets (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT NOT NULL,
    worktree_id TEXT,
    worktree_key TEXT GENERATED ALWAYS AS (IFNULL(worktree_id, '')) STORED,
    target_url TEXT NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    UNIQUE(project_id, worktree_key)
);

CREATE INDEX IF NOT EXISTS idx_workbench_browser_targets_project
    ON workbench_browser_targets(project_id, updated_at DESC);

-- orchestrator_tasks 表：Orchestrator 权威任务队列
CREATE TABLE IF NOT EXISTS orchestrator_tasks (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    title TEXT NOT NULL,
    goal TEXT NOT NULL,
    acceptance_criteria TEXT NOT NULL,
    status TEXT NOT NULL,
    workflow_state TEXT NOT NULL DEFAULT 'backlog',
    run_state TEXT NOT NULL DEFAULT 'idle',
    attempt_phase TEXT,
    source TEXT NOT NULL DEFAULT 'internal',
    external_id TEXT,
    external_identifier TEXT,
    external_url TEXT,
    external_state TEXT,
    external_labels_json TEXT,
    runner_provider TEXT,
    claude_session_id TEXT,
    transcript_path TEXT,
    runtime_started_at TEXT,
    last_activity_at TEXT,
    last_runtime_event TEXT,
    last_runtime_message TEXT,
    priority INTEGER NOT NULL DEFAULT 0,
    branch_name TEXT,
    worktree_id TEXT,
    session_id TEXT,
    blocked_reason TEXT,
    attempt INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_orchestrator_tasks_project_status
    ON orchestrator_tasks(project_id, status, priority, created_at);

CREATE INDEX IF NOT EXISTS idx_orchestrator_tasks_status
    ON orchestrator_tasks(status, priority, created_at);

-- orchestrator_project_config 表：历史项目级策略，仅保留展示/调试，运行时读取 AppConfig.orchestrator
CREATE TABLE IF NOT EXISTS orchestrator_project_config (
    project_id TEXT PRIMARY KEY,
    enabled INTEGER NOT NULL DEFAULT 0,
    max_concurrent_tasks INTEGER NOT NULL DEFAULT 1,
    branch_prefix TEXT NOT NULL DEFAULT 'agent',
    verification_commands_json TEXT NOT NULL DEFAULT '[]',
    auto_commit INTEGER NOT NULL DEFAULT 1,
    auto_push_task_branch INTEGER NOT NULL DEFAULT 1,
    auto_merge_to_main INTEGER NOT NULL DEFAULT 1,
    auto_push_main INTEGER NOT NULL DEFAULT 1,
    retry_limit INTEGER NOT NULL DEFAULT 0,
    retain_worktree_on_done INTEGER NOT NULL DEFAULT 0,
    retain_worktree_on_blocked INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- orchestrator_task_events 表：任务生命周期事件
CREATE TABLE IF NOT EXISTS orchestrator_task_events (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    message TEXT NOT NULL,
    payload_json TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_orchestrator_task_events_task
    ON orchestrator_task_events(task_id, created_at);

-- orchestrator_task_evidence 表：验证/交付 evidence
CREATE TABLE IF NOT EXISTS orchestrator_task_evidence (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    summary TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_orchestrator_task_evidence_task
    ON orchestrator_task_evidence(task_id, created_at);

-- orchestrator_task_attempts 表：Runner attempt 与 worktree/session 映射
CREATE TABLE IF NOT EXISTS orchestrator_task_attempts (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    attempt INTEGER NOT NULL,
    worktree_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    prompt TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    completed_at TEXT,
    UNIQUE(task_id, attempt)
);

CREATE INDEX IF NOT EXISTS idx_orchestrator_task_attempts_session
    ON orchestrator_task_attempts(session_id, status);

-- orchestrator_remote_outbox 表：远端项目离线创建任务的本机待投递队列
-- status: pending/sending/mirrored/failed；sending 由 dispatcher lease 过期后恢复 pending
CREATE TABLE IF NOT EXISTS orchestrator_remote_outbox (
    id TEXT PRIMARY KEY,
    device_id TEXT NOT NULL,
    device_name TEXT NOT NULL,
    remote_project_path TEXT NOT NULL,
    remote_project_id TEXT,
    request_json TEXT NOT NULL,
    status TEXT NOT NULL,
    remote_task_id TEXT,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    sent_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_orchestrator_remote_outbox_status
    ON orchestrator_remote_outbox(status, updated_at, device_id);

CREATE INDEX IF NOT EXISTS idx_orchestrator_remote_outbox_project
    ON orchestrator_remote_outbox(device_id, remote_project_path, status);

-- orchestrator_remote_task_mirrors 表：远端权威任务的本机展示快照，不能被本机 scheduler/验证/交付消费
CREATE TABLE IF NOT EXISTS orchestrator_remote_task_mirrors (
    id TEXT PRIMARY KEY,
    device_id TEXT NOT NULL,
    device_name TEXT NOT NULL,
    remote_project_id TEXT NOT NULL,
    remote_project_path TEXT NOT NULL,
    remote_task_id TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    last_synced_at TEXT NOT NULL,
    UNIQUE(device_id, remote_task_id)
);

CREATE INDEX IF NOT EXISTS idx_orchestrator_remote_task_mirrors_project
    ON orchestrator_remote_task_mirrors(device_id, remote_project_id, last_synced_at);

-- orchestrator_remote_task_create_requests 表：owning device 上的远端 create 幂等键
-- clientRequestId 重复到达时直接返回第一次写入的 task_id，避免响应超时后的重复任务。
CREATE TABLE IF NOT EXISTS orchestrator_remote_task_create_requests (
    request_id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- sync_request_ledger 表：Prompt/SSH/Scratchpad v2 push-batch 幂等 outcome ledger
-- 键 UNIQUE(claimed_device_id, domain, client_request_id)；claimed_device_id 仅为收敛标签，非认证。
-- 同 key/同 payload_hash 返回原 outcome 且不重复 apply；同 key/不同 hash 返回 conflict。
-- 实际建表由 backend/runtime.rs::init_db → SyncRequestLedgerRepo::ensure_schema 执行。
CREATE TABLE IF NOT EXISTS sync_request_ledger (
    claimed_device_id TEXT NOT NULL,
    domain TEXT NOT NULL,
    client_request_id TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    outcome_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(claimed_device_id, domain, client_request_id)
);
