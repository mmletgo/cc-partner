//! agent_hub/targets/portable/frontmatter — frontmatter 解析 helper
//!
//! Business Logic（为什么需要这个模块）:
//!     Skill/Command/Agent Markdown 的元数据都写在 YAML frontmatter 中，Hub 需要统一的
//!     解析入口：已知键进入 payload，未知键保留进 target_extensions 不得丢弃；部分
//!     生成器会把中文写成双引号 JSON `\uXXXX` 转义，必须解码后才能在 Hub 正常显示。
//!
//! Code Logic（这个模块做什么）:
//!     从原 portable.rs 拆出：`parse_simple_frontmatter` 简单 `key: value` 行解析、
//!     双引号/单引号标量解码、JSON 字符串内部转义展开、已知键清单（KNOWN_*_KEYS，
//!     供 skill_scan / markdown_scan 复用）与 `unknown_fields_extension` 未知键诊断收集。

use crate::agent_hub::{
    assets::{PortabilityDiagnostic, CODE_UNKNOWN_SOURCE_FIELD},
    models::AgentTarget,
};
use serde_json::Value;
use std::collections::BTreeMap;

/// 解析 YAML frontmatter（`---` ... `---`）为 key→string map + 未知键列表。
///
/// Business Logic: Skill/Command/Agent 元数据在 frontmatter；未知键不得丢弃。
///     Codex/部分生成器会把中文写成双引号 JSON `\uXXXX`，必须解码后才能在 Hub 正常显示。
/// Code Logic: 仅处理简单 `key: value` 行，不引入完整 YAML 依赖；双引号标量按 JSON 字符串解码。
pub fn parse_simple_frontmatter(text: &str) -> (BTreeMap<String, String>, Vec<String>, &str) {
    let trimmed = text.strip_prefix('\u{feff}').unwrap_or(text);
    if !trimmed.starts_with("---") {
        return (BTreeMap::new(), Vec::new(), text);
    }
    let rest = &trimmed[3..];
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))
        .unwrap_or(rest);
    let Some((front, body)) = rest
        .split_once("\n---")
        .or_else(|| rest.split_once("\r\n---"))
    else {
        return (BTreeMap::new(), Vec::new(), text);
    };
    let body = body
        .strip_prefix('\n')
        .or_else(|| body.strip_prefix("\r\n"))
        .unwrap_or(body);
    let mut map = BTreeMap::new();
    let mut unknown_order = Vec::new();
    for line in front.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let key = k.trim().to_string();
        let val = decode_frontmatter_scalar(v);
        if !map.contains_key(&key) {
            unknown_order.push(key.clone());
        }
        map.insert(key, val);
    }
    (map, unknown_order, body)
}

/// 解码 frontmatter 单行标量。
///
/// Business Logic: 生成器常把中文 description 写成 `"\u7528..."`；只剥引号会把转义原文展示给用户。
/// Code Logic: 双引号优先 `serde_json` 解码（含 `\uXXXX` / `\"` / `\\`）；失败则剥引号并手工展开
///     常见 JSON 转义。单引号只把 YAML `''` 还原成 `'`。未加引号的 UTF-8 原文原样返回。
fn decode_frontmatter_scalar(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        if let Ok(decoded) = serde_json::from_str::<String>(trimmed) {
            return decoded;
        }
        return unescape_json_string_inner(&trimmed[1..trimmed.len() - 1]);
    }
    if trimmed.len() >= 2 && trimmed.starts_with('\'') && trimmed.ends_with('\'') {
        return trimmed[1..trimmed.len() - 1].replace("''", "'");
    }
    trimmed.to_string()
}

/// 把 JSON/YAML 双引号字符串的内部转义展开为真实字符。
///
/// Business Logic: `serde_json` 失败时（YAML 独有转义）仍应尽量把 `\uXXXX` 显示成汉字。
/// Code Logic: 识别 `\uXXXX`、`\"`、`\\`、`\n`/`\r`/`\t`、`\/`；非法序列原样保留。
pub(super) fn unescape_json_string_inner(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('/') => out.push('/'),
            Some('u') => {
                let hex: String = (0..4).filter_map(|_| chars.next()).collect();
                if hex.len() == 4 {
                    if let Ok(code) = u32::from_str_radix(&hex, 16) {
                        if let Some(decoded) = char::from_u32(code) {
                            out.push(decoded);
                            continue;
                        }
                    }
                }
                out.push('\\');
                out.push('u');
                out.push_str(&hex);
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// 已知 frontmatter 键（不进入 unknown）。
pub(super) const KNOWN_SKILL_KEYS: &[&str] = &["name", "description"];
pub(super) const KNOWN_COMMAND_KEYS: &[&str] = &[
    "name",
    "description",
    "argument-hint",
    "argument_hint",
    "arguments",
    "allowed-tools",
    "model",
];
pub(super) const KNOWN_AGENT_KEYS: &[&str] = &[
    "name",
    "description",
    "tools",
    "model",
    "mode",
    "permission",
    "permissions",
    "provider",
];

/// 从 map 收集未知键诊断与 target_extensions 对象。
///
/// Business Logic: 未知字段保留在 source target extension，并记 unknownSourceField。
/// Code Logic: 过滤 known keys；剩余进 JSON object + 诊断。
pub fn unknown_fields_extension(
    target: AgentTarget,
    fields: &BTreeMap<String, String>,
    known: &[&str],
    pointer_prefix: &str,
) -> (BTreeMap<AgentTarget, Value>, Vec<PortabilityDiagnostic>) {
    let mut ext_obj = serde_json::Map::new();
    let mut diags = Vec::new();
    for (k, v) in fields {
        if known.contains(&k.as_str()) {
            continue;
        }
        ext_obj.insert(k.clone(), Value::String(v.clone()));
        diags.push(
            PortabilityDiagnostic::new(
                CODE_UNKNOWN_SOURCE_FIELD,
                format!("{pointer_prefix}/{k}"),
                "unknown source frontmatter field retained in target_extensions",
            )
            .with_value_metadata(v),
        );
    }
    let mut extensions = BTreeMap::new();
    if !ext_obj.is_empty() {
        extensions.insert(target, Value::Object(ext_obj));
    }
    (extensions, diags)
}
