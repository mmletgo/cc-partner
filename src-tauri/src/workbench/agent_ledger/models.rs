//! workbench/agent_ledger/models — Agent Metadata Ledger 领域模型
//!
//! Business Logic（为什么需要这个模块）:
//!     用户需要从 Agent 终态自动形成的 metadata-only 用量与 outcome 历史；
//!     禁止落 Prompt、回复、terminal bytes、transcript path、cwd、env、credential。
//!
//! Code Logic（这个模块做什么）:
//!     定义 entry/outcome/usage/query/summary DTO、货币校验与 cost 无损 minor-units 转换。

use crate::error::AppError;
use serde::{Deserialize, Serialize};

/// Ledger 终态 outcome（固定四态）。
///
/// Business Logic（为什么需要这个类型）:
///     UI/聚合只依赖稳定 token，不能依赖厂商文案或自由字符串。
///
/// Code Logic（这个类型做什么）:
///     `completed|failed|cancelled|disconnected`；serde camelCase。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentLedgerOutcome {
    /// 正常完成
    Completed,
    /// 失败结束
    Failed,
    /// 用户/系统取消
    Cancelled,
    /// terminal 丢失或对账断开
    Disconnected,
}

impl AgentLedgerOutcome {
    /// 返回存储/聚合用稳定 token。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     SQLite TEXT 与 SQL 过滤需要固定字面量。
    ///
    /// Code Logic（这个函数做什么）:
    ///     映射到 snake 风格短码。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Disconnected => "disconnected",
        }
    }

    /// 解析 outcome；未知 fail-closed。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     读库与 API 入站不得静默默认 completed。
    ///
    /// Code Logic（这个函数做什么）:
    ///     匹配 as_str 与 camelCase；否则 None。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "completed" | "Completed" => Some(Self::Completed),
            "failed" | "Failed" => Some(Self::Failed),
            "cancelled" | "canceled" | "Cancelled" | "Canceled" => Some(Self::Cancelled),
            "disconnected" | "Disconnected" => Some(Self::Disconnected),
            _ => None,
        }
    }
}

/// 可靠 cumulative usage 快照（仅 adapter 结构化字段）。
///
/// Business Logic（为什么需要这个类型）:
///     provider 只在给出可验证 structured usage 时写入；unknown 保持 null，禁止估算。
///
/// Code Logic（这个类型做什么）:
///     可选 model/token/cost；cost_major 为 provider 主单位十进制字符串（非估算）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReliableUsageSnapshot {
    /// 可选 model id（adapter 可靠字段）
    pub model_id: Option<String>,
    /// 累计 input tokens
    pub input_tokens: Option<u64>,
    /// 累计 output tokens
    pub output_tokens: Option<u64>,
    /// 累计 cache read tokens
    pub cache_read_tokens: Option<u64>,
    /// 累计 cache write tokens
    pub cache_write_tokens: Option<u64>,
    /// provider 主单位金额字符串，如 "0.0123"；无则 cost 列保持 null
    pub cost_major: Option<String>,
    /// ISO 4217 三字符货币（大写校验）
    pub cost_currency: Option<String>,
}

impl ReliableUsageSnapshot {
    /// 判断快照是否含任何可靠 usage 字段。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     全空快照无需参与 null-fill 合并。
    ///
    /// Code Logic（这个函数做什么）:
    ///     任一 token/model/cost 字段为 Some 即 true。
    pub fn has_any(&self) -> bool {
        self.model_id.is_some()
            || self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.cache_read_tokens.is_some()
            || self.cache_write_tokens.is_some()
            || self.cost_major.is_some()
            || self.cost_currency.is_some()
    }

    /// 将 cost_major + currency 无损转为 minor units；失败返回 None（不估算）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     仅当 provider 金额可按 ISO 4217 exponent 无损转整数 minor 时才落库。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验货币 → 解析十进制 → fraction 位数 ≤ exponent 时乘 10^exp。
    pub fn cost_minor_units(&self) -> Option<u64> {
        let currency = self.cost_currency.as_deref()?;
        let major = self.cost_major.as_deref()?;
        convert_major_to_minor_units(major, currency).ok()
    }
}

