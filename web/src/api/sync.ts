/**
 * 局域网同步 API - 触发 per-device/domain 收敛真值同步
 *
 * Business Logic（为什么需要这个模块）:
 *   Settings 同步 tab 需要展示每台设备/每个领域的 succeeded/partial/unreachable 等状态，
 *   不能再把 partial 显示成成功。本模块封装 trigger_sync 返回的 SyncRunResult。
 *
 * Code Logic（这个模块做什么）:
 *   invoke('trigger_sync') 并导出与 Rust SyncRunResult / SyncDomainOutcome 对齐的类型。
 */

import { invoke } from './client';

/** 设备级同步状态（与 Rust DeviceSyncStatus snake_case 对齐） */
export type DeviceSyncStatus =
  | 'succeeded'
  | 'partial'
  | 'unreachable'
  | 'protocol_error'
  | 'resource_limit';

/** 传输失败分类 */
export type TransportClass = 'network' | 'timeout' | 'http';

/** 单领域 typed outcome（tag = kind） */
export type SyncDomainOutcome =
  | { kind: 'succeeded'; pulled: number; pushed: number; unchanged: number }
  | { kind: 'partial'; applied: number; failed: Array<{ id: string; code: string; message: string }> }
  | { kind: 'unreachable'; class: TransportClass }
  | { kind: 'protocol_error'; code: string }
  | { kind: 'resource_limit'; limit: string };

/** 单领域报告 */
export interface DomainSyncReport {
  domain: string;
  outcome: SyncDomainOutcome;
}

/** 单设备报告 */
export interface DeviceSyncReport {
  device_id: string;
  device_name: string;
  status: DeviceSyncStatus;
  domains: DomainSyncReport[];
}

/**
 * 一轮局域网同步结果。
 *
 * Business Logic: succeeded_devices / synced 只计全成功设备。
 * Code Logic: 与 Rust SyncRunResult 字段一致（snake_case）。
 */
export interface SyncRunResult {
  accepted: boolean;
  succeeded_devices: number;
  /** 兼容字段，= succeeded_devices */
  synced: number;
  devices: DeviceSyncReport[];
  note: string;
}

/**
 * 判断领域 outcome 是否全成功。
 *
 * Business Logic: UI 不得把 partial/unreachable 显示为成功色。
 * Code Logic: kind === 'succeeded'。
 */
export function isDomainSucceeded(outcome: SyncDomainOutcome): boolean {
  return outcome.kind === 'succeeded';
}

/**
 * 判断设备是否全成功。
 *
 * Business Logic: 设备级成功 pill 只在 status=succeeded 时使用。
 * Code Logic: status === 'succeeded'。
 */
export function isDeviceSucceeded(status: DeviceSyncStatus): boolean {
  return status === 'succeeded';
}

/**
 * 从 succeeded outcome 取 pulled/pushed/unchanged，其它返回 null。
 *
 * Business Logic: Settings 仅在成功时展示三计数。
 * Code Logic: narrow kind。
 */
export function succeededCounts(
  outcome: SyncDomainOutcome,
): { pulled: number; pushed: number; unchanged: number } | null {
  if (outcome.kind !== 'succeeded') return null;
  return {
    pulled: outcome.pulled,
    pushed: outcome.pushed,
    unchanged: outcome.unchanged,
  };
}

export const syncApi = {
  /**
   * 触发局域网全领域同步，返回 per-device/domain 真值。
   *
   * Business Logic: Settings / Prompt / SSH / Scratchpad 共用。
   * Code Logic: invoke trigger_sync。
   */
  trigger: () => invoke<SyncRunResult>('trigger_sync'),
};
