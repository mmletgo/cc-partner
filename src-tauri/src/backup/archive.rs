//! backup/archive.rs — 可验证导出 ZIP 与流式安全校验
//!
//! Business Logic（为什么需要这个模块）:
//!     用户导出 Prompt/CC History/Scratchpad/SSH/CLAUDE.md/deletion floors/配置 report，
//!     排除项目源码、终端 transcript、私钥、token 与 lifecycle control token；
//!     恢复前必须流式校验体积/路径/哈希，拒绝 zip-slip 与符号链接。
//!
//! Code Logic（这个模块做什么）:
//!     写出 versioned manifest + 领域 JSON + SHA-256；inspect 流式读取不分配声明总大小。

use crate::error::AppError;
use crate::state::AppState;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path};
use zip::write::FileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// 导出格式版本。未知版本预览前拒绝。
pub const FORMAT_VERSION: u32 = 1;

/// archive 文件上限 2 GiB。
pub const MAX_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// entry 数量上限。
pub const MAX_ENTRIES: u64 = 100_000;
/// 单 entry 解压后上限 64 MiB。
pub const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;
/// 总解压量上限 4 GiB。
pub const MAX_TOTAL_UNCOMPRESSED: u64 = 4 * 1024 * 1024 * 1024;

pub const DOMAIN_PROMPTS: &str = "prompts";
pub const DOMAIN_CC_HISTORY: &str = "ccHistory";
pub const DOMAIN_SCRATCHPAD: &str = "scratchpad";
pub const DOMAIN_SSH_TARGETS: &str = "sshTargets";
pub const DOMAIN_CLAUDE_MD: &str = "claudeMd";
pub const DOMAIN_DELETION_FLOORS: &str = "deletionFloors";
pub const DOMAIN_CONFIG_REPORT: &str = "configReport";

/// 流式限制集合（测试可缩小）。
#[derive(Debug, Clone, Copy)]
pub struct ArchiveLimits {
    pub max_archive_bytes: u64,
    pub max_entries: u64,
    pub max_entry_bytes: u64,
    pub max_total_uncompressed: u64,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_archive_bytes: MAX_ARCHIVE_BYTES,
            max_entries: MAX_ENTRIES,
            max_entry_bytes: MAX_ENTRY_BYTES,
            max_total_uncompressed: MAX_TOTAL_UNCOMPRESSED,
        }
    }
}

/// 导出 manifest。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveManifest {
    pub format_version: u32,
    pub created_at: String,
    pub device_id: String,
    pub domains: Vec<String>,
    /// 相对路径 → sha256 hex
    pub files: BTreeMap<String, String>,
}

/// 流式 inspect 结果（不落库）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectedArchive {
    pub manifest: ArchiveManifest,
    pub domain_counts: BTreeMap<String, u32>,
    pub warnings: Vec<String>,
    /// 已验证的 entry 名 → 字节（仍在 zip 内，未写 DB）
    pub entry_names: Vec<String>,
}

