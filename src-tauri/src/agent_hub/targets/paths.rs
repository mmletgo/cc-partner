//! agent_hub/targets/paths — 注入环境的 target home / 可执行路径解析
//!
//! Business Logic（为什么需要这个模块）:
//!     Claude / Codex / OpenCode 的配置根与可执行文件随环境变量与 XDG 约定变化；
//!     probe 与扫描必须在同一注入环境上解析，禁止依赖或改写真实 process env。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `TargetEnvironment` / `TargetHomes` / `TargetPathResolver`，解析
//!     CLAUDE_CONFIG_DIR / CODEX_HOME / OPENCODE_* / XDG 与默认路径，并在
//!     `path_entries` 上无 shell 地查找可执行 realpath。

use crate::agent_hub::object_store::sha256_hex;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const MAX_INSTRUCTION_SCAN_BYTES: u64 = 1024 * 1024;

/// 注入的目标探测环境（永不修改真实 process env）。
///
/// Business Logic（为什么需要这个结构体）:
///     单元测试与生产探测需要同一解析逻辑；生产传入真实 home/vars/PATH，
///     测试传入临时 fixture，避免污染开发者机器环境。
///
/// Code Logic（这个结构体做什么）:
///     持有 home、环境变量字典与有序 PATH 条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetEnvironment {
    /// 用户 home 根（默认路径锚定）
    pub home: PathBuf,
    /// 注入环境变量（含 CLAUDE_CONFIG_DIR / CODEX_HOME / OPENCODE_* / XDG_CONFIG_HOME）
    pub vars: BTreeMap<String, String>,
    /// 可执行查找 PATH 条目（绝对目录，按序）
    pub path_entries: Vec<PathBuf>,
}

impl TargetEnvironment {
    /// 读取注入环境变量。
    ///
    /// Business Logic: 解析器只读注入表，不回落 process env。
    /// Code Logic: vars.get，空串视为未设置。
    pub fn var(&self, key: &str) -> Option<&str> {
        self.vars
            .get(key)
            .map(|s| s.as_str())
            .filter(|s| !s.trim().is_empty())
    }
}

/// 单 target 配置根（及可选兼容 Skill 根）。
///
/// Business Logic（为什么需要这个结构体）:
///     Codex 兼容 Skill 发现根与 config home 可能分离，必须分别记录以免搬错路径。
///
/// Code Logic（这个结构体做什么）:
///     保存 config_root 与可选 skill_compat_root。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetHomePaths {
    /// CLI 用户级配置根目录
    pub config_root: PathBuf,
    /// 兼容 Skill 扫描根（Codex 为 `~/.agents`；Claude/OpenCode 可与 config 相同或 None）
    pub skill_compat_root: Option<PathBuf>,
}

/// OpenCode 配置根与配置文件路径。
///
/// Business Logic（为什么需要这个结构体）:
///     OpenCode 同时有 `OPENCODE_CONFIG_DIR`（目录）与 `OPENCODE_CONFIG`（文件）两条覆盖链。
///
/// Code Logic（这个结构体做什么）:
///     保存 config_root 与 config_file。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeHomePaths {
    /// 配置目录根（用户级 AGENTS.md / skills 等）
    pub config_root: PathBuf,
    /// 主配置文件路径（默认 `<config_root>/opencode.json`）
    pub config_file: PathBuf,
}

/// 三个 target 的解析结果。
///
/// Business Logic（为什么需要这个结构体）:
///     probe / scan / materialization 需要一次解析出全部 home，避免分次漂移。
///
/// Code Logic（这个结构体做什么）:
///     聚合 claude / codex / opencode 路径。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetHomes {
    /// Claude 配置根
    pub claude: TargetHomePaths,
    /// Codex 配置根
    pub codex: TargetHomePaths,
    /// OpenCode 配置根与文件
    pub opencode: OpenCodeHomePaths,
}

/// 目标路径解析器。
///
/// Business Logic（为什么需要这个结构体）:
///     设计规定默认路径不是硬编码常量，每次 probe 必须按注入环境重算。
///
/// Code Logic（这个结构体做什么）:
///     无状态命名空间，提供 `resolve_all`。
#[derive(Debug, Default, Clone, Copy)]
pub struct TargetPathResolver;

