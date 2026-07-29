//! agent_hub/config_patch/jsonc — ownership-aware JSON/JSONC span patcher
//!
//! Business Logic（为什么需要这个模块）:
//!     Claude / OpenCode 配置常为 JSONC（注释、尾逗号）。Hub 只能替换 owned member 的
//!     leaf span，保留注释、CRLF、无关 plugin 配置与键序。
//!
//! Code Logic（这个模块做什么）:
//!     本地 tokenizer（string/escape/punct/ws/comment）+ object member span 索引；
//!     仅替换目标 key 的 value span 或插入/删除 member；无外部 JSONC formatter 依赖。

use crate::agent_hub::config_patch::{
    blocked_result, check_cas, conflict_result, missing_owned_value, owned_value_from_json,
    value_content_hash, ConfigOwnedPathMeta, ConfigPatchOutcome, ConfigPathDiff,
    ManagedConfigPatch, OwnedConfigValue, PatchedConfig, SemanticConfigPatcher,
};
use crate::agent_hub::object_store::sha256_hex;
use crate::error::AppError;

/// JSONC 语义配置 patcher。
///
/// Business Logic（为什么需要这个结构体）:
///     MCP / plugin 配置的 owned member 必须 span 级改写。
///
/// Code Logic（这个结构体做什么）:
///     无状态；实现 SemanticConfigPatcher。
#[derive(Debug, Default, Clone, Copy)]
pub struct JsoncConfigPatcher;

/// 词法 token 种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    Comma,
    String,
    Number,
    Ident, // true/false/null
    LineComment,
    BlockComment,
    Whitespace,
}

/// 带字节 span 的 token。
#[derive(Debug, Clone)]
struct Tok {
    kind: Kind,
    start: usize,
    end: usize,
}

/// object 内一个 member 的 span 信息。
#[derive(Debug, Clone)]
struct MemberSpan {
    /// 未转义 key
    key: String,
    /// key 字符串 token start（含引号）
    key_start: usize,
    /// key 字符串 token end
    key_end: usize,
    /// value 起始字节（含）
    value_start: usize,
    /// value 结束字节（不含）
    value_end: usize,
    /// 整个 member 起始（key 前 leading ws/comment 后的 key 起点，或含前导逗号区域由删除逻辑处理）
    member_start: usize,
    /// 整个 member 结束（value_end 或含 trailing comma）
    member_end: usize,
    /// 是否有 trailing comma（member_end 可越过）
    has_trailing_comma: bool,
}

/// 解析出的 object 层。
#[derive(Debug, Clone)]
struct ObjectLayer {
    /// `{` 位置
    open: usize,
    /// `}` 位置
    close: usize,
    members: Vec<MemberSpan>,
}

impl JsoncConfigPatcher {
    /// 构造 patcher。
    pub fn new() -> Self {
        Self
    }

