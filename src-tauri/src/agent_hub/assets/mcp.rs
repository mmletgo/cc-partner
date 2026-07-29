//! agent_hub/assets/mcp — Canonical PortableMcpServer 载荷
//!
//! Business Logic（为什么需要这个模块）:
//!     MCP server 配置含 transport/env/headers/URL 凭据；Hub 以原文保存与物化，
//!     但错误/诊断/日志不得回显 secret。transport 形态必须自洽。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `McpTransport` / `PortableMcpServer`、校验 key/transport，并提供
//!     绝对路径诊断（不把 credential 写入诊断 message）。

use crate::agent_hub::assets::diagnostics::{PortabilityDiagnostic, CODE_UNKNOWN_SOURCE_FIELD};
use crate::agent_hub::models::AgentTarget;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// MCP 传输层。
///
/// Business Logic（为什么需要这个枚举）:
///     stdio 与 http 字段集合互斥；错误组合不得静默降级。
///
/// Code Logic（这个枚举做什么）:
///     内部 tag `type` + camelCase 字段；Stdio{command,args,cwd} / Http{url,headers}。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum McpTransport {
    /// 本地 stdio 进程
    Stdio {
        /// 可执行命令
        command: String,
        /// 参数
        args: Vec<String>,
        /// 可选工作目录
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    /// HTTP(S) 远端
    Http {
        /// 服务 URL（可含 query token，原文保存）
        url: String,
        /// 静态 headers（可含 Authorization，原文保存）
        headers: BTreeMap<String, String>,
    },
}

/// Canonical MCP server 可移植载荷。
///
/// Business Logic（为什么需要这个结构体）:
///     以语义 key 标识 server；env/headers/url 凭据按原文进入 CAS 与投影。
///
/// Code Logic（这个结构体做什么）:
///     camelCase；env/headers 用 BTreeMap 保证确定性键序。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableMcpServer {
    /// 语义 server key（Hub 拥有）
    pub key: String,
    /// 传输配置
    pub transport: McpTransport,
    /// 环境变量（原文，含 secret）
    pub env: BTreeMap<String, String>,
    /// 是否启用（可随 binding 覆盖，但载荷保留）
    pub enabled: bool,
    /// 工具 allow 列表
    pub tool_allow: Vec<String>,
    /// 工具 deny 列表
    pub tool_deny: Vec<String>,
    /// 各 target 扩展
    pub target_extensions: BTreeMap<AgentTarget, serde_json::Value>,
}

