//! backup_restore_smoke — 可验证导出/恢复最小黑盒 smoke。
//!
//! Business Logic（为什么需要这个测试）:
//!     N2 Task6 要求 inspect 在坏包上 fail-closed，合法最小包能通过流式校验；
//!     集成 smoke 不依赖完整 AppState harness，覆盖 archive 公共 API。
//!
//! Code Logic（这个模块做什么）:
//!     (a) 手工坏 ZIP（checksum 不匹配）→ inspect_archive_streaming 拒绝；
//!     (b) 手工合法 ZIP（manifest 哈希与内容一致）→ inspect 成功且 format_version 对齐。

use app_lib::backup::{
    inspect_archive_streaming, ArchiveLimits, ArchiveManifest, DOMAIN_PROMPTS, FORMAT_VERSION,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use tempfile::tempdir;
use zip::write::FileOptions;
use zip::CompressionMethod;
use zip::ZipWriter;

/// 计算与 archive 模块一致的 sha256 hex。
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// 写入最小 ZIP：manifest.json + 若干 entry 原始字节。
fn write_zip(
    path: &std::path::Path,
    manifest: &ArchiveManifest,
    files: &BTreeMap<String, Vec<u8>>,
) {
    let file = File::create(path).expect("create zip");
    let mut zip = ZipWriter::new(file);
    let options = FileOptions::default().compression_method(CompressionMethod::Stored);
    let mbytes = serde_json::to_vec_pretty(manifest).expect("manifest json");
    zip.start_file("manifest.json", options)
        .expect("manifest entry");
    zip.write_all(&mbytes).expect("write manifest");
    for (name, bytes) in files {
        zip.start_file(name.as_str(), options).expect("file entry");
        zip.write_all(bytes).expect("write file");
    }
    zip.finish().expect("finish zip");
}

/// 坏包 inspect 必须拒绝（checksum mismatch）。
#[test]
fn bad_zip_inspect_rejects() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("bad.zip");
    let payload = b"[1]";
    let mut files = BTreeMap::new();
    files.insert("prompts/items.json".into(), payload.to_vec());

    let mut manifest = ArchiveManifest {
        format_version: FORMAT_VERSION,
        created_at: "t".into(),
        device_id: "d".into(),
        domains: vec![DOMAIN_PROMPTS.into()],
        files: BTreeMap::new(),
    };
    // 故意写入与真实内容不一致的哈希
    manifest
        .files
        .insert("prompts/items.json".into(), sha256_hex(b"[]"));

    write_zip(&path, &manifest, &files);
    let err = inspect_archive_streaming(&path, ArchiveLimits::default())
        .expect_err("checksum mismatch must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("校验")
            || msg.contains("checksum")
            || msg.contains("hash")
            || msg.contains("不匹配"),
        "unexpected error: {msg}"
    );
}

/// 合法最小包 inspect 成功。
#[test]
fn good_zip_inspect_succeeds() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("good.zip");
    let payload = serde_json::to_vec(&json!([])).expect("empty array");
    let mut files = BTreeMap::new();
    files.insert("prompts/items.json".into(), payload.clone());

    let mut manifest = ArchiveManifest {
        format_version: FORMAT_VERSION,
        created_at: "2026-07-15T00:00:00Z".into(),
        device_id: "dev-smoke".into(),
        domains: vec![DOMAIN_PROMPTS.into()],
        files: BTreeMap::new(),
    };
    manifest
        .files
        .insert("prompts/items.json".into(), sha256_hex(&payload));

    write_zip(&path, &manifest, &files);
    let inspected =
        inspect_archive_streaming(&path, ArchiveLimits::default()).expect("good zip inspect");
    assert_eq!(inspected.manifest.format_version, FORMAT_VERSION);
    assert!(
        inspected
            .domain_counts
            .keys()
            .any(|k| k == DOMAIN_PROMPTS || k.contains("prompt")),
        "expected prompts domain in {:?}",
        inspected.domain_counts
    );
}
