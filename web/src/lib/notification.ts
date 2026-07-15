/**
 * notification - 通知权限检测/请求/运营通知发送（@tauri-apps/plugin-notification 封装）
 *
 * Business Logic（为什么需要这个模块）:
 *   cc-partner 通过系统通知推送健康提醒（久坐/喝水）与 Orchestrator 运营状态变更。
 *   macOS 需用户授权「通知」权限；欢迎页/设置页的第 4 个权限引导需检测与请求它。
 *   通知权限不属于 TCC（不走 Rust FFI），由 tauri-plugin-notification 的 JS API 管理。
 *   运营通知必须仅发送 title/body，禁止 action/extra，避免锁屏泄漏与未认证点击动作。
 *
 * Code Logic（这个模块做什么）:
 *   - checkNotificationGranted(): macOS 调 isPermissionGranted()，非 macOS 视为已授权
 *   - requestNotificationPermission(): macOS 调 requestPermission()，非 macOS no-op
 *   - sendOperationalNotification({title,body}): 仅转发 sendNotification({title,body})
 *   权限探测/请求失败均保守降级（视为已授权 / 静默），不阻断主流程（通知是可选功能）。
 */

import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from '@tauri-apps/plugin-notification';
import { isMacos } from './platform';

/**
 * 查询通知授权状态
 *
 * Business Logic: usePermissions 轮询时合并进统一权限视图，供权限卡片显示授权状态。
 * Code Logic: 非 macOS 一律返回 true（不引导）；macOS 调 isPermissionGranted()，
 *   异常保守返回 true（探测失败不阻断）。
 *
 * @returns 是否已授权发送通知
 */
export async function checkNotificationGranted(): Promise<boolean> {
  if (!isMacos()) return true;
  try {
    return await isPermissionGranted();
  } catch {
    // 探测失败保守视为已授权，不阻断主流程
    return true;
  }
}

/**
 * 请求通知权限
 *
 * Business Logic: 用户在欢迎页/设置页点「去设置」时触发，弹系统通知授权框。
 * Code Logic: 非 macOS no-op；macOS 调 requestPermission()（返回 granted/denied/default）。
 *   授权状态由 usePermissions 轮询反映，此处不直接写 state（保持单一数据源）。
 */
export async function requestNotificationPermission(): Promise<void> {
  if (!isMacos()) return;
  try {
    await requestPermission();
  } catch {
    // 请求失败静默，轮询反映真实状态
  }
}

/**
 * 发送运营状态系统通知（仅 title/body）。
 *
 * Business Logic（为什么需要这个函数）:
 *   Orchestrator Human Review / Blocked / outbox failed / Done 需要可选系统提醒；
 *   当前桌面 notification plugin 不承诺点击回调，故禁止 actionType/onAction/extra，
 *   避免锁屏泄漏任务内容或暗示可从通知直接执行业务动作。
 *
 * Code Logic（这个函数做什么）:
 *   只调用 `sendNotification({ title, body })`，不附加任何 action/extra 字段。
 *
 * @param options.title - 隐私安全固定标题（i18n）
 * @param options.body - 隐私安全固定正文（i18n）
 */
export function sendOperationalNotification(options: {
  title: string;
  body: string;
}): void {
  sendNotification({ title: options.title, body: options.body });
}
