//! agent_hub/object_store — 明文 SHA-256 CAS 与 TreeManifest
//!
//! Business Logic（为什么需要这个模块）:
//!     Agent Hub 需要跨设备可复制的不可变正文与目录资产；凭据与普通内容一样以明文 blob 保存，
//!     不走 field redaction 或加密包装。目录资产用 TreeManifest 保留相对路径、hash 与 executable bit。
//!
//! Code Logic（这个模块做什么）:
//!     在 `<data_dir>/agent-hub/objects/sha256/<first-two>/<hash>` 上提供 put/get blob/tree、
//!     目录扫描入库与未引用 GC；写入路径为 sibling UUID temp → sync → 重读校验 hash → rename →
//!     父目录 sync（Unix）。

use crate::error::AppError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

/// CAS 根目录 Unix 权限（仅当前用户）。
#[cfg(unix)]
const ROOT_MODE: u32 = 0o700;
/// object / temp 文件 Unix 权限。
#[cfg(unix)]
const OBJECT_MODE: u32 = 0o600;

/// 明文 content-addressed object store。
///
/// Business Logic（为什么需要这个结构体）:
///     revision / projection / snapshot 共享同一份不可变对象库，避免重复拷贝正文。
///
/// Code Logic（这个结构体做什么）:
///     持有 objects 根路径；对外暴露 put/get/gc 与路径查询。
#[derive(Debug, Clone)]
pub struct ObjectStore {
    /// `<data_dir>/agent-hub/objects`
    root: PathBuf,
}

/// 已入库对象句柄。
///
/// Business Logic（为什么需要这个结构体）:
///     调用方只需记住 hash，即可在后续 revision 中引用。
///
/// Code Logic（这个结构体做什么）:
///     保存小写 hex SHA-256。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredObject {
    /// 对象内容 SHA-256（小写 hex）
    pub hash: String,
}

/// 目录清单中的条目类型。
///
/// Business Logic（为什么需要这个枚举）:
///     文件与 symlink 的投影/可移植性语义不同，不能混为一谈。
///
/// Code Logic（这个枚举做什么）:
///     camelCase 序列化；`file` / `symlink`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TreeEntryType {
    /// 普通文件（精确字节）
    File,
    /// 符号链接（blob 内容为 link target 文本）
    Symlink,
}

impl TreeEntryType {
    /// 稳定 wire 字符串。
    ///
    /// Business Logic: manifest / 日志需要稳定 token。
    /// Code Logic: 返回 `file` / `symlink`。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Symlink => "symlink",
        }
    }
}

/// TreeManifest 中的一条路径记录。
///
/// Business Logic（为什么需要这个结构体）:
///     Skill/Plugin 等目录资产必须保留相对路径、内容 hash 与 executable bit。
///
/// Code Logic（这个结构体做什么）:
///     保存规范化正斜杠相对路径与元数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeEntry {
    /// 规范化 forward-slash 相对路径
    pub path: String,
    /// 对应 blob 的 SHA-256 hex
    pub blob_hash: String,
    /// 文件或 symlink
    pub entry_type: TreeEntryType,
    /// Unix executable bit（owner/group/other 任一 x）
    pub executable: bool,
}

/// 排序后的目录清单。
///
/// Business Logic（为什么需要这个结构体）:
///     同一目录内容必须有确定性 hash，供 revision.tree_manifest_hash 引用。
///
/// Code Logic（这个结构体做什么）:
///     持有按 path 字典序排序的 entries；序列化为紧凑 JSON 后入 CAS。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeManifest {
    /// 已排序的路径条目
    pub entries: Vec<TreeEntry>,
}

