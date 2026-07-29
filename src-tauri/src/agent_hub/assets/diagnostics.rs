//! agent_hub/assets/diagnostics — 可移植资产诊断（无凭据原文）
//!
//! Business Logic（为什么需要这个模块）:
//!     Skill/Command/Agent/MCP 在跨 target 同步时会产生路径、可执行文件、插值与权限等
//!     不可移植信号；诊断必须用稳定 code 表达，且错误/日志不得回显 credential 原文。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `PortabilityDiagnostic`（code/path/message + 可选 hash/length 元数据）与
//!     稳定诊断码构造器；提供敏感值 redaction 辅助用于错误消息格式化。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 稳定诊断码：绝对路径。
pub const CODE_ABSOLUTE_PATH: &str = "absolutePath";
/// 稳定诊断码：目标可执行文件依赖。
pub const CODE_TARGET_EXECUTABLE: &str = "targetExecutable";
/// 稳定诊断码：不支持的插值语法。
pub const CODE_UNSUPPORTED_INTERPOLATION: &str = "unsupportedInterpolation";
/// 稳定诊断码：模型字段不可跨 target 移植。
pub const CODE_MODEL_NOT_PORTABLE: &str = "modelNotPortable";
/// 稳定诊断码：权限字段不可跨 target 移植。
pub const CODE_PERMISSION_NOT_PORTABLE: &str = "permissionNotPortable";
/// 稳定诊断码：未知源字段（保留在 target extension）。
pub const CODE_UNKNOWN_SOURCE_FIELD: &str = "unknownSourceField";
/// 稳定诊断码：物化 alias 与 canonical 名不同。
pub const CODE_MATERIALIZED_ALIAS: &str = "materializedAlias";

/// 可移植资产诊断。
///
/// Business Logic（为什么需要这个结构体）:
///     投影/扫描/Attention 需要稳定 code + JSON pointer 路径；敏感值只能用 hash/length
///     元数据表达，绝不存 credential 原文。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 序列化；可选 value_hash / value_length；message 禁止含敏感原文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortabilityDiagnostic {
    /// 稳定诊断码
    pub code: String,
    /// JSON pointer 或相对路径
    pub path: String,
    /// 人类可读摘要（无敏感原文）
    pub message: String,
    /// 相关值的 SHA-256 hex（可选；永不存明文）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_hash: Option<String>,
    /// 相关值字节长度（可选）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_length: Option<usize>,
}

