//! agent_hub/plugins/hook_mapping — 证据驱动、fail-closed 的 Hook 跨 target 映射
//!
//! Business Logic（为什么需要这个模块）:
//!     Hook 默认 targetOnly；只有 support manifest 中具备双端 schema、信任模型与真实 CLI
//!     evidence 的 mapping 才允许跨 target 渲染。缺映射/版本漂移/合同字段丢失/信任模型不一致/
//!     evidence 不在 quality matrix 时必须保持 source-only，禁止猜测移植。
//!
//! Code Logic（这个模块做什么）:
//!     从编译期 support-manifest 的 `hookMappings` 加载注册表（初始可为空）；
//!     以 exact match 评估 intent/source/destination/schema/trust/required fields/evidence。

use crate::agent_hub::models::AgentTarget;
use crate::agent_hub::plugins::models::{HookEventIntent, PortableHook};
use crate::agent_hub::support::{builtin_support_manifest, SupportManifest};
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Hook 跨 target 信任模型。
///
/// Business Logic（为什么需要这个枚举）:
///     只有显式声明且双端合同可核对的信任模型才允许映射；未知模型 fail-closed。
///
/// Code Logic（这个枚举做什么）:
///     camelCase wire token；当前仅 `exactContract`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HookTrustModel {
    /// 输入/输出合同 required fields 与 schema 必须 exact 匹配
    ExactContract,
}

impl HookTrustModel {
    /// 稳定 wire 字符串。
    ///
    /// Business Logic: mapping 注册与诊断共用 token。
    /// Code Logic: camelCase。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExactContract => "exactContract",
        }
    }

    /// 解析 wire token。
    ///
    /// Business Logic: 未知信任模型不得 silent fallback。
    /// Code Logic: 仅匹配 as_str。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "exactContract" => Some(Self::ExactContract),
            _ => None,
        }
    }
}

/// 单条已编译的 Hook 跨 target 映射记录。
///
/// Business Logic（为什么需要这个结构体）:
///     跨 target 渲染唯一权威：intent + 双端 + schema + trust + evidence + required fields。
///
/// Code Logic（这个结构体做什么）:
///     camelCase；来自 support-manifest `hookMappings[]`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookMappingRecord {
    /// 事件意图
    pub intent: HookEventIntent,
    /// 来源 target
    pub source_target: AgentTarget,
    /// 目标 target
    pub destination_target: AgentTarget,
    /// 映射声明的 schema 版本（须同时匹配 source/destination adapter schema 期望）
    pub schema_version: u32,
    /// 信任模型
    pub trust_model: HookTrustModel,
    /// quality-matrix evidence ID
    pub evidence_id: String,
    /// 输入合同必须保留的字段名
    #[serde(default)]
    pub required_input_fields: Vec<String>,
    /// 输出合同必须保留的字段名
    #[serde(default)]
    pub required_output_fields: Vec<String>,
}

/// Hook 映射评估结果。
///
/// Business Logic（为什么需要这个枚举）:
///     调用方必须区分“可跨 target 渲染”与“保持 source-only”，并保留稳定诊断 token。
///
/// Code Logic（这个枚举做什么）:
///     Allowed 携带命中记录；SourceOnly 携带 reasons。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookMappingDecision {
    /// 允许投影到 destination（记录已校验）
    Allowed {
        /// 命中的 mapping
        record: HookMappingRecord,
    },
    /// 保持 source-only
    SourceOnly {
        /// 稳定诊断 token（无 secret）
        reasons: Vec<String>,
    },
}

impl HookMappingDecision {
    /// 是否允许跨 target。
    ///
    /// Business Logic: render 路径短路用。
    /// Code Logic: matches Allowed。
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed { .. })
    }

    /// 诊断 reasons（Allowed 为空）。
    ///
    /// Business Logic: UI/测试展示 fail-closed 原因。
    /// Code Logic: clone reasons 或空。
    pub fn reasons(&self) -> Vec<String> {
        match self {
            Self::Allowed { .. } => vec![],
            Self::SourceOnly { reasons } => reasons.clone(),
        }
    }
}