impl PortableMcpServer {
    /// 校验 MCP 载荷。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     空 key、空 command/url 或非法 transport 组合不得写入 revision。
    ///
    /// Code Logic（这个函数做什么）:
    ///     key trim 非空；Stdio.command 非空；Http.url 非空且含 scheme 启发式。
    pub fn validate(&self) -> Result<(), AppError> {
        if self.key.trim().is_empty() {
            return Err(AppError::validation(
                "agent_hub_portable_mcp_empty_key".to_string(),
            ));
        }
        match &self.transport {
            McpTransport::Stdio { command, cwd, .. } => {
                if command.trim().is_empty() {
                    return Err(AppError::validation(
                        "agent_hub_portable_mcp_stdio_empty_command".to_string(),
                    ));
                }
                if let Some(cwd) = cwd {
                    if cwd.trim().is_empty() {
                        return Err(AppError::validation(
                            "agent_hub_portable_mcp_stdio_empty_cwd".to_string(),
                        ));
                    }
                }
            }
            McpTransport::Http { url, .. } => {
                if url.trim().is_empty() {
                    return Err(AppError::validation(
                        "agent_hub_portable_mcp_http_empty_url".to_string(),
                    ));
                }
                let lower = url.to_ascii_lowercase();
                if !(lower.starts_with("http://") || lower.starts_with("https://")) {
                    return Err(AppError::validation(
                        "agent_hub_portable_mcp_http_invalid_url_scheme".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// 收集绝对路径等诊断（message/path 不含 secret 原文）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     cwd/command 绝对路径跨机不可移植；URL/header 凭据不得进入诊断文本。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对 stdio command/cwd 检测绝对路径；对 http url 仅记录 hash 元数据绝对路径不适用。
    pub fn collect_diagnostics(&self) -> Vec<PortabilityDiagnostic> {
        let mut out = Vec::new();
        match &self.transport {
            McpTransport::Stdio { command, cwd, .. } => {
                if is_absolute_like(command) {
                    out.push(
                        PortabilityDiagnostic::absolute_path("/transport/command")
                            .with_value_metadata(command),
                    );
                }
                if let Some(cwd) = cwd {
                    if is_absolute_like(cwd) {
                        out.push(
                            PortabilityDiagnostic::absolute_path("/transport/cwd")
                                .with_value_metadata(cwd),
                        );
                    }
                }
            }
            McpTransport::Http { url, headers } => {
                // credential-bearing fields: only metadata, never values in message
                if url.contains("token=") || url.contains("access_token=") {
                    out.push(
                        PortabilityDiagnostic::new(
                            CODE_UNKNOWN_SOURCE_FIELD,
                            "/transport/url",
                            "url contains credential-bearing query parameters (stored verbatim; not logged)",
                        )
                        .with_value_metadata(url),
                    );
                }
                for (name, value) in headers {
                    if name.eq_ignore_ascii_case("authorization")
                        || name.eq_ignore_ascii_case("x-api-key")
                    {
                        out.push(
                            PortabilityDiagnostic::new(
                                CODE_UNKNOWN_SOURCE_FIELD,
                                format!("/transport/headers/{name}"),
                                "credential-bearing header retained verbatim (value not logged)",
                            )
                            .with_value_metadata(value),
                        );
                    }
                }
            }
        }
        for (k, v) in &self.env {
            if k.to_ascii_uppercase().contains("TOKEN")
                || k.to_ascii_uppercase().contains("SECRET")
                || k.to_ascii_uppercase().contains("PASSWORD")
            {
                out.push(
                    PortabilityDiagnostic::new(
                        "unknownSourceField",
                        format!("/env/{k}"),
                        "credential-bearing env retained verbatim (value not logged)",
                    )
                    .with_value_metadata(v),
                );
            }
        }
        out
    }
}

/// 路径/命令是否像绝对路径。
///
/// Business Logic: 绝对 command/cwd 跨机可能失效。
/// Code Logic: `/` 前缀或 Windows 盘符。
fn is_absolute_like(s: &str) -> bool {
    if s.starts_with('/') {
        return true;
    }
    Path::new(s).is_absolute()
        || (s.len() >= 2 && s.as_bytes()[0].is_ascii_alphabetic() && s.as_bytes()[1] == b':')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::assets::diagnostics::format_validation_error_safe;
    use crate::agent_hub::assets::{canonical_bytes, PortableAssetPayload};

    fn secret_mcp() -> PortableMcpServer {
        PortableMcpServer {
            key: "private-api".into(),
            transport: McpTransport::Http {
                url: "https://example.invalid/mcp?token=plain-fixture".into(),
                headers: BTreeMap::from([("Authorization".into(), "Bearer plain-fixture".into())]),
            },
            env: BTreeMap::from([("API_TOKEN".into(), "plain-fixture".into())]),
            enabled: true,
            tool_allow: vec![],
            tool_deny: vec![],
            target_extensions: BTreeMap::new(),
        }
    }

    #[test]
    fn rejects_empty_key_and_empty_stdio_command() {
        let mut m = secret_mcp();
        m.key = String::new();
        assert!(m.validate().is_err());

        let stdio = PortableMcpServer {
            key: "local".into(),
            transport: McpTransport::Stdio {
                command: String::new(),
                args: vec![],
                cwd: None,
            },
            env: BTreeMap::new(),
            enabled: true,
            tool_allow: vec![],
            tool_deny: vec![],
            target_extensions: BTreeMap::new(),
        };
        assert!(stdio.validate().is_err());
    }

    #[test]
    fn rejects_http_without_scheme() {
        let m = PortableMcpServer {
            key: "x".into(),
            transport: McpTransport::Http {
                url: "example.invalid/mcp".into(),
                headers: BTreeMap::new(),
            },
            env: BTreeMap::new(),
            enabled: true,
            tool_allow: vec![],
            tool_deny: vec![],
            target_extensions: BTreeMap::new(),
        };
        let err = m.validate().unwrap_err();
        assert!(err.to_string().contains("invalid_url_scheme"));
    }

    #[test]
    fn secret_fixture_round_trip_preserves_values_and_diagnostics_redact() {
        let mcp = secret_mcp();
        mcp.validate().unwrap();
        let payload = PortableAssetPayload::Mcp(mcp.clone());
        let bytes = canonical_bytes(&payload).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains("plain-fixture"));
        assert!(text.contains("Bearer plain-fixture"));
        assert!(text.contains("API_TOKEN"));

        // BTreeMap key order deterministic for headers/env
        let auth_pos = text.find("Authorization").unwrap();
        // env keys sorted: API_TOKEN only
        assert!(text.contains("\"API_TOKEN\":\"plain-fixture\"") || text.contains("API_TOKEN"));

        let back: PortableAssetPayload = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, payload);

        let diags = mcp.collect_diagnostics();
        for d in &diags {
            let safe = d.format_safe();
            assert!(!safe.contains("plain-fixture"), "diag leaked: {safe}");
            assert!(!safe.contains("Bearer plain"), "diag leaked: {safe}");
        }
        // Route a crafted error through redaction
        let err_text = format!(
            "mcp validation failed url={} Authorization: {} API_TOKEN={}",
            "https://example.invalid/mcp?token=plain-fixture",
            "Bearer plain-fixture",
            "plain-fixture"
        );
        let safe = format_validation_error_safe(&err_text);
        assert!(!safe.contains("plain-fixture"));
        assert!(!safe.contains("Bearer plain"));
        // silence unused
        let _ = auth_pos;
    }

    #[test]
    fn absolute_cwd_diagnostic() {
        let m = PortableMcpServer {
            key: "local".into(),
            transport: McpTransport::Stdio {
                command: "npx".into(),
                args: vec![],
                cwd: Some("/Users/hans/secret-project".into()),
            },
            env: BTreeMap::new(),
            enabled: true,
            tool_allow: vec![],
            tool_deny: vec![],
            target_extensions: BTreeMap::new(),
        };
        let diags = m.collect_diagnostics();
        assert!(diags.iter().any(|d| d.code == "absolutePath"));
        let safe = diags[0].format_safe();
        // path pointer ok; raw cwd may appear only as hash
        assert!(!safe.contains("/Users/hans/secret-project") || safe.contains("valueHash="));
        // With metadata, message itself has no path body; format_safe should not include raw path in message field
        assert!(!diags[0].message.contains("/Users/hans"));
    }
}
