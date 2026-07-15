//! receiver/finalize — finalize intent journal 与原子落盘
//!
//! Business Logic: place→history 崩溃窗口靠 intent 恢复；并发同名靠 no-replace place。
//! Code Logic: durable intent 写清 + hard_link/rename_no_replace + certify + history 晋升。

use super::{
    compute_sha256_nofollow, ensure_path_within_dir, normalize_path, open_regular_file_nofollow,
    open_regular_file_nofollow_std, resolve_filename, sanitize_receive_basename,
};
use crate::error::AppError;
use crate::models::transfer::{TransferDirection, TransferStatus, TransferTask};
use crate::state::AppState;
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

/// finalize 落盘意图 journal 目录名（位于 receive_dir 下）。
pub(super) const FINALIZE_INTENT_DIR: &str = ".cc-partner-transfer-intents";

/// 落盘前写入的 durable intent（跨进程恢复 place→history 窗口）。
///
/// Business Logic（为什么需要这个结构）:
///     最终文件可能已 place 成功，但 transfer_history 尚未写入；进程崩溃后内存 active/墓碑丢失，
///     若不保留 intent，handle_init 会接受同一 transfer_id 并产生后缀重复文件。
///
/// Code Logic（这个结构做什么）:
///     序列化为 receive_dir 下 `.cc-partner-transfer-intents/<transfer_id>.json`；
///     含 transfer 元数据与候选 final_path；history+墓碑成功后删除。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct FinalizeIntent {
    pub(crate) transfer_id: String,
    pub(crate) filename: String,
    pub(crate) size: u64,
    pub(crate) sha256: String,
    pub(crate) chunk_size: u64,
    pub(crate) final_filename: String,
    pub(crate) final_path: String,
    pub(crate) created_at: String,
}

/// 当前时间 RFC3339 ISO 字符串。
pub(super) fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

/// 完成传输：SHA256 校验临时文件，通过则原子落盘（处理冲突）+ 写历史；失败标记 failed。
///
/// Business Logic: 文件全部接收后需校验完整性，确保无误后落地为最终文件名。
///     并发同名不同 transfer_id 不得互相覆盖：resolve_filename 到 place 必须在 receive_dir 锁内，
///     并用 exclusive create + 失败重试后缀，禁止静默 replace。
///     落盘成功与 transfer_history 持久化之间不得出现“已 completed 但无 durable 终态”窗口：
///     否则崩溃/DB 失败会导致重启后 complete=false、status=unknown、发送端假失败并后缀重试。
/// Code Logic:
///     1. 仅处理 direction=Receive 的任务；
///     2. 若 active 已是 Completed（落盘成功但 history 未写入），只重试 durable 晋升；
///     3. 计算 .tmp 的 SHA256，与任务记录的 sha256 比较；
///     4. 校验失败：标记 failed + 删除 .tmp + emit failed；
///     5. 校验通过：持 receive_dir 锁，**每个候选**严格 resolve → 写 intent → no-replace place；
///     6. 先 mark_completed 保留 active（可恢复），再 durable record；成功后才 remove + 墓碑 + emit。
pub async fn finalize_transfer(state: &AppState, transfer_id: &str) -> Result<(), AppError> {
    let task = match state.transfers.get(transfer_id) {
        Some(t) => t,
        None => return Ok(()),
    };

    // 防御：即便 chunk 漏检，finalize 也不得操作 Send 源文件（哈希失败会删、成功会 move）。
    if task.direction != TransferDirection::Receive {
        tracing::warn!(
            "finalize 拒绝非 Receive 任务: {transfer_id}, direction={:?}",
            task.direction
        );
        return Ok(());
    }

    // 落盘已成功但 history 未 durable：在同一绑定句柄上重做 len+SHA+fsync，并确认目录项身份未替换，
    // 再晋升，禁止二次 place 或假报 completed。
    if task.status == TransferStatus::Completed {
        return promote_completed_to_durable(state, transfer_id, &task).await;
    }

    let tmp_path = PathBuf::from(&task.file_path);

    // 校验 SHA256（no-follow：禁止 symlink tmp 把哈希指到 receive_dir 外任意文件）。
    let actual = match compute_sha256_nofollow(&tmp_path).await {
        Ok(h) => h,
        Err(e) => {
            on_receive_failed(state, transfer_id, &format!("读取临时文件失败: {e}")).await;
            return Ok(());
        }
    };

    if actual != task.sha256 {
        // 校验失败：删除损坏的临时文件
        let _ = tokio::fs::remove_file(&tmp_path).await;
        on_receive_failed(
            state,
            transfer_id,
            &format!("SHA256 校验失败: 期望={}, 实际={actual}", task.sha256),
        )
        .await;
        return Ok(());
    }

    // 解析文件名冲突并原子落盘；落盘前再次校验 basename 与 receive_dir 边界。
    let receive_dir = PathBuf::from(&state.config.read().expect("config 读锁中毒").receive_dir);
    let safe_filename = match sanitize_receive_basename(&task.filename, "filename") {
        Ok(name) => name,
        Err(e) => {
            on_receive_failed(state, transfer_id, &format!("非法文件名: {e}")).await;
            return Ok(());
        }
    };

    // receive_dir 锁覆盖 resolve → write intent → exclusive place，防止并发同名 TOCTOU 覆盖。
    let dir_lock = state.transfers.receive_dir_lock();
    let _dir_guard = dir_lock.lock().await;

    // 每个候选（首选与后缀）严格：resolve → 持久化 intent → no-replace place。
    // 禁止先 place 再补写 intent，也禁止 place 前删除 intent（place 成功但 history 前崩溃会丢 journal）。
    let placed = match place_final_file_with_intent(
        &receive_dir,
        &safe_filename,
        &tmp_path,
        transfer_id,
        &task,
    )
    .await
    {
        Ok(p) => p,
        Err(PlaceFinalError::DurabilityPending { placed, message }) => {
            // 文件已落地：保留 intent + Completed active，返回可重试错误，禁止 Failed history。
            tracing::error!(
                "receive place durability pending: {transfer_id} -> {} ({message})",
                placed.final_path.display()
            );
            let completed_at = now_iso();
            state.transfers.mark_completed(
                transfer_id,
                completed_at,
                Some(placed.final_path.to_string_lossy().to_string()),
            );
            return Err(AppError::unavailable(format!(
                "最终文件已落地但 durability 未完成，请重试 complete: {transfer_id}: {message}"
            )));
        }
        Err(PlaceFinalError::Unplaced(e)) => {
            on_receive_failed(state, transfer_id, &format!("原子落盘失败: {e}")).await;
            return Ok(());
        }
    };
    let placed_path = placed.final_path;
    tracing::debug!(
        "receive exclusive place ok: {transfer_id} -> {} ({})",
        placed.final_filename,
        placed_path.display()
    );

    // 先把 active 标为 Completed 并写入最终路径，但**不** remove / 不写墓碑 / 不 emit completed。
    // history 成功前对发送端仍表现为 transferring，complete/chunk 可重试 durable 晋升。
    let completed_at = now_iso();
    state.transfers.mark_completed(
        transfer_id,
        completed_at,
        Some(placed_path.to_string_lossy().to_string()),
    );
    let completed_task = state.transfers.get(transfer_id).ok_or_else(|| {
        AppError::generic(format!(
            "落盘后丢失 active 任务，无法写入 transfer_history: {transfer_id}"
        ))
    })?;
    promote_completed_to_durable(state, transfer_id, &completed_task).await
}