/// finalize 入参（首次终态或重放 null-fill）。
///
/// Business Logic（为什么需要这个类型）:
///     writer 只接收 metadata + 可靠 usage，禁止 prompt/transcript/path。
///
/// Code Logic（这个类型做什么）:
///     聚合 identity、时间窗、outcome 与可选 usage。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLedgerFinalizeInput {
    /// 与 A1 agent session id 一一对应（唯一键）
    pub agent_session_id: String,
    /// 所属项目
    pub project_id: String,
    /// 可选 worktree
    pub worktree_id: Option<String>,
    /// provider 稳定 id
    pub provider_id: String,
    /// 可选 model（也可来自 usage）
    pub model_id: Option<String>,
    /// 开始时间 RFC3339
    pub started_at: String,
    /// 结束时间 RFC3339
    pub ended_at: String,
    /// 终态 outcome
    pub outcome: AgentLedgerOutcome,
    /// 可选可靠 usage
    pub usage: Option<ReliableUsageSnapshot>,
}

/// 持久化后的 ledger 行（metadata-only DTO）。
///
/// Business Logic（为什么需要这个类型）:
///     本机分页与清除回读只暴露运营 metadata，无正文/路径/凭据。
///
/// Code Logic（这个类型做什么）:
///     对齐 `agent_session_ledger` 列；serde camelCase。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentLedgerEntry {
    /// 行主键 UUID
    pub id: String,
    /// 唯一 agent session id
    pub agent_session_id: String,
    /// 项目 id
    pub project_id: String,
    /// 可选 worktree
    pub worktree_id: Option<String>,
    /// provider id
    pub provider_id: String,
    /// 可选 model
    pub model_id: Option<String>,
    /// 开始 RFC3339
    pub started_at: String,
    /// 结束 RFC3339
    pub ended_at: String,
    /// 非负 duration（ms）
    pub duration_ms: u64,
    /// 终态 outcome
    pub outcome: AgentLedgerOutcome,
    /// 可选 input tokens
    pub input_tokens: Option<u64>,
    /// 可选 output tokens
    pub output_tokens: Option<u64>,
    /// 可选 cache read
    pub cache_read_tokens: Option<u64>,
    /// 可选 cache write
    pub cache_write_tokens: Option<u64>,
    /// 可选 cost minor units
    pub cost_minor_units: Option<u64>,
    /// 可选 ISO 货币
    pub cost_currency: Option<String>,
    /// 创建时间
    pub created_at: String,
    /// 更新时间
    pub updated_at: String,
}

/// 本机分页查询。
///
/// Business Logic（为什么需要这个类型）:
///     本机明细需有界分页与封闭 filter，不提供全文搜索。
///
/// Code Logic（这个类型做什么）:
///     limit 默认 50 最大 200；cursor 为 opaque base64url。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentLedgerQuery {
    /// 可选 project 过滤
    pub project_id: Option<String>,
    /// 可选 provider 过滤
    pub provider_id: Option<String>,
    /// 可选 outcome 过滤
    pub outcome: Option<AgentLedgerOutcome>,
    /// 可选 ended_at 下界（含）RFC3339
    pub ended_after: Option<String>,
    /// 可选 ended_at 上界（含）RFC3339
    pub ended_before: Option<String>,
    /// opaque cursor
    pub cursor: Option<String>,
    /// 页大小；None→50，硬顶 200
    pub limit: Option<u32>,
}

/// 分页结果。
///
/// Business Logic（为什么需要这个类型）:
///     前端 load-more 需要 items + nextCursor。
///
/// Code Logic（这个类型做什么）:
///     next_cursor 仅在 has_more 时存在。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentLedgerPage {
    /// 本页条目
    pub items: Vec<AgentLedgerEntry>,
    /// 下一页 cursor
    pub next_cursor: Option<String>,
}

