//! agent_hub/autostart — 用户级登录自启动安装/检查/卸载（无 sudo）。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户首次启用 Agent Hub 后，关闭 GUI 仍希望登录时自动拉起 backend owner。
//!     必须用当前用户 LaunchAgent / Task Scheduler / systemd --user，失败时返回
//!     `backgroundStartUnavailable` 且不破坏同进程 runtime。
//!
//! Code Logic（这个模块做什么）:
//!     生成平台 artifact（plist / Task XML / unit），原子写入注入路径，再以 argv 数组
//!     调用 launchctl / schtasks / systemctl。`inspect` 确认已安装项引用当前可执行文件。
//!     全部通过 `FileAdapter`/`CommandAdapter` 注入，单测不得触碰开发者真实登录项。

use crate::error::AppError;
use std::path::{Path, PathBuf};

/// 登录自启动失败时的稳定业务 code。
pub const BACKGROUND_START_UNAVAILABLE: &str = "backgroundStartUnavailable";

/// macOS LaunchAgent label / 文件名。
pub const MACOS_LAUNCH_AGENT_LABEL: &str = "com.cc-partner.agent-hub";
/// Linux systemd user unit 名。
pub const LINUX_SYSTEMD_UNIT: &str = "cc-partner-agent-hub.service";
/// Windows Task Scheduler 任务名。
pub const WINDOWS_TASK_NAME: &str = "cc-partner-agent-hub";

/// 可注入的文件读写抽象。
///
/// Business Logic（为什么需要这个 trait）:
///     单测必须在 temp 路径生成/校验 artifact，禁止写开发者真实 LaunchAgents。
///
/// Code Logic（这个 trait 做什么）:
///     create_dir_all / write_atomic / read / remove / exists。
pub trait FileAdapter {
    /// 递归创建目录。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     首次安装需确保 LaunchAgents / systemd user / 任务 XML 父目录存在。
    ///
    /// Code Logic（这个函数做什么）:
    ///     等价 `std::fs::create_dir_all`。
    fn create_dir_all(&self, path: &Path) -> Result<(), AppError>;

    /// 原子写入文件内容。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     崩溃半写会留下损坏 plist/unit，导致登录启动失败。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写同目录临时文件后 rename 到目标。
    fn write_atomic(&self, path: &Path, content: &[u8]) -> Result<(), AppError>;

    /// 读取 UTF-8 文件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     inspect 需解析已安装 artifact 是否指向当前可执行文件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读文件为 String；不存在返回 NotFound。
    fn read_to_string(&self, path: &Path) -> Result<String, AppError>;

    /// 删除文件；不存在视为成功。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     uninstall 与失败回滚需要清理 artifact。
    ///
    /// Code Logic（这个函数做什么）:
    ///     remove_file；NotFound → Ok。
    fn remove_file(&self, path: &Path) -> Result<(), AppError>;

    /// 路径是否存在。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     inspect 在文件缺失时直接判定未安装。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `path.exists()`。
    fn exists(&self, path: &Path) -> bool;
}

/// 可注入的外部命令执行抽象。
///
/// Business Logic（为什么需要这个 trait）:
///     install/remove 必须调用 launchctl/schtasks/systemctl，单测只记录 argv。
///
/// Code Logic（这个 trait 做什么）:
///     `run(program, args)` 返回退出码与合并输出；失败映射业务错误。
pub trait CommandAdapter {
    /// 以 argv 数组执行命令（无 shell）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     平台登录启动注册必须走 argv，禁止 `sh -c` 拼接。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 `(exit_code, stdout+stderr)`。
    fn run(&self, program: &str, args: &[&str]) -> Result<(i32, String), AppError>;
}

/// inspect 结果。
///
/// Business Logic（为什么需要这个结构）:
///     只有确认登录项引用当前可执行文件后，才能把 `background_enabled` 置 true。
///
/// Code Logic（这个结构做什么）:
///     installed + matches_current_executable 两布尔。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutostartInspect {
    /// artifact 是否存在。
    pub installed: bool,
    /// 内容是否引用当前可执行文件与 `supervise`。
    pub matches_current_executable: bool,
}

/// Agent Hub 用户级登录自启动适配器。
///
/// Business Logic（为什么需要这个结构）:
///     集中生成三平台 artifact 并 install/inspect/remove，避免 GUI 层硬编码路径。
///
/// Code Logic（这个结构做什么）:
///     持有当前 backend 可执行路径与用户 home（测试可注入 temp home）。
#[derive(Debug, Clone)]
pub struct AgentHubAutostart {
    /// 当前 `cc-partner-backend`（或等价）可执行文件绝对路径。
    executable: PathBuf,
    /// 用户 home，用于派生 LaunchAgents / .config / 任务路径。
    home: PathBuf,
}

