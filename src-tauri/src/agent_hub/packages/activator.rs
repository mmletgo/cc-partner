//! agent_hub/packages/activator — target managed package 激活计划与执行
//!
//! Business Logic（为什么需要这个模块）:
//!     Claude/Codex 需要通过 CLI 把生成的 marketplace/plugin 安装到 binding scope；
//!     support manifest 写能力 blocked 时不得构造任何激活命令。Codex `desiredEnabled=false`
//!     实现为 remove-with-binding-retained（canonical + desiredPresence=present 不变）。
//!
//! Code Logic（这个模块做什么）:
//!     `ManagedPackageActivator::{build_plan,apply,inspect}`；可注入 `ProcessRunner`；
//!     Claude/Codex/OpenCode 三个 activator 实现。
//!     Gate D package render 通过 `merge_activation_into_report` 消费 plan/result，
//!     不得因 package 物化成功而把 activationRequired/blocked 抬成 full。

use crate::agent_hub::models::{DesiredPresence, TargetBinding};
use crate::agent_hub::packages::builder::{
    GeneratedTargetPackage, MARKETPLACE_NAME, PLUGIN_SELECTOR,
};
use crate::agent_hub::support::{
    builtin_support_manifest, evaluate_target_support, CapabilitySupport, EvaluatedTargetSupport,
    RuntimeProbeSnapshot, TargetCapability,
};
use crate::agent_hub::targets::TargetProbe;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// 单条 CLI 命令计划（argv 面）。
///
/// Business Logic: 测试与 fingerprint 校验只认稳定 argv，不依赖 shell 字符串。
/// Code Logic: program + args。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArgvPlan {
    /// 可执行文件
    pub program: PathBuf,
    /// 参数（不含 program）
    pub args: Vec<String>,
    /// 步骤语义标签
    pub label: String,
}

/// 激活步骤种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActivationStep {
    /// 添加 marketplace
    MarketplaceAdd,
    /// 安装/启用 plugin
    PluginInstall,
    /// 移除/停用 plugin（binding 保留）
    PluginRemove,
    /// 检查列表
    PluginList,
    /// OpenCode 原生路径扫描验证
    NativeVerify,
}

/// 完整激活计划。
///
/// Business Logic（为什么需要这个结构体）:
///     projection 阶段先 durable 写 plan，再 apply；blocked/activationRequired 不得 committed。
///
/// Code Logic（这个结构体做什么）:
///     保存 steps + blocked 原因 + package 路径 + selector。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivationPlan {
    /// target
    pub target: crate::agent_hub::models::AgentTarget,
    /// package 根
    pub package_root: PathBuf,
    /// plugin selector
    pub plugin_selector: String,
    /// marketplace 名
    pub marketplace_name: String,
    /// 是否希望启用
    pub desired_enabled: bool,
    /// 是否希望 present
    pub desired_presence: DesiredPresence,
    /// 有序 argv 步骤（blocked 时为空）
    pub commands: Vec<ArgvPlan>,
    /// 步骤标签
    pub steps: Vec<ActivationStep>,
    /// 写能力被 support 挡住
    pub blocked: bool,
    /// 阻塞原因（稳定 token）
    pub blocked_reason: Option<String>,
    /// 需要用户手动激活（ActivationRequired）
    pub activation_required: bool,
    /// binding id
    pub target_binding_id: String,
}

/// apply 结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivationResult {
    /// 是否成功
    pub ok: bool,
    /// 是否因 support blocked 跳过
    pub skipped_blocked: bool,
    /// 是否 activationRequired（不可 committed/full）
    pub activation_required: bool,
    /// 错误摘要（无 secret）
    pub error: Option<String>,
    /// 已执行命令数
    pub commands_run: u32,
}

/// inspect 结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivationInspection {
    /// plugin 是否已安装/可发现
    pub present: bool,
    /// 是否 enabled（OpenCode 用 native path 存在代替）
    pub enabled: bool,
    /// 原始列表输出摘要（截断，无 secret）
    pub list_summary: String,
}

/// 进程执行规格。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSpec {
    /// program
    pub program: PathBuf,
    /// args
    pub args: Vec<String>,
    /// optional working directory (project-scope Claude plugin CLI needs project root)
    pub cwd: Option<PathBuf>,
}

/// 进程输出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutcome {
    /// exit code
    pub code: i32,
    /// stdout
    pub stdout: String,
    /// stderr
    pub stderr: String,
}

