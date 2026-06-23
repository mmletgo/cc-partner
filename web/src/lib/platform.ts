/**
 * platform - 运行平台判断（复用 navigator.platform）
 *
 * Business Logic（为什么需要这个模块）:
 *   macOS 通知权限引导（lib/notification.ts、usePermissions）需区分是否在 macOS：非 macOS
 *   视为通知已授权、不发 tauri notification JS API 调用。复用 shortcutRecorder 已采用的
 *   navigator.platform 判断模式，避免重复造平台检测逻辑（项目规则 9 复用）。
 *
 * Code Logic（这个模块做什么）:
 *   isMacos(platform?) 读取 navigator.platform（可注入便于单测），小写后包含 'mac' 即 true。
 */

/**
 * 判断当前是否运行在 macOS
 *
 * Business Logic: 通知权限等 macOS 专属引导需据此跳过非 macOS。
 * Code Logic: navigator.platform 含 'mac'（MacIntel/Macintosh）即 true；接受可选 platform
 *   参数便于单测注入，缺省读 globalThis.navigator?.platform。
 *
 * @param platform 平台字符串，缺省读 globalThis.navigator.platform
 * @returns 是否 macOS
 */
export function isMacos(platform: string = globalThis.navigator?.platform ?? ''): boolean {
  return platform.toLowerCase().includes('mac');
}