impl AgentHubAutostart {
    /// 生产：current_exe + dirs::home_dir。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 启用 Hub 时用真实路径安装登录启动。
    ///
    /// Code Logic（这个函数做什么）:
    ///     解析 current_exe 与 home；缺失 home → backgroundStartUnavailable。
    pub fn from_environment() -> Result<Self, AppError> {
        let executable = std::env::current_exe().map_err(AppError::from)?;
        let home =
            dirs::home_dir().ok_or_else(|| AppError::unavailable(BACKGROUND_START_UNAVAILABLE))?;
        Ok(Self { executable, home })
    }

    /// 注入路径构造（单测 / 显式路径）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单测使用 temp home 与假 exe，不碰真实登录项。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 absolute-ish PathBuf（不做 canonicalize，便于 snapshot 稳定）。
    pub fn new(executable: PathBuf, home: PathBuf) -> Self {
        Self { executable, home }
    }

    /// 当前可执行路径。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     测试与调用方需要读取将写入 artifact 的 exe 路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回内部 Path 引用。
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// 生成 macOS LaunchAgent plist 内容。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     登录时 RunAtLoad 启动 `supervise`；不设 KeepAlive，退避由 supervise 拥有。
    ///
    /// Code Logic（这个函数做什么）:
    ///     输出 XML plist：Label + ProgramArguments=[exe, supervise] + RunAtLoad。
    pub fn render_macos_plist(&self) -> String {
        let exe = escape_xml(&self.executable.to_string_lossy());
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>supervise</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
</dict>
</plist>
"#,
            label = MACOS_LAUNCH_AGENT_LABEL,
            exe = exe,
        )
    }

    /// 生成 Windows 当前用户 Task Scheduler XML（LogonTrigger）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户登录时启动 supervise；无第二套重启策略。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Task 1.2 XML：LogonTrigger + Exec Command/Arguments=supervise。
    pub fn render_windows_task_xml(&self) -> String {
        let exe = escape_xml(&self.executable.to_string_lossy());
        format!(
            r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <Arguments>supervise</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
            exe = exe,
        )
    }

    /// 生成 Linux systemd user unit。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户会话 default.target 拉起 supervise；无 Restart=，退避由 supervise 拥有。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Type=simple，ExecStart=`exe supervise`，WantedBy=default.target。
    pub fn render_linux_systemd_unit(&self) -> String {
        let exe = self.executable.to_string_lossy();
        format!(
            r#"[Unit]
Description=cc-partner Agent Hub background owner
After=default.target

[Service]
Type=simple
ExecStart={exe} supervise
# Restart policy intentionally omitted: supervise owns exponential backoff.

[Install]
WantedBy=default.target
"#,
            exe = exe,
        )
    }

    /// macOS plist 路径：`~/Library/LaunchAgents/com.cc-partner.agent-hub.plist`。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     install/inspect/remove 需要同一稳定路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     home/Library/LaunchAgents/<label>.plist。
    pub fn macos_plist_path(&self) -> PathBuf {
        self.home
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{MACOS_LAUNCH_AGENT_LABEL}.plist"))
    }

    /// Linux unit 路径：`~/.config/systemd/user/cc-partner-agent-hub.service`。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     user unit 文件位置固定，供 systemctl enable 与 inspect。
    ///
    /// Code Logic（这个函数做什么）:
    ///     home/.config/systemd/user/<unit>。
    pub fn linux_unit_path(&self) -> PathBuf {
        self.home
            .join(".config")
            .join("systemd")
            .join("user")
            .join(LINUX_SYSTEMD_UNIT)
    }

    /// Windows 任务 XML 暂存路径（schtasks /Create /XML 输入）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     schtasks 需要 XML 文件；放在 home 下应用私有目录，测试可注入。
    ///
    /// Code Logic（这个函数做什么）:
    ///     home/.cc-partner/autostart/cc-partner-agent-hub.task.xml。
    pub fn windows_task_xml_path(&self) -> PathBuf {
        self.home
            .join(".cc-partner")
            .join("autostart")
            .join("cc-partner-agent-hub.task.xml")
    }

    /// 当前平台 artifact 路径。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     inspect/remove 按宿主 OS 选择路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     cfg 分支返回对应 PathBuf。
    pub fn artifact_path(&self) -> PathBuf {
        #[cfg(target_os = "macos")]
        {
            self.macos_plist_path()
        }
        #[cfg(target_os = "windows")]
        {
            self.windows_task_xml_path()
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            self.linux_unit_path()
        }
        #[cfg(not(any(unix, windows)))]
        {
            self.home.join("cc-partner-agent-hub.autostart")
        }
    }

    /// 当前平台 artifact 正文。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     install 写入与 snapshot 测试共用同一渲染。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 OS 调用对应 render。
    pub fn render_artifact(&self) -> String {
        #[cfg(target_os = "macos")]
        {
            self.render_macos_plist()
        }
        #[cfg(target_os = "windows")]
        {
            self.render_windows_task_xml()
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            self.render_linux_systemd_unit()
        }
        #[cfg(not(any(unix, windows)))]
        {
            format!("{} supervise\n", self.executable.display())
        }
    }

    /// 安装登录自启动。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户确认启用 Hub 后台后，注册无 sudo 登录项；权限失败 → backgroundStartUnavailable。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写 artifact → 调用平台 bootstrap/create/enable；命令非零 → unavailable code。
    pub fn install(
        &self,
        files: &impl FileAdapter,
        cmds: &impl CommandAdapter,
    ) -> Result<(), AppError> {
        let path = self.artifact_path();
        if let Some(parent) = path.parent() {
            files.create_dir_all(parent).map_err(map_permission_err)?;
        }
        let body = self.render_artifact();
        files
            .write_atomic(&path, body.as_bytes())
            .map_err(map_permission_err)?;
        self.register_with_os(cmds, &path)
            .map_err(map_permission_err)?;
        Ok(())
    }

    /// 检查已安装登录项是否引用当前可执行文件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     仅在 inspect 确认后才能把 `background_enabled=true` 持久化；失败/不匹配应重置该字段。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读 artifact；不存在 → installed=false；内容含 exe 路径与 `supervise` → matches。
    pub fn inspect(&self, files: &impl FileAdapter) -> Result<AutostartInspect, AppError> {
        let path = self.artifact_path();
        if !files.exists(&path) {
            return Ok(AutostartInspect {
                installed: false,
                matches_current_executable: false,
            });
        }
        let content = files.read_to_string(&path)?;
        let exe = self.executable.to_string_lossy();
        let matches = content.contains(exe.as_ref()) && content.contains("supervise");
        Ok(AutostartInspect {
            installed: true,
            matches_current_executable: matches,
        })
    }

    /// 卸载登录自启动。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户关闭后台或切换安装路径时需清理登录项；不停止当前已运行的 owner 进程。
    ///
    /// Code Logic（这个函数做什么）:
    ///     平台 bootout/delete/disable → 删除 artifact 文件。
    pub fn remove(
        &self,
        files: &impl FileAdapter,
        cmds: &impl CommandAdapter,
    ) -> Result<(), AppError> {
        let path = self.artifact_path();
        let _ = self.unregister_with_os(cmds, &path);
        files.remove_file(&path).map_err(map_permission_err)?;
        Ok(())
    }

    /// 平台注册命令（argv 数组）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     写入文件后仍需通知 OS 加载登录项。
    ///
    /// Code Logic（这个函数做什么）:
    ///     macOS: launchctl bootstrap gui/$UID <plist>
    ///     Windows: schtasks /Create /TN <name> /XML <path> /F
    ///     Linux: systemctl --user daemon-reload && enable --now <unit>
    fn register_with_os(
        &self,
        cmds: &impl CommandAdapter,
        artifact: &Path,
    ) -> Result<(), AppError> {
        #[cfg(target_os = "macos")]
        {
            let uid = current_uid_string();
            let domain = format!("gui/{uid}");
            let path = artifact.to_string_lossy();
            let (code, out) =
                cmds.run("launchctl", &["bootstrap", domain.as_str(), path.as_ref()])?;
            if code != 0 {
                // 已加载时尝试 bootout 再 bootstrap
                let _ = cmds.run("launchctl", &["bootout", domain.as_str(), path.as_ref()]);
                let (code2, out2) =
                    cmds.run("launchctl", &["bootstrap", domain.as_str(), path.as_ref()])?;
                if code2 != 0 {
                    return Err(AppError::unavailable(format!(
                        "{BACKGROUND_START_UNAVAILABLE}: launchctl bootstrap failed: {out2}"
                    )));
                }
                let _ = out;
            }
            let _ = code;
            Ok(())
        }
        #[cfg(target_os = "windows")]
        {
            let path = artifact.to_string_lossy();
            let (code, out) = cmds.run(
                "schtasks",
                &[
                    "/Create",
                    "/TN",
                    WINDOWS_TASK_NAME,
                    "/XML",
                    path.as_ref(),
                    "/F",
                ],
            )?;
            if code != 0 {
                return Err(AppError::unavailable(format!(
                    "{BACKGROUND_START_UNAVAILABLE}: schtasks create failed: {out}"
                )));
            }
            Ok(())
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let _ = artifact;
            let (code, out) = cmds.run("systemctl", &["--user", "daemon-reload"])?;
            if code != 0 {
                return Err(AppError::unavailable(format!(
                    "{BACKGROUND_START_UNAVAILABLE}: systemctl daemon-reload failed: {out}"
                )));
            }
            let (code, out) = cmds.run(
                "systemctl",
                &["--user", "enable", "--now", LINUX_SYSTEMD_UNIT],
            )?;
            if code != 0 {
                return Err(AppError::unavailable(format!(
                    "{BACKGROUND_START_UNAVAILABLE}: systemctl enable failed: {out}"
                )));
            }
            Ok(())
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (cmds, artifact);
            Err(AppError::unavailable(BACKGROUND_START_UNAVAILABLE))
        }
    }

    /// 平台注销命令。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     卸载时先从 OS 取消注册，再删文件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     launchctl bootout / schtasks /Delete / systemctl disable --now。
    fn unregister_with_os(
        &self,
        cmds: &impl CommandAdapter,
        artifact: &Path,
    ) -> Result<(), AppError> {
        #[cfg(target_os = "macos")]
        {
            let uid = current_uid_string();
            let domain = format!("gui/{uid}");
            let path = artifact.to_string_lossy();
            let _ = cmds.run("launchctl", &["bootout", domain.as_str(), path.as_ref()]);
            Ok(())
        }
        #[cfg(target_os = "windows")]
        {
            let _ = artifact;
            let _ = cmds.run("schtasks", &["/Delete", "/TN", WINDOWS_TASK_NAME, "/F"]);
            Ok(())
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let _ = artifact;
            let _ = cmds.run(
                "systemctl",
                &["--user", "disable", "--now", LINUX_SYSTEMD_UNIT],
            );
            let _ = cmds.run("systemctl", &["--user", "daemon-reload"]);
            Ok(())
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (cmds, artifact);
            Ok(())
        }
    }
}

