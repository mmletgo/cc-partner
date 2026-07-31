//! config_store.rs — 配置文件的原子落盘与可注入 IO 适配层
//!
//! Business Logic（为什么需要这个模块）:
//!     配置是设备级权威状态；并发写、断电或崩溃时若半写 `config.json`，会导致启动失败或
//!     静默丢字段。需要“写临时文件 → fsync → 重读校验 → 原子替换 → 父目录 fsync”的
//!     可恢复语义，并在测试中按阶段注入故障证明旧文件/内存不被破坏。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `ConfigStore` trait、`FsConfigStore` 文件系统实现、`ConfigIo` 可替换 IO 适配器，
//!     以及测试用故障注入/内存 store。临时文件命名 `.config.json.<uuid>.tmp`，启动时清理
//!     超过 24 小时的陈旧临时文件。

use crate::config::AppConfig;
use crate::error::AppError;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// 配置持久化抽象。
///
/// Business Logic（为什么需要这个 trait）:
///     生产路径走磁盘原子写，命令层测试需要注入 save 失败，因此读写必须可替换。
///
/// Code Logic（这个 trait 做什么）:
///     `load` 读取权威 config；`save_atomic` 以 durable replace 语义提交候选配置。
pub trait ConfigStore: Send + Sync {
    /// 加载权威配置文件。
    fn load(&self) -> Result<AppConfig, AppError>;

    /// 原子保存候选配置（失败时不改写旧文件）。
    fn save_atomic(&self, candidate: &AppConfig) -> Result<(), AppError>;
}

/// 可注入的底层配置 IO 阶段。
///
/// Business Logic（为什么需要这个枚举）:
///     故障注入测试要精确覆盖 create/write/flush/file-sync/rename/directory-sync 各阶段。
///
/// Code Logic（这个枚举做什么）:
///     标记 `ConfigIo` 实现中可失败的步骤，供测试 adapter 匹配。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigIoStage {
    Create,
    Write,
    Flush,
    FileSync,
    Rename,
    DirectorySync,
}

/// 配置落盘底层 IO 适配器。
///
/// Business Logic（为什么需要这个 trait）:
///     真实 `std::fs` 很难在单元测试中稳定模拟中途断电；把 create/write/fsync/rename
///     抽成 trait 后，测试可在指定阶段返回错误并断言旧 JSON 仍可解析。
///
/// Code Logic（这个 trait 做什么）:
///     封装临时文件创建、写入、flush、sync、原子替换与父目录 sync。
pub trait ConfigIo: Send + Sync {
    /// 以 create_new 打开临时文件。
    fn create_new(&self, path: &Path) -> Result<File, AppError>;

    /// 写入全部字节。
    fn write_all(&self, file: &mut File, data: &[u8]) -> Result<(), AppError>;

    /// flush 用户态缓冲。
    fn flush(&self, file: &mut File) -> Result<(), AppError>;

    /// 同步文件数据到稳定存储。
    fn sync_all(&self, file: &mut File) -> Result<(), AppError>;

    /// 把临时文件原子替换为目标 config.json。
    fn atomic_replace(&self, temp: &Path, target: &Path) -> Result<(), AppError>;

    /// 同步父目录元数据，使 rename 本身 durable。
    fn sync_directory(&self, dir: &Path) -> Result<(), AppError>;
}

/// 生产用标准文件系统 IO。
///
/// Business Logic（为什么需要这个结构）:
///     默认路径需要真实 fsync/rename；Windows 还需 replace-existing + write-through 语义。
///
/// Code Logic（这个结构做什么）:
///     直接调用 std::fs 与平台原生 replace API。
#[derive(Debug, Default)]
pub struct StdConfigIo;

impl ConfigIo for StdConfigIo {
    /// 创建新临时文件（已存在则失败，防碰撞）。
    fn create_new(&self, path: &Path) -> Result<File, AppError> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(AppError::from)
    }

    /// 写入全部字节。
    fn write_all(&self, file: &mut File, data: &[u8]) -> Result<(), AppError> {
        file.write_all(data).map_err(AppError::from)
    }

    /// flush 缓冲。
    fn flush(&self, file: &mut File) -> Result<(), AppError> {
        file.flush().map_err(AppError::from)
    }

    /// fsync 文件内容。
    fn sync_all(&self, file: &mut File) -> Result<(), AppError> {
        file.sync_all().map_err(AppError::from)
    }

    /// 平台原子替换。
    fn atomic_replace(&self, temp: &Path, target: &Path) -> Result<(), AppError> {
        atomic_replace_path(temp, target)
    }

    /// 父目录 fsync。
    fn sync_directory(&self, dir: &Path) -> Result<(), AppError> {
        sync_dir(dir)
    }
}

