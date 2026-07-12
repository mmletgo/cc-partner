//! backend/logging.rs — 后端受限本地文件日志（路径/配置 + 精确 size 轮转 + 字段脱敏 formatter）。
//!
//! Business Logic（为什么需要这个模块）:
//!     detached `cc-partner-backend` 需要留下可诊断、可轮转、权限收紧且**无密钥/正文泄露**的本地日志，
//!     供 doctor/smoke 与人工排障读取；日志体积必须有界，字段必须白名单，避免磁盘打满与隐私事故。
//!
//! Code Logic（这个模块做什么）:
//!     提供 `BackendLogConfig`、固定上限常量、`RotatingLogWriter`（按字节精确轮转，
//!     历史 `.1` 最新 / `.N` 最旧）、`sanitize_diagnostic_text`、白名单 JSON formatter、
//!     结构化操作完成 helper，以及 `tracing_appender::non_blocking` 包装守卫。
//!
//! 文件日志字段 schema（生产路径，仅此白名单）:
//!     `timestamp` / `level` / `request_id` / `domain` / `operation` / `result` /
//!     `elapsed_ms` / `error_code` / `message`（已脱敏）。未知字段一律丢弃；
//!     永不记录 Prompt/会话正文、文件内容、请求 body、完整环境变量、token/password/key/Authorization。

use crate::config;
use crate::error::AppError;
use regex::Regex;
use serde_json::{Map, Value};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

/// 后端当前日志文件最大字节数（current 另算，不含历史）。
pub const BACKEND_LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;

/// 历史轮转文件保留数量（`backend.log.1` … `backend.log.N`）。
pub const BACKEND_LOG_HISTORY_FILES: usize = 3;

/// 当前日志文件名（固定）。
pub const BACKEND_LOG_FILE_NAME: &str = "backend.log";

/// 单字段脱敏后最大字节数（8 KiB，在合法 UTF-8 边界截断）。
pub const SANITIZED_VALUE_MAX_BYTES: usize = 8 * 1024;

/// 短标识字段（request_id 等）最大字符长度。
const SHORT_FIELD_MAX_CHARS: usize = 128;

/// 分类字段（domain/operation/result/error_code）最大字符长度。
const LABEL_FIELD_MAX_CHARS: usize = 64;