/// 聚合时间窗。
///
/// Business Logic（为什么需要这个类型）:
///     Fleet/本机 summary 只允许 24h/7d/30d，禁止任意 range 在 remote 面暴露。
///
/// Code Logic（这个类型做什么）:
///     三态枚举 + wire 字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LedgerWindow {
    /// 最近 24 小时
    #[serde(rename = "24h", alias = "hours24", alias = "Hours24")]
    Hours24,
    /// 最近 7 天
    #[serde(rename = "7d", alias = "days7", alias = "Days7")]
    Days7,
    /// 最近 30 天
    #[serde(rename = "30d", alias = "days30", alias = "Days30")]
    Days30,
}

impl LedgerWindow {
    /// 返回 wire token。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     API/P2P 需要稳定 window 字面量。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `24h|7d|30d`。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hours24 => "24h",
            Self::Days7 => "7d",
            Self::Days30 => "30d",
        }
    }

    /// 解析 window；未知 fail-closed。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     非法 window 不得静默回退。
    ///
    /// Code Logic（这个函数做什么）:
    ///     匹配 24h/7d/30d 与枚举别名。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "24h" | "hours24" | "Hours24" => Some(Self::Hours24),
            "7d" | "days7" | "Days7" => Some(Self::Days7),
            "30d" | "days30" | "Days30" => Some(Self::Days30),
            _ => None,
        }
    }

    /// 窗口时长（秒）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     聚合边界按 now-window 计算。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 u64 秒数。
    pub fn duration_secs(self) -> u64 {
        match self {
            Self::Hours24 => 24 * 3600,
            Self::Days7 => 7 * 24 * 3600,
            Self::Days30 => 30 * 24 * 3600,
        }
    }
}

/// usage 覆盖度。
///
/// Business Logic（为什么需要这个类型）:
///     有 unknown token 时不得把聚合显示为 0 或 complete。
///
/// Code Logic（这个类型做什么）:
///     complete|partial|unavailable。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LedgerUsageCoverage {
    /// 全部 session 都有可靠 usage 贡献
    Complete,
    /// 部分 session 有 usage
    Partial,
    /// 无任何可靠 usage
    Unavailable,
}

impl LedgerUsageCoverage {
    /// 返回 wire token。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     前端/P2P 解码 coverage。
    ///
    /// Code Logic（这个函数做什么）:
    ///     camelCase 字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Unavailable => "unavailable",
        }
    }
}

/// 按货币分组的 cost 桶。
///
/// Business Logic（为什么需要这个类型）:
///     多货币不得折算；逐 currency 展示 minor units。
///
/// Code Logic（这个类型做什么）:
///     currency + minor_units 合计。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrencyAmount {
    /// ISO 4217
    pub currency: String,
    /// 该货币 minor units 合计
    pub minor_units: u64,
}

/// 时间窗聚合摘要。
///
/// Business Logic（为什么需要这个类型）:
///     Fleet/本机 summary 只读聚合，不暴露 entry/session id。
///
/// Code Logic（这个类型做什么）:
///     sessions/outcome 计数 + 可选 token + cost 桶 + coverage。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentLedgerSummary {
    /// 时间窗
    pub window: LedgerWindow,
    /// 可选 project 范围（单项目聚合时填）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// session 总数
    pub sessions: u64,
    /// completed 数
    pub completed: u64,
    /// failed 数（不含 cancelled）
    pub failed: u64,
    /// cancelled 数
    pub cancelled: u64,
    /// disconnected 数
    pub disconnected: u64,
    /// duration 合计 ms
    pub duration_ms: u64,
    /// 可靠 input tokens 合计（无贡献时 null）
    pub input_tokens: Option<u64>,
    /// 可靠 output tokens 合计
    pub output_tokens: Option<u64>,
    /// 按货币 cost 桶
    pub cost_by_currency: Vec<CurrencyAmount>,
    /// usage 覆盖度
    pub usage_coverage: LedgerUsageCoverage,
}

