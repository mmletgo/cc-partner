import { readFileSync } from 'node:fs';

/**
 * Business Logic（为什么需要这个函数）:
 *   侧栏项目入口的“添加项目”弹层渲染在 240px 侧栏内，固定宽度过大时会被 Sidebar 横向裁切。
 *
 * Code Logic（这个函数做什么）:
 *   读取 CSS Modules 源码并断言来源弹层使用与侧栏内容宽度匹配的宽度约束。
 */
function assertContains(source: string, expected: string, message: string): void {
  if (!source.includes(expected)) {
    throw new Error(message);
  }
}

/**
 * Business Logic（为什么需要这个测试）:
 *   用户点击侧栏“+”时需要完整看到“本机项目 / 局域网设备”两个入口，不能丢失左侧图标和内边距。
 *
 * Code Logic（这个测试做什么）:
 *   锁定 `.sourcePopover` 使用 box-sizing 与不超过侧栏内容区的 width 计算。
 */
async function main(): Promise<void> {
  const css = readFileSync(new URL('./WorkbenchProjectRail.module.css', import.meta.url), 'utf8');
  assertContains(css, '.sourcePopover {', 'source popover style exists');
  assertContains(css, 'box-sizing: border-box;', 'source popover should include border and padding in width');
  assertContains(
    css,
    'width: min(260px, calc(var(--sidebar-width) - var(--space-6)));',
    'source popover should fit within sidebar content width',
  );
  assertContains(css, '.sourceOption > svg {', 'source option icon style exists');
  assertContains(css, 'flex: 0 0 auto;', 'source option icon should not shrink');
}

void main();