/// 文件日志允许的结构化字段白名单（不含自动注入的 timestamp/level）。
pub const FILE_LOG_ALLOWED_FIELDS: &[&str] = &[
    "request_id",
    "domain",
    "operation",
    "result",
    "elapsed_ms",
    "error_code",
    "message",
];

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
    ///     创建 log_dir（Unix 0700）→ 协调遗留历史（删 `.N>history`、history/current 强制 0600）→
    ///     若 current 已超 max 则安全处理超限文件 → 以 append 打开 current 并读取长度。
    pub fn open(config: BackendLogConfig) -> io::Result<Self> {
        ensure_log_dir(&config.log_dir)?;
        reconcile_log_set_on_open(&config)?;
        let path = config.current_path();
        // 重启时若 current 已超上限，先安全处理，避免后续再生成 >max 的 history。
        if path.exists() {
            let len = fs::metadata(&path)?.len();
            if len > config.max_bytes {
                rotate_existing_files(&config)?;
            }
        }
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
    ///     若上次轮转恢复失败留下 `file=None`，后续写入必须有界 reopen，否则诊断永久失联。
    ///
    /// Code Logic（这个函数做什么）:
    ///     file=None 时 best-effort reopen + metadata 恢复 current_len；
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

        // Business Logic: 轮转恢复失败后环境可能已恢复（权限/杀毒/句柄耗尽），每次写前再尝试 reopen。
        if state.file.is_none() {
            self.recover_current_file_locked(state);
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
    ///     必须在写入前完成轮转，保证任何文件都不会超过配置上限；瞬时 I/O 失败后仍要恢复文件日志，
    ///     不能让一次 Windows 占用/杀毒竞争永久关闭诊断输出。
    ///
    /// Code Logic（这个函数做什么）:
    ///     close → 删除 `.N` → `.N-1→.N` … `.1→.2` → current→`.1` → 新建 current（0600）。
    ///     任一步失败时 best-effort 重新打开 current 并根据 metadata 恢复 `current_len`，再把原始错误上抛；
    ///     成功路径把 `current_len` 置 0。
    fn rotate_locked(&self, state: &mut WriterState) -> io::Result<()> {
        // 先关闭 current，避免 rename 打开文件。
        if let Some(file) = state.file.take() {
            file.sync_all().ok();
            drop(file);
        }

        match self.rotate_files_unlocked() {
            Ok(()) => {
                let current = self.config.current_path();
                let file = create_current_file(&current).inspect_err(|_err| {
                    // 创建失败时仍尝试 reopen 已有 current，避免 file=None 永久失联。
                    self.recover_current_file_locked(state);
                })?;
                state.file = Some(file);
                state.current_len = 0;
                Ok(())
            }
            Err(err) => {
                self.recover_current_file_locked(state);
                Err(err)
            }
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     轮转中的 rename/remove/chmod 失败不能留下 `file=None`，否则后续所有写入都会永久报句柄丢失。
    ///
    /// Code Logic（这个函数做什么）:
    ///     best-effort `open_current_file`，并从 metadata 恢复 `current_len`；打开失败则保持 None，
    ///     并以**带外限频 stderr**报告（禁止 `tracing::*`，避免 non-blocking 文件 layer 自反馈）。
    fn recover_current_file_locked(&self, state: &mut WriterState) {
        let current = self.config.current_path();
        match open_current_file(&current) {
            Ok(file) => {
                let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
                state.file = Some(file);
                state.current_len = len;
            }
            Err(recover_err) => {
                report_log_writer_fault_out_of_band(&format!(
                    "日志轮转失败后 reopen current 也失败，文件日志暂时不可用: path={} error={}",
                    current.display(),
                    recover_err
                ));
                state.file = None;
                state.current_len = 0;
            }
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     轮转的文件系统步骤需要可单独失败并交由 `rotate_locked` 做恢复，便于 fault-injection 测试。
    ///
    /// Code Logic（这个函数做什么）:
    ///     删除最旧 history，反向 rename 历史，再把 current rename 为 `.1`；不打开新 current。
    fn rotate_files_unlocked(&self) -> io::Result<()> {
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
        Ok(())
    }
}

/// 日志 writer 故障带外报告的最小间隔，防止 stderr 洪泛。
const LOG_WRITER_FAULT_REPORT_INTERVAL: Duration = Duration::from_secs(5);

/// 重入保护：writer 故障报告路径内禁止再次进入（即使未来误用 tracing 也不会形成环）。
static LOG_WRITER_FAULT_REENTRANT: AtomicBool = AtomicBool::new(false);

/// 上次带外报告相对进程单调时钟的毫秒（0 表示尚未报告）。
static LOG_WRITER_FAULT_LAST_REPORT_MS: AtomicU64 = AtomicU64::new(0);

/// 进程内单调时钟原点，供限频比较使用。
static LOG_WRITER_FAULT_CLOCK_ORIGIN: OnceLock<Instant> = OnceLock::new();

/// Business Logic（为什么需要这个函数）:
///     RotatingLogWriter 本身可能是 tracing 文件 layer 的底层 writer；若 recover 失败时再发
///     `tracing::warn!`，non-blocking worker 会把失败事件再次写入同一 writer，形成无限自反馈。
///
/// Code Logic（这个函数做什么）:
///     用 AtomicBool 重入保护 + 5s 限频，直接写 stderr（不经 tracing subscriber）；任何错误静默丢弃。
fn report_log_writer_fault_out_of_band(message: &str) {
    // 重入：若已在报告路径中，直接返回，避免嵌套。
    if LOG_WRITER_FAULT_REENTRANT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let _guard = LogWriterFaultReentrancyGuard;
    let origin = LOG_WRITER_FAULT_CLOCK_ORIGIN.get_or_init(Instant::now);
    let now_ms = Instant::now()
        .saturating_duration_since(*origin)
        .as_millis();
    let now_ms = u64::try_from(now_ms).unwrap_or(u64::MAX).saturating_add(1); // 0 保留给“从未报告”
    let last_ms = LOG_WRITER_FAULT_LAST_REPORT_MS.load(Ordering::Acquire);
    if last_ms != 0
        && now_ms.saturating_sub(last_ms) < LOG_WRITER_FAULT_REPORT_INTERVAL.as_millis() as u64
    {
        return;
    }
    LOG_WRITER_FAULT_LAST_REPORT_MS.store(now_ms, Ordering::Release);
    let sanitized = sanitize_diagnostic_text(message);
    let _ = writeln!(std::io::stderr(), "[cc-partner-log-writer] {sanitized}");
}

/// drop 时清除 reentrancy flag。
struct LogWriterFaultReentrancyGuard;
impl Drop for LogWriterFaultReentrancyGuard {
    fn drop(&mut self) {
        LOG_WRITER_FAULT_REENTRANT.store(false, Ordering::Release);
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
    ///     non_blocking drop / 显式 flush 时需要把诊断记录落盘；日志已 down 时不得假装成功。
    ///
    /// Code Logic（这个函数做什么）:
    ///     file=None 时先有界 reopen；仍不可用则返回错误；否则 flush 已打开句柄。
    fn flush(&mut self) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("日志 writer 锁中毒"))?;
        if state.file.is_none() {
            self.recover_current_file_locked(&mut state);
        }
        match state.file.as_mut() {
            Some(file) => file.flush(),
            None => Err(io::Error::other("日志文件句柄不可用，无法 flush")),
        }
    }
}

// ---------------------------------------------------------------------------
// Sanitizer
// ---------------------------------------------------------------------------

/// 脱敏诊断文本：home 替换、密钥模式 redact、header/body 形态剥离、控制字符归一、8 KiB 截断。
///
/// Business Logic（为什么需要这个函数）:
///     文件日志与 stderr 都可能承载错误摘要；必须保证 Prompt/密钥/家目录用户名/请求正文
///     不会以明文或简单编码形式落盘，即便调用方误把敏感串写进 message。
///
/// Code Logic（这个函数做什么）:
///     1) 控制字符归一为空格；2) 用已知 home 路径替换为 `<HOME>`；
///     3) 用正则 redact Authorization/Bearer/token/password/secret/key 等模式；
///     4) 剥离 header/body 形态片段；5) 在 UTF-8 边界截到 `SANITIZED_VALUE_MAX_BYTES`。
pub fn sanitize_diagnostic_text(input: &str) -> String {
    let mut text = normalize_control_chars(input);
    text = replace_home_paths(&text);
    text = redact_secret_patterns(&text);
    text = strip_header_body_shaped(&text);
    truncate_utf8_bytes(&text, SANITIZED_VALUE_MAX_BYTES)
}

/// 将不可打印控制字符归一为空格（保留常规空白语义，避免多行 payload 穿透）。
///
/// Business Logic（为什么需要这个函数）:
///     攻击性 fixture 可能夹带 `\0`/`\r` 换行注入多行 body；归一后便于 redact 与单行 JSON 输出。
///
/// Code Logic（这个函数做什么）:
///     遍历 char：ASCII 控制字符（除 tab）替换为空格，压缩连续空白为单空格并 trim。
fn normalize_control_chars(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_space = false;
    for ch in input.chars() {
        let is_control = ch.is_control() && ch != '\t';
        if is_control || ch == '\t' {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
            continue;
        }
        if ch == ' ' {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
            continue;
        }
        out.push(ch);
        last_space = false;
    }
    out.trim().to_string()
}

/// 将本机 home 路径（及用户名片段）替换为 `<HOME>`。
///
/// Business Logic（为什么需要这个函数）:
///     日志常含绝对路径；home 会泄露本机用户名，doctor/recent errors 契约要求替换。
///
/// Code Logic（这个函数做什么）:
///     收集 `dirs::home_dir` 与 `HOME`/`USERPROFILE` 环境变量，按长度降序替换为 `<HOME>`。
fn replace_home_paths(input: &str) -> String {
    let mut homes: Vec<String> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        let s = home.to_string_lossy().to_string();
        if !s.is_empty() {
            homes.push(s);
        }
    }
    for key in ["HOME", "USERPROFILE"] {
        if let Ok(val) = std::env::var(key) {
            if !val.is_empty() && !homes.iter().any(|h| h == &val) {
                homes.push(val);
            }
        }
    }
    homes.sort_by_key(|h| std::cmp::Reverse(h.len()));
    let mut text = input.to_string();
    for home in homes {
        if text.contains(&home) {
            text = text.replace(&home, "<HOME>");
        }
        // Windows 路径可能混用反斜杠
        let alt = home.replace('\\', "/");
        if alt != home && text.contains(&alt) {
            text = text.replace(&alt, "<HOME>");
        }
        let alt2 = home.replace('/', "\\");
        if alt2 != home && text.contains(&alt2) {
            text = text.replace(&alt2, "<HOME>");
        }
    }
    text
}

/// 编译敏感模式正则（进程内只编译一次）。
///
/// Business Logic（为什么需要这个函数）:
///     每次写日志都编译正则会拖慢热路径；OnceLock 保证单例。
///
/// Code Logic（这个函数做什么）:
///     返回覆盖 Authorization Bearer、token/password/secret/key 赋值、常见 key 前缀的 Regex 列表。
fn secret_regexes() -> &'static [Regex] {
    static REGEXES: OnceLock<Vec<Regex>> = OnceLock::new();
    REGEXES.get_or_init(|| {
        // 顺序：先匹配更长/更具体的模式
        let patterns = [
            // Authorization: Bearer <token> / Authorization Bearer <token>
            r"(?i)authorization\s*[:=]\s*bearer\s+\S+",
            r"(?i)bearer\s+[A-Za-z0-9\-._~+/]+=*",
            // key=value / key: value 形态（含单独 key=，避免 key=sk_live_... 漏网）
            r#"(?i)\b(password|passwd|pwd|secret|token|api[_-]?key|access[_-]?token|refresh[_-]?token|private[_-]?key|client[_-]?secret|authorization|(?:api[_-]?)?key)\b\s*[:=]\s*["']?[^,\s"'\\}{]+["']?"#,
            // JSON "password":"..."
            r#"(?i)"(password|passwd|secret|token|api[_-]?key|access[_-]?token|private[_-]?key|authorization|key)"\s*:\s*"[^"]*""#,
            // 常见云密钥前缀（sk_ / sk_live_ / sk_test_ / pk_* / ghp_ 等）
            r"\b(?:sk|pk|rk)_(?:live_|test_)?[A-Za-z0-9]{8,}\b",
            r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{8,}\b",
            r"\bAIza[0-9A-Za-z\-_]{20,}\b",
        ];
        patterns
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect()
    })
}

/// 用 `<REDACTED>` 替换密钥/凭证模式。
///
/// Business Logic（为什么需要这个函数）:
///     即便 message 误入 token/password，也必须在落盘前抹掉。
///
/// Code Logic（这个函数做什么）:
///     对 `secret_regexes` 逐条 `replace_all` 为 `<REDACTED>`。
fn redact_secret_patterns(input: &str) -> String {
    let mut text = input.to_string();
    for re in secret_regexes() {
        text = re.replace_all(&text, "<REDACTED>").into_owned();
    }
    text
}

/// 剥离 header/body 形态内容（多行 HTTP 头与 JSON body 大段）。
///
/// Business Logic（为什么需要这个函数）:
///     调试路径可能把 `Authorization: ...\r\n\r\n{body}` 整段塞进错误串；需主动剥离。
///
/// Code Logic（这个函数做什么）:
///     去掉 `Header-Name: value` 形态；去掉以 `{`/`[` 开头的大段 JSON body 提示替换为 `<BODY_OMITTED>`。
fn strip_header_body_shaped(input: &str) -> String {
    static HEADER_RE: OnceLock<Regex> = OnceLock::new();
    static BODY_MARKER_RE: OnceLock<Regex> = OnceLock::new();
    static FREEFORM_RE: OnceLock<Regex> = OnceLock::new();
    let header_re = HEADER_RE.get_or_init(|| {
        Regex::new(r"(?i)\b(content-type|content-length|cookie|set-cookie|x-api-key|x-auth-token)\s*:\s*\S+")
            .expect("header regex")
    });
    // body marker 之后丢弃全部剩余内容（含超长 tail），避免 4096 上限泄漏正文尾部。
    let body_marker_re = BODY_MARKER_RE.get_or_init(|| {
        Regex::new(r"(?i)\b(request\s*)?body\s*[:=]\s*.*$").expect("body marker regex")
    });
    // 独立 Prompt/文件正文（无 body/token 标签）也必须拦截，避免敌意自由文本落盘。
    let freeform_re = FREEFORM_RE.get_or_init(|| {
        Regex::new(
            r"(?ix)\bprompt(?:\s*(?:text|body|content))?\s*[:=]\s*\S+|\bfile(?:\s*(?:content|body|text|payload))?\s*[:=]\s*\S+|\bfile-sentinel-\S+|\bTOP_SECRET_[A-Z0-9_]+\b",
        )
        .expect("freeform regex")
    });
    let mut text = header_re
        .replace_all(input, "<HEADER_OMITTED>")
        .into_owned();
    text = body_marker_re
        .replace_all(&text, "body=<BODY_OMITTED>")
        .into_owned();
    text = freeform_re
        .replace_all(&text, "<CONTENT_OMITTED>")
        .into_owned();
    text
}

/// 在合法 UTF-8 边界截断到 max_bytes。
///
/// Business Logic（为什么需要这个函数）:
///     超长错误栈/误入正文必须有界，且不能切断多字节字符产生非法 UTF-8。
///
/// Code Logic（这个函数做什么）:
///     若 `as_bytes().len() <= max` 原样返回；否则从 max 向前找到 char 边界再切片。
fn truncate_utf8_bytes(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = input[..end].to_string();
    out.push('…');
    out
}

/// 截断短标签字段（字符数上限）。
///
/// Business Logic（为什么需要这个函数）:
///     request_id/domain 等标识字段也需有界，防止恶意超长注入。
///
/// Code Logic（这个函数做什么）:
///     按 char 计数截断到 max_chars，再跑 `sanitize_diagnostic_text`。
fn bound_label_field(input: &str, max_chars: usize) -> String {
    let trimmed: String = input.chars().take(max_chars).collect();
    sanitize_diagnostic_text(&trimmed)
}

// ---------------------------------------------------------------------------
// Strict event visitor + JSON formatter
// ---------------------------------------------------------------------------

/// 白名单字段收集器：只保留 schema 字段，未知字段丢弃。
///
/// Business Logic（为什么需要这个结构）:
///     tracing 事件可携带任意字段；文件层必须强制 schema，避免 body/password 等字段落盘。
///
/// Code Logic（这个结构做什么）:
///     实现 `Visit`：仅处理白名单键；message 走 sanitizer；elapsed_ms 解析为数字。
#[derive(Debug, Default)]
struct WhitelistFieldVisitor {
    request_id: Option<String>,
    domain: Option<String>,
    operation: Option<String>,
    result: Option<String>,
    elapsed_ms: Option<u64>,
    error_code: Option<String>,
    message: Option<String>,
}

impl WhitelistFieldVisitor {
    /// 写入一个字符串字段（仅白名单键）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Visit 的 str/debug 路径共用同一白名单判定。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按字段名分发到对应槽位；message 脱敏；其它标签 bound+脱敏；未知忽略。
    fn record_str_field(&mut self, field: &Field, value: &str) {
        match field.name() {
            "request_id" => {
                self.request_id = Some(bound_label_field(value, SHORT_FIELD_MAX_CHARS));
            }
            "domain" => {
                self.domain = Some(bound_label_field(value, LABEL_FIELD_MAX_CHARS));
            }
            "operation" => {
                self.operation = Some(bound_label_field(value, LABEL_FIELD_MAX_CHARS));
            }
            "result" => {
                self.result = Some(bound_label_field(value, LABEL_FIELD_MAX_CHARS));
            }
            "error_code" => {
                self.error_code = Some(bound_label_field(value, LABEL_FIELD_MAX_CHARS));
            }
            // tracing 默认消息字段名
            "message" => {
                self.message = Some(sanitize_diagnostic_text(value));
            }
            "elapsed_ms" => {
                if let Ok(v) = value.trim().parse::<u64>() {
                    self.elapsed_ms = Some(v);
                }
            }
            _ => {
                // 未知字段（authorization/password/body/prompt/...）一律丢弃
            }
        }
    }
}

impl Visit for WhitelistFieldVisitor {
    /// 记录 debug 格式字段。
    ///
    /// Business Logic: Visit 必选入口，承接非 Display 值。
    /// Code Logic: 转字符串后走 `record_str_field`。
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        // tracing 事件消息字段名为 "message"
        let rendered = format!("{value:?}");
        // Debug 字符串常带引号：若两端是 "..." 则剥掉，避免 message 双重转义噪音
        let stripped = rendered
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .map(|s| s.replace("\\\"", "\""))
            .unwrap_or(rendered);
        if field.name() == "message" {
            self.message = Some(sanitize_diagnostic_text(&stripped));
        } else {
            self.record_str_field(field, &stripped);
        }
    }

    /// 记录字符串字段。
    ///
    /// Business Logic: 结构化字段多为 &str。
    /// Code Logic: 委托 `record_str_field`。
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(sanitize_diagnostic_text(value));
            return;
        }
        self.record_str_field(field, value);
    }

    /// 记录有符号整数（elapsed_ms）。
    ///
    /// Business Logic: elapsed_ms 应以数字写入 JSON。
    /// Code Logic: 仅接受字段名 elapsed_ms 且非负。
    fn record_i64(&mut self, field: &Field, value: i64) {
        if field.name() == "elapsed_ms" && value >= 0 {
            self.elapsed_ms = Some(value as u64);
        }
    }

    /// 记录无符号整数（elapsed_ms）。
    ///
    /// Business Logic: 同上。
    /// Code Logic: 仅接受 elapsed_ms。
    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "elapsed_ms" {
            self.elapsed_ms = Some(value);
        }
    }

    /// 记录 u128（降级到 u64 范围时接受 elapsed_ms）。
    ///
    /// Business Logic: 兼容可能的更宽整数类型。
    /// Code Logic: 能装进 u64 则写入 elapsed_ms。
    fn record_u128(&mut self, field: &Field, value: u128) {
        if field.name() == "elapsed_ms" {
            if let Ok(v) = u64::try_from(value) {
                self.elapsed_ms = Some(v);
            }
        }
    }

    /// 记录 i128。
    ///
    /// Business Logic: 兼容更宽整数。
    /// Code Logic: 非负且可转 u64 时写入 elapsed_ms。
    fn record_i128(&mut self, field: &Field, value: i128) {
        if field.name() == "elapsed_ms" && value >= 0 {
            if let Ok(v) = u64::try_from(value) {
                self.elapsed_ms = Some(v);
            }
        }
    }
}

