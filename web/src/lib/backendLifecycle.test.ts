import { describe, test } from 'vitest';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

/**
 * Business Logic（为什么需要这个函数）:
 *   Task 6 要求 GUI 关闭必须给用户选择是否停止后台后端，静态测试需要用清晰断言防止回归。
 *
 * Code Logic（这个函数做什么）:
 *   接收布尔条件和失败消息；条件为 false 时抛出 Error，让测试用例失败。
 */
function assert(condition: unknown, message: string): asserts condition {
  if (!condition) {
    throw new Error(message);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   关闭选择弹窗的所有用户可见文案必须走 common namespace 的中英文资源，不能硬编码在组件中。
 *
 * Code Logic（这个函数做什么）:
 *   读取指定语言的 common.json 并解析成可按 key 访问的对象。
 */
function readCommonLocale(language: 'en' | 'zh'): Record<string, unknown> {
  const content = readFileSync(
    join(process.cwd(), 'src', 'i18n', 'locales', language, 'common.json'),
    'utf8',
  );
  return JSON.parse(content) as Record<string, unknown>;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要验证 backendClose 下的固定 key 同时存在于 en/zh，避免某个语言运行时缺文案。
 *
 * Code Logic（这个函数做什么）:
 *   用对象路径逐级读取 key，并断言最终值为非空字符串。
 */
function assertLocaleKey(locale: Record<string, unknown>, key: string): void {
  const value = key
    .split('.')
    .reduce<unknown>(
      (current, part) =>
        current && typeof current === 'object'
          ? (current as Record<string, unknown>)[part]
          : undefined,
      locale,
    );
  assert(typeof value === 'string' && value.length > 0, `缺少 i18n key: ${key}`);
}

describe('backendLifecycle', () => {
  test('App close choice listener and i18n keys enforce GUI vs full close paths', () => {
    const appSource = readFileSync(join(process.cwd(), 'src', 'App.tsx'), 'utf8');

    // T5 起 export function + props；T5 关闭路径经 flushPendingWritesThenClose 门闩
    const backendCloseListenerStart = appSource.search(
      /(?:export\s+)?function\s+BackendCloseChoiceListener\s*\(/,
    );
    const appComponentStart = appSource.indexOf('export default function App()', backendCloseListenerStart);
    assert(backendCloseListenerStart >= 0, 'App.tsx 必须定义 BackendCloseChoiceListener');
    assert(appComponentStart > backendCloseListenerStart, 'App.tsx 必须在 App 组件前定义 BackendCloseChoiceListener');
    const backendCloseListenerSource = appSource.slice(backendCloseListenerStart, appComponentStart);
    const mainWindowGuardIndex = backendCloseListenerSource.search(
      /(?:currentWindow|getCurrentWindow\(\))\.label\s*(?:===|!==)\s*['"]main['"]/,
    );
    const closeRequestedIndex = backendCloseListenerSource.indexOf('onCloseRequested');
    const trayCloseRequestedIndex = backendCloseListenerSource.indexOf("'backend:close-requested'");
    assert(mainWindowGuardIndex >= 0, 'BackendCloseChoiceListener 必须按 window.label 限制为 main 窗口');
    assert(
      appSource.includes('shouldMountGlobalWindowListeners'),
      'App.tsx 必须用 shouldMountGlobalWindowListeners 限制健康/权限/运营通知只挂主窗',
    );
    assert(
      appSource.includes('MainWindowOnlyListeners') &&
        appSource.includes('MainWindowOperationalNotifications'),
      'App.tsx 必须把健康/权限/运营通知收进 main-only 包装',
    );
    assert(
      appSource.includes('workbench:apply-deeplink'),
      'App.tsx 必须监听卫星窗深链 workbench:apply-deeplink',
    );
    assert(
      mainWindowGuardIndex < closeRequestedIndex && mainWindowGuardIndex < trayCloseRequestedIndex,
      'BackendCloseChoiceListener 必须先判断 main 窗口，再注册窗口关闭和托盘关闭监听',
    );

    assert(
      appSource.includes('getCurrentWindow') || appSource.includes('onCloseRequested'),
      'App.tsx 必须监听 Tauri close requested 事件',
    );
    assert(appSource.includes('preventDefault()'), 'App.tsx 必须 preventDefault 阻止直接关闭');
    assert(appSource.includes('backendApi.stop()'), 'App.tsx 必须在完整关闭路径调用 backendApi.stop()');
    assert(
      appSource.includes('flushPendingWritesThenClose'),
      'App.tsx 关闭路径必须经 flushPendingWritesThenClose 门闩',
    );

    const guiOnlyHandler = appSource.match(
      /handleGuiOnlyClose\s*=\s*async\s*\(\)\s*=>\s*\{([\s\S]*?)\n\s*\}/,
    );
    assert(guiOnlyHandler !== null, 'App.tsx 必须定义 handleGuiOnlyClose');
    assert(
      guiOnlyHandler[1].includes("flushPendingWritesThenClose('gui'") ||
        guiOnlyHandler[1].includes('flushPendingWritesThenClose("gui"'),
      '仅关闭 GUI 路径必须 flushPendingWritesThenClose(gui)',
    );
    assert(
      !guiOnlyHandler[1].includes("mode: 'full'") &&
        !guiOnlyHandler[1].includes('flushPendingWritesThenClose(\'full\''),
      '仅关闭 GUI 路径不能走 full close',
    );

    const fullCloseHandler = appSource.match(
      /handleFullClose\s*=\s*async\s*\(\)\s*=>\s*\{([\s\S]*?)\n\s*\}/,
    );
    assert(fullCloseHandler !== null, 'App.tsx 必须定义 handleFullClose');
    assert(
      fullCloseHandler[1].includes("flushPendingWritesThenClose('full'") ||
        fullCloseHandler[1].includes('flushPendingWritesThenClose("full"'),
      '前后端都关闭路径必须 flushPendingWritesThenClose(full)',
    );
    assert(
      fullCloseHandler[1].includes('backendApi.stop') &&
        fullCloseHandler[1].includes('backendApi.exitGui'),
      '前后端都关闭路径必须注入 stop 与 exitGui',
    );

    for (const locale of [readCommonLocale('en'), readCommonLocale('zh')]) {
      for (const key of [
        'backendClose.title',
        'backendClose.guiOnly',
        'backendClose.stopBackend',
        'backendClose.cancel',
      ]) {
        assertLocaleKey(locale, key);
      }
    }
  });
});