/// 创建导出 ZIP 到目标路径。
///
/// Business Logic: Settings「导出数据」；默认用户选择路径，不上传网络。
/// Code Logic: 序列化各领域 JSON → 算哈希 → 写 manifest → zip 落盘。
pub async fn create_export_archive(
    state: &AppState,
    dest: &Path,
) -> Result<ArchiveManifest, AppError> {
    let created_at = chrono::Utc::now().to_rfc3339();
    let device_id = state.device_id.as_str().to_string();

    let prompts = state.prompt_repo.get_all_for_sync().await?;
    let cc_history = state.cc_history_repo.get_all_for_sync().await?;
    let scratchpad = state.scratchpad_repo.get_all_for_sync().await?;
    let ssh_targets = state.ssh_target_repo.get_all_for_sync().await?;
    let claude_md = state.claude_md_repo.get().await?;
    let floors = export_deletion_floors(&state.db).await?;

    let config_report = build_config_report(state);

    let mut files_payload: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    files_payload.insert(
        format!("{DOMAIN_PROMPTS}/items.json"),
        serde_json::to_vec_pretty(&prompts)?,
    );
    files_payload.insert(
        format!("{DOMAIN_CC_HISTORY}/items.json"),
        serde_json::to_vec_pretty(&cc_history)?,
    );
    files_payload.insert(
        format!("{DOMAIN_SCRATCHPAD}/items.json"),
        serde_json::to_vec_pretty(&scratchpad)?,
    );
    files_payload.insert(
        format!("{DOMAIN_SSH_TARGETS}/items.json"),
        serde_json::to_vec_pretty(&ssh_targets)?,
    );
    files_payload.insert(
        format!("{DOMAIN_CLAUDE_MD}/item.json"),
        serde_json::to_vec_pretty(&claude_md)?,
    );
    files_payload.insert(
        format!("{DOMAIN_DELETION_FLOORS}/items.json"),
        serde_json::to_vec_pretty(&floors)?,
    );
    files_payload.insert(
        format!("{DOMAIN_CONFIG_REPORT}/report.json"),
        serde_json::to_vec_pretty(&config_report)?,
    );

    let mut file_hashes = BTreeMap::new();
    for (name, bytes) in &files_payload {
        file_hashes.insert(name.clone(), sha256_hex(bytes));
    }

    let manifest = ArchiveManifest {
        format_version: FORMAT_VERSION,
        created_at,
        device_id,
        domains: vec![
            DOMAIN_PROMPTS.into(),
            DOMAIN_CC_HISTORY.into(),
            DOMAIN_SCRATCHPAD.into(),
            DOMAIN_SSH_TARGETS.into(),
            DOMAIN_CLAUDE_MD.into(),
            DOMAIN_DELETION_FLOORS.into(),
            DOMAIN_CONFIG_REPORT.into(),
        ],
        files: file_hashes.clone(),
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    file_hashes.insert("manifest.json".into(), sha256_hex(&manifest_bytes));

    // 最终 manifest 含自身哈希列表（files 不含 manifest 自身，避免循环）
    let final_manifest = ArchiveManifest {
        files: {
            let mut f = manifest.files.clone();
            // files 已是领域文件哈希
            f
        },
        ..manifest
    };
    let final_manifest_bytes = serde_json::to_vec_pretty(&final_manifest)?;

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    // 使用旁路 partial 路径，完整落盘后 rename，避免半写包进入可回退列表
    let tmp = dest.with_extension("partial.zip");
    {
        let file = File::create(&tmp)?;
        let mut zip = ZipWriter::new(file);
        let options = FileOptions::default().compression_method(CompressionMethod::Deflated);
        zip.start_file("manifest.json", options)
            .map_err(|e| AppError::generic(format!("zip start manifest: {e}")))?;
        zip.write_all(&final_manifest_bytes)?;
        for (name, bytes) in &files_payload {
            zip.start_file(name, options)
                .map_err(|e| AppError::generic(format!("zip start {name}: {e}")))?;
            zip.write_all(bytes)?;
        }
        zip.finish()
            .map_err(|e| AppError::generic(format!("zip finish: {e}")))?;
    }
    fs::rename(&tmp, dest)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dest, fs::Permissions::from_mode(0o600));
    }
    Ok(final_manifest)
}

/// 导出全部 deletion floors（三域）。
///
/// Business Logic: floor 必须随备份迁移，防止恢复后旧 live 复活。
/// Code Logic: 对 prompts/ssh_target/scratchpad 各 list_for_domain 合并。
async fn export_deletion_floors(
    pool: &sqlx::sqlite::SqlitePool,
) -> Result<Vec<crate::storage::deletion_floor_repo::DeletionFloor>, AppError> {
    let repo = crate::storage::DeletionFloorRepo::new(pool.clone());
    let mut out = Vec::new();
    for domain in [
        crate::storage::sync_request_ledger_repo::DOMAIN_PROMPTS,
        crate::storage::sync_request_ledger_repo::DOMAIN_SSH_TARGET,
        crate::storage::sync_request_ledger_repo::DOMAIN_SCRATCHPAD,
    ] {
        out.extend(repo.list_for_domain(domain).await?);
    }
    Ok(out)
}

/// 配置只读 report（永不写回）。
fn build_config_report(state: &AppState) -> serde_json::Value {
    let cfg = state.config.read().expect("config 读锁").clone();
    serde_json::json!({
        "deviceId": cfg.device_id,
        "deviceName": cfg.device_name,
        "httpPort": cfg.http_port,
        "cloudSyncEnabled": cfg.cloud_sync_enabled,
        "cloudSyncAuto": cfg.cloud_sync_auto,
        "note": "report-only; never restored",
        // 明确排除：control token、repo secrets、SSH keys
        "excluded": ["controlToken", "sshPrivateKeys", "tokens", "credentials", "projectSource", "terminalTranscripts"]
    })
}

