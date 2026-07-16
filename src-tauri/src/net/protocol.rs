//! net/protocol.rs — P2P 协议元数据与能力宣告（v0→v1）
//!
//! Business Logic（为什么需要这个模块）:
//!     局域网内的 cc-partner 实例需要互相判断对方支持哪些 P2P 能力（错误信封、运行时通知、Inbox 等），
//!     才能安全地选择调用新路由。本模块定义 v1 协议元数据结构 `PeerProtocolInfo`，供 health/对端探测时
//!     交换 `{protocol_version, capabilities}`，并容忍缺失字段（视为旧版 v0/无能力），保证与早期版本的向后兼容。
//!     本轮只维护 v0→v1 一代兼容。
//!
//! Code Logic（这个模块做什么）:
//!     - `PeerProtocolInfo`：serde 结构，`protocol_version` 缺省为 0，`capabilities` 缺省为空且去重排序。
//!     - `server_protocol_info()`：返回本机当前 build 实际支持的完整能力（本轮只宣告 `errors.envelope.v1`）。
//!     - `supports(capability)`：要求 protocol_version >= 1 且能力 token 完全匹配。
//!     - 反序列化对缺失字段和未知未来字段均保持容忍（v0 兼容 / 前向兼容）。

use serde::{Deserialize, Serialize};

/// 当前协议大版本号。
///
/// Business Logic: 本机宣告的对端互通协议版本；旧版/缺失字段的对端视为 v0。
/// Code Logic: 仅在新增不兼容能力时递增；本轮固定为 1。
pub const PROTOCOL_VERSION_V1: u32 = 1;

/// 能力 token：v1 标准错误信封（`/api/...` 错误统一返回 `{error}` 信封结构）。
///
/// Business Logic: 对端据此决定能否信任本机错误响应的稳定信封格式。
///
/// **语义边界（重要）**：本 token **只**描述错误响应的线材格式（P2pErrorEnvelope），
/// **不**描述路由访问权限或路由是否存在。具体而言：
/// - 缺少该能力的 v0 对端**仍可**被调用任意已存在的 `/api/...` 路由；只是它的错误
///   响应可能仍是老形态 `{error: "..."}`，客户端需用 `parse_peer_response` 兼容两种形态。
/// - 该能力**不**代表对端实现了某个特定新路由；调用新路由前应另行确认路由存在
///   （如通过 health/version 或直接尝试并处理 404）。
/// - 当前唯一已声明的 v1 能力就是本 token；后续 Runtime/Inbox 等能力会随各自路由
///   原子地加入独立 token，**不应**复用本 token 表达"支持新路由"。
///
/// Code Logic: 字符串常量，与 `PeerProtocolInfo::supports()` 做精确匹配。
pub const CAPABILITY_ERRORS_ENVELOPE_V1: &str = "errors.envelope.v1";

/// 能力 token：v1 Orchestrator owning-device runtime-snapshot 路由
/// （`POST /api/orchestrator/runtime-snapshot`）。
///
/// Business Logic（为什么需要这个 token）:
///     对端在调用 owning-device runtime-snapshot 路由前，必须确认本设备已实现该路由，
///     否则旧版本（未挂载该路由）会返回 404 并触发不必要的重试。该 token 与
///     `errors.envelope.v1` 解耦——后者只描述错误响应的线材格式，不代表路由存在。
///
/// **语义边界（重要）**：本 token 与对应路由 / 能力**原子地**上线：本机宣告该 token 等价于
/// 本机实现了 `POST /api/orchestrator/runtime-snapshot` 路由。调用方应在调用该路由前
/// 先用 `PeerProtocolInfo::supports(token)` 确认；本机当前 build 总是宣告该 token。
///
/// Code Logic: 字符串常量，与 `PeerProtocolInfo::supports()` 做精确匹配；随路由一起在
/// `server_protocol_info()` 中宣告。
pub const CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1: &str = "orchestrator.runtime-snapshot.v1";

/// 能力 token：v1 Orchestrator Human Review 有界 diff 快照
/// （`POST /api/orchestrator/tasks/review-diff`）。
///
/// Business Logic（为什么需要这个 token）:
///     对端在拉取 owning-device review diff 前必须确认本设备已实现该路由；
///     旧版本缺失能力时客户端应显示 unsupported，不得猜测旧接口或接受任意 repo path/ref。
///     本 token 与 review-diff 路由原子上线，与 `errors.envelope.v1` 解耦。
///
/// Code Logic（这个常量做什么）:
///     字符串常量，与 `PeerProtocolInfo::supports()` 精确匹配；列入 `server_protocol_info()`。
pub const CAPABILITY_ORCHESTRATOR_REVIEW_DIFF_V1: &str = "orchestrator.review-diff.v1";

