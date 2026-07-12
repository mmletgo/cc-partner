//! commands/lan_firewall_dependency.rs — 局域网防火墙依赖检测命令
//!
//! Business Logic（为什么需要这个模块）:
//!     Workbench 局域网互联依赖本机 P2P HTTP TCP 端口与 mDNS UDP 5353 被系统防火墙允许入站；
//!     Settings 依赖环境页需要向用户展示当前端口、局域网 IP 和对应系统的放行方法。
//!
//! Code Logic（这个模块做什么）:
//!     检测 HTTP 服务监听、局域网 IP，并按平台只读探测 TCP/mDNS 防火墙放行状态；
//!     不申请管理员权限、不修改防火墙，读取不到放行规则时按未开放返回并保留可复制指引。

use crate::net::discovery::local_lan_ip;
use crate::state::AppState;
use serde::Serialize;
use std::process::Command;
use std::sync::atomic::Ordering;
use tauri::State;

const MDNS_PORT: u16 = 5353;

/// 局域网防火墙指引支持的平台。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LanFirewallPlatform {
    MacOs,
    Windows,
    Linux,
    Unsupported,
}

/// 单个检测项 DTO。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LanFirewallCheckDto {
    pub id: String,
    pub ok: bool,
    pub detail: String,
}

/// 系统防火墙只读探测结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LanFirewallProbe {
    pub tcp_allowed: bool,
    pub mdns_allowed: bool,
}

/// 单条系统方法步骤 DTO。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LanFirewallStepDto {
    pub label_key: String,
}

/// 单条可复制命令 DTO。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LanFirewallCommandDto {
    pub label_key: String,
    pub command: String,
}

/// 当前系统的防火墙放行指引 DTO。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LanFirewallGuidanceDto {
    pub summary_key: String,
    pub steps: Vec<LanFirewallStepDto>,
    pub commands: Vec<LanFirewallCommandDto>,
}

/// Settings 依赖环境页消费的局域网防火墙依赖 DTO。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LanFirewallDependencyStatusDto {
    pub platform: String,
    pub platform_label: String,
    pub lan_ip: Option<String>,
    pub http_port: u16,
    pub mdns_port: u16,
    pub app_path: Option<String>,
    pub checks: Vec<LanFirewallCheckDto>,
    pub guidance: LanFirewallGuidanceDto,
}