/// 把权限/能力类错误统一映射为 backgroundStartUnavailable。
///
/// Business Logic（为什么需要这个函数）:
///     产品 UI 只识别稳定 code；底层 IO 权限失败不能伪装成 generic。
///
/// Code Logic（这个函数做什么）:
///     已是该 code 的 Unavailable 原样返回；其它错误包装为 unavailable code。
fn map_permission_err(err: AppError) -> AppError {
    if err.code() == BACKGROUND_START_UNAVAILABLE
        || err.code().starts_with(BACKGROUND_START_UNAVAILABLE)
    {
        return err;
    }
    AppError::unavailable(format!("{BACKGROUND_START_UNAVAILABLE}: {err}"))
}

/// XML 最小转义。
///
/// Business Logic（为什么需要这个函数）:
///     可执行路径可能含 & < >，写入 plist/Task XML 必须合法。
///
/// Code Logic（这个函数做什么）:
///     替换 & < > " '。
fn escape_xml(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// 当前用户 uid 字符串（macOS launchctl domain）。
///
/// Business Logic（为什么需要这个函数）:
///     `launchctl bootstrap gui/<uid>` 需要会话域。
///
/// Code Logic（这个函数做什么）:
///     libc getuid 转十进制字符串。
#[cfg(target_os = "macos")]
fn current_uid_string() -> String {
    unsafe { libc::getuid() }.to_string()
}

/// 生产文件系统适配器。
///
/// Business Logic（为什么需要这个结构）:
///     真实 install 写入用户 home 下 artifact。
///
/// Code Logic（这个结构做什么）:
///     委托 std::fs，write_atomic 用同目录 .tmp + rename。
#[derive(Debug, Default)]
pub struct StdFileAdapter;

impl FileAdapter for StdFileAdapter {
    /// 递归创建目录。
    ///
    /// Business Logic（为什么需要这个函数）: 见 trait。
    /// Code Logic（这个函数做什么）: `fs::create_dir_all`。
    fn create_dir_all(&self, path: &Path) -> Result<(), AppError> {
        std::fs::create_dir_all(path).map_err(AppError::from)
    }

    /// 原子写。
    ///
    /// Business Logic（为什么需要这个函数）: 见 trait。
    /// Code Logic（这个函数做什么）: 写 path.tmp → rename。
    fn write_atomic(&self, path: &Path, content: &[u8]) -> Result<(), AppError> {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, content).map_err(AppError::from)?;
        std::fs::rename(&tmp, path).map_err(AppError::from)
    }

    /// 读文件。
    ///
    /// Business Logic（为什么需要这个函数）: 见 trait。
    /// Code Logic（这个函数做什么）: `fs::read_to_string`。
    fn read_to_string(&self, path: &Path) -> Result<String, AppError> {
        std::fs::read_to_string(path).map_err(AppError::from)
    }

    /// 删文件。
    ///
    /// Business Logic（为什么需要这个函数）: 见 trait。
    /// Code Logic（这个函数做什么）: NotFound 忽略。
    fn remove_file(&self, path: &Path) -> Result<(), AppError> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AppError::from(e)),
        }
    }

    /// 是否存在。
    ///
    /// Business Logic（为什么需要这个函数）: 见 trait。
    /// Code Logic（这个函数做什么）: `path.exists()`。
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

