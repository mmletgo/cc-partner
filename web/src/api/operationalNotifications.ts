/**
 * Operational notification snapshot API。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面端系统通知协调器需要从 N1 owner 拉取隐私安全的当前状态 baseline，
 *   以在冷启动/gap 后不重发旧通知；只能通过注册的 Tauri snapshot 命令访问，
 *   不得绕过控制面读本地 Orchestrator 仓库。
 *
 * Code Logic（这个模块做什么）:
 *   唯一封装 `get_operational_notification_snapshot` invoke，返回类型化 DTO。
 */

import { invoke } from './client';

/** 运营通知 kind（与后端 OperationalNotificationKind 对齐）。 */
export type OperationalNotificationKind =
  | 'humanReview'
  | 'blocked'
  | 'remoteOutboxFailed'
  | 'taskDone'
  | 'agentNeedsInput'
  | 'agentFailed'
  | 'experimentDecision';

/**
 * 单条运营通知事件（隐私安全：无任务标题/项目/goal）。
 *
 * Business Logic（为什么需要这个类型）:
 *   Tauri live event 与 snapshot items 共用同构字段，便于 dedupe 与 handshake。
 *
 * Code Logic（字段说明）:
 *   kind/opaqueSourceId/stateVersion 组成 dedupe key；relay 信封含 ownerInstanceId/sequence。
 */
export interface OperationalNotificationEvent {
  kind: OperationalNotificationKind;
  opaqueSourceId: string;
  stateVersion: number;
  occurredAt: string;
  ownerInstanceId?: string;
  sequence?: number;
}

/**
 * 运营通知 snapshot（owner baseline）。
 *
 * Business Logic（为什么需要这个类型）:
 *   冷启动与 gap 重连时建立 no-notify baseline，并提供 asOfCursor 丢弃过时缓冲事件。
 *
 * Code Logic（字段说明）:
 *   asOfCursor 是事件游标；items 最多约 1000 条；truncated 表示服务端是否截断。
 */
export interface OperationalNotificationSnapshot {
  asOfCursor: { ownerInstanceId: string; sequence: number };
  items: OperationalNotificationEvent[];
  truncated: boolean;
}

/** 桌面 snapshot 命令名（对齐 Rust #[tauri::command]）。 */
export const OPERATIONAL_NOTIFICATION_SNAPSHOT_COMMAND =
  'get_operational_notification_snapshot' as const;

/** live 运营通知 Tauri event 名。 */
export const OPERATIONAL_NOTIFICATION_EVENT = 'operational:notification' as const;

/** N1 runtime gap Tauri event 名（owner 切换/ring gap 时重做 handshake）。 */
export const BACKEND_RUNTIME_GAP_EVENT = 'backend:runtime-gap' as const;

/**
 * 拉取运营通知 snapshot baseline。
 *
 * Business Logic（为什么需要这个函数）:
 *   coordinator 在 listener 注册后必须用 owner snapshot 建立 no-notify 基线，
 *   才能安全消费之后的 live/buffered 事件而不刷屏。
 *
 * Code Logic（这个函数做什么）:
 *   invoke `get_operational_notification_snapshot`，无参数，返回类型化 snapshot。
 */
function getOperationalNotificationSnapshot(): Promise<OperationalNotificationSnapshot> {
  return invoke<OperationalNotificationSnapshot>(OPERATIONAL_NOTIFICATION_SNAPSHOT_COMMAND);
}

export const operationalNotificationsApi = {
  getSnapshot: getOperationalNotificationSnapshot,
};