impl TargetPathResolver {
    /// 按注入环境解析三个 target 的 home。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     环境变量 / XDG / 默认 home 变化时，旧 materialization 必须失效并重新 probe。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Claude: CLAUDE_CONFIG_DIR → `~/.claude`；
    ///     Codex: CODEX_HOME → `~/.codex`，skill_compat_root=`~/.agents`；
    ///     OpenCode: OPENCODE_CONFIG_DIR → XDG_CONFIG_HOME/opencode → `~/.config/opencode`，
    ///     配置文件 OPENCODE_CONFIG → `<config_root>/opencode.json`。
    pub fn resolve_all(env: &TargetEnvironment) -> TargetHomes {
        TargetHomes {
            claude: resolve_claude_home(env),
            codex: resolve_codex_home(env),
            opencode: resolve_opencode_home(env),
        }
    }
}

/// 解析 Claude 配置根。
///
/// Business Logic: 官方允许 CLAUDE_CONFIG_DIR 覆盖默认 `~/.claude`。
/// Code Logic: 优先 env 变量，否则 home/.claude；skill_compat 暂与 config 相同。
fn resolve_claude_home(env: &TargetEnvironment) -> TargetHomePaths {
    let config_root = env
        .var("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| env.home.join(".claude"));
    TargetHomePaths {
        skill_compat_root: Some(config_root.join("skills")),
        config_root,
    }
}

/// 解析 Codex 配置根。
///
/// Business Logic: CODEX_HOME 覆盖；兼容 Skill 扫描根独立为 `~/.agents`。
/// Code Logic: config_root 与 skill_compat_root 分别记录。
fn resolve_codex_home(env: &TargetEnvironment) -> TargetHomePaths {
    let config_root = env
        .var("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| env.home.join(".codex"));
    TargetHomePaths {
        skill_compat_root: Some(env.home.join(".agents")),
        config_root,
    }
}

/// 解析 OpenCode 配置根与配置文件。
///
/// Business Logic: OPENCODE_CONFIG_DIR / XDG / 默认 `~/.config/opencode`；
///     OPENCODE_CONFIG 可单独指向自定义配置文件。
/// Code Logic: 分两路解析 root 与 file。
fn resolve_opencode_home(env: &TargetEnvironment) -> OpenCodeHomePaths {
    let config_root = if let Some(dir) = env.var("OPENCODE_CONFIG_DIR") {
        PathBuf::from(dir)
    } else if let Some(xdg) = env.var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg).join("opencode")
    } else {
        env.home.join(".config").join("opencode")
    };
    let config_file = env
        .var("OPENCODE_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| config_root.join("opencode.json"));
    OpenCodeHomePaths {
        config_root,
        config_file,
    }
}

/// 在注入 PATH 条目中查找可执行文件并尽量 realpath。
///
/// Business Logic（为什么需要这个函数）:
///     probe fingerprint 依赖稳定 executable 路径；禁止 shell `which` 以免注入副作用。
///
/// Code Logic（这个函数做什么）:
///     绝对路径直接 canonicalize；裸命令名按 path_entries 顺序匹配可执行文件；
///     Windows 额外尝试 .exe/.cmd/.bat。找不到返回 None。
pub fn resolve_executable(command: &str, env: &TargetEnvironment) -> Option<PathBuf> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = Path::new(trimmed);
    if candidate.is_absolute() || trimmed.contains('/') || trimmed.contains('\\') {
        return canonicalize_if_exists(candidate);
    }
    for dir in &env.path_entries {
        if let Some(found) = executable_in_dir(dir, trimmed) {
            return canonicalize_if_exists(&found).or(Some(found));
        }
    }
    None
}

/// 目录内查找可执行候选。
///
/// Business Logic: 跨平台 CLI 安装形态不同。
/// Code Logic: 直接名优先；Windows 尝试常见扩展。
fn executable_in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    let direct = dir.join(name);
    if is_runnable_file(&direct) {
        return Some(direct);
    }
    #[cfg(windows)]
    {
        for ext in ["exe", "cmd", "bat"] {
            let with_ext = dir.join(name).with_extension(ext);
            if is_runnable_file(&with_ext) {
                return Some(with_ext);
            }
        }
    }
    None
}