/// 扫描目录时的非致命诊断。
///
/// Business Logic（为什么需要这个枚举）:
///     路径逃逸/越界 symlink 不能静默跟随，但也不应让整树扫描失败到无法诊断。
///
/// Code Logic（这个枚举做什么）:
///     标记 OutsideRoot / PathTraversal 等；调用方可将该资产标为 partial。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TreeEntryDiagnostic {
    /// symlink 解析后落在资产根之外
    OutsideRoot {
        /// 相对路径
        path: String,
    },
    /// 相对路径含 `..` 或其它非法组件
    PathTraversal {
        /// 原始相对路径
        path: String,
    },
}

/// 目录扫描入库结果。
///
/// Business Logic（为什么需要这个结构体）:
///     调用方需要 manifest hash、完整清单与诊断列表才能决定是否 partial。
///
/// Code Logic（这个结构体做什么）:
///     聚合 StoredObject（manifest）、TreeManifest 与 diagnostics。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutTreeResult {
    /// manifest 对象
    pub object: StoredObject,
    /// 成功收录的清单
    pub manifest: TreeManifest,
    /// 非致命诊断
    pub diagnostics: Vec<TreeEntryDiagnostic>,
}

impl ObjectStore {
    /// 打开或初始化 CAS 根。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Hub 启动后需要在 data_dir 下准备私有 object 目录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 `<data_dir>/agent-hub/objects` 与 `sha256` 子目录（Unix 0700），返回 store。
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, AppError> {
        let root = data_dir.as_ref().join("agent-hub").join("objects");
        ensure_dir_mode(
            &root,
            #[cfg(unix)]
            ROOT_MODE,
        )?;
        ensure_dir_mode(
            &root.join("sha256"),
            #[cfg(unix)]
            ROOT_MODE,
        )?;
        Ok(Self { root })
    }

    /// CAS 根目录（`<data_dir>/agent-hub/objects`）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     测试与诊断需要断言对象路径不逃逸 root。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回内部 root 引用。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 计算对象在 CAS 中的最终路径。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     调用方/测试需验证 hash 映射且路径始终落在 root 下。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `sha256/<first-two>/<hash>`；非法 hash 返回 Validation。
    pub fn object_path(&self, hash: &str) -> Result<PathBuf, AppError> {
        validate_hash(hash)?;
        let prefix = &hash[..2];
        Ok(self.root.join("sha256").join(prefix).join(hash))
    }

    /// 写入精确字节 blob。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     revision payload / 目录文件 / symlink target 都以明文精确字节入库。
    ///
    /// Code Logic（这个函数做什么）:
    ///     计算 SHA-256；已存在则直接返回；否则 sibling UUID temp → 0600 → write →
    ///     sync_all → re-read/hash 校验 → rename → 父目录 sync。
    pub async fn put_blob(&self, bytes: &[u8]) -> Result<StoredObject, AppError> {
        let hash = sha256_hex(bytes);
        let path = self.object_path(&hash)?;
        if path.is_file() {
            return Ok(StoredObject { hash });
        }
        let bytes = bytes.to_vec();
        let root = self.root.clone();
        let hash_for_write = hash.clone();
        tokio::task::spawn_blocking(move || write_object_atomic(&root, &hash_for_write, &bytes))
            .await
            .map_err(|e| AppError::generic(format!("object_store put_blob join: {e}")))??;
        Ok(StoredObject { hash })
    }

    /// 读取 blob 精确字节。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     投影/合并/导出需要取回原文。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验 hash → 读文件；缺失返回 NotFound；路径逃逸由 validate_hash 拒绝。
    pub async fn get_blob(&self, hash: &str) -> Result<Vec<u8>, AppError> {
        let path = self.object_path(hash)?;
        let path_clone = path.clone();
        let hash_owned = hash.to_string();
        tokio::task::spawn_blocking(move || {
            if !path_clone.is_file() {
                return Err(AppError::not_found(format!(
                    "agent_hub_object_not_found:{hash_owned}"
                )));
            }
            fs::read(&path_clone).map_err(AppError::from)
        })
        .await
        .map_err(|e| AppError::generic(format!("object_store get_blob join: {e}")))?
    }