/// 从 support manifest 提取 hook 映射注册表。
///
/// Business Logic（为什么需要这个函数）:
///     初始注册表可为空；空表示“尚无证据化跨 target mapping”，是合法 source-only 结果。
///
/// Code Logic（这个函数做什么）:
///     读取 `SupportManifest.hook_mappings`；解析 intent/trust；校验 evidence_id 与 schema_version。
pub fn hook_mapping_registry_from_manifest(
    manifest: &SupportManifest,
) -> Result<Vec<HookMappingRecord>, AppError> {
    let mut out = Vec::with_capacity(manifest.hook_mappings.len());
    for raw in &manifest.hook_mappings {
        if raw.schema_version == 0 {
            return Err(AppError::validation(
                "agent_hub_hook_mapping_schema_version_zero",
            ));
        }
        if raw.evidence_id.trim().is_empty() {
            return Err(AppError::validation(
                "agent_hub_hook_mapping_empty_evidence_id",
            ));
        }
        if raw.source_target == raw.destination_target {
            return Err(AppError::validation(
                "agent_hub_hook_mapping_same_source_destination",
            ));
        }
        let intent = HookEventIntent::parse(&raw.intent).ok_or_else(|| {
            AppError::validation(format!(
                "agent_hub_hook_mapping_unknown_intent:{}",
                raw.intent
            ))
        })?;
        let trust_model = HookTrustModel::parse(&raw.trust_model).ok_or_else(|| {
            AppError::validation(format!(
                "agent_hub_hook_mapping_unknown_trust_model:{}",
                raw.trust_model
            ))
        })?;
        out.push(HookMappingRecord {
            intent,
            source_target: raw.source_target,
            destination_target: raw.destination_target,
            schema_version: raw.schema_version,
            trust_model,
            evidence_id: raw.evidence_id.clone(),
            required_input_fields: raw.required_input_fields.clone(),
            required_output_fields: raw.required_output_fields.clone(),
        });
    }
    Ok(out)
}

/// 加载编译期内置 hook 映射注册表。
///
/// Business Logic: 生产路径只认 include_str 合同，禁止 runtime 改写。
/// Code Logic: builtin_support_manifest + extract。
pub fn builtin_hook_mapping_registry() -> Result<Vec<HookMappingRecord>, AppError> {
    let manifest = builtin_support_manifest()?;
    hook_mapping_registry_from_manifest(&manifest)
}