    /// 词法扫描。
    ///
    /// Business Logic: 注释/字符串/空白必须识别才能保留。
    /// Code Logic: 手写状态机，失败返回 Err。
    fn tokenize(bytes: &[u8]) -> Result<Vec<Tok>, String> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            match b {
                b'{' => {
                    out.push(Tok {
                        kind: Kind::LBrace,
                        start: i,
                        end: i + 1,
                    });
                    i += 1;
                }
                b'}' => {
                    out.push(Tok {
                        kind: Kind::RBrace,
                        start: i,
                        end: i + 1,
                    });
                    i += 1;
                }
                b'[' => {
                    out.push(Tok {
                        kind: Kind::LBracket,
                        start: i,
                        end: i + 1,
                    });
                    i += 1;
                }
                b']' => {
                    out.push(Tok {
                        kind: Kind::RBracket,
                        start: i,
                        end: i + 1,
                    });
                    i += 1;
                }
                b':' => {
                    out.push(Tok {
                        kind: Kind::Colon,
                        start: i,
                        end: i + 1,
                    });
                    i += 1;
                }
                b',' => {
                    out.push(Tok {
                        kind: Kind::Comma,
                        start: i,
                        end: i + 1,
                    });
                    i += 1;
                }
                b'"' => {
                    let start = i;
                    i += 1;
                    while i < bytes.len() {
                        match bytes[i] {
                            b'\\' => {
                                i += 1;
                                if i >= bytes.len() {
                                    return Err("invalid_string_escape".into());
                                }
                                i += 1;
                            }
                            b'"' => {
                                i += 1;
                                break;
                            }
                            _ => i += 1,
                        }
                    }
                    if i == start + 1 || bytes[i - 1] != b'"' {
                        // 可能未闭合
                        if i >= bytes.len()
                            && (start + 1 >= bytes.len() || bytes[bytes.len() - 1] != b'"')
                        {
                            return Err("unclosed_string".into());
                        }
                    }
                    out.push(Tok {
                        kind: Kind::String,
                        start,
                        end: i,
                    });
                }
                b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                    let start = i;
                    i += 2;
                    while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
                        i += 1;
                    }
                    out.push(Tok {
                        kind: Kind::LineComment,
                        start,
                        end: i,
                    });
                }
                b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                    let start = i;
                    i += 2;
                    let mut closed = false;
                    while i + 1 < bytes.len() {
                        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                            i += 2;
                            closed = true;
                            break;
                        }
                        i += 1;
                    }
                    if !closed {
                        return Err("unclosed_block_comment".into());
                    }
                    out.push(Tok {
                        kind: Kind::BlockComment,
                        start,
                        end: i,
                    });
                }
                b' ' | b'\t' | b'\n' | b'\r' => {
                    let start = i;
                    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r') {
                        i += 1;
                    }
                    out.push(Tok {
                        kind: Kind::Whitespace,
                        start,
                        end: i,
                    });
                }
                b'-' | b'0'..=b'9' => {
                    let start = i;
                    i += 1;
                    while i < bytes.len()
                        && matches!(bytes[i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                    {
                        i += 1;
                    }
                    out.push(Tok {
                        kind: Kind::Number,
                        start,
                        end: i,
                    });
                }
                b't' | b'f' | b'n' => {
                    let start = i;
                    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                    let word = std::str::from_utf8(&bytes[start..i]).unwrap_or("");
                    if !matches!(word, "true" | "false" | "null") {
                        return Err(format!("invalid_ident:{word}"));
                    }
                    out.push(Tok {
                        kind: Kind::Ident,
                        start,
                        end: i,
                    });
                }
                _ => {
                    return Err(format!("unexpected_byte_at_{i}"));
                }
            }
        }
        Ok(out)
    }

    /// 跳过空白与注释 token 索引。
    fn skip_trivia(tokens: &[Tok], mut i: usize) -> usize {
        while i < tokens.len()
            && matches!(
                tokens[i].kind,
                Kind::Whitespace | Kind::LineComment | Kind::BlockComment
            )
        {
            i += 1;
        }
        i
    }

    /// 解析 value 并返回结束 token 索引（exclusive）与 value 字节 end。
    ///
    /// Business Logic: 正确识别嵌套 object/array 边界。
    /// Code Logic: 递归消费 tokens。
    fn parse_value(tokens: &[Tok], start: usize) -> Result<(usize, usize), String> {
        let i = Self::skip_trivia(tokens, start);
        if i >= tokens.len() {
            return Err("unexpected_eof_value".into());
        }
        match tokens[i].kind {
            Kind::String | Kind::Number | Kind::Ident => Ok((i + 1, tokens[i].end)),
            Kind::LBrace => {
                let (_obj, next) = Self::parse_object(tokens, i)?;
                Ok((next, tokens[next - 1].end))
            }
            Kind::LBracket => {
                let next = Self::parse_array(tokens, i)?;
                Ok((next, tokens[next - 1].end))
            }
            other => Err(format!("expected_value_got_{other:?}")),
        }
    }

    fn parse_array(tokens: &[Tok], start: usize) -> Result<usize, String> {
        if tokens.get(start).map(|t| t.kind) != Some(Kind::LBracket) {
            return Err("expected_lbracket".into());
        }
        let mut i = start + 1;
        loop {
            i = Self::skip_trivia(tokens, i);
            if i >= tokens.len() {
                return Err("unclosed_array".into());
            }
            if tokens[i].kind == Kind::RBracket {
                return Ok(i + 1);
            }
            let (next, _) = Self::parse_value(tokens, i)?;
            i = next;
            i = Self::skip_trivia(tokens, i);
            if i < tokens.len() && tokens[i].kind == Kind::Comma {
                i += 1;
                continue;
            }
            if i < tokens.len() && tokens[i].kind == Kind::RBracket {
                return Ok(i + 1);
            }
            return Err("invalid_array".into());
        }
    }

    /// 解析 object，返回 layer + 结束 token 索引。
    fn parse_object(tokens: &[Tok], start: usize) -> Result<(ObjectLayer, usize), String> {
        if tokens.get(start).map(|t| t.kind) != Some(Kind::LBrace) {
            return Err("expected_lbrace".into());
        }
        let open = tokens[start].start;
        let mut i = start + 1;
        let mut members = Vec::new();
        loop {
            i = Self::skip_trivia(tokens, i);
            if i >= tokens.len() {
                return Err("unclosed_object".into());
            }
            if tokens[i].kind == Kind::RBrace {
                let close = tokens[i].start;
                return Ok((
                    ObjectLayer {
                        open,
                        close,
                        members,
                    },
                    i + 1,
                ));
            }
            if tokens[i].kind != Kind::String {
                return Err("expected_object_key".into());
            }
            let key_tok = &tokens[i];
            let key = Self::decode_json_string_slice(
                // bytes not available here — decode later in apply with full text
                // We store raw span; decode when we have bytes.
                b"",
                key_tok.start,
                key_tok.end,
            )
            .unwrap_or_default();
            // key placeholder filled by caller with real bytes; store empty for now and re-decode
            let key_start = key_tok.start;
            let key_end = key_tok.end;
            let member_start = key_start;
            i += 1;
            i = Self::skip_trivia(tokens, i);
            if i >= tokens.len() || tokens[i].kind != Kind::Colon {
                return Err("expected_colon".into());
            }
            i += 1;
            i = Self::skip_trivia(tokens, i);
            let value_start = if i < tokens.len() {
                tokens[i].start
            } else {
                return Err("expected_value".into());
            };
            let (after_val, value_end) = Self::parse_value(tokens, i)?;
            i = after_val;
            let mut member_end = value_end;
            let mut has_trailing_comma = false;
            let after_ws = Self::skip_trivia(tokens, i);
            if after_ws < tokens.len() && tokens[after_ws].kind == Kind::Comma {
                has_trailing_comma = true;
                member_end = tokens[after_ws].end;
                i = after_ws + 1;
            } else {
                i = after_ws;
            }
            members.push(MemberSpan {
                key, // may be empty; refilled below if needed
                key_start,
                key_end,
                value_start,
                value_end,
                member_start,
                member_end,
                has_trailing_comma,
            });
            // loop continues; RBrace handled at top
            let _ = member_start;
        }
    }

    /// 用真实 bytes 重新 decode object member keys。
    fn fill_keys(bytes: &[u8], layer: &mut ObjectLayer) -> Result<(), String> {
        for m in &mut layer.members {
            m.key = Self::decode_json_string_slice(bytes, m.key_start, m.key_end)?;
        }
        Ok(())
    }

    fn decode_json_string_slice(bytes: &[u8], start: usize, end: usize) -> Result<String, String> {
        if end <= start + 1 || bytes.get(start) != Some(&b'"') || bytes.get(end - 1) != Some(&b'"')
        {
            // allow empty bytes during structural parse
            if bytes.is_empty() {
                return Ok(String::new());
            }
            return Err("invalid_string_span".into());
        }
        let inner = &bytes[start + 1..end - 1];
        let mut out = String::new();
        let mut i = 0;
        while i < inner.len() {
            match inner[i] {
                b'\\' => {
                    i += 1;
                    if i >= inner.len() {
                        return Err("bad_escape".into());
                    }
                    match inner[i] {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            if i + 4 >= inner.len() {
                                return Err("bad_unicode_escape".into());
                            }
                            let hex = std::str::from_utf8(&inner[i + 1..i + 5])
                                .map_err(|_| "bad_unicode_escape".to_string())?;
                            let cp = u32::from_str_radix(hex, 16)
                                .map_err(|_| "bad_unicode_escape".to_string())?;
                            out.push(
                                char::from_u32(cp)
                                    .ok_or_else(|| "bad_unicode_escape".to_string())?,
                            );
                            i += 4;
                        }
                        _ => return Err("bad_escape".into()),
                    }
                    i += 1;
                }
                c => {
                    out.push(c as char);
                    i += 1;
                }
            }
        }
        Ok(out)
    }

    /// 剥离 JSONC 注释与尾逗号得到可 serde 解析的 JSON 文本。
    ///
    /// Business Logic: inspect 需要语义值；不修改原字节。
    /// Code Logic: 基于 token 重建无注释文本，去掉对象/数组尾逗号。
    fn to_json_text(bytes: &[u8], tokens: &[Tok]) -> Result<String, String> {
        let mut out = String::new();
        let mut i = 0;
        while i < tokens.len() {
            let t = &tokens[i];
            match t.kind {
                Kind::LineComment | Kind::BlockComment => {
                    // drop
                }
                Kind::Comma => {
                    // look ahead for } or ]
                    let j = Self::skip_trivia(tokens, i + 1);
                    if j < tokens.len() && matches!(tokens[j].kind, Kind::RBrace | Kind::RBracket) {
                        // trailing comma — drop
                    } else {
                        out.push(',');
                    }
                }
                Kind::Whitespace => {
                    // 保留最小空白以便 debug，但 serde 不依赖
                    let slice = &bytes[t.start..t.end];
                    out.push_str(std::str::from_utf8(slice).unwrap_or(" "));
                }
                _ => {
                    let slice = &bytes[t.start..t.end];
                    out.push_str(
                        std::str::from_utf8(slice).map_err(|_| "invalid_utf8".to_string())?,
                    );
                }
            }
            i += 1;
        }
        Ok(out)
    }

    /// 解析根 object layer。
    fn parse_root_object(bytes: &[u8]) -> Result<(Vec<Tok>, ObjectLayer), String> {
        let tokens = Self::tokenize(bytes)?;
        let start = Self::skip_trivia(&tokens, 0);
        if start >= tokens.len() || tokens[start].kind != Kind::LBrace {
            return Err("root_not_object".into());
        }
        let (mut layer, end) = Self::parse_object(&tokens, start)?;
        let after = Self::skip_trivia(&tokens, end);
        if after != tokens.len() {
            return Err("trailing_tokens".into());
        }
        Self::fill_keys(bytes, &mut layer)?;
        Ok((tokens, layer))
    }

    /// 导航 path 找到 leaf 所在 object layer 与 member 索引。
    ///
    /// Business Logic: 父必须是 object；中间路径缺失时 insert 路径。
    /// Code Logic: 递归 re-parse value spans。
    fn find_member_at_path(
        bytes: &[u8],
        tokens: &[Tok],
        root: &ObjectLayer,
        path: &[String],
    ) -> Result<FindResult, String> {
        if path.is_empty() {
            return Err("empty_path".into());
        }
        let mut layer = root.clone();
        for (depth, key) in path.iter().enumerate() {
            let idx = layer.members.iter().position(|m| &m.key == key);
            if depth == path.len() - 1 {
                return Ok(FindResult {
                    parent: layer,
                    member_index: idx,
                    leaf_key: key.clone(),
                });
            }
            let Some(mi) = idx else {
                return Err(format!("missing_parent:{key}"));
            };
            let member = &layer.members[mi];
            // parse nested object at value span
            // tokenize_in_range 已将 token span 转为绝对字节坐标，勿再 offset_layer。
            let nested_tokens =
                Self::tokenize_in_range(bytes, member.value_start, member.value_end)?;
            let nstart = Self::skip_trivia(&nested_tokens, 0);
            if nstart >= nested_tokens.len() || nested_tokens[nstart].kind != Kind::LBrace {
                return Err(format!("parent_type_mismatch:{key}"));
            }
            let (mut nested, _) = Self::parse_object(&nested_tokens, nstart)?;
            Self::fill_keys(bytes, &mut nested)?;
            let _ = tokens; // root tokens kept for API symmetry
            layer = nested;
        }
        Err("unreachable".into())
    }

    fn tokenize_in_range(bytes: &[u8], start: usize, end: usize) -> Result<Vec<Tok>, String> {
        let slice = &bytes[start..end];
        let mut toks = Self::tokenize(slice)?;
        for t in &mut toks {
            t.start += start;
            t.end += start;
        }
        Ok(toks)
    }

    /// 推断缩进与换行风格。
    fn style_from_bytes(bytes: &[u8], parent: &ObjectLayer) -> (String, String) {
        let text = std::str::from_utf8(bytes).unwrap_or("");
        let newline = if text.contains("\r\n") {
            "\r\n".to_string()
        } else {
            "\n".to_string()
        };
        // 从第一个 member 或 open 后空白推断 indent
        if let Some(m) = parent.members.first() {
            // look backward from key_start for indent spaces after newline
            let before = &bytes[..m.key_start];
            if let Some(pos) = before.iter().rposition(|&c| c == b'\n') {
                let indent_bytes = &before[pos + 1..];
                if indent_bytes.iter().all(|&c| c == b' ' || c == b'\t') {
                    return (newline, String::from_utf8_lossy(indent_bytes).into_owned());
                }
            }
        }
        // default 2 spaces
        (newline, "  ".to_string())
    }

    /// 序列化 JSON 值（紧凑、稳定键序）。
    fn render_json_value(value: &serde_json::Value) -> String {
        crate::agent_hub::config_patch::canonicalize_value(value).to_string()
    }

    /// 应用单条 set/delete 到字节缓冲。
    fn apply_path(
        bytes: &[u8],
        path: &[String],
        value: Option<&serde_json::Value>,
    ) -> Result<Vec<u8>, String> {
        if path.is_empty() {
            return Err("empty_path".into());
        }
        // 对于多层 path，采用“读 JSON → 改语义 → 对 root 的第一层 member 做 span 写回”
        // 当中间层缺失时 blocked（调用方应先创建父）。
        let (tokens, root) = Self::parse_root_object(bytes)?;
        if path.len() == 1 {
            return Self::apply_leaf_on_object(bytes, &root, &path[0], value);
        }
        // nested: find top-level parent member for path[0], recursively patch its value bytes
        let top = path[0].as_str();
        let find = Self::find_member_at_path(bytes, &tokens, &root, &[top.to_string()])?;
        let Some(mi) = find.member_index else {
            // 需要创建顶层 parent object
            if value.is_none() {
                // 删除不存在路径 no-op
                return Ok(bytes.to_vec());
            }
            // 创建 path[0] = nested object containing rest
            let nested = Self::build_nested_json(&path[1..], value.cloned())?;
            return Self::apply_leaf_on_object(bytes, &root, top, Some(&nested));
        };
        let member = &find.parent.members[mi];
        let nested_bytes = &bytes[member.value_start..member.value_end];
        // ensure nested is object
        let nested_tokens = Self::tokenize(nested_bytes)?;
        let ns = Self::skip_trivia(&nested_tokens, 0);
        if ns >= nested_tokens.len() || nested_tokens[ns].kind != Kind::LBrace {
            return Err(format!("parent_type_mismatch:{top}"));
        }
        let patched_nested = Self::apply_path(nested_bytes, &path[1..], value)?;
        // replace value span of top member
        let mut out = Vec::with_capacity(bytes.len() + patched_nested.len());
        out.extend_from_slice(&bytes[..member.value_start]);
        out.extend_from_slice(&patched_nested);
        out.extend_from_slice(&bytes[member.value_end..]);
        Ok(out)
    }

    fn build_nested_json(
        path: &[String],
        value: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        if path.is_empty() {
            return value.ok_or_else(|| "empty_nested".to_string());
        }
        let mut current = value.unwrap_or(serde_json::Value::Null);
        for key in path.iter().rev() {
            let mut map = serde_json::Map::new();
            map.insert(key.clone(), current);
            current = serde_json::Value::Object(map);
        }
        Ok(current)
    }

    fn apply_leaf_on_object(
        bytes: &[u8],
        parent: &ObjectLayer,
        key: &str,
        value: Option<&serde_json::Value>,
    ) -> Result<Vec<u8>, String> {
        match (parent.members.iter().position(|m| m.key == key), value) {
            (Some(mi), Some(v)) => {
                let m = &parent.members[mi];
                let rendered = Self::render_json_value(v);
                let mut out = Vec::with_capacity(bytes.len() + rendered.len());
                out.extend_from_slice(&bytes[..m.value_start]);
                out.extend_from_slice(rendered.as_bytes());
                out.extend_from_slice(&bytes[m.value_end..]);
                Ok(out)
            }
            (None, Some(v)) => {
                // insert new member
                let (newline, indent) = Self::style_from_bytes(bytes, parent);
                let rendered = Self::render_json_value(v);
                let key_json = serde_json::to_string(key).unwrap_or_else(|_| format!("\"{key}\""));
                let insert_body = if parent.members.is_empty() {
                    format!(
                        "{nl}{indent}{key}: {val}{nl}",
                        nl = newline,
                        indent = indent,
                        key = key_json,
                        val = rendered
                    )
                } else {
                    // insert before closing brace; ensure previous member has comma
                    let last = parent.members.last().unwrap();
                    let need_comma = !last.has_trailing_comma;
                    let prefix = if need_comma { "," } else { "" };
                    format!(
                        "{prefix}{nl}{indent}{key}: {val}",
                        prefix = prefix,
                        nl = newline,
                        indent = indent,
                        key = key_json,
                        val = rendered
                    )
                };
                let mut out = Vec::with_capacity(bytes.len() + insert_body.len() + 8);
                if parent.members.is_empty() {
                    // insert between { and }
                    out.extend_from_slice(&bytes[..=parent.open]);
                    out.extend_from_slice(insert_body.as_bytes());
                    out.extend_from_slice(indent.trim_end_matches([' ', '\t']).as_bytes());
                    // closing with less indent — keep original close
                    out.extend_from_slice(&bytes[parent.close..]);
                } else {
                    let last = parent.members.last().unwrap();
                    // after last member_end
                    out.extend_from_slice(&bytes[..last.member_end]);
                    out.extend_from_slice(insert_body.as_bytes());
                    out.extend_from_slice(&bytes[last.member_end..]);
                }
                Ok(out)
            }
            (Some(mi), None) => {
                // delete member
                let m = &parent.members[mi];
                let mut del_start = m.member_start;
                let mut del_end = m.member_end;
                // if not last and has no trailing comma, but previous had comma — fine
                // if last and previous has trailing comma, keep previous comma or remove ours
                if mi == 0 && parent.members.len() == 1 {
                    // only member: remove from after { to before }
                    del_start = parent.open + 1;
                    del_end = parent.close;
                } else if mi > 0 && !m.has_trailing_comma {
                    // remove previous member's trailing comma if we are last
                    // or remove our leading by eating previous comma
                    let prev = &parent.members[mi - 1];
                    if prev.has_trailing_comma {
                        // remove prev trailing comma + this member
                        del_start = prev.value_end;
                        let slice = &bytes[prev.value_end..m.key_start];
                        if let Some(rel) = slice.iter().position(|&c| c == b',') {
                            del_start = prev.value_end + rel;
                        }
                    }
                }
                // also eat trailing whitespace/newline after deleted member for cleanliness
                while del_end < bytes.len() && matches!(bytes[del_end], b' ' | b'\t') {
                    del_end += 1;
                }
                if del_end + 1 < bytes.len()
                    && bytes[del_end] == b'\r'
                    && bytes[del_end + 1] == b'\n'
                {
                    del_end += 2;
                } else if del_end < bytes.len() && bytes[del_end] == b'\n' {
                    del_end += 1;
                }
                let mut out = Vec::with_capacity(bytes.len());
                out.extend_from_slice(&bytes[..del_start]);
                out.extend_from_slice(&bytes[del_end..]);
                Ok(out)
            }
            (None, None) => Ok(bytes.to_vec()),
        }
    }

    /// 读取 path 的 JSON 值（经 to_json_text）。
    fn read_path_value(bytes: &[u8], path: &[String]) -> Result<Option<serde_json::Value>, String> {
        let tokens = Self::tokenize(bytes)?;
        let text = Self::to_json_text(bytes, &tokens)?;
        let root: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("invalid_json:{e}"))?;
        let mut cur = &root;
        for key in path {
            match cur {
                serde_json::Value::Object(map) => match map.get(key) {
                    Some(v) => cur = v,
                    None => return Ok(None),
                },
                _ => return Err(format!("parent_type_mismatch:{key}")),
            }
        }
        Ok(Some(cur.clone()))
    }
}