/// P2P owner-local 批量 summary 请求（snake_case，与 lan-fleet owner batch 一致）。
///
/// Business Logic（为什么需要这个类型）:
///     控制设备按 owning device 一次请求多个本机 project 的时间窗聚合；
///     不得请求 entry 列表或 remote: 包装 id。
///
/// Code Logic（这个类型做什么）:
///     snake_case project_ids + window 字面量；上限由路由校验。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentLedgerSummaryBatchReq {
    /// 本机 inner project id 列表（非 remote:）
    #[serde(default)]
    pub project_ids: Vec<String>,
    /// 时间窗：24h|7d|30d
    #[serde(default)]
    pub window: String,
}

/// P2P 批量 summary 响应（camelCase，无 entry/session id）。
///
/// Business Logic（为什么需要这个类型）:
///     Fleet join 与 remote client 需要 per-project 聚合；序列化结果不得含 entries。
///
/// Code Logic（这个类型做什么）:
///     window + projects[]（每个是 AgentLedgerSummary，带 project_id）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentLedgerSummaryBatchResp {
    /// 请求的时间窗
    pub window: LedgerWindow,
    /// 按请求顺序的 per-project 聚合
    pub projects: Vec<AgentLedgerSummary>,
}

/// P2P 批量 summary 最多 project id 数。
pub const AGENT_LEDGER_SUMMARY_MAX_PROJECTS: usize = 100;

/// 校验 ISO 4217 三字符大写货币码。
///
/// Business Logic（为什么需要这个函数）:
///     非法/小写/非 3 字符货币不得入库。
///
/// Code Logic（这个函数做什么）:
///     长度 3 且全大写 ASCII 字母；返回规范化大写字符串。
pub fn validate_currency_code(raw: &str) -> Result<String, AppError> {
    let s = raw.trim();
    if s.len() != 3 || !s.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(AppError::validation(format!(
            "cost_currency 必须为 ISO 4217 三字符大写代码，收到: {raw}"
        )));
    }
    Ok(s.to_string())
}

/// 已知 currency 的 ISO 4217 minor exponent。
///
/// Business Logic（为什么需要这个函数）:
///     exponent 未知则无法无损转换，cost 必须保持 null。
///
/// Code Logic（这个函数做什么）:
///     常见 0/2/3 货币表；未知 None。
pub fn currency_exponent(currency: &str) -> Option<u32> {
    match currency {
        // zero-decimal
        "BIF" | "CLP" | "DJF" | "GNF" | "JPY" | "KMF" | "KRW" | "MGA" | "PYG" | "RWF" | "UGX"
        | "VND" | "VUV" | "XAF" | "XOF" | "XPF" => Some(0),
        // three-decimal
        "BHD" | "IQD" | "JOD" | "KWD" | "LYD" | "OMR" | "TND" => Some(3),
        // two-decimal common
        "USD" | "EUR" | "GBP" | "CNY" | "AUD" | "CAD" | "CHF" | "HKD" | "SGD" | "NZD" | "SEK"
        | "NOK" | "DKK" | "INR" | "MXN" | "BRL" | "ZAR" | "TRY" | "RUB" | "PLN" | "TWD" | "THB"
        | "IDR" | "MYR" | "PHP" | "AED" | "SAR" | "ILS" | "CZK" | "HUF" | "RON" | "BGN" | "HRK"
        | "ISK" | "UAH" | "ARS" | "CLF" | "COP" | "PEN" => Some(2),
        _ => None,
    }
}

