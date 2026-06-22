/**
 * Health API - 通过 Tauri invoke 调用 Rust 后端健康提醒命令
 *
 * Business Logic（为什么需要这个模块）:
 *   久坐监测 / 工作休息状态机 / 系统通知提醒需要前端读写：开关监测、
 *   手动暂停/贪睡/跳过、读取当前状态与今日活跃统计、整体配置回写。
 *   本模块封装这 8 个 invoke 调用，供 Health 页与 toast 消费。
 *
 * Code Logic（这个模块做什么）:
 *   基于 invoke 封装 8 个命令，返回类型化的 Promise，参数字段 camelCase
 *   对齐 Rust #[tauri::command] 签名。
 */

import { invoke } from './client';
import type { HealthConfig, HealthStatus, ActivityStats, ActivityDetail } from '@/lib/types';

export const healthApi = {
  /** 读取当前健康提醒状态（相位 / 暂停 / 贪睡到期 / 配置阈值） */
  getStatus: () => invoke<HealthStatus>('get_health_status'),

  /** 开启/关闭久坐监测（落盘 config.health.enabled） */
  toggleEnabled: (enabled: boolean) =>
    invoke<HealthConfig>('toggle_health_enabled', { enabled }),

  /** 暂停/恢复监测（仅内存标记，重启失效） */
  togglePaused: (paused: boolean) =>
    invoke<void>('toggle_health_paused', { paused }),

  /** 贪睡提醒 N 分钟 */
  snooze: (minutes: number) =>
    invoke<void>('snooze_reminder', { minutes }),

  /** 跳过本次提醒（重置状态机回 Idle + 清贪睡） */
  skip: () => invoke<void>('skip_reminder'),

  /** 记录一次喝水（health:water 提醒 toast 的「已喝水」按钮调用） */
  recordWater: () => invoke<void>('record_water'),

  /** 整体覆盖 config.health（工作窗口/休息/通知/记录标题/免打扰/保留天数） */
  updateConfig: (config: HealthConfig) =>
    invoke<HealthConfig>('update_health_config', { config }),

  /** 读取自 sinceTs 以来的活跃/闲置分钟数统计 */
  getStats: (sinceTs: number) =>
    invoke<ActivityStats>('get_activity_stats', { sinceTs }),

  /** 读取自 sinceTs 以来的活动明细(app 排行 + 24 小时分布) */
  getDetail: (sinceTs: number) =>
    invoke<ActivityDetail>('get_activity_detail', { sinceTs }),
};