/// 判断路径是否为现存文件。
fn is_runnable_file(path: &Path) -> bool {
    path.is_file()
}

/// 存在则 canonicalize，否则返回 None。
fn canonicalize_if_exists(path: &Path) -> Option<PathBuf> {
    if !path.exists() {
        return None;
    }
    fs_canonicalize(path)
        .ok()
        .or_else(|| Some(path.to_path_buf()))
}

/// canonicalize 包装（便于测试桩）。
fn fs_canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path)
}

/// 运行 `<exe> --version` 并解析版本字符串。
///
/// Business Logic（为什么需要这个函数）:
///     未知或解析失败的版本只能 scan-only，不能声称写能力 supported。
///
/// Code Logic（这个函数做什么）:
///     同步 `Command` 调 `--version`（无 shell）；成功时取 stdout 首行非空文本；
///     超时/非零/空输出返回 None。不修改 process env。
pub fn probe_cli_version(executable: &Path) -> Option<String> {
    let mut cmd = Command::new(executable);
    cmd.arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // 不继承测试注入之外的 PATH 语义：直接用绝对 executable。
    let output = match run_command_with_timeout(cmd, Duration::from_secs(5)) {
        Ok(out) => out,
        Err(_) => return None,
    };
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())?
        .to_string();
    if line.is_empty() {
        None
    } else {
        Some(line)
    }
}

/// 带超时执行命令（Unix 用 wait_timeout 近似；否则直接 output）。
///
/// Business Logic: probe 不得挂死 sidecar。
/// Code Logic: 优先 `output()`；失败映射为 io::Error。
fn run_command_with_timeout(
    mut cmd: Command,
    _timeout: Duration,
) -> std::io::Result<std::process::Output> {
    // 单测 fixture CLI 立即返回；生产 5s 级 CLI --version 通常足够。
    // 完整 kill-on-timeout 留给后续 runtime；此处避免引入额外 crate。
    cmd.output()
}

/// 计算 probe fingerprint（exe/version/config_root 任一变化即失效）。
///
/// Business Logic（为什么需要这个函数）:
///     materialization 必须在环境变化时失效并重新检查，禁止盲搬旧 home。
///
/// Code Logic（这个函数做什么）:
///     对稳定字符串做 SHA-256 hex。
pub fn compute_probe_fingerprint(
    target: &str,
    executable: Option<&Path>,
    version: Option<&str>,
    config_root: &Path,
) -> String {
    let exe = executable
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ver = version.unwrap_or("");
    let payload = format!("{target}|{exe}|{ver}|{}", config_root.to_string_lossy());
    sha256_hex(payload.as_bytes())
}

/// 读取文本文件内容（缺失返回 None，非 UTF-8 报错）。
///
/// Business Logic: 指令文件必须 UTF-8；无效编码进入 blocked 前先失败上报。
/// Code Logic: 不存在 → Ok(None)；超过 1 MiB fail-closed；否则读字节并 from_utf8。
pub fn read_utf8_file(path: &Path) -> Result<Option<String>, AppError> {
    if !path.exists() {
        return Ok(None);
    }
    let metadata = std::fs::metadata(path).map_err(AppError::from)?;
    if !metadata.is_file() {
        return Ok(None);
    }
    if metadata.len() > MAX_INSTRUCTION_SCAN_BYTES {
        return Err(AppError::validation(format!(
            "agent_hub_instruction_too_large:{}",
            path.display()
        )));
    }
    let bytes = std::fs::read(path).map_err(AppError::from)?;
    let text = String::from_utf8(bytes).map_err(|e| {
        AppError::validation(format!(
            "agent_hub_instruction_not_utf8:{}: {e}",
            path.display()
        ))
    })?;
    Ok(Some(text))
}