/// 生产命令适配器。
///
/// Business Logic（为什么需要这个结构）:
///     install/remove 真正调用系统工具。
///
/// Code Logic（这个结构做什么）:
///     `std::process::Command`，合并 stdout/stderr。
#[derive(Debug, Default)]
pub struct StdCommandAdapter;

impl CommandAdapter for StdCommandAdapter {
    /// 执行外部命令。
    ///
    /// Business Logic（为什么需要这个函数）: 见 trait。
    /// Code Logic（这个函数做什么）: spawn + output，返回 code 与合并文本。
    fn run(&self, program: &str, args: &[&str]) -> Result<(i32, String), AppError> {
        let output = std::process::Command::new(program)
            .args(args)
            .output()
            .map_err(AppError::from)?;
        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        let err = String::from_utf8_lossy(&output.stderr);
        if !err.is_empty() {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(&err);
        }
        let code = output.status.code().unwrap_or(1);
        Ok((code, combined))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// 内存文件适配器。
    #[derive(Default, Clone)]
    struct MemFiles {
        map: Arc<RefCell<HashMap<PathBuf, Vec<u8>>>>,
    }

    impl FileAdapter for MemFiles {
        fn create_dir_all(&self, _path: &Path) -> Result<(), AppError> {
            Ok(())
        }

        fn write_atomic(&self, path: &Path, content: &[u8]) -> Result<(), AppError> {
            self.map
                .borrow_mut()
                .insert(path.to_path_buf(), content.to_vec());
            Ok(())
        }

        fn read_to_string(&self, path: &Path) -> Result<String, AppError> {
            self.map
                .borrow()
                .get(path)
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .ok_or_else(|| AppError::not_found(path.display().to_string()))
        }

        fn remove_file(&self, path: &Path) -> Result<(), AppError> {
            self.map.borrow_mut().remove(path);
            Ok(())
        }

        fn exists(&self, path: &Path) -> bool {
            self.map.borrow().contains_key(path)
        }
    }

    /// 记录 argv 的假命令适配器。
    #[derive(Default)]
    struct RecordingCmds {
        calls: RefCell<Vec<(String, Vec<String>)>>,
        /// 若 true，所有命令返回失败。
        fail: bool,
    }

    impl CommandAdapter for RecordingCmds {
        fn run(&self, program: &str, args: &[&str]) -> Result<(i32, String), AppError> {
            self.calls.borrow_mut().push((
                program.to_string(),
                args.iter().map(|s| (*s).to_string()).collect(),
            ));
            if self.fail {
                Ok((1, "permission denied".to_string()))
            } else {
                Ok((0, String::new()))
            }
        }
    }

    fn sample() -> AgentHubAutostart {
        AgentHubAutostart::new(
            PathBuf::from("/opt/cc-partner/cc-partner-backend"),
            PathBuf::from("/Users/test"),
        )
    }

    /// macOS plist 快照：supervise、无 shell、无 KeepAlive。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     登录启动契约固定；二次重启策略必须由 supervise 拥有。
    ///
    /// Code Logic（这个测试做什么）:
    ///     渲染 plist，断言 ProgramArguments 含 exe+supervise，无 KeepAlive/shell。
    #[test]
    fn macos_plist_snapshot_invokes_supervise_without_keepalive() {
        let a = sample();
        let plist = a.render_macos_plist();
        assert!(plist.contains("<string>/opt/cc-partner/cc-partner-backend</string>"));
        assert!(plist.contains("<string>supervise</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(!plist.to_lowercase().contains("keepalive"));
        assert!(!plist.contains("/bin/sh"));
        assert!(!plist.contains("bash"));
        assert_eq!(
            a.macos_plist_path(),
            PathBuf::from("/Users/test/Library/LaunchAgents/com.cc-partner.agent-hub.plist")
        );
    }

    /// Windows Task XML 快照：LogonTrigger + supervise。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     Windows 登录触发器与 argv 契约必须稳定。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 LogonTrigger、Command、Arguments=supervise，无 Restart 类策略字段滥用。
    #[test]
    fn windows_task_xml_snapshot_logon_supervise() {
        let a = sample();
        let xml = a.render_windows_task_xml();
        assert!(xml.contains("<LogonTrigger>"));
        assert!(xml.contains("<Command>/opt/cc-partner/cc-partner-backend</Command>"));
        assert!(xml.contains("<Arguments>supervise</Arguments>"));
        assert!(!xml.contains("cmd.exe"));
        assert!(!xml.contains("/bin/sh"));
    }

    /// Linux unit 快照：ExecStart supervise，无 Restart=。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     systemd 不得叠加第二套重启策略。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 ExecStart 与 WantedBy，且正文无 Restart=。
    #[test]
    fn linux_unit_snapshot_no_restart_policy() {
        let a = sample();
        let unit = a.render_linux_systemd_unit();
        assert!(unit.contains("ExecStart=/opt/cc-partner/cc-partner-backend supervise"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(!unit.contains("Restart="));
        assert_eq!(
            a.linux_unit_path(),
            PathBuf::from("/Users/test/.config/systemd/user/cc-partner-agent-hub.service")
        );
    }

    /// install 后 inspect 匹配当前 exe；remove 后未安装。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     background_enabled 只能在 inspect 确认后置 true；卸载需可观测。
    ///
    /// Code Logic（这个测试做什么）:
    ///     MemFiles + RecordingCmds 走 install/inspect/remove 全路径。
    #[test]
    fn install_inspect_remove_with_injected_adapters() {
        let a = sample();
        let files = MemFiles::default();
        let cmds = RecordingCmds::default();
        a.install(&files, &cmds).unwrap();
        let insp = a.inspect(&files).unwrap();
        assert!(insp.installed);
        assert!(insp.matches_current_executable);
        assert!(!cmds.calls.borrow().is_empty());
        // 无 shell 拼接：每个 call 的 args 都是独立字符串
        for (prog, args) in cmds.calls.borrow().iter() {
            assert!(!prog.contains(' '));
            assert!(!args.iter().any(|s| s.contains("&&") || s.contains(';')));
        }
        a.remove(&files, &cmds).unwrap();
        let insp2 = a.inspect(&files).unwrap();
        assert!(!insp2.installed);
    }

    /// 命令权限失败返回 backgroundStartUnavailable。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     平台能力不足时 UI 显示稳定 code，同进程 runtime 仍可继续。
    ///
    /// Code Logic（这个测试做什么）:
    ///     fail=true 的 CommandAdapter，install Err.code 含 backgroundStartUnavailable。
    #[test]
    fn install_permission_failure_maps_to_background_start_unavailable() {
        let a = sample();
        let files = MemFiles::default();
        let cmds = RecordingCmds {
            fail: true,
            ..Default::default()
        };
        let err = a.install(&files, &cmds).unwrap_err();
        assert!(
            err.code().contains(BACKGROUND_START_UNAVAILABLE),
            "unexpected code: {}",
            err.code()
        );
    }

    /// inspect 在内容不匹配当前 exe 时 matches=false。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     旧安装指向其它二进制时不得把 background_enabled 标 true。
    ///
    /// Code Logic（这个测试做什么）:
    ///     手写错误路径 artifact，assert matches_current_executable=false。
    #[test]
    fn inspect_detects_mismatched_executable() {
        let a = sample();
        let files = MemFiles::default();
        let path = a.artifact_path();
        files
            .write_atomic(&path, b"ExecStart=/other/bin/backend supervise\n")
            .unwrap();
        let insp = a.inspect(&files).unwrap();
        assert!(insp.installed);
        assert!(!insp.matches_current_executable);
    }
}
