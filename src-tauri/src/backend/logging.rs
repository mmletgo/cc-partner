//! backend/logging.rs — 后端受限本地文件日志（路径/配置 + 精确 size 轮转 writer）。
//!
//! Business Logic（为什么需要这个模块）:
//!     detached `cc-partner-backend` 需要留下可诊断、可轮转、权限收紧的本地日志，
//!     供 doctor/smoke 与人工排障读取；日志体积必须有界，避免磁盘被打满。
//!
//! Code Logic（这个模块做什么）:
//!     提供 `BackendLogConfig`、固定上限常量、`RotatingLogWriter`（按字节精确轮转，
//!     历史 `.1` 最新 / `.N` 最旧）以及 `tracing_appender::non_blocking` 包装守卫。

use crate::config;
use crate::error::AppError;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};

/// 后端当前日志文件最大字节数（current 另算，不含历史）。
pub const BACKEND_LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;

/// 历史轮转文件保留数量（`backend.log.1` … `backend.log.N`）。
pub const BACKEND_LOG_HISTORY_FILES: usize = 3;

/// 当前日志文件名（固定）。
pub const BACKEND_LOG_FILE_NAME: &str = "backend.log";

/// 后端文件日志配置（目录、上限、历史份数）。
///
/// Business Logic（为什么需要这个结构）:
///     serve/doctor/测试都需要同一套路径与轮转上限，避免硬编码散落。
///
/// Code Logic（这个结构做什么）:
///     持有 log_dir / max_bytes / history_files；生产路径由 `data_dir()/logs` 派生。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendLogConfig {
    /// 日志目录（Unix 创建时 mode 0700）。
    pub log_dir: PathBuf,
    /// 当前文件最大字节数。
    pub max_bytes: u64,
    /// 历史文件保留份数（`.1` … `.N`）。
    pub history_files: usize,
}

impl BackendLogConfig {
    /// 构造生产环境默认日志配置。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     serve 启动时需与 config/control/db 同一 `data_dir` 隔离根下的固定日志路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     解析 `backend_log_dir()`，上限固定为 5 MiB / 3 历史文件。
    pub fn production() -> Result<Self, AppError> {
        Ok(Self {
            log_dir: config::backend_log_dir()?,
            max_bytes: BACKEND_LOG_MAX_BYTES,
            history_files: BACKEND_LOG_HISTORY_FILES,
        })
    }

    /// 当前日志文件绝对路径：`<log_dir>/backend.log`。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     writer/doctor/测试需要定位 current 文件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 `log_dir` 下拼接固定文件名 `backend.log`。
    pub fn current_path(&self) -> PathBuf {
        self.log_dir.join(BACKEND_LOG_FILE_NAME)
    }

    /// 第 `index` 份历史文件路径（1-based：`.1` 最新）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     轮转与测试需要稳定的历史文件命名。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 `backend.log.<index>`；index 从 1 开始。
    pub fn history_path(&self, index: usize) -> PathBuf {
        self.log_dir
            .join(format!("{BACKEND_LOG_FILE_NAME}.{index}"))
    }
}

/// 持有 non-blocking worker 的生命周期守卫（须存活到 serve 结束）。
///
/// Business Logic（为什么需要这个结构）:
///     non-blocking 后台线程在 drop 时 flush；serve 必须持有 guard 直到关闭，
///     否则进程退出前诊断记录可能丢失。
///
/// Code Logic（这个结构做什么）:
///     包装 `tracing_appender::non_blocking::WorkerGuard`，drop 时等待 worker 排空。
#[derive(Debug)]
pub struct BackendLoggingGuard {
    _worker_guard: WorkerGuard,
}

/// 精确按字节轮转的后端日志 writer。
///
/// Business Logic（为什么需要这个结构）:
///     需要确定性的 size 上限与历史文件语义（`.1` 最新、`.N` 最旧、无 `.N+1`），
///     第三方 rolling 策略往往按时间或近似大小，无法满足 doctor/smoke 契约。
///
/// Code Logic（这个结构做什么）:
///     在 mutex 内维护 current 文件句柄与已写长度；写入前若会越界则先轮转；
///     单条记录超过 max 返回 `InvalidInput` 且不写盘。
#[derive(Debug)]
pub struct RotatingLogWriter {
    config: BackendLogConfig,
    state: Mutex<WriterState>,
}

#[derive(Debug)]
struct WriterState {
    file: Option<File>,
    current_len: u64,
}