/// 文件系统 ConfigStore。
///
/// Business Logic（为什么需要这个结构）:
///     生产配置读写必须落到 `config.json`，并保证崩溃后仍有一份完整权威文件。
///
/// Code Logic（这个结构做什么）:
///     持有目标路径与 `ConfigIo`；`save_atomic` 执行完整 durable replace 流水线；
///     `load` 只读正式文件，从不把 `.tmp` 当配置。
pub struct FsConfigStore {
    path: PathBuf,
    io: Arc<dyn ConfigIo>,
}

impl FsConfigStore {
    /// 使用默认 `config.json` 路径与标准 IO 构造 store。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     `AppConfig::load/save` 与 runtime 初始化需要零配置接入生产路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     解析 `config_file_path()`，包装 `StdConfigIo`。
    pub fn default_path() -> Result<Self, AppError> {
        Ok(Self::new(
            crate::config::config_file_path()?,
            Arc::new(StdConfigIo),
        ))
    }

    /// 使用指定路径与 IO 适配器构造 store。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单测需要临时目录与故障注入 IO，不能写真实 `~/.cc-partner`。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 path 与 Arc IO。
    pub fn new(path: PathBuf, io: Arc<dyn ConfigIo>) -> Self {
        Self { path, io }
    }

    /// 返回权威配置文件路径。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     测试与诊断需要核对落盘位置。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回内部 path 引用。
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 清理同目录下超过 24 小时的 `.config.json.*.tmp`。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     崩溃可能留下临时文件；启动时清理陈旧 tmp，避免目录膨胀，且绝不把 tmp 当配置加载。
    ///
    /// Code Logic（这个函数做什么）:
    ///     枚举父目录，匹配 `.config.json.<uuid>.tmp` 前缀模式；mtime 早于 now-24h 则删除。
    pub fn cleanup_stale_temp_files(&self) -> Result<(), AppError> {
        let Some(parent) = self.path.parent() else {
            return Ok(());
        };
        if !parent.exists() {
            return Ok(());
        }
        let cutoff = SystemTime::now()
            .checked_sub(Duration::from_secs(24 * 60 * 60))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        for entry in fs::read_dir(parent)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !is_config_temp_name(&name) {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let modified = match meta.modified() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if modified < cutoff {
                let _ = fs::remove_file(entry.path());
            }
        }
        Ok(())
    }
}

impl ConfigStore for FsConfigStore {
    /// 读取权威 config.json。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     启动与迁移需要加载磁盘上的权威配置；临时文件不得参与。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先清理陈旧 tmp；读 UTF-8 JSON 并反序列化为 `AppConfig`。
    fn load(&self) -> Result<AppConfig, AppError> {
        let _ = self.cleanup_stale_temp_files();
        if !self.path.exists() {
            return Err(AppError::not_found(format!(
                "配置文件不存在: {}",
                self.path.display()
            )));
        }
        let text = fs::read_to_string(&self.path)?;
        let cfg = serde_json::from_str(&text)?;
        Ok(cfg)
    }

    /// 以 durable replace 语义保存候选配置。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户改设置后必须完整落盘；任一步失败时旧配置继续权威，避免半写 JSON。
    ///
    /// Code Logic（这个函数做什么）:
    ///     1) 确保父目录存在（Unix 0700）；2) 同目录 `.config.json.<uuid>.tmp` create_new；
    ///     3) 写紧凑 UTF-8 JSON、文件 0600、flush + sync_all；4) 重读反序列化并与 candidate 等价；
    ///     5) 原子替换；6) 父目录 sync；失败只删除本次 temp。
    fn save_atomic(&self, candidate: &AppConfig) -> Result<(), AppError> {
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| AppError::validation("配置路径缺少父目录"))?;
        ensure_config_dir(parent)?;