#[derive(Debug)]
struct FindResult {
    parent: ObjectLayer,
    member_index: Option<usize>,
    #[allow(dead_code)]
    leaf_key: String,
}

impl SemanticConfigPatcher for JsoncConfigPatcher {
    fn inspect(&self, bytes: &[u8], path: &[String]) -> Result<OwnedConfigValue, AppError> {
        match Self::read_path_value(bytes, path) {
            Ok(None) => Ok(missing_owned_value()),
            Ok(Some(v)) => Ok(owned_value_from_json(v)),
            Err(reason) => Err(AppError::validation(format!(
                "config_patch_blocked:{reason}"
            ))),
        }
    }

    fn apply(
        &self,
        bytes: &[u8],
        patches: &[ManagedConfigPatch],
    ) -> Result<PatchedConfig, AppError> {
        // 空输入：允许从 {} 起步
        let mut current = if bytes.is_empty() {
            b"{}".to_vec()
        } else {
            bytes.to_vec()
        };

        // 预校验可解析
        if let Err(reason) = Self::parse_root_object(&current) {
            return Ok(blocked_result(bytes, reason));
        }

        let mut preview = Vec::new();
        let mut owned = Vec::new();

        for patch in patches {
            let before = match Self::read_path_value(&current, &patch.path) {
                Ok(v) => v,
                Err(reason) => return Ok(blocked_result(bytes, reason)),
            };
            let before_hash = before.as_ref().map(value_content_hash);
            if !check_cas(patch.expected_base_hash.as_deref(), before_hash.as_deref()) {
                return Ok(conflict_result(bytes, patch, before_hash));
            }

            let next = match Self::apply_path(&current, &patch.path, patch.value.as_ref()) {
                Ok(b) => b,
                Err(reason) => return Ok(blocked_result(bytes, reason)),
            };

            // 校验仍可解析
            if let Err(reason) = Self::parse_root_object(&next) {
                return Ok(blocked_result(
                    bytes,
                    format!("post_patch_invalid:{reason}"),
                ));
            }

            let after = match Self::read_path_value(&next, &patch.path) {
                Ok(v) => v,
                Err(reason) => return Ok(blocked_result(bytes, reason)),
            };
            let after_hash = after.as_ref().map(value_content_hash);
            preview.push(ConfigPathDiff {
                owner_id: patch.owner_id.clone(),
                path: patch.path.clone(),
                before_hash: before_hash.clone(),
                after_hash: after_hash.clone(),
                before_value: before,
                after_value: after,
            });
            owned.push(ConfigOwnedPathMeta {
                owner_id: patch.owner_id.clone(),
                path: patch.path.clone(),
                base_value_hash: after_hash,
            });
            current = next;
        }

        Ok(PatchedConfig {
            outcome: ConfigPatchOutcome::Applied,
            document_hash: sha256_hex(&current),
            bytes: current,
            owned_path_hashes: owned,
            preview_diff: preview,
        })
    }
}

