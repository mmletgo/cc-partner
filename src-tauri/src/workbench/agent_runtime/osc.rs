//! workbench/agent_runtime/osc — app-private Agent OSC 帧流式解码与剥离
//!
//! Business Logic（为什么需要这个模块）:
//!     Agent adapter hook 通过 OSC 向 owner 上报 phase/version；这些帧不得进入 terminal UI/replay，
//!     且必须有界（16 KiB / 20 events/s），避免污染输出或放大 DoS。
//!
//! Code Logic（这个模块做什么）:
//!     流式状态机识别前缀 `\x1b]777;cc-partner-agent-v1;`，收集到 ST(`\x1b\\`) 或 BEL(`\x07`)，
//!     base64url 解码 JSON → `AgentRuntimeMutation`；visible 字节不含 OSC 载荷。

use super::models::{AgentRuntimeMutation, AgentSessionPhase};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;
use std::time::{Duration, Instant};

/// OSC 单帧最大字节数（含前缀后的 payload，至 ST 前）。
pub const AGENT_OSC_MAX_FRAME_BYTES: usize = 16 * 1024;

/// 每 terminal 每秒最多接受的 Agent event 数。
pub const AGENT_OSC_MAX_EVENTS_PER_SEC: u32 = 20;

/// 精确匹配的 OSC 前缀字节。
const OSC_PREFIX: &[u8] = b"\x1b]777;cc-partner-agent-v1;";

/// OSC 解码诊断（有界，不进 terminal UI）。
///
/// Business Logic（为什么需要这个类型）:
///     无效 base64、超限、未知 phase 等只能记诊断，不能把坏帧当可见输出。
///
/// Code Logic（这个类型做什么）:
///     稳定 code + 可选 detail；不含 payload 正文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOscDiagnostic {
    /// 稳定诊断 code
    pub code: &'static str,
    /// 可选短说明（无敏感内容）
    pub detail: Option<String>,
}

/// 一次 `push` 的解码结果。
///
/// Business Logic（为什么需要这个类型）:
///     调用方需要把 visible 送 UI，mutations 送 reducer，diagnostics 仅日志/指标。
///
/// Code Logic（这个类型做什么）:
///     三元组：visible 字节、解析成功的 mutation 列表、诊断列表。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[allow(dead_code)] // visible/diagnostics 由 reader 与测试共同消费
pub struct AgentOscDecodeResult {
    /// 可安全写入 terminal/replay 的字节
    pub visible: Vec<u8>,
    /// 成功解析的 mutation（已做 rate-limit 合并）
    pub mutations: Vec<AgentRuntimeMutation>,
    /// 有界诊断
    pub diagnostics: Vec<AgentOscDiagnostic>,
}

/// OSC JSON 载荷（wire 格式）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentOscPayload {
    agent_session_id: String,
    terminal_session_id: String,
    #[serde(default)]
    provider_id: Option<String>,
    #[serde(default)]
    native_session_id: Option<String>,
    phase: String,
    version: u64,
    occurred_at: String,
    #[serde(default)]
    outcome_code: Option<String>,
}

/// 流式 Agent OSC 解码器（每 terminal 一个实例）。
///
/// Business Logic（为什么需要这个结构体）:
///     PTY 输出分块到达，OSC 可能跨 chunk；且同一 terminal 需要独立 rate bucket。
///
/// Code Logic（这个结构体做什么）:
///     维护 scan 状态（Idle / MatchingPrefix / InPayload）、payload 缓冲与每秒事件桶。
#[derive(Debug)]
pub struct AgentOscDecoder {
    /// 当前扫描状态
    state: OscScanState,
    /// 正在匹配前缀时的已匹配长度
    prefix_matched: usize,
    /// payload 缓冲（前缀之后、ST 之前）
    payload_buf: Vec<u8>,
    /// 当前秒窗口起点
    rate_window_start: Instant,
    /// 当前窗口已接受事件数
    rate_window_count: u32,
    /// 当前窗口因超限被合并丢弃的事件数
    rate_coalesced: u32,
    /// 超限时暂存的最新 mutation（窗口结束或下一次接受前发出）
    pending_coalesce: Option<AgentRuntimeMutation>,
}