/// 能力 token：v1 Orchestrator WORKFLOW 文档 API
/// （`POST /api/orchestrator/workflow-document/{get,validate,save}`）。
///
/// Business Logic（为什么需要这个 token）:
///     remote shortcut 在读写 owning device 的 WORKFLOW.md 前必须确认对端已实现权威文档路由；
///     旧 peer 缺失时客户端应 Unsupported，不得猜测文件路径或旧接口。本 token 与三条路由原子上线。
///
/// Code Logic（这个常量做什么）:
///     字符串常量，列入 `server_protocol_info()`；client `supports` 门控。
pub const CAPABILITY_ORCHESTRATOR_WORKFLOW_DOCUMENT_V1: &str = "orchestrator.workflow-document.v1";

/// 能力 token：v1 Orchestrator Agent adapter catalog
/// （`POST /api/orchestrator/agent-adapters`）。
///
/// Business Logic（为什么需要这个 token）:
///     远端只协商是否支持 adapter 可用性查询；不含 path/env，也非授权机制。
///
/// Code Logic（这个常量做什么）:
///     字符串常量，列入 `server_protocol_info()`。
pub const CAPABILITY_ORCHESTRATOR_AGENT_ADAPTERS_V1: &str = "orchestrator.agent-adapters.v1";

/// 能力 token：v1 Automated Candidate Experiments
/// （`POST /api/orchestrator/experiments/*`）。
///
/// Business Logic（为什么需要这个 token）:
///     远端创建实验组前必须确认对端支持组级原子合同；旧 peer 无能力时
///     不得静默降级为 N 条普通 task。
///
/// Code Logic（这个常量做什么）:
///     字符串常量，列入 `server_protocol_info()`。
pub const CAPABILITY_ORCHESTRATOR_EXPERIMENTS_V1: &str = "orchestrator.experiments.v1";

/// 能力 token：v1 全局 Attention/Inbox 快照路由（`GET /api/mobile/attention`）。
///
/// Business Logic（为什么需要这个 token）:
///     移动端在拉取 Inbox 快照前必须确认对端已实现 attention 路由；旧版本缺失该能力时
///     应明确显示 unsupported，不得猜测或拼接旧接口结果。本 token 与
///     `errors.envelope.v1` 解耦——后者只描述错误响应线材格式。
///
/// **语义边界（重要）**：本 token 与 `GET /api/mobile/attention` 路由**原子地**上线：
/// 本机宣告该 token 等价于本机实现了该路由。调用方应在调用前用
/// `PeerProtocolInfo::supports(token)` 确认；本机当前 build 总是宣告该 token。
///
/// Code Logic: 字符串常量，与 `PeerProtocolInfo::supports()` 做精确匹配；随路由一起在
/// `server_protocol_info()` 中宣告。
pub const CAPABILITY_ATTENTION_V1: &str = "attention.v1";
/// Attention Inbox v2（含 Agent needsInput/failed 投影；capability 仅协议协商）。
pub const CAPABILITY_ATTENTION_V2: &str = "attention.v2";

/// 能力 token：v1 文件传输显式 complete/finalize 握手
/// （`POST /api/transfer/complete/:id`）。
///
/// Business Logic（为什么需要这个 token）:
///     新发送端在 size=0 / full-tmp 续传场景必须调用 complete 才能触发对端落盘；
///     旧接收端没有该路由，无条件调用会导致 404 假失败与重试重复副本。
///     本 token 与 complete 路由原子上线：宣告即表示路由可用。
///     对无该能力的 legacy 对端，发送端对普通非空传输回退为“最后一块 chunk 已 finalize”，
///     对 size=0/full-tmp 则明确 unsupported，不得假报成功。
///
/// Code Logic: 字符串常量，与 `PeerProtocolInfo::supports()` 做精确匹配；随路由一起在
/// `server_protocol_info()` 中宣告。
pub const CAPABILITY_TRANSFER_COMPLETE_V1: &str = "transfer.complete.v1";

