import { shouldUseTauriMobileAccessInfo } from './mobile';

/**
 * Business Logic（为什么需要这个函数）:
 *   桌面端 MobileAccessCard 必须走 Tauri invoke 获取 access-info，普通手机浏览器才走同源 HTTP。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛错，让 tsx 测试进程以非零状态退出。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

assert(
  shouldUseTauriMobileAccessInfo({
    __TAURI_INTERNALS__: { transformCallback: () => 1 },
  }),
  'Tauri desktop runtime should use invoke for mobile access info',
);

assert(
  !shouldUseTauriMobileAccessInfo({}),
  'plain browser runtime should use same-origin HTTP for mobile access info',
);

console.log('mobile.test.ts passed');