    /// 将 TreeManifest 序列化后写入 CAS。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     目录资产 revision 引用 manifest hash，而非散落路径列表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     复制并按 path 排序 entries → 紧凑 JSON → put_blob。
    pub async fn put_tree(&self, manifest: &TreeManifest) -> Result<StoredObject, AppError> {
        let mut sorted = manifest.clone();
        sorted.entries.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then_with(|| a.blob_hash.cmp(&b.blob_hash))
        });
        let bytes = serde_json::to_vec(&sorted)
            .map_err(|e| AppError::generic(format!("tree manifest serialize: {e}")))?;
        self.put_blob(&bytes).await
    }

    /// 读取并反序列化 TreeManifest。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GC/投影/导出需要展开目录清单。
    ///
    /// Code Logic（这个函数做什么）:
    ///     get_blob → JSON 反序列化。
    pub async fn get_tree(&self, hash: &str) -> Result<TreeManifest, AppError> {
        let bytes = self.get_blob(hash).await?;
        serde_json::from_slice(&bytes)
            .map_err(|e| AppError::generic(format!("tree manifest deserialize: {e}")))
    }

    /// 扫描目录，入库文件/内链 symlink，并写入 TreeManifest。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     纳管 Skill/Plugin 目录时需保留精确字节与 executable bit；越界 symlink 只记诊断。
    ///
    /// Code Logic（这个函数做什么）:
    ///     递归 walk；规范化相对路径；文件 put_blob；symlink 读 target 文本为 blob，
    ///     解析后若落在 root 外则 OutsideRoot 且不跟随；含 `..` 则 PathTraversal。
    pub async fn put_tree_from_directory(
        &self,
        dir: impl AsRef<Path>,
    ) -> Result<PutTreeResult, AppError> {
        let dir = dir.as_ref().to_path_buf();
        let store = self.clone();
        tokio::task::spawn_blocking(move || scan_directory_blocking(&store, &dir))
            .await
            .map_err(|e| {
                AppError::generic(format!("object_store put_tree_from_directory join: {e}"))
            })?
    }

    /// 删除未被引用的 object。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     仅当 revision / projection job / snapshot 均不引用时才能 GC，避免破坏可恢复历史。
    ///
    /// Code Logic（这个函数做什么）:
    ///     遍历 `sha256/*/*` 文件，hash 不在 keep 集合则删除；返回删除数量。
    pub async fn gc_unreferenced(&self, keep: &HashSet<String>) -> Result<usize, AppError> {
        let root = self.root.clone();
        let keep = keep.clone();
        tokio::task::spawn_blocking(move || gc_unreferenced_blocking(&root, &keep))
            .await
            .map_err(|e| AppError::generic(format!("object_store gc join: {e}")))?
    }
}

/// 计算字节的小写 hex SHA-256。
///
/// Business Logic（为什么需要这个函数）:
///     CAS 身份与 revision hash 统一使用同一算法。
///
/// Code Logic（这个函数做什么）:
///     Sha256 digest → lowercase hex。
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// 校验 hash 为 64 位小写 hex。
///
/// Business Logic（为什么需要这个函数）:
///     非法 hash 绝不能拼出 root 外路径。
///
/// Code Logic（这个函数做什么）:
///     长度 64 且仅 `[0-9a-f]`。
fn validate_hash(hash: &str) -> Result<(), AppError> {
    if hash.len() != 64 || !hash.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
        return Err(AppError::validation(format!(
            "agent_hub_invalid_object_hash:{hash}"
        )));
    }
    Ok(())
}

/// 确保目录存在并设置 Unix 权限。
///
/// Business Logic（为什么需要这个函数）:
///     Hub 对象库对当前用户私有（0700）。
///
/// Code Logic（这个函数做什么）:
///     create_dir_all + 可选 set_mode。
fn ensure_dir_mode(path: &Path, #[cfg(unix)] mode: u32) -> Result<(), AppError> {
    if !path.exists() {
        fs::create_dir_all(path)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(mode);
        fs::set_permissions(path, perms)?;
    }
    let _ = path;
    Ok(())
}