/// 扫描状态机。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OscScanState {
    /// 寻找 ESC 或前缀
    Idle,
    /// 已见部分前缀
    MatchingPrefix,
    /// 已完整匹配前缀，收集 payload
    InPayload,
}

impl Default for AgentOscDecoder {
    /// Business Logic（为什么需要这个函数）:
    ///     每个 terminal reader 启动时需要零状态解码器。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Idle + 空缓冲 + 新 rate 窗口。
    fn default() -> Self {
        Self {
            state: OscScanState::Idle,
            prefix_matched: 0,
            payload_buf: Vec::new(),
            rate_window_start: Instant::now(),
            rate_window_count: 0,
            rate_coalesced: 0,
            pending_coalesce: None,
        }
    }
}

impl AgentOscDecoder {
    /// 向解码器推送原始字节，返回可见输出与 mutation。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     terminal backend 在写入 replay/UI 前必须剥离 app-private OSC 并提取结构化事件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     逐字节状态机；完整帧 → decode_payload；溢出/非法 → diagnostic 且不回灌可见输出；
    ///     结束时若 1s 窗口已过期则冲刷 pending_coalesce。
    pub fn push(&mut self, input: &[u8]) -> AgentOscDecodeResult {
        let mut result = AgentOscDecodeResult::default();
        let mut i = 0;
        while i < input.len() {
            match self.state {
                OscScanState::Idle => {
                    if input[i] == 0x1b {
                        self.state = OscScanState::MatchingPrefix;
                        self.prefix_matched = 1;
                        i += 1;
                    } else {
                        // 批量拷贝直到下一个 ESC
                        if let Some(rel) = input[i..].iter().position(|&b| b == 0x1b) {
                            result.visible.extend_from_slice(&input[i..i + rel]);
                            i += rel;
                        } else {
                            result.visible.extend_from_slice(&input[i..]);
                            break;
                        }
                    }
                }
                OscScanState::MatchingPrefix => {
                    let expected = OSC_PREFIX[self.prefix_matched];
                    if input[i] == expected {
                        self.prefix_matched += 1;
                        i += 1;
                        if self.prefix_matched == OSC_PREFIX.len() {
                            self.state = OscScanState::InPayload;
                            self.payload_buf.clear();
                        }
                    } else {
                        // 前缀不匹配：把已匹配部分当可见输出回放，当前字节留给 Idle 再判
                        result
                            .visible
                            .extend_from_slice(&OSC_PREFIX[..self.prefix_matched]);
                        self.state = OscScanState::Idle;
                        self.prefix_matched = 0;
                        // 不递增 i：当前字节可能是新的 ESC
                    }
                }
                OscScanState::InPayload => {
                    // ST = ESC \ ；或 BEL 终止
                    if input[i] == 0x07 {
                        i += 1;
                        self.finish_payload(&mut result);
                        continue;
                    }
                    if input[i] == 0x1b {
                        // 可能是 ST 的 ESC
                        if i + 1 < input.len() {
                            if input[i + 1] == b'\\' {
                                i += 2;
                                self.finish_payload(&mut result);
                                continue;
                            }
                            // ESC 后不是 \：帧非法，丢弃 payload 并让 ESC 重新匹配
                            result.diagnostics.push(AgentOscDiagnostic {
                                code: "agent_osc_invalid_terminator",
                                detail: None,
                            });
                            self.reset_payload_scan();
                            // 不递增 i，从 ESC 重新 MatchingPrefix
                            continue;
                        }
                        // ESC 在 chunk 末尾：暂存，等待下一 chunk
                        // 用 payload 内嵌 0x1b 标记；下一 push 再判
                        self.payload_buf.push(0x1b);
                        i += 1;
                        if self.payload_buf.len() > AGENT_OSC_MAX_FRAME_BYTES {
                            result.diagnostics.push(AgentOscDiagnostic {
                                code: "agent_osc_frame_overflow",
                                detail: Some(format!(">{AGENT_OSC_MAX_FRAME_BYTES}")),
                            });
                            self.reset_payload_scan();
                        }
                        continue;
                    }
                    // 若缓冲以孤立 ESC 结尾，下一字节应判定 ST
                    if self.payload_buf.last() == Some(&0x1b) {
                        if input[i] == b'\\' {
                            self.payload_buf.pop();
                            i += 1;
                            self.finish_payload(&mut result);
                            continue;
                        }
                        // 不是 ST：丢弃帧，把 ESC 与当前字节按可见处理
                        result.diagnostics.push(AgentOscDiagnostic {
                            code: "agent_osc_invalid_terminator",
                            detail: None,
                        });
                        self.payload_buf.pop();
                        self.reset_payload_scan();
                        // ESC 不回灌可见（属于坏帧一部分）；当前字节继续
                        continue;
                    }
                    self.payload_buf.push(input[i]);
                    i += 1;
                    if self.payload_buf.len() > AGENT_OSC_MAX_FRAME_BYTES {
                        result.diagnostics.push(AgentOscDiagnostic {
                            code: "agent_osc_frame_overflow",
                            detail: Some(format!(">{AGENT_OSC_MAX_FRAME_BYTES}")),
                        });
                        self.reset_payload_scan();
                    }
                }
            }
        }
        // push 结束时若 rate 窗口已过期，立即冲刷 coalesce，避免依赖“下一次 PTY 字节”
        self.flush_pending_if_window_elapsed(&mut result);
        result
    }

