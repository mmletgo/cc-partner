//! workbench/agent_runtime/opencode_bridge — OpenCode OSC runtime bridge
//!
//! Business Logic（为什么需要这个模块）:
//!     OpenCode 可见 Runner 不能解析人类 stdout 文本完成；必须用项目内派生 Plugin 订阅官方
//!     session/permission 事件，经 app-private OSC 进入既有 AgentOscDecoder/reducer。
//!     该文件是 app-version 派生物，不是用户 canonical Plugin，不进入 Snapshot。
//!
//! Code Logic（这个模块做什么）:
//!     生成确定性 TypeScript 源（hash 钉死）、preview/materialize/verify 桥文件、
//!     官方事件→Agent phase 映射（含 seenActive 守卫），以及 reserved-path 碰撞检测。

use crate::agent_hub::object_store::sha256_hex;
use crate::agent_hub::projection::atomic_writer::{
    AtomicProjectionWriter, AtomicWriteOutcome, FileWriteRequest,
};
use crate::error::AppError;
use crate::workbench::agent_runtime::models::{AgentRuntimeMutation, AgentSessionPhase};
use crate::workbench::agent_runtime::osc::encode_agent_osc_frame_full;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 项目内保留的 OpenCode runtime bridge 相对路径。
pub const OPENCODE_RUNTIME_BRIDGE_REL_PATH: &str = ".opencode/plugins/cc-partner-runtime.ts";

/// 钉死的生成源 SHA-256（app 升级必须显式变更并出现 preview diff）。
pub const OPENCODE_RUNTIME_BRIDGE_SOURCE_HASH: &str =
    "4971b1bee3448878552d82bff688cbc5891212f1d510d260b57068ce028dd6d7";

/// provider wire id（与 openCodeVisible 对齐；Task5 注册前本地常量）。
pub const OPENCODE_VISIBLE_PROVIDER_ID: &str = "openCodeVisible";

/// 稳定错误/状态 code：未 opt-in 时仅 preview，provider 必须 fail-closed。
#[allow(dead_code)] // Task5/UI 消费稳定 code 字面量
pub const CODE_RUNTIME_BRIDGE_REQUIRED: &str = "runtimeBridgeRequired";

/// 稳定错误/状态 code：保留路径存在不同字节，禁止静默覆盖。
#[allow(dead_code)] // Task5/UI 与 portable 扫描消费稳定 code
pub const CODE_EXTERNAL_COLLISION: &str = "externalCollision";

/// 官方 OpenCode plugin 事件（测试夹具与映射输入）。
///
/// Business Logic（为什么需要这个枚举）:
///     bridge 只信任官方 event 名称；不得从人类 stdout 猜测。
///
/// Code Logic（这个枚举做什么）:
///     覆盖 session.status / session.idle / session.error / permission.asked。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenCodeOfficialEvent {
    /// session.status，payload 含 status 字符串
    SessionStatus {
        /// native session id
        session_id: String,
        /// busy | retry | idle | 其它
        status: String,
    },
    /// session.idle
    SessionIdle {
        /// native session id
        session_id: String,
    },
    /// session.error
    SessionError {
        /// native session id
        session_id: String,
    },
    /// permission.asked（不携带权限正文）
    PermissionAsked {
        /// native session id
        session_id: String,
    },
}

/// 单次映射输出的 OSC 帧语义（不含敏感正文）。
///
/// Business Logic（为什么需要这个结构体）:
///     测试与 verify 路径需要断言 phase/version/native id，而不是原始 TypeScript。
///
/// Code Logic（这个结构体做什么）:
///     phase + event_version + native_session_id + occurred_at + 完整 OSC 字节。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeMappedFrame {
    /// Agent phase
    pub phase: AgentSessionPhase,
    /// 严格递增，从 2 起
    pub event_version: u64,
    /// 绑定的 native session
    pub native_session_id: String,
    /// RFC3339
    pub occurred_at: String,
    /// 完整 OSC 帧字节
    pub osc_bytes: Vec<u8>,
}