        let temp = parent.join(format!(".config.json.{}.tmp", Uuid::new_v4()));
        let write_result = (|| -> Result<(), AppError> {
            let mut file = self.io.create_new(&temp)?;
            set_owner_only_file_permissions(&temp)?;
            let text = serde_json::to_string(candidate)?;
            self.io.write_all(&mut file, text.as_bytes())?;
            self.io.flush(&mut file)?;
            self.io.sync_all(&mut file)?;
            // 在 rename 前释放句柄，避免 Windows 占用导致 replace 失败。
            drop(file);

            let reread_text = fs::read_to_string(&temp)?;
            let reread: AppConfig = serde_json::from_str(&reread_text)
                .map_err(|e| AppError::generic(format!("配置临时文件重读反序列化失败: {e}")))?;
            if !configs_equivalent(&reread, candidate) {
                return Err(AppError::generic(
                    "配置临时文件重读内容与候选不一致，拒绝提交",
                ));
            }

            // rename/replace 是 durability commit 点：成功后磁盘权威已是 candidate。
            // DirectorySync 仅加固目录项元数据；失败不得回滚已提交内容，也不应让上层
            // 误判为“未提交”而跳过 memory swap（否则 disk=NEW / memory=OLD → lost update）。
            self.io.atomic_replace(&temp, &self.path)?;
            if let Err(e) = self.io.sync_directory(parent) {
                tracing::warn!(
                    "配置目录 fsync 失败（config 已原子替换成功，继续提交）: {e}; path={}",
                    self.path.display()
                );
            }
            Ok(())
        })();

        if write_result.is_err() {
            // 仅清理本次 temp；rename 成功后 temp 已不存在，remove 是 no-op。
            let _ = fs::remove_file(&temp);
        }
        write_result
    }
}

/// 判断目录项名是否为本模块的配置临时文件。
///
/// Business Logic（为什么需要这个函数）:
///     启动清理必须只碰 `.config.json.*.tmp`，不能误删其它隐藏文件。
///
/// Code Logic（这个函数做什么）:
///     匹配前缀 `.config.json.` 且后缀 `.tmp`。
fn is_config_temp_name(name: &str) -> bool {
    name.starts_with(".config.json.")
        && name.ends_with(".tmp")
        && name.len() > ".config.json..tmp".len()
}

/// 比较两份配置在持久化语义上是否等价。
///
/// Business Logic（为什么需要这个函数）:
///     re-read 校验要拒绝“写坏/被篡改但碰巧还能反序列化”的临时文件。
///
/// Code Logic（这个函数做什么）:
///     通过 serde_json Value 比较，避免手写 PartialEq 漏字段。
fn configs_equivalent(a: &AppConfig, b: &AppConfig) -> bool {
    match (serde_json::to_value(a), serde_json::to_value(b)) {
        (Ok(va), Ok(vb)) => va == vb,
        _ => false,
    }
}

/// 确保配置目录存在并尽量收紧权限。
///
/// Business Logic（为什么需要这个函数）:
///     config/secrets 目录应对当前用户私有（Unix 0700）。
///
/// Code Logic（这个函数做什么）:
///     create_dir_all；Unix 再 set 0o700。
fn ensure_config_dir(path: &Path) -> Result<(), AppError> {
    if !path.exists() {
        fs::create_dir_all(path)?;
    }
    set_owner_only_dir_permissions(path)?;
    Ok(())
}

/// Unix 目录权限 0700；其它平台 no-op。
fn set_owner_only_dir_permissions(path: &Path) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o700);
        fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Unix 文件权限 0600；其它平台 no-op。
fn set_owner_only_file_permissions(path: &Path) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// 平台原子替换：Unix rename；Windows ReplaceFileW / MoveFileExW。
///
/// Business Logic（为什么需要这个函数）:
///     替换过程不能出现“先删目标再写新文件”的空窗；Windows 还需 write-through 语义。
///
/// Code Logic（这个函数做什么）:
///     Unix: `fs::rename`；Windows: 目标存在用 `ReplaceFileW`，否则
///     `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`。
fn atomic_replace_path(temp: &Path, target: &Path) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        fs::rename(temp, target).map_err(AppError::from)
    }
    #[cfg(windows)]
    {
        windows_atomic_replace(temp, target)
    }
    #[cfg(not(any(unix, windows)))]
    {
        fs::rename(temp, target).map_err(AppError::from)
    }
}

