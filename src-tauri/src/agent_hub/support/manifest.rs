//! agent_hub/support/manifest — 编译期 support manifest 解析与 fail-closed 求值
//!
//! Business Logic（为什么需要这个模块）:
//!     adapter 写能力必须绑定 exact min/current 版本与 quality-matrix evidence；
//!     版本缺失、evidence 空、semver 畸形、fingerprint 漂移时只能 scan-only。
//!
//! Code Logic（这个模块做什么）:
//!     `SUPPORT_MANIFEST_JSON` 通过 include_str 嵌入；serde 解析 camelCase JSON；
//!     `evaluate_target_support` 对每个 capability 独立降级，不改写编译期字节。

use crate::agent_hub::models::AgentTarget;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 编译期嵌入的 support-manifest.json 原文。
///
/// Business Logic: runtime 写路径不能改写支持合同；发布物与源码同字节。
/// Code Logic: `include_str!` 常量。
pub const SUPPORT_MANIFEST_JSON: &str = include_str!("support-manifest.json");

/// 目标 CLI 能力维度。
///
/// Business Logic（为什么需要这个枚举）:
///     scan / render / activate / deactivate / live-reload 必须分项求值，不能一把梭。
///
/// Code Logic（这个枚举做什么）:
///     camelCase wire token；未知 JSON key 解析失败（fail-closed）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TargetCapability {
    /// 扫描指令文件
    ScanInstruction,
    /// 渲染/投影指令（写）
    RenderInstruction,
    /// 扫描 portable assets
    ScanPortableAssets,
    /// 渲染 portable assets（写计划）
    RenderPortableAssets,
    /// 激活 managed package
    ActivatePackage,
    /// 卸载/停用 managed package
    DeactivatePackage,
    /// 变更是否 live reload
    LiveReload,
}

impl TargetCapability {
    /// 稳定 wire 字符串。
    ///
    /// Business Logic: 日志、Attention、检查脚本共用 token。
    /// Code Logic: 与 serde camelCase 对齐。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ScanInstruction => "scanInstruction",
            Self::RenderInstruction => "renderInstruction",
            Self::ScanPortableAssets => "scanPortableAssets",
            Self::RenderPortableAssets => "renderPortableAssets",
            Self::ActivatePackage => "activatePackage",
            Self::DeactivatePackage => "deactivatePackage",
            Self::LiveReload => "liveReload",
        }
    }

    /// 是否为“写侧”能力（缺证据时必须 blocked）。
    ///
    /// Business Logic: 扫描可在 fail-closed 下保留 read-only；写/激活必须证据齐全。
    /// Code Logic: render*/activate*/deactivate*/liveReload 为 write-side。
    pub fn is_write_side(self) -> bool {
        matches!(
            self,
            Self::RenderInstruction
                | Self::RenderPortableAssets
                | Self::ActivatePackage
                | Self::DeactivatePackage
                | Self::LiveReload
        )
    }

    /// 解析 wire token。
    ///
    /// Business Logic: 未知 capability 不得静默忽略。
    /// Code Logic: 精确匹配；未知返回 None。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "scanInstruction" => Some(Self::ScanInstruction),
            "renderInstruction" => Some(Self::RenderInstruction),
            "scanPortableAssets" => Some(Self::ScanPortableAssets),
            "renderPortableAssets" => Some(Self::RenderPortableAssets),
            "activatePackage" => Some(Self::ActivatePackage),
            "deactivatePackage" => Some(Self::DeactivatePackage),
            "liveReload" => Some(Self::LiveReload),
            _ => None,
        }
    }
}

/// 单 capability 支持级别。
///
/// Business Logic（为什么需要这个枚举）:
///     UI/投影需要区分 blocked / 只读 / 已支持 / 需重启 / 需手动激活。
///
/// Code Logic（这个枚举做什么）:
///     camelCase；`supportedAfterRestart` / `activationRequired` 保留 camelCase。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CapabilitySupport {
    /// 禁止该能力
    Blocked,
    /// 只读（可 scan / 预览，不可写）
    ReadOnly,
    /// 已验证支持
    Supported,
    /// 支持但需新会话/重启生效
    SupportedAfterRestart,
    /// 需要用户手动激活步骤
    ActivationRequired,
}