/// preview / materialize / verify 结果。
///
/// Business Logic（为什么需要这个枚举）:
///     未 opt-in、碰撞、已同步、已写入必须可区分；provider 只在 Verified 时启用。
///
/// Code Logic（这个枚举做什么）:
///     携带相对路径、hash、稳定 code。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum OpenCodeBridgeOutcome {
    /// 仅预览，未写盘
    Preview {
        relative_path: String,
        source_hash: String,
        would_create: bool,
        would_overwrite: bool,
        code: Option<String>,
    },
    /// 已与期望字节一致
    Verified {
        relative_path: String,
        source_hash: String,
        absolute_path: String,
    },
    /// 原子写入成功
    Materialized {
        relative_path: String,
        source_hash: String,
        absolute_path: String,
    },
    /// 未 opt-in：provider 必须 fail-closed
    RuntimeBridgeRequired {
        relative_path: String,
        source_hash: String,
    },
    /// 保留路径存在不同内容
    ExternalCollision {
        relative_path: String,
        source_hash: String,
        absolute_path: String,
        current_hash: Option<String>,
    },
}

/// OpenCode runtime bridge 生成/校验 API。
///
/// Business Logic（为什么需要这个结构体）:
///     Orchestrator/project opt-in 需要 preview→materialize→verify 同一份确定性源。
///
/// Code Logic（这个结构体做什么）:
///     无状态；全部方法基于当前 app 版本常量源。
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenCodeRuntimeBridge;

/// 事件映射状态机（每 bound native session 一个实例语义）。
///
/// Business Logic（为什么需要这个结构体）:
///     session.idle 可能在 prompt 真正开始前出现；Completed 只能在 seenActive 之后。
///
/// Code Logic（这个结构体做什么）:
///     绑定首个 native session、seenActive、event_version 从 1 起每次 emit +1。
#[derive(Debug, Clone)]
pub struct OpenCodeEventMapper {
    agent_session_id: String,
    terminal_session_id: String,
    bound_native_session_id: Option<String>,
    seen_active: bool,
    event_version: u64,
}

impl OpenCodeEventMapper {
    /// 构造 mapper。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Plugin/OSC 需要固定 agent/terminal 身份，才能让 decoder 关联 runtime 行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     version=1；seen_active=false；未绑定 native。
    pub fn new(
        agent_session_id: impl Into<String>,
        terminal_session_id: impl Into<String>,
    ) -> Self {
        Self {
            agent_session_id: agent_session_id.into(),
            terminal_session_id: terminal_session_id.into(),
            bound_native_session_id: None,
            seen_active: false,
            event_version: 1,
        }
    }

    /// 当前是否已见 busy/retry/permission。
    pub fn seen_active(&self) -> bool {
        self.seen_active
    }

    /// 已绑定的 native session（首个）。
    pub fn bound_native_session_id(&self) -> Option<&str> {
        self.bound_native_session_id.as_deref()
    }

    /// 映射官方事件为可选 OSC 帧。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     多 session 输出必须忽略非绑定 session；pre-active idle 不得 completed。
    ///
    /// Code Logic（这个函数做什么）:
    ///     bind 首个 sessionID；busy/retry→working+seenActive；permission→needsInput；
    ///     status idle→idle；session.idle→completed 仅 seenActive，否则 idle；error→failed。
    pub fn map_event(&mut self, event: &OpenCodeOfficialEvent) -> Option<OpenCodeMappedFrame> {
        let (session_id, phase, mark_active) = match event {
            OpenCodeOfficialEvent::SessionStatus { session_id, status } => {
                let s = status.as_str();
                if s == "busy" || s == "retry" {
                    (session_id.as_str(), AgentSessionPhase::Working, true)
                } else if s == "idle" {
                    (session_id.as_str(), AgentSessionPhase::Idle, false)
                } else {
                    return None;
                }
            }
            OpenCodeOfficialEvent::PermissionAsked { session_id } => {
                (session_id.as_str(), AgentSessionPhase::NeedsInput, true)
            }
            OpenCodeOfficialEvent::SessionIdle { session_id } => {
                if self.bound_native_session_id.is_none() {
                    self.bound_native_session_id = Some(session_id.clone());
                }
                if self.bound_native_session_id.as_deref() != Some(session_id.as_str()) {
                    return None;
                }
                let phase = if self.seen_active {
                    AgentSessionPhase::Completed
                } else {
                    AgentSessionPhase::Idle
                };
                return Some(self.emit(phase, session_id));
            }
            OpenCodeOfficialEvent::SessionError { session_id } => {
                (session_id.as_str(), AgentSessionPhase::Failed, false)
            }
        };

        if self.bound_native_session_id.is_none() {
            self.bound_native_session_id = Some(session_id.to_string());
        }
        if self.bound_native_session_id.as_deref() != Some(session_id) {
            return None;
        }
        if mark_active {
            self.seen_active = true;
        }
        Some(self.emit(phase, session_id))
    }