/// Windows 原子替换实现。
#[cfg(windows)]
fn windows_atomic_replace(temp: &Path, target: &Path) -> Result<(), AppError> {
    use std::os::windows::ffi::OsStrExt;

    /// 把路径编成 NUL 结尾宽字符串。
    fn to_wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(
            lp_existing_file_name: *const u16,
            lp_new_file_name: *const u16,
            dw_flags: u32,
        ) -> i32;
        fn ReplaceFileW(
            lp_replaced_file_name: *const u16,
            lp_replacement_file_name: *const u16,
            lp_backup_file_name: *const u16,
            dw_replace_flags: u32,
            lp_exclude: *mut core::ffi::c_void,
            lp_reserved: *mut core::ffi::c_void,
        ) -> i32;
    }

    let temp_w = to_wide(temp);
    let target_w = to_wide(target);

    if target.exists() {
        // ReplaceFileW 无 write-through 标志；替换成功后对目标文件 FlushFileBuffers，
        // 对齐首创路径 MoveFileExW(...|WRITE_THROUGH) 的文件数据落盘语义。
        // 父目录 sync 仍由 save_atomic 的 sync_directory 负责（目录项 durability）。
        let ok = unsafe {
            ReplaceFileW(
                target_w.as_ptr(),
                temp_w.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(AppError::from(std::io::Error::last_os_error()));
        }
        // best-effort write-through：打开已替换目标并 fsync 文件数据。
        match OpenOptions::new().write(true).open(target) {
            Ok(mut f) => {
                if let Err(e) = f.flush() {
                    tracing::warn!("Windows ReplaceFileW 后 flush 目标失败: {e}");
                }
                if let Err(e) = f.sync_all() {
                    tracing::warn!("Windows ReplaceFileW 后 fsync 目标失败: {e}");
                }
            }
            Err(e) => {
                tracing::warn!("Windows ReplaceFileW 后打开目标做 fsync 失败: {e}");
            }
        }
        Ok(())
    } else {
        let flags = MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;
        let ok = unsafe { MoveFileExW(temp_w.as_ptr(), target_w.as_ptr(), flags) };
        if ok == 0 {
            return Err(AppError::from(std::io::Error::last_os_error()));
        }
        Ok(())
    }
}

/// 同步目录项，使 rename durable。
///
/// Business Logic（为什么需要这个函数）:
///     仅 fsync 文件不够：目录项更新也需落盘，否则崩溃后可能看不到新文件名。
///
/// Code Logic（这个函数做什么）:
///     打开目录句柄后 `sync_all`；Windows 用 BACKUP_SEMANTICS 打开目录。
fn sync_dir(dir: &Path) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        let file = File::open(dir)?;
        file.sync_all()?;
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(dir)?;
        file.sync_all()?;
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = dir;
        Ok(())
    }
}

/// 测试用：可在指定阶段失败一次的 ConfigIo。
///
/// Business Logic（为什么需要这个结构）:
///     单元测试需证明 create/write/flush/file-sync/rename/directory-sync 任一失败时旧配置仍可解析。
///
/// Code Logic（这个结构做什么）:
///     包装真实 IO；命中 `fail_stage` 时返回一次错误，之后恢复正常。
#[derive(Clone)]
pub struct FaultInjectingConfigIo {
    inner: Arc<dyn ConfigIo>,
    fail_stage: ConfigIoStage,
    /// 是否已经注入过失败（true = 后续放行）。
    fired: Arc<std::sync::atomic::AtomicBool>,
    /// 可选：Write 阶段写入替代字节以制造 re-read mismatch。
    corrupt_write: Option<Vec<u8>>,
}

impl FaultInjectingConfigIo {
    /// 构造在指定阶段失败一次的 IO。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     每个故障场景对应一个 stage。
    ///
    /// Code Logic（这个函数做什么）:
    ///     包装 inner，记录 fail_stage。
    pub fn fail_once(inner: Arc<dyn ConfigIo>, fail_stage: ConfigIoStage) -> Self {
        Self {
            inner,
            fail_stage,
            fired: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            corrupt_write: None,
        }
    }