impl CapabilitySupport {
    /// 稳定 wire 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::ReadOnly => "readOnly",
            Self::Supported => "supported",
            Self::SupportedAfterRestart => "supportedAfterRestart",
            Self::ActivationRequired => "activationRequired",
        }
    }

    /// 是否属于 Supported* 族（需要 evidence）。
    ///
    /// Business Logic: gate 检查器要求 Supported* 绑定 quality-matrix ID。
    /// Code Logic: Supported / SupportedAfterRestart / ActivationRequired。
    pub fn is_supported_family(self) -> bool {
        matches!(
            self,
            Self::Supported | Self::SupportedAfterRestart | Self::ActivationRequired
        )
    }

    /// 解析 wire token。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "blocked" => Some(Self::Blocked),
            "readOnly" => Some(Self::ReadOnly),
            "supported" => Some(Self::Supported),
            "supportedAfterRestart" => Some(Self::SupportedAfterRestart),
            "activationRequired" => Some(Self::ActivationRequired),
            _ => None,
        }
    }
}

/// 可执行探测规格。
///
/// Business Logic（为什么需要这个结构体）:
///     adapter 必须声明如何解析 CLI 版本与可选 help 指纹，禁止硬编码猜测。
///
/// Code Logic（这个结构体做什么）:
///     commandNames / versionArgs / 可选前后缀与 helpFingerprint。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutableProbeSpec {
    /// 查找的命令名列表（按序）
    pub command_names: Vec<String>,
    /// 版本探测参数（默认 `["--version"]`）
    #[serde(default = "default_version_args")]
    pub version_args: Vec<String>,
    /// 版本输出允许的前缀（剥离后解析 semver）
    #[serde(default)]
    pub version_prefix: Option<String>,
    /// 版本输出允许的后缀
    #[serde(default)]
    pub version_suffix: Option<String>,
    /// 可选 help 输出指纹（SHA-256 hex）；不匹配则 fail-closed
    #[serde(default)]
    pub help_fingerprint: Option<String>,
}

fn default_version_args() -> Vec<String> {
    vec!["--version".to_string()]
}

/// 单个 target 的支持记录（编译期 manifest 行）。
///
/// Business Logic（为什么需要这个结构体）:
///     min/current 版本、capabilities、evidence 是写能力门禁的唯一权威源。
///
/// Code Logic（这个结构体做什么）:
///     camelCase；版本字段可为 null（开发态）；求值时 null → scan-only。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetSupportRecord {
    /// 目标 CLI
    pub target: AgentTarget,
    /// adapter/schema 版本
    pub adapter_schema_version: u32,
    /// 最小已测版本（null = 未认证）
    #[serde(default)]
    pub min_tested_version: Option<String>,
    /// 当前已测版本（null = 未认证）
    #[serde(default)]
    pub current_tested_version: Option<String>,
    /// 超过该 major 的版本视为破坏性未知
    #[serde(default)]
    pub guarded_major: Option<u64>,
    /// 可执行探测规格
    pub executable_probe: ExecutableProbeSpec,
    /// 激活命令指纹（key=稳定命令面 token，value=SHA-256 hex）
    #[serde(default)]
    pub activation_command_fingerprints: BTreeMap<String, String>,
    /// 分项能力
    pub capabilities: BTreeMap<TargetCapability, CapabilitySupport>,
    /// quality-matrix evidence IDs
    #[serde(default)]
    pub evidence_ids: Vec<String>,
}

/// support-manifest 中的 Hook 跨 target 映射原始行（字符串 intent/trust，避免 support→plugins 循环依赖）。
///
/// Business Logic（为什么需要这个结构体）:
///     Hook mapping 合同与 target support 同文件发布；解析校验在 plugins::hook_mapping。
///
/// Code Logic（这个结构体做什么）:
///     camelCase；intent/trustModel 为稳定 wire 字符串。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupportHookMappingRecord {
    /// 事件意图 token（如 preToolUse）
    pub intent: String,
    /// 来源 target
    pub source_target: AgentTarget,
    /// 目标 target
    pub destination_target: AgentTarget,
    /// schema 版本
    pub schema_version: u32,
    /// 信任模型 token（如 exactContract）
    pub trust_model: String,
    /// quality-matrix evidence ID
    pub evidence_id: String,
    /// 输入合同 required fields
    #[serde(default)]
    pub required_input_fields: Vec<String>,
    /// 输出合同 required fields
    #[serde(default)]
    pub required_output_fields: Vec<String>,
}

/// 完整 support manifest 根对象。
///
/// Business Logic: 一次发布内三 target 合同必须齐套；Hook 跨 target mapping 默认空。
/// Code Logic: schemaVersion + targets[] + optional hookMappings[]。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupportManifest {
    /// manifest schema 版本
    pub schema_version: u32,
    /// 三 target 记录
    pub targets: Vec<TargetSupportRecord>,
    /// 证据驱动的 Hook 跨 target 映射（初始可为空）
    #[serde(default)]
    pub hook_mappings: Vec<SupportHookMappingRecord>,
}