/// Business Logic（为什么需要这个函数）:
///     最终文件已落地后，只有 transfer_history 成功才可向发送端/UI 宣告 completed。
///     record 失败或崩溃窗口内必须保留可恢复 active，禁止“文件在、终态丢”。
///
/// Code Logic（这个函数做什么）:
///     要求 active 已是 Receive+Completed；`transfer_repo.record` 成功后才 remove、
///     写成功墓碑并 emit `transfer:completed`；record 失败保留 active 并返回 Err。
pub(super) async fn promote_completed_to_durable(
    state: &AppState,
    transfer_id: &str,
    task: &TransferTask,
) -> Result<(), AppError> {
    if task.direction != TransferDirection::Receive {
        return Err(AppError::validation(format!(
            "仅 Receive 任务可晋升 durable 终态: {transfer_id}"
        )));
    }
    if task.status != TransferStatus::Completed {
        return Err(AppError::generic(format!(
            "任务尚未 Completed，不能晋升 durable: {transfer_id}"
        )));
    }

    // 写 Completed history 前必须在同一绑定句柄上证明 len/SHA/fsync，并确认父目录项身份未替换。
    // 禁止仅按路径 reopen+fsync：普通文件原子替换后 no-follow 仍会通过，导致静默错误认证。
    let final_path = PathBuf::from(&task.file_path);
    if let Err(e) = certify_final_file_for_history(&final_path, task.size, &task.sha256).await {
        return Err(AppError::unavailable(format!(
            "最终文件已落地但 durability 未完成，请重试 complete: {transfer_id}: {e}"
        )));
    }

    state.transfer_repo.record(task).await.map_err(|e| {
        tracing::error!("transfer_history 写入失败，保留 active 等待重试: {transfer_id}, {e}");
        e
    })?;

    let final_path_str = final_path.to_string_lossy().to_string();
    let final_filename = final_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(task.filename.as_str())
        .to_string();
    let completed_at = task.completed_at.clone().unwrap_or_else(now_iso);

    state.transfers.remove(transfer_id);
    state.transfers.record_tombstone(
        transfer_id,
        crate::transfer::registry::TransferTombstone {
            outcome: crate::transfer::registry::TransferOutcome::Completed {
                final_filename,
                file_path: final_path_str.clone(),
            },
            received_bytes: task.size,
            size: task.size,
            filename: task.filename.clone(),
            completed_at,
            created_at: std::time::Instant::now(),
        },
    );

    // history + 墓碑已就绪：清除跨崩溃 intent journal。
    let receive_dir = PathBuf::from(&state.config.read().expect("config 读锁中毒").receive_dir);
    if let Err(e) = clear_finalize_intent(&receive_dir, transfer_id).await {
        tracing::warn!("清除 finalize intent 失败（可忽略）: {transfer_id}, {e}");
    }

    state.emit_event(
        "transfer:completed",
        serde_json::json!({
            "id": transfer_id,
            "status": "completed",
            "filePath": final_path_str,
        }),
    );

    tracing::info!("文件接收完成: {transfer_id} -> {final_path_str}");
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     place 前必须把候选 final 路径持久化到 journal，才能在进程崩溃后避免同 transfer_id 重传后缀副本。
///     intent 目录/文件若是 symlink/reparse，普通 create_dir_all/write 会跟随写出 receive_dir；
///     路径检查后再 create/rename 仍有父目录交换 TOCTOU，必须绑定已验证目录 HANDLE/fd。
///
/// Code Logic（这个函数做什么）:
///     Unix：open 已验证 receive_dir fd → openat/mkdirat 绑定 intent 目录 → openat 创建 tmp 写字节
///     + sync_all → renameat 正式 intent → fsync intent 目录；
///     Windows：CreateFile 打开 receive_dir（BACKUP_SEMANTICS）→ NtCreateFile 相对路径创建/打开
///     普通 intent 目录（OPEN_REPARSE_POINT 后拒绝 reparse）→ 相对路径 create/write/
///     NtSetInformationFile(FileRenameInformation=10, RootDirectory=intent HANDLE)/unlink，
///     检查后不再用绝对 path 做 create/rename/delete。
pub(super) async fn write_finalize_intent(
    receive_dir: &Path,
    intent: &FinalizeIntent,
) -> Result<(), AppError> {
    // 词法边界校验（basename/逃逸）；真正写入走 dirfd / directory HANDLE，不再 path-ops。
    let _path = finalize_intent_path(receive_dir, &intent.transfer_id)?;
    let safe_id = sanitize_receive_basename(&intent.transfer_id, "transfer_id")?;
    let intent_name = format!("{safe_id}.json");
    let tmp_name = format!("{safe_id}.json.tmp");
    let bytes = serde_json::to_vec_pretty(intent)?;

    #[cfg(unix)]
    {
        let joined = tokio::task::spawn_blocking({
            let receive_dir = receive_dir.to_path_buf();
            let intent_name = intent_name.clone();
            let tmp_name = tmp_name.clone();
            let bytes = bytes.clone();
            move || write_finalize_intent_unix_dirfd(&receive_dir, &intent_name, &tmp_name, &bytes)
        })
        .await
        .map_err(|e| AppError::generic(format!("write_finalize_intent join 失败: {e}")))?;
        joined
    }

    #[cfg(windows)]
    {
        let joined = tokio::task::spawn_blocking({
            let receive_dir = receive_dir.to_path_buf();
            let intent_name = intent_name.clone();
            let tmp_name = tmp_name.clone();
            let bytes = bytes.clone();
            move || {
                write_finalize_intent_windows_handle(&receive_dir, &intent_name, &tmp_name, &bytes)
            }
        })
        .await
        .map_err(|e| AppError::generic(format!("write_finalize_intent join 失败: {e}")))?;
        joined
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (receive_dir, intent_name, tmp_name, bytes);
        Err(AppError::generic(
            "当前平台无法以目录句柄相对路径写 finalize intent".to_string(),
        ))
    }
}

/// Business Logic（为什么需要这个函数）:
///     finalize intent 目录若被替换为指向 receive_dir 外的 symlink，create_dir_all/write 会跟随逃逸。
///
/// Code Logic（这个函数做什么）:
///     对 receive_dir 下 FINALIZE_INTENT_DIR：不存在则 create_dir（目录项本身）；存在则 symlink_metadata
///     拒绝 symlink/非目录；返回校验后的 path。
///     生产写路径已改用 Unix dirfd / Windows directory HANDLE；本 helper 仅保留给单测与诊断。
#[allow(dead_code)]
pub(super) async fn ensure_regular_intent_dir(receive_dir: &Path) -> Result<PathBuf, AppError> {
    let dir = receive_dir.join(FINALIZE_INTENT_DIR);
    ensure_path_within_dir(receive_dir, &dir)?;
    match tokio::fs::symlink_metadata(&dir).await {
        Ok(meta) => {
            let ft = meta.file_type();
            if ft.is_symlink() {
                return Err(AppError::validation(format!(
                    "intent 目录是符号链接，拒绝写入: {}",
                    dir.display()
                )));
            }
            if !ft.is_dir() {
                return Err(AppError::validation(format!(
                    "intent 路径不是目录: {}",
                    dir.display()
                )));
            }
            Ok(dir)
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {
            match tokio::fs::create_dir(&dir).await {
                Ok(()) => {}
                Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                    return Box::pin(ensure_regular_intent_dir(receive_dir)).await;
                }
                Err(err) => return Err(AppError::from(err)),
            }
            match tokio::fs::symlink_metadata(&dir).await {
                Ok(meta) => {
                    let ft = meta.file_type();
                    if ft.is_symlink() || !ft.is_dir() {
                        return Err(AppError::validation(format!(
                            "intent 目录创建后不是普通目录: {}",
                            dir.display()
                        )));
                    }
                    Ok(dir)
                }
                Err(err) => Err(AppError::from(err)),
            }
        }
        Err(e) => Err(AppError::from(e)),
    }
}

/// Business Logic（为什么需要这个函数）:
///     写 intent 临时文件时若路径已是指向外部的 symlink，tokio::fs::write 会跟随并截断外部文件。
///     create_new 后若关闭再按路径重开，攻击者可在两次 open 之间把临时文件换成 hardlink/普通文件。
///
/// Code Logic（这个函数做什么）:
///     先 remove 残留同名 tmp；create_new 打开后不丢弃句柄，write_all + flush + sync_all。
///     生产 intent 写入已改用目录句柄相对路径；本 helper 供单测验证 create_new 语义。
#[allow(dead_code)]
pub(super) async fn write_bytes_create_new_nofollow(
    path: &Path,
    bytes: &[u8],
) -> Result<(), AppError> {
    let _ = tokio::fs::remove_file(path).await;
    let created = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(f) => f,
        Err(e) => {
            return Err(AppError::from(std::io::Error::new(
                e.kind(),
                format!("创建 intent 临时文件失败 {}: {e}", path.display()),
            )));
        }
    };
    let mut file = tokio::fs::File::from_std(created);
    if let Err(err) = file.write_all(bytes).await {
        let _ = tokio::fs::remove_file(path).await;
        return Err(AppError::from(err));
    }
    if let Err(err) = file.flush().await {
        let _ = tokio::fs::remove_file(path).await;
        return Err(AppError::from(err));
    }
    if let Err(err) = file.sync_all().await {
        let _ = tokio::fs::remove_file(path).await;
        return Err(AppError::from(err));
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     rename/hardlink/unlink 后若不同步父目录，断电可能丢失目录项，而 SQLite history 已 completed。
///
/// Code Logic（这个函数做什么）:
///     打开目录并 fsync（Unix）或 FlushFileBuffers（Windows）；失败上抛。
///     Windows 必须用可写目录句柄（GENERIC_WRITE / write access）：`FlushFileBuffers`
///     要求写权限，只读句柄会 AccessDenied，导致 durability 永远无法晋升。
pub(super) fn fsync_dir(dir: &Path) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let file = std::fs::File::open(dir).map_err(|e| {
            AppError::from(std::io::Error::new(
                e.kind(),
                format!("打开目录以 fsync 失败 {}: {e}", dir.display()),
            ))
        })?;
        let rc = unsafe { libc::fsync(file.as_raw_fd()) };
        if rc != 0 {
            return Err(AppError::from(std::io::Error::last_os_error()));
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_BACKUP_SEMANTICS 才能打开目录句柄；OPEN_REPARSE_POINT 拒绝跟随 junction。
        // write(true) 申请 GENERIC_WRITE，满足 FlushFileBuffers 权限要求。
        pub(super) const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        pub(super) const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(dir)
            .map_err(|e| {
                AppError::from(std::io::Error::new(
                    e.kind(),
                    format!("打开目录以 FlushFileBuffers 失败 {}: {e}", dir.display()),
                ))
            })?;
        // 目录 HANDLE 上 sync_all ≈ FlushFileBuffers（需 GENERIC_WRITE）。
        file.sync_all().map_err(AppError::from)?;
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Business Logic（为什么需要这个函数）:
///     place 前必须把接收 tmp 的数据刷到稳定存储，否则断电可能只剩 history completed 而文件内容丢失。
///
/// Code Logic（这个函数做什么）:
///     no-follow 以可写方式打开普通文件后 sync_all（含元数据）。
///     Windows 上 `sync_all` → FlushFileBuffers 需要 GENERIC_WRITE，只读句柄会 AccessDenied。
pub(super) async fn sync_regular_file(path: &Path) -> Result<(), AppError> {
    let file = open_regular_file_nofollow(path, true).await?;
    // tokio File sync_all
    let std_file = file.into_std().await;
    std_file.sync_all().map_err(AppError::from)?;
    Ok(())
}

/// Unix：在已验证 receive_dir fd 上 openat/mkdirat/renameat 写 intent，关闭父目录交换 TOCTOU。
#[cfg(unix)]
pub(super) fn write_finalize_intent_unix_dirfd(
    receive_dir: &Path,
    intent_name: &str,
    tmp_name: &str,
    bytes: &[u8],
) -> Result<(), AppError> {
    use std::ffi::CString;
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::{AsRawFd, FromRawFd};

    let receive_c = CString::new(receive_dir.as_os_str().as_bytes())
        .map_err(|_| AppError::validation("receive_dir 含内部 NUL"))?;
    let intent_dir_c =
        CString::new(FINALIZE_INTENT_DIR).map_err(|_| AppError::validation("intent dir 名非法"))?;
    let intent_c = CString::new(intent_name.as_bytes())
        .map_err(|_| AppError::validation("intent 文件名含内部 NUL"))?;
    let tmp_c = CString::new(tmp_name.as_bytes())
        .map_err(|_| AppError::validation("intent tmp 名含内部 NUL"))?;

    // O_DIRECTORY|O_RDONLY；若 receive_dir 是 symlink，open 跟随其目标——配置路径可信，关键是 intent 子项不逃逸。
    let receive_fd = unsafe {
        libc::open(
            receive_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if receive_fd < 0 {
        return Err(AppError::from(std::io::Error::last_os_error()));
    }
    let receive_file = unsafe { std::fs::File::from_raw_fd(receive_fd) };

    // mkdirat intent 目录；EEXIST 时 openat 校验为普通目录。
    let mkdir_rc = unsafe { libc::mkdirat(receive_file.as_raw_fd(), intent_dir_c.as_ptr(), 0o700) };
    if mkdir_rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EEXIST) {
            return Err(AppError::from(err));
        }
    }
    let intent_dir_fd = unsafe {
        libc::openat(
            receive_file.as_raw_fd(),
            intent_dir_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if intent_dir_fd < 0 {
        let err = std::io::Error::last_os_error();
        // O_NOFOLLOW 遇 symlink → ELOOP/EPERM
        return Err(AppError::validation(format!(
            "intent 目录是符号链接或不可绑定的普通目录: {err}"
        )));
    }
    let intent_dir_file = unsafe { std::fs::File::from_raw_fd(intent_dir_fd) };

    // 清残留 tmp 目录项（unlinkat 不跟随）。
    let _ = unsafe { libc::unlinkat(intent_dir_file.as_raw_fd(), tmp_c.as_ptr(), 0) };

    let tmp_fd = unsafe {
        libc::openat(
            intent_dir_file.as_raw_fd(),
            tmp_c.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if tmp_fd < 0 {
        return Err(AppError::from(std::io::Error::last_os_error()));
    }
    let mut tmp_file = unsafe { std::fs::File::from_raw_fd(tmp_fd) };
    if let Err(err) = tmp_file.write_all(bytes) {
        let _ = unsafe { libc::unlinkat(intent_dir_file.as_raw_fd(), tmp_c.as_ptr(), 0) };
        return Err(AppError::from(err));
    }
    if let Err(err) = tmp_file.flush() {
        let _ = unsafe { libc::unlinkat(intent_dir_file.as_raw_fd(), tmp_c.as_ptr(), 0) };
        return Err(AppError::from(err));
    }
    if let Err(err) = tmp_file.sync_all() {
        let _ = unsafe { libc::unlinkat(intent_dir_file.as_raw_fd(), tmp_c.as_ptr(), 0) };
        return Err(AppError::from(err));
    }
    drop(tmp_file);

    // 覆盖正式 intent：先 unlink 旧目录项，再 renameat。
    let _ = unsafe { libc::unlinkat(intent_dir_file.as_raw_fd(), intent_c.as_ptr(), 0) };
    let rename_rc = unsafe {
        libc::renameat(
            intent_dir_file.as_raw_fd(),
            tmp_c.as_ptr(),
            intent_dir_file.as_raw_fd(),
            intent_c.as_ptr(),
        )
    };
    if rename_rc != 0 {
        let err = std::io::Error::last_os_error();
        let _ = unsafe { libc::unlinkat(intent_dir_file.as_raw_fd(), tmp_c.as_ptr(), 0) };
        return Err(AppError::from(err));
    }
    // fsync intent 目录，保证 rename 目录项断电后可见。
    let rc = unsafe { libc::fsync(intent_dir_file.as_raw_fd()) };
    if rc != 0 {
        return Err(AppError::from(std::io::Error::last_os_error()));
    }
    // 保持 receive_file 活到此处，避免 fd 过早关闭。
    drop(intent_dir_file);
    drop(receive_file);
    Ok(())
}

/// Windows：绑定 receive_dir / intent 目录 HANDLE，用相对路径 create/rename/unlink，
/// 拒绝 reparse/junction，关闭检查后路径操作 TOCTOU。
///
/// Business Logic（为什么需要这个函数）:
///     Windows 上 path-check-then-path-ops 可在校验后把 intent 目录换成 junction/reparse，
///     使 create/rename/delete 逃逸 receive_dir；必须相对已验证目录 HANDLE 操作。
///
/// Code Logic（这个函数做什么）:
///     1) CreateFileW 打开 receive_dir（BACKUP_SEMANTICS，不跟随 reparse 打开目录自身后拒绝 reparse）；
///     2) NtCreateFile 相对 receive HANDLE 创建/打开 intent 子目录（OPEN_REPARSE_POINT 后拒绝 reparse）；
///     3) 相对 intent HANDLE 删除旧 tmp/intent 目录项、CREATE_NEW 写 tmp、FlushFileBuffers、
///        NtSetInformationFile(FileRenameInformation=10, RootDirectory=intent HANDLE) 原子改名；
///     4) FlushFileBuffers(intent 目录)。
#[cfg(windows)]
pub(super) fn write_finalize_intent_windows_handle(
    receive_dir: &Path,
    intent_name: &str,
    tmp_name: &str,
    bytes: &[u8],
) -> Result<(), AppError> {
    use std::ffi::c_void;
    use std::io::{ErrorKind, Write};
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle, RawHandle};
    use std::ptr;

    type BOOL = i32;
    type DWORD = u32;
    type HANDLE = *mut c_void;
    type NTSTATUS = i32;
    type ULONG = u32;
    type USHORT = u16;
    type ACCESS_MASK = u32;

    pub(super) const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;
    pub(super) const FILE_FLAG_BACKUP_SEMANTICS: DWORD = 0x0200_0000;
    pub(super) const FILE_FLAG_OPEN_REPARSE_POINT: DWORD = 0x0020_0000;
    pub(super) const FILE_SHARE_READ: DWORD = 0x0000_0001;
    pub(super) const FILE_SHARE_WRITE: DWORD = 0x0000_0002;
    pub(super) const FILE_SHARE_DELETE: DWORD = 0x0000_0004;
    pub(super) const OPEN_EXISTING: DWORD = 3;
    pub(super) const GENERIC_READ: DWORD = 0x8000_0000;
    pub(super) const GENERIC_WRITE: DWORD = 0x4000_0000;
    pub(super) const FILE_ATTRIBUTE_DIRECTORY: DWORD = 0x10;
    pub(super) const FILE_ATTRIBUTE_REPARSE_POINT: DWORD = 0x400;
    pub(super) const FILE_ATTRIBUTE_NORMAL: DWORD = 0x80;
    pub(super) const ERROR_ALREADY_EXISTS: i32 = 183;
    pub(super) const ERROR_FILE_EXISTS: i32 = 80;
    pub(super) const ERROR_FILE_NOT_FOUND: i32 = 2;
    pub(super) const ERROR_PATH_NOT_FOUND: i32 = 3;
    pub(super) const FILE_OPEN: ULONG = 0x0000_0001;
    pub(super) const FILE_CREATE: ULONG = 0x0000_0002;
    pub(super) const FILE_OPEN_IF: ULONG = 0x0000_0003;
    pub(super) const FILE_DIRECTORY_FILE: ULONG = 0x0000_0001;
    pub(super) const FILE_NON_DIRECTORY_FILE: ULONG = 0x0000_0040;
    pub(super) const FILE_SYNCHRONOUS_IO_NONALERT: ULONG = 0x0000_0020;
    pub(super) const FILE_OPEN_REPARSE_POINT: ULONG = 0x0020_0000;
    pub(super) const FILE_DELETE_ON_CLOSE: ULONG = 0x0000_1000;
    pub(super) const DELETE: ACCESS_MASK = 0x0001_0000;
    pub(super) const FILE_LIST_DIRECTORY: ACCESS_MASK = 0x0001;
    pub(super) const FILE_ADD_FILE: ACCESS_MASK = 0x0002;
    pub(super) const FILE_ADD_SUBDIRECTORY: ACCESS_MASK = 0x0004;
    pub(super) const FILE_WRITE_DATA: ACCESS_MASK = 0x0002;
    pub(super) const FILE_READ_ATTRIBUTES: ACCESS_MASK = 0x0080;
    pub(super) const FILE_WRITE_ATTRIBUTES: ACCESS_MASK = 0x0100;
    pub(super) const SYNCHRONIZE: ACCESS_MASK = 0x0010_0000;
    pub(super) const FILE_GENERIC_WRITE: ACCESS_MASK =
        GENERIC_WRITE | FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES | SYNCHRONIZE;
    // NtSetInformationFile 的 FileRenameInformation = 10（不是 Win32 FileRenameInfo=3）。
    pub(super) const FILE_RENAME_INFORMATION_CLASS: ULONG = 10;
    pub(super) const OBJ_CASE_INSENSITIVE: ULONG = 0x0000_0040;
    pub(super) const STATUS_OBJECT_NAME_COLLISION: NTSTATUS = 0xC000_0035u32 as NTSTATUS;
    pub(super) const STATUS_OBJECT_NAME_NOT_FOUND: NTSTATUS = 0xC000_0034u32 as NTSTATUS;
    pub(super) const STATUS_OBJECT_PATH_NOT_FOUND: NTSTATUS = 0xC000_003Au32 as NTSTATUS;
    pub(super) const STATUS_DELETE_PENDING: NTSTATUS = 0xC000_0056u32 as NTSTATUS;

    #[repr(C)]
    pub(super) struct UnicodeString {
        length: USHORT,
        maximum_length: USHORT,
        buffer: *mut u16,
    }

    #[repr(C)]
    pub(super) struct ObjectAttributes {
        length: ULONG,
        root_directory: HANDLE,
        object_name: *mut UnicodeString,
        attributes: ULONG,
        security_descriptor: *mut c_void,
        security_quality_of_service: *mut c_void,
    }

    #[repr(C)]
    pub(super) struct IoStatusBlock {
        status: NTSTATUS,
        information: usize,
    }

    /// NtSetInformationFile(FileRenameInformation) 缓冲布局：BOOLEAN + 对齐后的 RootDirectory。
    #[repr(C)]
    pub(super) struct FileRenameInformation {
        replace_if_exists: u8, // BOOLEAN
        root_directory: HANDLE,
        file_name_length: ULONG,
        // file_name: [u16; 1] follows in buffer
        file_name: [u16; 1],
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub(super) fn CreateFileW(
            lp_file_name: *const u16,
            dw_desired_access: DWORD,
            dw_share_mode: DWORD,
            lp_security_attributes: *mut c_void,
            dw_creation_disposition: DWORD,
            dw_flags_and_attributes: DWORD,
            h_template_file: HANDLE,
        ) -> HANDLE;
        pub(super) fn GetFileInformationByHandle(
            h_file: HANDLE,
            lp_file_information: *mut ByHandleFileInformation,
        ) -> BOOL;
        pub(super) fn FlushFileBuffers(h_file: HANDLE) -> BOOL;
    }

    #[link(name = "ntdll")]
    extern "system" {
        pub(super) fn NtCreateFile(
            file_handle: *mut HANDLE,
            desired_access: ACCESS_MASK,
            object_attributes: *mut ObjectAttributes,
            io_status_block: *mut IoStatusBlock,
            allocation_size: *mut i64,
            file_attributes: ULONG,
            share_access: ULONG,
            create_disposition: ULONG,
            create_options: ULONG,
            ea_buffer: *mut c_void,
            ea_length: ULONG,
        ) -> NTSTATUS;
        pub(super) fn NtSetInformationFile(
            file_handle: HANDLE,
            io_status_block: *mut IoStatusBlock,
            file_information: *mut c_void,
            length: ULONG,
            file_information_class: ULONG,
        ) -> NTSTATUS;
        pub(super) fn RtlNtStatusToDosError(status: NTSTATUS) -> DWORD;
    }

    #[repr(C)]
    pub(super) struct ByHandleFileInformation {
        file_attributes: DWORD,
        creation_time: u64,
        last_access_time: u64,
        last_write_time: u64,
        volume_serial_number: DWORD,
        file_size_high: DWORD,
        file_size_low: DWORD,
        number_of_links: DWORD,
        file_index_high: DWORD,
        file_index_low: DWORD,
    }

    /// Business Logic: 路径必须是合法宽字符串，供 CreateFileW / NtCreateFile 使用。
    /// Code Logic: encode_wide + 拒绝内部 NUL + 追加 NUL 终止符。
    pub(super) fn to_wide(path: &Path) -> Result<Vec<u16>, AppError> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        if wide.iter().any(|&u| u == 0) {
            return Err(AppError::validation("路径含内部 NUL".to_string()));
        }
        wide.push(0);
        Ok(wide)
    }

    /// Business Logic: 相对路径组件名（intent 目录/文件）同样需宽字符串。
    /// Code Logic: 按 UTF-8 字节转宽字符；拒绝空串与内部 NUL。
    pub(super) fn name_to_wide(name: &str) -> Result<Vec<u16>, AppError> {
        if name.is_empty() || name.contains('\0') {
            return Err(AppError::validation("相对路径名非法".to_string()));
        }
        let mut wide: Vec<u16> = std::ffi::OsStr::new(name).encode_wide().collect();
        if wide.iter().any(|&u| u == 0) {
            return Err(AppError::validation("相对路径名含内部 NUL".to_string()));
        }
        wide.push(0);
        Ok(wide)
    }

    /// Business Logic: NTSTATUS 非成功时映射为 Win32 错误，便于上层处理 AlreadyExists/NotFound。
    /// Code Logic: status >= 0 成功；否则 RtlNtStatusToDosError。
    pub(super) fn nt_ok(status: NTSTATUS) -> Result<(), std::io::Error> {
        if status >= 0 {
            Ok(())
        } else {
            let dos = unsafe { RtlNtStatusToDosError(status) };
            Err(std::io::Error::from_raw_os_error(dos as i32))
        }
    }

    /// Business Logic: 目录 HANDLE 必须是普通目录，不能是 reparse/junction。
    /// Code Logic: GetFileInformationByHandle 检查 DIRECTORY 且无 REPARSE_POINT。
    pub(super) fn ensure_plain_directory_handle(handle: HANDLE) -> Result<(), AppError> {
        let mut info: ByHandleFileInformation = unsafe { zeroed() };
        let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
        if ok == 0 {
            return Err(AppError::from(std::io::Error::last_os_error()));
        }
        if info.file_attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(AppError::validation(
                "intent 路径不是目录（句柄绑定失败）".to_string(),
            ));
        }
        if info.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(AppError::validation(
                "intent 目录是 reparse/junction，拒绝写入".to_string(),
            ));
        }
        Ok(())
    }

    /// Business Logic: 相对父目录 HANDLE 打开/创建子对象，避免绝对 path 在校验后被交换。
    /// Code Logic: 组装 UNICODE_STRING + OBJECT_ATTRIBUTES，调用 NtCreateFile。
    unsafe fn open_relative(
        parent: HANDLE,
        name_wide: &mut [u16],
        desired_access: ACCESS_MASK,
        create_disposition: ULONG,
        create_options: ULONG,
        file_attributes: ULONG,
    ) -> Result<OwnedHandle, AppError> {
        // name_wide 含结尾 NUL；Length 不含 NUL。
        let name_units = name_wide.len().saturating_sub(1);
        let byte_len = name_units * 2;
        if byte_len > u16::MAX as usize {
            return Err(AppError::validation("相对路径名过长".to_string()));
        }
        let mut unicode = UnicodeString {
            length: byte_len as USHORT,
            maximum_length: (byte_len + 2) as USHORT,
            buffer: name_wide.as_mut_ptr(),
        };
        let mut attrs = ObjectAttributes {
            length: size_of::<ObjectAttributes>() as ULONG,
            root_directory: parent,
            object_name: &mut unicode,
            attributes: OBJ_CASE_INSENSITIVE,
            security_descriptor: ptr::null_mut(),
            security_quality_of_service: ptr::null_mut(),
        };
        let mut iosb: IoStatusBlock = zeroed();
        let mut out: HANDLE = ptr::null_mut();
        let status = NtCreateFile(
            &mut out,
            desired_access,
            &mut attrs,
            &mut iosb,
            ptr::null_mut(),
            file_attributes,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            create_disposition,
            create_options,
            ptr::null_mut(),
            0,
        );
        if let Err(err) = nt_ok(status) {
            // 统一 AlreadyExists 语义，供调用方判断。
            if status == STATUS_OBJECT_NAME_COLLISION
                || err.raw_os_error() == Some(ERROR_ALREADY_EXISTS)
                || err.raw_os_error() == Some(ERROR_FILE_EXISTS)
            {
                return Err(AppError::from(std::io::Error::new(
                    ErrorKind::AlreadyExists,
                    err,
                )));
            }
            if status == STATUS_OBJECT_NAME_NOT_FOUND
                || status == STATUS_OBJECT_PATH_NOT_FOUND
                || err.raw_os_error() == Some(ERROR_FILE_NOT_FOUND)
                || err.raw_os_error() == Some(ERROR_PATH_NOT_FOUND)
            {
                return Err(AppError::from(std::io::Error::new(
                    ErrorKind::NotFound,
                    err,
                )));
            }
            return Err(AppError::from(err));
        }
        if out.is_null() || out == INVALID_HANDLE_VALUE {
            return Err(AppError::generic(
                "NtCreateFile 返回无效 HANDLE".to_string(),
            ));
        }
        Ok(OwnedHandle::from_raw_handle(out as RawHandle))
    }

    /// Business Logic: 删除 intent 目录项必须相对目录 HANDLE，不能再走绝对 path remove。
    /// Code Logic: 以 DELETE|FILE_DELETE_ON_CLOSE 打开后立即 drop（关闭时删除）。
    unsafe fn unlink_relative(parent: HANDLE, name_wide: &mut [u16]) -> Result<(), AppError> {
        match open_relative(
            parent,
            name_wide,
            DELETE | SYNCHRONIZE | FILE_READ_ATTRIBUTES,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE
                | FILE_SYNCHRONOUS_IO_NONALERT
                | FILE_DELETE_ON_CLOSE
                | FILE_OPEN_REPARSE_POINT,
            0,
        ) {
            Ok(handle) => {
                drop(handle);
                Ok(())
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("os error 2")
                    || msg.contains("os error 3")
                    || msg.contains("NotFound")
                    || msg.contains("找不到")
                {
                    Ok(())
                } else {
                    // 也接受 std NotFound kind。
                    Err(e)
                }
            }
        }
    }

    /// Business Logic: rename 必须相对已验证 intent 目录 HANDLE，禁止 basename 相对 CWD 解析。
    /// Code Logic: NtSetInformationFile(FileRenameInformation=10) + RootDirectory=intent HANDLE
    ///     + ReplaceIfExists=TRUE；禁止 Win32 FileRenameInfo 信息类或 RootDirectory=NULL。
    unsafe fn rename_relative(
        file: HANDLE,
        intent_dir: HANDLE,
        new_name_wide: &[u16],
    ) -> Result<(), AppError> {
        if intent_dir.is_null() || intent_dir == INVALID_HANDLE_VALUE {
            return Err(AppError::generic(
                "intent 目录 HANDLE 无效，拒绝 rename".to_string(),
            ));
        }
        let name_units = new_name_wide.len().saturating_sub(1);
        let name_bytes = name_units * 2;
        // 结构体 + 文件名（不含内嵌的 1 个 wchar，需额外 (name_units-1)）
        let extra = name_units.saturating_sub(1) * 2;
        let total = size_of::<FileRenameInformation>() + extra;
        let mut buf = vec![0u8; total];
        let info = buf.as_mut_ptr() as *mut FileRenameInformation;
        (*info).replace_if_exists = 1;
        // 关键：绑定已验证 intent 目录 HANDLE，basename 不得相对进程 CWD。
        (*info).root_directory = intent_dir;
        (*info).file_name_length = name_bytes as ULONG;
        ptr::copy_nonoverlapping(
            new_name_wide.as_ptr(),
            (*info).file_name.as_mut_ptr(),
            name_units,
        );
        let mut iosb: IoStatusBlock = zeroed();
        let status = NtSetInformationFile(
            file,
            &mut iosb,
            buf.as_mut_ptr() as *mut c_void,
            total as ULONG,
            FILE_RENAME_INFORMATION_CLASS,
        );
        nt_ok(status).map_err(AppError::from)
    }

    let receive_wide = to_wide(receive_dir)?;
    let mut intent_dir_wide = name_to_wide(FINALIZE_INTENT_DIR)?;
    let mut intent_name_wide = name_to_wide(intent_name)?;
    let mut tmp_name_wide = name_to_wide(tmp_name)?;

    // 打开 receive_dir 自身（BACKUP_SEMANTICS + OPEN_REPARSE_POINT），再拒绝 reparse。
    let receive_raw = unsafe {
        CreateFileW(
            receive_wide.as_ptr(),
            GENERIC_READ | FILE_LIST_DIRECTORY | FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if receive_raw.is_null() || receive_raw == INVALID_HANDLE_VALUE {
        return Err(AppError::from(std::io::Error::last_os_error()));
    }
    let receive_handle = unsafe { OwnedHandle::from_raw_handle(receive_raw as RawHandle) };
    ensure_plain_directory_handle(receive_handle.as_raw_handle() as HANDLE)?;

    // 相对 receive HANDLE 创建/打开 intent 目录；OPEN_REPARSE_POINT 后拒绝 reparse。
    let intent_dir_handle = match unsafe {
        open_relative(
            receive_handle.as_raw_handle() as HANDLE,
            &mut intent_dir_wide,
            GENERIC_READ
                | FILE_LIST_DIRECTORY
                | FILE_ADD_FILE
                | FILE_ADD_SUBDIRECTORY
                | FILE_WRITE_ATTRIBUTES
                | SYNCHRONIZE
                | DELETE,
            FILE_OPEN_IF,
            FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
            FILE_ATTRIBUTE_DIRECTORY,
        )
    } {
        Ok(h) => h,
        Err(e) => {
            return Err(AppError::validation(format!(
                "无法绑定 intent 目录 HANDLE: {e}"
            )));
        }
    };
    ensure_plain_directory_handle(intent_dir_handle.as_raw_handle() as HANDLE)?;

    let parent = intent_dir_handle.as_raw_handle() as HANDLE;

    // 清残留 tmp 目录项（相对 unlink，不跟随 reparse 目标内容）。
    let _ = unsafe { unlink_relative(parent, &mut tmp_name_wide) };

    // CREATE_NEW 相对创建 tmp 普通文件；立即 into_raw 交给 File，避免与 OwnedHandle double-close。
    let tmp_owned = unsafe {
        open_relative(
            parent,
            &mut tmp_name_wide,
            FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES | DELETE | SYNCHRONIZE,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
            FILE_ATTRIBUTE_NORMAL,
        )
    }
    .map_err(|e| {
        AppError::from(std::io::Error::new(
            ErrorKind::Other,
            format!("创建 intent 临时文件失败: {e}"),
        ))
    })?;
    let mut tmp_file = unsafe { std::fs::File::from_raw_handle(tmp_owned.into_raw_handle()) };
    if let Err(err) = (|| -> Result<(), AppError> {
        tmp_file.write_all(bytes).map_err(AppError::from)?;
        tmp_file.flush().map_err(AppError::from)?;
        tmp_file.sync_all().map_err(AppError::from)?;
        Ok(())
    })() {
        // 写失败：关闭句柄后相对 unlink 残留 tmp。
        drop(tmp_file);
        let _ = unsafe { unlink_relative(parent, &mut tmp_name_wide) };
        return Err(err);
    }

    // 删除旧正式 intent 目录项（若存在），再把 tmp rename 为正式名。
    // rename 需要仍打开的文件 HANDLE。
    let _ = unsafe { unlink_relative(parent, &mut intent_name_wide) };
    let tmp_raw = tmp_file.into_raw_handle();
    let tmp_handle = unsafe { OwnedHandle::from_raw_handle(tmp_raw) };
    if let Err(err) = unsafe {
        rename_relative(
            tmp_handle.as_raw_handle() as HANDLE,
            parent,
            &intent_name_wide,
        )
    } {
        drop(tmp_handle);
        let _ = unsafe { unlink_relative(parent, &mut tmp_name_wide) };
        return Err(err);
    }
    drop(tmp_handle);

    // FlushFileBuffers(intent 目录)，保证 rename 目录项断电后可见。
    let flush_ok = unsafe { FlushFileBuffers(parent) };
    if flush_ok == 0 {
        return Err(AppError::from(std::io::Error::last_os_error()));
    }

    drop(intent_dir_handle);
    drop(receive_handle);
    let _ = (STATUS_DELETE_PENDING, FILE_OPEN);
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     durable history + 墓碑成功后必须删除 intent，避免后续误恢复。
///     删除同样不能在 path-check 后再用绝对 path remove（父目录可被换成 reparse）。
///
/// Code Logic（这个函数做什么）:
///     Unix：receive_dir fd + openat intent 目录 + unlinkat；Windows：目录 HANDLE 相对 unlink；
///     其它平台：词法路径 remove（无 dirfd 原语时的降级，生产目标平台为 unix/windows）。
pub(super) async fn clear_finalize_intent(
    receive_dir: &Path,
    transfer_id: &str,
) -> Result<(), AppError> {
    // 边界校验副作用（非法 id / 逃逸）。
    let _ = finalize_intent_path(receive_dir, transfer_id)?;
    let safe_id = sanitize_receive_basename(transfer_id, "transfer_id")?;
    let intent_name = format!("{safe_id}.json");

    #[cfg(unix)]
    {
        let receive_dir = receive_dir.to_path_buf();
        let intent_name = intent_name.clone();
        tokio::task::spawn_blocking(move || {
            clear_finalize_intent_unix_dirfd(&receive_dir, &intent_name)
        })
        .await
        .map_err(|e| AppError::generic(format!("clear_finalize_intent join 失败: {e}")))?
    }

    #[cfg(windows)]
    {
        let receive_dir = receive_dir.to_path_buf();
        let intent_name = intent_name.clone();
        tokio::task::spawn_blocking(move || {
            clear_finalize_intent_windows_handle(&receive_dir, &intent_name)
        })
        .await
        .map_err(|e| AppError::generic(format!("clear_finalize_intent join 失败: {e}")))?
    }

    #[cfg(not(any(unix, windows)))]
    {
        let path = finalize_intent_path(receive_dir, transfer_id)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AppError::from(e)),
        }
    }
}

/// Unix：相对 receive_dir / intent 目录 fd unlinkat 删除 intent。
#[cfg(unix)]
pub(super) fn clear_finalize_intent_unix_dirfd(
    receive_dir: &Path,
    intent_name: &str,
) -> Result<(), AppError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::{AsRawFd, FromRawFd};

    let receive_c = CString::new(receive_dir.as_os_str().as_bytes())
        .map_err(|_| AppError::validation("receive_dir 含内部 NUL"))?;
    let intent_dir_c =
        CString::new(FINALIZE_INTENT_DIR).map_err(|_| AppError::validation("intent dir 名非法"))?;
    let intent_c = CString::new(intent_name.as_bytes())
        .map_err(|_| AppError::validation("intent 文件名含内部 NUL"))?;

    let receive_fd = unsafe {
        libc::open(
            receive_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if receive_fd < 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == ErrorKind::NotFound {
            return Ok(());
        }
        return Err(AppError::from(err));
    }
    let receive_file = unsafe { std::fs::File::from_raw_fd(receive_fd) };

    let intent_dir_fd = unsafe {
        libc::openat(
            receive_file.as_raw_fd(),
            intent_dir_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if intent_dir_fd < 0 {
        let err = std::io::Error::last_os_error();
        // 目录不存在 / 是 symlink → 视为无 intent 可清。
        if err.kind() == ErrorKind::NotFound
            || err.raw_os_error() == Some(libc::ELOOP)
            || err.raw_os_error() == Some(libc::EPERM)
        {
            return Ok(());
        }
        return Err(AppError::from(err));
    }
    let intent_dir_file = unsafe { std::fs::File::from_raw_fd(intent_dir_fd) };
    let rc = unsafe { libc::unlinkat(intent_dir_file.as_raw_fd(), intent_c.as_ptr(), 0) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() != ErrorKind::NotFound {
            return Err(AppError::from(err));
        }
    }
    drop(intent_dir_file);
    drop(receive_file);
    Ok(())
}

/// Windows：相对目录 HANDLE 删除 intent 文件，拒绝 reparse 目录上的 path-ops。
#[cfg(windows)]
pub(super) fn clear_finalize_intent_windows_handle(
    receive_dir: &Path,
    intent_name: &str,
) -> Result<(), AppError> {
    // 复用写路径的打开语义：绑定 plain directory HANDLE 后相对 unlink。
    // 为避免重复大段 FFI，这里走“打开 intent 目录 HANDLE + 相对 DELETE_ON_CLOSE”。
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use std::ptr;

    type BOOL = i32;
    type DWORD = u32;
    type HANDLE = *mut c_void;
    type NTSTATUS = i32;
    type ULONG = u32;
    type USHORT = u16;
    type ACCESS_MASK = u32;

    pub(super) const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;
    pub(super) const FILE_FLAG_BACKUP_SEMANTICS: DWORD = 0x0200_0000;
    pub(super) const FILE_FLAG_OPEN_REPARSE_POINT: DWORD = 0x0020_0000;
    pub(super) const FILE_SHARE_READ: DWORD = 0x1;
    pub(super) const FILE_SHARE_WRITE: DWORD = 0x2;
    pub(super) const FILE_SHARE_DELETE: DWORD = 0x4;
    pub(super) const OPEN_EXISTING: DWORD = 3;
    pub(super) const GENERIC_READ: DWORD = 0x8000_0000;
    pub(super) const FILE_ATTRIBUTE_DIRECTORY: DWORD = 0x10;
    pub(super) const FILE_ATTRIBUTE_REPARSE_POINT: DWORD = 0x400;
    pub(super) const FILE_OPEN: ULONG = 0x1;
    pub(super) const FILE_DIRECTORY_FILE: ULONG = 0x1;
    pub(super) const FILE_NON_DIRECTORY_FILE: ULONG = 0x40;
    pub(super) const FILE_SYNCHRONOUS_IO_NONALERT: ULONG = 0x20;
    pub(super) const FILE_OPEN_REPARSE_POINT: ULONG = 0x0020_0000;
    pub(super) const FILE_DELETE_ON_CLOSE: ULONG = 0x1000;
    pub(super) const DELETE: ACCESS_MASK = 0x0001_0000;
    pub(super) const FILE_LIST_DIRECTORY: ACCESS_MASK = 0x1;
    pub(super) const FILE_READ_ATTRIBUTES: ACCESS_MASK = 0x80;
    pub(super) const SYNCHRONIZE: ACCESS_MASK = 0x0010_0000;
    pub(super) const OBJ_CASE_INSENSITIVE: ULONG = 0x40;

    #[repr(C)]
    pub(super) struct UnicodeString {
        length: USHORT,
        maximum_length: USHORT,
        buffer: *mut u16,
    }
    #[repr(C)]
    pub(super) struct ObjectAttributes {
        length: ULONG,
        root_directory: HANDLE,
        object_name: *mut UnicodeString,
        attributes: ULONG,
        security_descriptor: *mut c_void,
        security_quality_of_service: *mut c_void,
    }
    #[repr(C)]
    pub(super) struct IoStatusBlock {
        status: NTSTATUS,
        information: usize,
    }
    #[repr(C)]
    pub(super) struct ByHandleFileInformation {
        file_attributes: DWORD,
        creation_time: u64,
        last_access_time: u64,
        last_write_time: u64,
        volume_serial_number: DWORD,
        file_size_high: DWORD,
        file_size_low: DWORD,
        number_of_links: DWORD,
        file_index_high: DWORD,
        file_index_low: DWORD,
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub(super) fn CreateFileW(
            lp_file_name: *const u16,
            dw_desired_access: DWORD,
            dw_share_mode: DWORD,
            lp_security_attributes: *mut c_void,
            dw_creation_disposition: DWORD,
            dw_flags_and_attributes: DWORD,
            h_template_file: HANDLE,
        ) -> HANDLE;
        pub(super) fn GetFileInformationByHandle(
            h_file: HANDLE,
            lp_file_information: *mut ByHandleFileInformation,
        ) -> BOOL;
    }
    #[link(name = "ntdll")]
    extern "system" {
        pub(super) fn NtCreateFile(
            file_handle: *mut HANDLE,
            desired_access: ACCESS_MASK,
            object_attributes: *mut ObjectAttributes,
            io_status_block: *mut IoStatusBlock,
            allocation_size: *mut i64,
            file_attributes: ULONG,
            share_access: ULONG,
            create_disposition: ULONG,
            create_options: ULONG,
            ea_buffer: *mut c_void,
            ea_length: ULONG,
        ) -> NTSTATUS;
        pub(super) fn RtlNtStatusToDosError(status: NTSTATUS) -> DWORD;
    }

    pub(super) fn to_wide(path: &Path) -> Result<Vec<u16>, AppError> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        if wide.iter().any(|&u| u == 0) {
            return Err(AppError::validation("路径含内部 NUL".to_string()));
        }
        wide.push(0);
        Ok(wide)
    }
    pub(super) fn name_to_wide(name: &str) -> Result<Vec<u16>, AppError> {
        if name.is_empty() || name.contains('\0') {
            return Err(AppError::validation("相对路径名非法".to_string()));
        }
        let mut wide: Vec<u16> = std::ffi::OsStr::new(name).encode_wide().collect();
        if wide.iter().any(|&u| u == 0) {
            return Err(AppError::validation("相对路径名含内部 NUL".to_string()));
        }
        wide.push(0);
        Ok(wide)
    }
    pub(super) fn ensure_plain_directory_handle(handle: HANDLE) -> Result<(), AppError> {
        let mut info: ByHandleFileInformation = unsafe { zeroed() };
        let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
        if ok == 0 {
            return Err(AppError::from(std::io::Error::last_os_error()));
        }
        if info.file_attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(AppError::validation("intent 路径不是目录".to_string()));
        }
        if info.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(AppError::validation(
                "intent 目录是 reparse/junction，拒绝删除".to_string(),
            ));
        }
        Ok(())
    }
    unsafe fn open_relative(
        parent: HANDLE,
        name_wide: &mut [u16],
        desired_access: ACCESS_MASK,
        create_disposition: ULONG,
        create_options: ULONG,
    ) -> Result<OwnedHandle, AppError> {
        let name_units = name_wide.len().saturating_sub(1);
        let byte_len = name_units * 2;
        let mut unicode = UnicodeString {
            length: byte_len as USHORT,
            maximum_length: (byte_len + 2) as USHORT,
            buffer: name_wide.as_mut_ptr(),
        };
        let mut attrs = ObjectAttributes {
            length: size_of::<ObjectAttributes>() as ULONG,
            root_directory: parent,
            object_name: &mut unicode,
            attributes: OBJ_CASE_INSENSITIVE,
            security_descriptor: ptr::null_mut(),
            security_quality_of_service: ptr::null_mut(),
        };
        let mut iosb: IoStatusBlock = zeroed();
        let mut out: HANDLE = ptr::null_mut();
        let status = NtCreateFile(
            &mut out,
            desired_access,
            &mut attrs,
            &mut iosb,
            ptr::null_mut(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            create_disposition,
            create_options,
            ptr::null_mut(),
            0,
        );
        if status < 0 {
            let dos = RtlNtStatusToDosError(status) as i32;
            if dos == 2 || dos == 3 {
                return Err(AppError::from(std::io::Error::new(
                    ErrorKind::NotFound,
                    std::io::Error::from_raw_os_error(dos),
                )));
            }
            return Err(AppError::from(std::io::Error::from_raw_os_error(dos)));
        }
        Ok(OwnedHandle::from_raw_handle(out as RawHandle))
    }

    let receive_wide = to_wide(receive_dir)?;
    let mut intent_dir_wide = name_to_wide(FINALIZE_INTENT_DIR)?;
    let mut intent_name_wide = name_to_wide(intent_name)?;

    let receive_raw = unsafe {
        CreateFileW(
            receive_wide.as_ptr(),
            GENERIC_READ | FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if receive_raw.is_null() || receive_raw == INVALID_HANDLE_VALUE {
        let err = std::io::Error::last_os_error();
        if err.kind() == ErrorKind::NotFound {
            return Ok(());
        }
        return Err(AppError::from(err));
    }
    let receive_handle = unsafe { OwnedHandle::from_raw_handle(receive_raw as RawHandle) };
    if let Err(e) = ensure_plain_directory_handle(receive_handle.as_raw_handle() as HANDLE) {
        // receive_dir 本身是 reparse：拒绝 path 删除，直接失败更安全。
        return Err(e);
    }

    let intent_dir_handle = match unsafe {
        open_relative(
            receive_handle.as_raw_handle() as HANDLE,
            &mut intent_dir_wide,
            GENERIC_READ | FILE_LIST_DIRECTORY | SYNCHRONIZE | FILE_READ_ATTRIBUTES,
            FILE_OPEN,
            FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
        )
    } {
        Ok(h) => h,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("NotFound") || msg.contains("os error 2") || msg.contains("os error 3")
            {
                return Ok(());
            }
            return Err(e);
        }
    };
    if let Err(_e) = ensure_plain_directory_handle(intent_dir_handle.as_raw_handle() as HANDLE) {
        // intent 目录是 reparse：不跟随删除外部目标，视为已无安全 intent。
        return Ok(());
    }

    match unsafe {
        open_relative(
            intent_dir_handle.as_raw_handle() as HANDLE,
            &mut intent_name_wide,
            DELETE | SYNCHRONIZE | FILE_READ_ATTRIBUTES,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE
                | FILE_SYNCHRONOUS_IO_NONALERT
                | FILE_DELETE_ON_CLOSE
                | FILE_OPEN_REPARSE_POINT,
        )
    } {
        Ok(h) => drop(h),
        Err(e) => {
            let msg = e.to_string();
            if !(msg.contains("NotFound")
                || msg.contains("os error 2")
                || msg.contains("os error 3"))
            {
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Code Logic: 拼 intent 文件路径并校验仍在 receive_dir 内。
pub(super) fn finalize_intent_path(
    receive_dir: &Path,
    transfer_id: &str,
) -> Result<PathBuf, AppError> {
    let safe_id = sanitize_receive_basename(transfer_id, "transfer_id")?;
    let intent_dir = receive_dir.join(FINALIZE_INTENT_DIR);
    // intent 目录若是 symlink，canonicalize 会跟随逃逸；先 no-follow 拒绝。
    match std::fs::symlink_metadata(&intent_dir) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(AppError::validation(format!(
                "intent 目录是符号链接，拒绝写入: {}",
                intent_dir.display()
            )));
        }
        Ok(meta) if !meta.file_type().is_dir() => {
            return Err(AppError::validation(format!(
                "intent 路径不是目录: {}",
                intent_dir.display()
            )));
        }
        Ok(_) | Err(_) => {}
    }
    let path = intent_dir.join(format!("{safe_id}.json"));
    // 词法边界：用 normalize 而非对可能不存在/symlink 父目录 canonicalize。
    let canonical_dir = match receive_dir.canonicalize() {
        Ok(p) => p,
        Err(_) => normalize_path(receive_dir),
    };
    let lexical = canonical_dir
        .join(FINALIZE_INTENT_DIR)
        .join(format!("{safe_id}.json"));
    if !lexical.starts_with(&canonical_dir) {
        return Err(AppError::validation(
            "目标路径逃逸 receive_dir，拒绝写入".to_string(),
        ));
    }
    // 若 intent 目录已是普通目录，再做一次真实路径校验。
    if intent_dir.is_dir() {
        ensure_path_within_dir(receive_dir, &path)?;
    }
    Ok(path)
}

/// 原子落盘结果。
pub(super) struct PlacedFile {
    pub(super) final_filename: String,
    pub(super) final_path: PathBuf,
}

/// place 失败分类：未放置 vs 已放置但 durability 待补。
pub(super) enum PlaceFinalError {
    /// hard_link/rename 未成功：可写 Failed history / 换后缀 / 重传。
    Unplaced(AppError),
    /// 最终路径已独占成功，但 fsync 最终文件/父目录失败：保留 Completed active + intent，禁止 Failed。
    DurabilityPending { placed: PlacedFile, message: String },
}

impl From<PlaceFinalError> for AppError {
    fn from(value: PlaceFinalError) -> Self {
        match value {
            PlaceFinalError::Unplaced(err) => err,
            PlaceFinalError::DurabilityPending { message, .. } => AppError::unavailable(message),
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     finalize 在 place 前必须为**每个**候选 final 路径（首选与后缀）写 durable intent。
///     若仅在首次候选写 intent，冲突后清 intent 再 place 后缀，place→补写 intent 之间崩溃
///     会丢失 journal：重启后同 transfer_id 可 reopen 并生成第二份后缀副本。
///
/// Code Logic（这个函数做什么）:
///     有限循环：resolve 候选 → 校验 basename/边界 → **先** write_finalize_intent 指向该候选
///     → no-replace place；AlreadyExists 则保留/覆盖下一候选 intent 再试，绝不 place 无匹配
///     intent 的路径；其它错误上抛（intent 指向未成功路径时 recovery 会清 intent 允许干净重传）。
///     调用方必须已持有 receive_dir 锁。
pub(super) async fn place_final_file_with_intent(
    receive_dir: &Path,
    safe_filename: &str,
    tmp_path: &Path,
    transfer_id: &str,
    task: &TransferTask,
) -> Result<PlacedFile, PlaceFinalError> {
    // 有限重试：极端并发下 resolve 可能反复撞车；上限防止死循环。
    for _ in 0..10_000 {
        let final_filename = resolve_filename(receive_dir, safe_filename);
        if let Err(e) = sanitize_receive_basename(&final_filename, "final_filename") {
            return Err(PlaceFinalError::Unplaced(AppError::validation(format!(
                "非法最终文件名: {e}"
            ))));
        }
        let final_path = receive_dir.join(&final_filename);
        ensure_path_within_dir(receive_dir, &final_path).map_err(PlaceFinalError::Unplaced)?;

        // 每个候选：先 journal 再 place（含首次与后缀）。
        let intent = FinalizeIntent {
            transfer_id: transfer_id.to_string(),
            filename: task.filename.clone(),
            size: task.size,
            sha256: task.sha256.clone(),
            chunk_size: task.chunk_size,
            final_filename: final_filename.clone(),
            final_path: final_path.to_string_lossy().to_string(),
            created_at: now_iso(),
        };
        write_finalize_intent(receive_dir, &intent)
            .await
            .map_err(PlaceFinalError::Unplaced)?;

        match commit_tmp_to_final_no_replace(tmp_path, &final_path).await {
            Ok(()) => {
                return Ok(PlacedFile {
                    final_filename,
                    final_path,
                });
            }
            Err(CommitFinalError::AlreadyExists) => {
                // 候选被占：下一轮会写新候选 intent 再 place，禁止无 intent 的 place helper。
                continue;
            }
            Err(CommitFinalError::DurabilityPending(e)) => {
                // 最终文件已落地：保留 intent，交上层 mark Completed + 可重试，禁止 Failed。
                return Err(PlaceFinalError::DurabilityPending {
                    placed: PlacedFile {
                        final_filename,
                        final_path,
                    },
                    message: format!("最终文件已落地但 durability 同步失败: {e}"),
                });
            }
            Err(CommitFinalError::Failed(e)) => {
                // 非冲突失败：intent 仍在但 final 缺失；recovery 会清 intent 允许重传。
                return Err(PlaceFinalError::Unplaced(AppError::generic(format!(
                    "提交最终文件失败: {e}"
                ))));
            }
        }
    }
    Err(PlaceFinalError::Unplaced(AppError::generic(
        "无法分配不冲突的最终文件名（重试耗尽）".to_string(),
    )))
}

/// Business Logic（为什么需要这个函数）:
///     并发同名接收时，仅 `exists()` 检查后 rename 会在 POSIX 上静默覆盖先落地的文件。
///     零字节占位再 rename 仍有 TOCTOU：外部进程可在两步之间替换路径；失败时无条件
///     `remove_file` 还可能误删竞争者文件；崩溃会留下“看似成功”的零字节最终文件。
///     因此必须用 hard_link(tmp, final) 作为跨平台 no-replace 提交原语。
///
/// Code Logic（这个函数做什么）:
///     循环 resolve 候选名 → 校验路径边界 → `hard_link(tmp, final)` 原子抢占最终路径
///     （已存在 → AlreadyExists，换后缀继续）→ 成功后删除 tmp（失败仅 warn，最终文件已落地）。
///     hard_link 在跨卷等场景不可用时，回退到平台 no-replace rename（见
///     `commit_tmp_to_final_no_replace`），仍保证失败路径不删除非本次创建的文件。
///     **生产 finalize 必须走 `place_final_file_with_intent`**（每个候选 journal-before-place）；
///     本 helper 仅用于无 journal 的底层 no-replace 单测。
///     调用方必须已持有 receive_dir 锁（同进程额外协调）。
#[cfg(test)]
pub(super) async fn place_final_file_exclusive(
    receive_dir: &Path,
    safe_filename: &str,
    tmp_path: &Path,
) -> Result<PlacedFile, AppError> {
    // 有限重试：极端并发下 resolve 可能反复撞车；上限防止死循环。
    for _ in 0..10_000 {
        let final_filename = resolve_filename(receive_dir, safe_filename);
        if let Err(e) = sanitize_receive_basename(&final_filename, "final_filename") {
            return Err(AppError::validation(format!("非法最终文件名: {e}")));
        }
        let final_path = receive_dir.join(&final_filename);
        ensure_path_within_dir(receive_dir, &final_path)?;

        match commit_tmp_to_final_no_replace(tmp_path, &final_path).await {
            Ok(()) => {
                return Ok(PlacedFile {
                    final_filename,
                    final_path,
                });
            }
            Err(CommitFinalError::AlreadyExists) => {
                // 候选名被并发占走，重新 resolve 下一个后缀。
                continue;
            }
            Err(CommitFinalError::DurabilityPending(e)) => {
                // 测试 helper 将 durability pending 视为成功落盘（文件已在）。
                tracing::warn!("place exclusive durability pending: {e}");
                return Ok(PlacedFile {
                    final_filename,
                    final_path,
                });
            }
            Err(CommitFinalError::Failed(e)) => {
                return Err(AppError::generic(format!("提交最终文件失败: {e}")));
            }
        }
    }
    Err(AppError::generic(
        "无法分配不冲突的最终文件名（重试耗尽）".to_string(),
    ))
}

/// place 底层 commit 错误：区分未放置 / 已放置但 durability 未完成。
pub(super) enum CommitFinalError {
    AlreadyExists,
    Failed(std::io::Error),
    DurabilityPending(std::io::Error),
}

/// Business Logic（为什么需要这个函数）:
///     将已校验的临时文件提交到最终路径，且不得覆盖/删除路径上已有的竞争文件。
///     hard_link 是首选：同 inode 链接要么成功要么 AlreadyExists，无零字节占位窗口。
///     place 成功后的 fsync 失败不能伪装成“未放置”，否则上层会写 Failed 并清 intent。
///
/// Code Logic（这个函数做什么）:
///     1) 同步 hard_link(tmp → final)；成功后 best-effort remove tmp；
///     2) hard_link 因 AlreadyExists 失败 → AlreadyExists，调用方换后缀；
///     3) hard_link 因跨卷/不支持失败 → 平台 no-replace rename 回退；
///        回退失败绝不 remove final（可能是竞争者文件）；
///     4) place 成功后 fsync 最终普通文件 + 父目录；失败返回 DurabilityPending。
pub(super) async fn commit_tmp_to_final_no_replace(
    tmp_path: &Path,
    final_path: &Path,
) -> Result<(), CommitFinalError> {
    // place 前把 tmp 数据刷到稳定存储，避免断电丢内容而 history 已 completed。
    if let Err(e) = sync_regular_file(tmp_path).await {
        return Err(CommitFinalError::Failed(std::io::Error::other(format!(
            "sync tmp 失败: {e}"
        ))));
    }
    // hard_link 是同步 syscall；放在 blocking 上下文外也可接受（单次元数据操作）。
    match std::fs::hard_link(tmp_path, final_path) {
        Ok(()) => {
            if let Err(e) = tokio::fs::remove_file(tmp_path).await {
                tracing::warn!(
                    "最终文件 hard_link 成功但删除 tmp 失败 ({}): {e}",
                    tmp_path.display()
                );
            }
            // 最终目录项 + unlink 后 fsync 最终文件与 receive_dir。
            if let Err(e) = ensure_final_file_durable(final_path).await {
                return Err(CommitFinalError::DurabilityPending(std::io::Error::other(
                    format!("fsync final/receive_dir 失败: {e}"),
                )));
            }
            Ok(())
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(CommitFinalError::AlreadyExists),
        Err(link_err) => {
            // 跨卷/不支持 hard_link：回退到平台 no-replace rename，仍禁止覆盖已有 final。
            match rename_no_replace(tmp_path, final_path).await {
                Ok(()) => {
                    if let Err(e) = ensure_final_file_durable(final_path).await {
                        return Err(CommitFinalError::DurabilityPending(std::io::Error::other(
                            format!("fsync final/receive_dir 失败: {e}"),
                        )));
                    }
                    Ok(())
                }
                Err(rename_err) if rename_err.kind() == ErrorKind::AlreadyExists => {
                    Err(CommitFinalError::AlreadyExists)
                }
                Err(rename_err) => {
                    // 保留原始 hard_link 错误上下文，便于诊断跨卷场景。
                    Err(CommitFinalError::Failed(std::io::Error::new(
                        rename_err.kind(),
                        format!(
                            "hard_link 失败 ({link_err}) 且 no-replace rename 失败 ({rename_err})"
                        ),
                    )))
                }
            }
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     place 成功后的即时 fsync：最终普通文件与父目录项进入稳定存储。
///     **不得**单独用于 Completed history 晋升：路径 reopen 无法抵御普通文件替换。
///
/// Code Logic（这个函数做什么）:
///     no-follow sync 最终普通文件，再 fsync 其父目录；任一步失败上抛。
pub(super) async fn ensure_final_file_durable(final_path: &Path) -> Result<(), AppError> {
    sync_regular_file(final_path).await?;
    if let Some(parent) = final_path.parent() {
        fsync_dir(parent)?;
    }
    Ok(())
}

/// 绑定文件身份：用于 durability 晋升前确认父目录项仍指向同一普通文件。
///
/// Business Logic（为什么需要这个结构）:
///     durability 失败与重试之间，最终路径上的普通文件可被原子替换；仅 no-follow open
///     会通过，但内容/身份已变，不能再按原始 size/SHA 写 Completed history。
///
/// Code Logic（这个结构做什么）:
///     Unix 存 (dev,ino)；Windows 存 (volume_serial, file_index)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BoundFileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(windows)]
    volume_serial: u32,
    #[cfg(windows)]
    file_index: u64,
    #[cfg(not(any(unix, windows)))]
    _marker: u8,
}

/// Business Logic（为什么需要这个函数）:
///     从已打开的普通文件句柄读取稳定身份，供 fsync 后与父目录项对照。
///
/// Code Logic（这个函数做什么）:
///     Unix: fstat → (st_dev, st_ino)；Windows: GetFileInformationByHandle →
///     (volume_serial_number, file_index_high<<32|file_index_low)。
pub(super) fn file_identity_from_std(file: &std::fs::File) -> Result<BoundFileIdentity, AppError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = file.metadata().map_err(AppError::from)?;
        Ok(BoundFileIdentity {
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        type BOOL = i32;
        type DWORD = u32;
        type HANDLE = *mut std::ffi::c_void;

        #[repr(C)]
        pub(super) struct ByHandleFileInformation {
            file_attributes: DWORD,
            creation_time: u64,
            last_access_time: u64,
            last_write_time: u64,
            volume_serial_number: DWORD,
            file_size_high: DWORD,
            file_size_low: DWORD,
            number_of_links: DWORD,
            file_index_high: DWORD,
            file_index_low: DWORD,
        }

        #[link(name = "kernel32")]
        extern "system" {
            pub(super) fn GetFileInformationByHandle(
                h_file: HANDLE,
                lp_file_information: *mut ByHandleFileInformation,
            ) -> BOOL;
        }

        let mut info = unsafe { std::mem::zeroed::<ByHandleFileInformation>() };
        let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) };
        if ok == 0 {
            return Err(AppError::from(std::io::Error::last_os_error()));
        }
        let file_index = ((info.file_index_high as u64) << 32) | (info.file_index_low as u64);
        Ok(BoundFileIdentity {
            volume_serial: info.volume_serial_number,
            file_index,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Err(AppError::from(std::io::Error::new(
            ErrorKind::Unsupported,
            "当前平台无法读取文件身份",
        )))
    }
}

/// Business Logic（为什么需要这个函数）:
///     fsync 后必须通过父目录 HANDLE/fd 再次打开同名目录项，证明仍是同一 inode/file id。
///
/// Code Logic（这个函数做什么）:
///     Unix: open 父目录 O_DIRECTORY → openat(O_NOFOLLOW|O_RDONLY) → fstat 身份；
///     Windows: CreateFileW 打开父目录（BACKUP_SEMANTICS|OPEN_REPARSE_POINT，拒绝 reparse）
///     → NtCreateFile 相对 basename 打开普通文件 → GetFileInformationByHandle。
pub(super) fn dirent_identity_via_parent(final_path: &Path) -> Result<BoundFileIdentity, AppError> {
    let parent = final_path.parent().ok_or_else(|| {
        AppError::validation(format!(
            "最终路径无父目录，无法确认目录项身份: {}",
            final_path.display()
        ))
    })?;
    let name = final_path.file_name().ok_or_else(|| {
        AppError::validation(format!(
            "最终路径无文件名，无法确认目录项身份: {}",
            final_path.display()
        ))
    })?;

    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::io::{AsRawFd, FromRawFd};

        let parent_c = CString::new(parent.as_os_str().as_bytes())
            .map_err(|_| AppError::validation("父目录路径含内部 NUL"))?;
        let name_c = CString::new(name.as_bytes())
            .map_err(|_| AppError::validation("最终文件名含内部 NUL"))?;
        let parent_fd = unsafe {
            libc::open(
                parent_c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if parent_fd < 0 {
            return Err(AppError::from(std::io::Error::last_os_error()));
        }
        let parent_file = unsafe { std::fs::File::from_raw_fd(parent_fd) };
        let child_fd = unsafe {
            libc::openat(
                parent_file.as_raw_fd(),
                name_c.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if child_fd < 0 {
            let err = std::io::Error::last_os_error();
            return Err(AppError::from(std::io::Error::new(
                err.kind(),
                format!(
                    "经父目录确认最终文件身份失败 {}: {err}",
                    final_path.display()
                ),
            )));
        }
        let child = unsafe { std::fs::File::from_raw_fd(child_fd) };
        let id = file_identity_from_std(&child)?;
        drop(child);
        drop(parent_file);
        Ok(id)
    }

    #[cfg(windows)]
    {
        use std::ffi::c_void;
        use std::mem::{size_of, zeroed};
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
        use std::ptr;

        type BOOL = i32;
        type DWORD = u32;
        type HANDLE = *mut c_void;
        type NTSTATUS = i32;
        type ULONG = u32;
        type USHORT = u16;
        type ACCESS_MASK = u32;

        pub(super) const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;
        pub(super) const FILE_FLAG_BACKUP_SEMANTICS: DWORD = 0x0200_0000;
        pub(super) const FILE_FLAG_OPEN_REPARSE_POINT: DWORD = 0x0020_0000;
        pub(super) const FILE_SHARE_READ: DWORD = 0x1;
        pub(super) const FILE_SHARE_WRITE: DWORD = 0x2;
        pub(super) const FILE_SHARE_DELETE: DWORD = 0x4;
        pub(super) const OPEN_EXISTING: DWORD = 3;
        pub(super) const GENERIC_READ: DWORD = 0x8000_0000;
        pub(super) const FILE_LIST_DIRECTORY: ACCESS_MASK = 0x0001;
        pub(super) const FILE_READ_ATTRIBUTES: ACCESS_MASK = 0x0080;
        pub(super) const SYNCHRONIZE: ACCESS_MASK = 0x0010_0000;
        pub(super) const FILE_OPEN: ULONG = 0x0000_0001;
        pub(super) const FILE_NON_DIRECTORY_FILE: ULONG = 0x0000_0040;
        pub(super) const FILE_SYNCHRONOUS_IO_NONALERT: ULONG = 0x0000_0020;
        pub(super) const FILE_OPEN_REPARSE_POINT: ULONG = 0x0020_0000;
        pub(super) const OBJ_CASE_INSENSITIVE: ULONG = 0x0000_0040;
        pub(super) const FILE_ATTRIBUTE_DIRECTORY: DWORD = 0x10;
        pub(super) const FILE_ATTRIBUTE_REPARSE_POINT: DWORD = 0x400;

        #[repr(C)]
        pub(super) struct UnicodeString {
            length: USHORT,
            maximum_length: USHORT,
            buffer: *mut u16,
        }
        #[repr(C)]
        pub(super) struct ObjectAttributes {
            length: ULONG,
            root_directory: HANDLE,
            object_name: *mut UnicodeString,
            attributes: ULONG,
            security_descriptor: *mut c_void,
            security_quality_of_service: *mut c_void,
        }
        #[repr(C)]
        pub(super) struct IoStatusBlock {
            status: NTSTATUS,
            information: usize,
        }
        #[repr(C)]
        pub(super) struct ByHandleFileInformation {
            file_attributes: DWORD,
            creation_time: u64,
            last_access_time: u64,
            last_write_time: u64,
            volume_serial_number: DWORD,
            file_size_high: DWORD,
            file_size_low: DWORD,
            number_of_links: DWORD,
            file_index_high: DWORD,
            file_index_low: DWORD,
        }

        #[link(name = "kernel32")]
        extern "system" {
            pub(super) fn CreateFileW(
                lp_file_name: *const u16,
                dw_desired_access: DWORD,
                dw_share_mode: DWORD,
                lp_security_attributes: *mut c_void,
                dw_creation_disposition: DWORD,
                dw_flags_and_attributes: DWORD,
                h_template_file: HANDLE,
            ) -> HANDLE;
            pub(super) fn GetFileInformationByHandle(
                h_file: HANDLE,
                lp_file_information: *mut ByHandleFileInformation,
            ) -> BOOL;
        }
        #[link(name = "ntdll")]
        extern "system" {
            pub(super) fn NtCreateFile(
                file_handle: *mut HANDLE,
                desired_access: ACCESS_MASK,
                object_attributes: *mut ObjectAttributes,
                io_status_block: *mut IoStatusBlock,
                allocation_size: *mut i64,
                file_attributes: ULONG,
                share_access: ULONG,
                create_disposition: ULONG,
                create_options: ULONG,
                ea_buffer: *mut c_void,
                ea_length: ULONG,
            ) -> NTSTATUS;
            pub(super) fn RtlNtStatusToDosError(status: NTSTATUS) -> DWORD;
        }

        pub(super) fn to_wide(path: &Path) -> Result<Vec<u16>, AppError> {
            let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
            if wide.iter().any(|&u| u == 0) {
                return Err(AppError::validation("路径含内部 NUL".to_string()));
            }
            wide.push(0);
            Ok(wide)
        }
        pub(super) fn name_to_wide(name: &std::ffi::OsStr) -> Result<Vec<u16>, AppError> {
            let mut wide: Vec<u16> = name.encode_wide().collect();
            if wide.is_empty() || wide.iter().any(|&u| u == 0) {
                return Err(AppError::validation("相对路径名非法".to_string()));
            }
            wide.push(0);
            Ok(wide)
        }

        let parent_wide = to_wide(parent)?;
        let mut name_wide = name_to_wide(name)?;
        let parent_raw = unsafe {
            CreateFileW(
                parent_wide.as_ptr(),
                GENERIC_READ | FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            )
        };
        if parent_raw.is_null() || parent_raw == INVALID_HANDLE_VALUE {
            return Err(AppError::from(std::io::Error::last_os_error()));
        }
        let parent_handle = unsafe { OwnedHandle::from_raw_handle(parent_raw as RawHandle) };
        let mut pinfo: ByHandleFileInformation = unsafe { zeroed() };
        let pok = unsafe {
            GetFileInformationByHandle(parent_handle.as_raw_handle() as HANDLE, &mut pinfo)
        };
        if pok == 0 {
            return Err(AppError::from(std::io::Error::last_os_error()));
        }
        if pinfo.file_attributes & FILE_ATTRIBUTE_DIRECTORY == 0
            || pinfo.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(AppError::validation(
                "父目录不是普通目录（reparse/非目录），拒绝确认身份".to_string(),
            ));
        }

        let name_units = name_wide.len().saturating_sub(1);
        let byte_len = name_units * 2;
        if byte_len > u16::MAX as usize {
            return Err(AppError::validation("相对路径名过长".to_string()));
        }
        let mut unicode = UnicodeString {
            length: byte_len as USHORT,
            maximum_length: (byte_len + 2) as USHORT,
            buffer: name_wide.as_mut_ptr(),
        };
        let mut attrs = ObjectAttributes {
            length: size_of::<ObjectAttributes>() as ULONG,
            root_directory: parent_handle.as_raw_handle() as HANDLE,
            object_name: &mut unicode,
            attributes: OBJ_CASE_INSENSITIVE,
            security_descriptor: ptr::null_mut(),
            security_quality_of_service: ptr::null_mut(),
        };
        let mut iosb: IoStatusBlock = unsafe { zeroed() };
        let mut out: HANDLE = ptr::null_mut();
        let status = unsafe {
            NtCreateFile(
                &mut out,
                GENERIC_READ | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                &mut attrs,
                &mut iosb,
                ptr::null_mut(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_OPEN,
                FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
                ptr::null_mut(),
                0,
            )
        };
        if status < 0 {
            let dos = unsafe { RtlNtStatusToDosError(status) };
            return Err(AppError::from(std::io::Error::from_raw_os_error(
                dos as i32,
            )));
        }
        if out.is_null() || out == INVALID_HANDLE_VALUE {
            return Err(AppError::generic(
                "NtCreateFile 返回无效 HANDLE（dirent 身份确认）".to_string(),
            ));
        }
        let child = unsafe { OwnedHandle::from_raw_handle(out as RawHandle) };
        let mut cinfo: ByHandleFileInformation = unsafe { zeroed() };
        let cok =
            unsafe { GetFileInformationByHandle(child.as_raw_handle() as HANDLE, &mut cinfo) };
        if cok == 0 {
            return Err(AppError::from(std::io::Error::last_os_error()));
        }
        if cinfo.file_attributes & FILE_ATTRIBUTE_DIRECTORY != 0
            || cinfo.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(AppError::validation(
                "最终目录项不是普通文件（reparse/目录）".to_string(),
            ));
        }
        let file_index = ((cinfo.file_index_high as u64) << 32) | (cinfo.file_index_low as u64);
        Ok(BoundFileIdentity {
            volume_serial: cinfo.volume_serial_number,
            file_index,
        })
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, name);
        Err(AppError::from(std::io::Error::new(
            ErrorKind::Unsupported,
            "当前平台无法经父目录确认文件身份",
        )))
    }
}

/// Business Logic（为什么需要这个函数）:
///     Completed history 晋升（含 DurabilityPending 重试与 intent 恢复）前，必须证明路径上的
///     普通文件仍是原始传输内容，且 fsync 作用在该同一句柄上。若仅按路径 reopen+fsync，
///     替换后的同尺寸普通文件会静默通过 no-follow 并被认证为成功。
///
/// Code Logic（这个函数做什么）:
///     1) no-follow **读写**打开普通文件句柄（Windows FlushFileBuffers 需 GENERIC_WRITE）；
///     2) 读 len，流式 SHA256（同一句柄）；
///     3) 与 expected size/sha 比对；
///     4) 同一句柄 sync_all；
///     5) fsync 父目录（Windows 目录句柄同样需写权限）；
///     6) 经父目录 HANDLE/fd 重新打开目录项，比对 file id/inode，不一致则拒绝晋升。
pub(super) async fn certify_final_file_for_history(
    final_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<(), AppError> {
    let path = final_path.to_path_buf();
    let expected_sha = expected_sha256.to_string();
    tokio::task::spawn_blocking(move || {
        certify_final_file_for_history_blocking(&path, expected_size, &expected_sha)
    })
    .await
    .map_err(|e| AppError::generic(format!("certify_final_file_for_history join 失败: {e}")))?
}

/// Business Logic（为什么需要这个函数）:
///     certify 的同步实现，在 blocking 线程持有文件句柄完成 hash/fsync/身份确认。
///
/// Code Logic（这个函数做什么）:
///     见 `certify_final_file_for_history`。
///     以 writable=true no-follow 打开：Windows 上同一句柄 `sync_all`→FlushFileBuffers
///     需要 GENERIC_WRITE；只读打开会 AccessDenied，Completed history 永远无法晋升。
pub(super) fn certify_final_file_for_history_blocking(
    final_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<(), AppError> {
    use std::io::Read;

    // 同步 no-follow 读写打开：读用于 hash，写权限用于 Windows FlushFileBuffers。
    let std_file = open_regular_file_nofollow_std(final_path, true)?;
    let meta = std_file.metadata().map_err(AppError::from)?;
    if meta.len() != expected_size {
        return Err(AppError::validation(format!(
            "最终文件长度与任务不一致: path={}, len={}, expected={expected_size}",
            final_path.display(),
            meta.len()
        )));
    }
    let bound_id = file_identity_from_std(&std_file)?;

    // 同一句柄流式 SHA256。
    let mut file = std_file;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 8192];
    // 从头读：open 后默认 offset=0。
    loop {
        let n = file.read(&mut buf).map_err(AppError::from)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected_sha256 {
        return Err(AppError::validation(format!(
            "最终文件 SHA256 与任务不一致（可能被替换）: path={}, actual={actual}, expected={expected_sha256}",
            final_path.display()
        )));
    }

    // 同一句柄 fsync（Windows 需写权限句柄）。
    file.sync_all().map_err(AppError::from)?;
    if let Some(parent) = final_path.parent() {
        fsync_dir(parent)?;
    }

    // 经父目录重新打开目录项，确认仍是同一 file id/inode。
    let dirent_id = dirent_identity_via_parent(final_path)?;
    if dirent_id != bound_id {
        return Err(AppError::validation(format!(
            "最终文件目录项身份已变化（可能被原子替换），拒绝写 Completed history: {}",
            final_path.display()
        )));
    }
    // 保持 file 活到身份确认之后，缩小替换窗口。
    drop(file);
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     hard_link 不可用（跨卷/网络盘/受限 FS）时仍必须保证 no-replace：已有目标则失败、
///     绝不覆盖竞争者；也不得用零字节占位 + 普通 rename（崩溃会留下假最终文件，外部
///     进程可在间隙替换路径）。
///
/// Code Logic（这个函数做什么）:
///     在 blocking 线程调用平台原生**原子排他 rename**：
///     - Linux：`renameat2(..., RENAME_NOREPLACE)`；
///     - macOS：`renamex_np(..., RENAME_EXCL)`；
///     - Windows：`MoveFileExW` **不带** `MOVEFILE_REPLACE_EXISTING`（目标存在 → 失败）；
///     平台/内核无法提供 no-replace 时 fail-closed 返回 Unsupported，**禁止**占位回退。
pub(super) async fn rename_no_replace(tmp_path: &Path, final_path: &Path) -> std::io::Result<()> {
    let from = tmp_path.to_path_buf();
    let to = final_path.to_path_buf();
    tokio::task::spawn_blocking(move || rename_no_replace_blocking(&from, &to))
        .await
        .map_err(|e| std::io::Error::other(format!("rename_no_replace join 失败: {e}")))?
}

/// Business Logic（为什么需要这个函数）:
///     真正的 no-replace rename 是同步 syscall，必须在 blocking 上下文执行。
///
/// Code Logic（这个函数做什么）:
///     见 `rename_no_replace` 的平台分支；仅同步路径。
pub(super) fn rename_no_replace_blocking(
    tmp_path: &Path,
    final_path: &Path,
) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let from = CString::new(tmp_path.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::new(ErrorKind::InvalidInput, "tmp 路径含内部 NUL"))?;
        let to = CString::new(final_path.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::new(ErrorKind::InvalidInput, "final 路径含内部 NUL"))?;
        // RENAME_NOREPLACE = 1：目标存在则失败，不覆盖。
        pub(super) const RENAME_NOREPLACE: libc::c_uint = 1;
        // SAFETY: 路径已转为以 NUL 结尾的 CString；renameat2 对 AT_FDCWD 语义与 rename 一致。
        let rc = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                RENAME_NOREPLACE,
            )
        };
        if rc == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        // ENOSYS / EINVAL：内核或 FS 不支持 noreplace → fail-closed，禁止回退普通 rename。
        if err.raw_os_error() == Some(libc::ENOSYS) || err.raw_os_error() == Some(libc::EINVAL) {
            return Err(std::io::Error::new(
                ErrorKind::Unsupported,
                format!("文件系统不支持 RENAME_NOREPLACE: {err}"),
            ));
        }
        // EEXIST → AlreadyExists，供调用方换后缀。
        if err.raw_os_error() == Some(libc::EEXIST) {
            return Err(std::io::Error::new(ErrorKind::AlreadyExists, err));
        }
        Err(err)
    }

    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let from = CString::new(tmp_path.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::new(ErrorKind::InvalidInput, "tmp 路径含内部 NUL"))?;
        let to = CString::new(final_path.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::new(ErrorKind::InvalidInput, "final 路径含内部 NUL"))?;
        // RENAME_EXCL = 0x00000004：目标存在则失败（与 Linux RENAME_NOREPLACE 同语义）。
        pub(super) const RENAME_EXCL: libc::c_uint = 0x0000_0004;
        // SAFETY: CString 保证 NUL 结尾；renamex_np 在失败时设置 errno。
        let rc = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), RENAME_EXCL) };
        if rc == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ENOTSUP) || err.raw_os_error() == Some(libc::ENOSYS) {
            return Err(std::io::Error::new(
                ErrorKind::Unsupported,
                format!("文件系统不支持 RENAME_EXCL: {err}"),
            ));
        }
        if err.raw_os_error() == Some(libc::EEXIST) {
            return Err(std::io::Error::new(ErrorKind::AlreadyExists, err));
        }
        Err(err)
    }

    #[cfg(windows)]
    {
        // 关键：Rust std 的 `fs::rename` 在 Windows 调用 MoveFileExW **带**
        // MOVEFILE_REPLACE_EXISTING，会覆盖竞争者。必须直接调原生 API 且 flags=0。
        use std::os::windows::ffi::OsStrExt;

        #[link(name = "kernel32")]
        extern "system" {
            pub(super) fn MoveFileExW(
                lp_existing_file_name: *const u16,
                lp_new_file_name: *const u16,
                dw_flags: u32,
            ) -> i32;
        }

        /// Business Logic: 把 OsStr 编成 Windows 宽字符串（NUL 结尾），供 MoveFileExW 使用。
        /// Code Logic: encode_wide 追加 0；路径含内部 NUL 时返回 InvalidInput。
        pub(super) fn to_wide(path: &Path) -> std::io::Result<Vec<u16>> {
            let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
            if wide.iter().any(|&u| u == 0) {
                return Err(std::io::Error::new(
                    ErrorKind::InvalidInput,
                    "路径含内部 NUL",
                ));
            }
            wide.push(0);
            Ok(wide)
        }

        let from = to_wide(tmp_path)?;
        let to = to_wide(final_path)?;
        // flags=0：不带 MOVEFILE_REPLACE_EXISTING(0x1)；目标存在则失败，绝不覆盖。
        // SAFETY: 宽字符串以 NUL 结尾；flags 明确为 0。
        let ok = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) };
        if ok != 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        // ERROR_ALREADY_EXISTS=183 / ERROR_FILE_EXISTS=80 → AlreadyExists（调用方换后缀）。
        match err.raw_os_error() {
            Some(183) | Some(80) => Err(std::io::Error::new(ErrorKind::AlreadyExists, err)),
            _ if err.kind() == ErrorKind::AlreadyExists => Err(err),
            _ => Err(err),
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (tmp_path, final_path);
        Err(std::io::Error::new(
            ErrorKind::Unsupported,
            "当前平台无法提供原子 no-replace rename；禁止占位回退",
        ))
    }
}

/// 接收失败统一处理：标记 failed + 写历史 + remove + emit failed。
///
/// Business Logic（为什么需要这个函数）:
///     接收端运行在 HTTP 路由中，独立后端没有 GUI 句柄；失败事件仍需统一通知 GUI 或 headless adapter。
///
/// Code Logic（这个函数做什么）:
///     更新 registry/history 后通过 `AppState::emit_event` 发布 `transfer:failed`。
pub(super) async fn on_receive_failed(state: &AppState, transfer_id: &str, error: &str) {
    let completed_at = now_iso();
    // 在移除前抓任务快照，用于写 Failed 墓碑（Finding 4）。
    let snapshot = state.transfers.get(transfer_id);
    state
        .transfers
        .mark_failed(transfer_id, completed_at.clone());
    if let Some(t) = state.transfers.get(transfer_id) {
        let _ = state.transfer_repo.record(&t).await;
    }
    state.transfers.remove(transfer_id);

    // Finding 4: 写失败墓碑，重放的最后一块返回 success:false，但与第一次 finalize 失败的结果一致，
    // 避免"第一次失败、重放变成功"的诡异步态。
    if let Some(t) = &snapshot {
        state.transfers.record_tombstone(
            transfer_id,
            crate::transfer::registry::TransferTombstone {
                outcome: crate::transfer::registry::TransferOutcome::Failed {
                    error: error.to_string(),
                },
                received_bytes: t.transferred_bytes,
                size: t.size,
                filename: t.filename.clone(),
                completed_at: completed_at.clone(),
                created_at: std::time::Instant::now(),
            },
        );
    }

    state.emit_event(
        "transfer:failed",
        serde_json::json!({
            "id": transfer_id,
            "status": "failed",
            "errorMessage": error,
        }),
    );
}