    /// 构造 Write 阶段写入损坏内容（用于 re-read mismatch）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     需要覆盖“临时文件可反序列化但与 candidate 不等价”的拒绝路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Write 时改写 payload，其它阶段走 inner。
    pub fn corrupt_on_write(inner: Arc<dyn ConfigIo>, corrupt_bytes: Vec<u8>) -> Self {
        Self {
            inner,
            fail_stage: ConfigIoStage::Write,
            fired: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            corrupt_write: Some(corrupt_bytes),
        }
    }

    /// 若本阶段应失败则返回错误。
    fn maybe_fail(&self, stage: ConfigIoStage) -> Result<(), AppError> {
        if stage == self.fail_stage
            && self
                .fired
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                )
                .is_ok()
            && self.corrupt_write.is_none()
        {
            return Err(AppError::generic(format!(
                "注入故障: config io stage {stage:?}"
            )));
        }
        Ok(())
    }
}

impl ConfigIo for FaultInjectingConfigIo {
    fn create_new(&self, path: &Path) -> Result<File, AppError> {
        self.maybe_fail(ConfigIoStage::Create)?;
        self.inner.create_new(path)
    }

    fn write_all(&self, file: &mut File, data: &[u8]) -> Result<(), AppError> {
        if let Some(corrupt) = &self.corrupt_write {
            // 损坏路径：写入替代字节，不触发 generic fail。
            return self.inner.write_all(file, corrupt);
        }
        self.maybe_fail(ConfigIoStage::Write)?;
        self.inner.write_all(file, data)
    }

    fn flush(&self, file: &mut File) -> Result<(), AppError> {
        self.maybe_fail(ConfigIoStage::Flush)?;
        self.inner.flush(file)
    }

    fn sync_all(&self, file: &mut File) -> Result<(), AppError> {
        self.maybe_fail(ConfigIoStage::FileSync)?;
        self.inner.sync_all(file)
    }

    fn atomic_replace(&self, temp: &Path, target: &Path) -> Result<(), AppError> {
        self.maybe_fail(ConfigIoStage::Rename)?;
        self.inner.atomic_replace(temp, target)
    }

    fn sync_directory(&self, dir: &Path) -> Result<(), AppError> {
        self.maybe_fail(ConfigIoStage::DirectorySync)?;
        self.inner.sync_directory(dir)
    }
}

/// 测试用内存 ConfigStore。
///
/// Business Logic（为什么需要这个结构）:
///     ConfigRuntime 并发测试不依赖真实磁盘。
///
/// Code Logic（这个结构做什么）:
///     Mutex 内保存 Option<AppConfig>；可选 fail_next_save。
#[derive(Default)]
pub struct MemoryConfigStore {
    data: std::sync::Mutex<Option<AppConfig>>,
    fail_next_save: std::sync::atomic::AtomicBool,
}