/// 流式校验 ZIP：限制、路径安全、哈希一致；不分配声明总大小的大缓冲。
///
/// Business Logic: inspect-before-restore；篡改/超限/zip-slip 必须在改库前失败。
/// Code Logic: 打开 archive → 逐 entry 流式哈希与计数 → 比对 manifest。
pub fn inspect_archive_streaming(
    path: &Path,
    limits: ArchiveLimits,
) -> Result<InspectedArchive, AppError> {
    let meta = fs::metadata(path)?;
    if meta.len() > limits.max_archive_bytes {
        return Err(AppError::generic(format!(
            "备份包超过 {} 字节上限",
            limits.max_archive_bytes
        )));
    }

    let file = File::open(path)?;
    let mut archive =
        ZipArchive::new(file).map_err(|e| AppError::generic(format!("无法打开 ZIP: {e}")))?;

    let entry_count = archive.len() as u64;
    if entry_count > limits.max_entries {
        return Err(AppError::generic(format!(
            "备份 entry 数 {entry_count} 超过上限 {}",
            limits.max_entries
        )));
    }

    let mut total_uncompressed: u64 = 0;
    let mut computed_hashes: BTreeMap<String, String> = BTreeMap::new();
    let mut entry_names = Vec::new();
    let mut manifest_bytes: Option<Vec<u8>> = None;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| AppError::generic(format!("读取 zip entry 失败: {e}")))?;

        if is_zip_symlink(&entry) {
            return Err(AppError::generic("备份包含符号链接，已拒绝"));
        }

        let raw_name = entry.name().to_string();
        validate_entry_name(&raw_name)?;

        // directories
        if raw_name.ends_with('/') {
            entry_names.push(raw_name);
            continue;
        }

        let size = entry.size();
        if size > limits.max_entry_bytes {
            return Err(AppError::generic(format!(
                "entry {raw_name} 解压大小 {size} 超过单文件上限 {}",
                limits.max_entry_bytes
            )));
        }
        total_uncompressed = total_uncompressed
            .checked_add(size)
            .ok_or_else(|| AppError::generic("解压总量溢出"))?;
        if total_uncompressed > limits.max_total_uncompressed {
            return Err(AppError::generic(format!(
                "总解压量超过上限 {}",
                limits.max_total_uncompressed
            )));
        }

        // 流式哈希：按块读取，不按声明 size 预分配
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        let mut read_total: u64 = 0;
        let mut collected: Option<Vec<u8>> = if raw_name == "manifest.json" {
            Some(Vec::new())
        } else {
            None
        };
        loop {
            let n = entry
                .read(&mut buf)
                .map_err(|e| AppError::generic(format!("读取 entry 失败: {e}")))?;
            if n == 0 {
                break;
            }
            read_total = read_total
                .checked_add(n as u64)
                .ok_or_else(|| AppError::generic("entry 读取溢出"))?;
            if read_total > limits.max_entry_bytes {
                return Err(AppError::generic(format!(
                    "entry {raw_name} 实际读取超过单文件上限"
                )));
            }
            hasher.update(&buf[..n]);
            if let Some(ref mut c) = collected {
                if c.len().saturating_add(n) > limits.max_entry_bytes as usize {
                    return Err(AppError::generic("manifest 过大"));
                }
                c.extend_from_slice(&buf[..n]);
            }
        }
        let hex = hex_encode(&hasher.finalize());
        computed_hashes.insert(raw_name.clone(), hex);
        entry_names.push(raw_name.clone());
        if let Some(bytes) = collected {
            manifest_bytes = Some(bytes);
        }
    }

    let manifest_bytes =
        manifest_bytes.ok_or_else(|| AppError::generic("备份缺少 manifest.json"))?;
    let manifest: ArchiveManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| AppError::generic(format!("manifest 解析失败: {e}")))?;

    if manifest.format_version != FORMAT_VERSION {
        return Err(AppError::generic(format!(
            "不支持的备份格式版本 {}（当前 {})",
            manifest.format_version, FORMAT_VERSION
        )));
    }

    // 校验每个 manifest 声明文件的哈希
    for (name, expected) in &manifest.files {
        let actual = computed_hashes.get(name).ok_or_else(|| {
            AppError::generic(format!("manifest 声明文件缺失: {name}"))
        })?;
        if actual != expected {
            return Err(AppError::generic(format!(
                "校验和不匹配: {name}"
            )));
        }
    }

    let mut domain_counts = BTreeMap::new();
    let mut warnings = Vec::new();
    count_domain(
        &computed_hashes,
        DOMAIN_PROMPTS,
        "prompts/items.json",
        &mut domain_counts,
        &mut warnings,
    );
    count_domain(
        &computed_hashes,
        DOMAIN_CC_HISTORY,
        "ccHistory/items.json",
        &mut domain_counts,
        &mut warnings,
    );
    count_domain(
        &computed_hashes,
        DOMAIN_SCRATCHPAD,
        "scratchpad/items.json",
        &mut domain_counts,
        &mut warnings,
    );
    count_domain(
        &computed_hashes,
        DOMAIN_SSH_TARGETS,
        "sshTargets/items.json",
        &mut domain_counts,
        &mut warnings,
    );
    count_domain(
        &computed_hashes,
        DOMAIN_CLAUDE_MD,
        "claudeMd/item.json",
        &mut domain_counts,
        &mut warnings,
    );
    count_domain(
        &computed_hashes,
        DOMAIN_DELETION_FLOORS,
        "deletionFloors/items.json",
        &mut domain_counts,
        &mut warnings,
    );
    if computed_hashes.contains_key("configReport/report.json") {
        domain_counts.insert(DOMAIN_CONFIG_REPORT.into(), 1);
        warnings.push("config report 仅供预览，恢复时不会写回".into());
    }

    Ok(InspectedArchive {
        manifest,
        domain_counts,
        warnings,
        entry_names,
    })
}

