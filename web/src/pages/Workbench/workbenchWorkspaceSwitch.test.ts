// node:fs 类型由 @types/node 提供,但本仓库 tsconfig 未在 compilerOptions.types 显式纳入 node,
// tsx 测试上下文下类型缺失,故局部抑制(运行时 tsx 正常解析;node:fs 是 node 内置,无需安装)。
// @ts-expect-error - 本仓库 tsconfig 未在 compilerOptions.types 纳入 node,node:fs 类型缺失,运行时 tsx 正常
import { readFileSync } from 'node:fs';

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 终端和文件预览应提供对称入口，避免用户从终端回到已打开文件时只能重新点击右侧文件树。
 *
 * Code Logic（这个函数做什么）:
 *   读取源码或 locale 文本并断言包含指定片段；缺失时抛出带上下文的错误。
 */
function assertContains(source: string, expected: string, message: string): void {
  if (!source.includes(expected)) {
    throw new Error(message);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端工具栏的文件预览按钮需要锁住可回归检查的最小契约：有打开文件才可点、点击切到文件层、
 *   且中英文 tooltip 与文件预览返回终端入口对称。
 *
 * Code Logic（这个函数做什么）:
 *   静态读取 Workbench 页面和 workbench i18n 资源，检查切换回调、按钮绑定、禁用条件和文案 key。
 */
async function main(): Promise<void> {
  const workbenchSource = readFileSync(new URL('./Workbench.tsx', import.meta.url), 'utf8');
  const zhLocale = readFileSync(new URL('../../i18n/locales/zh/workbench.json', import.meta.url), 'utf8');
  const enLocale = readFileSync(new URL('../../i18n/locales/en/workbench.json', import.meta.url), 'utf8');

  assertContains(workbenchSource, 'const handleReturnToFiles = useCallback', 'terminal -> files callback exists');
  assertContains(workbenchSource, "setWorkspaceView('files');", 'callback opens file workspace layer');
  assertContains(workbenchSource, 'disabled={fileTabs.length === 0}', 'file preview button is disabled with no opened tabs');
  assertContains(workbenchSource, "t('workbench:fileWorkspace.openFiles')", 'button uses localized file preview label');
  assertContains(zhLocale, '"openFiles": "文件预览"', 'zh file preview label exists');
  assertContains(enLocale, '"openFiles": "File preview"', 'en file preview label exists');
}

void main();
