/**
 * permissionEntries - 权限状态 → 展示条目的共享映射
 *
 * Business Logic（为什么需要这个模块）:
 *   Welcome 引导页与设置页的「权限管理」都需要把后端 PermissionsStatus 渲染成
 *   「图标 + 标题 + 说明 + 授权状态」的标准条目。把映射逻辑收敛到一处，避免两个页面
 *   各写一份重复的 screenCapture/inputMonitoring 构造（规则 9 复用）。
 *
 * Code Logic（这个模块做什么）:
 *   `mapPermissions(status, t)` 接收 welcome ns 的翻译函数，返回 PermissionEntry[]，
 *   顺序固定为屏幕录制 → 辅助功能 → 输入监控 → 通知。通知权限由前端 JS API 检测
 *   （lib/notification.ts），非 TCC；仅 macOS 引导，非 macOS 视为已授权。
 */

import type { ReactElement } from 'react';
import type { TFunction } from 'i18next';
import { BellIcon, HealthIcon, InfoIcon, KeyboardIcon } from '@/lib/icons';
import type { PermissionsStatus } from '@/lib/types';

/** 单条权限条目的展示格式（供 PermissionCard 渲染） */
export interface PermissionEntry {
  id: string;
  icon: ReactElement;
  title: string;
  description: string;
  granted: boolean;
}

/**
 * 将后端 PermissionsStatus 转换为 PermissionEntry 列表（四条）
 *
 * Business Logic（四条权限的真实消费者）:
 *   - 屏幕录制：区域截图（xcap 抓屏）
 *   - 辅助功能：健康提醒读取前台活动窗口标题（active-win-pos-rs 走 AX API）
 *   - 输入监控：健康提醒键鼠活跃采样（device_query 走 IOHIDManager）
 *   - 通知：系统通知（健康提醒久坐/喝水），由 @tauri-apps/plugin-notification 发送，需用户授权
 *   全局快捷键基于 RegisterEventHotKey，无需任何 TCC 权限，故不在引导之列。
 *
 * @param status - 后端返回的权限状态
 * @param t - i18next 翻译函数（welcome ns，复用 permission.* 文案）
 * @returns 用于渲染的权限条目数组（屏幕录制 → 辅助功能 → 输入监控 → 通知）
 */
export function mapPermissions(
  status: PermissionsStatus,
  t: TFunction<'welcome'>,
): PermissionEntry[] {
  return [
    {
      id: 'screenCapture',
      icon: <InfoIcon />,
      title: t('permission.screenRecording.title'),
      description: t('permission.screenRecording.description'),
      granted: status.screenCapture.granted,
    },
    {
      id: 'accessibility',
      icon: <HealthIcon />,
      title: t('permission.accessibility.title'),
      description: t('permission.accessibility.description'),
      granted: status.accessibility.granted,
    },
    {
      id: 'inputMonitoring',
      icon: <KeyboardIcon />,
      title: t('permission.inputMonitoring.title'),
      description: t('permission.inputMonitoring.description'),
      granted: status.inputMonitoring.granted,
    },
    {
      id: 'notification',
      icon: <BellIcon />,
      title: t('permission.notification.title'),
      description: t('permission.notification.description'),
      granted: status.notification.granted,
    },
  ];
}