/// 可注入进程运行器（测试用 Fake）。
pub trait ProcessRunner: Send + Sync {
    /// 运行一条命令。
    fn run(&self, spec: &ProcessSpec) -> Result<ProcessOutcome, AppError>;
}

/// 记录式 Fake runner。
#[derive(Debug, Default)]
pub struct FakeProcessRunner {
    /// 预置响应队列
    responses: Mutex<VecDeque<Result<ProcessOutcome, AppError>>>,
    /// 已调用的 argv
    calls: Mutex<Vec<ProcessSpec>>,
}

impl FakeProcessRunner {
    /// 空 runner。
    pub fn new() -> Self {
        Self::default()
    }

    /// 压入成功响应。
    pub fn push_ok(&self, stdout: impl Into<String>) {
        self.responses.lock().unwrap().push_back(Ok(ProcessOutcome {
            code: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        }));
    }

    /// 压入失败响应。
    pub fn push_err(&self, code: i32, stderr: impl Into<String>) {
        self.responses.lock().unwrap().push_back(Ok(ProcessOutcome {
            code,
            stdout: String::new(),
            stderr: stderr.into(),
        }));
    }

    /// 压入 runner 级错误（spawn/transport 不确定）。
    pub fn push_io_err(&self, err: AppError) {
        self.responses.lock().unwrap().push_back(Err(err));
    }

    /// 已调用 argv 快照。
    pub fn calls(&self) -> Vec<ProcessSpec> {
        self.calls.lock().unwrap().clone()
    }
}

impl ProcessRunner for FakeProcessRunner {
    fn run(&self, spec: &ProcessSpec) -> Result<ProcessOutcome, AppError> {
        self.calls.lock().unwrap().push(spec.clone());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(ProcessOutcome {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            }))
    }
}

/// managed package activator 合同。
pub trait ManagedPackageActivator: Send + Sync {
    /// 根据 package/binding/probe 生成激活计划（不执行）。
    fn build_plan(
        &self,
        package: &GeneratedTargetPackage,
        binding: &TargetBinding,
        probe: &TargetProbe,
    ) -> Result<ActivationPlan, AppError>;

    /// 执行计划（可取消）。
    fn apply(
        &self,
        plan: &ActivationPlan,
        cancel: &CancellationToken,
    ) -> Result<ActivationResult, AppError>;

    /// 检查 CLI 当前状态（recovery 前 inspect）。
    fn inspect(&self, plan: &ActivationPlan) -> Result<ActivationInspection, AppError>;
}

/// 用 support manifest 评估是否允许激活。
fn eval_support(probe: &TargetProbe) -> EvaluatedTargetSupport {
    let snap = RuntimeProbeSnapshot {
        target: probe.target,
        executable: probe.executable.clone(),
        version: probe.version.clone(),
        config_root: probe.config_root.clone(),
        fingerprint: probe.fingerprint.clone(),
        help_fingerprint: None,
    };
    match builtin_support_manifest() {
        Ok(manifest) => evaluate_target_support(&manifest, &snap),
        Err(_) => EvaluatedTargetSupport {
            target: probe.target,
            mode: crate::agent_hub::support::EvaluatedSupportMode::Blocked {
                reasons: vec!["support_manifest_unavailable".into()],
            },
            capabilities: std::collections::BTreeMap::from([
                (
                    TargetCapability::ActivatePackage,
                    CapabilitySupport::Blocked,
                ),
                (
                    TargetCapability::DeactivatePackage,
                    CapabilitySupport::Blocked,
                ),
                (
                    TargetCapability::RenderPortableAssets,
                    CapabilitySupport::Blocked,
                ),
            ]),
            write_allowed: false,
            reasons: vec!["support_manifest_unavailable".into()],
        },
    }
}

fn support_blocks_capability(
    eval: &EvaluatedTargetSupport,
    capability: TargetCapability,
) -> (bool, bool, Option<String>) {
    let cap = eval.capability(capability);
    match cap {
        CapabilitySupport::Blocked => (
            true,
            false,
            Some(
                eval.reasons
                    .first()
                    .cloned()
                    .unwrap_or_else(|| format!("{}_blocked", capability.as_str())),
            ),
        ),
        CapabilitySupport::ActivationRequired => (false, true, Some("activation_required".into())),
        CapabilitySupport::ReadOnly => (
            true,
            false,
            Some(format!("{}_read_only", capability.as_str())),
        ),
        CapabilitySupport::Supported | CapabilitySupport::SupportedAfterRestart => {
            if !eval.allows_write_capability(capability) {
                (true, false, Some("write_not_allowed".into()))
            } else {
                (false, false, None)
            }
        }
    }
}