/// 将主单位十进制字符串无损转为 minor units。
///
/// Business Logic（为什么需要这个函数）:
///     需要舍入或 exponent 未知时返回错误，调用方写 null。
///
/// Code Logic（这个函数做什么）:
///     解析可选符号的 `整数.小数`；小数位数 > exp → Err；否则 pad 后解析 u64。
pub fn convert_major_to_minor_units(major: &str, currency: &str) -> Result<u64, AppError> {
    let currency = validate_currency_code(currency)?;
    let exp = currency_exponent(&currency).ok_or_else(|| {
        AppError::validation(format!("未知货币 exponent，无法无损转换: {currency}"))
    })?;
    let s = major.trim();
    if s.is_empty() {
        return Err(AppError::validation("cost_major 为空"));
    }
    // 禁止科学计数与负号（cost 应为非负）
    if s.contains(['e', 'E', '+']) || s.starts_with('-') {
        return Err(AppError::validation(format!(
            "cost_major 格式非法（禁止科学计数/负数）: {major}"
        )));
    }
    let (int_part, frac_part) = match s.split_once('.') {
        Some((i, f)) => (i, f),
        None => (s, ""),
    };
    if int_part.is_empty() || !int_part.chars().all(|c| c.is_ascii_digit()) {
        return Err(AppError::validation(format!("cost_major 整数部分非法: {major}")));
    }
    if !frac_part.chars().all(|c| c.is_ascii_digit()) {
        return Err(AppError::validation(format!("cost_major 小数部分非法: {major}")));
    }
    if frac_part.len() as u32 > exp {
        return Err(AppError::validation(format!(
            "cost_major 小数位数超过 {currency} exponent={exp}，禁止舍入: {major}"
        )));
    }
    let mut digits = String::with_capacity(int_part.len() + exp as usize);
    digits.push_str(int_part);
    digits.push_str(frac_part);
    for _ in 0..(exp as usize).saturating_sub(frac_part.len()) {
        digits.push('0');
    }
    // strip leading zeros but keep single 0
    let trimmed = digits.trim_start_matches('0');
    let normalized = if trimmed.is_empty() { "0" } else { trimmed };
    normalized
        .parse::<u64>()
        .map_err(|_| AppError::validation(format!("cost_major 溢出 u64: {major}")))
}

/// 由 started/ended RFC3339 计算非负 duration_ms。
///
/// Business Logic（为什么需要这个函数）:
///     时钟回拨或非法时间不得写出负 duration。
///
/// Code Logic（这个函数做什么）:
///     解析 RFC3339；end<start → 0；否则毫秒差。
pub fn compute_duration_ms(started_at: &str, ended_at: &str) -> Result<u64, AppError> {
    let start = chrono::DateTime::parse_from_rfc3339(started_at.trim()).map_err(|e| {
        AppError::validation(format!("started_at 非法 RFC3339: {e}"))
    })?;
    let end = chrono::DateTime::parse_from_rfc3339(ended_at.trim()).map_err(|e| {
        AppError::validation(format!("ended_at 非法 RFC3339: {e}"))
    })?;
    let ms = end.signed_duration_since(start).num_milliseconds();
    if ms < 0 {
        Ok(0)
    } else {
        Ok(ms as u64)
    }
}

