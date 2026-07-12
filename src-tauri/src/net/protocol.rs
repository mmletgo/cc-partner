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
/// Code Logic: 字符串常量，与 `PeerProtocolInfo::supports()` 做精确匹配。
pub const CAPABILITY_ERRORS_ENVELOPE_V1: &str = "errors.envelope.v1";

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
        self.protocol_version >= PROTOCOL_VERSION_V1 && self.capabilities.iter().any(|c| c == capability)
    }
}

/// 返回本机当前 build 的完整协议元数据。
///
/// Business Logic（为什么需要这个函数）:
///     本机对外（health/对端探测）需要宣告自身支持的能力集合，且必须是当前 build 实际存在路由的子集。
///     本轮（计划初始）只宣告 `errors.envelope.v1`；后续 Runtime/Inbox 计划会随其路由原子地加入新常量与宣告。
///
/// Code Logic（这个函数做什么）:
///     构造 `protocol_version = 1`，capabilities 为已排序、去重的当前支持能力列表。
pub fn server_protocol_info() -> PeerProtocolInfo {
    PeerProtocolInfo {
        protocol_version: PROTOCOL_VERSION_V1,
        capabilities: vec![CAPABILITY_ERRORS_ENVELOPE_V1.to_string()],
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
    ///     `server_protocol_info()` 是本机对外的能力宣告入口，本轮（计划初始）必须宣告 v1
    ///     且只包含 `errors.envelope.v1`；其它能力由后续 Runtime/Inbox 计划随路由原子加入。
    ///
    /// Code Logic（这个测试做什么）:
    ///     调用 `server_protocol_info()`，断言 protocol_version == 1 且 capabilities
    ///     去重排序后正好等于 `["errors.envelope.v1"]`。
    #[test]
    fn server_protocol_info_advertises_v1_with_errors_envelope() {
        let info = server_protocol_info();
        assert_eq!(info.protocol_version, 1);
        assert_eq!(info.capabilities, vec!["errors.envelope.v1".to_string()]);
    }
}