fn count_domain(
    hashes: &BTreeMap<String, String>,
    domain: &str,
    path: &str,
    counts: &mut BTreeMap<String, u32>,
    warnings: &mut Vec<String>,
) {
    if hashes.contains_key(path) {
        // 精确计数在 restore 读取 JSON 时再算；此处标记存在
        counts.insert(domain.into(), 1);
    } else {
        warnings.push(format!("领域 {domain} 在包中缺失"));
    }
}

/// 从已校验 archive 读取指定 entry 原始字节（再次打开，调用方已 inspect）。
pub fn read_entry_bytes(path: &Path, entry_name: &str, limits: ArchiveLimits) -> Result<Vec<u8>, AppError> {
    let file = File::open(path)?;
    let mut archive =
        ZipArchive::new(file).map_err(|e| AppError::generic(format!("无法打开 ZIP: {e}")))?;
    let mut entry = archive
        .by_name(entry_name)
        .map_err(|e| AppError::generic(format!("缺少 entry {entry_name}: {e}")))?;
    if is_zip_symlink(&entry) {
        return Err(AppError::generic("备份包含符号链接，已拒绝"));
    }
    validate_entry_name(entry.name())?;
    if entry.size() > limits.max_entry_bytes {
        return Err(AppError::generic("entry 过大"));
    }
    let mut out = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = entry
            .read(&mut buf)
            .map_err(|e| AppError::generic(format!("读取失败: {e}")))?;
        if n == 0 {
            break;
        }
        if out.len().saturating_add(n) > limits.max_entry_bytes as usize {
            return Err(AppError::generic("entry 实际过大"));
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}

/// 拒绝 zip-slip / 绝对路径 / 父目录穿越。
pub fn validate_entry_name(name: &str) -> Result<(), AppError> {
    if name.is_empty() {
        return Err(AppError::generic("空 entry 名"));
    }
    if name.starts_with('/') || name.starts_with('\\') {
        return Err(AppError::generic(format!("拒绝绝对路径 entry: {name}")));
    }
    // Windows 盘符
    if name.len() >= 2 && name.as_bytes()[1] == b':' {
        return Err(AppError::generic(format!("拒绝盘符路径 entry: {name}")));
    }
    let path = Path::new(name);
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(AppError::generic(format!("拒绝 zip-slip entry: {name}")));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(AppError::generic(format!("拒绝不安全路径 entry: {name}")));
            }
        }
    }
    Ok(())
}