    /// 无新字节时轮询冲刷：1s 窗口到期后投递 pending_coalesce 最后状态。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     突发 OSC 后 PTY 可能长时间静默；若不在 idle tick 冲刷，终态 phase 会永久丢失。
    ///
    /// Code Logic（这个函数做什么）:
    ///     若窗口已过期则 flush pending 并重置 rate 桶；否则返回空结果。
    pub fn poll_flush(&mut self) -> AgentOscDecodeResult {
        let mut result = AgentOscDecodeResult::default();
        self.flush_pending_if_window_elapsed(&mut result);
        result
    }

    /// 强制冲刷 pending（reader 退出 / terminal 关闭时调用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     会话结束时不能因仍在 1s 窗口内而丢弃最后 coalesced phase。
    ///
    /// Code Logic（这个函数做什么）:
    ///     无论窗口是否到期，取出 pending_coalesce 并重置 rate 计数。
    pub fn force_flush_pending(&mut self) -> AgentOscDecodeResult {
        let mut result = AgentOscDecodeResult::default();
        if self.rate_coalesced > 0 {
            result.diagnostics.push(AgentOscDiagnostic {
                code: "agent_osc_rate_limited",
                detail: Some(format!("coalesced={}", self.rate_coalesced)),
            });
        }
        if let Some(prev) = self.pending_coalesce.take() {
            result.mutations.push(prev);
        }
        self.rate_window_start = Instant::now();
        self.rate_window_count = 0;
        self.rate_coalesced = 0;
        result
    }

    /// 是否仍有待冲刷的 coalesced mutation。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     reader 循环在静默期需要决定是否 sleep+poll_flush，而不是无限阻塞 read。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 `pending_coalesce.is_some()`。
    pub fn has_pending_coalesce(&self) -> bool {
        self.pending_coalesce.is_some()
    }

    /// 距当前 rate 窗口结束的剩余时间（用于 idle tick sleep）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     reader 应在窗口到期时立刻 flush，而不是固定盲等 1s。
    ///
    /// Code Logic（这个函数做什么）:
    ///     若无 pending 返回 None；否则 `1s - elapsed`（下限 0）。
    pub fn duration_until_rate_window_end(&self) -> Option<Duration> {
        self.pending_coalesce.as_ref()?;
        let elapsed = self.rate_window_start.elapsed();
        let window = Duration::from_secs(1);
        Some(window.saturating_sub(elapsed))
    }