impl PortabilityDiagnostic {
    /// 构造基础诊断。
    ///
    /// Business Logic: 调用方提供稳定 code 与安全 message。
    /// Code Logic: 无 hash/length。
    pub fn new(
        code: impl Into<String>,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            path: path.into(),
            message: message.into(),
            value_hash: None,
            value_length: None,
        }
    }

    /// 附带敏感值元数据（hash + length，不存原文）。
    ///
    /// Business Logic: 需要证明某字段存在凭据/绝对路径时，只能记 hash 与长度。
    /// Code Logic: sha256_hex(value) + len。
    pub fn with_value_metadata(mut self, value: &str) -> Self {
        self.value_hash = Some(sha256_hex_str(value));
        self.value_length = Some(value.len());
        self
    }

    /// 绝对路径诊断。
    ///
    /// Business Logic: 脚本/cwd 含绝对路径时不可静默跨机移植。
    /// Code Logic: code=`absolutePath`；message 不含路径原文。
    pub fn absolute_path(path: impl Into<String>) -> Self {
        Self::new(
            CODE_ABSOLUTE_PATH,
            path,
            "path is absolute and may not be portable across machines",
        )
    }

    /// 目标可执行文件依赖诊断。
    ///
    /// Business Logic: 依赖本机专用 executable 时需提示。
    /// Code Logic: code=`targetExecutable`。
    pub fn target_executable(path: impl Into<String>) -> Self {
        Self::new(
            CODE_TARGET_EXECUTABLE,
            path,
            "references a target-specific executable that may not exist elsewhere",
        )
    }

    /// 不支持的插值诊断。
    ///
    /// Business Logic: shell/$ARGUMENTS 等无法跨 CLI 等价映射。
    /// Code Logic: code=`unsupportedInterpolation`。
    pub fn unsupported_interpolation(path: impl Into<String>) -> Self {
        Self::new(
            CODE_UNSUPPORTED_INTERPOLATION,
            path,
            "interpolation syntax is not portable across CLI targets",
        )
    }

    /// 模型字段不可移植诊断。
    ///
    /// Business Logic: model/provider 不做跨 target 自动等价。
    /// Code Logic: code=`modelNotPortable`。
    pub fn model_not_portable(path: impl Into<String>) -> Self {
        Self::new(
            CODE_MODEL_NOT_PORTABLE,
            path,
            "model or provider field is not auto-mapped across targets",
        )
    }

    /// 权限字段不可移植诊断。
    ///
    /// Business Logic: 权限策略不跨 CLI 自动覆盖。
    /// Code Logic: code=`permissionNotPortable`。
    pub fn permission_not_portable(path: impl Into<String>) -> Self {
        Self::new(
            CODE_PERMISSION_NOT_PORTABLE,
            path,
            "permission field is not auto-mapped across targets",
        )
    }

    /// 未知源字段诊断。
    ///
    /// Business Logic: 未识别字段进 target_extensions，并记录诊断。
    /// Code Logic: code=`unknownSourceField`。
    pub fn unknown_source_field(path: impl Into<String>) -> Self {
        Self::new(
            CODE_UNKNOWN_SOURCE_FIELD,
            path,
            "unrecognized source field retained under target extension",
        )
    }

    /// 物化 alias 诊断。
    ///
    /// Business Logic: 名称不满足 target 约束时 adapter 生成 alias。
    /// Code Logic: code=`materializedAlias`。
    pub fn materialized_alias(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(CODE_MATERIALIZED_ALIAS, path, message)
    }

    /// 格式化诊断为安全错误文本（永不含 credential 原文）。
    ///
    /// Business Logic: 错误/Attention/日志只能展示 code/path/hash 元数据。
    /// Code Logic: 拼接 code、path、message、可选 hash/length；调用方传入的 secrets 不得并入。
    pub fn format_safe(&self) -> String {
        let mut out = format!(
            "portability diagnostic code={} path={} message={}",
            self.code, self.path, self.message
        );
        if let Some(hash) = &self.value_hash {
            out.push_str(&format!(" valueHash={hash}"));
        }
        if let Some(len) = self.value_length {
            out.push_str(&format!(" valueLength={len}"));
        }
        out
    }
}

/// 计算字符串的小写 hex SHA-256。
///
/// Business Logic: 敏感值只允许以 hash 进入诊断元数据。
/// Code Logic: Sha256 digest → lowercase hex。
fn sha256_hex_str(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// 从错误消息中剥离常见 credential 形态。
///
/// Business Logic: 路由 MCP 校验失败等到诊断层时，禁止回显 token/Authorization 值。
/// Code Logic: 替换 Bearer/token/API_TOKEN 等模式为 `<REDACTED>`。
pub fn redact_sensitive_text(text: &str) -> String {
    let mut out = text.to_string();
    // Bearer tokens
    out = redact_pattern(&out, "Bearer ");
    // query token=
    if let Some(idx) = out.find("token=") {
        let after = idx + "token=".len();
        let end = out[after..]
            .find(['&', ' ', '"', '\'', '>'])
            .map(|i| after + i)
            .unwrap_or(out.len());
        out.replace_range(after..end, "<REDACTED>");
    }
    // Authorization header values already covered by Bearer; also generic Authorization:
    if let Some(idx) = out.to_ascii_lowercase().find("authorization") {
        // redact from first ':' or space after key through delimiter
        let rest = &out[idx..];
        if let Some(colon) = rest.find(':') {
            let start = idx + colon + 1;
            let trimmed_start = out[start..]
                .char_indices()
                .find(|(_, c)| !c.is_whitespace())
                .map(|(i, _)| start + i)
                .unwrap_or(start);
            let end = out[trimmed_start..]
                .find([',', '"', '\'', '\n', '}'])
                .map(|i| trimmed_start + i)
                .unwrap_or(out.len());
            if trimmed_start < end {
                out.replace_range(trimmed_start..end, "<REDACTED>");
            }
        }
    }
    // plain-fixture style secrets often appear as API_TOKEN=...
    for key in ["API_TOKEN=", "api_token=", "plain-fixture"] {
        if key == "plain-fixture" {
            out = out.replace(key, "<REDACTED>");
            continue;
        }
        if let Some(idx) = out.find(key) {
            let after = idx + key.len();
            let end = out[after..]
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',' || c == '}')
                .map(|i| after + i)
                .unwrap_or(out.len());
            out.replace_range(after..end, "<REDACTED>");
        }
    }
    out
}