/// 评估 Hook 是否可映射到 destination。
///
/// Business Logic（为什么需要这个函数）:
///     缺映射、schema 不匹配、required field 丢失、trust 不一致、evidence 不在矩阵 → source-only。
///
/// Code Logic（这个函数做什么）:
///     1) 同源 destination → Allowed 虚拟同 target（无 registry 项）
///     2) 在 registry 中 exact 查找 intent+source+dest
///     3) 校验 schema_version / trust / required fields / evidence 集合
pub fn evaluate_hook_mapping(
    hook: &PortableHook,
    destination: AgentTarget,
    registry: &[HookMappingRecord],
    known_evidence_ids: &BTreeSet<String>,
    expected_schema_version: u32,
    expected_trust: HookTrustModel,
) -> HookMappingDecision {
    // 同 target：不需要跨 target mapping 记录
    if hook.source_target == destination {
        return HookMappingDecision::Allowed {
            record: HookMappingRecord {
                intent: hook.event_intent,
                source_target: hook.source_target,
                destination_target: destination,
                schema_version: expected_schema_version,
                trust_model: expected_trust,
                evidence_id: "same-target".into(),
                required_input_fields: vec![],
                required_output_fields: vec![],
            },
        };
    }

    let candidates: Vec<&HookMappingRecord> = registry
        .iter()
        .filter(|r| {
            r.intent == hook.event_intent
                && r.source_target == hook.source_target
                && r.destination_target == destination
        })
        .collect();

    if candidates.is_empty() {
        return HookMappingDecision::SourceOnly {
            reasons: vec!["hook_mapping_absent".into()],
        };
    }

    // 多条命中 fail-closed（合同必须唯一）
    if candidates.len() > 1 {
        return HookMappingDecision::SourceOnly {
            reasons: vec!["hook_mapping_ambiguous".into()],
        };
    }
    let record = candidates[0];
    let mut reasons = Vec::new();

    if record.schema_version != expected_schema_version {
        reasons.push("hook_mapping_schema_version_mismatch".into());
    }
    // PortableHook 侧当前固定 schema=1（无独立字段）；若未来扩展，在此比较 hook 自身版本
    if expected_schema_version == 0 {
        reasons.push("hook_expected_schema_version_zero".into());
    }

    if record.trust_model != expected_trust {
        reasons.push("hook_mapping_trust_model_differs".into());
    }

    if !contract_has_required_fields(&hook.input_contract, &record.required_input_fields) {
        reasons.push("hook_mapping_input_contract_missing_required_field".into());
    }
    if !contract_has_required_fields(&hook.output_contract, &record.required_output_fields) {
        reasons.push("hook_mapping_output_contract_missing_required_field".into());
    }

    let evidence = record.evidence_id.trim();
    if evidence.is_empty() {
        reasons.push("hook_mapping_evidence_id_empty".into());
    } else if !known_evidence_ids.contains(evidence) {
        reasons.push("hook_mapping_evidence_id_absent_from_quality_matrix".into());
    }

    if !reasons.is_empty() {
        return HookMappingDecision::SourceOnly { reasons };
    }

    HookMappingDecision::Allowed {
        record: record.clone(),
    }
}