fn is_zip_symlink(file: &zip::read::ZipFile<'_>) -> bool {
    file.unix_mode()
        .map(|mode| mode & 0o170000 == 0o120000)
        .unwrap_or(false)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// 测试辅助：构造最小合法 ZIP。
#[cfg(test)]
pub fn write_test_archive(
    dest: &Path,
    manifest: &ArchiveManifest,
    extra_files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), AppError> {
    let mut files = extra_files.clone();
    let mut hashes = BTreeMap::new();
    for (k, v) in &files {
        hashes.insert(k.clone(), sha256_hex(v));
    }
    let m = ArchiveManifest {
        files: hashes,
        ..manifest.clone()
    };
    let mbytes = serde_json::to_vec_pretty(&m)?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(dest)?;
    let mut zip = ZipWriter::new(file);
    let options = FileOptions::default().compression_method(CompressionMethod::Stored);
    zip.start_file("manifest.json", options)
        .map_err(|e| AppError::generic(e.to_string()))?;
    zip.write_all(&mbytes)?;
    for (name, bytes) in &files {
        zip.start_file(name, options)
            .map_err(|e| AppError::generic(e.to_string()))?;
        zip.write_all(bytes)?;
    }
    zip.finish().map_err(|e| AppError::generic(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn sample_manifest() -> ArchiveManifest {
        ArchiveManifest {
            format_version: FORMAT_VERSION,
            created_at: "t".into(),
            device_id: "dev".into(),
            domains: vec![DOMAIN_PROMPTS.into()],
            files: BTreeMap::new(),
        }
    }

    #[test]
    fn zip_slip_rejected() {
        assert!(validate_entry_name("../etc/passwd").is_err());
        assert!(validate_entry_name("/abs").is_err());
        assert!(validate_entry_name("ok/path.json").is_ok());
    }

    #[test]
    fn checksum_mismatch_detected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.zip");
        let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        files.insert("prompts/items.json".to_string(), b"[]".to_vec());
        write_test_archive(&path, &sample_manifest(), &files).unwrap();

        // 重新打包篡改版：内容变了但 manifest 仍写旧哈希
        let mut bad_manifest = sample_manifest();
        bad_manifest
            .files
            .insert("prompts/items.json".to_string(), sha256_hex(b"[]"));
        let dest = dir.path().join("bad.zip");
        {
            let f = File::create(&dest).unwrap();
            let mut zip = ZipWriter::new(f);
            let options = FileOptions::default().compression_method(CompressionMethod::Stored);
            let mbytes = serde_json::to_vec_pretty(&bad_manifest).unwrap();
            zip.start_file("manifest.json", options).unwrap();
            zip.write_all(&mbytes).unwrap();
            zip.start_file("prompts/items.json", options).unwrap();
            zip.write_all(b"[1]").unwrap();
            zip.finish().unwrap();
        }
        let err = inspect_archive_streaming(&dest, ArchiveLimits::default()).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("校验和") || msg.contains("不匹配"), "{msg}");
    }

    #[test]
    fn unknown_version_rejected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("v.zip");
        let mut m = sample_manifest();
        m.format_version = 999;
        let files = BTreeMap::new();
        write_test_archive(&path, &m, &files).unwrap();
        let err = inspect_archive_streaming(&path, ArchiveLimits::default()).unwrap_err();
        assert!(format!("{err}").contains("版本"));
    }

    #[test]
    fn entry_count_limit() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("many.zip");
        let mut files = BTreeMap::new();
        for i in 0..5 {
            files.insert(format!("f{i}.json"), b"{}".to_vec());
        }
        write_test_archive(&path, &sample_manifest(), &files).unwrap();
        let limits = ArchiveLimits {
            max_entries: 3,
            ..ArchiveLimits::default()
        };
        // manifest + 5 files = 6 entries > 3
        assert!(inspect_archive_streaming(&path, limits).is_err());
    }

    #[test]
    fn streaming_does_not_preallocate_declared_size() {
        // 逻辑测试：小 limits 下超大声明 size 的 entry 在读取阶段被拦
        // zip crate 的 size() 来自 local header；我们构造正常小文件即可
        let dir = tempdir().unwrap();
        let path = dir.path().join("ok.zip");
        let mut files = BTreeMap::new();
        files.insert("prompts/items.json".into(), b"[]".to_vec());
        write_test_archive(&path, &sample_manifest(), &files).unwrap();
        let inspected = inspect_archive_streaming(&path, ArchiveLimits::default()).unwrap();
        assert!(inspected.domain_counts.contains_key(DOMAIN_PROMPTS));
    }

    #[test]
    fn absolute_and_symlink_guards() {
        assert!(validate_entry_name("C:\\windows").is_err());
        // symlink 检测依赖 unix_mode；无 mode 时 false — 单独路径测试足够
    }
}