/// 合并两个 cumulative usage：逐字段取可靠 max，拒绝 counter 回退。
///
/// Business Logic（为什么需要这个函数）:
///     adapter 上报 cumulative 快照；回退视为不可靠，拒绝合并该字段。
///
/// Code Logic（这个函数做什么）:
///     base 与 incoming 字段级 max；incoming 小于 base 时返回 Err。
///     model_id / currency：已有值冲突则 Err；空则填。
pub fn merge_usage_monotonic(
    base: &ReliableUsageSnapshot,
    incoming: &ReliableUsageSnapshot,
) -> Result<ReliableUsageSnapshot, AppError> {
    fn merge_counter(name: &str, cur: Option<u64>, next: Option<u64>) -> Result<Option<u64>, AppError> {
        match (cur, next) {
            (Some(a), Some(b)) if b < a => Err(AppError::validation(format!(
                "usage counter 回退: {name} {b} < {a}"
            ))),
            (Some(a), Some(b)) => Ok(Some(a.max(b))),
            (Some(a), None) => Ok(Some(a)),
            (None, Some(b)) => Ok(Some(b)),
            (None, None) => Ok(None),
        }
    }

    let model_id = match (
        base.model_id.as_deref().map(str::trim).filter(|s| !s.is_empty()),
        incoming
            .model_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    ) {
        (Some(a), Some(b)) if a != b => {
            return Err(AppError::conflict(format!(
                "model_id 冲突: {a} vs {b}"
            )));
        }
        (Some(a), _) => Some(a.to_string()),
        (None, Some(b)) => Some(b.to_string()),
        (None, None) => None,
    };

    let cost_currency = match (
        base.cost_currency.as_deref(),
        incoming.cost_currency.as_deref(),
    ) {
        (Some(a), Some(b)) if a != b => {
            return Err(AppError::conflict(format!(
                "cost_currency 冲突: {a} vs {b}"
            )));
        }
        (Some(a), _) => Some(validate_currency_code(a)?),
        (None, Some(b)) => Some(validate_currency_code(b)?),
        (None, None) => None,
    };

    // cost_major：保留能产生更大或相等 minor 的一侧；无法比较时若已有则保留 base
    let cost_major = match (
        base.cost_major.as_deref(),
        incoming.cost_major.as_deref(),
        cost_currency.as_deref(),
    ) {
        (None, Some(b), _) => Some(b.to_string()),
        (Some(a), None, _) => Some(a.to_string()),
        (Some(a), Some(b), Some(cur)) => {
            let ma = convert_major_to_minor_units(a, cur).ok();
            let mb = convert_major_to_minor_units(b, cur).ok();
            match (ma, mb) {
                (Some(x), Some(y)) if y < x => {
                    return Err(AppError::validation(format!(
                        "usage cost 回退: {y} < {x}"
                    )));
                }
                (Some(x), Some(y)) if y >= x => Some(b.to_string()),
                (Some(_), None) => Some(a.to_string()),
                (None, Some(_)) => Some(b.to_string()),
                _ => Some(a.to_string()),
            }
        }
        (Some(a), Some(_b), None) => Some(a.to_string()),
        (None, None, _) => None,
    };

    Ok(ReliableUsageSnapshot {
        model_id,
        input_tokens: merge_counter("input_tokens", base.input_tokens, incoming.input_tokens)?,
        output_tokens: merge_counter("output_tokens", base.output_tokens, incoming.output_tokens)?,
        cache_read_tokens: merge_counter(
            "cache_read_tokens",
            base.cache_read_tokens,
            incoming.cache_read_tokens,
        )?,
        cache_write_tokens: merge_counter(
            "cache_write_tokens",
            base.cache_write_tokens,
            incoming.cache_write_tokens,
        )?,
        cost_major,
        cost_currency,
    })
}

