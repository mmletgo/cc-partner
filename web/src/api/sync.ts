/**
 * 局域网同步与可验证备份 API
 *
 * Business Logic（为什么需要这个模块）:
 *   Settings 同步 tab 需要展示 per-device/domain 收敛结果，并提供可验证导出/恢复；
 *   恢复前自动 pre-restore，失败任务可按 job 回滚。
 *
 * Code Logic（这个模块做什么）:
 *   - syncApi.trigger → invoke('trigger_sync')，导出 SyncRunResult 与 success helpers
 *   - backupApi 封装 create/inspect/restore/listJobs/listBackups/rollback（camelCase DTO）
 *   - pickBackupExportPath / pickBackupArchivePath 走 plugin-dialog 选路径
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
   * Business Logic: Settings 触发 Prompt 与速记本局域网同步。
   * Code Logic: invoke trigger_sync。
   */
  trigger: () => invoke<SyncRunResult>('trigger_sync'),
};

// ─── Verified backup / restore (N2) ─────────────────────────────────────────

/** create_backup 成功结果 */
export interface BackupCreateResult {
  path: string;
  formatVersion: number;
}

/** inspect_backup 预览 */
export interface BackupInspectPreview {
  formatVersion: number;
  domainCounts: Record<string, number>;
  warnings: string[];
  conflictsEstimate: number;
}

/** 恢复模式：合并 / 替换所选领域 */
export type RestoreMode = 'merge' | 'replaceDomain';

/** restore_backup / rollback_recovery_job 结果 */
export interface BackupRestoreResult {
  jobId: string;
  status: string;
  appliedDomains: string[];
  preRestoreBackupPath?: string | null;
  errorSummary?: string | null;
}

/** 恢复任务行（list_recovery_jobs） */
export interface RecoveryJobRow {
  id: string;
  status: string;
  archivePath?: string | null;
  preRestoreBackupPath?: string | null;
  selectedDomainsJson: string;
  mode: string;
  errorSummary?: string | null;
  createdAt: string;
  updatedAt: string;
}

/** pre-restore 备份列表项 */
export interface PreRestoreBackupInfo {
  path: string;
  createdAt?: string | null;
}

/**
 * 可恢复领域 token（configReport 仅导出，不可勾选恢复）。
 *
 * Business Logic: 恢复 UI 只允许这些领域。
 * Code Logic: const 元组 + 派生类型。
 */
export const BACKUP_RESTORE_DOMAINS = [
  'prompts',
  'ccHistory',
  'scratchpad',
  'deletionFloors',
] as const;

/** 仅供旧备份包显式恢复的退役领域；新包不得导出。 */
export const LEGACY_BACKUP_RESTORE_DOMAINS = ['sshTargets', 'claudeMd'] as const;

export type BackupRestoreDomain =
  | (typeof BACKUP_RESTORE_DOMAINS)[number]
  | (typeof LEGACY_BACKUP_RESTORE_DOMAINS)[number];

/**
 * 返回当前备份预览可展示的恢复领域。
 *
 * Business Logic（为什么需要）:
 * 新备份不再包含 SSH/CLAUDE.md，但用户仍需能从明确包含这些领域的旧包中选择恢复。
 *
 * Code Logic（做什么）:
 * 始终返回当前产品领域；仅当 inspect 的 domainCounts 明确列出退役领域时追加兼容选项。
 */
export function getBackupRestoreDomains(
  domainCounts: Record<string, number>,
): BackupRestoreDomain[] {
  return [
    ...BACKUP_RESTORE_DOMAINS,
    ...LEGACY_BACKUP_RESTORE_DOMAINS.filter((domain) =>
      Object.prototype.hasOwnProperty.call(domainCounts, domain),
    ),
  ];
}

export const backupApi = {
  /**
   * 导出可验证备份包到指定路径。
   *
   * Business Logic: Settings 导出按钮保存 zip。
   * Code Logic: invoke create_backup { destPath }。
   */
  create: (destPath: string) =>
    invoke<BackupCreateResult>('create_backup', { destPath }),

  /**
   * 解析备份包预览（版本/领域计数/警告/冲突估计）。
   *
   * Business Logic: 恢复前让用户确认领域与风险。
   * Code Logic: invoke inspect_backup { archivePath }。
   */
  inspect: (archivePath: string) =>
    invoke<BackupInspectPreview>('inspect_backup', { archivePath }),

  /**
   * 按模式恢复勾选领域。
   *
   * Business Logic: merge 合并；replaceDomain 先清领域再导入；后端写 pre-restore。
   * Code Logic: invoke restore_backup { archivePath, mode, domains }。
   */
  restore: (archivePath: string, mode: RestoreMode, domains: string[]) =>
    invoke<BackupRestoreResult>('restore_backup', {
      archivePath,
      mode,
      domains,
    }),

  /**
   * 列出最近恢复任务。
   *
   * Business Logic: 任务列表支持回滚。
   * Code Logic: invoke list_recovery_jobs { limit? }。
   */
  listJobs: (limit?: number) =>
    invoke<RecoveryJobRow[]>('list_recovery_jobs', { limit }),

  /**
   * 列出 pre-restore 备份。
   *
   * Business Logic: 诊断/运维可查看自动备份。
   * Code Logic: invoke list_pre_restore_backups。
   */
  listBackups: () => invoke<PreRestoreBackupInfo[]>('list_pre_restore_backups'),

  /**
   * 用任务的 pre-restore 备份回滚。
   *
   * Business Logic: 恢复失败或后悔时一键回滚。
   * Code Logic: invoke rollback_recovery_job { jobId }。
   */
  rollback: (jobId: string) =>
    invoke<BackupRestoreResult>('rollback_recovery_job', { jobId }),
};

/**
 * 选择导出备份保存路径。
 *
 * Business Logic（为什么需要这个函数）:
 *   用户导出可验证备份时需选定本机 zip 落盘路径。
 *
 * Code Logic（这个函数做什么）:
 *   动态 import plugin-dialog 的 save；defaultPath=cc-partner-export.zip；
 *   filters 优先 zip；返回 string 路径或 null（取消）。
 */
export async function pickBackupExportPath(): Promise<string | null> {
  const { save } = await import('@tauri-apps/plugin-dialog');
  const selected = await save({
    defaultPath: 'cc-partner-export.zip',
    filters: [{ name: 'ZIP', extensions: ['zip'] }],
  });
  if (typeof selected === 'string' && selected.length > 0) {
    return selected;
  }
  return null;
}

/**
 * 选择待恢复的备份 zip 路径。
 *
 * Business Logic（为什么需要这个函数）:
 *   恢复流程先选本地备份归档再 inspect。
 *
 * Code Logic（这个函数做什么）:
 *   动态 import plugin-dialog 的 open；multiple/directory=false；filters zip；
 *   返回 string 路径或 null（取消/非 string）。
 */
export async function pickBackupArchivePath(): Promise<string | null> {
  const { open } = await import('@tauri-apps/plugin-dialog');
  const selected = await open({
    multiple: false,
    directory: false,
    filters: [{ name: 'ZIP', extensions: ['zip'] }],
  });
  if (typeof selected === 'string' && selected.length > 0) {
    return selected;
  }
  return null;
}
