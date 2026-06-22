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