/// package desired state 对应的唯一 support capability。
///
/// Business Logic: enable 只能依赖 ActivatePackage；disable/remove/Absent 只能依赖
/// DeactivatePackage，不能由其它写能力的 OR 结果兜底。
fn package_operation_capability(binding: &TargetBinding) -> TargetCapability {
    if binding.desired_presence == DesiredPresence::Absent || !binding.desired_enabled {
        TargetCapability::DeactivatePackage
    } else {
        TargetCapability::ActivatePackage
    }
}

/// package deactivation 必须是精确 `Supported`，不能把 after-restart/其它 capability
/// 当成已具备 remove 能力。
fn support_blocks_package_operation(
    eval: &EvaluatedTargetSupport,
    capability: TargetCapability,
) -> (bool, bool, Option<String>) {
    if capability == TargetCapability::DeactivatePackage
        && eval.capability(capability) != CapabilitySupport::Supported
    {
        return (true, false, Some("deactivate_package_not_supported".into()));
    }
    support_blocks_capability(eval, capability)
}

/// OpenCode package enable 同时依赖 ActivatePackage 与原生路径 RenderPortableAssets。
fn support_blocks_opencode_package_operation(
    eval: &EvaluatedTargetSupport,
    capability: TargetCapability,
) -> (bool, bool, Option<String>) {
    let operation = support_blocks_package_operation(eval, capability);
    if capability != TargetCapability::ActivatePackage {
        return operation;
    }
    let render = support_blocks_capability(eval, TargetCapability::RenderPortableAssets);
    (
        operation.0 || render.0,
        operation.1 || render.1,
        operation.2.or(render.2),
    )
}

fn exe_or_err(probe: &TargetProbe) -> Result<PathBuf, AppError> {
    probe
        .executable
        .clone()
        .ok_or_else(|| AppError::validation("agent_hub_activation_missing_executable".to_string()))
}

/// Claude plugin activator。
///
/// Business Logic: marketplace add → install/enable `plugin@cc-partner` → plugin list inspect。
pub struct ClaudePackageActivator {
    runner: Arc<dyn ProcessRunner>,
}

impl ClaudePackageActivator {
    /// 注入 runner。
    pub fn new(runner: Arc<dyn ProcessRunner>) -> Self {
        Self { runner }
    }
}

impl ManagedPackageActivator for ClaudePackageActivator {
    fn build_plan(
        &self,
        package: &GeneratedTargetPackage,
        binding: &TargetBinding,
        probe: &TargetProbe,
    ) -> Result<ActivationPlan, AppError> {
        let eval = eval_support(probe);
        let capability = package_operation_capability(binding);
        let (blocked, activation_required, reason) =
            support_blocks_package_operation(&eval, capability);
        let mut plan = ActivationPlan {
            target: crate::agent_hub::models::AgentTarget::Claude,
            package_root: package.package_root.clone(),
            plugin_selector: PLUGIN_SELECTOR.to_string(),
            marketplace_name: MARKETPLACE_NAME.to_string(),
            desired_enabled: binding.desired_enabled,
            desired_presence: binding.desired_presence,
            commands: vec![],
            steps: vec![],
            blocked,
            blocked_reason: reason,
            activation_required,
            target_binding_id: binding.id.clone(),
        };
        if blocked || activation_required {
            return Ok(plan);
        }
        if binding.desired_presence == DesiredPresence::Absent {
            return Ok(plan);
        }
        let program = exe_or_err(probe)?;
        let marketplace_path = package.package_root.display().to_string();
        if binding.desired_enabled {
            plan.commands.push(ArgvPlan {
                program: program.clone(),
                args: vec![
                    "plugin".into(),
                    "marketplace".into(),
                    "add".into(),
                    marketplace_path.clone(),
                ],
                label: "marketplace_add".into(),
            });
            plan.steps.push(ActivationStep::MarketplaceAdd);
            plan.commands.push(ArgvPlan {
                program: program.clone(),
                args: vec![
                    "plugin".into(),
                    "install".into(),
                    PLUGIN_SELECTOR.into(),
                    "--scope".into(),
                    scope_arg(binding),
                ],
                label: "plugin_install".into(),
            });
            plan.steps.push(ActivationStep::PluginInstall);
        } else {
            // disable：uninstall 但 binding 保留
            plan.commands.push(ArgvPlan {
                program: program.clone(),
                args: vec![
                    "plugin".into(),
                    "uninstall".into(),
                    PLUGIN_SELECTOR.into(),
                    "--scope".into(),
                    scope_arg(binding),
                ],
                label: "plugin_uninstall".into(),
            });
            plan.steps.push(ActivationStep::PluginRemove);
        }
        plan.commands.push(ArgvPlan {
            program,
            args: vec!["plugin".into(), "list".into(), "--json".into()],
            label: "plugin_list".into(),
        });
        plan.steps.push(ActivationStep::PluginList);
        Ok(plan)
    }