impl RotatingLogWriter {
    /// 打开（或创建）轮转日志 writer。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     serve 启动时必须确保日志目录存在且权限收紧，并读取 current 已有长度以支持重启续写。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 log_dir（Unix 0700）→ 以 append 打开 current（Unix 0600）→ 读取 metadata 长度。
    pub fn open(config: BackendLogConfig) -> io::Result<Self> {
        ensure_log_dir(&config.log_dir)?;
        let path = config.current_path();
        let file = open_current_file(&path)?;
        let current_len = file.metadata()?.len();
        Ok(Self {
            config,
            state: Mutex::new(WriterState {
                file: Some(file),
                current_len,
            }),
        })
    }

    /// 当前日志文件路径。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     测试与 doctor 需要核对 writer 绑定的 current 路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `BackendLogConfig::current_path`。
    pub fn current_path(&self) -> PathBuf {
        self.config.current_path()
    }

    /// 包装为 non-blocking writer，并返回生命周期守卫。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     tracing 热路径不应阻塞在磁盘 IO；同时必须持有 guard 直到 serve 结束才能 flush。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用 `tracing_appender::non_blocking`，把 `WorkerGuard` 装入 `BackendLoggingGuard`。
    pub fn into_non_blocking(self) -> (NonBlocking, BackendLoggingGuard) {
        let (non_blocking, worker_guard) = tracing_appender::non_blocking(self);
        (
            non_blocking,
            BackendLoggingGuard {
                _worker_guard: worker_guard,
            },
        )
    }

    /// 在持锁状态下写入一条记录（可能先轮转）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单条写入是轮转决策的原子单位：要么整条进 current，要么先轮转再写。
    ///
    /// Code Logic（这个函数做什么）:
    ///     单条 > max → InvalidInput；将越界 → rotate；再 append 并更新 current_len。
    fn write_record_locked(&self, state: &mut WriterState, buf: &[u8]) -> io::Result<usize> {
        let record_len = buf.len() as u64;
        if record_len > self.config.max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "单条日志 {} 字节超过上限 {} 字节",
                    record_len, self.config.max_bytes
                ),
            ));
        }

        if state.current_len.saturating_add(record_len) > self.config.max_bytes {
            self.rotate_locked(state)?;
        }

        let file = state
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("日志文件句柄在写入前丢失"))?;
        file.write_all(buf)?;
        file.flush()?;
        state.current_len = state.current_len.saturating_add(record_len);
        Ok(buf.len())
    }

    /// 执行 size 轮转：关文件 → 删最旧 → 依次 rename → 重开 current。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     必须在写入前完成轮转，保证任何文件都不会超过配置上限。
    ///
    /// Code Logic（这个函数做什么）:
    ///     close → 删除 `.N` → `.N-1→.N` … `.1→.2` → current→`.1` → 新建 current（0600）。
    ///     任一步失败上抛，不丢弃调用方记录（由上层决定是否重试）。
    fn rotate_locked(&self, state: &mut WriterState) -> io::Result<()> {
        // 先关闭 current，避免 rename 打开文件。
        if let Some(file) = state.file.take() {
            file.sync_all().ok();
            drop(file);
        }

        let history = self.config.history_files.max(1);
        let oldest = self.config.history_path(history);
        if oldest.exists() {
            fs::remove_file(&oldest)?;
        }

        // 从旧到新反向 rename：.2→.3, .1→.2
        for index in (1..history).rev() {
            let from = self.config.history_path(index);
            let to = self.config.history_path(index + 1);
            if from.exists() {
                fs::rename(&from, &to)?;
                #[cfg(unix)]
                apply_file_mode_0600(&to)?;
            }
        }

        let current = self.config.current_path();
        if current.exists() {
            let first_history = self.config.history_path(1);
            fs::rename(&current, &first_history)?;
            #[cfg(unix)]
            apply_file_mode_0600(&first_history)?;
        }

        let file = create_current_file(&current)?;
        state.file = Some(file);
        state.current_len = 0;
        Ok(())
    }
}

impl Write for RotatingLogWriter {
    /// 将缓冲写入当前日志（必要时先轮转）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     作为 tracing/non_blocking 的底层 `Write` 实现，承接每一条已格式化记录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     获取 mutex → `write_record_locked`；锁中毒时返回 Other 错误。
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("日志 writer 锁中毒"))?;
        self.write_record_locked(&mut state, buf)
    }

    /// 刷新当前文件缓冲。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     non_blocking drop / 显式 flush 时需要把诊断记录落盘。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对已打开的 current 文件调用 `flush`。
    fn flush(&mut self) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("日志 writer 锁中毒"))?;
        if let Some(file) = state.file.as_mut() {
            file.flush()?;
        }
        Ok(())
    }
}

/// 确保日志目录存在并在 Unix 上设为 0700。
///
/// Business Logic（为什么需要这个函数）:
///     日志可能含本机路径/错误摘要，目录必须仅当前用户可进。
///
/// Code Logic（这个函数做什么）:
///     `create_dir_all` 后 Unix 上 `set_permissions(0o700)`。
fn ensure_log_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(dir)?.permissions();
        perms.set_mode(0o700);
        fs::set_permissions(dir, perms)?;
    }
    Ok(())
}