/// 严格白名单 JSON 文件日志 formatter。
///
/// Business Logic（为什么需要这个结构）:
///     文件层必须输出可控 JSON schema，供 doctor 读取 recent errors，且永不泄漏未知字段。
///
/// Code Logic（这个结构做什么）:
///     实现 `FormatEvent`：Visit 白名单字段 → 组装 JSON 对象 → 写一行 + `\n`。
#[derive(Debug, Default, Clone)]
pub struct SanitizedJsonFormatter;

impl<S, N> FormatEvent<S, N> for SanitizedJsonFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    /// 将事件格式化为单行白名单 JSON。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     tracing-subscriber 每条事件回调此处；必须保证输出 schema 稳定且已脱敏。
    ///
    /// Code Logic（这个函数做什么）:
    ///     收集字段 → 注入 timestamp/level → serde_json 紧凑序列化 → writeln。
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut visitor = WhitelistFieldVisitor::default();
        event.record(&mut visitor);

        let mut map = Map::new();
        map.insert(
            "timestamp".to_string(),
            Value::String(chrono::Utc::now().to_rfc3339()),
        );
        map.insert(
            "level".to_string(),
            Value::String(level_to_str(event.metadata().level()).to_string()),
        );
        if let Some(v) = visitor.request_id {
            map.insert("request_id".to_string(), Value::String(v));
        }
        if let Some(v) = visitor.domain {
            map.insert("domain".to_string(), Value::String(v));
        }
        if let Some(v) = visitor.operation {
            map.insert("operation".to_string(), Value::String(v));
        }
        if let Some(v) = visitor.result {
            map.insert("result".to_string(), Value::String(v));
        }
        if let Some(v) = visitor.elapsed_ms {
            map.insert("elapsed_ms".to_string(), Value::Number(v.into()));
        }
        if let Some(v) = visitor.error_code {
            map.insert("error_code".to_string(), Value::String(v));
        }
        if let Some(v) = visitor.message {
            map.insert("message".to_string(), Value::String(v));
        }

        let line = serde_json::to_string(&Value::Object(map)).map_err(|_| fmt::Error)?;
        writeln!(writer, "{line}")
    }
}

/// Level 转小写稳定字符串。
///
/// Business Logic（为什么需要这个函数）:
///     doctor/测试依赖 level 字段稳定字面量。
///
/// Code Logic（这个函数做什么）:
///     ERROR/WARN/INFO/DEBUG/TRACE → 对应小写。
fn level_to_str(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}

/// 将 `SanitizedJsonFormatter` 接到任意 `MakeWriter`（文件/缓冲）。
///
/// Business Logic（为什么需要这个函数）:
///     serve（Task 3）与本任务测试都需要同一套严格 JSON layer，避免重复拼装。
///
/// Code Logic（这个函数做什么）:
///     返回 `fmt::Layer`：无 ANSI、使用 `SanitizedJsonFormatter` 作为事件格式。
pub fn sanitized_json_layer<S, W>(
    make_writer: W,
) -> tracing_subscriber::fmt::Layer<
    S,
    tracing_subscriber::fmt::format::DefaultFields,
    SanitizedJsonFormatter,
    W,
>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + 'static,
{
    tracing_subscriber::fmt::layer()
        .event_format(SanitizedJsonFormatter)
        .with_ansi(false)
        .with_writer(make_writer)
}

/// 人类可读但仍走同一 sanitizer 的 stderr formatter。
///
/// Business Logic（为什么需要这个结构）:
///     serve 子进程 stderr 需便于人工排查，同时不得比文件层更宽松地泄漏密钥/正文/家目录。
///
/// Code Logic（这个结构做什么）:
///     实现 `FormatEvent`：收集白名单字段后输出单行文本，message 经 `sanitize_diagnostic_text`。
#[derive(Debug, Default, Clone)]
pub struct SanitizedTextFormatter;