    /// 生成 mutation（供 reducer 测试）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     OSC 解码后的 mutation 形状必须与 bridge 帧一致。
    ///
    /// Code Logic（这个函数做什么）:
    ///     expected_version = event_version - 1。
    pub fn frame_to_mutation(
        frame: &OpenCodeMappedFrame,
        agent_session_id: &str,
        terminal_session_id: &str,
    ) -> AgentRuntimeMutation {
        AgentRuntimeMutation {
            agent_session_id: agent_session_id.to_string(),
            terminal_session_id: terminal_session_id.to_string(),
            expected_version: frame.event_version.saturating_sub(1),
            event_version: frame.event_version,
            phase: frame.phase,
            native_session_id: Some(frame.native_session_id.clone()),
            outcome_code: None,
            occurred_at: frame.occurred_at.clone(),
        }
    }

    fn emit(&mut self, phase: AgentSessionPhase, native_session_id: &str) -> OpenCodeMappedFrame {
        self.event_version = self.event_version.saturating_add(1);
        let occurred_at = chrono::Utc::now().to_rfc3339();
        let osc_bytes = encode_agent_osc_frame_full(
            &self.agent_session_id,
            &self.terminal_session_id,
            Some(OPENCODE_VISIBLE_PROVIDER_ID),
            Some(native_session_id),
            phase,
            self.event_version,
            &occurred_at,
            None,
        );
        OpenCodeMappedFrame {
            phase,
            event_version: self.event_version,
            native_session_id: native_session_id.to_string(),
            occurred_at,
            osc_bytes,
        }
    }
}