/// 能力 token：v1 文件传输断点续传（resume）契约
/// （稳定 `protocol_transfer_id` + receiver checkpoint / init resume_offset）。
///
/// Business Logic（为什么需要这个 token）:
///     发送端 resume 前必须确认对端 checkpoint 按 protocol id 可命中；旧 peer 无此能力时
///     只能走 retry，不得显示假续传。本 token 与现有 init/chunk resume_offset 语义原子宣告。
///
/// Code Logic: 字符串常量，列入 `server_protocol_info()`；客户端 `supports` 门控 resume。
pub const CAPABILITY_TRANSFER_RESUME_V1: &str = "transfer.resume.v1";

/// 能力 token：v1 CC History 分页同步
/// （`POST /api/cc-history/sync/{manifest-page,items,push-batch}`）。
///
/// Business Logic（为什么需要这个 token）:
///     新客户端在走有界分页协议前必须确认对端已挂载三个 paged 路由；旧版本缺失该能力时
///     必须回退 legacy `/pull|/push`，不得猜测 paged 路由存在。本 token 与三条路由原子上线。
///
/// Code Logic（这个函数做什么）:
///     字符串常量，与 `PeerProtocolInfo::supports()` 做精确匹配；随路由一起在
///     `server_protocol_info()` 中宣告。
pub const CAPABILITY_CC_HISTORY_PAGED_SYNC_V1: &str = "cc-history.paged-sync.v1";

/// 能力 token：Prompt/SSH/Scratchpad 有界 manifest/items/push-batch v2
/// （`POST /api/sync/prompts/*`、`/api/ssh-target/sync/*`、`/api/scratchpad/sync/*` 新路由）。
///
/// Business Logic（为什么需要这个 token）:
///     新客户端在走三域 v2 分页协议前必须确认对端已挂载路由 **且** 具备原子 idempotency ledger
///     （`sync_request_ledger` + 事务 bulk upsert）。本 token 与三域 ledger 写路径**原子**上线：
///     宣告即表示 push-batch 同 key 重放安全。
///
/// Code Logic（这个常量做什么）:
///     字符串常量，与 `PeerProtocolInfo::supports()` 精确匹配；列入 `server_protocol_info()`。
pub const CAPABILITY_SYNC_MANIFEST_V2: &str = "sync.manifest.v2";

/// 能力 token：v1 Workbench mutation outcome envelope
/// （mutation 成功通道 `succeeded|unknown` + operation ledger 查询）。
///
/// Business Logic（为什么需要这个 token）:
///     对端在消费 mutation envelope / 查询 operation ledger 前必须确认本机已实现
///     mutation-outcome 契约；旧版本缺失该能力时只能走 legacy 单次调用，不得假设
///     unknown envelope 或 ledger 状态可查。本 token 与对应 wire/route 原子上线。
///
/// Code Logic（这个常量做什么）:
///     字符串常量，与 `PeerProtocolInfo::supports()` 做精确匹配；随能力一起在
///     `server_protocol_info()` 中宣告。
pub const CAPABILITY_WORKBENCH_MUTATION_OUTCOME_V1: &str = "workbench.mutation-outcome.v1";

/// 能力 token：v2 Workbench Claude session 搜索结果 DTO
/// （`{items, truncated, diagnostics}` + 混部 dual-decode）。
///
/// Business Logic（为什么需要这个 token）:
///     对端在期望有界预算诊断的搜索结果前必须确认本机返回 SessionSearchResult 对象；
///     旧版本仅返回 `Vec<SessionSearchHit>` 数组。本 token 与 search 路由 DTO/decoder 原子上线。
///
/// Code Logic（这个常量做什么）:
///     字符串常量，列入 `server_protocol_info()`；客户端可据此选择解析策略（亦可用 dual-decode）。
pub const CAPABILITY_WORKBENCH_SESSION_SEARCH_RESULT_V2: &str =
    "workbench.session-search-result.v2";

/// 能力 token：v1 Workbench Agent session runtime
/// （snapshot + NDJSON `agentRuntime` 事件；协议协商 only，非权限 token）。
///
/// Business Logic（为什么需要这个 token）:
///     remote/mobile 在拉取 Agent phase snapshot 前必须确认对端已实现
///     `POST /api/workbench/agent-runtime/snapshot` 与 events 变体；旧 peer 显示 unsupported，
///     不得回退为 Claude session 猜测。本 token 只做版本协商，不是 LAN 鉴权。
/// Code Logic（这个常量做什么）:
///     字符串常量，列入 `server_protocol_info()`；与 snapshot 路由原子上线。
pub const CAPABILITY_WORKBENCH_AGENT_RUNTIME_V1: &str = "workbench.agent-runtime.v1";

