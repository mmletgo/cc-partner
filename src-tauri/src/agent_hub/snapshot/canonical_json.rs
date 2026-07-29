//! agent_hub/snapshot/canonical_json — RFC 8785 兼容的 JSON 子集
//!
//! Business Logic（为什么需要这个模块）:
//!     SnapshotEnvelope 的 snapshotHash 必须跨实现/跨设备一致；普通 serde map 顺序
//!     与浮点/大整数语义不能进入 v1 wire。
//!
//! Code Logic（这个模块做什么）:
//!     接受 ASCII key、无浮点、安全整数、标准 JSON 转义的子集；按字节序排序 object key
//!     后无空白输出；解析时检测重复 decoded key。

use serde_json::{Map, Number, Value};
use std::collections::BTreeMap;
use std::fmt;

/// IEEE-754 精确整数上限（2^53-1）。
pub const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Canonical JSON 子集错误。
///
/// Business Logic: 诊断只描述 schema/类型/大小元数据，不回显凭据正文。
/// Code Logic: Display 文案仅含计数/类型名/键名规则。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalJsonError {
    /// 含浮点数
    FloatNotAllowed,
    /// 整数超出安全范围
    IntegerOutOfRange { value: String },
    /// object key 非 ASCII
    NonAsciiKey { key: String },
    /// 重复 decoded key
    DuplicateKey { key: String },
    /// 输入不是合法 UTF-8 JSON
    InvalidJson { message: String },
    /// 数字不能无损表示为安全整数
    NumberNotSafeInteger,
}

impl fmt::Display for CanonicalJsonError {
    /// 稳定、无正文泄露的错误文案。
    ///
    /// Business Logic: 错误可进 log/UI，不得含 credential 原文。
    /// Code Logic: 仅格式化枚举字段。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FloatNotAllowed => write!(f, "canonical_json: float not allowed"),
            Self::IntegerOutOfRange { value } => {
                write!(f, "canonical_json: integer out of range ({value})")
            }
            Self::NonAsciiKey { key } => {
                write!(
                    f,
                    "canonical_json: non-ASCII object key (len={})",
                    key.len()
                )
            }
            Self::DuplicateKey { key } => {
                write!(
                    f,
                    "canonical_json: duplicate object key (len={})",
                    key.len()
                )
            }
            Self::InvalidJson { message } => {
                write!(f, "canonical_json: invalid json ({message})")
            }
            Self::NumberNotSafeInteger => {
                write!(f, "canonical_json: number is not a safe integer")
            }
        }
    }
}

impl std::error::Error for CanonicalJsonError {}

/// 将 `serde_json::Value` 序列化为 RFC8785 兼容子集字节。
///
/// Business Logic（为什么需要这个函数）:
///     snapshotHash 输入必须与 map 插入顺序无关，且拒绝浮点/非 ASCII key/超大整数。
///
/// Code Logic（这个函数做什么）:
///     递归校验后无空白写出；object key 按 UTF-8/ASCII 字节序排序；字符串用标准 JSON escape。
pub fn canonicalize_value(value: &Value) -> Result<Vec<u8>, CanonicalJsonError> {
    let mut out = Vec::with_capacity(256);
    write_canonical(value, &mut out)?;
    Ok(out)
}

/// 从 JSON 文本解析并检测重复 key。
///
/// Business Logic（为什么需要这个函数）:
///     导入方必须在 typed 反序列化前拒绝重复 key，避免 silent overwrite。
///
/// Code Logic（这个函数做什么）:
///     用 `serde_json::Deserializer` + `Value` 的 Map 去重检测；非 ASCII key 留给 canonicalize 阶段。
pub fn parse_json_value_strict(input: &str) -> Result<Value, CanonicalJsonError> {
    let mut de = serde_json::Deserializer::from_str(input);
    let value = Value::deserialize_with_duplicate_detection(&mut de).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("duplicate object key") {
            // 从错误消息中尽量提取 key 长度，不回显 key 原文。
            CanonicalJsonError::DuplicateKey {
                key: extract_duplicate_key_hint(&msg),
            }
        } else {
            CanonicalJsonError::InvalidJson { message: msg }
        }
    })?;
    de.end().map_err(|e| CanonicalJsonError::InvalidJson {
        message: e.to_string(),
    })?;
    Ok(value)
}

/// 从错误文案抽取 key 提示（长度占位，不存原文）。
///
/// Business Logic: Display 不得携带可能含 secret 的 key 原文。
/// Code Logic: 返回空串或截断 token。
fn extract_duplicate_key_hint(msg: &str) -> String {
    // 我们自己的错误会构造 "duplicate object key: <key>"；外部错误返回空。
    if let Some(rest) = msg.strip_prefix("duplicate object key: ") {
        return rest.to_string();
    }
    String::new()
}