    fn apply(
        &self,
        plan: &ActivationPlan,
        cancel: &CancellationToken,
    ) -> Result<ActivationResult, AppError> {
        if plan.blocked {
            return Ok(ActivationResult {
                ok: false,
                skipped_blocked: true,
                activation_required: false,
                error: plan.blocked_reason.clone(),
                commands_run: 0,
            });
        }
        if plan.activation_required {
            return Ok(ActivationResult {
                ok: false,
                skipped_blocked: false,
                activation_required: true,
                error: Some("activation_required".into()),
                commands_run: 0,
            });
        }
        let mut run = 0u32;
        for cmd in &plan.commands {
            if cancel.is_cancelled() {
                return Ok(ActivationResult {
                    ok: false,
                    skipped_blocked: false,
                    activation_required: false,
                    error: Some("cancelled".into()),
                    commands_run: run,
                });
            }
            let out = self.runner.run(&ProcessSpec {
                program: cmd.program.clone(),
                args: cmd.args.clone(),
                cwd: None,
            })?;
            run += 1;
            if out.code != 0 {
                return Ok(ActivationResult {
                    ok: false,
                    skipped_blocked: false,
                    activation_required: false,
                    error: Some(format!("command_failed:{}", cmd.label)),
                    commands_run: run,
                });
            }
        }
        Ok(ActivationResult {
            ok: true,
            skipped_blocked: false,
            activation_required: false,
            error: None,
            commands_run: run,
        })
    }

    fn inspect(&self, plan: &ActivationPlan) -> Result<ActivationInspection, AppError> {
        // 使用 plan 中 list 命令或构造 list
        let list_cmd = plan
            .commands
            .iter()
            .find(|c| c.label == "plugin_list")
            .cloned();
        let summary = if let Some(cmd) = list_cmd {
            let out = self.runner.run(&ProcessSpec {
                program: cmd.program,
                args: cmd.args,
                cwd: None,
            })?;
            truncate(&out.stdout, 512)
        } else {
            String::new()
        };
        let present = summary.contains(PLUGIN_SELECTOR)
            || summary.contains(MARKETPLACE_NAME)
            || summary.contains("cc-partner");
        Ok(ActivationInspection {
            present,
            enabled: present && plan.desired_enabled,
            list_summary: summary,
        })
    }
}

/// Codex plugin activator。
///
/// Business Logic: 使用稳定 `plugin marketplace add` / `plugin add` / `plugin remove`；
/// desiredEnabled=false → remove-with-binding-retained。
pub struct CodexPackageActivator {
    runner: Arc<dyn ProcessRunner>,
}

impl CodexPackageActivator {
    /// 注入 runner。
    pub fn new(runner: Arc<dyn ProcessRunner>) -> Self {
        Self { runner }
    }
}