/// 能力 token：v1 Workbench 浏览器自动验证
/// （`POST /api/workbench/browser-verification/{create,get,cancel,artifact}`）。
///     remote/mobile 在调用 owner 验证路由前必须确认对端已实现该契约；
///     旧 peer 缺失时显示 unsupported，不得回退为不安全 iframe DOM 访问。
///     与四条路由原子上线；列入 `server_protocol_info()`。
pub const CAPABILITY_WORKBENCH_BROWSER_VERIFICATION_V1: &str =
    "workbench.browser-verification.v1";
/// 能力 token：Workbench workspace safe restore（owner-local preflight + safe attach）。
///     控制设备在转发 remote project 的 preflight/safe-attach 前必须确认 owner 支持
///     纯读 preflight 与仅 attach 已有 tmux 的契约；旧 peer 缺失时只恢复 project selection。
///     字符串常量，列入 `server_protocol_info()`；client `supports` 门控。
pub const CAPABILITY_WORKBENCH_WORKSPACE_SAFE_RESTORE_V1: &str =
    "workbench.workspace-safe-restore.v1";

/// 能力 token：v1 Workbench LAN Agent Fleet owner batch
/// （`POST /api/workbench/lan-fleet/snapshot`）。
///
/// Business Logic（为什么需要这个 token）:
///     控制设备在 fan-out 拉取 owning device 的 Fleet 摘要前必须确认对端已实现
///     有界 local-only batch 路由；旧 peer 缺失时显示 unsupported，不得递归猜测。
///     本 token 只做协议协商，不是 LAN 鉴权或设备信任。
///
/// Code Logic（这个常量做什么）:
///     字符串常量，列入 `server_protocol_info()`；与 snapshot 路由原子上线。
pub const CAPABILITY_WORKBENCH_LAN_FLEET_V1: &str = "workbench.lan-fleet.v1";

/// P2P 协议元数据：对端互换的协议版本与能力清单。
///
/// Business Logic（为什么需要这个结构）:
///     health/对端探测需要一份稳定的 JSON 结构表达“本机或对端当前支持哪些 P2P 能力”。
///     JSON 形如 `{ "protocol_version": 1, "capabilities": ["errors.envelope.v1"] }`，
///     缺失字段必须安全回落为 v0/空能力（旧版 v0 兼容）。
///
/// Code Logic（这个结构做什么）:
///     - `protocol_version`：缺省 0（`#[serde(default)]`），表示未携带协议元数据的旧版。
///     - `capabilities`：缺省空数组，反序列化后通过 `deserialize_with` 去重并按字典序排序，
///       生成稳定、确定性的能力集合；额外/未知字段通过 `#[serde(deny_unknown_fields = false)]`（默认）被忽略。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerProtocolInfo {
    /// 协议大版本号；缺失时默认为 0（旧版 v0）。
    #[serde(default)]
    pub protocol_version: u32,
    /// 能力 token 列表；缺失时为空，反序列化后去重并按字典序排序。
    #[serde(default, deserialize_with = "deserialize_sorted_dedup_capabilities")]
    pub capabilities: Vec<String>,
}

impl PeerProtocolInfo {
    /// 判断本机/对端是否精确支持某项能力。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     调用新路由前必须确认对端真正支持对应能力 token；仅当 protocol_version >= 1
    ///     且 capabilities 包含该 token 时才返回 true，避免对旧版（v0）对端误调新路由。
    ///
    /// Code Logic（这个函数做什么）:
    ///     要求 `protocol_version >= 1` 且 capabilities 中存在与入参完全相等（`==`）的字符串。
    pub fn supports(&self, capability: &str) -> bool {
        self.protocol_version >= PROTOCOL_VERSION_V1
            && self.capabilities.iter().any(|c| c == capability)
    }
}