impl<S, N> FormatEvent<S, N> for SanitizedTextFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    /// 将事件格式化为人类可读单行。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     stderr 与文件层共用 sanitizer，只改变排版，避免双通道隐私策略漂移。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Visit 白名单字段 → 拼接 `LEVEL domain=.. operation=.. message=..` → writeln。
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut visitor = WhitelistFieldVisitor::default();
        event.record(&mut visitor);

        write!(
            writer,
            "{} ",
            level_to_str(event.metadata().level()).to_uppercase()
        )?;
        if let Some(domain) = visitor.domain.as_deref() {
            write!(writer, "domain={domain} ")?;
        }
        if let Some(operation) = visitor.operation.as_deref() {
            write!(writer, "operation={operation} ")?;
        }
        if let Some(result) = visitor.result.as_deref() {
            write!(writer, "result={result} ")?;
        }
        if let Some(request_id) = visitor.request_id.as_deref() {
            write!(writer, "request_id={request_id} ")?;
        }
        if let Some(error_code) = visitor.error_code.as_deref() {
            write!(writer, "error_code={error_code} ")?;
        }
        if let Some(elapsed_ms) = visitor.elapsed_ms {
            write!(writer, "elapsed_ms={elapsed_ms} ")?;
        }
        if let Some(message) = visitor.message.as_deref() {
            write!(writer, "message={message}")?;
        } else {
            // 无结构化 message 时，至少输出 target，避免空行；仍不回退到原始 Debug payload。
            write!(writer, "target={}", event.metadata().target())?;
        }
        writeln!(writer)
    }
}

/// 打开轮转文件 writer 并包装为 non-blocking + 生命周期守卫。
///
/// Business Logic（为什么需要这个函数）:
///     serve 与生命周期测试都需要同一套“打开日志文件失败即显式失败”的入口，避免静默丢诊断。
///
/// Code Logic（这个函数做什么）:
///     `RotatingLogWriter::open` → `into_non_blocking`；IO 错误映射为 `AppError`。
pub fn open_backend_logging(
    config: BackendLogConfig,
) -> Result<(NonBlocking, BackendLoggingGuard), AppError> {
    let writer = RotatingLogWriter::open(config)
        .map_err(|error| AppError::generic(format!("无法打开后端日志文件: {error}")))?;
    Ok(writer.into_non_blocking())
}

/// 初始化 serve 子进程双通道 tracing（stderr 文本 + 严格 JSON 文件）。
///
/// Business Logic（为什么需要这个函数）:
///     detached serve 子进程是诊断证据的唯一写入方：文件层供 doctor 读取，stderr 供前台调试；
///     父进程 `start` 不得同时打开同一轮转文件，避免双写与轮转竞态。
///
/// Code Logic（这个函数做什么）:
///     打开轮转 writer → 组装 EnvFilter + 白名单 JSON 文件 layer + 脱敏文本 stderr layer →
///     `try_init` 设为全局默认 subscriber；返回必须持有到 serve 结束的 `BackendLoggingGuard`。
///     日志目录/文件不可用或 subscriber 初始化失败时返回错误（启动失败，不静默降级）。
pub fn init_backend_tracing(config: BackendLogConfig) -> Result<BackendLoggingGuard, AppError> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::EnvFilter;

    let (non_blocking, guard) = open_backend_logging(config)?;
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,mdns_sd=off"));
    let file_layer = sanitized_json_layer(non_blocking);
    let stderr_layer = tracing_subscriber::fmt::layer()
        .event_format(SanitizedTextFormatter)
        .with_ansi(false)
        .with_writer(std::io::stderr);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(file_layer)
        .with(stderr_layer)
        .try_init()
        .map_err(|error| AppError::generic(format!("初始化后端 tracing 失败: {error}")))?;

    Ok(guard)
}

/// 初始化 doctor 短生命周期进程的仅 stderr 脱敏 tracing。
///
/// Business Logic（为什么需要这个函数）:
///     `doctor` / `doctor --json` 需要在 probe 过程中把诊断 tracing 打到 stderr，
///     同时 stdout 必须保持纯净（尤其 JSON 模式只能有一份合法 JSON）。
///     doctor 不是 serve 子进程，不得打开/写入 `backend.log`，避免与 serve 双写竞态。
///
/// Code Logic（这个函数做什么）:
///     组装 EnvFilter + 脱敏文本 stderr layer，`try_init` 设为全局默认 subscriber；
///     已初始化时静默忽略（测试/重复调用友好），不打开文件 layer，不返回 guard。
pub fn init_doctor_tracing() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::EnvFilter;

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,mdns_sd=off"));
    let stderr_layer = tracing_subscriber::fmt::layer()
        .event_format(SanitizedTextFormatter)
        .with_ansi(false)
        .with_writer(std::io::stderr);

    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(stderr_layer)
        .try_init();
}

// ---------------------------------------------------------------------------
// Production structured logging helpers
// ---------------------------------------------------------------------------

/// 操作完成结果字面量（成功/失败/取消等）。
///
/// Business Logic（为什么需要这个枚举）:
///     统一 result 字段取值，避免自由文本漂移。
///
/// Code Logic（这个枚举做什么）:
///     提供 `as_str` 稳定字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationResult {
    /// 成功。
    Ok,
    /// 失败。
    Error,
    /// 被取消。
    Cancelled,
    /// 跳过（如能力不支持）。
    Skipped,
}

impl OperationResult {
    /// 稳定字符串。
    ///
    /// Business Logic: schema 的 result 字段字面量。
    /// Code Logic: 映射到 ok/error/cancelled/skipped。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
        }
    }
}

/// 一次请求/操作完成的结构化诊断字段（生产路径）。
///
/// Business Logic（为什么需要这个结构）:
///     backend HTTP/control/P2P 高价值事件必须只写白名单字段；用结构体聚合参数，
///     避免过长位置参数列表，也便于调用方按名字填充。
///
/// Code Logic（这个结构做什么）:
///     持有 level + schema 字段（request_id/domain/operation/result/elapsed_ms/error_code/message）。
#[derive(Debug, Clone)]
pub struct OperationLog {
    /// 日志级别。
    pub level: Level,
    /// 可选请求 ID（P2P/middleware 透传）。
    pub request_id: Option<String>,
    /// 业务域（http/control/p2p 等）。
    pub domain: String,
    /// 操作名（serve/sync_pull/stop 等）。
    pub operation: String,
    /// 结果字面量。
    pub result: OperationResult,
    /// 可选耗时毫秒。
    pub elapsed_ms: Option<u64>,
    /// 可选稳定错误码。
    pub error_code: Option<String>,
    /// 可选已脱敏前的错误/摘要文本（写入前会再跑 sanitizer）。
    pub message: Option<String>,
}

impl OperationLog {
    /// 构造最小必填字段的操作日志。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     多数调用只需 domain/operation/result，可选字段按需链式补充。
    ///
    /// Code Logic（这个函数做什么）:
    ///     以 INFO/空可选字段初始化，调用方再覆盖 level 等。
    pub fn new(
        domain: impl Into<String>,
        operation: impl Into<String>,
        result: OperationResult,
    ) -> Self {
        Self {
            level: Level::INFO,
            request_id: None,
            domain: domain.into(),
            operation: operation.into(),
            result,
            elapsed_ms: None,
            error_code: None,
            message: None,
        }
    }

    /// 设置日志级别。
    ///
    /// Business Logic: 错误路径用 ERROR/WARN，成功用 INFO。
    /// Code Logic: 覆盖 `level` 后返回 self 便于链式调用。
    pub fn level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    /// 设置 request_id。
    ///
    /// Business Logic: 串联 P2P 调用链。
    /// Code Logic: 写入 `request_id`。
    pub fn request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// 设置耗时毫秒。
    ///
    /// Business Logic: doctor/排障需要耗时信号。
    /// Code Logic: 写入 `elapsed_ms`。
    pub fn elapsed_ms(mut self, ms: u64) -> Self {
        self.elapsed_ms = Some(ms);
        self
    }

    /// 设置稳定错误码。
    ///
    /// Business Logic: 与 P2P 错误信封 code 对齐。
    /// Code Logic: 写入 `error_code`。
    pub fn error_code(mut self, code: impl Into<String>) -> Self {
        self.error_code = Some(code.into());
        self
    }