    /// 完成一帧 payload 的解码并应用 rate limit。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     合法 ST/BEL 后必须解析 JSON 并决定是否进入 mutations。
    ///
    /// Code Logic（这个函数做什么）:
    ///     base64url decode → JSON → phase parse → rate bucket → mutations 或 coalesce。
    fn finish_payload(&mut self, result: &mut AgentOscDecodeResult) {
        let payload = std::mem::take(&mut self.payload_buf);
        self.reset_payload_scan();
        match decode_payload_bytes(&payload) {
            Ok(mutation) => self.accept_mutation(mutation, result),
            Err(diag) => result.diagnostics.push(diag),
        }
    }

    /// 接受 mutation，应用 20/s rate limit（超出保留最后状态）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     高频 Hook 不能淹没 owner；超出部分合并为最后状态并计数。
    ///
    /// Code Logic（这个函数做什么）:
    ///     滑动 1s 窗口；未超限直接 push；超限写入 pending_coalesce 并累计 coalesced。
    fn accept_mutation(
        &mut self,
        mutation: AgentRuntimeMutation,
        result: &mut AgentOscDecodeResult,
    ) {
        if self.rate_window_start.elapsed().as_secs() >= 1 {
            self.flush_pending_if_window_elapsed(result);
        }
        if self.rate_window_count < AGENT_OSC_MAX_EVENTS_PER_SEC {
            self.rate_window_count += 1;
            result.mutations.push(mutation);
        } else {
            self.rate_coalesced += 1;
            self.pending_coalesce = Some(mutation);
        }
    }

    /// 若 rate 窗口 ≥1s，冲刷 pending 并重置计数桶。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     push / poll_flush / accept 共用同一冲刷语义，保证最后状态在窗口结束后一定投递。
    ///
    /// Code Logic（这个函数做什么）:
    ///     elapsed≥1s 时：诊断 rate_limited → push pending → 重置 window/count/coalesced。
    fn flush_pending_if_window_elapsed(&mut self, result: &mut AgentOscDecodeResult) {
        if self.rate_window_start.elapsed().as_secs() < 1 {
            return;
        }
        if self.rate_coalesced > 0 {
            result.diagnostics.push(AgentOscDiagnostic {
                code: "agent_osc_rate_limited",
                detail: Some(format!("coalesced={}", self.rate_coalesced)),
            });
        }
        if let Some(prev) = self.pending_coalesce.take() {
            result.mutations.push(prev);
        }
        self.rate_window_start = Instant::now();
        self.rate_window_count = 0;
        self.rate_coalesced = 0;
    }

    /// 重置 payload 扫描（回到 Idle）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     溢出/非法终止后必须离开 InPayload，避免污染后续输出。
    ///
    /// Code Logic（这个函数做什么）:
    ///     state=Idle，清空 prefix/payload。
    fn reset_payload_scan(&mut self) {
        self.state = OscScanState::Idle;
        self.prefix_matched = 0;
        self.payload_buf.clear();
    }
}

/// 解码 payload 字节为 mutation。
///
/// Business Logic（为什么需要这个函数）:
///     单帧闭合后需要把 base64url JSON 变成类型安全 mutation。
///
/// Code Logic（这个函数做什么）:
///     URL_SAFE_NO_PAD decode → serde_json → phase parse；失败返回诊断 code。
fn decode_payload_bytes(payload: &[u8]) -> Result<AgentRuntimeMutation, AgentOscDiagnostic> {
    if payload.is_empty() {
        return Err(AgentOscDiagnostic {
            code: "agent_osc_empty_payload",
            detail: None,
        });
    }
    let json_bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| AgentOscDiagnostic {
            code: "agent_osc_invalid_base64",
            detail: None,
        })?;
    let parsed: AgentOscPayload =
        serde_json::from_slice(&json_bytes).map_err(|e| AgentOscDiagnostic {
            code: "agent_osc_invalid_json",
            detail: Some(e.to_string()),
        })?;
    let phase = AgentSessionPhase::parse(&parsed.phase).ok_or_else(|| AgentOscDiagnostic {
        code: "agent_osc_unknown_phase",
        detail: Some(parsed.phase.clone()),
    })?;
    if parsed.agent_session_id.trim().is_empty() || parsed.terminal_session_id.trim().is_empty() {
        return Err(AgentOscDiagnostic {
            code: "agent_osc_missing_ids",
            detail: None,
        });
    }
    let event_version = parsed.version;
    let expected_version = event_version.saturating_sub(1);
    let _ = parsed.provider_id; // wire 可带 provider；reducer/create 路径使用，mutation 不改 provider
    Ok(AgentRuntimeMutation {
        agent_session_id: parsed.agent_session_id,
        terminal_session_id: parsed.terminal_session_id,
        expected_version,
        event_version,
        phase,
        native_session_id: parsed.native_session_id,
        outcome_code: parsed.outcome_code,
        occurred_at: parsed.occurred_at,
    })
}