/// 返回本机当前 build 的完整协议元数据。
///
/// Business Logic（为什么需要这个函数）:
///     本机对外（health/对端探测）需要宣告自身支持的能力集合，且必须是当前 build 实际存在路由的子集。
///     本轮宣告 `attention.v1`、`cc-history.paged-sync.v1`、`errors.envelope.v1`、
///     `orchestrator.review-diff.v1`、`orchestrator.runtime-snapshot.v1`、
///     `orchestrator.workflow-document.v1`、`sync.manifest.v2`、
///     `transfer.complete.v1`、`transfer.resume.v1` 与 `workbench.mutation-outcome.v1`；
///     各自与对应路由/ledger/契约原子上线。
///
/// Code Logic（这个函数做什么）:
///     构造 `protocol_version = 1`，capabilities 为已排序、去重的当前支持能力列表。
pub fn server_protocol_info() -> PeerProtocolInfo {
    PeerProtocolInfo {
        protocol_version: PROTOCOL_VERSION_V1,
        capabilities: vec![
            CAPABILITY_ATTENTION_V1.to_string(),
            CAPABILITY_ATTENTION_V2.to_string(),
            CAPABILITY_CC_HISTORY_PAGED_SYNC_V1.to_string(),
            CAPABILITY_ERRORS_ENVELOPE_V1.to_string(),
            CAPABILITY_ORCHESTRATOR_AGENT_ADAPTERS_V1.to_string(),
            CAPABILITY_ORCHESTRATOR_EXPERIMENTS_V1.to_string(),
            CAPABILITY_ORCHESTRATOR_REVIEW_DIFF_V1.to_string(),
            CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1.to_string(),
            CAPABILITY_ORCHESTRATOR_WORKFLOW_DOCUMENT_V1.to_string(),
            CAPABILITY_SYNC_MANIFEST_V2.to_string(),
            CAPABILITY_TRANSFER_COMPLETE_V1.to_string(),
            CAPABILITY_TRANSFER_RESUME_V1.to_string(),
            CAPABILITY_WORKBENCH_AGENT_RUNTIME_V1.to_string(),
            CAPABILITY_WORKBENCH_BROWSER_VERIFICATION_V1.to_string(),
            CAPABILITY_WORKBENCH_LAN_FLEET_V1.to_string(),
            CAPABILITY_WORKBENCH_MUTATION_OUTCOME_V1.to_string(),
            CAPABILITY_WORKBENCH_SESSION_SEARCH_RESULT_V2.to_string(),
            CAPABILITY_WORKBENCH_WORKSPACE_SAFE_RESTORE_V1.to_string(),
        ],
    }
}

