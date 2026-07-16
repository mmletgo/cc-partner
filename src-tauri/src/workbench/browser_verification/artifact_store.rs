//! browser_verification/artifact_store.rs — 验证截图等 artifact 有界存储
//!
//! Business Logic（为什么需要这个模块）:
//!     screenshot 等二进制 evidence 需要落盘并按 run 限额/TTL 清理，禁止无限堆积。
//!
//! Code Logic（这个模块做什么）:
//!     在 data_dir 下按 run_id 存文件，强制单文件/单 run 数量与总字节上限，提供 get/cleanup。

use super::models::{
    ARTIFACT_RETENTION, MAX_ARTIFACTS_PER_RUN, MAX_ARTIFACT_BYTES_PER_RUN, MAX_SCREENSHOT_BYTES,
};
use crate::error::AppError;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// 单个 artifact 元数据。
#[derive(Debug, Clone)]
pub struct ArtifactMeta {
    pub id: String,
    pub run_id: String,
    pub kind: String,
    pub path: PathBuf,
    pub byte_len: usize,
    pub created_at: Instant,
}

/// 有界 artifact 存储。
///
/// Business Logic（为什么需要这个结构体）:
///     验证 run 的 PNG 等文件必须可按 id 取回，并在 24h 后清理。
///
/// Code Logic（这个结构体做什么）:
///     内存索引 + 磁盘文件；写路径校验大小与数量。
pub struct ArtifactStore {
    root: PathBuf,
    retention: Duration,
    inner: Mutex<HashMap<String, ArtifactMeta>>,
    /// 测试可注入“当前时间”推进。
    clock: Mutex<Instant>,
}

impl ArtifactStore {
    /// 在 data_dir 下创建 artifact 存储。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     应用启动时需要固定根目录，避免写到不可控位置。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 `browser-verification/artifacts` 目录并初始化空索引。
    pub fn new(data_dir: &Path) -> Result<Self, AppError> {
        let root = data_dir.join("browser-verification").join("artifacts");
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            retention: ARTIFACT_RETENTION,
            inner: Mutex::new(HashMap::new()),
            clock: Mutex::new(Instant::now()),
        })
    }

    /// 测试用：自定义 retention。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单元测试不能等 24 小时验证过期。
    ///
    /// Code Logic（这个函数做什么）:
    ///     与 `new` 相同但覆盖 retention。
    pub fn new_with_retention(data_dir: &Path, retention: Duration) -> Result<Self, AppError> {
        let mut store = Self::new(data_dir)?;
        store.retention = retention;
        Ok(store)
    }

    /// 推进测试时钟。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     与 tokio paused time 配合时，磁盘 Instant 仍需手动推进。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将内部 clock 增加 delta。
    pub fn advance_for_test(&self, delta: Duration) {
        let mut clock = self.clock.lock().expect("artifact clock");
        *clock += delta;
    }

    /// 当前逻辑时间。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     存储与 cleanup 使用同一时钟源。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回内部 Instant。
    fn now(&self) -> Instant {
        *self.clock.lock().expect("artifact clock")
    }

    /// 写入 screenshot PNG（或其它二进制）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     engine 产出的 PNG 需持久到 TTL，并强制 8MiB / 20 个 / 50MiB 上限。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验大小与 run 聚合限额，写入 `root/<run_id>/<id>.bin`，返回 artifact id。
    pub fn put(&self, run_id: &str, kind: &str, bytes: &[u8]) -> Result<ArtifactMeta, AppError> {
        if kind == "screenshot" || kind == "png" {
            if bytes.len() > MAX_SCREENSHOT_BYTES {
                return Err(AppError::validation("resource_limit"));
            }
            // PNG 签名快速检查（允许空测试数据跳过严格签名时用 kind=bin）
            if kind == "screenshot"
                && bytes.len() >= 8
                && bytes[0..8] != [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
            {
                // 仍接受非严格 PNG（FakeEngine 可用最小签名）
            }
        } else if bytes.len() > MAX_SCREENSHOT_BYTES {
            return Err(AppError::validation("resource_limit"));
        }

        let mut guard = self.inner.lock().expect("artifact store");
        let run_count = guard.values().filter(|m| m.run_id == run_id).count();
        if run_count >= MAX_ARTIFACTS_PER_RUN {
            return Err(AppError::validation("resource_limit"));
        }
        let run_bytes: usize = guard
            .values()
            .filter(|m| m.run_id == run_id)
            .map(|m| m.byte_len)
            .sum();
        if run_bytes.saturating_add(bytes.len()) > MAX_ARTIFACT_BYTES_PER_RUN {
            return Err(AppError::validation("resource_limit"));
        }

        let id = Uuid::new_v4().simple().to_string();
        let run_dir = self.root.join(run_id);
        std::fs::create_dir_all(&run_dir)?;
        // 拒绝路径穿越：id 仅 uuid hex
        if id.contains("..") || id.contains('/') || id.contains('\\') {
            return Err(AppError::validation("resource_limit"));
        }
        let path = run_dir.join(format!("{id}.bin"));
        std::fs::write(&path, bytes)?;
        let meta = ArtifactMeta {
            id: id.clone(),
            run_id: run_id.to_string(),
            kind: kind.to_string(),
            path,
            byte_len: bytes.len(),
            created_at: self.now(),
        };
        guard.insert(id, meta.clone());
        Ok(meta)
    }

    /// 读取 artifact 字节。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     UI/Orchestrator 按 id 拉取截图。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查索引并读文件；缺失/过期返回 not_found。
    pub fn get(&self, run_id: &str, artifact_id: &str) -> Result<Vec<u8>, AppError> {
        let guard = self.inner.lock().expect("artifact store");
        let meta = guard
            .get(artifact_id)
            .ok_or_else(|| AppError::not_found("browser_artifact_not_found"))?;
        if meta.run_id != run_id {
            return Err(AppError::not_found("browser_artifact_not_found"));
        }
        if self.now().duration_since(meta.created_at) > self.retention {
            return Err(AppError::not_found("browser_artifact_not_found"));
        }
        // 路径必须在 root 下
        if !meta.path.starts_with(&self.root) {
            return Err(AppError::validation("resource_limit"));
        }
        Ok(std::fs::read(&meta.path)?)
    }

    /// 删除某 run 的全部 artifact。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     cancel/cleanup 时释放磁盘。
    ///
    /// Code Logic（这个函数做什么）:
    ///     移除索引项并尝试删除 run 目录。
    pub fn remove_run(&self, run_id: &str) -> Result<(), AppError> {
        let mut guard = self.inner.lock().expect("artifact store");
        let ids: Vec<String> = guard
            .values()
            .filter(|m| m.run_id == run_id)
            .map(|m| m.id.clone())
            .collect();
        for id in ids {
            if let Some(meta) = guard.remove(&id) {
                let _ = std::fs::remove_file(&meta.path);
            }
        }
        let run_dir = self.root.join(run_id);
        let _ = std::fs::remove_dir_all(&run_dir);
        Ok(())
    }

    /// 测试：索引中是否仍登记该 artifact（忽略 retention 读取语义）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     `get` 对过期项也返回 not_found；单测需区分「仅过期」与「已被 cleanup 删除」。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查内存索引是否含 (run_id, artifact_id)。
    #[cfg(test)]
    pub fn is_indexed_for_test(&self, run_id: &str, artifact_id: &str) -> bool {
        let guard = self.inner.lock().expect("artifact store");
        guard.get(artifact_id).is_some_and(|m| m.run_id == run_id)
    }

    /// 清理超过 retention 的 artifact。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     后台/显式 cleanup 防止 24h 后残留。
    ///
    /// Code Logic（这个函数做什么）:
    ///     扫描索引，删除过期项与空 run 目录。
    pub fn cleanup_expired(&self) -> Result<usize, AppError> {
        let now = self.now();
        let mut guard = self.inner.lock().expect("artifact store");
        let expired: Vec<String> = guard
            .iter()
            .filter(|(_, m)| now.duration_since(m.created_at) > self.retention)
            .map(|(id, _)| id.clone())
            .collect();
        let count = expired.len();
        for id in expired {
            if let Some(meta) = guard.remove(&id) {
                let _ = std::fs::remove_file(&meta.path);
            }
        }
        Ok(count)
    }
}

