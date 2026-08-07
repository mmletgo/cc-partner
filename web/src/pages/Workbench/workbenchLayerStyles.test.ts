import { describe, test } from 'vitest';
import { readFileSync } from 'node:fs';

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 浏览器/文件预览层和自动化层必须盖住常驻终端层，否则终端会遮挡预览或自动化看板。
 *
 * Code Logic（这个函数做什么）:
 *   读取 Workbench CSS Modules 源码并断言终端层、浏览器层、文件层和 hidden 状态保留必要的层级规则。
 */
function assertContains(source: string, expected: string, message: string): void {
  if (!source.includes(expected)) {
    throw new Error(message);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   自动化控制台必须作为可见内容撑开 Workbench 中心区域，不能重新混入终端/文件层的绝对定位隐藏模型。
 *
 * Code Logic（这个函数做什么）:
 *   检查源码不包含指定字符串；存在时抛出错误暴露布局契约回退。
 */
function assertNotContains(source: string, unexpected: string, message: string): void {
  if (source.includes(unexpected)) {
    throw new Error(message);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   浏览器/文件工作区与终端区采用叠层切换，回归测试需要锁住“不卸载终端但不遮挡预览层”的样式契约。
 *
 * Code Logic（这个函数做什么）:
 *   检查 `.terminalLayer` 使用基础层级、`.browserLayer` / `.fileLayer` / `.automationLayer` 使用更高层级，hidden 状态同时禁用可见性和指针。
 */
describe('workbenchLayerStyles', () => {
  test('layered workbench layout keeps automation in flow and hidden layers non-interactive', async () => {
  const css = readFileSync(new URL('./Workbench.module.css', import.meta.url), 'utf8');
  assertContains(css, '.terminalLayer {', 'terminal layer style exists');
  assertContains(css, 'z-index: var(--z-base);', 'terminal layer stays below file layer');
  assertContains(css, '.browserLayer', 'browser layer style exists');
  assertContains(css, '.fileLayer {', 'file layer style exists');
  assertContains(css, '.automationLayer {', 'automation layer style exists');
  assertContains(
    css,
    '.terminalLayer,\n.browserLayer,\n.fileLayer {',
    'terminal, browser, and file layers should share the absolute overlay base',
  );
  assertNotContains(
    css,
    '.browserLayer,\n.automationLayer',
    'automation layer must stay outside the hidden absolute browser layer model',
  );
  assertNotContains(
    css,
    '.fileLayer,\n.automationLayer',
    'automation layer must stay in normal flow so it can keep the project automation page visible',
  );
  assertContains(
    css,
    'position: relative;',
    'automation layer should be positioned relative inside mainWorkspace instead of absolute',
  );
  assertContains(css, '.automationBody {', 'automation body scroll style exists');
  assertContains(css, 'z-index: var(--z-sticky);', 'file layer renders above terminal layer');
  assertContains(css, "data-hidden='true']", 'hidden layer selector exists');
  assertContains(css, 'opacity: 0;', 'hidden layer is visually transparent');
  assertContains(css, 'visibility: hidden;', 'hidden layer is not visible');
  assertContains(css, 'pointer-events: none;', 'hidden layer does not intercept input');
  });

  test('inactive terminal windows leave layout while preserving the mounted React pane', () => {
    const css = readFileSync(new URL('./Workbench.module.css', import.meta.url), 'utf8');
    if (!/\.terminalPaneFrame\s*\{[\s\S]*?display:\s*none;[\s\S]*?\}/.test(css)) {
      throw new Error('inactive terminal pane must discard its stale WebView compositing layer');
    }
    if (!/\.terminalPaneFrame\[data-active='true'\]\s*\{[\s\S]*?display:\s*grid;[\s\S]*?\}/.test(css)) {
      throw new Error('active terminal pane must re-enter layout before xterm recovery runs');
    }
  });
});