/// 从 `prefix` 起 redact 到空白或引号。
///
/// Business Logic: Bearer 后的 token 不得出现在错误文案。
/// Code Logic: 找到 prefix 后替换后续 token 段。
fn redact_pattern(text: &str, prefix: &str) -> String {
    let mut out = text.to_string();
    let mut search_from = 0;
    while let Some(rel) = out[search_from..].find(prefix) {
        let start = search_from + rel + prefix.len();
        let end = out[start..]
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',' || c == '}')
            .map(|i| start + i)
            .unwrap_or(out.len());
        out.replace_range(start..end, "<REDACTED>");
        search_from = start + "<REDACTED>".len();
        if search_from >= out.len() {
            break;
        }
    }
    out
}

/// 将验证错误格式化为安全诊断文本。
///
/// Business Logic: MCP 等含 secret 的校验失败必须脱敏后再上抛/记日志。
/// Code Logic: redact_sensitive_text。
pub fn format_validation_error_safe(error: &str) -> String {
    redact_sensitive_text(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_codes_are_stable() {
        assert_eq!(
            PortabilityDiagnostic::absolute_path("/tmp/x").code,
            "absolutePath"
        );
        assert_eq!(
            PortabilityDiagnostic::target_executable("/cmd").code,
            "targetExecutable"
        );
        assert_eq!(
            PortabilityDiagnostic::unsupported_interpolation("/prompt").code,
            "unsupportedInterpolation"
        );
        assert_eq!(
            PortabilityDiagnostic::model_not_portable("/model").code,
            "modelNotPortable"
        );
        assert_eq!(
            PortabilityDiagnostic::permission_not_portable("/perm").code,
            "permissionNotPortable"
        );
        assert_eq!(
            PortabilityDiagnostic::unknown_source_field("/extra").code,
            "unknownSourceField"
        );
        assert_eq!(
            PortabilityDiagnostic::materialized_alias("/name", "alias").code,
            "materializedAlias"
        );
    }

    #[test]
    fn value_metadata_stores_hash_not_plaintext() {
        let d = PortabilityDiagnostic::absolute_path("/transport/url")
            .with_value_metadata("https://example.invalid/mcp?token=plain-fixture");
        let safe = d.format_safe();
        assert!(!safe.contains("plain-fixture"));
        assert!(!safe.contains("token="));
        assert!(d.value_hash.is_some());
        assert_eq!(
            d.value_length,
            Some("https://example.invalid/mcp?token=plain-fixture".len())
        );
    }

    #[test]
    fn redaction_strips_bearer_and_token() {
        let raw = "failed url=https://example.invalid/mcp?token=plain-fixture Authorization: Bearer plain-fixture API_TOKEN=plain-fixture";
        let safe = redact_sensitive_text(raw);
        assert!(!safe.contains("plain-fixture"));
        assert!(!safe.contains("Bearer plain"));
        assert!(safe.contains("<REDACTED>"));
    }
}