/// DTO 字段名扫描：禁止敏感/正文类字段名出现在序列化形状中。
///
/// Business Logic（为什么需要这个函数）:
///     privacy 回归：Ledger DTO 不得携带 prompt/response/transcript 等键。
///
/// Code Logic（这个函数做什么）:
///     对 JSON 键路径做小写子串匹配；返回命中的 forbidden 片段。
pub fn scan_forbidden_ledger_field_names(value: &serde_json::Value) -> Vec<String> {
    const FORBIDDEN: &[&str] = &[
        "prompt",
        "response",
        "transcript_path",
        "transcriptpath",
        "terminal_bytes",
        "terminalbytes",
        "environment",
        "env",
        "credential",
        "native_session_id",
        "nativesessionid",
        "cwd",
    ];
    let mut hits = Vec::new();
    fn walk(v: &serde_json::Value, hits: &mut Vec<String>, forbidden: &[&str]) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, child) in map {
                    let lower = k.to_ascii_lowercase();
                    for f in forbidden {
                        if lower == *f || lower.contains(f) {
                            if !hits.iter().any(|h| h == f) {
                                hits.push((*f).to_string());
                            }
                        }
                    }
                    walk(child, hits, forbidden);
                }
            }
            serde_json::Value::Array(arr) => {
                for child in arr {
                    walk(child, hits, forbidden);
                }
            }
            _ => {}
        }
    }
    walk(value, &mut hits, FORBIDDEN);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic: 非法货币必须拒绝。
    /// Code Logic: 小写、长度、非字母。
    #[test]
    fn rejects_invalid_currency_codes() {
        assert!(validate_currency_code("usd").is_err());
        assert!(validate_currency_code("US").is_err());
        assert!(validate_currency_code("USDD").is_err());
        assert!(validate_currency_code("US1").is_err());
        assert_eq!(validate_currency_code("USD").unwrap(), "USD");
    }

    /// Business Logic: cost 必须无损；多一位小数禁止舍入。
    /// Code Logic: USD exp=2；JPY exp=0。
    #[test]
    fn cost_conversion_is_lossless_or_null() {
        assert_eq!(convert_major_to_minor_units("1.23", "USD").unwrap(), 123);
        assert_eq!(convert_major_to_minor_units("1", "USD").unwrap(), 100);
        assert!(convert_major_to_minor_units("1.234", "USD").is_err());
        assert_eq!(convert_major_to_minor_units("100", "JPY").unwrap(), 100);
        assert!(convert_major_to_minor_units("100.1", "JPY").is_err());
        assert!(convert_major_to_minor_units("1e2", "USD").is_err());
        assert!(convert_major_to_minor_units("-1", "USD").is_err());
    }

    /// Business Logic: counter 回退不可靠。
    /// Code Logic: merge 拒绝 b < a。
    #[test]
    fn merge_rejects_counter_rollback() {
        let base = ReliableUsageSnapshot {
            input_tokens: Some(100),
            ..Default::default()
        };
        let incoming = ReliableUsageSnapshot {
            input_tokens: Some(90),
            ..Default::default()
        };
        assert!(merge_usage_monotonic(&base, &incoming).is_err());
    }

    /// Business Logic: privacy — DTO 序列化不得含禁止字段名。
    /// Code Logic: 用最小 entry JSON 扫描。
    #[test]
    fn entry_dto_has_no_forbidden_field_names() {
        let entry = AgentLedgerEntry {
            id: "i".into(),
            agent_session_id: "a".into(),
            project_id: "p".into(),
            worktree_id: None,
            provider_id: "claudeCodeVisible".into(),
            model_id: None,
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: "2026-01-01T00:01:00Z".into(),
            duration_ms: 60_000,
            outcome: AgentLedgerOutcome::Completed,
            input_tokens: Some(1),
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            cost_minor_units: None,
            cost_currency: None,
            created_at: "2026-01-01T00:01:00Z".into(),
            updated_at: "2026-01-01T00:01:00Z".into(),
        };
        let json = serde_json::to_value(&entry).unwrap();
        let hits = scan_forbidden_ledger_field_names(&json);
        // "input_tokens" 含 token 但不是 forbidden 列表中的 prompt/response 等
        assert!(
            hits.is_empty(),
            "forbidden field names in entry DTO: {hits:?}"
        );
        // 显式确认关键禁止键不存在
        let s = json.to_string().to_ascii_lowercase();
        assert!(!s.contains("\"prompt\""));
        assert!(!s.contains("transcript"));
        assert!(!s.contains("terminalbytes"));
        assert!(!s.contains("nativesessionid"));
        assert!(!s.contains("environment"));
    }

    /// Business Logic: outcome 解析覆盖 cancelled 别名。
    /// Code Logic: canceled/cancelled 均解析。
    #[test]
    fn outcome_parse_accepts_canceled_alias() {
        assert_eq!(
            AgentLedgerOutcome::parse("canceled"),
            Some(AgentLedgerOutcome::Cancelled)
        );
        assert_eq!(AgentLedgerOutcome::Cancelled.as_str(), "cancelled");
    }
}