impl OpenCodeRuntimeBridge {
    /// 返回当前 app 版本的确定性 TypeScript 源。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     升级必须改变 hash 并出现 project preview diff；源不得依赖随机性。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 `OPENCODE_RUNTIME_BRIDGE_TYPESCRIPT` 常量。
    pub fn generated_source() -> &'static str {
        OPENCODE_RUNTIME_BRIDGE_TYPESCRIPT
    }

    /// 返回生成源 SHA-256。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     materialize/verify/snapshot 测试钉死同一 hash。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验常量 hash 与实时 sha256 一致后返回。
    pub fn generated_source_hash() -> &'static str {
        debug_assert_eq!(
            sha256_hex(Self::generated_source().as_bytes()),
            OPENCODE_RUNTIME_BRIDGE_SOURCE_HASH
        );
        OPENCODE_RUNTIME_BRIDGE_SOURCE_HASH
    }

    /// 项目根下的绝对路径。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     只允许精确保留路径 `.opencode/plugins/cc-partner-runtime.ts`。
    ///
    /// Code Logic（这个函数做什么）:
    ///     join 固定相对路径。
    pub fn absolute_path(project_root: &Path) -> PathBuf {
        project_root.join(OPENCODE_RUNTIME_BRIDGE_REL_PATH)
    }

    /// 未写盘预览：报告是否需要创建/会否碰撞。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     未 opt-in 项目只能 preview + runtimeBridgeRequired，不得 materialize。
    ///
    /// Code Logic（这个函数做什么）:
    ///     opted_in=false → RuntimeBridgeRequired 包装的 Preview 语义；
    ///     存在不同字节 → ExternalCollision；否则 Preview。
    pub fn preview(project_root: &Path, opted_in: bool) -> OpenCodeBridgeOutcome {
        let rel = OPENCODE_RUNTIME_BRIDGE_REL_PATH.to_string();
        let hash = Self::generated_source_hash().to_string();
        let target = Self::absolute_path(project_root);
        if !opted_in {
            return OpenCodeBridgeOutcome::RuntimeBridgeRequired {
                relative_path: rel,
                source_hash: hash,
            };
        }
        match current_file_hash(&target) {
            Ok(Some(current)) if current != hash => OpenCodeBridgeOutcome::ExternalCollision {
                relative_path: rel,
                source_hash: hash,
                absolute_path: target.display().to_string(),
                current_hash: Some(current),
            },
            Ok(Some(_)) => OpenCodeBridgeOutcome::Preview {
                relative_path: rel,
                source_hash: hash,
                would_create: false,
                would_overwrite: false,
                code: None,
            },
            Ok(None) => OpenCodeBridgeOutcome::Preview {
                relative_path: rel,
                source_hash: hash,
                would_create: true,
                would_overwrite: false,
                code: None,
            },
            Err(_) => OpenCodeBridgeOutcome::Preview {
                relative_path: rel,
                source_hash: hash,
                would_create: true,
                would_overwrite: false,
                code: Some("path_unreadable".into()),
            },
        }
    }

    /// 在已 opt-in 的 checkout 原子物化 bridge 文件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Workbench main/worktree 启动 OpenCode 前必须有 hash 验证的派生 Plugin。
    ///
    /// Code Logic（这个函数做什么）:
    ///     未 opt-in → RuntimeBridgeRequired；不同字节 → ExternalCollision（不写）；
    ///     已是期望 hash → Verified；否则 AtomicProjectionWriter（expected=None 仅当不存在，
    ///     存在且相同已短路；存在不同已 collision）。
    pub fn materialize(
        project_root: &Path,
        opted_in: bool,
    ) -> Result<OpenCodeBridgeOutcome, AppError> {
        let rel = OPENCODE_RUNTIME_BRIDGE_REL_PATH.to_string();
        let hash = Self::generated_source_hash().to_string();
        let target = Self::absolute_path(project_root);
        if !opted_in {
            return Ok(OpenCodeBridgeOutcome::RuntimeBridgeRequired {
                relative_path: rel,
                source_hash: hash,
            });
        }
        let current = current_file_hash(&target)?;
        if let Some(ref cur) = current {
            if cur != &hash {
                return Ok(OpenCodeBridgeOutcome::ExternalCollision {
                    relative_path: rel,
                    source_hash: hash,
                    absolute_path: target.display().to_string(),
                    current_hash: Some(cur.clone()),
                });
            }
            return Ok(OpenCodeBridgeOutcome::Verified {
                relative_path: rel,
                source_hash: hash,
                absolute_path: target.display().to_string(),
            });
        }

        let bytes = Self::generated_source().as_bytes();
        let writer = AtomicProjectionWriter::new();
        let outcome = writer.write_file(FileWriteRequest {
            target: &target,
            rendered_bytes: bytes,
            rendered_hash: &hash,
            expected_external_hash: None,
        })?;
        match outcome {
            AtomicWriteOutcome::Replaced { .. } | AtomicWriteOutcome::AlreadyRendered { .. } => {
                Ok(OpenCodeBridgeOutcome::Materialized {
                    relative_path: rel,
                    source_hash: hash,
                    absolute_path: target.display().to_string(),
                })
            }
            AtomicWriteOutcome::Drift { current_hash } => {
                Ok(OpenCodeBridgeOutcome::ExternalCollision {
                    relative_path: rel,
                    source_hash: hash,
                    absolute_path: target.display().to_string(),
                    current_hash,
                })
            }
            AtomicWriteOutcome::DirectoryUnknownFiles { .. } => Err(AppError::generic(
                "opencode_runtime_bridge_unexpected_directory_outcome",
            )),
        }
    }

    /// 校验磁盘文件 hash 是否等于当前 app 生成源。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     provider 启用前必须 evidence：文件存在且 hash 匹配；碰撞/缺失 fail-closed。
    ///
    /// Code Logic（这个函数做什么）:
    ///     未 opt-in → RuntimeBridgeRequired；缺失/不同 → ExternalCollision 或 Required；匹配 → Verified。
    pub fn verify(project_root: &Path, opted_in: bool) -> OpenCodeBridgeOutcome {
        let rel = OPENCODE_RUNTIME_BRIDGE_REL_PATH.to_string();
        let hash = Self::generated_source_hash().to_string();
        let target = Self::absolute_path(project_root);
        if !opted_in {
            return OpenCodeBridgeOutcome::RuntimeBridgeRequired {
                relative_path: rel,
                source_hash: hash,
            };
        }
        match current_file_hash(&target) {
            Ok(Some(current)) if current == hash => OpenCodeBridgeOutcome::Verified {
                relative_path: rel,
                source_hash: hash,
                absolute_path: target.display().to_string(),
            },
            Ok(Some(current)) => OpenCodeBridgeOutcome::ExternalCollision {
                relative_path: rel,
                source_hash: hash,
                absolute_path: target.display().to_string(),
                current_hash: Some(current),
            },
            Ok(None) => OpenCodeBridgeOutcome::RuntimeBridgeRequired {
                relative_path: rel,
                source_hash: hash,
            },
            Err(_) => OpenCodeBridgeOutcome::RuntimeBridgeRequired {
                relative_path: rel,
                source_hash: hash,
            },
        }
    }

    /// 扫描时判断保留路径字节是否为派生 bridge（忽略）或外部碰撞。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     OpenCode portable 扫描不得把匹配生成字节的 bridge 当作用户 Plugin 导入；
    ///     不同字节必须 externalCollision，禁止静默覆盖。
    ///
    /// Code Logic（这个函数做什么）:
    ///     相对路径精确匹配 → Some(true=our bytes / false=collision)；其它路径 None。
    pub fn classify_reserved_path(relative_path: &str, bytes: &[u8]) -> Option<bool> {
        let norm = relative_path.trim_start_matches("./").replace('\\', "/");
        if norm != OPENCODE_RUNTIME_BRIDGE_REL_PATH {
            return None;
        }
        let h = sha256_hex(bytes);
        Some(h == OPENCODE_RUNTIME_BRIDGE_SOURCE_HASH)
    }
}

