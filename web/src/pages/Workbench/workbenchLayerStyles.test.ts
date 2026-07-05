import { readFileSync } from 'node:fs';

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 文件预览层和自动化层必须盖住常驻终端层，否则终端会遮挡文件预览或自动化看板。
 *
 * Code Logic（这个函数做什么）:
 *   读取 Workbench CSS Modules 源码并断言终端层、文件层和 hidden 状态保留必要的层级规则。
 */
function assertContains(source: string, expected: string, message: string): void {
  if (!source.includes(expected)) {
    throw new Error(message);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   文件工作区与终端区采用叠层切换，回归测试需要锁住“不卸载终端但不遮挡文件层”的样式契约。
 *
 * Code Logic（这个函数做什么）:
 *   检查 `.terminalLayer` 使用基础层级、`.fileLayer` / `.automationLayer` 使用更高层级，hidden 状态同时禁用可见性和指针。
 */
async function main(): Promise<void> {
  const css = readFileSync(new URL('./Workbench.module.css', import.meta.url), 'utf8');
  assertContains(css, '.terminalLayer {', 'terminal layer style exists');
  assertContains(css, 'z-index: var(--z-base);', 'terminal layer stays below file layer');
  assertContains(css, '.fileLayer {', 'file layer style exists');
  assertContains(css, '.automationLayer {', 'automation layer style exists');
  assertContains(css, '.automationBody {', 'automation body scroll style exists');
  assertContains(css, 'z-index: var(--z-sticky);', 'file layer renders above terminal layer');
  assertContains(css, "data-hidden='true']", 'hidden layer selector exists');
  assertContains(css, 'opacity: 0;', 'hidden layer is visually transparent');
  assertContains(css, 'visibility: hidden;', 'hidden layer is not visible');
  assertContains(css, 'pointer-events: none;', 'hidden layer does not intercept input');
}

void main();