/// 递归写出 canonical JSON。
///
/// Business Logic: 子集校验与排序必须在写出路径上强制执行。
/// Code Logic: match Value 各变体；Object 转 BTreeMap 排序。
fn write_canonical(value: &Value, out: &mut Vec<u8>) -> Result<(), CanonicalJsonError> {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(n) => write_number(n, out)?,
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(item, out)?;
            }
            out.push(b']');
        }
        Value::Object(map) => write_object(map, out)?,
    }
    Ok(())
}

/// 写出安全整数。
///
/// Business Logic: 禁止浮点与超过 2^53-1 的整数进入 hash 输入。
/// Code Logic: as_i64 / as_u64 检查范围后 itoa 风格写出。
fn write_number(n: &Number, out: &mut Vec<u8>) -> Result<(), CanonicalJsonError> {
    if let Some(i) = n.as_i64() {
        if !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&i) {
            return Err(CanonicalJsonError::IntegerOutOfRange {
                value: i.to_string(),
            });
        }
        out.extend_from_slice(i.to_string().as_bytes());
        return Ok(());
    }
    if let Some(u) = n.as_u64() {
        if u > MAX_SAFE_INTEGER as u64 {
            return Err(CanonicalJsonError::IntegerOutOfRange {
                value: u.to_string(),
            });
        }
        out.extend_from_slice(u.to_string().as_bytes());
        return Ok(());
    }
    // f64 或其它
    if n.as_f64().is_some() {
        return Err(CanonicalJsonError::FloatNotAllowed);
    }
    Err(CanonicalJsonError::NumberNotSafeInteger)
}