/// 读文件 hash；不存在 → None。
///
/// Business Logic（为什么需要这个函数）:
///     preview/verify 需要区分缺失与碰撞。
///
/// Code Logic（这个函数做什么）:
///     读全量字节后 sha256；NotFound → None。
fn current_file_hash(path: &Path) -> Result<Option<String>, AppError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(sha256_hex(&bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(AppError::from(e)),
    }
}

/// 确定性生成的 OpenCode Plugin TypeScript 源（依赖-free）。
///
/// Business Logic（为什么需要这个常量）:
///     必须可复现；hash 钉在 OPENCODE_RUNTIME_BRIDGE_SOURCE_HASH。
///
/// Code Logic（这个常量做什么）:
///     event hook + OSC base64url 写出；只读两个 ID env；不回显 secret。
const OPENCODE_RUNTIME_BRIDGE_TYPESCRIPT: &str = r###"/**
 * cc-partner OpenCode runtime bridge (app-version derived system materialization).
 * NOT a user canonical Plugin; excluded from Snapshot.
 * Emits app-private OSC only; never includes prompt/message/permission content or env secrets.
 */
type PluginEvent = {
  on: (name: string, handler: (payload: Record<string, unknown>) => void | Promise<void>) => void;
};

type PluginApi = {
  event: PluginEvent;
};

function readRequiredEnv(name: string): string {
  const value = process.env[name];
  if (typeof value !== "string") return "";
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : "";
}