/// 测试辅助：从字节中移除 owned path 对应 value span 后比较无关区域。
///
/// Business Logic: 证明无关 span 字节级保留。
/// Code Logic: 将 owned value 替换为固定占位再比。
#[cfg(test)]
pub fn strip_owned_span(bytes: &[u8], path: &[String]) -> Result<Vec<u8>, String> {
    let (tokens, root) = JsoncConfigPatcher::parse_root_object(bytes)?;
    let find = JsoncConfigPatcher::find_member_at_path(bytes, &tokens, &root, path)?;
    let Some(mi) = find.member_index else {
        return Ok(bytes.to_vec());
    };
    let m = &find.parent.members[mi];
    let mut out = bytes.to_vec();
    out.splice(m.value_start..m.value_end, b"#".iter().copied());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::config_patch::value_content_hash;

    fn p(
        path: &[&str],
        value: Option<serde_json::Value>,
        expected: Option<&str>,
    ) -> ManagedConfigPatch {
        ManagedConfigPatch {
            owner_id: "hub".into(),
            path: path.iter().map(|s| (*s).to_string()).collect(),
            value,
            expected_base_hash: expected.map(|s| s.to_string()),
        }
    }

    // JSONC fixture with line/block comments, trailing commas, CRLF, unrelated plugin
    fn seed_jsonc() -> Vec<u8> {
        let s = "{\r\n  // keep this comment\r\n  \"plugins\": {\r\n    \"userPlugin\": true,\r\n  },\r\n  /* block comment stays */\r\n  \"mcpServers\": {\r\n    \"user-owned\": { \"command\": \"uvx\" },\r\n    \"cc_partner_x\": { \"command\": \"old\" },\r\n  },\r\n}\r\n";
        s.as_bytes().to_vec()
    }

    #[test]
    fn jsonc_preserves_comments_and_unrelated_when_patching_owned() {
        let patcher = JsoncConfigPatcher;
        let before = seed_jsonc();
        let result = patcher
            .apply(
                &before,
                &[p(
                    &["mcpServers", "cc_partner_x"],
                    Some(serde_json::json!({"command":"new","args":["a"]})),
                    None,
                )],
            )
            .expect("apply");
        assert!(matches!(result.outcome, ConfigPatchOutcome::Applied));
        let after = String::from_utf8(result.bytes.clone()).expect("utf8");
        assert!(after.contains("// keep this comment"));
        assert!(after.contains("/* block comment stays */"));
        assert!(after.contains("userPlugin"));
        assert!(after.contains("user-owned"));
        assert!(after.contains("\r\n"));
        assert!(
            after.contains("\"command\":\"new\"")
                || after.contains("\"command\": \"new\"")
                || after.contains("new")
        );

        let path = vec!["mcpServers".into(), "cc_partner_x".into()];
        let strip_before = strip_owned_span(&before, &path).expect("strip b");
        let strip_after = strip_owned_span(&result.bytes, &path).expect("strip a");
        assert_eq!(
            strip_before, strip_after,
            "unrelated spans must be byte-identical"
        );
    }

    #[test]
    fn jsonc_conflict_does_not_overwrite() {
        let patcher = JsoncConfigPatcher;
        let before = seed_jsonc();
        let current = patcher
            .inspect(&before, &["mcpServers".into(), "cc_partner_x".into()])
            .expect("inspect");
        let wrong = "deadbeef".to_string() + &"0".repeat(56);
        let result = patcher
            .apply(
                &before,
                &[p(
                    &["mcpServers", "cc_partner_x"],
                    Some(serde_json::json!({"command":"hijack"})),
                    Some(&wrong),
                )],
            )
            .expect("apply");
        assert!(matches!(
            result.outcome,
            ConfigPatchOutcome::Conflict { .. }
        ));
        assert_eq!(result.bytes, before);
        assert!(!String::from_utf8_lossy(&result.bytes).contains("hijack"));
        assert_eq!(
            match &result.outcome {
                ConfigPatchOutcome::Conflict { current_hash, .. } => current_hash.clone(),
                _ => None,
            },
            current.value_hash
        );
    }

    #[test]
    fn jsonc_cas_match_applies() {
        let patcher = JsoncConfigPatcher;
        let before = seed_jsonc();
        let current = patcher
            .inspect(&before, &["mcpServers".into(), "cc_partner_x".into()])
            .expect("inspect");
        let hash = current.value_hash.clone().expect("h");
        // verify hash formula
        assert_eq!(Some(value_content_hash(&current.value)), current.value_hash);
        let result = patcher
            .apply(
                &before,
                &[p(
                    &["mcpServers", "cc_partner_x"],
                    Some(serde_json::json!({"command":"ok"})),
                    Some(&hash),
                )],
            )
            .expect("apply");
        assert!(matches!(result.outcome, ConfigPatchOutcome::Applied));
    }

    #[test]
    fn jsonc_invalid_blocked_preserves_bytes() {
        let seed = b"{ not jsonc ";
        let patcher = JsoncConfigPatcher;
        let result = patcher
            .apply(
                seed,
                &[p(&["mcpServers", "x"], Some(serde_json::json!(1)), None)],
            )
            .expect("apply");
        assert!(matches!(result.outcome, ConfigPatchOutcome::Blocked { .. }));
        assert_eq!(result.bytes, seed);
    }

    #[test]
    fn jsonc_trailing_comma_and_insert_new_member() {
        let seed = b"{\n  \"a\": 1,\n}\n";
        let patcher = JsoncConfigPatcher;
        let result = patcher
            .apply(
                seed,
                &[p(
                    &["mcpServers", "cc_partner_x"],
                    Some(serde_json::json!({"command":"n"})),
                    None,
                )],
            )
            .expect("apply");
        assert!(matches!(result.outcome, ConfigPatchOutcome::Applied));
        let after = String::from_utf8(result.bytes).expect("utf8");
        assert!(after.contains("\"a\""));
        assert!(after.contains("cc_partner_x"));
        assert!(after.contains("command"));
    }

    #[test]
    fn jsonc_parent_type_mismatch_blocked() {
        let seed = br#"{"mcpServers":"not-object"}"#;
        let patcher = JsoncConfigPatcher;
        let result = patcher
            .apply(
                seed,
                &[p(
                    &["mcpServers", "cc_partner_x"],
                    Some(serde_json::json!({"command":"x"})),
                    None,
                )],
            )
            .expect("apply");
        assert!(matches!(result.outcome, ConfigPatchOutcome::Blocked { .. }));
        assert_eq!(result.bytes, seed);
    }

    #[test]
    fn jsonc_removal_preserves_user_keys() {
        let patcher = JsoncConfigPatcher;
        let before = seed_jsonc();
        let result = patcher
            .apply(&before, &[p(&["mcpServers", "cc_partner_x"], None, None)])
            .expect("apply");
        assert!(matches!(result.outcome, ConfigPatchOutcome::Applied));
        let after = String::from_utf8(result.bytes).expect("utf8");
        assert!(after.contains("user-owned"));
        assert!(after.contains("// keep this comment"));
        assert!(!after.contains("cc_partner_x"));
    }
}
