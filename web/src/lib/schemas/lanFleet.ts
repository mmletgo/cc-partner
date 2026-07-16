/**
 * LAN Agent Fleet 运行时 schema。
 *
 * Business Logic（为什么需要这个模块）:
 *   IPC/HTTP 可能损坏或混合版本；写入 hook 前 fail-closed 拒绝负计数/非法 enum，
 *   但保留 field-level unknown（git/browser）。
 *
 * Code Logic（这个模块做什么）:
 *   严格 decoder：非负整数、合法 enum、嵌套 devices/projects。
 */

import type {
  AgentPhaseCounts,
  FleetBrowserState,
  FleetFreshness,
  FleetGitState,
  FleetReachability,
  LanFleetDeviceSummary,
  LanFleetProjectSummary,
  LanFleetSnapshot,
} from '../types/lanFleet';
import type { FleetAgentActivityStatus } from '../types/agentLedger';
import { agentLedgerSummaryDecoder } from './agentLedger';
import {
  arrayDecoder,
  booleanDecoder,
  ContractDecodeError,
  defineDecoder,
  enumDecoder,
  nullableDecoder,
  numberDecoder,
  objectDecoder,
  optionalDecoder,
  stringDecoder,
  type Decoder,
} from '../runtimeSchema';

/**
 * Business Logic（为什么需要这个函数）:
 *   计数不得为负，否则 badge 与 slots 展示错误。
 *
 * Code Logic（这个函数做什么）:
 *   有限非负整数。
 */
export const nonNegativeIntDecoder: Decoder<number> = defineDecoder(
  'NonNegativeInt',
  (value, path = '$') => {
    const n = numberDecoder.decode(value, path);
    if (!Number.isFinite(n) || n < 0 || !Number.isInteger(n)) {
      throw new ContractDecodeError('NonNegativeInt', path, 'primitive');
    }
    return n;
  },
);

export const fleetReachabilityDecoder: Decoder<FleetReachability> = enumDecoder(
  'FleetReachability',
  ['live', 'offline', 'unsupported'] as const,
);

export const fleetFreshnessDecoder: Decoder<FleetFreshness> = enumDecoder('FleetFreshness', [
  'live',
  'cached',
  'unknown',
] as const);

export const fleetGitStateDecoder: Decoder<FleetGitState> = enumDecoder('FleetGitState', [
  'clean',
  'dirty',
  'conflict',
  'unknown',
] as const);

export const fleetBrowserStateDecoder: Decoder<FleetBrowserState> = enumDecoder(
  'FleetBrowserState',
  ['active', 'absent', 'unknown'] as const,
);

export const agentPhaseCountsDecoder: Decoder<AgentPhaseCounts> = objectDecoder(
  'AgentPhaseCounts',
  {
    launching: nonNegativeIntDecoder,
    working: nonNegativeIntDecoder,
    needsInput: nonNegativeIntDecoder,
    idle: nonNegativeIntDecoder,
    completed: nonNegativeIntDecoder,
    failed: nonNegativeIntDecoder,
    disconnected: nonNegativeIntDecoder,
  },
);

export const fleetAgentActivityStatusDecoder: Decoder<FleetAgentActivityStatus> = enumDecoder(
  'FleetAgentActivityStatus',
  ['live', 'unsupported', 'unavailable'] as const,
);

export const lanFleetProjectSummaryDecoder: Decoder<LanFleetProjectSummary> = objectDecoder(
  'LanFleetProjectSummary',
  {
    projectId: stringDecoder,
    displayName: stringDecoder,
    projectKind: stringDecoder,
    agentCounts: agentPhaseCountsDecoder,
    attentionCount: nonNegativeIntDecoder,
    terminalCount: nonNegativeIntDecoder,
    gitState: fleetGitStateDecoder,
    browserState: fleetBrowserStateDecoder,
    orchestratorRunning: nonNegativeIntDecoder,
    orchestratorRetrying: nonNegativeIntDecoder,
    lastActivityAt: nullableDecoder(stringDecoder),
    agentActivityStatus: optionalDecoder(fleetAgentActivityStatusDecoder),
    agentActivity: optionalDecoder(nullableDecoder(agentLedgerSummaryDecoder)),
  },
);

export const lanFleetDeviceSummaryDecoder: Decoder<LanFleetDeviceSummary> = objectDecoder(
  'LanFleetDeviceSummary',
  {
    deviceId: stringDecoder,
    deviceName: stringDecoder,
    reachability: fleetReachabilityDecoder,
    freshness: fleetFreshnessDecoder,
    schedulerSlotsUsed: nullableDecoder(nonNegativeIntDecoder),
    schedulerSlotsMax: nullableDecoder(nonNegativeIntDecoder),
    projects: arrayDecoder(lanFleetProjectSummaryDecoder),
    errorCode: nullableDecoder(stringDecoder),
    capturedAt: nullableDecoder(stringDecoder),
  },
);

export const lanFleetSnapshotDecoder: Decoder<LanFleetSnapshot> = objectDecoder(
  'LanFleetSnapshot',
  {
    generatedAt: stringDecoder,
    devices: arrayDecoder(lanFleetDeviceSummaryDecoder),
    truncated: booleanDecoder,
  },
);