/// 检查 JSON 合同对象是否包含全部 required 字段（顶层 object key）。
///
/// Business Logic: 合同字段丢失视为映射失效，禁止半截投影。
/// Code Logic: 仅当 contract 为 object 时检查 key 存在；非 object 且 required 非空 → false。
fn contract_has_required_fields(contract: &serde_json::Value, required: &[String]) -> bool {
    if required.is_empty() {
        return true;
    }
    let Some(obj) = contract.as_object() else {
        return false;
    };
    required.iter().all(|k| obj.contains_key(k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::support::load_support_manifest_from_str;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn sample_hook() -> PortableHook {
        PortableHook {
            event_intent: HookEventIntent::PreToolUse,
            input_contract: json!({"toolName": "Bash", "sessionId": "s1"}),
            output_contract: json!({"permission": "allow"}),
            command_tree_hash: None,
            source_target: AgentTarget::Claude,
            target_extensions: BTreeMap::new(),
        }
    }

    fn fixture_record() -> HookMappingRecord {
        HookMappingRecord {
            intent: HookEventIntent::PreToolUse,
            source_target: AgentTarget::Claude,
            destination_target: AgentTarget::Codex,
            schema_version: 1,
            trust_model: HookTrustModel::ExactContract,
            evidence_id: "L3-AGENT-HUB-HOOK-FIXTURE-001".into(),
            required_input_fields: vec!["toolName".into()],
            required_output_fields: vec!["permission".into()],
        }
    }

    fn evidence_set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| (*s).to_string()).collect()
    }

    /// Business Logic: 无 registry 记录时跨 target 必须 source-only。
    #[test]
    fn absent_mapping_is_source_only() {
        let hook = sample_hook();
        let decision = evaluate_hook_mapping(
            &hook,
            AgentTarget::Codex,
            &[],
            &evidence_set(&["L3-AGENT-HUB-HOOK-FIXTURE-001"]),
            1,
            HookTrustModel::ExactContract,
        );
        assert!(!decision.is_allowed());
        assert!(decision
            .reasons()
            .iter()
            .any(|r| r == "hook_mapping_absent"));
    }

    /// Business Logic: schema 不匹配不得跨 target。
    #[test]
    fn schema_version_mismatch_is_source_only() {
        let hook = sample_hook();
        let mut record = fixture_record();
        record.schema_version = 2;
        let decision = evaluate_hook_mapping(
            &hook,
            AgentTarget::Codex,
            &[record],
            &evidence_set(&["L3-AGENT-HUB-HOOK-FIXTURE-001"]),
            1,
            HookTrustModel::ExactContract,
        );
        assert!(!decision.is_allowed());
        assert!(decision
            .reasons()
            .iter()
            .any(|r| r == "hook_mapping_schema_version_mismatch"));
    }

    /// Business Logic: 输入合同丢失 required 字段 → source-only。
    #[test]
    fn missing_required_input_field_is_source_only() {
        let mut hook = sample_hook();
        hook.input_contract = json!({"sessionId": "s1"}); // 丢 toolName
        let decision = evaluate_hook_mapping(
            &hook,
            AgentTarget::Codex,
            &[fixture_record()],
            &evidence_set(&["L3-AGENT-HUB-HOOK-FIXTURE-001"]),
            1,
            HookTrustModel::ExactContract,
        );
        assert!(!decision.is_allowed());
        assert!(decision
            .reasons()
            .iter()
            .any(|r| r == "hook_mapping_input_contract_missing_required_field"));
    }

    /// Business Logic: 输出合同丢失 required 字段 → source-only。
    #[test]
    fn missing_required_output_field_is_source_only() {
        let mut hook = sample_hook();
        hook.output_contract = json!({});
        let decision = evaluate_hook_mapping(
            &hook,
            AgentTarget::Codex,
            &[fixture_record()],
            &evidence_set(&["L3-AGENT-HUB-HOOK-FIXTURE-001"]),
            1,
            HookTrustModel::ExactContract,
        );
        assert!(!decision.is_allowed());
        assert!(decision
            .reasons()
            .iter()
            .any(|r| r == "hook_mapping_output_contract_missing_required_field"));
    }

    /// Business Logic: 未知 trust token fail-closed；exactContract 匹配才允许。
    #[test]
    fn trust_model_unknown_token_rejected_and_exact_allows() {
        assert_eq!(HookTrustModel::parse("totallyUnknown"), None);
        assert_eq!(
            HookTrustModel::parse("exactContract"),
            Some(HookTrustModel::ExactContract)
        );
        let hook = sample_hook();
        let ok = evaluate_hook_mapping(
            &hook,
            AgentTarget::Codex,
            &[fixture_record()],
            &evidence_set(&["L3-AGENT-HUB-HOOK-FIXTURE-001"]),
            1,
            HookTrustModel::ExactContract,
        );
        assert!(ok.is_allowed());
    }

    /// Business Logic: evidence 不在 quality matrix → source-only。
    #[test]
    fn evidence_absent_from_matrix_is_source_only() {
        let hook = sample_hook();
        let decision = evaluate_hook_mapping(
            &hook,
            AgentTarget::Codex,
            &[fixture_record()],
            &evidence_set(&[]), // 空矩阵
            1,
            HookTrustModel::ExactContract,
        );
        assert!(!decision.is_allowed());
        assert!(decision
            .reasons()
            .iter()
            .any(|r| r == "hook_mapping_evidence_id_absent_from_quality_matrix"));
    }

    /// Business Logic: 完整 fixture mapping + evidence 才允许跨 target。
    #[test]
    fn checked_in_fixture_mapping_allows_cross_target() {
        let hook = sample_hook();
        let decision = evaluate_hook_mapping(
            &hook,
            AgentTarget::Codex,
            &[fixture_record()],
            &evidence_set(&["L3-AGENT-HUB-HOOK-FIXTURE-001"]),
            1,
            HookTrustModel::ExactContract,
        );
        assert!(decision.is_allowed());
        match decision {
            HookMappingDecision::Allowed { record } => {
                assert_eq!(record.destination_target, AgentTarget::Codex);
                assert_eq!(record.evidence_id, "L3-AGENT-HUB-HOOK-FIXTURE-001");
            }
            _ => panic!("expected allowed"),
        }
    }

    /// Business Logic: 同 target 无需 registry 即可 Allowed。
    #[test]
    fn same_target_is_allowed_without_registry() {
        let hook = sample_hook();
        let decision = evaluate_hook_mapping(
            &hook,
            AgentTarget::Claude,
            &[],
            &BTreeSet::new(),
            1,
            HookTrustModel::ExactContract,
        );
        assert!(decision.is_allowed());
    }

    /// Business Logic: 内置 manifest 初始 hookMappings 可为空。
    #[test]
    fn builtin_registry_may_be_empty() {
        let reg = builtin_hook_mapping_registry().expect("builtin");
        assert!(
            reg.is_empty(),
            "initial Gate D registry must stay empty until real CLI evidence lands"
        );
    }

    /// Business Logic: manifest JSON 可声明 hookMappings（测试 fixture）。
    #[test]
    fn manifest_hook_mappings_round_trip() {
        let json = r#"{
          "schemaVersion": 1,
          "targets": [
            {
              "target": "claude",
              "adapterSchemaVersion": 1,
              "minTestedVersion": "1.0.0",
              "currentTestedVersion": "1.0.0",
              "executableProbe": { "commandNames": ["claude"], "versionArgs": ["--version"] },
              "capabilities": {
                "scanInstruction": "readOnly",
                "renderInstruction": "blocked",
                "scanPortableAssets": "readOnly",
                "renderPortableAssets": "blocked",
                "activatePackage": "blocked",
                "deactivatePackage": "blocked",
                "liveReload": "blocked"
              },
              "evidenceIds": ["E1"]
            },
            {
              "target": "codex",
              "adapterSchemaVersion": 1,
              "minTestedVersion": "1.0.0",
              "currentTestedVersion": "1.0.0",
              "executableProbe": { "commandNames": ["codex"], "versionArgs": ["--version"] },
              "capabilities": {
                "scanInstruction": "readOnly",
                "renderInstruction": "blocked",
                "scanPortableAssets": "readOnly",
                "renderPortableAssets": "blocked",
                "activatePackage": "blocked",
                "deactivatePackage": "blocked",
                "liveReload": "blocked"
              },
              "evidenceIds": ["E2"]
            },
            {
              "target": "opencode",
              "adapterSchemaVersion": 1,
              "minTestedVersion": "1.0.0",
              "currentTestedVersion": "1.0.0",
              "executableProbe": { "commandNames": ["opencode"], "versionArgs": ["--version"] },
              "capabilities": {
                "scanInstruction": "readOnly",
                "renderInstruction": "blocked",
                "scanPortableAssets": "readOnly",
                "renderPortableAssets": "blocked",
                "activatePackage": "blocked",
                "deactivatePackage": "blocked",
                "liveReload": "blocked"
              },
              "evidenceIds": ["E3"]
            }
          ],
          "hookMappings": [
            {
              "intent": "preToolUse",
              "sourceTarget": "claude",
              "destinationTarget": "codex",
              "schemaVersion": 1,
              "trustModel": "exactContract",
              "evidenceId": "L3-AGENT-HUB-HOOK-FIXTURE-001",
              "requiredInputFields": ["toolName"],
              "requiredOutputFields": ["permission"]
            }
          ]
        }"#;
        let m = load_support_manifest_from_str(json).unwrap();
        let reg = hook_mapping_registry_from_manifest(&m).unwrap();
        assert_eq!(reg.len(), 1);
        assert_eq!(reg[0].intent, HookEventIntent::PreToolUse);
        assert_eq!(reg[0].trust_model, HookTrustModel::ExactContract);
    }
}
