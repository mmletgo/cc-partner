/**
 * 移动端访问入口 API。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面端需要向用户展示手机浏览器可访问的局域网 `/mobile` 地址。
 *
 * Code Logic（这个模块做什么）:
 *   封装 `/api/mobile/access-info` 同源 HTTP 调用，返回统一的 MobileAccessInfo DTO。
 */

import type { MobileAccessInfo } from '@/lib/types';
import { invoke } from './client';
import { getJson } from './workbenchHttp';

interface MobileAccessInfoRuntime {
  __TAURI_INTERNALS__?: {
    transformCallback?: unknown;
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   桌面端 Settings/Workbench 运行在 Tauri WebView 中，不能依赖普通浏览器同源 HTTP；手机浏览器则不能调用 invoke。
 *
 * Code Logic（这个函数做什么）:
 *   检测 Tauri 注入的 transformCallback，存在时返回 true；测试可传入 runtime 对象避免依赖真实 window。
 */
export function shouldUseTauriMobileAccessInfo(
  runtime: MobileAccessInfoRuntime | undefined =
    typeof window === 'undefined' ? undefined : (window as MobileAccessInfoRuntime),
): boolean {
  return typeof runtime?.__TAURI_INTERNALS__?.transformCallback === 'function';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   设置页或移动端入口组件需要获取当前设备名、HTTP 端口和可访问 URL 列表。
 *
 * Code Logic（这个函数做什么）:
 *   Tauri 桌面环境走 `get_mobile_access_info` invoke；普通浏览器环境 GET `/api/mobile/access-info`。
 */
export function getMobileAccessInfo(): Promise<MobileAccessInfo> {
  if (shouldUseTauriMobileAccessInfo()) {
    return invoke<MobileAccessInfo>('get_mobile_access_info');
  }
  return getJson<MobileAccessInfo>('/api/mobile/access-info');
}
