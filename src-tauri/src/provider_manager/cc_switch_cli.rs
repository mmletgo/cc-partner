//! provider_manager/cc_switch_cli.rs — cc-switch CLI 检测、调用与安装。
//!
//! Business Logic（为什么需要这个模块）:
//!     切换 provider 会改写 `~/.claude/settings.json`、`~/.codex/auth.json` 等"活配置"，
//!     逻辑（common-config 合并、official-auth 保留、原子写）复杂且易出错。我们不自行写盘，
//!     而是委托给 cc-switch CLI（与 GUI 共享同一服务层）执行切换。
//!
//!     关键约束：在 Linux/Windows 上 cc-switch CLI 与 GUI 二进制同名 `cc-switch`，因此
//!     **绝不能**按名字判定 PATH 上的 `cc-switch` 就是 CLI；必须按"行为"判定（--help 含
//!     CLI-only 子命令 + --version 退出 0），且只调用解析到的绝对路径。
//!
//! Code Logic（这个模块做什么）:
//!     - `detect()`：在常见安装目录与 PATH 中找候选，逐个做行为探测，命中即缓存绝对路径。
//!     - `version()`：`cc-switch --version`。
//!     - `run_switch()`：`cc-switch --app <app> provider switch <id>`（仅此一个子命令；
//!       永不调用 `daemon`/`proxy` 以免与 GUI 冲突）。
//!     - `install()`：macOS 走 brew；其余平台返回人工指引（不自行 curl|bash，不覆盖 GUI）。
//!     - `detect_gui()`：best-effort 检测已安装 GUI 及其版本（只读，从不启动 GUI）。

use crate::error::AppError;
use crate::provider_manager::models::{AgentApp, CcSwitchGuiStatus, InstallResult};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;
use tokio::process::Command;

const PROBE_TIMEOUT_SECS: u64 = 4;
const SWITCH_TIMEOUT_SECS: u64 = 45;
#[cfg(target_os = "macos")]
const BREW_INSTALL_TIMEOUT_SECS: u64 = 300;
/// CLI `--help` 中必然出现的子命令 token（GUI 二进制不会有）。
const CLI_HELP_MARKERS: &[&str] = &["provider", "use", "sessions"];

/// 进程内缓存的已检测 CLI 绝对路径（首次检测后复用，安装/失效后清空）。
static DETECTED: Mutex<Option<PathBuf>> = Mutex::new(None);

struct Captured {
    stdout: String,
    success: bool,
}

/// 运行 `<exe> <args>` 并在 `timeout_secs` 内捕获 stdout/exit；超时/启动失败返回 `None`。
async fn capture(exe: &Path, args: &[&str], timeout_secs: u64) -> Option<Captured> {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(Duration::from_secs(timeout_secs), cmd.output()).await {
        Ok(Ok(o)) => o,
        _ => return None,
    };
    Some(Captured {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        success: output.status.success(),
    })
}

/// 判定候选二进制是否"行为像 CLI"：`--version` 退出 0 且 `--help` 含 CLI-only 子命令。
///
/// Business Logic: GUI 二进制被以 `--version`/`--help` 调用时可能弹窗/挂起/打印 GUI help，
/// 不会列出 `provider`/`use`/`sessions`。超时（kill_on_drop）兜底，避免 GUI 窗口挂死探测。
async fn behaves_like_cli(path: &Path) -> bool {
    let version = match capture(path, &["--version"], PROBE_TIMEOUT_SECS).await {
        Some(c) if c.success && !c.stdout.trim().is_empty() => c.stdout,
        _ => return false,
    };
    let _ = version; // 仅用于确认退出 0 + 非空
    match capture(path, &["--help"], PROBE_TIMEOUT_SECS).await {
        Some(c) => CLI_HELP_MARKERS.iter().all(|m| c.stdout.contains(m)),
        None => false,
    }
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "cc-switch.exe"
    } else {
        "cc-switch"
    }
}