/// 运行时 probe 快照（用于与 manifest 比对）。
///
/// Business Logic（为什么需要这个结构体）:
///     executable realpath / version / config root 变化必须使旧 support 失效。
///
/// Code Logic（这个结构体做什么）:
///     承载运行时探测结果与可选 help 指纹，不持有 secret。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeProbeSnapshot {
    /// 目标
    pub target: AgentTarget,
    /// 可执行 realpath
    pub executable: Option<PathBuf>,
    /// 原始版本字符串（含前缀后缀）
    pub version: Option<String>,
    /// 配置根
    pub config_root: PathBuf,
    /// 与 TargetProbe.fingerprint 对齐的环境指纹
    pub fingerprint: String,
    /// 可选实际 help 指纹
    pub help_fingerprint: Option<String>,
}

/// 求值后的整体模式。
///
/// Business Logic: UI 与投影调度需要一等 scan-only / certified / blocked 摘要。
/// Code Logic: 枚举 + reasons。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvaluatedSupportMode {
    /// 只能扫描；写侧全部降级
    ScanOnly {
        /// 降级原因（稳定 token，无 secret）
        reasons: Vec<String>,
    },
    /// 版本与 evidence 齐备，按 capability map 开放
    Certified,
    /// 目标整体不可用
    Blocked {
        /// 原因
        reasons: Vec<String>,
    },
}

/// 单 target 求值结果。
///
/// Business Logic: 每 capability 独立；写侧可被强制降级。
/// Code Logic: mode + 降级后的 capabilities + reasons。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluatedTargetSupport {
    /// 目标
    pub target: AgentTarget,
    /// 整体模式
    pub mode: EvaluatedSupportMode,
    /// 求值后的 capability → support
    pub capabilities: BTreeMap<TargetCapability, CapabilitySupport>,
    /// 是否允许任意写侧能力
    pub write_allowed: bool,
    /// 汇总原因
    pub reasons: Vec<String>,
}

impl EvaluatedTargetSupport {
    /// 查询某 capability 的求值结果（缺省 blocked）。
    ///
    /// Business Logic: 未知 capability 一律 blocked。
    /// Code Logic: get.or Blocked。
    pub fn capability(&self, cap: TargetCapability) -> CapabilitySupport {
        self.capabilities
            .get(&cap)
            .copied()
            .unwrap_or(CapabilitySupport::Blocked)
    }

    /// 写侧能力是否被允许为 Supported* 族。
    ///
    /// Business Logic: 激活/投影路径在写被禁止时必须直接短路。
    /// Code Logic: write_allowed 且 capability 属于 supported family。
    pub fn allows_write_capability(&self, cap: TargetCapability) -> bool {
        if !cap.is_write_side() {
            return !matches!(self.capability(cap), CapabilitySupport::Blocked);
        }
        self.write_allowed && self.capability(cap).is_supported_family()
    }
}

/// 解析内置 manifest。
///
/// Business Logic: 进程启动/测试使用编译期合同。
/// Code Logic: 解析 `SUPPORT_MANIFEST_JSON`。
pub fn builtin_support_manifest() -> Result<SupportManifest, AppError> {
    load_support_manifest_from_str(SUPPORT_MANIFEST_JSON)
}

/// 从 JSON 字符串加载 manifest（测试可注入）。
///
/// Business Logic: 单元测试需要 fail-closed fixture，不能改 include_str 文件。
/// Code Logic: serde_json + 结构校验（target 可解析）。
pub fn load_support_manifest_from_str(json: &str) -> Result<SupportManifest, AppError> {
    let manifest: SupportManifest = serde_json::from_str(json).map_err(|e| {
        AppError::validation(format!("agent_hub_support_manifest_invalid_json: {e}"))
    })?;
    if manifest.schema_version == 0 {
        return Err(AppError::validation(
            "agent_hub_support_manifest_schema_version_zero",
        ));
    }
    for record in &manifest.targets {
        if record.executable_probe.command_names.is_empty() {
            return Err(AppError::validation(format!(
                "agent_hub_support_manifest_empty_command_names:{}",
                record.target.as_str()
            )));
        }
    }
    Ok(manifest)
}

/// 在 manifest 中查找 target 记录。
///
/// Business Logic: 缺失 target 视为无合同 → scan-only/blocked。
/// Code Logic: 线性查找；重复时返回第一个（checker 另验唯一）。
pub fn find_target_record(
    manifest: &SupportManifest,
    target: AgentTarget,
) -> Option<&TargetSupportRecord> {
    manifest.targets.iter().find(|r| r.target == target)
}

