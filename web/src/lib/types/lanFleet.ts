/**
 * LAN Agent Fleet 前端类型（与 Rust DTO camelCase 对齐）。
 *
 * Business Logic（为什么需要这个模块）:
 *   Fleet 视图与 Project Rail 需要跨设备只读摘要；不得包含 Prompt、terminal bytes 或远端绝对 path。
 *
 * Code Logic（这个模块做什么）:
 *   定义 reachability/freshness/git/browser 与 snapshot 结构。
 */

import type { AgentLedgerSummary, FleetAgentActivityStatus } from './agentLedger';

/** 设备可达性（协议/网络，非认证）。 */
export type FleetReachability = 'live' | 'offline' | 'unsupported';

/** 数据新鲜度。 */
export type FleetFreshness = 'live' | 'cached' | 'unknown';

/** Git 摘要。 */
export type FleetGitState = 'clean' | 'dirty' | 'conflict' | 'unknown';

/** Browser preview 摘要。 */
export type FleetBrowserState = 'active' | 'absent' | 'unknown';

/**
 * Agent phase 计数。
 *
 * Business Logic（为什么需要这个类型）:
 *   Rail 只对 needsInput/failed 打 badge；working 不形成红标。
 */
export interface AgentPhaseCounts {
  launching: number;
  working: number;
  needsInput: number;
  idle: number;
  completed: number;
  failed: number;
  disconnected: number;
}

/**
 * 单项目 Fleet 摘要。
 */
export interface LanFleetProjectSummary {
  projectId: string;
  displayName: string;
  projectKind: string;
  agentCounts: AgentPhaseCounts;
  attentionCount: number;
  terminalCount: number;
  gitState: FleetGitState;
  browserState: FleetBrowserState;
  orchestratorRunning: number;
  orchestratorRetrying: number;
  lastActivityAt: string | null;
  /** 7d ledger join 状态；unsupported/unavailable 不得显示 usage=0 */
  agentActivityStatus?: FleetAgentActivityStatus;
  /** 仅 live 时有值 */
  agentActivity?: AgentLedgerSummary | null;
}

/**
 * 单设备 Fleet 摘要。
 */
export interface LanFleetDeviceSummary {
  deviceId: string;
  deviceName: string;
  reachability: FleetReachability;
  freshness: FleetFreshness;
  schedulerSlotsUsed: number | null;
  schedulerSlotsMax: number | null;
  projects: LanFleetProjectSummary[];
  errorCode: string | null;
  capturedAt: string | null;
}

/**
 * 控制设备全局 Fleet 快照。
 */
export interface LanFleetSnapshot {
  generatedAt: string;
  devices: LanFleetDeviceSummary[];
  truncated: boolean;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Rail 异常 badge 只需 needsInput+failed。
 *
 * Code Logic（这个函数做什么）:
 *   饱和加总。
 */
export function fleetExceptionCount(counts: AgentPhaseCounts): number {
  return Math.max(0, counts.needsInput) + Math.max(0, counts.failed);
}
