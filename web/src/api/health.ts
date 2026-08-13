/**
 * Health API - 通过 Tauri invoke 调用 Rust 后端健康提醒命令
 *
 * Business Logic（为什么需要这个模块）:
 *   健康监测 / 可配置提醒模板 / 系统通知 / 全屏遮罩 / 活动统计需要前端读写：
 *   开关监测、按模板确认/跳过/贪睡/开始会话、读取状态与今日统计、整体配置回写、
 *   近 N 天习惯统计、手动 +1 / 删除饮水记录。
 *
 * Code Logic（这个模块做什么）:
 *   基于 invoke 封装命令，返回类型化 Promise，参数字段 camelCase
 *   对齐 Rust #[tauri::command] 签名。
 */

import { invoke } from './client';
import type { HealthConfig, HealthStatus, ActivityStats, ActivityDetail, HabitStats } from '@/lib/types';

export type HealthReminderAction = 'completed' | 'skipped' | 'snoozed';

export interface HealthSessionStart {
  endTs: number;
  templateId: string;
}

export const healthApi = {
  /** 读取当前健康提醒状态（相位 / 暂停 / 贪睡到期 / 遮罩队列 / 配置阈值） */
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

  /** 贪睡 rest 模板 N 分钟（旧命令包装） */
  snooze: (minutes: number) =>
    invoke<void>('snooze_reminder', { minutes }),

  /** 跳过 rest 模板本次提醒（只处理 rest，不再重置整机） */
  skip: () => invoke<void>('skip_reminder'),

  /** 记录一次喝水（water/completed 包装） */
  recordWater: () => invoke<void>('record_water'),

  /** 跳过本次喝水提醒 */
  skipWater: () => invoke<void>('skip_water_reminder'),

  /** 延迟本次喝水提醒 N 分钟 */
  snoozeWater: (minutes: number) =>
    invoke<void>('snooze_water_reminder', { minutes }),

  /**
   * Business Logic（为什么需要这个函数）:
   *   遮罩与统计卡都按 templateId 确认/跳过/贪睡，不能再写死饮水/休息两套命令。
   *
   * Code Logic（这个函数做什么）:
   *   invoke acknowledge_health_reminder；snoozed 时带 minutes。
   */
  acknowledge: (templateId: string, action: HealthReminderAction, minutes?: number) =>
    invoke<void>('acknowledge_health_reminder', { templateId, action, minutes }),

  /**
   * Business Logic（为什么需要这个函数）:
   *   休息/提肛/自定义 session 共用权威倒计时。
   *
   * Code Logic（这个函数做什么）:
   *   invoke start_health_session，返回 {endTs, templateId}。
   */
  startSession: (templateId: string) =>
    invoke<HealthSessionStart>('start_health_session', { templateId }),

  /**
   * Business Logic（为什么需要这个函数）:
   *   instant 模板（饮水/自定义打卡）允许手动 +1。
   *
   * Code Logic（这个函数做什么）:
   *   invoke add_habit_manual。
   */
  addHabitManual: (templateId: string) =>
    invoke<number>('add_habit_manual', { templateId }),

  /** 整体覆盖 config.health（含 reminders；固定启用字段由后端归一） */
  updateConfig: (config: HealthConfig) =>
    invoke<HealthConfig>('update_health_config', { config }),

  /** 读取自 sinceTs 以来的活跃/闲置分钟数统计 */
  getStats: (sinceTs: number) =>
    invoke<ActivityStats>('get_activity_stats', { sinceTs }),

  /** 读取自 sinceTs 以来的活动明细(app 排行 + 窗口标题排行 + 24 小时分布) */
  getDetail: (sinceTs: number) =>
    invoke<ActivityDetail>('get_activity_detail', { sinceTs }),

  /** 关闭当前健康提醒全屏遮罩(有队列则弹出下一项) */
  closeOverlay: () => invoke<void>('close_health_overlay'),

  /** 启动 rest 模板全屏倒计时(后端权威 endTs,多屏同步) */
  startRest: () => invoke<HealthSessionStart>('start_health_rest'),

  /** 读取近 N 天习惯统计(按模板聚合 + 饮水/休息兼容字段) */
  getHabitStats: (days?: number) => invoke<HabitStats>('get_habit_stats', { days }),

  /** 手动加计一次饮水(HabitStatsCard 饮水 +1) */
  addWaterManual: () => invoke<number>('add_water_manual'),

  /** 删除指定 id 的饮水记录(历史记录删除 UI,P1 增量) */
  deleteWaterRecord: (id: number) => invoke<boolean>('delete_water_record', { id }),

  /** 记录一次休息完成(兼容旧遮罩「已完成」) */
  recordRestCompleted: () => invoke<void>('record_rest_completed'),
};