impl MemoryConfigStore {
    /// 用初始配置构造。
    pub fn with_config(cfg: AppConfig) -> Self {
        Self {
            data: std::sync::Mutex::new(Some(cfg)),
            fail_next_save: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// 下次 save 失败一次。
    pub fn fail_next_save(&self) {
        self.fail_next_save
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// 读取当前内存快照。
    pub fn snapshot(&self) -> Option<AppConfig> {
        self.data.lock().ok().and_then(|g| g.clone())
    }
}

impl ConfigStore for MemoryConfigStore {
    fn load(&self) -> Result<AppConfig, AppError> {
        self.data
            .lock()
            .map_err(|_| AppError::generic("MemoryConfigStore 锁中毒"))?
            .clone()
            .ok_or_else(|| AppError::not_found("MemoryConfigStore 为空"))
    }

    fn save_atomic(&self, candidate: &AppConfig) -> Result<(), AppError> {
        if self
            .fail_next_save
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(AppError::generic("注入故障: MemoryConfigStore save"));
        }
        let mut guard = self
            .data
            .lock()
            .map_err(|_| AppError::generic("MemoryConfigStore 锁中毒"))?;
        *guard = Some(candidate.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig};

    /// 构造可校验的最小合法配置。
    fn sample_config(device_name: &str) -> AppConfig {
        AppConfig {
            device_id: "dev-atomic-1".into(),
            device_name: device_name.into(),
            http_port: 0,
            receive_dir: "/tmp/cc-partner-files".into(),
            db_path: "/tmp/cc-partner-data.db".into(),
            screenshot_hotkey: "<ctrl>+<shift>+s".into(),
            prompt_optimizer_hotkey: "<ctrl>".into(),
            prompt_optimizer_fill_language: "zh".into(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
            agent_hub: crate::config::AgentHubConfig::default(),
            manual_peers: Vec::new(),
        }
    }

    /// 读取磁盘 JSON 并断言可反序列化。
    fn assert_disk_parses(path: &Path) -> AppConfig {
        let text = fs::read_to_string(path).expect("应能读 config.json");
        serde_json::from_str(&text).expect("旧 JSON 必须仍可解析")
    }

    /// 统计父目录中匹配的临时文件数量。
    fn count_temp_files(dir: &Path) -> usize {
        fs::read_dir(dir)
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .filter(|e| is_config_temp_name(&e.file_name().to_string_lossy()))
            .count()
    }

    #[test]
    fn utf8_round_trip_preserves_chinese_fields() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.json");
        let mut cfg = sample_config("测试设备-中文");
        cfg.receive_dir = "/tmp/接收目录".into();
        let store = FsConfigStore::new(path.clone(), Arc::new(StdConfigIo));
        store.save_atomic(&cfg).expect("save");
        let loaded = store.load().expect("load");
        assert_eq!(loaded.device_name, "测试设备-中文");
        assert_eq!(loaded.receive_dir, "/tmp/接收目录");
        assert!(configs_equivalent(&loaded, &cfg));
    }

    #[test]
    fn temp_files_use_unique_names_per_invocation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.json");
        // 在 rename 前失败：失败后 temp 应被清理。
        let io = Arc::new(FaultInjectingConfigIo::fail_once(
            Arc::new(StdConfigIo),
            ConfigIoStage::Rename,
        ));
        let store = FsConfigStore::new(path, io);
        let cfg = sample_config("n1");
        let _ = store.save_atomic(&cfg);
        assert_eq!(count_temp_files(temp.path()), 0);

        // 连续两次健康 save 后不应残留 temp。
        let store2 = FsConfigStore::new(temp.path().join("config.json"), Arc::new(StdConfigIo));
        store2.save_atomic(&cfg).expect("save1");
        let mut cfg2 = cfg.clone();
        cfg2.device_name = "n2".into();
        store2.save_atomic(&cfg2).expect("save2");
        assert_eq!(count_temp_files(temp.path()), 0);
        assert_eq!(store2.load().unwrap().device_name, "n2");
    }

    #[test]
    fn re_read_mismatch_rejects_and_keeps_old_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.json");
        let initial = sample_config("old-name");
        let good = FsConfigStore::new(path.clone(), Arc::new(StdConfigIo));
        good.save_atomic(&initial).expect("seed");

        let corrupt_cfg = sample_config("tampered");
        let corrupt_bytes = serde_json::to_vec(&corrupt_cfg).expect("ser");
        let bad_io = Arc::new(FaultInjectingConfigIo::corrupt_on_write(
            Arc::new(StdConfigIo),
            corrupt_bytes,
        ));
        let store = FsConfigStore::new(path.clone(), bad_io);
        let candidate = sample_config("new-name");
        let err = store.save_atomic(&candidate).expect_err("mismatch 应失败");
        assert!(
            err.to_string().contains("不一致") || err.to_string().contains("mismatch"),
            "错误应提示 re-read 不一致: {err}"
        );
        let on_disk = assert_disk_parses(&path);
        assert_eq!(on_disk.device_name, "old-name");
        assert_eq!(count_temp_files(temp.path()), 0);

        // 之后健康 save 成功。
        let healthy = FsConfigStore::new(path.clone(), Arc::new(StdConfigIo));
        healthy
            .save_atomic(&candidate)
            .expect("后续健康 save 应成功");
        assert_eq!(healthy.load().unwrap().device_name, "new-name");
    }

    fn assert_stage_failure_preserves_old(stage: ConfigIoStage) {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.json");
        let initial = sample_config("old");
        FsConfigStore::new(path.clone(), Arc::new(StdConfigIo))
            .save_atomic(&initial)
            .expect("seed");

        let io = Arc::new(FaultInjectingConfigIo::fail_once(
            Arc::new(StdConfigIo),
            stage,
        ));
        let store = FsConfigStore::new(path.clone(), io);
        let mut candidate = sample_config("new");
        candidate.device_name = "should-not-commit".into();
        let err = store
            .save_atomic(&candidate)
            .expect_err("注入故障应导致失败");
        assert!(
            err.to_string().contains("注入故障") || err.to_string().contains("stage"),
            "错误应来自注入: {err}"
        );
        let on_disk = assert_disk_parses(&path);
        assert_eq!(on_disk.device_name, "old");
        assert_eq!(count_temp_files(temp.path()), 0, "失败后不得残留本次 temp");

        // 健康 save 随后成功。
        let healthy = FsConfigStore::new(path, Arc::new(StdConfigIo));
        healthy
            .save_atomic(&candidate)
            .expect("后续健康 save 应成功");
        assert_eq!(healthy.load().unwrap().device_name, "should-not-commit");
    }

    #[test]
    fn fault_at_create_preserves_old_json() {
        assert_stage_failure_preserves_old(ConfigIoStage::Create);
    }

    #[test]
    fn fault_at_write_preserves_old_json() {
        assert_stage_failure_preserves_old(ConfigIoStage::Write);
    }

    #[test]
    fn fault_at_flush_preserves_old_json() {
        assert_stage_failure_preserves_old(ConfigIoStage::Flush);
    }

    #[test]
    fn fault_at_file_sync_preserves_old_json() {
        assert_stage_failure_preserves_old(ConfigIoStage::FileSync);
    }

    #[test]
    fn fault_at_rename_preserves_old_json() {
        assert_stage_failure_preserves_old(ConfigIoStage::Rename);
    }

    #[test]
    fn fault_at_directory_sync_still_commits_new_json() {
        // directory-sync 失败发生在 rename 之后：rename 已是 commit 点。
        // 契约：save_atomic 返回 Ok，磁盘权威 = NEW，无 temp 残留。
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.json");
        let initial = sample_config("old");
        FsConfigStore::new(path.clone(), Arc::new(StdConfigIo))
            .save_atomic(&initial)
            .expect("seed");

        let io = Arc::new(FaultInjectingConfigIo::fail_once(
            Arc::new(StdConfigIo),
            ConfigIoStage::DirectorySync,
        ));
        let store = FsConfigStore::new(path.clone(), io);
        let candidate = sample_config("new-after-rename");
        store
            .save_atomic(&candidate)
            .expect("dir sync 失败不应回滚已 rename 的提交");
        assert_eq!(count_temp_files(temp.path()), 0);
        let on_disk = assert_disk_parses(&path);
        assert_eq!(
            on_disk.device_name, "new-after-rename",
            "rename 后磁盘权威必须是 NEW"
        );

        let healthy = FsConfigStore::new(path, Arc::new(StdConfigIo));
        let mut next = candidate.clone();
        next.device_name = "healthy-next".into();
        healthy.save_atomic(&next).expect("后续健康 save 应成功");
        assert_eq!(healthy.load().unwrap().device_name, "healthy-next");
    }

    #[test]
    fn cleanup_removes_only_stale_matching_temps() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.json");
        let store = FsConfigStore::new(path, Arc::new(StdConfigIo));

        let stale = temp.path().join(".config.json.stale-uuid.tmp");
        fs::write(&stale, b"{}").expect("write stale");
        let fresh = temp.path().join(".config.json.fresh-uuid.tmp");
        fs::write(&fresh, b"{}").expect("write fresh");
        let other = temp.path().join(".other.tmp");
        fs::write(&other, b"x").expect("other");

        assert!(is_config_temp_name(".config.json.abc.tmp"));
        assert!(!is_config_temp_name("config.json"));
        assert!(!is_config_temp_name(".other.tmp"));

        #[cfg(unix)]
        {
            use std::process::Command;
            let _ = Command::new("touch")
                .args(["-t", "200001010000", stale.to_str().unwrap()])
                .status();
            store.cleanup_stale_temp_files().expect("cleanup");
            assert!(!stale.exists(), "陈旧 temp 应被清理");
            assert!(fresh.exists(), "新鲜 temp 应保留");
            assert!(other.exists(), "非匹配文件不得删除");
        }
        #[cfg(not(unix))]
        {
            let _ = (store, stale, fresh, other);
        }
    }
}
