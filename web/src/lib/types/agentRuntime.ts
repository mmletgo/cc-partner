/**
 * Agent Runtime 投影域类型（A2 前端 projection 合同）。
 *
 * Business Logic（为什么需要这个模块）:
 *   Desktop/Mobile/Attention 需要统一的 Agent phase 投影模型，只能消费 A1 agent runtime
 *   真值，不得把 Orchestrator 旧 Claude 字段当作 Agent 状态。
 *
 * Code Logic（这个模块做什么）:
 *   导出 phase/freshness、单 session 投影、snapshot/event DTO 与前端聚合状态形状。
 */

/**
 * Agent session 生命周期 phase（provider-neutral）。
 *
 * Business Logic（为什么需要这个类型）:
 *   UI 与 Attention 只能依赖稳定 phase token，不能依赖厂商文案。
 *
 * Code Logic（这个类型做什么）:
 *   对齐 Rust AgentSessionPhase 的 camelCase 序列化字面量。
 */
export type AgentPhase =
  | 'launching'
  | 'working'
  | 'needsInput'
  | 'idle'
  | 'completed'
  | 'failed'
  | 'disconnected';

/**
 * Agent 投影新鲜度。
 *
 * Business Logic（为什么需要这个类型）:
 *   remote offline / 缓存 / 能力不支持必须显式区分，禁止把缺失伪装成 live。
 *
 * Code Logic（这个类型做什么）:
 *   live | cached | offline | unsupported。
 */
export type AgentFreshness = 'live' | 'cached' | 'offline' | 'unsupported';

/**
 * 单条 Agent session 投影（终端/列表 selector 消费）。
 *
 * Business Logic（为什么需要这个类型）:
 *   terminal tab 只需 provider/phase/version/关联 ID；禁止正文/路径/native session。
 *
 * Code Logic（字段说明）:
 *   对齐 A1 AgentSessionRuntimeDto 投影子集 + 前端 freshness；version 用于 CAS 乱序保护。
 */
export interface AgentSessionProjection {
  id: string;
  projectId: string;
  worktreeId?: string;
  terminalSessionId: string;
  taskId?: string;
  providerId: string;
  phase: AgentPhase;
  version: number;
  lastActivityAt: string;
  freshness: AgentFreshness;
  /** owner 内 agent 单调版本对应的 outcome（可选，仅终态）。 */
  outcomeCode?: string | null;
  isActive?: boolean;
}

/**
 * A1 Agent runtime 有界 snapshot（owner baseline）。
 *
 * Business Logic（为什么需要这个类型）:
 *   Gap/进入项目时用 ownerInstanceId + asOfSequence + sessions 建立 baseline。
 *
 * Code Logic（字段说明）:
 *   对齐 Rust AgentRuntimeSnapshot camelCase；sessions 最多 1000。
 */
export interface AgentRuntimeSnapshot {
  ownerInstanceId: string;
  asOfSequence: number;
  projectId?: string | null;
  sessions: AgentSessionRuntimeDto[];
  truncated: boolean;
}

/**
 * A1 Agent session 运行时 DTO（边界解码形状）。
 *
 * Business Logic（为什么需要这个类型）:
 *   snapshot/event 入站必须校验完整 DTO 后再映射为 projection。
 *
 * Code Logic（字段说明）:
 *   对齐 Rust AgentSessionRuntimeDto；不含 nativeSessionId。
 */
export interface AgentSessionRuntimeDto {
  id: string;
  projectId: string;
  worktreeId?: string | null;
  terminalSessionId: string;
  orchestratorTaskId?: string | null;
  orchestratorAttempt?: number | null;
  providerId: string;
  phase: AgentPhase;
  version: number;
  startedAt: string;
  lastActivityAt: string;
  endedAt?: string | null;
  outcomeCode?: string | null;
  resumedFromAgentSessionId?: string | null;
  isActive: boolean;
}

/**
 * Agent runtime 变更事件（Tauri/HTTP 增量）。
 *
 * Business Logic（为什么需要这个类型）:
 *   live 事件携带单 session 投影 + 可选 owner/sequence 供 handshake 缓冲排序。
 *
 * Code Logic（字段说明）:
 *   agentSession 为 DTO；ownerInstanceId/sequence 来自 relay 信封或上层注入。
 */
export interface AgentRuntimeEvent {
  agentSession: AgentSessionRuntimeDto;
  ownerInstanceId?: string;
  sequence?: number;
}

/**
 * 前端 Agent runtime 聚合状态（纯 reducer 真值）。
 *
 * Business Logic（为什么需要这个类型）:
 *   组件只读 selector，不直接 mutate Map；snapshot 与 event 共用同一结构。
 *
 * Code Logic（字段说明）:
 *   byAgentId 全量索引；latestAgentIdByTerminal 每 terminal 最新 agent id。
 */
export interface AgentRuntimeState {
  ownerInstanceId: string | null;
  asOfSequence: number;
  byAgentId: ReadonlyMap<string, AgentSessionProjection>;
  latestAgentIdByTerminal: ReadonlyMap<string, string>;
}