/// 候选可执行路径：常见安装目录优先，再叠加 PATH 条目（去重）。
fn candidate_paths() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let name = exe_name();
    if let Some(h) = dirs::home_dir() {
        out.push(h.join(".local").join("bin").join(name));
        out.push(h.join(".cargo").join("bin").join(name));
        #[cfg(windows)]
        out.push(
            h.join("AppData")
                .join("Local")
                .join("cc-partner")
                .join("bin")
                .join(name),
        );
    }
    out.push(PathBuf::from("/opt/homebrew/bin").join(name));
    out.push(PathBuf::from("/usr/local/bin").join(name));
    out.push(PathBuf::from("/usr/bin").join(name));
    if let Some(path_env) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_env) {
            let cand = dir.join(name);
            if !out.contains(&cand) {
                out.push(cand);
            }
        }
    }
    out
}

/// 检测 cc-switch CLI，返回其绝对路径（命中后缓存）。
pub(in crate::provider_manager) async fn detect() -> Option<PathBuf> {
    if let Ok(guard) = DETECTED.lock() {
        if let Some(cached) = guard.as_ref() {
            if cached.is_file() {
                return Some(cached.clone());
            }
        }
    }
    for cand in candidate_paths() {
        if !cand.is_file() {
            continue;
        }
        if behaves_like_cli(&cand).await {
            if let Ok(mut g) = DETECTED.lock() {
                *g = Some(cand.clone());
            }
            return Some(cand);
        }
    }
    None
}

/// 清空缓存的检测结果（安装/失效后调用，强制重新探测）。
pub(in crate::provider_manager) fn invalidate() {
    if let Ok(mut g) = DETECTED.lock() {
        *g = None;
    }
}

/// `cc-switch --version` 的 trimmed 输出（退出 0 且非空时）。
pub(in crate::provider_manager) async fn version(path: &Path) -> Option<String> {
    let c = capture(path, &["--version"], PROBE_TIMEOUT_SECS).await?;
    if c.success {
        let v = c.stdout.trim().to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    None
}

/// 构造切换子命令参数（纯函数，便于单测）。
fn switch_args(app: AgentApp, provider_id: &str) -> Vec<String> {
    vec![
        "--app".to_string(),
        app.as_str().to_string(),
        "provider".to_string(),
        "switch".to_string(),
        provider_id.to_string(),
    ]
}

/// 执行 `cc-switch --app <app> provider switch <id>`。
///
/// 仅调用这一个临时子命令（运行后即退出）；绝不调用 `daemon`/`proxy`。
pub(in crate::provider_manager) async fn run_switch(
    path: &Path,
    app: AgentApp,
    provider_id: &str,
) -> Result<(), AppError> {
    let owned = switch_args(app, provider_id);
    let args: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    let mut cmd = Command::new(path);
    cmd.args(args.as_slice())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(SWITCH_TIMEOUT_SECS), cmd.output())
        .await
        .map_err(|_| AppError::timeout(format!("cc-switch 切换超时（{SWITCH_TIMEOUT_SECS} 秒）")))?
        .map_err(|e| AppError::generic(format!("启动 cc-switch 失败: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            "命令返回非零状态".to_string()
        };
        return Err(AppError::generic(format!("cc-switch 切换失败: {detail}")));
    }
    Ok(())
}

/// 安装 cc-switch CLI（显式用户动作；macOS 走 brew，其余平台返回人工指引）。
pub(in crate::provider_manager) async fn install() -> Result<InstallResult, AppError> {
    #[cfg(target_os = "macos")]
    {
        if let Some(brew) = which_brew() {
            return install_via_brew(brew).await;
        }
    }
    Ok(manual_install_result())
}

#[cfg(target_os = "macos")]
fn which_brew() -> Option<PathBuf> {
    for p in ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"] {
        if Path::new(p).is_file() {
            return Some(PathBuf::from(p));
        }
    }
    None
}

