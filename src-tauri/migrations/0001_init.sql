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

-- workbench_sessions 表：工作台终端 tab 元数据，运行期 PTY/tmux attach 在启动时重建
CREATE TABLE IF NOT EXISTS workbench_sessions (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    command TEXT NOT NULL,
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
