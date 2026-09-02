/**
 * 电脑发给本机 `/mobile` 的虚拟目标与任务判定。
 *
 * Business Logic（为什么需要这个模块）:
 *   手机不是 P2P 设备；传输页要有稳定「手机」目标，下载按钮也要认出这类任务。
 *
 * Code Logic（这个模块做什么）:
 *   导出与 Rust `MOBILE_INBOX_DEVICE_ID` 相同的常量，以及 device/offer 判定。
 */

import type { Device, TransferTask } from '@/lib/types';

/** 与 `src-tauri/src/transfer/mod.rs` 的 `MOBILE_INBOX_DEVICE_ID` 对齐。 */
export const MOBILE_INBOX_DEVICE_ID = 'cc-partner-mobile-inbox';

/**
 * Business Logic（为什么需要这个函数）:
 *   下拉框、发送参数、任务对端展示都要认出虚拟手机目标。
 *
 * Code Logic（这个函数做什么）:
 *   id 与常量精确相等。
 */
export function isMobileInboxDevice(deviceId: string | null | undefined): boolean {
  return deviceId === MOBILE_INBOX_DEVICE_ID;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   手机传输列表对「发给手机且已完成」显示 Download，不能靠 Receive 伪装。
 *
 * Code Logic（这个函数做什么）:
 *   Send + completed + peer 为 inbox id。
 */
export function isMobileInboxOffer(
  task: Pick<TransferTask, 'direction' | 'status' | 'peerDeviceId'>,
): boolean {
  return (
    task.direction === 'send' &&
    task.status === 'completed' &&
    isMobileInboxDevice(task.peerDeviceId)
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   传输页设备下拉需要一项始终在线的「手机」，且不进 list_devices。
 *
 * Code Logic（这个函数做什么）:
 *   合成 Device；address/port 为空，UI 不得展示假 IP。
 */
export function buildMobileInboxDevice(label: string): Device {
  return {
    id: MOBILE_INBOX_DEVICE_ID,
    name: label,
    address: '',
    port: 0,
    status: 'online',
  };
}