#[cfg(target_os = "macos")]
async fn install_via_brew(brew: PathBuf) -> Result<InstallResult, AppError> {
    let mut cmd = Command::new(&brew);
    cmd.args(["install", "cc-switch-cli"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(BREW_INSTALL_TIMEOUT_SECS), cmd.output())
        .await
        .map_err(|_| AppError::timeout("brew install 超时"))?
        .map_err(|e| AppError::generic(format!("启动 brew 失败: {e}")))?;
    invalidate();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if output.status.success() {
        let path = detect().await;
        let version = match &path {
            Some(p) => version(p).await,
            None => None,
        };
        Ok(InstallResult {
            method: "brew".into(),
            ok: true,
            version,
            path: path.map(|p| p.to_string_lossy().into_owned()),
            message: None,
            url: None,
        })
    } else {
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        Ok(InstallResult {
            method: "brew".into(),
            ok: false,
            version: None,
            path: None,
            message: Some(format!("brew install 失败: {detail}")),
            url: None,
        })
    }
}

/// 非 brew 平台（或 macOS 无 brew）返回人工安装指引，绝不自行 `curl|bash`。
fn manual_install_result() -> InstallResult {
    let (url, msg): (&str, String) = if cfg!(target_os = "macos") {
        (
            "https://github.com/SaladDay/cc-switch-cli#-installation",
            "未检测到 Homebrew。请安装 cc-switch-cli（`brew install cc-switch-cli` 或官方安装脚本），它与你现有的 cc-switch GUI 共享同一份数据，不会影响 GUI。".into(),
        )
    } else if cfg!(target_os = "windows") {
        (
            "https://github.com/SaladDay/cc-switch-cli/releases/latest",
            "请从 GitHub Releases 下载 cc-switch-cli 压缩包，解压到不与 GUI 冲突的目录（如 %LOCALAPPDATA%\\cc-partner\\bin）。注意：Windows 上 cc-switch CLI 与 GUI 二进制同名，请勿相互覆盖。".into(),
        )
    } else {
        (
            "https://github.com/SaladDay/cc-switch-cli#-installation",
            "请安装 cc-switch-cli（官方安装脚本或 release 压缩包），安装到不与 GUI 冲突的目录。注意：Linux 上 cc-switch CLI 与 GUI 二进制同名 `cc-switch`，请勿相互覆盖。".into(),
        )
    };
    InstallResult {
        method: "manual".into(),
        ok: false,
        version: None,
        path: None,
        message: Some(msg),
        url: Some(url.into()),
    }
}

/// best-effort 检测已安装 cc-switch GUI 及其版本（只读，从不启动/修改 GUI）。
///
/// v1 仅 macOS（按 `.app` bundle 的 `Info.plist` 读版本）；其余平台返回 `None`（未知），
/// 因为 Linux/Windows 上 GUI 与 CLI 同名，无法靠文件存在区分。
pub(in crate::provider_manager) fn detect_gui(
    cli_version: Option<&str>,
) -> Option<CcSwitchGuiStatus> {
    #[cfg(target_os = "macos")]
    {
        let (installed, version) = gui_installation();
        let version_mismatch = match (version.as_deref(), cli_version) {
            (Some(g), Some(c)) => Some(major_version_differs(g, c)),
            _ => None,
        };
        Some(CcSwitchGuiStatus {
            installed,
            version,
            running: None,
            version_mismatch,
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = cli_version;
        None
    }
}

#[cfg(target_os = "macos")]
fn gui_installation() -> (bool, Option<String>) {
    let mut dirs = vec![PathBuf::from("/Applications")];
    if let Some(h) = dirs::home_dir() {
        dirs.push(h.join("Applications"));
    }
    for base in &dirs {
        for name in ["cc-switch.app", "CC-Switch.app", "CC Switch.app"] {
            let app = base.join(name);
            if app.is_dir() {
                return (true, read_app_version(&app));
            }
        }
    }
    (false, None)
}

#[cfg(target_os = "macos")]
fn read_app_version(app: &Path) -> Option<String> {
    let plist = app.join("Contents").join("Info.plist");
    let text = std::fs::read_to_string(&plist).ok()?;
    extract_plist_version(&text, "CFBundleShortVersionString")
        .or_else(|| extract_plist_version(&text, "CFBundleVersion"))
}

/// 从 XML plist 文本中提取某个 `<key>` 后紧跟的 `<string>` 值。
#[cfg(any(test, target_os = "macos"))]
fn extract_plist_version(text: &str, key: &str) -> Option<String> {
    let idx = text.find(key)?;
    let after = &text[idx + key.len()..];
    let open = after.find("<string>")?;
    let rest = &after[open + "<string>".len()..];
    let close = rest.find("</string>")?;
    let v = rest[..close].trim().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// 比较版本字符串的主版本号是否不同。
#[cfg(any(test, target_os = "macos"))]
fn major_version_differs(a: &str, b: &str) -> bool {
    major_of(a) != major_of(b)
}

#[cfg(any(test, target_os = "macos"))]
fn major_of(v: &str) -> String {
    let v = v.trim();
    let start = v.find(|c: char| c.is_ascii_digit()).unwrap_or(v.len());
    let rest = &v[start..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        v.to_string()
    } else {
        digits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_args_shape() {
        let args = switch_args(AgentApp::Codex, "abc-123");
        assert_eq!(
            args,
            vec!["--app", "codex", "provider", "switch", "abc-123"]
        );
    }

    #[test]
    fn major_version_compare() {
        assert!(major_version_differs("5.9.3", "6.0.0"));
        assert!(!major_version_differs("5.9.3", "5.10.1"));
        // CLI 输出带名字前缀也要能解析。
        assert!(major_version_differs("cc-switch 5.9.3", "6.0.0"));
        assert!(!major_version_differs("cc-switch 5.9.3", "5.1.0"));
    }

    #[test]
    fn plist_extraction() {
        let text = "<?xml version=\"1.0\"?>\
            <plist version=\"1.0\"><dict>\
            <key>CFBundleShortVersionString</key><string>5.9.2</string>\
            <key>CFBundleVersion</key><string>20260801</string>\
            </dict></plist>";
        assert_eq!(
            extract_plist_version(text, "CFBundleShortVersionString").as_deref(),
            Some("5.9.2")
        );
        assert_eq!(
            extract_plist_version(text, "CFBundleVersion").as_deref(),
            Some("20260801")
        );
        assert!(extract_plist_version(text, "Missing").is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cli_vs_gui_binary_discrimination() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();

        // CLI-like: --version 退出 0 输出版本；--help 列出 provider/use/sessions。
        let cli_script = "#!/bin/sh\n\
            case \"$1\" in\n\
              --version) echo 'cc-switch 5.9.3';;\n\
              --help) echo 'Usage: provider list/use/sessions switch';;\n\
              *) exit 0;;\n\
            esac\n";
        let cli_path = dir.path().join("cc-switch");
        std::fs::write(&cli_path, cli_script).unwrap();
        let mut perms = std::fs::metadata(&cli_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&cli_path, perms).unwrap();
        assert!(
            behaves_like_cli(&cli_path).await,
            "CLI-like 脚本应被识别为 CLI"
        );

        // GUI-like: --version 退出 0 但 --help 不含 CLI 子命令 token。
        let gui_script = "#!/bin/sh\n\
            case \"$1\" in\n\
              --version) echo 'cc-switch GUI 5.9.2';;\n\
              --help) echo 'CC Switch GUI options'; exit 0;;\n\
              *) echo 'launching GUI';;\n\
            esac\n";
        let gui_path = dir.path().join("cc-switch-gui");
        std::fs::write(&gui_path, gui_script).unwrap();
        let mut perms = std::fs::metadata(&gui_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&gui_path, perms).unwrap();
        assert!(
            !behaves_like_cli(&gui_path).await,
            "GUI-like 脚本（无 provider/use/sessions）不应被误判为 CLI"
        );
    }
}