impl LanFirewallPlatform {
    /// Business Logic（为什么需要这个函数）:
    ///     前端需要稳定的平台枚举字符串来选择 i18n 文案和展示逻辑。
    ///
    /// Code Logic（这个函数做什么）:
    ///     把 Rust 内部平台枚举映射为前端 DTO 使用的小写平台 key。
    fn as_key(self) -> &'static str {
        match self {
            Self::MacOs => "macos",
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Unsupported => "unsupported",
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     后端 DTO 需要提供一个可读平台标签，便于普通渲染和调试。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将内部平台枚举映射为短平台名；用户可见最终文案仍由前端 i18n 决定。
    fn label(self) -> &'static str {
        match self {
            Self::MacOs => "macOS",
            Self::Windows => "Windows",
            Self::Linux => "Linux",
            Self::Unsupported => "Unsupported",
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     防火墙指引需要按照当前编译运行平台生成，避免前端猜测桌面系统。
///
/// Code Logic（这个函数做什么）:
///     读取 `std::env::consts::OS` 并归一为内部平台枚举；非三大桌面平台归为 Unsupported。
fn current_lan_firewall_platform() -> LanFirewallPlatform {
    match std::env::consts::OS {
        "macos" => LanFirewallPlatform::MacOs,
        "windows" => LanFirewallPlatform::Windows,
        "linux" => LanFirewallPlatform::Linux,
        _ => LanFirewallPlatform::Unsupported,
    }
}

/// Business Logic（为什么需要这个函数）:
///     macOS socketfilterfw 命令需要带当前应用可执行文件路径，路径中可能包含空格。
///
/// Code Logic（这个函数做什么）:
///     使用双引号包裹路径并转义反斜杠与双引号，返回可读的 shell 命令参数。
fn quote_shell_path(path: &str) -> String {
    format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Business Logic（为什么需要这个函数）:
///     Settings 依赖环境页需要展示当前应用路径，以便 macOS 用户按 App 防火墙放行。
///
/// Code Logic（这个函数做什么）:
///     调用 `std::env::current_exe`，失败或非 UTF-8 路径返回 None。
fn current_app_path() -> Option<String> {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.into_os_string().into_string().ok())
}

/// Business Logic（为什么需要这个函数）:
///     防火墙开放状态需要通过当前系统工具读取，但应用不能申请管理员权限或修改系统设置。
///
/// Code Logic（这个函数做什么）:
///     执行只读系统命令并合并 stdout/stderr；命令不存在或无法启动时返回 None。
fn read_command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    Some(text)
}

/// Business Logic（为什么需要这个函数）:
///     macOS 应用防火墙关闭时，TCP 端口与 mDNS 入站不再被该防火墙拦截，应直接显示已开放。
///
/// Code Logic（这个函数做什么）:
///     解析 socketfilterfw --getglobalstate 输出；返回 true 表示全局防火墙关闭，false 表示开启。
fn parse_macos_global_firewall_open(output: &str) -> Option<bool> {
    let lower = output.to_ascii_lowercase();
    let compact = lower.replace(' ', "");
    if lower.contains("disabled") || compact.contains("state=0") {
        return Some(true);
    }
    if lower.contains("enabled") || compact.contains("state=1") {
        return Some(false);
    }
    None
}

/// Business Logic（为什么需要这个函数）:
///     macOS “阻止所有入站连接”会覆盖单个应用放行状态，必须优先判定为未开放。
///
/// Code Logic（这个函数做什么）:
///     解析 socketfilterfw --getblockall 输出；返回 true 表示 block all 已开启。
fn parse_macos_block_all_enabled(output: &str) -> Option<bool> {
    let lower = output.to_ascii_lowercase();
    let compact = lower.replace(' ', "");
    if lower.contains("disabled")
        || lower.contains("off")
        || lower.contains(": no")
        || lower.contains("= no")
        || compact.contains("state=0")
    {
        return Some(false);
    }
    if lower.contains("enabled")
        || lower.contains(" on")
        || lower.contains(": yes")
        || lower.contains("= yes")
        || compact.contains("state=1")
    {
        return Some(true);
    }
    None
}

/// Business Logic（为什么需要这个函数）:
///     macOS 应用防火墙按 App 放行，用户需要看到当前 cc-partner App 是否允许接收入站连接。
///
/// Code Logic（这个函数做什么）:
///     解析 socketfilterfw --getappblocked 输出；true 表示 App 未被阻止，false 表示被阻止或未加入规则。
fn parse_macos_app_allowed(output: &str) -> Option<bool> {
    let lower = output.to_ascii_lowercase();
    let compact = lower.replace(' ', "");
    if lower.contains("not blocked") || compact.contains("state=0") {
        return Some(true);
    }
    if lower.contains("is blocked") || compact.contains("state=1") {
        return Some(false);
    }
    if lower.contains("not part") || lower.contains("not found") {
        return Some(false);
    }
    None
}

/// Business Logic（为什么需要这个函数）:
///     macOS 用户通常通过应用防火墙允许 cc-partner，而不是分别添加端口规则。
///
/// Code Logic（这个函数做什么）:
///     读取 socketfilterfw 全局、block all 和当前 App 状态；无法读取到明确允许时返回 false。
fn macos_application_firewall_allows_app(app_path: Option<&str>) -> bool {
    let socketfilterfw = "/usr/libexec/ApplicationFirewall/socketfilterfw";
    if read_command_output(socketfilterfw, &["--getglobalstate"])
        .and_then(|output| parse_macos_global_firewall_open(&output))
        == Some(true)
    {
        return true;
    }
    if read_command_output(socketfilterfw, &["--getblockall"])
        .and_then(|output| parse_macos_block_all_enabled(&output))
        == Some(true)
    {
        return false;
    }
    app_path
        .and_then(|path| read_command_output(socketfilterfw, &["--getappblocked", path]))
        .and_then(|output| parse_macos_app_allowed(&output))
        .unwrap_or(false)
}

/// Business Logic（为什么需要这个函数）:
///     Windows 需要读取当前 profile 和入站允许规则，直接判断目标端口是否已开放。
///
/// Code Logic（这个函数做什么）:
///     通过 PowerShell 读取 Get-NetFirewallProfile/Get-NetFirewallRule；匹配 ALLOW 输出即认为开放。
fn windows_firewall_allows_port(protocol: &str, port: u16) -> bool {
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue';\
         $enabledProfiles=Get-NetFirewallProfile | Where-Object {{$_.Enabled -eq $true}};\
         if ($enabledProfiles.Count -eq 0) {{ 'ALLOW'; exit 0 }};\
         $port='{port}'; $proto='{protocol}';\
         $rules=Get-NetFirewallRule -Enabled True -Direction Inbound -Action Allow | ForEach-Object {{ $_ | Get-NetFirewallPortFilter }} | Where-Object {{ $_.Protocol -eq $proto -and ($_.LocalPort -eq $port -or $_.LocalPort -eq 'Any' -or ($_.LocalPort -is [array] -and ($_.LocalPort -contains $port -or $_.LocalPort -contains 'Any'))) }};\
         if ($rules) {{ 'ALLOW' }} else {{ 'DENY' }}"
    );
    ["powershell", "powershell.exe", "pwsh", "pwsh.exe"]
        .iter()
        .filter_map(|program| {
            read_command_output(
                program,
                &[
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    script.as_str(),
                ],
            )
        })
        .any(|output| output.to_ascii_uppercase().contains("ALLOW"))
}

/// Business Logic（为什么需要这个函数）:
///     Linux/Ubuntu 常见防火墙是 ufw 或 firewalld，需要按实际启用工具读取端口放行状态。
///
/// Code Logic（这个函数做什么）:
///     读取 ufw status 和 firewalld 端口/服务列表；防火墙明确 inactive/not running 时视为未阻止。
fn linux_firewall_allows_port(protocol: &str, port: u16) -> bool {
    let needle = format!("{port}/{protocol}");
    if let Some(output) = read_command_output("ufw", &["status"]) {
        let lower = output.to_ascii_lowercase();
        if lower.contains("status: inactive") {
            return true;
        }
        if lower
            .lines()
            .any(|line| line.contains(&needle) && line.contains("allow"))
        {
            return true;
        }
    }

    if let Some(output) = read_command_output("firewall-cmd", &["--state"]) {
        let lower = output.to_ascii_lowercase();
        if lower.contains("not running") {
            return true;
        }
        if lower.contains("running") {
            let ports = read_command_output("firewall-cmd", &["--list-ports"])
                .unwrap_or_default()
                .to_ascii_lowercase();
            if ports
                .split_whitespace()
                .any(|port_rule| port_rule == needle)
            {
                return true;
            }
            if protocol == "udp" && port == MDNS_PORT {
                let services = read_command_output("firewall-cmd", &["--list-services"])
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                return services.split_whitespace().any(|service| service == "mdns");
            }
        }
    }

    false
}

/// Business Logic（为什么需要这个函数）:
///     Settings 依赖环境页需要直接显示 TCP 当前端口与 mDNS UDP 5353 是否已开放。
///
/// Code Logic（这个函数做什么）:
///     按平台调用只读系统探测函数，返回明确 boolean；无法证明开放时返回 false。
fn probe_lan_firewall(
    platform: LanFirewallPlatform,
    http_port: u16,
    mdns_port: u16,
    app_path: Option<&str>,
) -> LanFirewallProbe {
    match platform {
        LanFirewallPlatform::MacOs => {
            let app_allowed = macos_application_firewall_allows_app(app_path);
            LanFirewallProbe {
                tcp_allowed: http_port > 0 && app_allowed,
                mdns_allowed: app_allowed,
            }
        }
        LanFirewallPlatform::Windows => LanFirewallProbe {
            tcp_allowed: http_port > 0 && windows_firewall_allows_port("TCP", http_port),
            mdns_allowed: windows_firewall_allows_port("UDP", mdns_port),
        },
        LanFirewallPlatform::Linux => LanFirewallProbe {
            tcp_allowed: http_port > 0 && linux_firewall_allows_port("tcp", http_port),
            mdns_allowed: linux_firewall_allows_port("udp", mdns_port),
        },
        LanFirewallPlatform::Unsupported => LanFirewallProbe {
            tcp_allowed: false,
            mdns_allowed: false,
        },
    }
}

/// Business Logic（为什么需要这个函数）:
///     不同系统开放 P2P TCP 端口和 mDNS UDP 5353 的方法不同，后端需要给前端稳定指导数据。
///
/// Code Logic（这个函数做什么）:
///     根据平台、HTTP 端口、mDNS 端口和可选应用路径生成 i18n key 化步骤与命令字符串；
///     只生成命令预览，不执行命令。
pub(crate) fn build_lan_firewall_guidance(
    platform: LanFirewallPlatform,
    http_port: u16,
    mdns_port: u16,
    app_path: Option<&str>,
) -> LanFirewallGuidanceDto {
    match platform {
        LanFirewallPlatform::MacOs => {
            let mut commands = Vec::new();
            if let Some(path) = app_path {
                let quoted = quote_shell_path(path);
                commands.push(LanFirewallCommandDto {
                    label_key: "settings:lanFirewall.guidance.macos.allowAppCommand".to_string(),
                    command: format!(
                        "sudo /usr/libexec/ApplicationFirewall/socketfilterfw --add {quoted}"
                    ),
                });
                commands.push(LanFirewallCommandDto {
                    label_key: "settings:lanFirewall.guidance.macos.unblockAppCommand".to_string(),
                    command: format!(
                        "sudo /usr/libexec/ApplicationFirewall/socketfilterfw --unblockapp {quoted}"
                    ),
                });
            }
            LanFirewallGuidanceDto {
                summary_key: "settings:lanFirewall.guidance.macos.summary".to_string(),
                steps: vec![
                    LanFirewallStepDto {
                        label_key: "settings:lanFirewall.guidance.macos.stepSystemSettings"
                            .to_string(),
                    },
                    LanFirewallStepDto {
                        label_key: "settings:lanFirewall.guidance.macos.stepAllowApp".to_string(),
                    },
                    LanFirewallStepDto {
                        label_key: "settings:lanFirewall.guidance.macos.stepPorts".to_string(),
                    },
                ],
                commands,
            }
        }
        LanFirewallPlatform::Windows => {
            let mut commands = Vec::new();
            if http_port > 0 {
                commands.push(LanFirewallCommandDto {
                    label_key: "settings:lanFirewall.guidance.windows.tcpCommand".to_string(),
                    command: format!(
                        "netsh advfirewall firewall add rule name=\"cc-partner P2P TCP {http_port}\" dir=in action=allow protocol=TCP localport={http_port}"
                    ),
                });
            }
            commands.push(LanFirewallCommandDto {
                label_key: "settings:lanFirewall.guidance.windows.mdnsCommand".to_string(),
                command: format!(
                    "netsh advfirewall firewall add rule name=\"cc-partner mDNS UDP {mdns_port}\" dir=in action=allow protocol=UDP localport={mdns_port}"
                ),
            });
            LanFirewallGuidanceDto {
                summary_key: "settings:lanFirewall.guidance.windows.summary".to_string(),
                steps: vec![
                    LanFirewallStepDto {
                        label_key: "settings:lanFirewall.guidance.windows.stepAdmin".to_string(),
                    },
                    LanFirewallStepDto {
                        label_key: "settings:lanFirewall.guidance.windows.stepRules".to_string(),
                    },
                ],
                commands,
            }
        }
        LanFirewallPlatform::Linux => {
            let mut commands = Vec::new();
            if http_port > 0 {
                commands.extend([
                    LanFirewallCommandDto {
                        label_key: "settings:lanFirewall.guidance.linux.ufwTcp".to_string(),
                        command: format!("sudo ufw allow {http_port}/tcp"),
                    },
                    LanFirewallCommandDto {
                        label_key: "settings:lanFirewall.guidance.linux.ufwMdns".to_string(),
                        command: format!("sudo ufw allow {mdns_port}/udp"),
                    },
                ]);
            } else {
                commands.push(LanFirewallCommandDto {
                    label_key: "settings:lanFirewall.guidance.linux.ufwMdns".to_string(),
                    command: format!("sudo ufw allow {mdns_port}/udp"),
                });
            }
            if http_port > 0 {
                commands.push(LanFirewallCommandDto {
                    label_key: "settings:lanFirewall.guidance.linux.firewalldTcp".to_string(),
                    command: format!("sudo firewall-cmd --permanent --add-port={http_port}/tcp"),
                });
            }
            commands.extend([
                LanFirewallCommandDto {
                    label_key: "settings:lanFirewall.guidance.linux.firewalldMdns".to_string(),
                    command: format!("sudo firewall-cmd --permanent --add-port={mdns_port}/udp"),
                },
                LanFirewallCommandDto {
                    label_key: "settings:lanFirewall.guidance.linux.firewalldReload".to_string(),
                    command: "sudo firewall-cmd --reload".to_string(),
                },
            ]);
            LanFirewallGuidanceDto {
                summary_key: "settings:lanFirewall.guidance.linux.summary".to_string(),
                steps: vec![
                    LanFirewallStepDto {
                        label_key: "settings:lanFirewall.guidance.linux.stepChooseFirewall"
                            .to_string(),
                    },
                    LanFirewallStepDto {
                        label_key: "settings:lanFirewall.guidance.linux.stepReload".to_string(),
                    },
                ],
                commands,
            }
        }
        LanFirewallPlatform::Unsupported => LanFirewallGuidanceDto {
            summary_key: "settings:lanFirewall.guidance.unsupported.summary".to_string(),
            steps: vec![LanFirewallStepDto {
                label_key: "settings:lanFirewall.guidance.unsupported.stepManual".to_string(),
            }],
            commands: Vec::new(),
        },
    }
}

/// Business Logic（为什么需要这个函数）:
///     Settings 依赖环境页需要清楚显示 HTTP/LAN 基础状态，以及 TCP/mDNS 防火墙是否已开放。
///
/// Code Logic（这个函数做什么）:
///     生成 HTTP 监听、局域网 IP、TCP 防火墙和 mDNS 防火墙四项检查；所有检查都返回明确 boolean。
fn build_lan_firewall_checks(
    http_port: u16,
    lan_ip: Option<&str>,
    mdns_port: u16,
    probe: LanFirewallProbe,
) -> Vec<LanFirewallCheckDto> {
    vec![
        LanFirewallCheckDto {
            id: "httpListener".to_string(),
            ok: http_port > 0,
            detail: if http_port > 0 {
                format!("TCP {http_port}")
            } else {
                "not-listening".to_string()
            },
        },
        LanFirewallCheckDto {
            id: "lanIp".to_string(),
            ok: lan_ip.is_some(),
            detail: lan_ip.unwrap_or("unavailable").to_string(),
        },
        LanFirewallCheckDto {
            id: "tcpFirewall".to_string(),
            ok: probe.tcp_allowed,
            detail: if http_port > 0 {
                format!("TCP {http_port}")
            } else {
                "unavailable".to_string()
            },
        },
        LanFirewallCheckDto {
            id: "mdnsFirewall".to_string(),
            ok: probe.mdns_allowed,
            detail: format!("UDP {mdns_port}"),
        },
    ]
}

/// 检测局域网互联防火墙依赖。
///
/// Business Logic（为什么需要这个函数）:
///     用户通过局域网访问本机项目时，需要明确当前设备应开放哪些端口以及如何在当前系统放行。
///
/// Code Logic（这个函数做什么）:
///     从 AppState 读取实际 HTTP 监听端口，探测局域网 IP 和防火墙开放状态，生成平台化指引 DTO。
#[tauri::command]
pub fn check_lan_firewall_dependency(state: State<'_, AppState>) -> LanFirewallDependencyStatusDto {
    let platform = current_lan_firewall_platform();
    let http_port = state.actual_http_port.load(Ordering::SeqCst);
    let lan_ip = local_lan_ip().map(|ip| ip.to_string());
    let app_path = current_app_path();
    let probe = probe_lan_firewall(platform, http_port, MDNS_PORT, app_path.as_deref());
    let guidance = build_lan_firewall_guidance(platform, http_port, MDNS_PORT, app_path.as_deref());

    LanFirewallDependencyStatusDto {
        platform: platform.as_key().to_string(),
        platform_label: platform.label().to_string(),
        lan_ip: lan_ip.clone(),
        http_port,
        mdns_port: MDNS_PORT,
        app_path,
        checks: build_lan_firewall_checks(http_port, lan_ip.as_deref(), MDNS_PORT, probe),
        guidance,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_lan_firewall_checks, build_lan_firewall_guidance, LanFirewallPlatform,
        LanFirewallProbe,
    };

    /// Business Logic（为什么需要这个函数）:
    ///     防火墙指引测试需要断言不同系统会给出对应 TCP/UDP 端口开放命令。
    ///
    /// Code Logic（这个函数做什么）:
    ///     使用指定平台、HTTP 端口和 App 路径构造 guidance，并返回命令字符串数组便于断言。
    fn command_lines(platform: LanFirewallPlatform) -> Vec<String> {
        build_lan_firewall_guidance(
            platform,
            62116,
            5353,
            Some("/Applications/cc-partner.app/Contents/MacOS/cc-partner"),
        )
        .commands
        .into_iter()
        .map(|command| command.command)
        .collect()
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Windows 用户需要用 netsh 增加入站规则，覆盖 TCP 当前端口和 UDP 5353。
    ///
    /// Code Logic（这个测试做什么）:
    ///     生成 Windows guidance 并断言命令包含 TCP/UDP 两条 netsh localport 规则。
    #[test]
    fn windows_guidance_includes_tcp_and_mdns_netsh_rules() {
        let commands = command_lines(LanFirewallPlatform::Windows);
        assert!(commands
            .iter()
            .any(|command| command.contains("protocol=TCP localport=62116")));
        assert!(commands
            .iter()
            .any(|command| command.contains("protocol=UDP localport=5353")));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Linux/Ubuntu 用户可能使用 ufw 或 firewalld，指引必须同时覆盖两类防火墙。
    ///
    /// Code Logic（这个测试做什么）:
    ///     生成 Linux guidance 并断言 ufw 与 firewalld 都包含 TCP 当前端口和 UDP 5353。
    #[test]
    fn linux_guidance_includes_ufw_and_firewalld_rules() {
        let commands = command_lines(LanFirewallPlatform::Linux);
        assert!(commands
            .iter()
            .any(|command| command == "sudo ufw allow 62116/tcp"));
        assert!(commands
            .iter()
            .any(|command| command == "sudo ufw allow 5353/udp"));
        assert!(commands
            .iter()
            .any(|command| { command == "sudo firewall-cmd --permanent --add-port=62116/tcp" }));
        assert!(commands
            .iter()
            .any(|command| { command == "sudo firewall-cmd --permanent --add-port=5353/udp" }));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     macOS 应优先提示应用防火墙按 App 放行，而不是假装能可靠检测单端口放行状态。
    ///
    /// Code Logic（这个测试做什么）:
    ///     生成 macOS guidance 并断言 socketfilterfw 命令包含当前 App 可执行文件路径。
    #[test]
    fn macos_guidance_prefers_application_firewall_command() {
        let commands = command_lines(LanFirewallPlatform::MacOs);
        assert!(commands.iter().any(|command| {
            command.contains("/usr/libexec/ApplicationFirewall/socketfilterfw --add")
                && command.contains("/Applications/cc-partner.app/Contents/MacOS/cc-partner")
        }));
        assert!(commands.iter().any(|command| {
            command.contains("socketfilterfw --unblockapp")
                && command.contains("/Applications/cc-partner.app/Contents/MacOS/cc-partner")
        }));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     HTTP 服务未启动时还没有实际 P2P TCP 端口，指引不能生成 localport=0 这类无效规则误导用户。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 0 端口生成 Windows/Linux guidance，断言只保留 UDP 5353/mDNS 相关命令。
    #[test]
    fn guidance_omits_tcp_rule_when_http_port_is_unknown() {
        for platform in [LanFirewallPlatform::Windows, LanFirewallPlatform::Linux] {
            let commands = build_lan_firewall_guidance(platform, 0, 5353, None)
                .commands
                .into_iter()
                .map(|command| command.command)
                .collect::<Vec<_>>();
            assert!(commands.iter().any(|command| command.contains("5353")));
            assert!(!commands
                .iter()
                .any(|command| command.contains("localport=0")));
            assert!(!commands
                .iter()
                .any(|command| command.contains("allow 0/tcp")));
            assert!(!commands
                .iter()
                .any(|command| command.contains("add-port=0/tcp")));
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Settings 依赖环境页必须直接显示 TCP/mDNS 防火墙是否已开放，不能再显示“手动确认”。
    ///
    /// Code Logic（这个测试做什么）:
    ///     将探测结果注入 checks 构造函数，断言 tcpFirewall/mdnsFirewall 都返回明确 boolean。
    #[test]
    fn checks_report_tcp_and_mdns_firewall_boolean_probe_results() {
        let checks = build_lan_firewall_checks(
            62116,
            Some("192.168.1.12"),
            5353,
            LanFirewallProbe {
                tcp_allowed: true,
                mdns_allowed: false,
            },
        );

        let tcp = checks
            .iter()
            .find(|check| check.id == "tcpFirewall")
            .expect("tcp firewall check");
        let mdns = checks
            .iter()
            .find(|check| check.id == "mdnsFirewall")
            .expect("mdns firewall check");

        assert!(tcp.ok);
        assert!(!mdns.ok);
    }
}
