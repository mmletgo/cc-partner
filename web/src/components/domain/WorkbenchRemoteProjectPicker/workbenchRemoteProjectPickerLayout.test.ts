import { describe, test } from 'vitest';
import { readFileSync } from 'node:fs';

/**
 * Business Logic（为什么需要这个函数）:
 *   远端项目选择器是用户打开局域网项目的关键入口，布局回归会直接阻断工作台使用。
 *
 * Code Logic（这个函数做什么）:
 *   读取源码并断言关键布局契约存在或不存在；失败时抛出可定位原因。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移除手动路径功能后，需要防止组件、样式、i18n 和 helper 中残留入口再次显示。
 *
 * Code Logic（这个函数做什么）:
 *   断言指定源码不包含旧手动路径标记。
 */
function assertNotContains(source: string, unexpected: string, message: string): void {
  assert(!source.includes(unexpected), message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   远端项目弹窗应像文件选择器一样分区，避免设备、根位置和目录浏览混在同一纵向流里。
 *
 * Code Logic（这个函数做什么）:
 *   断言指定源码包含稳定的 CSS 契约片段。
 */
function assertContains(source: string, expected: string, message: string): void {
  assert(source.includes(expected), message);
}

describe('workbenchRemoteProjectPickerLayout', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   打开远端项目弹窗必须只保留设备、可浏览位置、远端目录和当前选择，并保证文案和操作区不贴边。
   *
   * Code Logic（这个测试做什么）:
   *   检查组件源码、CSS、locale 和 helper 中没有手动路径入口，并锁定左侧导航 + 右侧目录浏览 + Card 子区域 padding 契约。
   */
  test('picker exposes device/root/browser grid layout without manual path UI', () => {
    const component = readFileSync(new URL('./WorkbenchRemoteProjectPicker.tsx', import.meta.url), 'utf8');
    const css = readFileSync(new URL('./WorkbenchRemoteProjectPicker.module.css', import.meta.url), 'utf8');
    const helpers = readFileSync(new URL('../../../lib/workbenchRemoteProjects.ts', import.meta.url), 'utf8');
    const zhLocale = readFileSync(new URL('../../../i18n/locales/zh/workbench.json', import.meta.url), 'utf8');
    const enLocale = readFileSync(new URL('../../../i18n/locales/en/workbench.json', import.meta.url), 'utf8');

    for (const source of [component, css, helpers, zhLocale, enLocale]) {
      assertNotContains(source, 'manualPath', 'remote picker should not expose manual path UI or locale keys');
      assertNotContains(source, 'manualUnverified', 'remote picker should not expose manual path unverified state');
    }
    assertContains(component, '<Input', 'create-folder dialog may use Input; manual path Input remains forbidden by manualPath assertions');
    assertNotContains(component, 'canOpenRemoteManualProjectPath', 'remote picker should not use manual open helper');
    assertNotContains(component, 'normalizeRemoteManualPath', 'remote picker should not normalize manual paths');

    assertContains(component, '<Card.Header className={styles.header} padding="md">', 'picker header should keep text away from the border');
    assertContains(component, '<Card.Body className={styles.body} padding="md">', 'picker body should keep content away from the border');
    assertContains(component, '<Card.Footer className={styles.footer} padding="md">', 'picker footer should keep actions away from the border');
    assertContains(component, 'styles.devicesSection', 'device section should have a dedicated layout area');
    assertContains(component, 'styles.rootsSection', 'roots section should have a dedicated layout area');
    assertContains(css, 'grid-template-areas:', 'picker body should use explicit file-picker grid areas');
    assertContains(css, '"devices browser"', 'device navigation should stay beside the browser pane');
    assertContains(css, '"roots browser"', 'root navigation should stay beside the browser pane');
    assertContains(css, 'grid-area: devices;', 'device section should be assigned to the devices area');
    assertContains(css, 'grid-area: roots;', 'roots section should be assigned to the roots area');
    assertContains(css, 'grid-area: browser;', 'browser section should be assigned to the browser area');
    assertContains(css, 'flex-direction: column;', 'root list should behave as vertical navigation');
    assertContains(css, '.browser {', 'browser section should have a dedicated layout block');
    assertContains(css, 'overflow: hidden;', 'browser section should own its internal scrolling');
    assertContains(css, 'height: min(760px, calc(100vh - var(--space-8)));', 'picker should have a stable file-picker height');
  });
});