/// major/minor/patch 解析（容忍文档化前后缀）。
///
/// Business Logic: CLI 版本常带 `claude ` / `v` / 构建元数据；求值需要 exact core。
/// Code Logic: 剥离可选 prefix/suffix 后取首个 `X.Y.Z` 数字段；失败返回 None。
pub fn parse_semver_core(
    raw: &str,
    prefix: Option<&str>,
    suffix: Option<&str>,
) -> Option<(u64, u64, u64)> {
    let mut s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(p) = prefix {
        let p = p.trim();
        if !p.is_empty() {
            if let Some(rest) = s.strip_prefix(p) {
                s = rest.trim();
            }
        }
    }
    if let Some(suf) = suffix {
        let suf = suf.trim();
        if !suf.is_empty() {
            if let Some(rest) = s.strip_suffix(suf) {
                s = rest.trim();
            }
        }
    }
    // 去掉前导 'v'
    if let Some(rest) = s.strip_prefix('v').or_else(|| s.strip_prefix('V')) {
        s = rest.trim();
    }
    // 取第一个数字开头的 token（跳过产品名）
    let candidate = s
        .split_whitespace()
        .find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .unwrap_or(s);
    // 去掉 build/pre-release 后缀：1.2.3-beta+build → 1.2.3
    let core = candidate.split(['-', '+']).next().unwrap_or(candidate);
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    let patch = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    // 额外段忽略；若 major 解析失败已在上面返回
    if parts.next().is_some() {
        // 允许 1.2.3.4 形态：只取前三段
    }
    // 验证 core 至少以 digit 开头且包含合法点分
    if !core.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    // 拒绝含字母的 core（除已 strip 的 pre-release）
    if core.chars().any(|c| !(c.is_ascii_digit() || c == '.')) {
        return None;
    }
    Some((major, minor, patch))
}

/// 比较两个 semver core：返回 Ordering。
///
/// Business Logic: min ≤ current ≤ guarded 区间判断。
/// Code Logic: 元组比较。
fn cmp_semver(a: (u64, u64, u64), b: (u64, u64, u64)) -> std::cmp::Ordering {
    a.cmp(&b)
}

