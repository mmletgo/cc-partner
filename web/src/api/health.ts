/**
 * Health API - 通过 Tauri invoke 调用 Rust 后端健康提醒命令
 *
 * Business Logic（为什么需要这个模块）:
 *   久坐监测 / 工作休息状态机 / 系统通知 + toast + 全屏提醒 / 喝水提醒 /
 *   活动统计图表需要前端读写：开关监测、手动暂停/贪睡/跳过、读取当前状态与
 *   今日活跃统计 + app 排行/小时分布明细、记录喝水、整体配置回写。
 *   本模块封装这 11 个 invoke 调用，供 Health 页 / toast / 全屏遮罩消费。
 *
 * Code Logic（这个模块做什么）:
 *   基于 invoke 封装 11 个命令，返回类型化的 Promise，参数字段 camelCase
 *   对齐 Rust #[tauri::command] 签名。
 */

import { invoke } from './client';
import type { HealthConfig, HealthStatus, ActivityStats, ActivityDetail } from '@/lib/types';

export const healthApi = {
  /** 读取当前健康提醒状态（相位 / 暂停 / 贪睡到期 / 配置阈值） */
  getStatus: () => invoke<HealthStatus>('get_health_status'),

  /** 读取完整健康配置（全部字段，供设置表单初始化，避免 updateConfig 部分字段清零） */
  getConfig: () => invoke<HealthConfig>('get_health_config'),

  /** 读取健康提醒默认配置(设置页「恢复默认」用,对齐同步/AI 的 getDefault 模式) */
  getDefaultConfig: () => invoke<HealthConfig>('get_default_health_config'),

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

  /** 跳过本次喝水提醒（重置喝水计时） */
  skipWater: () => invoke<void>('skip_water_reminder'),

  /** 延迟本次喝水提醒 N 分钟 */
  snoozeWater: (minutes: number) =>
    invoke<void>('snooze_water_reminder', { minutes }),

  /** 整体覆盖 config.health（工作窗口/休息/喝水间隔/通知/记录标题/免打扰/保留天数；固定启用字段由后端归一） */
  updateConfig: (config: HealthConfig) =>
    invoke<HealthConfig>('update_health_config', { config }),

  /** 读取自 sinceTs 以来的活跃/闲置分钟数统计 */
  getStats: (sinceTs: number) =>
    invoke<ActivityStats>('get_activity_stats', { sinceTs }),

  /** 读取自 sinceTs 以来的活动明细(app 排行 + 24 小时分布) */
  getDetail: (sinceTs: number) =>
    invoke<ActivityDetail>('get_activity_detail', { sinceTs }),

  /** 关闭全部健康提醒全屏遮罩窗口(每屏一个透明置顶窗口) */
  closeOverlay: () => invoke<void>('close_health_overlay'),
};