/// 设置文件为 0600（Unix）。
///
/// Business Logic（为什么需要这个函数）:
///     object/temp 仅当前用户可读。
///
/// Code Logic（这个函数做什么）:
///     PermissionsExt set_mode 0600。
fn set_object_file_mode(path: &Path) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(OBJECT_MODE);
        fs::set_permissions(path, perms)?;
    }
    let _ = path;
    Ok(())
}

/// 原子写入单个 object 文件。
///
/// Business Logic（为什么需要这个函数）:
///     崩溃时不能留下半写且被误认为有效 hash 的对象。
///
/// Code Logic（这个函数做什么）:
///     同目录 `.tmp.<uuid>` create_new → 0600 → write → flush → sync_all →
///     re-read hash 校验 → rename → 父目录 sync。
fn write_object_atomic(root: &Path, hash: &str, bytes: &[u8]) -> Result<(), AppError> {
    validate_hash(hash)?;
    let prefix = &hash[..2];
    let dir = root.join("sha256").join(prefix);
    ensure_dir_mode(
        &dir,
        #[cfg(unix)]
        ROOT_MODE,
    )?;
    let final_path = dir.join(hash);
    if final_path.is_file() {
        return Ok(());
    }

    let temp = dir.join(format!(".tmp.{}", Uuid::new_v4()));
    let result = (|| -> Result<(), AppError> {
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            set_object_file_mode(&temp)?;
            file.write_all(bytes)?;
            file.flush()?;
            file.sync_all()?;
        }

        let reread = fs::read(&temp)?;
        let actual = sha256_hex(&reread);
        if actual != hash {
            return Err(AppError::generic(format!(
                "agent_hub_object_hash_mismatch:expected={hash},actual={actual}"
            )));
        }

        fs::rename(&temp, &final_path)?;
        // rename 后确保最终文件权限
        set_object_file_mode(&final_path)?;
        sync_dir(&dir);
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// 同步目录元数据（Unix）；失败仅 best-effort。
///
/// Business Logic（为什么需要这个函数）:
///     使 rename 本身 durable。
///
/// Code Logic（这个函数做什么）:
///     打开目录并 sync_all；Windows 忽略错误。
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    {
        if let Ok(file) = File::open(dir) {
            let _ = file.sync_all();
        }
    }
    let _ = dir;
}

/// 阻塞扫描目录并入库。
///
/// Business Logic（为什么需要这个函数）:
///     spawn_blocking 内完成全部 FS + 同步 put。
///
/// Code Logic（这个函数做什么）:
///     walk_dir 收集 entries/diagnostics → 写 manifest blob。
fn scan_directory_blocking(store: &ObjectStore, dir: &Path) -> Result<PutTreeResult, AppError> {
    let root = fs::canonicalize(dir).map_err(|e| {
        AppError::validation(format!(
            "agent_hub_tree_root_invalid:{}: {e}",
            dir.display()
        ))
    })?;
    if !root.is_dir() {
        return Err(AppError::validation(format!(
            "agent_hub_tree_root_not_dir:{}",
            root.display()
        )));
    }

    let mut entries: Vec<TreeEntry> = Vec::new();
    let mut diagnostics: Vec<TreeEntryDiagnostic> = Vec::new();
    walk_tree(
        &root,
        &root,
        Path::new(""),
        store,
        &mut entries,
        &mut diagnostics,
    )?;

    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest = TreeManifest { entries };
    let bytes = serde_json::to_vec(&manifest)
        .map_err(|e| AppError::generic(format!("tree manifest serialize: {e}")))?;
    let hash = sha256_hex(&bytes);
    write_object_atomic(&store.root, &hash, &bytes)?;
    Ok(PutTreeResult {
        object: StoredObject { hash },
        manifest,
        diagnostics,
    })
}

/// 递归 walk 一棵树。
///
/// Business Logic（为什么需要这个函数）:
///     需要在不跟随越界 symlink 的前提下收录所有安全条目。
///
/// Code Logic（这个函数做什么）:
///     对每个 dir entry：规范化相对路径；symlink 单独处理；文件 put；目录递归。
fn walk_tree(
    tree_root: &Path,
    current: &Path,
    rel: &Path,
    store: &ObjectStore,
    entries: &mut Vec<TreeEntry>,
    diagnostics: &mut Vec<TreeEntryDiagnostic>,
) -> Result<(), AppError> {
    let read_dir = fs::read_dir(current)?;
    let mut children: Vec<_> = read_dir.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(|e| e.file_name());

    for entry in children {
        let name = entry.file_name();
        let child_rel = rel.join(&name);
        let rel_str = normalize_rel_path(&child_rel);
        if rel_str.is_none() {
            diagnostics.push(TreeEntryDiagnostic::PathTraversal {
                path: child_rel.to_string_lossy().replace('\\', "/"),
            });
            continue;
        }
        let rel_str = rel_str.unwrap();
        let child_path = entry.path();
        let meta = fs::symlink_metadata(&child_path)?;

        if meta.file_type().is_symlink() {
            handle_symlink(
                tree_root,
                &child_path,
                &rel_str,
                store,
                entries,
                diagnostics,
            )?;
            continue;
        }
        if meta.is_dir() {
            walk_tree(
                tree_root,
                &child_path,
                &child_rel,
                store,
                entries,
                diagnostics,
            )?;
            continue;
        }
        if meta.is_file() {
            let bytes = fs::read(&child_path)?;
            let hash = sha256_hex(&bytes);
            write_object_atomic(&store.root, &hash, &bytes)?;
            entries.push(TreeEntry {
                path: rel_str,
                blob_hash: hash,
                entry_type: TreeEntryType::File,
                executable: is_executable(&meta),
            });
        }
    }
    Ok(())
}

/// 处理 symlink：越界记诊断，不跟随。
///
/// Business Logic（为什么需要这个函数）:
///     指向资产根外的 symlink 不得被跟随，否则会把宿主机任意文件吸入 CAS。
///
/// Code Logic（这个函数做什么）:
///     读 link target 文本；canonicalize parent+target 判断是否在 root 内；
///     外则 OutsideRoot；内则把 target 文本作为 blob 入库。
fn handle_symlink(
    tree_root: &Path,
    link_path: &Path,
    rel_str: &str,
    store: &ObjectStore,
    entries: &mut Vec<TreeEntry>,
    diagnostics: &mut Vec<TreeEntryDiagnostic>,
) -> Result<(), AppError> {
    let target = fs::read_link(link_path)?;
    let target_text = target.to_string_lossy().into_owned();

    // 解析 symlink：相对链接相对 link 父目录
    let resolved_candidate = if target.is_absolute() {
        target.clone()
    } else {
        link_path.parent().unwrap_or(link_path).join(&target)
    };

    let outside = match fs::canonicalize(&resolved_candidate) {
        Ok(resolved) => !is_path_within_root(tree_root, &resolved),
        Err(_) => {
            // 断链：若规范化后的逻辑路径明显越界也记 OutsideRoot
            let logical = normalize_logical_path(link_path.parent().unwrap_or(link_path), &target);
            match logical {
                Some(p) => !is_path_within_root(tree_root, &p),
                None => true,
            }
        }
    };

    if outside {
        diagnostics.push(TreeEntryDiagnostic::OutsideRoot {
            path: rel_str.to_string(),
        });
        return Ok(());
    }

    let bytes = target_text.as_bytes();
    let hash = sha256_hex(bytes);
    write_object_atomic(&store.root, &hash, bytes)?;
    entries.push(TreeEntry {
        path: rel_str.to_string(),
        blob_hash: hash,
        entry_type: TreeEntryType::Symlink,
        executable: false,
    });
    Ok(())
}

/// 判断 candidate 是否位于 root 之下（含 root 自身）。
///
/// Business Logic（为什么需要这个函数）:
///     symlink 逃逸检测依赖路径 containment。
///
/// Code Logic（这个函数做什么）:
///     逐 Component 比较；candidate 以 root 为前缀。
fn is_path_within_root(root: &Path, candidate: &Path) -> bool {
    let root_comps: Vec<_> = root.components().collect();
    let cand_comps: Vec<_> = candidate.components().collect();
    if cand_comps.len() < root_comps.len() {
        return false;
    }
    root_comps
        .iter()
        .zip(cand_comps.iter())
        .all(|(a, b)| a == b)
}

/// 不访问磁盘地拼接逻辑路径（用于断链判断）。
///
/// Business Logic（为什么需要这个函数）:
///     断链 symlink 无法 canonicalize，但仍需判断是否越界。
///
/// Code Logic（这个函数做什么）:
///     从 base 起应用 target 组件；遇到根上的 ParentDir 返回 None。
fn normalize_logical_path(base: &Path, target: &Path) -> Option<PathBuf> {
    let mut out = base.to_path_buf();
    for comp in target.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => {
                out = PathBuf::new();
                out.push(comp.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(s) => out.push(s),
        }
    }
    Some(out)
}

/// 将相对 Path 规范为 forward-slash 字符串；含 `..` 返回 None。
///
/// Business Logic（为什么需要这个函数）:
///     manifest path 必须可跨平台且禁止逃逸。
///
/// Code Logic（这个函数做什么）:
///     拒绝 Prefix/RootDir/ParentDir；Normal 以 `/` 连接。
fn normalize_rel_path(rel: &Path) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(s) => {
                let s = s.to_string_lossy();
                if s.is_empty() || s == "." {
                    continue;
                }
                if s == ".." || s.contains('\0') {
                    return None;
                }
                parts.push(s.into_owned());
            }
            Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => return None,
        }
    }
    Some(parts.join("/"))
}