/// 以 append 打开 current 日志文件，Unix 上强制 0600。
///
/// Business Logic（为什么需要这个函数）:
///     重启后续写必须保留已有内容，且权限始终收紧。
///
/// Code Logic（这个函数做什么）:
///     OpenOptions create+append+read；创建后/打开后 Unix 设 0600。
fn open_current_file(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)?;
    #[cfg(unix)]
    apply_file_mode_0600(path)?;
    Ok(file)
}

/// 创建新的空 current 文件（轮转后），Unix 上 0600。
///
/// Business Logic（为什么需要这个函数）:
///     轮转后 current 必须是新文件，且权限不能继承旧 umask 宽松值。
///
/// Code Logic（这个函数做什么）:
///     create+write+truncate，随后 Unix 设 0600。
fn create_current_file(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .read(true)
        .open(path)?;
    #[cfg(unix)]
    apply_file_mode_0600(path)?;
    Ok(file)
}

/// Unix：把文件 mode 设为 0600。
///
/// Business Logic（为什么需要这个函数）:
///     日志文件只应本用户读写。
///
/// Code Logic（这个函数做什么）:
///     `PermissionsExt::set_mode(0o600)` 后 `set_permissions`。
#[cfg(unix)]
fn apply_file_mode_0600(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 构造临时目录下的测试配置。
    ///
    /// Business Logic: 测试必须隔离，不能触碰用户真实 data_dir。
    /// Code Logic: tempfile + 极小 max_bytes 便于触发轮转。
    fn test_config(max_bytes: u64, history_files: usize) -> (tempfile::TempDir, BackendLogConfig) {
        let dir = tempfile::tempdir().expect("创建临时日志目录");
        let config = BackendLogConfig {
            log_dir: dir.path().to_path_buf(),
            max_bytes,
            history_files,
        };
        (dir, config)
    }

    fn read_string(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }

    fn file_len(path: &Path) -> u64 {
        fs::metadata(path).map(|m| m.len()).unwrap_or(0)
    }

    #[test]
    fn append_below_limit_stays_on_current() {
        let (_dir, config) = test_config(32, 3);
        let mut writer = RotatingLogWriter::open(config.clone()).expect("open writer");
        writer.write_all(b"hello").expect("write");
        writer.flush().expect("flush");

        assert_eq!(read_string(&config.current_path()), "hello");
        assert!(!config.history_path(1).exists());
    }

    #[test]
    fn rotates_before_crossing_size_limit() {
        let (_dir, config) = test_config(10, 3);
        let mut writer = RotatingLogWriter::open(config.clone()).expect("open writer");

        writer.write_all(b"12345").expect("first");
        writer.write_all(b"67890").expect("second fills");
        // 当前长度 10；再写 1 字节会越界，必须先轮转
        writer.write_all(b"X").expect("triggers rotate");
        writer.flush().expect("flush");

        assert_eq!(read_string(&config.current_path()), "X");
        assert_eq!(read_string(&config.history_path(1)), "1234567890");
        assert!(file_len(&config.current_path()) <= config.max_bytes);
        assert!(file_len(&config.history_path(1)) <= config.max_bytes);
    }

    #[test]
    fn history_ordering_keeps_dot1_newest_and_never_creates_dot4() {
        let (_dir, config) = test_config(4, 3);
        let mut writer = RotatingLogWriter::open(config.clone()).expect("open writer");

        // 每条 4 字节：写满即轮转，生成 A→B→C→D 序列
        for payload in [b"AAAA", b"BBBB", b"CCCC", b"DDDD"] {
            writer.write_all(payload).expect("write record");
        }
        // 再写一条触发把 DDDD 推入历史
        writer.write_all(b"EEEE").expect("final");
        writer.flush().expect("flush");

        assert_eq!(read_string(&config.current_path()), "EEEE");
        assert_eq!(read_string(&config.history_path(1)), "DDDD"); // 最新历史
        assert_eq!(read_string(&config.history_path(2)), "CCCC");
        assert_eq!(read_string(&config.history_path(3)), "BBBB"); // 最旧
        assert!(!config.history_path(4).exists(), ".4 绝不应存在");
        assert!(
            !config.log_dir.join("backend.log.4").exists(),
            "不得保留超出 history 的文件"
        );
    }

    #[test]
    fn reopen_reads_existing_current_length() {
        let (_dir, config) = test_config(10, 3);
        {
            let mut writer = RotatingLogWriter::open(config.clone()).expect("open");
            writer.write_all(b"12345").expect("seed");
            writer.flush().expect("flush");
        }

        let mut writer = RotatingLogWriter::open(config.clone()).expect("reopen");
        // 已有 5 字节，再写 5 字节刚好到上限，不应轮转
        writer.write_all(b"67890").expect("append to existing");
        writer.flush().expect("flush");
        assert_eq!(read_string(&config.current_path()), "1234567890");
        assert!(!config.history_path(1).exists());

        // 再写 1 字节必须轮转（证明 reopen 正确读取了 current_len=10）
        writer.write_all(b"Z").expect("rotate after reopen");
        writer.flush().expect("flush");
        assert_eq!(read_string(&config.current_path()), "Z");
        assert_eq!(read_string(&config.history_path(1)), "1234567890");
    }

    #[test]
    fn record_larger_than_limit_returns_invalid_input_and_never_exceeds_max() {
        let (_dir, config) = test_config(8, 3);
        let mut writer = RotatingLogWriter::open(config.clone()).expect("open");
        writer.write_all(b"ok").expect("small write");

        let err = writer
            .write_all(b"0123456789") // 10 > 8
            .expect_err("超大单条应失败");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        writer.flush().ok();
        assert_eq!(read_string(&config.current_path()), "ok");
        assert!(file_len(&config.current_path()) <= config.max_bytes);
        // 历史也不应被污染出超限文件
        for i in 1..=3 {
            let p = config.history_path(i);
            if p.exists() {
                assert!(file_len(&p) <= config.max_bytes);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_permissions_dir_0700_files_0600() {
        use std::os::unix::fs::PermissionsExt;

        let (_dir, config) = test_config(16, 3);
        let mut writer = RotatingLogWriter::open(config.clone()).expect("open");
        writer.write_all(b"1234567890abcdef").expect("fill");
        writer.write_all(b"next").expect("rotate");
        writer.flush().expect("flush");

        let dir_mode = fs::metadata(&config.log_dir)
            .expect("dir meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "日志目录应为 0700");

        for path in [
            config.current_path(),
            config.history_path(1),
        ] {
            let mode = fs::metadata(&path)
                .expect("file meta")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{path:?} 应为 0600");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_files_remain_readable_writable() {
        let (_dir, config) = test_config(16, 3);
        let mut writer = RotatingLogWriter::open(config.clone()).expect("open");
        writer.write_all(b"hello-windows").expect("write");
        writer.flush().expect("flush");

        // 当前进程应能读写 current
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(config.current_path())
            .expect("current 应对本进程可读写");
        let mut buf = String::new();
        use std::io::Read;
        f.read_to_string(&mut buf).expect("read");
        assert!(buf.contains("hello-windows"));
    }

    #[test]
    fn non_blocking_guard_flush_on_drop() {
        let (_dir, config) = test_config(64, 3);
        let writer = RotatingLogWriter::open(config.clone()).expect("open");
        let path = config.current_path();
        let (mut non_blocking, guard) = writer.into_non_blocking();

        non_blocking
            .write_all(b"flushed-by-guard\n")
            .expect("non_blocking write");
        non_blocking.flush().expect("flush request");
        drop(non_blocking);
        drop(guard); // 等待 worker 排空

        assert!(
            read_string(&path).contains("flushed-by-guard"),
            "drop guard 后记录应落盘"
        );
    }

    #[test]
    fn backend_log_config_paths_and_production_defaults() {
        let config = BackendLogConfig {
            log_dir: PathBuf::from("/tmp/cc-partner-data/logs"),
            max_bytes: BACKEND_LOG_MAX_BYTES,
            history_files: BACKEND_LOG_HISTORY_FILES,
        };
        assert_eq!(
            config.current_path(),
            PathBuf::from("/tmp/cc-partner-data/logs/backend.log")
        );
        assert_eq!(
            config.history_path(1),
            PathBuf::from("/tmp/cc-partner-data/logs/backend.log.1")
        );
        assert_eq!(config.max_bytes, 5 * 1024 * 1024);
        assert_eq!(config.history_files, 3);

        // production() 依赖当前 data_dir；仅校验常量与文件名契约，避免改写全局 env 竞态
        if let Ok(prod) = BackendLogConfig::production() {
            assert_eq!(prod.max_bytes, BACKEND_LOG_MAX_BYTES);
            assert_eq!(prod.history_files, BACKEND_LOG_HISTORY_FILES);
            assert_eq!(
                prod.current_path().file_name().and_then(|s| s.to_str()),
                Some(BACKEND_LOG_FILE_NAME)
            );
            assert!(
                prod.log_dir.ends_with("logs"),
                "生产日志目录应以 logs 结尾: {:?}",
                prod.log_dir
            );
        }
    }
}