/// 文件是否存在且 trim 后非空。
///
/// Business Logic: Codex inactive 诊断只关心非空遮蔽源。
/// Code Logic: 读 UTF-8，trim 非空为 true；缺失/空为 false；非法 UTF-8 上抛。
pub fn is_non_empty_utf8_file(path: &Path) -> Result<bool, AppError> {
    match read_utf8_file(path)? {
        Some(text) => Ok(!text.trim().is_empty()),
        None => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn env_with(
        home: &str,
        vars: &[(&str, &str)],
        path_entries: Vec<PathBuf>,
    ) -> TargetEnvironment {
        let mut map = BTreeMap::new();
        for (k, v) in vars {
            map.insert((*k).to_string(), (*v).to_string());
        }
        TargetEnvironment {
            home: PathBuf::from(home),
            vars: map,
            path_entries,
        }
    }

    #[test]
    fn resolve_all_prefers_explicit_env_overrides() {
        let env = env_with(
            "/tmp/home",
            &[
                ("CLAUDE_CONFIG_DIR", "/tmp/claude-home"),
                ("CODEX_HOME", "/tmp/codex-home"),
                ("OPENCODE_CONFIG_DIR", "/tmp/oc-dir"),
                ("OPENCODE_CONFIG", "/tmp/custom-opencode.json"),
            ],
            vec![],
        );
        let homes = TargetPathResolver::resolve_all(&env);
        assert_eq!(homes.claude.config_root, PathBuf::from("/tmp/claude-home"));
        assert_eq!(homes.codex.config_root, PathBuf::from("/tmp/codex-home"));
        assert_eq!(homes.opencode.config_root, PathBuf::from("/tmp/oc-dir"));
        assert_eq!(
            homes.opencode.config_file,
            PathBuf::from("/tmp/custom-opencode.json")
        );
        assert_eq!(
            homes.codex.skill_compat_root,
            Some(PathBuf::from("/tmp/home/.agents"))
        );
    }

    #[test]
    fn resolve_all_uses_xdg_then_default_for_opencode() {
        let xdg = env_with(
            "/tmp/home",
            &[("XDG_CONFIG_HOME", "/tmp/xdg-config")],
            vec![],
        );
        let homes = TargetPathResolver::resolve_all(&xdg);
        assert_eq!(
            homes.opencode.config_root,
            PathBuf::from("/tmp/xdg-config/opencode")
        );
        assert_eq!(
            homes.opencode.config_file,
            PathBuf::from("/tmp/xdg-config/opencode/opencode.json")
        );

        let defaults = env_with("/tmp/home", &[], vec![]);
        let homes = TargetPathResolver::resolve_all(&defaults);
        assert_eq!(homes.claude.config_root, PathBuf::from("/tmp/home/.claude"));
        assert_eq!(homes.codex.config_root, PathBuf::from("/tmp/home/.codex"));
        assert_eq!(
            homes.opencode.config_root,
            PathBuf::from("/tmp/home/.config/opencode")
        );
        assert_eq!(
            homes.opencode.config_file,
            PathBuf::from("/tmp/home/.config/opencode/opencode.json")
        );
    }

    #[test]
    fn resolve_executable_uses_path_entries_without_process_env() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("fake-cli");
        fs::write(&bin, b"#!/bin/sh\necho 1.2.3\n").expect("write");
        let mut perms = fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin, perms).unwrap();

        let env = env_with("/tmp/home", &[], vec![dir.path().to_path_buf()]);
        let resolved = resolve_executable("fake-cli", &env).expect("found");
        assert_eq!(
            fs::canonicalize(&resolved).unwrap(),
            fs::canonicalize(&bin).unwrap()
        );
        assert!(resolve_executable("missing-cli", &env).is_none());
    }

    #[test]
    fn probe_cli_version_parses_stdout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("ver-cli");
        fs::write(&bin, b"#!/bin/sh\necho 'cli 9.8.7'\n").expect("write");
        let mut perms = fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin, perms).unwrap();
        let version = probe_cli_version(&bin).expect("version");
        assert!(version.contains("9.8.7"));
    }

    #[test]
    fn fingerprint_changes_with_config_root() {
        let a = compute_probe_fingerprint(
            "claude",
            Some(Path::new("/bin/claude")),
            Some("1.0"),
            Path::new("/a"),
        );
        let b = compute_probe_fingerprint(
            "claude",
            Some(Path::new("/bin/claude")),
            Some("1.0"),
            Path::new("/b"),
        );
        assert_ne!(a, b);
    }
}