/// serde 反序列化器：把 capabilities 去重并按字典序排序。
///
/// Business Logic（为什么需要这个函数）:
///     capabilities 可能由多个来源拼接（对端健康响应、mDNS 提示等），调用方需要稳定的、确定性的集合，
///     去重避免重复 token 干扰精确匹配，排序便于 diff/日志/断言。
///
/// Code Logic（这个函数做什么）:
///     先按默认反序列化出 `Vec<String>`，再 sort + dedup，输出规范化后的 Vec。
fn deserialize_sorted_dedup_capabilities<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let mut values = Vec::<String>::deserialize(deserializer)?;
    values.sort();
    values.dedup();
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     对端可能是尚未携带 protocol 元数据的旧版客户端，反序列化 `{}` 必须安全回落为 v0 且无能力，
    ///     不能因字段缺失报错。
    ///
    /// Code Logic（这个测试做什么）:
    ///     从空 JSON 反序列化，断言 protocol_version == 0 且 capabilities 为空。
    #[test]
    fn missing_protocol_fields_are_legacy_v0() {
        let info: PeerProtocolInfo = serde_json::from_str("{}").unwrap();
        assert_eq!(info.protocol_version, 0);
        assert!(info.capabilities.is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     v1 元数据必须能稳定地序列化/反序列化往返，确保对端解析与本机宣告一致。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造一个 v1 PeerProtocolInfo，序列化为 JSON 后再反序列化，断言字段完全一致。
    #[test]
    fn v1_protocol_info_round_trips_through_json() {
        let info = PeerProtocolInfo {
            protocol_version: 1,
            capabilities: vec!["errors.envelope.v1".to_string()],
        };
        let json = serde_json::to_string(&info).unwrap();
        let parsed: PeerProtocolInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.protocol_version, 1);
        assert_eq!(parsed.capabilities, vec!["errors.envelope.v1".to_string()]);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     capabilities 列表可能由多个来源拼接，反序列化结果必须去重且按字典序排序，
    ///     保证对端比对能力时看到稳定的、确定性的集合。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化含重复、乱序能力的 JSON，断言结果去重且按字典序排序。
    #[test]
    fn capabilities_are_sorted_and_deduplicated_on_deserialize() {
        let json = r#"{
            "protocol_version": 1,
            "capabilities": ["errors.envelope.v1", "inbox.messages.v1", "errors.envelope.v1"]
        }"#;
        let info: PeerProtocolInfo = serde_json::from_str(json).unwrap();
        assert_eq!(
            info.capabilities,
            vec!["errors.envelope.v1", "inbox.messages.v1"]
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     协议必须前向兼容：对端未来可能携带本机尚不认识的字段，反序列化不能因此失败，
    ///     否则旧版本无法与新版本互通。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化含未知未来字段的 JSON，断言不报错且已知字段被正确解析。
    #[test]
    fn unknown_future_fields_are_ignored() {
        let json = r#"{
            "protocol_version": 1,
            "capabilities": ["errors.envelope.v1"],
            "runtime_notifications": ["v1"],
            "future_field": 42
        }"#;
        let info: PeerProtocolInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.protocol_version, 1);
        assert_eq!(info.capabilities, vec!["errors.envelope.v1".to_string()]);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     能力 token 是精确的全字符串匹配（如 `errors.envelope.v1`），不能被子串或前缀误命中，
    ///     否则对端可能误以为支持某能力而调用了未实现的更细路由。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 v1 + 单能力的 info，断言精确命中、子串/前缀/空串均不命中。
    #[test]
    fn supports_matches_exact_full_token_only() {
        let info = PeerProtocolInfo {
            protocol_version: 1,
            capabilities: vec!["errors.envelope.v1".to_string()],
        };
        assert!(info.supports("errors.envelope.v1"));
        assert!(!info.supports("errors.envelope"));
        assert!(!info.supports("errors.envelope.v1.extra"));
        assert!(!info.supports("v1"));
        assert!(!info.supports(""));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     即使对端宣称的能力列表里包含某 token，只要 protocol_version < 1（旧版 v0），
    ///     仍不能认为它真正支持该 v1 能力，避免对仅携带旧版字段的对端误调新路由。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 v0 但 capabilities 非空的 info，断言 supports() 对已知 token 仍返回 false。
    #[test]
    fn supports_requires_at_least_v1_protocol_version() {
        let info = PeerProtocolInfo {
            protocol_version: 0,
            capabilities: vec!["errors.envelope.v1".to_string()],
        };
        assert!(!info.supports("errors.envelope.v1"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     `server_protocol_info()` 是本机对外的能力宣告入口，本轮必须宣告 v1
    ///     且包含 `attention.v1`、`cc-history.paged-sync.v1`、`errors.envelope.v1`、
    ///     `orchestrator.review-diff.v1`、`orchestrator.runtime-snapshot.v1`、`sync.manifest.v2`、
    ///     `transfer.complete.v1`、`transfer.resume.v1` 与 `workbench.mutation-outcome.v1`
    ///     （分别与对应路由/ledger/契约原子上线；paged-sync 与三条 CC History 分页路由同 build；
    ///     sync.manifest.v2 与三域事务 bulk + ledger 同 build；
    ///     transfer.resume.v1 与 resume 幂等命令同 build；
    ///     workbench.mutation-outcome.v1 与 mutation ledger 同 build；
    ///     workbench.session-search-result.v2 与 SessionSearchResult DTO/decoder 同 build；
    ///     orchestrator.review-diff.v1 与 review-diff 路由同 build）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     调用 `server_protocol_info()`，断言 protocol_version == 1 且 capabilities
    ///     去重排序后正好等于当前 token 字典序列表；并确认 supports(paged-sync/v2/mutation/session-search/resume/review-diff) 为 true。
    #[test]
    fn server_protocol_info_advertises_v1_with_current_capabilities() {
        let info = server_protocol_info();
        assert_eq!(info.protocol_version, 1);
        assert_eq!(
            info.capabilities,
            vec![
                "attention.v1".to_string(),
                "attention.v2".to_string(),
                "cc-history.paged-sync.v1".to_string(),
                "errors.envelope.v1".to_string(),
                "orchestrator.agent-adapters.v1".to_string(),
                "orchestrator.experiments.v1".to_string(),
                "orchestrator.review-diff.v1".to_string(),
                "orchestrator.runtime-snapshot.v1".to_string(),
                "orchestrator.workflow-document.v1".to_string(),
                "sync.manifest.v2".to_string(),
                "transfer.complete.v1".to_string(),
                "transfer.resume.v1".to_string(),
                "workbench.agent-runtime.v1".to_string(),
                "workbench.browser-verification.v1".to_string(),
                "workbench.lan-fleet.v1".to_string(),
                "workbench.mutation-outcome.v1".to_string(),
                "workbench.session-search-result.v2".to_string(),
                "workbench.workspace-safe-restore.v1".to_string(),
            ]
        );
        assert!(info.supports(CAPABILITY_ATTENTION_V2));
        assert!(info.supports(CAPABILITY_CC_HISTORY_PAGED_SYNC_V1));
        assert!(info.supports(CAPABILITY_ORCHESTRATOR_AGENT_ADAPTERS_V1));
        assert!(info.supports(CAPABILITY_ORCHESTRATOR_REVIEW_DIFF_V1));
        assert!(info.supports(CAPABILITY_ORCHESTRATOR_WORKFLOW_DOCUMENT_V1));
        assert!(info.supports(CAPABILITY_SYNC_MANIFEST_V2));
        assert!(info.supports(CAPABILITY_TRANSFER_RESUME_V1));
        assert!(info.supports(CAPABILITY_WORKBENCH_AGENT_RUNTIME_V1));
        assert!(info.supports(CAPABILITY_WORKBENCH_BROWSER_VERIFICATION_V1));
        assert!(info.supports(CAPABILITY_WORKBENCH_LAN_FLEET_V1));
        assert!(info.supports(CAPABILITY_WORKBENCH_MUTATION_OUTCOME_V1));
        assert!(info.supports(CAPABILITY_WORKBENCH_SESSION_SEARCH_RESULT_V2));
        assert!(info.supports(CAPABILITY_WORKBENCH_WORKSPACE_SAFE_RESTORE_V1));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     旧 peer（未挂 ledger）不得被误判为支持 v2 正文 batch；客户端必须走 legacy 或 typed 失败。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造不含 `sync.manifest.v2` 的 v1 能力列表，断言 supports 为 false。
    #[test]
    fn legacy_server_omits_sync_manifest_v2() {
        let legacy = PeerProtocolInfo {
            protocol_version: 1,
            capabilities: vec![
                CAPABILITY_ATTENTION_V1.to_string(),
                CAPABILITY_CC_HISTORY_PAGED_SYNC_V1.to_string(),
                CAPABILITY_ERRORS_ENVELOPE_V1.to_string(),
                CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1.to_string(),
                CAPABILITY_TRANSFER_COMPLETE_V1.to_string(),
            ],
        };
        assert!(!legacy.supports(CAPABILITY_SYNC_MANIFEST_V2));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     三域事务 bulk + ledger 就绪后，fully wired server 必须宣告 `sync.manifest.v2`。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直接调用 `server_protocol_info()` 断言 token 存在且 supports 为 true。
    #[test]
    fn fully_wired_server_advertises_sync_manifest_v2() {
        let info = server_protocol_info();
        assert!(info
            .capabilities
            .iter()
            .any(|c| c == CAPABILITY_SYNC_MANIFEST_V2));
        assert!(info.supports(CAPABILITY_SYNC_MANIFEST_V2));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     旧 peer 无 agent-runtime 能力时客户端必须 unsupported，不得猜测 Claude session。
    ///
    /// Code Logic（这个测试做什么）:
    ///     legacy 能力列表不含 token → supports false；本机 server_protocol_info 含 token。
    #[test]
    fn mixed_version_old_peer_lacks_agent_runtime_capability() {
        let legacy = PeerProtocolInfo {
            protocol_version: 1,
            capabilities: vec![
                CAPABILITY_ERRORS_ENVELOPE_V1.to_string(),
                CAPABILITY_WORKBENCH_MUTATION_OUTCOME_V1.to_string(),
            ],
        };
        assert!(!legacy.supports(CAPABILITY_WORKBENCH_AGENT_RUNTIME_V1));
        assert!(server_protocol_info().supports(CAPABILITY_WORKBENCH_AGENT_RUNTIME_V1));
    }
}