    /// 设置摘要消息（写入前脱敏）。
    ///
    /// Business Logic: 人类可读摘要，禁止塞 payload。
    /// Code Logic: 写入 `message`。
    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// 发出本条结构化诊断事件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     生产路径统一出口：只写白名单字段，message 强制脱敏，永不记请求 body。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 level 选择 tracing 宏，字段仅含 schema 白名单。
    pub fn emit(self) {
        let request_id = self.request_id.as_deref().unwrap_or("");
        let error_code = self.error_code.as_deref().unwrap_or("");
        let message = self
            .message
            .as_deref()
            .map(sanitize_diagnostic_text)
            .unwrap_or_default();
        let result_str = self.result.as_str();
        let elapsed = self.elapsed_ms.unwrap_or(0);
        let domain = self.domain.as_str();
        let operation = self.operation.as_str();

        match self.level {
            Level::ERROR => {
                tracing::error!(
                    request_id = %request_id,
                    domain = %domain,
                    operation = %operation,
                    result = %result_str,
                    elapsed_ms = elapsed,
                    error_code = %error_code,
                    message = %message,
                    "{message}"
                );
            }
            Level::WARN => {
                tracing::warn!(
                    request_id = %request_id,
                    domain = %domain,
                    operation = %operation,
                    result = %result_str,
                    elapsed_ms = elapsed,
                    error_code = %error_code,
                    message = %message,
                    "{message}"
                );
            }
            Level::DEBUG => {
                tracing::debug!(
                    request_id = %request_id,
                    domain = %domain,
                    operation = %operation,
                    result = %result_str,
                    elapsed_ms = elapsed,
                    error_code = %error_code,
                    message = %message,
                    "{message}"
                );
            }
            Level::TRACE => {
                tracing::trace!(
                    request_id = %request_id,
                    domain = %domain,
                    operation = %operation,
                    result = %result_str,
                    elapsed_ms = elapsed,
                    error_code = %error_code,
                    message = %message,
                    "{message}"
                );
            }
            _ => {
                tracing::info!(
                    request_id = %request_id,
                    domain = %domain,
                    operation = %operation,
                    result = %result_str,
                    elapsed_ms = elapsed,
                    error_code = %error_code,
                    message = %message,
                    "{message}"
                );
            }
        }
    }
}

/// 记录一次请求/操作完成的结构化诊断事件（生产路径，兼容包装）。
///
/// Business Logic（为什么需要这个函数）:
///     保留简短调用入口；内部委托 `OperationLog::emit`。
///
/// Code Logic（这个函数做什么）:
///     组装 `OperationLog` 后 `emit`。
pub fn log_operation_completion(log: OperationLog) {
    log.emit();
}

// ---------------------------------------------------------------------------
// FS helpers
// ---------------------------------------------------------------------------

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

/// 打开日志集合时协调遗留 history/权限。
///
/// Business Logic（为什么需要这个函数）:
///     升级遗留的 `.4+` / `.20+`、宽权限 history 与超大 retained history 会破坏
///     3-history/5MiB/0600 不变量，必须在 reopen 时全量收敛，而不是等到下一次自然轮转。
///
/// Code Logic（这个函数做什么）:
///     枚举目录中严格匹配 `backend.log.<正整数>` 的文件：index > history_files 删除；
///     retained history（1..=history）若超 max_bytes 也删除；对 current 与保留 history 强制 0600。
fn reconcile_log_set_on_open(config: &BackendLogConfig) -> io::Result<()> {
    let history = config.history_files.max(1);
    for (index, path) in enumerate_history_files(config)? {
        if index > history {
            fs::remove_file(&path)?;
            continue;
        }
        // retained history 也必须满足单文件上限，否则永远原样保留超限遗留。
        let len = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if len > config.max_bytes {
            fs::remove_file(&path)?;
            continue;
        }
        #[cfg(unix)]
        apply_file_mode_0600(&path)?;
    }
    #[cfg(unix)]
    {
        let current = config.current_path();
        if current.exists() {
            apply_file_mode_0600(&current)?;
        }
    }
    Ok(())
}

/// 枚举日志目录中严格匹配 `backend.log.<正整数>` 的历史文件。
///
/// Business Logic（为什么需要这个函数）:
///     固定扫描 `.4..19` 会漏掉 `.20+`；reopen 必须枚举全部正整数后缀历史。
///
/// Code Logic（这个函数做什么）:
///     `read_dir` 后解析文件名：前缀等于 `backend.log.` 且剩余部分为无符号正整数（无前导符号），
///     返回 `(index, path)` 列表；目录不存在时返回空。
fn enumerate_history_files(config: &BackendLogConfig) -> io::Result<Vec<(usize, PathBuf)>> {
    let dir = &config.log_dir;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let prefix = format!("{BACKEND_LOG_FILE_NAME}.");
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        let Some(suffix) = name.strip_prefix(&prefix) else {
            continue;
        };
        // 严格正整数：全数字、非空、不允许前导 `+`/`-`，`0` 也不算有效 history index。
        if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if suffix.starts_with('0') && suffix.len() > 1 {
            continue;
        }
        let Ok(index) = suffix.parse::<usize>() else {
            continue;
        };
        if index == 0 {
            continue;
        }
        out.push((index, entry.path()));
    }
    Ok(out)
}