/// 判断 metadata 是否带 executable bit。
///
/// Business Logic（为什么需要这个函数）:
///     Skill 脚本等依赖 executable bit 才能在目标机正确执行。
///
/// Code Logic（这个函数做什么）:
///     Unix：mode & 0o111 != 0；其它平台 false。
fn is_executable(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

/// 阻塞 GC。
///
/// Business Logic（为什么需要这个函数）:
///     清理无引用 object 释放磁盘。
///
/// Code Logic（这个函数做什么）:
///     扫描 sha256 二级目录，文件名不在 keep 则删除。
fn gc_unreferenced_blocking(root: &Path, keep: &HashSet<String>) -> Result<usize, AppError> {
    let sha_root = root.join("sha256");
    if !sha_root.is_dir() {
        return Ok(0);
    }
    let mut removed = 0usize;
    for prefix_entry in fs::read_dir(&sha_root)? {
        let prefix_entry = prefix_entry?;
        if !prefix_entry.file_type()?.is_dir() {
            continue;
        }
        for obj in fs::read_dir(prefix_entry.path())? {
            let obj = obj?;
            let name = obj.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                // 清理残留 temp
                let _ = fs::remove_file(obj.path());
                continue;
            }
            if !keep.contains(name.as_ref()) {
                fs::remove_file(obj.path())?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[cfg(unix)]
    use std::os::unix::fs::{symlink, PermissionsExt};

    /// 构造临时 data_dir 上的 ObjectStore。
    ///
    /// Business Logic: 单测隔离真实 ~/.cc-partner。
    /// Code Logic: TempDir + ObjectStore::open。
    fn open_temp_store() -> (TempDir, ObjectStore) {
        let tmp = TempDir::new().unwrap();
        let store = ObjectStore::open(tmp.path()).unwrap();
        (tmp, store)
    }

    #[tokio::test]
    async fn put_blob_round_trip_exact_bytes_and_hash() {
        let (_tmp, store) = open_temp_store();
        let object = store.put_blob(b"token=plain-text").await.unwrap();
        assert_eq!(
            store.get_blob(&object.hash).await.unwrap(),
            b"token=plain-text"
        );
        assert_eq!(object.hash, sha256_hex(b"token=plain-text"));
        let path = store.object_path(&object.hash).unwrap();
        assert!(path.starts_with(store.root()));
        assert!(path.to_string_lossy().contains(&format!(
            "sha256/{}/{}",
            &object.hash[..2],
            object.hash
        )));
    }

    #[tokio::test]
    async fn put_blob_is_idempotent() {
        let (_tmp, store) = open_temp_store();
        let a = store.put_blob(b"same").await.unwrap();
        let b = store.put_blob(b"same").await.unwrap();
        assert_eq!(a, b);
    }

    /// open 契约：data_dir 只 join 一次 agent-hub/objects。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     防止 runtime 把已 join 的 CAS 根再 open 一次，嵌套成
    ///     `.../agent-hub/objects/agent-hub/objects`，造成跨路径 blob miss。
    ///
    /// Code Logic（这个函数做什么）:
    ///     同一 data_dir 两次 open 的 root 相等；blob 经 data_dir open 写入后
    ///     可被另一 data_dir open 读回；预 join 后的 open 根与正确根不等。
    #[tokio::test]
    async fn open_accepts_data_dir_not_prejoined_objects_root() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path();
        let writer = ObjectStore::open(data_dir).unwrap();
        let expected_root = data_dir.join("agent-hub").join("objects");
        assert_eq!(writer.root(), expected_root.as_path());

        let object = writer
            .put_blob(b"gate-a-object-store-root-contract")
            .await
            .unwrap();

        // 生产正确调用方：service / projection_ops / 修复后 runtime 都只传 data_dir。
        let reader = ObjectStore::open(data_dir).unwrap();
        assert_eq!(reader.root(), writer.root());
        assert_eq!(
            reader.get_blob(&object.hash).await.unwrap(),
            b"gate-a-object-store-root-contract"
        );

        // 回归：预 join 会打开嵌套 CAS 根，与正确根不同，读不到同一 blob。
        let nested = ObjectStore::open(data_dir.join("agent-hub").join("objects")).unwrap();
        assert_ne!(nested.root(), writer.root());
        assert!(nested.root().ends_with(
            Path::new("agent-hub")
                .join("objects")
                .join("agent-hub")
                .join("objects")
        ));
        let miss = nested.get_blob(&object.hash).await;
        assert!(miss.is_err(), "nested CAS must not see writer blobs");
    }

    #[tokio::test]
    async fn invalid_hash_never_escapes_cas_root() {
        let (_tmp, store) = open_temp_store();
        let err = store.object_path("../etc/passwd").unwrap_err();
        assert!(
            err.to_string().contains("invalid_object_hash")
                || err.to_string().contains("agent_hub_invalid")
        );
        let err = store.get_blob("zz").await.unwrap_err();
        assert!(
            err.to_string().contains("invalid_object_hash")
                || err.to_string().contains("agent_hub_invalid")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_root_0700_and_object_0600() {
        let (_tmp, store) = open_temp_store();
        let root_mode = fs::metadata(store.root()).unwrap().permissions().mode() & 0o777;
        assert_eq!(root_mode, 0o700);
        let object = store.put_blob(b"secret").await.unwrap();
        let path = store.object_path(&object.hash).unwrap();
        let obj_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(obj_mode, 0o600);
    }

    #[tokio::test]
    async fn put_tree_sorts_paths_and_round_trips() {
        let (_tmp, store) = open_temp_store();
        let blob_a = store.put_blob(b"a").await.unwrap();
        let blob_b = store.put_blob(b"b").await.unwrap();
        let manifest = TreeManifest {
            entries: vec![
                TreeEntry {
                    path: "z.txt".into(),
                    blob_hash: blob_b.hash.clone(),
                    entry_type: TreeEntryType::File,
                    executable: false,
                },
                TreeEntry {
                    path: "a.txt".into(),
                    blob_hash: blob_a.hash.clone(),
                    entry_type: TreeEntryType::File,
                    executable: true,
                },
            ],
        };
        let object = store.put_tree(&manifest).await.unwrap();
        let loaded = store.get_tree(&object.hash).await.unwrap();
        assert_eq!(loaded.entries[0].path, "a.txt");
        assert_eq!(loaded.entries[1].path, "z.txt");
        assert!(loaded.entries[0].executable);
        assert_eq!(loaded.entries[0].blob_hash, blob_a.hash);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn put_tree_from_directory_preserves_executable_bit() {
        let (_tmp, store) = open_temp_store();
        let tree = tempfile::tempdir().unwrap();
        let script = tree.path().join("run.sh");
        fs::write(&script, b"#!/bin/sh\necho hi\n").unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();
        fs::write(tree.path().join("readme.txt"), b"plain").unwrap();

        let result = store.put_tree_from_directory(tree.path()).await.unwrap();
        assert!(result.diagnostics.is_empty());
        let run = result
            .manifest
            .entries
            .iter()
            .find(|e| e.path == "run.sh")
            .unwrap();
        assert!(run.executable);
        assert_eq!(run.entry_type, TreeEntryType::File);
        let readme = result
            .manifest
            .entries
            .iter()
            .find(|e| e.path == "readme.txt")
            .unwrap();
        assert!(!readme.executable);
        assert_eq!(
            store.get_blob(&run.blob_hash).await.unwrap(),
            b"#!/bin/sh\necho hi\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_outside_root_returns_diagnostic_not_followed() {
        let (_tmp, store) = open_temp_store();
        let tree = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        fs::write(&secret, b"should-not-ingest").unwrap();
        symlink(&secret, tree.path().join("escape")).unwrap();
        fs::write(tree.path().join("ok.txt"), b"ok").unwrap();

        let result = store.put_tree_from_directory(tree.path()).await.unwrap();
        assert!(
            result.diagnostics.iter().any(
                |d| matches!(d, TreeEntryDiagnostic::OutsideRoot { path } if path == "escape")
            ),
            "diagnostics={:?}",
            result.diagnostics
        );
        assert!(result.manifest.entries.iter().all(|e| e.path != "escape"));
        assert!(result.manifest.entries.iter().any(|e| e.path == "ok.txt"));
        // 越界内容不得入库
        let secret_hash = sha256_hex(b"should-not-ingest");
        assert!(store.get_blob(&secret_hash).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_inside_root_stored_as_symlink_entry() {
        let (_tmp, store) = open_temp_store();
        let tree = tempfile::tempdir().unwrap();
        fs::write(tree.path().join("target.txt"), b"target-bytes").unwrap();
        symlink("target.txt", tree.path().join("link.txt")).unwrap();

        let result = store.put_tree_from_directory(tree.path()).await.unwrap();
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let link = result
            .manifest
            .entries
            .iter()
            .find(|e| e.path == "link.txt")
            .unwrap();
        assert_eq!(link.entry_type, TreeEntryType::Symlink);
        assert_eq!(
            store.get_blob(&link.blob_hash).await.unwrap(),
            b"target.txt"
        );
    }

    #[tokio::test]
    async fn gc_unreferenced_removes_only_unkept_objects() {
        let (_tmp, store) = open_temp_store();
        let keep_obj = store.put_blob(b"keep-me").await.unwrap();
        let drop_obj = store.put_blob(b"drop-me").await.unwrap();
        let mut keep = HashSet::new();
        keep.insert(keep_obj.hash.clone());
        let removed = store.gc_unreferenced(&keep).await.unwrap();
        assert!(removed >= 1);
        assert_eq!(store.get_blob(&keep_obj.hash).await.unwrap(), b"keep-me");
        assert!(store.get_blob(&drop_obj.hash).await.is_err());
    }
}