function emitOsc(
  phase: string,
  agentSessionId: string,
  terminalSessionId: string,
  nativeSessionId: string,
  version: number,
): void {
  const payload = {
    agentSessionId,
    terminalSessionId,
    providerId: "openCodeVisible",
    nativeSessionId,
    phase,
    version,
    occurredAt: new Date().toISOString(),
  };
  const json = JSON.stringify(payload);
  const b64 = Buffer.from(json, "utf8").toString("base64url");
  process.stdout.write("]777;cc-partner-agent-v1;" + b64 + "\");
}

export default async function ccPartnerRuntimePlugin(api: PluginApi): Promise<void> {
  const agentSessionId = readRequiredEnv("CC_PARTNER_AGENT_SESSION_ID");
  const terminalSessionId = readRequiredEnv("CC_PARTNER_TERMINAL_SESSION_ID");
  if (!agentSessionId || !terminalSessionId) {
    return;
  }

  let boundNativeSessionId: string | null = null;
  let seenActive = false;
  let eventVersion = 1;

  const bindAndFilter = (payload: Record<string, unknown>): string | null => {
    const raw = payload.sessionID ?? payload.sessionId;
    if (typeof raw !== "string" || raw.trim().length === 0) {
      return null;
    }
    const sid = raw.trim();
    if (boundNativeSessionId === null) {
      boundNativeSessionId = sid;
    }
    if (sid !== boundNativeSessionId) {
      return null;
    }
    return sid;
  };

  const nextEmit = (phase: string, nativeSessionId: string): void => {
    eventVersion += 1;
    emitOsc(phase, agentSessionId, terminalSessionId, nativeSessionId, eventVersion);
  };

  api.event.on("session.status", (payload) => {
    const sid = bindAndFilter(payload);
    if (!sid) return;
    const status = typeof payload.status === "string" ? payload.status : "";
    if (status === "busy" || status === "retry") {
      seenActive = true;
      nextEmit("working", sid);
      return;
    }
    if (status === "idle") {
      nextEmit("idle", sid);
    }
  });

  api.event.on("permission.asked", (payload) => {
    const sid = bindAndFilter(payload);
    if (!sid) return;
    seenActive = true;
    nextEmit("needsInput", sid);
  });

  api.event.on("session.idle", (payload) => {
    const sid = bindAndFilter(payload);
    if (!sid) return;
    if (seenActive) {
      nextEmit("completed", sid);
    } else {
      nextEmit("idle", sid);
    }
  });

  api.event.on("session.error", (payload) => {
    const sid = bindAndFilter(payload);
    if (!sid) return;
    nextEmit("failed", sid);
  });
}
"###;

// Ensure compile-time hash documentation matches.
const _: () = {
    // 长度守卫：hash 必须 64 hex
    assert!(OPENCODE_RUNTIME_BRIDGE_SOURCE_HASH.len() == 64);
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::agent_runtime::osc::AgentOscDecoder;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use tempfile::TempDir;

    /// Business Logic: 生成源 hash 钉死，升级必须显式改测试。
    #[test]
    fn generated_source_hash_is_pinned() {
        let live = sha256_hex(OpenCodeRuntimeBridge::generated_source().as_bytes());
        assert_eq!(live, OPENCODE_RUNTIME_BRIDGE_SOURCE_HASH);
        assert_eq!(OpenCodeRuntimeBridge::generated_source_hash(), live);
        let src = OpenCodeRuntimeBridge::generated_source();
        assert!(src.contains("CC_PARTNER_AGENT_SESSION_ID"));
        assert!(src.contains("CC_PARTNER_TERMINAL_SESSION_ID"));
        assert!(src.contains("session.status"));
        assert!(src.contains("session.idle"));
        assert!(src.contains("session.error"));
        assert!(src.contains("permission.asked"));
        assert!(src.contains("cc-partner-agent-v1"));
        assert!(src.contains("base64url"));
        assert!(src.contains("process.stdout.write"));
        // 不得嵌入 prompt/message 正文或 env secret 回显
        assert!(!src.contains("process.env.OPENAI"));
        assert!(!src.contains("API_KEY"));
        assert!(!src.contains("permission.content"));
    }

    /// Business Logic: 官方事件映射到 phase；version 从 2 起严格递增。
    #[test]
    fn event_mapping_busy_permission_idle_completed_failed() {
        let mut mapper = OpenCodeEventMapper::new("agent-a", "term-t");
        let f1 = mapper
            .map_event(&OpenCodeOfficialEvent::SessionStatus {
                session_id: "n1".into(),
                status: "busy".into(),
            })
            .unwrap();
        assert_eq!(f1.phase, AgentSessionPhase::Working);
        assert_eq!(f1.event_version, 2);
        assert_eq!(f1.native_session_id, "n1");

        let f2 = mapper
            .map_event(&OpenCodeOfficialEvent::SessionStatus {
                session_id: "n1".into(),
                status: "retry".into(),
            })
            .unwrap();
        assert_eq!(f2.phase, AgentSessionPhase::Working);
        assert_eq!(f2.event_version, 3);

        let f3 = mapper
            .map_event(&OpenCodeOfficialEvent::PermissionAsked {
                session_id: "n1".into(),
            })
            .unwrap();
        assert_eq!(f3.phase, AgentSessionPhase::NeedsInput);
        assert_eq!(f3.event_version, 4);

        let f4 = mapper
            .map_event(&OpenCodeOfficialEvent::SessionStatus {
                session_id: "n1".into(),
                status: "idle".into(),
            })
            .unwrap();
        assert_eq!(f4.phase, AgentSessionPhase::Idle);
        assert_eq!(f4.event_version, 5);

        let f5 = mapper
            .map_event(&OpenCodeOfficialEvent::SessionIdle {
                session_id: "n1".into(),
            })
            .unwrap();
        assert_eq!(f5.phase, AgentSessionPhase::Completed);
        assert_eq!(f5.event_version, 6);

        let f6 = mapper
            .map_event(&OpenCodeOfficialEvent::SessionError {
                session_id: "n1".into(),
            })
            .unwrap();
        assert_eq!(f6.phase, AgentSessionPhase::Failed);
        assert_eq!(f6.event_version, 7);

        // OSC 可被既有 decoder 接受
        let mut dec = AgentOscDecoder::default();
        let r = dec.push(&f1.osc_bytes);
        assert_eq!(r.mutations.len(), 1);
        assert_eq!(r.mutations[0].phase, AgentSessionPhase::Working);
        assert_eq!(r.mutations[0].event_version, 2);
        assert_eq!(r.mutations[0].native_session_id.as_deref(), Some("n1"));
        assert!(r.visible.is_empty());
    }

    /// Business Logic: pre-active session.idle 只能 idle，不能 completed。
    #[test]
    fn pre_active_session_idle_stays_idle_not_completed() {
        let mut mapper = OpenCodeEventMapper::new("a", "t");
        let f = mapper
            .map_event(&OpenCodeOfficialEvent::SessionIdle {
                session_id: "n".into(),
            })
            .unwrap();
        assert_eq!(f.phase, AgentSessionPhase::Idle);
        assert!(!mapper.seen_active());
        assert_eq!(f.event_version, 2);
    }

    /// Business Logic: 其它 native session 事件忽略。
    #[test]
    fn ignores_events_from_other_native_sessions() {
        let mut mapper = OpenCodeEventMapper::new("a", "t");
        let _ = mapper
            .map_event(&OpenCodeOfficialEvent::SessionStatus {
                session_id: "n1".into(),
                status: "busy".into(),
            })
            .unwrap();
        assert!(mapper
            .map_event(&OpenCodeOfficialEvent::SessionStatus {
                session_id: "n2".into(),
                status: "busy".into(),
            })
            .is_none());
        assert!(mapper
            .map_event(&OpenCodeOfficialEvent::SessionIdle {
                session_id: "n2".into(),
            })
            .is_none());
    }

    /// Business Logic: 未 opt-in 只 preview 语义 + runtimeBridgeRequired。
    #[test]
    fn unopted_project_preview_only_runtime_bridge_required() {
        let dir = TempDir::new().unwrap();
        let out = OpenCodeRuntimeBridge::preview(dir.path(), false);
        match out {
            OpenCodeBridgeOutcome::RuntimeBridgeRequired {
                relative_path,
                source_hash,
            } => {
                assert_eq!(relative_path, OPENCODE_RUNTIME_BRIDGE_REL_PATH);
                assert_eq!(source_hash, OPENCODE_RUNTIME_BRIDGE_SOURCE_HASH);
            }
            other => panic!("unexpected {other:?}"),
        }
        // materialize 不得写盘
        let mat = OpenCodeRuntimeBridge::materialize(dir.path(), false).unwrap();
        assert!(matches!(
            mat,
            OpenCodeBridgeOutcome::RuntimeBridgeRequired { .. }
        ));
        assert!(!OpenCodeRuntimeBridge::absolute_path(dir.path()).exists());
    }

    /// Business Logic: opted-in materialize 后 verify 成功；碰撞不覆盖。
    #[test]
    fn opted_in_materialize_verify_and_collision() {
        let dir = TempDir::new().unwrap();
        let mat = OpenCodeRuntimeBridge::materialize(dir.path(), true).unwrap();
        assert!(matches!(mat, OpenCodeBridgeOutcome::Materialized { .. }));
        let v = OpenCodeRuntimeBridge::verify(dir.path(), true);
        assert!(matches!(v, OpenCodeBridgeOutcome::Verified { .. }));

        // 再次 materialize → Verified 无覆盖写
        let again = OpenCodeRuntimeBridge::materialize(dir.path(), true).unwrap();
        assert!(matches!(again, OpenCodeBridgeOutcome::Verified { .. }));

        // 外部不同字节
        let path = OpenCodeRuntimeBridge::absolute_path(dir.path());
        std::fs::write(&path, b"user-plugin-not-ours\n").unwrap();
        let coll = OpenCodeRuntimeBridge::materialize(dir.path(), true).unwrap();
        match coll {
            OpenCodeBridgeOutcome::ExternalCollision {
                relative_path,
                current_hash,
                ..
            } => {
                assert_eq!(relative_path, OPENCODE_RUNTIME_BRIDGE_REL_PATH);
                assert!(current_hash.is_some());
            }
            other => panic!("expected collision, got {other:?}"),
        }
        // 未覆盖
        assert_eq!(std::fs::read(&path).unwrap(), b"user-plugin-not-ours\n");
        let class = OpenCodeRuntimeBridge::classify_reserved_path(
            OPENCODE_RUNTIME_BRIDGE_REL_PATH,
            b"user-plugin-not-ours\n",
        );
        assert_eq!(class, Some(false));
        let class_ok = OpenCodeRuntimeBridge::classify_reserved_path(
            OPENCODE_RUNTIME_BRIDGE_REL_PATH,
            OpenCodeRuntimeBridge::generated_source().as_bytes(),
        );
        assert_eq!(class_ok, Some(true));
        assert_eq!(
            OpenCodeRuntimeBridge::classify_reserved_path("other.ts", b"x"),
            None
        );
    }

    /// Business Logic: wrong-terminal mutation 由 decoder 产出但 caller 应丢弃；帧含正确 id。
    #[test]
    fn osc_frame_includes_ids_and_is_stripped_from_visible() {
        let mut mapper = OpenCodeEventMapper::new("agent-x", "term-y");
        let f = mapper
            .map_event(&OpenCodeOfficialEvent::SessionStatus {
                session_id: "native-1".into(),
                status: "busy".into(),
            })
            .unwrap();
        let mut dec = AgentOscDecoder::default();
        let mut input = b"hello".to_vec();
        input.extend_from_slice(&f.osc_bytes);
        input.extend_from_slice(b"world");
        let r = dec.push(&input);
        assert_eq!(r.visible, b"helloworld");
        assert_eq!(r.mutations[0].agent_session_id, "agent-x");
        assert_eq!(r.mutations[0].terminal_session_id, "term-y");
        // base64 payload 含 native + provider
        let prefix = b"\x1b]777;cc-partner-agent-v1;";
        assert!(f.osc_bytes.starts_with(prefix));
        let payload = &f.osc_bytes[prefix.len()..f.osc_bytes.len() - 2];
        let json = URL_SAFE_NO_PAD.decode(payload).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(v["providerId"], "openCodeVisible");
        assert_eq!(v["nativeSessionId"], "native-1");
        assert!(v.get("prompt").is_none());
        assert!(v.get("message").is_none());
    }
}