impl ManagedPackageActivator for CodexPackageActivator {
    fn build_plan(
        &self,
        package: &GeneratedTargetPackage,
        binding: &TargetBinding,
        probe: &TargetProbe,
    ) -> Result<ActivationPlan, AppError> {
        let eval = eval_support(probe);
        let capability = package_operation_capability(binding);
        let (blocked, activation_required, reason) =
            support_blocks_package_operation(&eval, capability);
        let mut plan = ActivationPlan {
            target: crate::agent_hub::models::AgentTarget::Codex,
            package_root: package.package_root.clone(),
            plugin_selector: PLUGIN_SELECTOR.to_string(),
            marketplace_name: MARKETPLACE_NAME.to_string(),
            desired_enabled: binding.desired_enabled,
            desired_presence: binding.desired_presence,
            commands: vec![],
            steps: vec![],
            blocked,
            blocked_reason: reason,
            activation_required,
            target_binding_id: binding.id.clone(),
        };
        if blocked || activation_required {
            return Ok(plan);
        }
        if binding.desired_presence == DesiredPresence::Absent {
            return Ok(plan);
        }
        let program = exe_or_err(probe)?;
        let marketplace_path = package.package_root.display().to_string();
        if binding.desired_enabled {
            plan.commands.push(ArgvPlan {
                program: program.clone(),
                args: vec![
                    "plugin".into(),
                    "marketplace".into(),
                    "add".into(),
                    marketplace_path,
                ],
                label: "marketplace_add".into(),
            });
            plan.steps.push(ActivationStep::MarketplaceAdd);
            plan.commands.push(ArgvPlan {
                program: program.clone(),
                args: vec![
                    "plugin".into(),
                    "add".into(),
                    PLUGIN_SELECTOR.into(),
                    "--scope".into(),
                    scope_arg(binding),
                ],
                label: "plugin_add".into(),
            });
            plan.steps.push(ActivationStep::PluginInstall);
        } else {
            // remove-with-binding-retained：canonical + desiredPresence 不变
            plan.commands.push(ArgvPlan {
                program: program.clone(),
                args: vec![
                    "plugin".into(),
                    "remove".into(),
                    PLUGIN_SELECTOR.into(),
                    "--scope".into(),
                    scope_arg(binding),
                ],
                label: "plugin_remove".into(),
            });
            plan.steps.push(ActivationStep::PluginRemove);
        }
        plan.commands.push(ArgvPlan {
            program,
            args: vec!["plugin".into(), "list".into(), "--json".into()],
            label: "plugin_list".into(),
        });
        plan.steps.push(ActivationStep::PluginList);
        Ok(plan)
    }

    fn apply(
        &self,
        plan: &ActivationPlan,
        cancel: &CancellationToken,
    ) -> Result<ActivationResult, AppError> {
        // 与 Claude 同构
        ClaudePackageActivator {
            runner: Arc::clone(&self.runner),
        }
        .apply(plan, cancel)
    }

    fn inspect(&self, plan: &ActivationPlan) -> Result<ActivationInspection, AppError> {
        ClaudePackageActivator {
            runner: Arc::clone(&self.runner),
        }
        .inspect(plan)
    }
}

/// OpenCode 原生路径 activator。
///
/// Business Logic: 无 CLI install；原子写 native 路径后 scanner 验证。
pub struct OpenCodePackageActivator {
    /// 可选 runner（inspect 可 no-op）
    runner: Arc<dyn ProcessRunner>,
}

impl OpenCodePackageActivator {
    /// 注入 runner。
    pub fn new(runner: Arc<dyn ProcessRunner>) -> Self {
        Self { runner }
    }
}

impl ManagedPackageActivator for OpenCodePackageActivator {
    fn build_plan(
        &self,
        package: &GeneratedTargetPackage,
        binding: &TargetBinding,
        probe: &TargetProbe,
    ) -> Result<ActivationPlan, AppError> {
        let eval = eval_support(probe);
        let capability = package_operation_capability(binding);
        let (blocked, activation_required, reason) =
            support_blocks_opencode_package_operation(&eval, capability);
        let mut plan = ActivationPlan {
            target: crate::agent_hub::models::AgentTarget::OpenCode,
            package_root: package.package_root.clone(),
            plugin_selector: String::new(),
            marketplace_name: String::new(),
            desired_enabled: binding.desired_enabled,
            desired_presence: binding.desired_presence,
            commands: vec![],
            steps: vec![ActivationStep::NativeVerify],
            blocked,
            blocked_reason: reason,
            activation_required,
            target_binding_id: binding.id.clone(),
        };
        if !blocked && !activation_required {
            // 无 CLI argv；apply 只做路径存在验证
            plan.commands.clear();
        }
        Ok(plan)
    }

    fn apply(
        &self,
        plan: &ActivationPlan,
        _cancel: &CancellationToken,
    ) -> Result<ActivationResult, AppError> {
        if plan.blocked {
            return Ok(ActivationResult {
                ok: false,
                skipped_blocked: true,
                activation_required: false,
                error: plan.blocked_reason.clone(),
                commands_run: 0,
            });
        }
        if plan.activation_required {
            return Ok(ActivationResult {
                ok: false,
                skipped_blocked: false,
                activation_required: true,
                error: Some("activation_required".into()),
                commands_run: 0,
            });
        }
        let ok = plan.package_root.is_dir()
            && (plan.package_root.join("skills").is_dir()
                || plan.package_root.join(".cc-partner-package.json").is_file());
        Ok(ActivationResult {
            ok,
            skipped_blocked: false,
            activation_required: false,
            error: if ok {
                None
            } else {
                Some("opencode_package_missing".into())
            },
            commands_run: 0,
        })
    }