/// 编码 mutation 为完整 OSC 帧字节（测试与 adapter 参考）。
///
/// Business Logic（为什么需要这个函数）:
///     单测需要稳定构造合法/分片帧，避免手写 base64 易错。
///
/// Code Logic（这个函数做什么）:
///     JSON camelCase → URL_SAFE_NO_PAD → 前缀 + payload + ST。
#[allow(dead_code)] // 单测 / adapter 参考帧构造 API surface
pub fn encode_agent_osc_frame(
    agent_session_id: &str,
    terminal_session_id: &str,
    phase: AgentSessionPhase,
    version: u64,
    occurred_at: &str,
) -> Vec<u8> {
    let json = serde_json::json!({
        "agentSessionId": agent_session_id,
        "terminalSessionId": terminal_session_id,
        "phase": match phase {
            AgentSessionPhase::Launching => "launching",
            AgentSessionPhase::Working => "working",
            AgentSessionPhase::NeedsInput => "needsInput",
            AgentSessionPhase::Idle => "idle",
            AgentSessionPhase::Completed => "completed",
            AgentSessionPhase::Failed => "failed",
            AgentSessionPhase::Disconnected => "disconnected",
        },
        "version": version,
        "occurredAt": occurred_at,
    });
    let b64 = URL_SAFE_NO_PAD.encode(json.to_string().as_bytes());
    let mut out = Vec::with_capacity(OSC_PREFIX.len() + b64.len() + 2);
    out.extend_from_slice(OSC_PREFIX);
    out.extend_from_slice(b64.as_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     OSC 可能跨 PTY read chunk 分片；拆帧后不得泄漏到可见输出。
    ///
    /// Code Logic（这个测试做什么）:
    ///     两段 push 拼完整帧，assert visible == beforeafter。
    #[test]
    fn split_osc_is_removed_from_visible_output() {
        let mut decoder = AgentOscDecoder::default();
        let frame = encode_agent_osc_frame(
            "a",
            "t1",
            AgentSessionPhase::Working,
            2,
            "2026-07-15T00:00:00Z",
        );
        // 找到 base64 中段切开（前缀后至少若干字节）
        let split_at = OSC_PREFIX.len() + 8;
        let mut a_input = b"before".to_vec();
        a_input.extend_from_slice(&frame[..split_at]);
        let mut b_input = frame[split_at..].to_vec();
        b_input.extend_from_slice(b"after");
        let a = decoder.push(&a_input);
        let b = decoder.push(&b_input);
        assert_eq!([a.visible, b.visible].concat(), b"beforeafter");
        assert_eq!(b.mutations.len(), 1);
        assert_eq!(b.mutations[0].agent_session_id, "a");
        assert_eq!(b.mutations[0].event_version, 2);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     计划用例：分片在 base64 中间切断时 visible 仍无载荷。
    ///
    /// Code Logic（这个测试做什么）:
    ///     手工构造与计划一致的 before + partial + rest + after。
    #[test]
    fn split_osc_plan_example_removed_from_visible() {
        let mut decoder = AgentOscDecoder::default();
        let frame = encode_agent_osc_frame(
            "a",
            "term",
            AgentSessionPhase::Working,
            3,
            "2026-07-15T00:00:01Z",
        );
        let mid = frame.len() / 2;
        let a = decoder.push([b"before".as_slice(), &frame[..mid]].concat().as_slice());
        let b = decoder.push(&[&frame[mid..], b"after"].concat());
        assert_eq!(
            [a.visible.as_slice(), b.visible.as_slice()].concat(),
            b"beforeafter"
        );
        assert_eq!(a.mutations.len() + b.mutations.len(), 1);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     无效 base64 不得进 UI，只能诊断。
    ///
    /// Code Logic（这个测试做什么）:
    ///     前缀 + 非法 base64 + ST；visible 空，diagnostics 含 invalid_base64。
    #[test]
    fn invalid_base64_yields_diagnostic_not_visible() {
        let mut decoder = AgentOscDecoder::default();
        let mut frame = OSC_PREFIX.to_vec();
        frame.extend_from_slice(b"!!!not-base64!!!");
        frame.extend_from_slice(b"\x1b\\");
        let r = decoder.push(&[&b"x"[..], &frame[..], &b"y"[..]].concat());
        assert_eq!(r.visible, b"xy");
        assert!(r.mutations.is_empty());
        assert!(r
            .diagnostics
            .iter()
            .any(|d| d.code == "agent_osc_invalid_base64"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     ST 可分片到达（ESC 与 \\ 分属两 chunk）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     push 到 ESC 止，再 push `\\after`，mutation 在第二段完成。
    #[test]
    fn fragmented_st_completes_on_next_chunk() {
        let mut decoder = AgentOscDecoder::default();
        let frame = encode_agent_osc_frame(
            "sess",
            "term",
            AgentSessionPhase::Idle,
            1,
            "2026-07-15T00:00:00Z",
        );
        // frame 以 ESC \ 结束；去掉最后 2 字节作为第一段，第二段给 ST
        let body = &frame[..frame.len() - 2];
        let a = decoder.push(body);
        assert!(a.mutations.is_empty());
        let b = decoder.push(b"\x1b\\after");
        assert_eq!(b.visible, b"after");
        assert_eq!(b.mutations.len(), 1);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     超过 16 KiB 的帧必须丢弃并诊断，防止缓冲膨胀。
    ///
    /// Code Logic（这个测试做什么）:
    ///     前缀 + >16KiB payload 无 ST；随后正常文本可见。
    #[test]
    fn frame_over_16kib_is_dropped() {
        let mut decoder = AgentOscDecoder::default();
        let mut huge = OSC_PREFIX.to_vec();
        huge.extend(std::iter::repeat(b'A').take(AGENT_OSC_MAX_FRAME_BYTES + 8));
        let r = decoder.push(&huge);
        assert!(r
            .diagnostics
            .iter()
            .any(|d| d.code == "agent_osc_frame_overflow"));
        assert!(r.mutations.is_empty());
        // 溢出后状态复位，后续文本可见
        let r2 = decoder.push(b"ok");
        assert_eq!(r2.visible, b"ok");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     同一 chunk 可含两帧 OSC，都必须剥离并解析。
    ///
    /// Code Logic（这个测试做什么）:
    ///     two frames 夹在 visible 之间。
    #[test]
    fn two_frames_in_one_chunk() {
        let mut decoder = AgentOscDecoder::default();
        let f1 = encode_agent_osc_frame(
            "s1",
            "t",
            AgentSessionPhase::Working,
            2,
            "2026-07-15T00:00:00Z",
        );
        let f2 = encode_agent_osc_frame(
            "s1",
            "t",
            AgentSessionPhase::Idle,
            3,
            "2026-07-15T00:00:01Z",
        );
        let mut input = b"A".to_vec();
        input.extend_from_slice(&f1);
        input.extend_from_slice(b"B");
        input.extend_from_slice(&f2);
        input.extend_from_slice(b"C");
        let r = decoder.push(&input);
        assert_eq!(r.visible, b"ABC");
        assert_eq!(r.mutations.len(), 2);
        assert_eq!(r.mutations[0].event_version, 2);
        assert_eq!(r.mutations[1].event_version, 3);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     普通 OSC（非 agent 前缀）必须原样透传，不能吞掉用户程序输出。
    ///
    /// Code Logic（这个测试做什么）:
    ///     `\x1b]0;title\x07` 完整出现在 visible。
    #[test]
    fn ordinary_osc_passthrough() {
        let mut decoder = AgentOscDecoder::default();
        let ordinary = b"\x1b]0;title\x07hello";
        let r = decoder.push(ordinary);
        assert_eq!(r.visible, ordinary);
        assert!(r.mutations.is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     每秒超过 20 个 event 时必须合并为最后状态，避免淹没 owner。
    ///
    /// Code Logic（这个测试做什么）:
    ///     连续 push 25 个完整帧；mutations 接受数 ≤ 20，pending coalesce 在窗口逻辑内保留最后。
    #[test]
    fn rate_limit_coalesces_beyond_20_per_second() {
        let mut decoder = AgentOscDecoder::default();
        let mut accepted = 0usize;
        let mut last_version = 0u64;
        for v in 1..=25u64 {
            let frame = encode_agent_osc_frame(
                "s",
                "t",
                AgentSessionPhase::Working,
                v,
                "2026-07-15T00:00:00Z",
            );
            let r = decoder.push(&frame);
            accepted += r.mutations.len();
            if let Some(m) = r.mutations.last() {
                last_version = m.event_version;
            }
        }
        // 直接接受最多 20；第 21–25 进入 coalesce
        assert!(accepted <= 20);
        assert_eq!(decoder.rate_window_count, 20);
        assert!(decoder.rate_coalesced >= 5);
        if let Some(pending) = &decoder.pending_coalesce {
            last_version = pending.event_version;
        }
        assert_eq!(last_version, 25);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     突发后 PTY 静默时，coalesced 终态必须在 1s 窗口后经 poll_flush 投递，不能永远挂起。
    ///
    /// Code Logic（这个测试做什么）:
    ///     push 25 帧（最后为 Completed）→ pending 持有 v25 → sleep 1.05s → poll_flush 交付 Completed。
    #[test]
    fn rate_limit_idle_poll_flush_delivers_final_phase() {
        let mut decoder = AgentOscDecoder::default();
        for v in 1..=25u64 {
            let phase = if v == 25 {
                AgentSessionPhase::Completed
            } else {
                AgentSessionPhase::Working
            };
            let frame = encode_agent_osc_frame("s", "t", phase, v, "2026-07-15T00:00:00Z");
            let _ = decoder.push(&frame);
        }
        assert!(decoder.has_pending_coalesce());
        assert_eq!(
            decoder.pending_coalesce.as_ref().map(|m| m.event_version),
            Some(25)
        );
        // 无更多 push：仅靠 idle tick
        std::thread::sleep(Duration::from_millis(1_050));
        let flushed = decoder.poll_flush();
        assert_eq!(flushed.mutations.len(), 1);
        assert_eq!(flushed.mutations[0].event_version, 25);
        assert_eq!(flushed.mutations[0].phase, AgentSessionPhase::Completed);
        assert!(!decoder.has_pending_coalesce());
        // 再次 poll 不应重复交付
        assert!(decoder.poll_flush().mutations.is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     BEL 终止的 OSC 也必须识别（部分终端用 BEL 代替 ST）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     前缀 + base64 + BEL。
    #[test]
    fn bel_terminator_accepted() {
        let mut decoder = AgentOscDecoder::default();
        let mut frame = encode_agent_osc_frame(
            "s",
            "t",
            AgentSessionPhase::Working,
            2,
            "2026-07-15T00:00:00Z",
        );
        // 替换末尾 ST 为 BEL
        frame.truncate(frame.len() - 2);
        frame.push(0x07);
        let r = decoder.push(&frame);
        assert_eq!(r.mutations.len(), 1);
        assert!(r.visible.is_empty());
    }
}