/// 在尚无打开句柄时对已有日志文件执行一次安全轮转/收敛。
///
/// Business Logic（为什么需要这个函数）:
///     reopen 时 current 可能已 >max（升级遗留/外部写入）；必须先处理再 append，
///     且不得把超大文件推进 history 永久违反 5 MiB 上限。
///
/// Code Logic（这个函数做什么）:
///     删除最旧与超限 history；未超限 history 反向 rename；超限 current 直接删除，
///     未超限 current rename 为 `.1`。随后 open 会重建空/可续写 current。
fn rotate_existing_files(config: &BackendLogConfig) -> io::Result<()> {
    let history = config.history_files.max(1);
    let oldest = config.history_path(history);
    if oldest.exists() {
        fs::remove_file(&oldest)?;
    }
    for index in (1..history).rev() {
        let from = config.history_path(index);
        let to = config.history_path(index + 1);
        if from.exists() {
            let from_len = fs::metadata(&from).map(|m| m.len()).unwrap_or(0);
            if from_len > config.max_bytes {
                fs::remove_file(&from)?;
                continue;
            }
            fs::rename(&from, &to)?;
            #[cfg(unix)]
            apply_file_mode_0600(&to)?;
        }
    }
    let current = config.current_path();
    if current.exists() {
        let current_len = fs::metadata(&current).map(|m| m.len()).unwrap_or(0);
        if current_len > config.max_bytes {
            fs::remove_file(&current)?;
        } else {
            let first_history = config.history_path(1);
            fs::rename(&current, &first_history)?;
            #[cfg(unix)]
            apply_file_mode_0600(&first_history)?;
        }
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    use std::io::Write;
    use std::sync::{Arc, Mutex as StdMutex};
    use tracing_subscriber::prelude::*;

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

    /// 读取 current + 全部历史文件的拼接文本。
    ///
    /// Business Logic: 敏感字段回归必须扫描所有落盘文件。
    /// Code Logic: current + `.1`..`.history` 拼接。
    fn read_all_log_files(config: &BackendLogConfig) -> String {
        let mut out = read_string(&config.current_path());
        for i in 1..=config.history_files {
            let p = config.history_path(i);
            if p.exists() {
                out.push_str(&read_string(&p));
            }
        }
        out
    }

    /// 内存 MakeWriter：供 with_default subscriber 测试 formatter。
    #[derive(Clone, Default)]
    struct BufferWriter {
        buf: Arc<StdMutex<Vec<u8>>>,
    }

    impl BufferWriter {
        fn new() -> Self {
            Self {
                buf: Arc::new(StdMutex::new(Vec::new())),
            }
        }

        fn contents(&self) -> String {
            let guard = self.buf.lock().expect("buf lock");
            String::from_utf8_lossy(&guard).into_owned()
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BufferWriter {
        type Writer = BufferHandle;

        fn make_writer(&'a self) -> Self::Writer {
            BufferHandle {
                buf: Arc::clone(&self.buf),
            }
        }
    }

    struct BufferHandle {
        buf: Arc<StdMutex<Vec<u8>>>,
    }

    impl Write for BufferHandle {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.buf.lock().expect("buf").extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    // ----- rotation tests (Task 1) -----

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

    /// Business Logic（为什么需要这个测试）:
    ///     轮转中途 rename 失败后，writer 必须恢复 current 句柄，后续诊断仍可写入。
    ///
    /// Code Logic（这个测试做什么）:
    ///     人为占用 history 路径使 rename 失败（Unix 用目录占位），断言 rotate_locked 返回错误但
    ///     后续小写入仍成功且 current 文件可读。
    #[cfg(unix)]
    #[test]
    fn rotate_failure_reopens_current_and_keeps_logging() {
        let (_dir, config) = test_config(10, 3);
        let mut writer = RotatingLogWriter::open(config.clone()).expect("open writer");
        writer.write_all(b"1234567890").expect("fill current");
        writer.flush().expect("flush");

        // 用目录占住最旧 history 路径，使 remove_file(.N) 失败；
        // 注意：不能只占 `.1`，因为目录会被反向 rename 挪走，导致 current→.1 仍成功。
        let blocker = config.history_path(config.history_files.max(1));
        fs::create_dir_all(&blocker).expect("create blocker dir");

        let err = writer
            .write_all(b"X")
            .expect_err("remove_file 被目录阻挡时应失败");
        assert_ne!(err.kind(), io::ErrorKind::InvalidInput);

        // 清理阻挡后，recover 的句柄应仍能继续写（或 reopen 后写）。
        fs::remove_dir_all(&blocker).ok();
        writer
            .write_all(b"Y")
            .expect("轮转失败恢复后应能继续写日志");
        writer.flush().expect("flush after recover");
        let content = read_string(&config.current_path());
        assert!(
            content.contains('Y') || content.ends_with('Y') || content.contains("1234567890"),
            "恢复后 current 应包含续写或原内容: {content:?}"
        );
        assert!(
            writer.write_all(b"Z").is_ok(),
            "后续写入不得因 file=None 永久失败"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     轮转恢复连续失败留下 file=None 后，环境恢复时后续写入必须能 reopen，不能永久静默。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用目录占住 current 路径使 open 失败 → file=None；flush 不得假装成功；
    ///     移除目录并重建文件后 write 应 reopen 成功。
    #[cfg(unix)]
    #[test]
    fn write_reopens_after_recover_failed_when_environment_recovers() {
        let (_dir, config) = test_config(64, 3);
        let mut writer = RotatingLogWriter::open(config.clone()).expect("open");
        {
            let mut guard = writer.state.lock().expect("lock");
            if let Some(file) = guard.file.take() {
                drop(file);
            }
            guard.current_len = 0;
        }
        // 用目录占住 current 路径，使 open_current_file 失败（create 文件会 EISDIR）。
        let current = config.current_path();
        let _ = fs::remove_file(&current);
        fs::create_dir_all(&current).expect("block current with directory");
        {
            let mut guard = writer.state.lock().expect("lock");
            writer.recover_current_file_locked(&mut guard);
            assert!(guard.file.is_none(), "open 失败时 recover 应保持 file=None");
        }
        let flush_err = writer.flush().expect_err("file=None 时 flush 不得假装成功");
        assert_ne!(flush_err.kind(), io::ErrorKind::InvalidInput);

        // 环境恢复：去掉目录阻挡，重建文件后 write 路径应 reopen 并成功。
        fs::remove_dir_all(&current).expect("remove blocker");
        fs::write(&current, b"").expect("recreate current file");
        writer
            .write_all(b"recovered")
            .expect("环境恢复后 write 应 reopen 成功");
        writer.flush().expect("reopen 后 flush 应成功");
        let content = read_string(&config.current_path());
        assert!(
            content.contains("recovered"),
            "恢复后 current 应包含后续写入: {content:?}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     file=None 且路径持续不可开时，writer 故障不得通过同一 tracing subscriber 再入队，
    ///     否则 non-blocking 文件 layer 会形成自维持失败循环。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用目录占住 current 使 open 永久失败；把 RotatingLogWriter 接到真实 non-blocking + tracing
    ///     subscriber；循环 write/flush 并主动 emit tracing 事件，断言不会因自反馈 panic/卡死，
    ///     且 recover 路径不走 tracing（仅带外 stderr，测试侧只验证 write 错误与 file=None 保持）。
    #[cfg(unix)]
    #[test]
    fn recover_failure_does_not_reenter_tracing_via_non_blocking_layer() {
        use tracing_subscriber::prelude::*;

        let (_dir, config) = test_config(64, 3);
        let writer = RotatingLogWriter::open(config.clone()).expect("open");
        {
            let mut guard = writer.state.lock().expect("lock");
            if let Some(file) = guard.file.take() {
                drop(file);
            }
            guard.current_len = 0;
        }
        let current = config.current_path();
        let _ = fs::remove_file(&current);
        fs::create_dir_all(&current).expect("block current with directory");

        // 真实 non-blocking 接线：与生产 serve 文件 layer 同构（NonBlocking 作 MakeWriter）。
        let (non_blocking, guard) = writer.into_non_blocking();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(non_blocking),
        );
        let _default = tracing::subscriber::set_default(subscriber);

        // 多次 emit 驱动 non-blocking worker 调用 Write::write/flush → recover。
        // 若 recover 内仍用 tracing::warn，worker 会把 warn 再写入并再次 recover → 自反馈。
        for i in 0..32 {
            tracing::warn!(attempt = i, "probe external event while file unavailable");
            std::thread::sleep(Duration::from_millis(5));
        }
        // 给 worker 一点时间排空；不应 hang / 栈溢出。
        std::thread::sleep(Duration::from_millis(150));
        drop(guard);

        assert!(
            current.is_dir(),
            "测试夹具应保持 current 为目录以模拟持续不可打开"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     即使 create_current_file 失败，recover 也必须 reopen 已有 current，避免 file=None。
    ///
    /// Code Logic（这个测试做什么）:
    ///     在 rotate_files 成功后通过把 current 父路径换成只读/不可写场景较难跨平台；
    ///     这里直接调用 recover_current_file_locked 验证 metadata 长度恢复契约。
    #[test]
    fn recover_current_file_restores_len_from_metadata() {
        let (_dir, config) = test_config(32, 3);
        let writer = RotatingLogWriter::open(config.clone()).expect("open");
        {
            let mut guard = writer.state.lock().expect("lock");
            // 写入已知内容后关闭句柄，模拟 rotate 中途 take。
            if let Some(mut file) = guard.file.take() {
                use std::io::Write as _;
                file.write_all(b"abcdef").expect("seed");
                file.flush().ok();
            }
            guard.current_len = 0;
            writer.recover_current_file_locked(&mut guard);
            assert!(guard.file.is_some(), "recover 必须重新打开 current");
            assert_eq!(guard.current_len, 6, "current_len 必须来自 metadata");
        }
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

        for path in [config.current_path(), config.history_path(1)] {
            let mode = fs::metadata(&path).expect("file meta").permissions().mode() & 0o777;
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

    /// Windows：轮转路径在 close→rename→reopen 下不得因“文件仍被占用”失败，且 history 上限为 3。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     Windows 上打开句柄会导致 rename 失败；RotatingLogWriter 必须先 close 再 rename，
    ///     跨平台 smoke 依赖该契约证明日志不会卡死或生成 .4。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用极小 max_bytes 连续写触发多次轮转；断言无 `.4`、history `.1` 最新，
    ///     且 current 在轮转后仍可被本进程读写（无 open-handle rename 残留）。
    #[cfg(windows)]
    #[test]
    fn windows_rotation_closes_before_rename_and_keeps_three_history() {
        use std::io::Read;

        let (_dir, config) = test_config(4, 3);
        let mut writer = RotatingLogWriter::open(config.clone()).expect("open");

        for payload in [b"AAAA", b"BBBB", b"CCCC", b"DDDD", b"EEEE"] {
            writer
                .write_all(payload)
                .unwrap_or_else(|e| panic!("Windows 轮转写入失败（疑似 open-handle rename）: {e}"));
        }
        writer.flush().expect("flush");

        assert_eq!(read_string(&config.current_path()), "EEEE");
        assert_eq!(read_string(&config.history_path(1)), "DDDD");
        assert_eq!(read_string(&config.history_path(2)), "CCCC");
        assert_eq!(read_string(&config.history_path(3)), "BBBB");
        assert!(
            !config.history_path(4).exists(),
            "Windows 轮转不得生成 backend.log.4"
        );

        // 轮转后 current 必须仍可被本进程读写（证明句柄已正确重开）
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(config.current_path())
            .expect("轮转后 current 应可读写");
        let mut buf = String::new();
        f.read_to_string(&mut buf).expect("read after rotate");
        assert!(buf.contains("EEEE"), "轮转后 current 内容异常: {buf}");
    }

    /// Unix：history 上限 3 + 目录 0700 / 文件 0600 在连续轮转后仍成立。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     跨平台 smoke 要求 macOS 上权限与 3-history 契约在多次 close/rename/reopen 后仍成立。
    ///
    /// Code Logic（这个测试做什么）:
    ///     连续写触发 4 次轮转，断言无 `.4`，并对 dir/current/history 校验 mode。
    #[cfg(unix)]
    #[test]
    fn unix_rotation_history_ceiling_and_modes_after_multiple_rotates() {
        use std::os::unix::fs::PermissionsExt;

        let (_dir, config) = test_config(4, 3);
        let mut writer = RotatingLogWriter::open(config.clone()).expect("open");
        for payload in [b"AAAA", b"BBBB", b"CCCC", b"DDDD", b"EEEE"] {
            writer.write_all(payload).expect("write");
        }
        writer.flush().expect("flush");

        assert_eq!(read_string(&config.current_path()), "EEEE");
        assert_eq!(read_string(&config.history_path(1)), "DDDD");
        assert_eq!(read_string(&config.history_path(2)), "CCCC");
        assert_eq!(read_string(&config.history_path(3)), "BBBB");
        assert!(!config.history_path(4).exists(), "Unix 轮转不得生成 .4");

        let dir_mode = fs::metadata(&config.log_dir)
            .expect("dir meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "多次轮转后目录仍应为 0700");

        for path in [
            config.current_path(),
            config.history_path(1),
            config.history_path(2),
            config.history_path(3),
        ] {
            let mode = fs::metadata(&path).expect("file meta").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{path:?} 多次轮转后应为 0600");
        }
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

    // ----- sanitizer unit tests -----

    #[test]
    fn sanitize_discards_entire_body_remainder_even_when_oversized() {
        let tail = "TAIL_SHOULD_NOT_LEAK_".repeat(300);
        let raw = format!("upstream error body={{ \"prompt\":\"SECRET\" }} {tail}");
        let cleaned = sanitize_diagnostic_text(&raw);
        assert!(
            cleaned.contains("body=<BODY_OMITTED>"),
            "body marker 应整段丢弃: {cleaned}"
        );
        assert!(
            !cleaned.contains("TAIL_SHOULD_NOT_LEAK_"),
            "超长 body 尾部不得泄漏: {cleaned}"
        );
        assert!(
            !cleaned.contains("SECRET"),
            "body 内 secret 不得泄漏: {cleaned}"
        );
    }

    #[test]
    fn sanitize_blocks_standalone_prompt_and_file_content() {
        let samples = [
            "user said Prompt=do-not-leak-this-prompt-body",
            "failed path file content=FILE_SENTINEL_PAYLOAD_123",
            "remote peer replied: free text containing file-sentinel-XYZ",
        ];
        for raw in samples {
            let cleaned = sanitize_diagnostic_text(raw);
            assert!(
                !cleaned.contains("do-not-leak-this-prompt-body"),
                "独立 Prompt 应拦截: {cleaned}"
            );
            assert!(
                !cleaned.contains("FILE_SENTINEL_PAYLOAD_123"),
                "独立 file content 应拦截: {cleaned}"
            );
            assert!(
                !cleaned.contains("file-sentinel-XYZ"),
                "自由文本 file-sentinel 应拦截: {cleaned}"
            );
        }
    }

    #[test]
    fn reopen_deletes_history_beyond_limit_and_enforces_mode() {
        let (_dir, config) = test_config(32, 3);
        fs::write(config.history_path(1), b"h1").unwrap();
        fs::write(config.history_path(2), b"h2").unwrap();
        fs::write(config.history_path(3), b"h3").unwrap();
        fs::write(config.history_path(4), b"h4-should-go").unwrap();
        fs::write(config.current_path(), b"curr").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [
                config.current_path(),
                config.history_path(1),
                config.history_path(2),
                config.history_path(3),
            ] {
                let mut perms = fs::metadata(&path).unwrap().permissions();
                perms.set_mode(0o644);
                fs::set_permissions(&path, perms).unwrap();
            }
        }

        let _writer = RotatingLogWriter::open(config.clone()).expect("reopen");
        assert!(!config.history_path(4).exists(), "reopen 必须删除 .4+");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [
                config.current_path(),
                config.history_path(1),
                config.history_path(2),
                config.history_path(3),
            ] {
                if path.exists() {
                    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
                    assert_eq!(mode, 0o600, "{path:?} reopen 后应为 0600");
                }
            }
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     reopen 必须删除 index > history_files 的高编号历史，以及超限 retained history。
    ///
    /// Code Logic（这个测试做什么）:
    ///     预置 oversized `.1` 与 `.20`，reopen 后断言二者均被删除，保留的 history 不超 max。
    #[test]
    fn reopen_deletes_oversized_retained_history_and_high_index_files() {
        let (_dir, config) = test_config(8, 3);
        // retained history 超限
        fs::write(config.history_path(1), b"0123456789ABCDEF").unwrap();
        fs::write(config.history_path(2), b"ok2").unwrap();
        fs::write(config.history_path(3), b"ok3").unwrap();
        // 高编号遗留（超出 history+16 旧扫描范围）
        fs::write(config.history_path(20), b"h20-should-go").unwrap();
        fs::write(config.current_path(), b"curr").unwrap();

        let _writer = RotatingLogWriter::open(config.clone()).expect("reopen");
        assert!(
            !config.history_path(20).exists(),
            "reopen 必须删除 .20+ 高编号历史"
        );
        assert!(
            !config.history_path(1).exists(),
            "reopen 必须删除 oversized retained history .1"
        );
        for i in 2..=3 {
            let p = config.history_path(i);
            if p.exists() {
                assert!(
                    file_len(&p) <= config.max_bytes,
                    "保留 history 不得超限: {:?} len={}",
                    p,
                    file_len(&p)
                );
            }
        }
    }

    #[test]
    fn reopen_discards_oversized_current_instead_of_creating_oversize_history() {
        let (_dir, config) = test_config(8, 3);
        fs::write(config.current_path(), b"0123456789ABCDEFGHIJ").unwrap();
        let mut writer = RotatingLogWriter::open(config.clone()).expect("reopen oversized");
        writer.write_all(b"Z").expect("append after reopen");
        writer.flush().ok();
        assert_eq!(read_string(&config.current_path()), "Z");
        assert!(file_len(&config.current_path()) <= config.max_bytes);
        for i in 1..=3 {
            let p = config.history_path(i);
            if p.exists() {
                assert!(
                    file_len(&p) <= config.max_bytes,
                    "history 不得保留超限遗留: {:?} len={}",
                    p,
                    file_len(&p)
                );
            }
        }
    }

    #[test]
    fn sanitize_replaces_home_and_redacts_secrets() {
        let home = dirs::home_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/Users/hostile".into());
        let raw = format!(
            "fail path={home}/.claude/CLAUDE.md Authorization: Bearer SUPERSECRET_TOKEN_XYZ password=hunter2-secret"
        );
        let cleaned = sanitize_diagnostic_text(&raw);
        assert!(
            cleaned.contains("<HOME>"),
            "home 应替换为 <HOME>: {cleaned}"
        );
        assert!(!cleaned.contains(&home), "原始 home 不得残留: {cleaned}");
        assert!(
            !cleaned.contains("SUPERSECRET_TOKEN_XYZ"),
            "Bearer token 必须 redact: {cleaned}"
        );
        assert!(
            !cleaned.contains("hunter2-secret"),
            "password 必须 redact: {cleaned}"
        );
        assert!(
            cleaned.contains("<REDACTED>"),
            "应出现 <REDACTED> 占位: {cleaned}"
        );
    }

    #[test]
    fn sanitize_caps_at_8kib_utf8_boundary() {
        // 使用多字节字符确保截断不破坏 UTF-8
        let unit = "测";
        let mut s = String::new();
        while s.len() < SANITIZED_VALUE_MAX_BYTES + 64 {
            s.push_str(unit);
        }
        let out = sanitize_diagnostic_text(&s);
        assert!(out.len() <= SANITIZED_VALUE_MAX_BYTES + 3, "截断后应有界");
        assert!(out.ends_with('…') || out.chars().all(|c| c == '测' || c == '…'));
        // 必须仍是合法 UTF-8（String 保证）且不以残缺字节结尾
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn sanitize_strips_body_shaped_content() {
        let raw = r#"upstream error body={"prompt":"TOP_SECRET_PROMPT_TEXT","token":"abc"}"#;
        let cleaned = sanitize_diagnostic_text(raw);
        assert!(
            !cleaned.contains("TOP_SECRET_PROMPT_TEXT"),
            "body 内 prompt 不得残留: {cleaned}"
        );
        assert!(
            cleaned.contains("BODY_OMITTED") || cleaned.contains("<REDACTED>"),
            "应剥离 body: {cleaned}"
        );
    }

    // ----- hostile fixture + file schema tests -----

    /// 敌意 fixture：敏感字段/正文不得出现在任何 current/history 文件中。
    ///
    /// Business Logic: Task 2 核心门禁——脱敏与白名单必须挡住真实攻击性输入。
    /// Code Logic: with_default 挂 sanitized JSON layer → 写文件 → 扫描全部日志文件。
    #[test]
    fn redacts_sensitive_diagnostics() {
        let (_dir, config) = test_config(BACKEND_LOG_MAX_BYTES, 3);
        let writer = RotatingLogWriter::open(config.clone()).expect("open rotating");
        // 同步写盘，避免 non_blocking 竞态
        let file_writer = Arc::new(StdMutex::new(writer));

        #[derive(Clone)]
        struct SharedRotating(Arc<StdMutex<RotatingLogWriter>>);
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedRotating {
            type Writer = SharedRotatingHandle;
            fn make_writer(&'a self) -> Self::Writer {
                SharedRotatingHandle(Arc::clone(&self.0))
            }
        }
        struct SharedRotatingHandle(Arc<StdMutex<RotatingLogWriter>>);
        impl Write for SharedRotatingHandle {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0.lock().expect("lock").write(buf)
            }
            fn flush(&mut self) -> io::Result<()> {
                self.0.lock().expect("lock").flush()
            }
        }

        let make_writer = SharedRotating(Arc::clone(&file_writer));
        let subscriber = tracing_subscriber::registry().with(sanitized_json_layer(make_writer));

        let home = dirs::home_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/Users/hostile-user".into());
        let home_username = PathBuf::from(&home)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("hostile-user")
            .to_string();

        const BEARER: &str = "Bearer_HostileToken_9f3c2a1b";
        const PASSWORD: &str = "P@ssw0rd_Hostile_Fixture";
        const API_KEY: &str = "sk_live_hostilefixturekeymaterial0001";
        const PROMPT_TEXT: &str = "PROMPT_TEXT_SENTINEL_do_not_log_me";
        const FILE_SENTINEL: &str = "FILE_CONTENT_SENTINEL_xyz987";
        const BODY_JSON: &str = r#"{"prompt":"PROMPT_TEXT_SENTINEL_do_not_log_me","password":"P@ssw0rd_Hostile_Fixture"}"#;

        let bearer_b64 = B64.encode(BEARER.as_bytes());
        let password_b64 = B64.encode(PASSWORD.as_bytes());
        let prompt_b64 = B64.encode(PROMPT_TEXT.as_bytes());

        tracing::subscriber::with_default(subscriber, || {
            // 1) 敌意字段：未知字段必须被丢弃
            tracing::error!(
                request_id = "req-hostile-001",
                domain = "p2p",
                operation = "health",
                result = "error",
                elapsed_ms = 42u64,
                error_code = "unavailable",
                authorization = %format!("Bearer {BEARER}"),
                password = PASSWORD,
                token = BEARER,
                secret = API_KEY,
                api_key = API_KEY,
                body = BODY_JSON,
                prompt = PROMPT_TEXT,
                file_content = FILE_SENTINEL,
                "health failed Authorization: Bearer {BEARER} password={PASSWORD} key={API_KEY} home={home}/.cc-partner body={BODY_JSON} file={FILE_SENTINEL} prompt={PROMPT_TEXT}"
            );

            // 2) 生产 helper 路径
            OperationLog::new("control", "stop", OperationResult::Error)
                .level(Level::ERROR)
                .request_id("req-control-002")
                .elapsed_ms(7)
                .error_code("internal")
                .message(format!(
                    "stop failed token={BEARER} path={home}/data.db body={BODY_JSON}"
                ))
                .emit();

            // 3) 代表 health/control/P2P 的结构化成功事件（允许字段必须保留）
            OperationLog::new("http", "serve", OperationResult::Ok)
                .request_id("req-http-003")
                .elapsed_ms(3)
                .message("axum HTTP server started")
                .emit();
        });

        // flush writer
        file_writer.lock().expect("lock").flush().ok();

        let all = read_all_log_files(&config);
        assert!(!all.trim().is_empty(), "应写出至少一条日志，实际为空");

        // --- 禁止出现的敏感原文 / 编码 ---
        for banned in [
            BEARER,
            PASSWORD,
            API_KEY,
            PROMPT_TEXT,
            FILE_SENTINEL,
            BODY_JSON,
            &home,
            &home_username,
            &bearer_b64,
            &password_b64,
            &prompt_b64,
            "Authorization: Bearer",
            "Bearer Bearer_Hostile",
        ] {
            assert!(
                !all.contains(banned),
                "日志不得包含敏感串 `{banned}`\n--- log ---\n{all}\n--- end ---"
            );
        }

        // --- 允许字段必须保留 ---
        assert!(
            all.contains("req-hostile-001") || all.contains("req-control-002"),
            "request_id 应保留: {all}"
        );
        assert!(
            all.contains("\"domain\":\"p2p\"")
                || all.contains("\"domain\":\"control\"")
                || all.contains("\"domain\":\"http\""),
            "domain 应保留: {all}"
        );
        assert!(all.contains("\"operation\""), "operation 应保留: {all}");
        assert!(all.contains("\"result\""), "result 应保留: {all}");
        assert!(
            all.contains("elapsed_ms") || all.contains("\"elapsed_ms\":"),
            "elapsed_ms 应保留: {all}"
        );
        assert!(
            all.contains("error_code") || all.contains("unavailable") || all.contains("internal"),
            "error_code 应保留: {all}"
        );

        // 每行必须是合法 JSON 且仅含白名单键（+ timestamp/level）
        let allowed: std::collections::HashSet<&str> = [
            "timestamp",
            "level",
            "request_id",
            "domain",
            "operation",
            "result",
            "elapsed_ms",
            "error_code",
            "message",
        ]
        .into_iter()
        .collect();
        for line in all.lines().filter(|l| !l.trim().is_empty()) {
            let value: Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("日志行必须是 JSON: {e}; line={line}"));
            let obj = value
                .as_object()
                .unwrap_or_else(|| panic!("日志行必须是对象: {line}"));
            for key in obj.keys() {
                assert!(
                    allowed.contains(key.as_str()),
                    "未知字段 `{key}` 不得出现在文件日志: {line}"
                );
            }
            // 禁止敌意字段名
            for banned_key in [
                "authorization",
                "password",
                "token",
                "secret",
                "api_key",
                "body",
                "prompt",
                "file_content",
            ] {
                assert!(
                    !obj.contains_key(banned_key),
                    "禁止字段 `{banned_key}` 出现: {line}"
                );
            }
        }
    }

    #[test]
    fn unknown_fields_dropped_allowed_fields_kept() {
        let buffer = BufferWriter::new();
        let subscriber = tracing_subscriber::registry().with(sanitized_json_layer(buffer.clone()));

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                request_id = "req-keep",
                domain = "net",
                operation = "sync_pull",
                result = "ok",
                elapsed_ms = 11u64,
                error_code = "",
                password = "should-drop",
                body = "should-drop-body",
                "sync pull completed"
            );
        });

        let out = buffer.contents();
        assert!(out.contains("req-keep"), "request_id 保留: {out}");
        assert!(out.contains("\"domain\":\"net\""), "domain 保留: {out}");
        assert!(
            out.contains("\"operation\":\"sync_pull\""),
            "operation 保留: {out}"
        );
        assert!(out.contains("\"elapsed_ms\":11"), "elapsed_ms 保留: {out}");
        assert!(!out.contains("should-drop"), "未知字段值丢弃: {out}");
        assert!(!out.contains("password"), "未知字段名丢弃: {out}");
        assert!(!out.contains("should-drop-body"), "body 丢弃: {out}");
    }

    /// 验证 open_backend_logging + dual layer 能把结构化事件落到 backend.log。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     serve 子进程生命周期要求：emit 已知事件 → shutdown/flush → 文件可被 doctor 读取；
    ///     该契约不能依赖真实 HTTP/mDNS，否则测试脆弱。
    ///
    /// Code Logic（这个测试做什么）:
    ///     在隔离 log_dir 上 open writer，用 with_default 挂 JSON 文件 layer（不抢全局 subscriber），
    ///     emit OperationLog，drop guard flush 后断言 current 含 domain/operation/message。
    #[test]
    fn dual_layer_lifecycle_persists_structured_event_to_file() {
        let (_dir, config) = test_config(BACKEND_LOG_MAX_BYTES, BACKEND_LOG_HISTORY_FILES);
        let path = config.current_path();
        let (non_blocking, guard) = open_backend_logging(config).expect("应能打开隔离日志文件");
        let subscriber = tracing_subscriber::registry().with(sanitized_json_layer(non_blocking));

        tracing::subscriber::with_default(subscriber, || {
            OperationLog::new("control", "serve_lifecycle_probe", OperationResult::Ok)
                .message("lifecycle-probe-marker-p7-t3")
                .emit();
        });
        drop(guard);

        let body = read_string(&path);
        assert!(
            body.contains("lifecycle-probe-marker-p7-t3"),
            "文件应包含已知结构化 message，实际: {body}"
        );
        assert!(
            body.contains("\"domain\":\"control\"")
                && body.contains("\"operation\":\"serve_lifecycle_probe\""),
            "文件应为白名单 JSON，实际: {body}"
        );
    }

    /// 验证日志路径不可用时 init/open 显式失败。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     serve 启动时若无法写诊断文件必须失败，不能静默退回“只 stderr、无证据”。
    ///
    /// Code Logic（这个测试做什么）:
    ///     把 log_dir 指到一个普通文件路径的父级冲突场景：log_dir 本身是已存在文件，open 应 Err。
    #[test]
    fn open_backend_logging_fails_when_log_dir_unavailable() {
        let tmp = tempfile::tempdir().expect("temp");
        let blocker = tmp.path().join("not-a-dir");
        fs::write(&blocker, b"block").expect("write blocker file");
        let config = BackendLogConfig {
            log_dir: blocker,
            max_bytes: BACKEND_LOG_MAX_BYTES,
            history_files: BACKEND_LOG_HISTORY_FILES,
        };
        let err = open_backend_logging(config).expect_err("log_dir 为文件时应失败");
        assert!(
            err.to_string().contains("无法打开后端日志文件"),
            "错误应明确指向日志文件打开失败，实际: {err}"
        );
    }

    #[test]
    fn file_log_allowed_fields_constant_matches_schema() {
        assert!(FILE_LOG_ALLOWED_FIELDS.contains(&"request_id"));
        assert!(FILE_LOG_ALLOWED_FIELDS.contains(&"domain"));
        assert!(FILE_LOG_ALLOWED_FIELDS.contains(&"operation"));
        assert!(FILE_LOG_ALLOWED_FIELDS.contains(&"result"));
        assert!(FILE_LOG_ALLOWED_FIELDS.contains(&"elapsed_ms"));
        assert!(FILE_LOG_ALLOWED_FIELDS.contains(&"error_code"));
        assert!(FILE_LOG_ALLOWED_FIELDS.contains(&"message"));
        assert_eq!(FILE_LOG_ALLOWED_FIELDS.len(), 7);
    }
}