/// 对运行时 probe 求值 support。
///
/// Business Logic（为什么需要这个函数）:
///     缺 min/current、空 evidence、畸形 semver、fingerprint/help 不匹配时写能力 blocked，
///     扫描可保留 read-only；禁止猜测命令。
///
/// Code Logic（这个函数做什么）:
///     1) 查 record；缺失 → Blocked
///     2) 收集 fail-closed reasons（null 版本/空 evidence/畸形/区间/指纹）
///     3) 有 reasons → ScanOnly：scan 侧 ReadOnly，写侧 Blocked
///     4) 无 reasons → Certified：透传 manifest capabilities（未知 key 已在解析期拒绝）
pub fn evaluate_target_support(
    manifest: &SupportManifest,
    probe: &RuntimeProbeSnapshot,
) -> EvaluatedTargetSupport {
    let Some(record) = find_target_record(manifest, probe.target) else {
        return EvaluatedTargetSupport {
            target: probe.target,
            mode: EvaluatedSupportMode::Blocked {
                reasons: vec!["manifest_target_missing".into()],
            },
            capabilities: scan_only_capabilities(),
            write_allowed: false,
            reasons: vec!["manifest_target_missing".into()],
        };
    };

    let mut reasons: Vec<String> = Vec::new();

    // 版本字段缺失/空
    let min_raw = record
        .min_tested_version
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let current_raw = record
        .current_tested_version
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if min_raw.is_none() {
        reasons.push("min_tested_version_missing".into());
    }
    if current_raw.is_none() {
        reasons.push("current_tested_version_missing".into());
    }

    // evidence 为空
    if record.evidence_ids.is_empty() || record.evidence_ids.iter().all(|id| id.trim().is_empty()) {
        reasons.push("evidence_ids_empty".into());
    }

    // 解析 min/current
    let prefix = record.executable_probe.version_prefix.as_deref();
    let suffix = record.executable_probe.version_suffix.as_deref();
    let min_semver = min_raw.and_then(|v| parse_semver_core(v, prefix, suffix));
    let current_semver = current_raw.and_then(|v| parse_semver_core(v, prefix, suffix));
    if min_raw.is_some() && min_semver.is_none() {
        reasons.push("min_tested_version_malformed".into());
    }
    if current_raw.is_some() && current_semver.is_none() {
        reasons.push("current_tested_version_malformed".into());
    }
    if let (Some(min_v), Some(cur_v)) = (min_semver, current_semver) {
        if cmp_semver(min_v, cur_v) == std::cmp::Ordering::Greater {
            reasons.push("min_greater_than_current".into());
        }
    }

    // 运行时版本
    let runtime_raw = probe
        .version
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if runtime_raw.is_none() {
        reasons.push("runtime_version_missing".into());
    }
    let runtime_semver = runtime_raw.and_then(|v| parse_semver_core(v, prefix, suffix));
    if runtime_raw.is_some() && runtime_semver.is_none() {
        reasons.push("runtime_version_malformed".into());
    }

    if let (Some(min_v), Some(run_v)) = (min_semver, runtime_semver) {
        if cmp_semver(run_v, min_v) == std::cmp::Ordering::Less {
            reasons.push("runtime_below_min_tested".into());
        }
    }

    // guarded major：runtime major > guarded → 未知破坏
    if let (Some(run_v), Some(guard)) = (runtime_semver, record.guarded_major) {
        if run_v.0 > guard {
            reasons.push("runtime_above_guarded_major".into());
        }
    }

    // 可执行缺失
    if probe.executable.is_none() {
        reasons.push("executable_missing".into());
    }

    // help fingerprint mismatch（仅当 manifest 声明了期望值）
    if let Some(expected) = record
        .executable_probe
        .help_fingerprint
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        match probe
            .help_fingerprint
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            None => reasons.push("help_fingerprint_missing".into()),
            Some(actual) if !actual.eq_ignore_ascii_case(expected) => {
                reasons.push("help_fingerprint_mismatch".into());
            }
            Some(_) => {}
        }
    }

    // fingerprint 空视为无效
    if probe.fingerprint.trim().is_empty() {
        reasons.push("probe_fingerprint_missing".into());
    }

    // 配置根空
    if probe.config_root.as_os_str().is_empty() {
        reasons.push("config_root_missing".into());
    }

    // capabilities 中不得出现无法识别的——serde 已拒；此处补：若 map 为空也 fail
    if record.capabilities.is_empty() {
        reasons.push("capabilities_empty".into());
    }

    // 若有 fail-closed reasons → ScanOnly（写 blocked，scan ReadOnly）
    if !reasons.is_empty() {
        let caps = scan_only_capabilities();
        return EvaluatedTargetSupport {
            target: probe.target,
            mode: EvaluatedSupportMode::ScanOnly {
                reasons: reasons.clone(),
            },
            capabilities: caps,
            write_allowed: false,
            reasons,
        };
    }

    // Certified：透传 manifest 能力；写侧是否真正允许取决于是否存在 Supported* 写能力
    let mut caps = BTreeMap::new();
    for (cap, support) in &record.capabilities {
        caps.insert(*cap, *support);
    }
    // 确保所有已知 capability 有条目（缺省 blocked）
    for cap in ALL_CAPABILITIES {
        caps.entry(cap).or_insert(CapabilitySupport::Blocked);
    }
    let write_allowed = caps
        .iter()
        .any(|(cap, support)| cap.is_write_side() && support.is_supported_family());

    EvaluatedTargetSupport {
        target: probe.target,
        mode: EvaluatedSupportMode::Certified,
        capabilities: caps,
        write_allowed,
        reasons: vec![],
    }
}

/// 全部已知 capability 列表。
const ALL_CAPABILITIES: [TargetCapability; 7] = [
    TargetCapability::ScanInstruction,
    TargetCapability::RenderInstruction,
    TargetCapability::ScanPortableAssets,
    TargetCapability::RenderPortableAssets,
    TargetCapability::ActivatePackage,
    TargetCapability::DeactivatePackage,
    TargetCapability::LiveReload,
];

/// scan-only 能力表：scan 侧 ReadOnly，写侧 Blocked。
///
/// Business Logic: fail-closed 时仍可 inventory，禁止写/激活。
/// Code Logic: 固定映射。
fn scan_only_capabilities() -> BTreeMap<TargetCapability, CapabilitySupport> {
    let mut caps = BTreeMap::new();
    for cap in ALL_CAPABILITIES {
        let support = if cap.is_write_side() {
            CapabilitySupport::Blocked
        } else {
            CapabilitySupport::ReadOnly
        };
        caps.insert(cap, support);
    }
    caps
}

