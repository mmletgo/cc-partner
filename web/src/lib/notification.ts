/**
 * notification - 通知权限检测/请求/运营通知发送
 *
 * Business Logic（为什么需要这个模块）:
 *   cc-partner 通过系统通知推送健康提醒与 Orchestrator 运营状态变更。
 *   macOS 通知权限按应用 Bundle 身份记账：Dev（com.cc-partner.app.dev）与
 *   Release（com.cc-partner.app）必须各自授权，互不继承。
 *   tauri-plugin-notification 桌面 stub 恒返回 Granted，**不得**作为权限权威源。
 *   权威源是 Rust `check_permissions` / `request_permission("notification")`
 *   （UNUserNotificationCenter）。
 *
 * Code Logic（这个模块做什么）:
 *   - checkNotificationGranted(): 非 macOS true；macOS 调 check_permissions().notification
 *   - requestNotificationPermission(): 非 macOS no-op；macOS request_permission notification
 *   - sendOperationalNotification: 仍用 plugin 发 toast（发送路径与权限探测分离）
 *   权限探测/请求 **fail-closed**（失败视为未授权），因通知为 Welcome 必选项。
 */

import {
  sendNotification,
} from '@tauri-apps/plugin-notification';
import { configApi } from '@/api/config';
import { isMacos } from './platform';

/**
 * 查询通知授权状态
 *
 * Business Logic: usePermissions 轮询时合并进统一权限视图；也可被运营通知路径复用。
 * Code Logic: 非 macOS 返回 true；macOS 经 check_permissions 读 notification.granted；
 *   异常 fail-closed 返回 false（通知为引导必选项，不得假绿）。
 *
 * @returns 是否已授权发送通知
 */
export async function checkNotificationGranted(): Promise<boolean> {
  if (!isMacos()) return true;
  try {
    const status = await configApi.permissions();
    return status.notification.granted;
  } catch {
    return false;
  }
}

/**
 * 请求通知权限
 *
 * Business Logic: 用户在 Welcome/设置页点「去设置」时触发系统通知授权框（及可选设置面板）。
 * Code Logic: 非 macOS no-op；macOS 调 request_permission('notification')。
 */
export async function requestNotificationPermission(): Promise<void> {
  if (!isMacos()) return;
  try {
    await configApi.requestPermission('notification', true);
  } catch {
    // 请求失败静默，轮询反映真实状态
  }
}

/**
 * 发送运营状态系统通知（仅 title/body）。
 *
 * Business Logic（为什么需要这个函数）:
 *   Orchestrator Human Review / Blocked / outbox failed / Done 需要可选系统提醒；
 *   禁止 actionType/onAction/extra，避免锁屏泄漏任务内容。
 *
 * Code Logic（这个函数做什么）:
 *   只调用 `sendNotification({ title, body })`。
 */
export function sendOperationalNotification(options: {
  title: string;
  body: string;
}): void {
  sendNotification({ title: options.title, body: options.body });
}