/// 写出标准 JSON 字符串（不归一 Unicode）。
///
/// Business Logic: 凭据与 asset 文本必须按原 Unicode 码点保留；仅 escape 控制字符与引号。
/// Code Logic: RFC 8259 风格 escape；非 ASCII 原样 UTF-8 输出。
fn write_string(s: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    for ch in s.chars() {
        match ch {
            '"' => out.extend_from_slice(br#"\""#),
            '\\' => out.extend_from_slice(br#"\\"#),
            '\u{08}' => out.extend_from_slice(br#"\b"#),
            '\u{0C}' => out.extend_from_slice(br#"\f"#),
            '\n' => out.extend_from_slice(br#"\n"#),
            '\r' => out.extend_from_slice(br#"\r"#),
            '\t' => out.extend_from_slice(br#"\t"#),
            c if (c as u32) < 0x20 => {
                let esc = format!("\\u{:04x}", c as u32);
                out.extend_from_slice(esc.as_bytes());
            }
            c => {
                let mut buf = [0u8; 4];
                let encoded = c.encode_utf8(&mut buf);
                out.extend_from_slice(encoded.as_bytes());
            }
        }
    }
    out.push(b'"');
}

/// 写出排序后的 object。
///
/// Business Logic: ASCII key 的字节序等于 RFC 8785 UTF-16 code unit 序。
/// Code Logic: 校验 ASCII → BTreeMap 排序 → 递归 value。
fn write_object(map: &Map<String, Value>, out: &mut Vec<u8>) -> Result<(), CanonicalJsonError> {
    let mut ordered: BTreeMap<&str, &Value> = BTreeMap::new();
    for (k, v) in map {
        if !k.is_ascii() {
            return Err(CanonicalJsonError::NonAsciiKey { key: k.clone() });
        }
        ordered.insert(k.as_str(), v);
    }
    out.push(b'{');
    let mut first = true;
    for (k, v) in ordered {
        if !first {
            out.push(b',');
        }
        first = false;
        write_string(k, out);
        out.push(b':');
        write_canonical(v, out)?;
    }
    out.push(b'}');
    Ok(())
}

/// 带重复 key 检测的 Value 反序列化扩展。
///
/// Business Logic: serde_json 默认 Map 后写覆盖；snapshot 必须 fail-closed。
/// Code Logic: 手写 visit_map，插入前 contains_key。
trait DeserializeWithDupDetect: Sized {
    fn deserialize_with_duplicate_detection<'de, D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>;
}

impl DeserializeWithDupDetect for Value {
    fn deserialize_with_duplicate_detection<'de, D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, MapAccess, SeqAccess, Visitor};
        use std::fmt;

        struct ValueVisitor;

        impl<'de> Visitor<'de> for ValueVisitor {
            type Value = Value;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("any valid JSON value")
            }

            fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
                Ok(Value::Bool(v))
            }

            fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
                Ok(Value::Number(v.into()))
            }

            fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
                Ok(Number::from(v).into())
            }

            fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Number::from_f64(v)
                    .map(Value::Number)
                    .ok_or_else(|| de::Error::custom("invalid float"))
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
                Ok(Value::String(v.to_owned()))
            }

            fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
                Ok(Value::String(v))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(Value::Null)
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(Value::Null)
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut vec = Vec::new();
                while let Some(elem) = seq.next_element_seed(ValueSeed)? {
                    vec.push(elem);
                }
                Ok(Value::Array(vec))
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut values = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom(format!("duplicate object key: {key}")));
                    }
                    let value = map.next_value_seed(ValueSeed)?;
                    values.insert(key, value);
                }
                Ok(Value::Object(values))
            }
        }

        struct ValueSeed;

        impl<'de> de::DeserializeSeed<'de> for ValueSeed {
            type Value = Value;

            fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                Value::deserialize_with_duplicate_detection(deserializer)
            }
        }

        deserializer.deserialize_any(ValueVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sha2::{Digest, Sha256};

    /// Business Logic: 插入顺序不得影响 canonical bytes。
    /// Code Logic: 两个 key 顺序相反的 object 输出相同。
    #[test]
    fn shuffled_object_keys_produce_identical_bytes() {
        let a = json!({"b": 1, "a": 2, "c": {"z": true, "y": false}});
        let b = json!({"c": {"y": false, "z": true}, "a": 2, "b": 1});
        let ca = canonicalize_value(&a).expect("a");
        let cb = canonicalize_value(&b).expect("b");
        assert_eq!(ca, cb);
        assert_eq!(
            String::from_utf8_lossy(&ca),
            r#"{"a":2,"b":1,"c":{"y":false,"z":true}}"#
        );
    }

    /// Business Logic: ASCII key 字节序即 RFC8785 在本子集上的排序。
    /// Code Logic: "A" < "B" < "a" < "b"（ASCII）。
    #[test]
    fn ascii_keys_sort_lexicographically_byte_order() {
        let v = json!({"b": 1, "a": 2, "B": 3, "A": 4});
        let bytes = canonicalize_value(&v).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            r#"{"A":4,"B":3,"a":2,"b":1}"#
        );
    }

    /// Business Logic: Unicode 字符串与标准 escape 必须字节级稳定。
    /// Code Logic: 中文原样 UTF-8；换行/引号/反斜杠 escape；控制字符 `\u00XX`。
    #[test]
    fn unicode_and_escapes_match_expected_bytes() {
        let v = json!({
            "msg": "你好\nworld\"\\",
            "tab": "a\tb",
            "ctrl": "\u{0001}"
        });
        let bytes = canonicalize_value(&v).unwrap();
        // keys sorted: ctrl, msg, tab
        let expected = "{\"ctrl\":\"\\u0001\",\"msg\":\"你好\\nworld\\\"\\\\\",\"tab\":\"a\\tb\"}";
        assert_eq!(String::from_utf8_lossy(&bytes), expected);
    }

    /// Business Logic: 浮点不得进入 snapshot hash 输入。
    #[test]
    fn floats_are_rejected() {
        let v = json!({"x": 1.5});
        let err = canonicalize_value(&v).unwrap_err();
        assert!(matches!(err, CanonicalJsonError::FloatNotAllowed));
        assert!(!err.to_string().contains("1.5") || err.to_string().contains("float"));
    }

    /// Business Logic: 非 ASCII object key 拒绝（子集）。
    #[test]
    fn non_ascii_keys_are_rejected() {
        let mut map = Map::new();
        map.insert("键".into(), Value::Bool(true));
        let v = Value::Object(map);
        let err = canonicalize_value(&v).unwrap_err();
        assert!(matches!(err, CanonicalJsonError::NonAsciiKey { .. }));
        // Display 只含 len，不含 key 原文
        assert!(!err.to_string().contains("键"));
    }

    /// Business Logic: 重复 decoded key fail-closed。
    #[test]
    fn duplicate_decoded_keys_are_rejected() {
        let raw = r#"{"a":1,"a":2}"#;
        let err = parse_json_value_strict(raw).unwrap_err();
        assert!(matches!(err, CanonicalJsonError::DuplicateKey { .. }));
    }

    /// Business Logic: 超过 2^53-1 的整数拒绝。
    #[test]
    fn integers_above_safe_max_are_rejected() {
        let too_big = MAX_SAFE_INTEGER + 1;
        let v = json!({ "n": too_big });
        let err = canonicalize_value(&v).unwrap_err();
        assert!(matches!(err, CanonicalJsonError::IntegerOutOfRange { .. }));

        let ok = json!({ "n": MAX_SAFE_INTEGER });
        assert!(canonicalize_value(&ok).is_ok());
    }

    /// Business Logic: 安全整数边界内可写出。
    #[test]
    fn safe_integer_boundary_accepted() {
        let v = json!({ "n": -MAX_SAFE_INTEGER });
        let bytes = canonicalize_value(&v).unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains(&(-MAX_SAFE_INTEGER).to_string()));
    }

    /// Business Logic: 同一 value 的 hash 与插入顺序无关。
    #[test]
    fn hash_of_shuffled_objects_is_identical() {
        let a = json!({"z": "plain-fixture-secret", "a": [1, 2, 3]});
        let b = json!({"a": [1, 2, 3], "z": "plain-fixture-secret"});
        let ha = Sha256::digest(canonicalize_value(&a).unwrap());
        let hb = Sha256::digest(canonicalize_value(&b).unwrap());
        assert_eq!(ha.as_slice(), hb.as_slice());
    }
}