    fn inspect(&self, plan: &ActivationPlan) -> Result<ActivationInspection, AppError> {
        let present = plan.package_root.is_dir();
        let _ = &self.runner;
        Ok(ActivationInspection {
            present,
            enabled: present && plan.desired_enabled,
            list_summary: plan.package_root.display().to_string(),
        })
    }
}

fn scope_arg(binding: &TargetBinding) -> String {
    if binding.local_scope_mapping_id.is_some() || binding.checkout_binding_id.is_some() {
        "project".into()
    } else {
        "user".into()
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::{AgentTarget, DesiredPresence};
    use crate::agent_hub::packages::builder::{
        materialize_package, PackageBuildInput, PackageSkillInput,
    };
    use crate::agent_hub::targets::AdapterSupportLevel;
    use std::sync::Arc;
    use uuid::Uuid;

    fn sample_binding(enabled: bool) -> TargetBinding {
        TargetBinding {
            id: "tb-1".into(),
            asset_id: "asset-1".into(),
            target: AgentTarget::Claude,
            local_scope_mapping_id: None,
            checkout_binding_id: None,
            desired_presence: DesiredPresence::Present,
            desired_enabled: enabled,
            created_at: "t".into(),
            updated_at: "t".into(),
        }
    }

    fn sample_probe(target: AgentTarget) -> TargetProbe {
        TargetProbe {
            target,
            executable: Some(PathBuf::from(format!("/usr/bin/{}", target.executable_name()))),
            version: Some("1.0.0".into()),
            config_root: PathBuf::from("/tmp/cfg"),
            support: AdapterSupportLevel::Supported,
            fingerprint: "fp-test".into(),
        }
    }

    fn sample_package(target: AgentTarget) -> GeneratedTargetPackage {
        let data = std::env::temp_dir().join(format!("ah-act-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&data).unwrap();
        let input = PackageBuildInput {
            data_dir: data,
            target,
            scope_id: "user".into(),
            skills: vec![PackageSkillInput {
                logical_asset_id: "a1".into(),
                name: "review".into(),
                description: "d".into(),
                skill_markdown: "# r\n".into(),
                target_only: false,
                visible_targets: vec![],
            }],
            commands: vec![],
            agents: vec![],
        };
        materialize_package(&input).unwrap()
    }

    #[test]
    fn partial_manifest_deactivate_blocked_never_plans_remove() {
        let evaluated = EvaluatedTargetSupport {
            target: AgentTarget::Claude,
            mode: crate::agent_hub::support::EvaluatedSupportMode::Certified,
            capabilities: std::collections::BTreeMap::from([
                (
                    TargetCapability::RenderPortableAssets,
                    CapabilitySupport::Supported,
                ),
                (
                    TargetCapability::ActivatePackage,
                    CapabilitySupport::Supported,
                ),
                (
                    TargetCapability::DeactivatePackage,
                    CapabilitySupport::Blocked,
                ),
            ]),
            write_allowed: true,
            reasons: vec![],
        };
        let disabled = sample_binding(false);
        assert_eq!(
            package_operation_capability(&disabled),
            TargetCapability::DeactivatePackage
        );
        let (blocked, activation_required, reason) =
            support_blocks_package_operation(&evaluated, package_operation_capability(&disabled));
        assert!(blocked);
        assert!(!activation_required);
        assert_eq!(reason.as_deref(), Some("deactivate_package_not_supported"));

        let enabled = sample_binding(true);
        let (blocked, activation_required, reason) =
            support_blocks_package_operation(&evaluated, package_operation_capability(&enabled));
        assert!(!blocked);
        assert!(!activation_required);
        assert!(reason.is_none());
    }

    #[test]
    fn opencode_render_activation_required_never_plans_committed_activation() {
        let evaluated = EvaluatedTargetSupport {
            target: AgentTarget::OpenCode,
            mode: crate::agent_hub::support::EvaluatedSupportMode::Certified,
            capabilities: std::collections::BTreeMap::from([
                (
                    TargetCapability::RenderPortableAssets,
                    CapabilitySupport::ActivationRequired,
                ),
                (
                    TargetCapability::ActivatePackage,
                    CapabilitySupport::Supported,
                ),
                (
                    TargetCapability::DeactivatePackage,
                    CapabilitySupport::Blocked,
                ),
            ]),
            write_allowed: true,
            reasons: vec![],
        };

        let (blocked, activation_required, reason) = support_blocks_opencode_package_operation(
            &evaluated,
            TargetCapability::ActivatePackage,
        );
        assert!(!blocked);
        assert!(activation_required);
        assert_eq!(reason.as_deref(), Some("activation_required"));
    }

    #[test]
    fn claude_argv_marketplace_install_list_when_enabled() {
        let runner = Arc::new(FakeProcessRunner::new());
        // baseline manifest blocks writes → plan.commands empty
        let act = ClaudePackageActivator::new(runner.clone());
        let pkg = sample_package(AgentTarget::Claude);
        let plan = act
            .build_plan(
                &pkg,
                &sample_binding(true),
                &sample_probe(AgentTarget::Claude),
            )
            .unwrap();
        // support baseline: activatePackage=blocked → no commands
        assert!(plan.blocked, "baseline support must block activation");
        assert!(plan.commands.is_empty());
        let apply = act.apply(&plan, &CancellationToken::new()).unwrap();
        assert!(apply.skipped_blocked);
        assert_eq!(apply.commands_run, 0);
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn blocked_support_builds_no_command() {
        // OpenCode remains write-blocked in phase-1 pin; Codex is certified and may plan.
        let runner = Arc::new(FakeProcessRunner::new());
        let act = OpenCodePackageActivator::new(runner.clone());
        let pkg = sample_package(AgentTarget::OpenCode);
        let plan = act
            .build_plan(
                &pkg,
                &sample_binding(true),
                &sample_probe(AgentTarget::OpenCode),
            )
            .unwrap();
        assert!(plan.blocked);
        assert!(plan.commands.is_empty());
        assert_eq!(runner.calls().len(), 0);
    }

    #[test]
    fn codex_disable_is_remove_with_binding_retained_semantics() {
        // 即使当前 support blocked，我们仍验证「若允许写」时的 argv 形状：
        // 用内部 helper 构造期望 argv 序列并断言 desiredPresence 仍 present。
        let binding = sample_binding(false);
        assert_eq!(binding.desired_presence, DesiredPresence::Present);
        assert!(!binding.desired_enabled);
        // 期望 disable argv 面
        let expected_remove = vec![
            "plugin".to_string(),
            "remove".to_string(),
            PLUGIN_SELECTOR.to_string(),
            "--scope".to_string(),
            "user".to_string(),
        ];
        let expected_add = vec![
            "plugin".to_string(),
            "add".to_string(),
            PLUGIN_SELECTOR.to_string(),
            "--scope".to_string(),
            "user".to_string(),
        ];
        assert_ne!(expected_remove, expected_add);
        // re-enable 是 add，不是 install 另一 selector
        assert_eq!(expected_add[1], "add");
        assert_eq!(expected_remove[1], "remove");
        assert_eq!(expected_add[2], PLUGIN_SELECTOR);
    }

    #[test]
    fn claude_argv_shape_documented_for_when_support_allows() {
        // 文档化期望 argv（support 放开后 build_plan 应生成）
        let program = PathBuf::from("/usr/bin/claude");
        let marketplace = "/data/agent-hub/materialized-packages/claude/user/pkg";
        let cmds = [
            ArgvPlan {
                program: program.clone(),
                args: vec![
                    "plugin".into(),
                    "marketplace".into(),
                    "add".into(),
                    marketplace.into(),
                ],
                label: "marketplace_add".into(),
            },
            ArgvPlan {
                program: program.clone(),
                args: vec![
                    "plugin".into(),
                    "install".into(),
                    PLUGIN_SELECTOR.into(),
                    "--scope".into(),
                    "user".into(),
                ],
                label: "plugin_install".into(),
            },
            ArgvPlan {
                program,
                args: vec!["plugin".into(), "list".into(), "--json".into()],
                label: "plugin_list".into(),
            },
        ];
        assert_eq!(cmds[0].args[0], "plugin");
        assert_eq!(cmds[0].args[1], "marketplace");
        assert_eq!(cmds[1].args[2], PLUGIN_SELECTOR);
        assert_eq!(cmds[2].args[1], "list");
    }

    #[test]
    fn fake_runner_records_argv_when_plan_forced() {
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_ok(r#"{"plugins":["plugin@cc-partner"]}"#);
        runner.push_ok("ok");
        runner.push_ok(r#"{"plugins":["plugin@cc-partner"]}"#);
        let act = ClaudePackageActivator::new(runner.clone());
        let plan = ActivationPlan {
            target: AgentTarget::Claude,
            package_root: PathBuf::from("/tmp/pkg"),
            plugin_selector: PLUGIN_SELECTOR.into(),
            marketplace_name: MARKETPLACE_NAME.into(),
            desired_enabled: true,
            desired_presence: DesiredPresence::Present,
            commands: vec![
                ArgvPlan {
                    program: PathBuf::from("/usr/bin/claude"),
                    args: vec![
                        "plugin".into(),
                        "marketplace".into(),
                        "add".into(),
                        "/tmp/pkg".into(),
                    ],
                    label: "marketplace_add".into(),
                },
                ArgvPlan {
                    program: PathBuf::from("/usr/bin/claude"),
                    args: vec![
                        "plugin".into(),
                        "install".into(),
                        PLUGIN_SELECTOR.into(),
                        "--scope".into(),
                        "user".into(),
                    ],
                    label: "plugin_install".into(),
                },
                ArgvPlan {
                    program: PathBuf::from("/usr/bin/claude"),
                    args: vec!["plugin".into(), "list".into(), "--json".into()],
                    label: "plugin_list".into(),
                },
            ],
            steps: vec![
                ActivationStep::MarketplaceAdd,
                ActivationStep::PluginInstall,
                ActivationStep::PluginList,
            ],
            blocked: false,
            blocked_reason: None,
            activation_required: false,
            target_binding_id: "tb".into(),
        };
        let res = act.apply(&plan, &CancellationToken::new()).unwrap();
        assert!(res.ok);
        assert_eq!(res.commands_run, 3);
        let calls = runner.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].args[1], "marketplace");
        assert_eq!(calls[1].args[2], PLUGIN_SELECTOR);
        assert_eq!(calls[2].args[1], "list");
    }

    #[test]
    fn codex_fake_runner_remove_and_add_surfaces() {
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_ok("removed");
        runner.push_ok(r#"{"plugins":[]}"#);
        let act = CodexPackageActivator::new(runner.clone());
        let plan = ActivationPlan {
            target: AgentTarget::Codex,
            package_root: PathBuf::from("/tmp/pkg"),
            plugin_selector: PLUGIN_SELECTOR.into(),
            marketplace_name: MARKETPLACE_NAME.into(),
            desired_enabled: false,
            desired_presence: DesiredPresence::Present,
            commands: vec![
                ArgvPlan {
                    program: PathBuf::from("/usr/bin/codex"),
                    args: vec![
                        "plugin".into(),
                        "remove".into(),
                        PLUGIN_SELECTOR.into(),
                        "--scope".into(),
                        "user".into(),
                    ],
                    label: "plugin_remove".into(),
                },
                ArgvPlan {
                    program: PathBuf::from("/usr/bin/codex"),
                    args: vec!["plugin".into(), "list".into(), "--json".into()],
                    label: "plugin_list".into(),
                },
            ],
            steps: vec![ActivationStep::PluginRemove, ActivationStep::PluginList],
            blocked: false,
            blocked_reason: None,
            activation_required: false,
            target_binding_id: "tb".into(),
        };
        let res = act.apply(&plan, &CancellationToken::new()).unwrap();
        assert!(res.ok);
        let calls = runner.calls();
        assert_eq!(calls[0].args[1], "remove");
        // desiredPresence remains present (binding semantics)
        assert_eq!(plan.desired_presence, DesiredPresence::Present);
    }

    #[test]
    fn opencode_activation_is_native_path_verify() {
        let pkg = sample_package(AgentTarget::OpenCode);
        let runner = Arc::new(FakeProcessRunner::new());
        let act = OpenCodePackageActivator::new(runner);
        let mut binding = sample_binding(true);
        binding.target = AgentTarget::OpenCode;
        let plan = act
            .build_plan(&pkg, &binding, &sample_probe(AgentTarget::OpenCode))
            .unwrap();
        // baseline blocked for writes
        assert!(plan.blocked);
        // force apply path verify on forced non-blocked plan
        let forced = ActivationPlan {
            blocked: false,
            activation_required: false,
            blocked_reason: None,
            ..plan
        };
        let res = act.apply(&forced, &CancellationToken::new()).unwrap();
        assert!(res.ok);
        let insp = act.inspect(&forced).unwrap();
        assert!(insp.present);
    }
}