/// 用 manifest 记录与 runtime 字段构造期望 fingerprint 载荷（诊断用）。
///
/// Business Logic: 错误只打印版本/指纹，不打印资产正文。
/// Code Logic: 复用 targets::compute_probe_fingerprint 格式。
pub fn format_probe_identity(probe: &RuntimeProbeSnapshot) -> String {
    let exe = probe
        .executable
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<missing>".into());
    let ver = probe.version.as_deref().unwrap_or("<missing>");
    format!(
        "target={} version={} executable={} configRoot={} fingerprint={}",
        probe.target.as_str(),
        ver,
        exe,
        probe.config_root.to_string_lossy(),
        probe.fingerprint
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::targets::compute_probe_fingerprint;
    use std::path::PathBuf;

    /// 构造最小合法 manifest 文本。
    fn fixture_manifest_json(
        min: Option<&str>,
        current: Option<&str>,
        evidence: &[&str],
        help_fp: Option<&str>,
        caps_extra: &str,
    ) -> String {
        let min_json = min
            .map(|v| format!("\"{v}\""))
            .unwrap_or_else(|| "null".into());
        let cur_json = current
            .map(|v| format!("\"{v}\""))
            .unwrap_or_else(|| "null".into());
        let evidence_json = evidence
            .iter()
            .map(|e| format!("\"{e}\""))
            .collect::<Vec<_>>()
            .join(",");
        let help_json = help_fp
            .map(|v| format!("\"{v}\""))
            .unwrap_or_else(|| "null".into());
        format!(
            r#"{{
  "schemaVersion": 1,
  "targets": [
    {{
      "target": "claude",
      "adapterSchemaVersion": 1,
      "minTestedVersion": {min_json},
      "currentTestedVersion": {cur_json},
      "guardedMajor": 2,
      "executableProbe": {{
        "commandNames": ["claude"],
        "versionArgs": ["--version"],
        "versionPrefix": null,
        "versionSuffix": null,
        "helpFingerprint": {help_json}
      }},
      "activationCommandFingerprints": {{}},
      "capabilities": {{
        "scanInstruction": "supported",
        "renderInstruction": "supported",
        "scanPortableAssets": "supported",
        "renderPortableAssets": "supported",
        "activatePackage": "supported",
        "deactivatePackage": "supported",
        "liveReload": "blocked"
        {caps_extra}
      }},
      "evidenceIds": [{evidence_json}]
    }},
    {{
      "target": "codex",
      "adapterSchemaVersion": 1,
      "minTestedVersion": "1.0.0",
      "currentTestedVersion": "1.0.0",
      "guardedMajor": 1,
      "executableProbe": {{
        "commandNames": ["codex"],
        "versionArgs": ["--version"]
      }},
      "capabilities": {{
        "scanInstruction": "readOnly",
        "renderInstruction": "blocked",
        "scanPortableAssets": "readOnly",
        "renderPortableAssets": "blocked",
        "activatePackage": "blocked",
        "deactivatePackage": "blocked",
        "liveReload": "blocked"
      }},
      "evidenceIds": ["L3-AGENT-HUB-CODEX-001"]
    }},
    {{
      "target": "opencode",
      "adapterSchemaVersion": 1,
      "minTestedVersion": "1.0.0",
      "currentTestedVersion": "1.0.0",
      "guardedMajor": 1,
      "executableProbe": {{
        "commandNames": ["opencode"],
        "versionArgs": ["--version"]
      }},
      "capabilities": {{
        "scanInstruction": "readOnly",
        "renderInstruction": "blocked",
        "scanPortableAssets": "readOnly",
        "renderPortableAssets": "blocked",
        "activatePackage": "blocked",
        "deactivatePackage": "blocked",
        "liveReload": "blocked"
      }},
      "evidenceIds": ["L3-AGENT-HUB-OPENCODE-001"]
    }}
  ]
}}"#
        )
    }

    fn probe_at(version: Option<&str>, help: Option<&str>) -> RuntimeProbeSnapshot {
        let exe = PathBuf::from("/tmp/fake/claude");
        let config = PathBuf::from("/tmp/fake/.claude");
        let fingerprint =
            compute_probe_fingerprint("claude", Some(exe.as_path()), version, config.as_path());
        RuntimeProbeSnapshot {
            target: AgentTarget::Claude,
            executable: Some(exe),
            version: version.map(|s| s.to_string()),
            config_root: config,
            fingerprint,
            help_fingerprint: help.map(|s| s.to_string()),
        }
    }

    /// Business Logic: 内置 manifest 必须可解析且含三 target。
    #[test]
    fn builtin_manifest_loads_three_targets() {
        let m = builtin_support_manifest().expect("builtin");
        assert_eq!(m.schema_version, 1);
        assert_eq!(m.targets.len(), 3);
        let names: Vec<_> = m.targets.iter().map(|t| t.target.as_str()).collect();
        assert!(names.contains(&"claude"));
        assert!(names.contains(&"codex"));
        assert!(names.contains(&"opencode"));
    }

    /// Business Logic: null min/current → scan-only，写能力 blocked。
    #[test]
    fn null_versions_force_scan_only_and_block_writes() {
        let json = fixture_manifest_json(None, None, &["L3-AGENT-HUB-CLAUDE-001"], None, "");
        let m = load_support_manifest_from_str(&json).unwrap();
        let eval = evaluate_target_support(&m, &probe_at(Some("1.0.0"), None));
        assert!(matches!(eval.mode, EvaluatedSupportMode::ScanOnly { .. }));
        assert!(!eval.write_allowed);
        assert_eq!(
            eval.capability(TargetCapability::RenderInstruction),
            CapabilitySupport::Blocked
        );
        assert_eq!(
            eval.capability(TargetCapability::ScanInstruction),
            CapabilitySupport::ReadOnly
        );
        assert!(eval.reasons.iter().any(|r| r.contains("min_tested")));
        assert!(eval.reasons.iter().any(|r| r.contains("current_tested")));
    }

    /// Business Logic: 空 evidence → scan-only。
    #[test]
    fn empty_evidence_forces_scan_only() {
        let json = fixture_manifest_json(Some("1.0.0"), Some("1.0.0"), &[], None, "");
        let m = load_support_manifest_from_str(&json).unwrap();
        let eval = evaluate_target_support(&m, &probe_at(Some("1.0.0"), None));
        assert!(matches!(eval.mode, EvaluatedSupportMode::ScanOnly { .. }));
        assert!(eval.reasons.iter().any(|r| r == "evidence_ids_empty"));
        assert!(!eval.allows_write_capability(TargetCapability::ActivatePackage));
    }

    /// Business Logic: 畸形 semver → scan-only。
    #[test]
    fn malformed_semver_forces_scan_only() {
        let json = fixture_manifest_json(Some("not-a-version"), Some("1.0.0"), &["E1"], None, "");
        let m = load_support_manifest_from_str(&json).unwrap();
        let eval = evaluate_target_support(&m, &probe_at(Some("1.0.0"), None));
        assert!(matches!(eval.mode, EvaluatedSupportMode::ScanOnly { .. }));
        assert!(eval
            .reasons
            .iter()
            .any(|r| r == "min_tested_version_malformed"));
    }

    /// Business Logic: runtime 畸形版本 → scan-only。
    #[test]
    fn malformed_runtime_version_forces_scan_only() {
        let json = fixture_manifest_json(Some("1.0.0"), Some("1.0.0"), &["E1"], None, "");
        let m = load_support_manifest_from_str(&json).unwrap();
        let eval = evaluate_target_support(&m, &probe_at(Some("totally-broken"), None));
        assert!(matches!(eval.mode, EvaluatedSupportMode::ScanOnly { .. }));
        assert!(eval
            .reasons
            .iter()
            .any(|r| r == "runtime_version_malformed"));
    }

    /// Business Logic: 低于 min → scan-only。
    #[test]
    fn below_min_forces_scan_only() {
        let json = fixture_manifest_json(Some("2.0.0"), Some("2.1.0"), &["E1"], None, "");
        let m = load_support_manifest_from_str(&json).unwrap();
        let eval = evaluate_target_support(&m, &probe_at(Some("1.9.9"), None));
        assert!(matches!(eval.mode, EvaluatedSupportMode::ScanOnly { .. }));
        assert!(eval.reasons.iter().any(|r| r == "runtime_below_min_tested"));
    }

    /// Business Logic: 超过 guarded major → scan-only。
    #[test]
    fn above_guarded_major_forces_scan_only() {
        let json = fixture_manifest_json(Some("1.0.0"), Some("1.5.0"), &["E1"], None, "");
        let m = load_support_manifest_from_str(&json).unwrap();
        // guardedMajor in fixture is 2; major 3 is above
        let eval = evaluate_target_support(&m, &probe_at(Some("3.0.0"), None));
        assert!(matches!(eval.mode, EvaluatedSupportMode::ScanOnly { .. }));
        assert!(eval
            .reasons
            .iter()
            .any(|r| r == "runtime_above_guarded_major"));
    }

    /// Business Logic: help fingerprint 不匹配 → scan-only。
    #[test]
    fn help_fingerprint_mismatch_forces_scan_only() {
        let json = fixture_manifest_json(
            Some("1.0.0"),
            Some("1.0.0"),
            &["E1"],
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            "",
        );
        let m = load_support_manifest_from_str(&json).unwrap();
        let eval = evaluate_target_support(
            &m,
            &probe_at(
                Some("1.0.0"),
                Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            ),
        );
        assert!(matches!(eval.mode, EvaluatedSupportMode::ScanOnly { .. }));
        assert!(eval
            .reasons
            .iter()
            .any(|r| r == "help_fingerprint_mismatch"));
    }

    /// Business Logic: 可执行缺失 → scan-only。
    #[test]
    fn missing_executable_forces_scan_only() {
        let json = fixture_manifest_json(Some("1.0.0"), Some("1.0.0"), &["E1"], None, "");
        let m = load_support_manifest_from_str(&json).unwrap();
        let mut probe = probe_at(Some("1.0.0"), None);
        probe.executable = None;
        probe.fingerprint =
            compute_probe_fingerprint("claude", None, Some("1.0.0"), &probe.config_root);
        let eval = evaluate_target_support(&m, &probe);
        assert!(matches!(eval.mode, EvaluatedSupportMode::ScanOnly { .. }));
        assert!(eval.reasons.iter().any(|r| r == "executable_missing"));
    }

    /// Business Logic: 齐备合同 + 匹配版本 → Certified 且写能力按 manifest 开放。
    #[test]
    fn certified_when_versions_and_evidence_match() {
        let json = fixture_manifest_json(Some("1.0.0"), Some("1.2.0"), &["E1"], None, "");
        let m = load_support_manifest_from_str(&json).unwrap();
        let eval = evaluate_target_support(&m, &probe_at(Some("1.1.0"), None));
        assert_eq!(eval.mode, EvaluatedSupportMode::Certified);
        assert!(eval.write_allowed);
        assert_eq!(
            eval.capability(TargetCapability::ActivatePackage),
            CapabilitySupport::Supported
        );
        assert!(eval.allows_write_capability(TargetCapability::RenderPortableAssets));
    }

    /// Business Logic: 未知 capability JSON key 必须解析失败（fail-closed）。
    #[test]
    fn unknown_capability_key_rejected_at_parse() {
        let json = fixture_manifest_json(
            Some("1.0.0"),
            Some("1.0.0"),
            &["E1"],
            None,
            r#", "totallyUnknownCap": "supported""#,
        );
        let err = load_support_manifest_from_str(&json).expect_err("unknown cap");
        let msg = err.to_string();
        assert!(
            msg.contains("agent_hub_support_manifest_invalid_json") || msg.contains("unknown"),
            "unexpected err: {msg}"
        );
    }

    /// Business Logic: 缺失 target 记录 → Blocked。
    #[test]
    fn missing_target_record_is_blocked() {
        let json = r#"{
          "schemaVersion": 1,
          "targets": []
        }"#;
        let m = load_support_manifest_from_str(json).unwrap();
        let eval = evaluate_target_support(&m, &probe_at(Some("1.0.0"), None));
        assert!(matches!(eval.mode, EvaluatedSupportMode::Blocked { .. }));
        assert!(!eval.write_allowed);
    }

    /// Business Logic: semver 解析容忍 prefix 与 pre-release。
    #[test]
    fn parse_semver_tolerates_prefix_and_prerelease() {
        assert_eq!(
            parse_semver_core("claude 1.2.3", Some("claude "), None),
            Some((1, 2, 3))
        );
        assert_eq!(
            parse_semver_core("v2.0.1-beta.1", None, None),
            Some((2, 0, 1))
        );
        assert_eq!(
            parse_semver_core("1.0.0+build.9", None, None),
            Some((1, 0, 0))
        );
        assert_eq!(parse_semver_core("", None, None), None);
        assert_eq!(parse_semver_core("not-semver", None, None), None);
    }

    /// Business Logic: 诊断字符串只含版本/指纹元数据。
    #[test]
    fn format_probe_identity_has_no_asset_body() {
        let id = format_probe_identity(&probe_at(Some("1.2.3"), None));
        assert!(id.contains("version=1.2.3"));
        assert!(id.contains("fingerprint="));
        assert!(!id.contains("SKILL.md"));
        assert!(!id.contains("Bearer"));
    }

    /// Business Logic: 编译期 JSON 与 runtime 解析一致且不可变常量。
    #[test]
    fn include_str_bytes_are_stable() {
        assert!(SUPPORT_MANIFEST_JSON.contains("\"schemaVersion\""));
        assert!(SUPPORT_MANIFEST_JSON.contains("\"claude\""));
        let m = builtin_support_manifest().unwrap();
        // 写能力在仓库基线应保持 blocked，直到 L3/CI 认证
        for t in &m.targets {
            for (cap, support) in &t.capabilities {
                if cap.is_write_side() {
                    assert_eq!(
                        *support,
                        CapabilitySupport::Blocked,
                        "{} write cap not blocked in baseline",
                        t.target.as_str()
                    );
                }
            }
        }
    }
}