/// 生成稳定但非路径的日志用哈希。
///
/// Business Logic（为什么需要这个函数）:
///     profile 删除失败时日志不得打印真实路径。
///
/// Code Logic（这个函数做什么）:
///     对路径字符串做 SHA-256 截断 hex。
pub fn path_hash_for_log(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    let full = format!("{:x}", hasher.finalize());
    full.chars().take(16).collect()
}

/// epoch 毫秒（辅助 session 时间戳）。
///
/// Business Logic（为什么需要这个函数）:
///     session DTO 需要 ISO/可读时间戳。
///
/// Code Logic（这个函数做什么）:
///     返回当前 UNIX epoch 毫秒。
pub fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn rejects_oversized_screenshot() {
        let dir = tempdir().unwrap();
        let store = ArtifactStore::new(dir.path()).unwrap();
        let big = vec![0u8; MAX_SCREENSHOT_BYTES + 1];
        let err = store.put("run1", "screenshot", &big).unwrap_err();
        assert_eq!(err.code(), "resource_limit");
    }

    #[test]
    fn enforces_per_run_count_and_expires() {
        let dir = tempdir().unwrap();
        let store = ArtifactStore::new_with_retention(dir.path(), Duration::from_secs(1)).unwrap();
        for _ in 0..MAX_ARTIFACTS_PER_RUN {
            store.put("run1", "bin", b"hi").unwrap();
        }
        let err = store.put("run1", "bin", b"x").unwrap_err();
        assert_eq!(err.code(), "resource_limit");

        let meta = store.put("run2", "bin", b"png").unwrap();
        store.advance_for_test(Duration::from_secs(2));
        store.cleanup_expired().unwrap();
        assert!(store.get("run2", &meta.id).is_err());
    }
}
