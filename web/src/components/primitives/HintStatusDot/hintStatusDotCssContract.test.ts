/**
 * HintStatusDot 与原点 CSS 特异性合同。
 *
 * Business Logic（为什么需要这个测试）:
 *   项目卡 / worktree / session tab 把原点 class 叠在 HintStatusDot 上。
 *   原点宽高与 status 色若特异性不低于 hint，数字会被压回 7–8px 或改色，DOM 测试仍绿。
 *
 * Code Logic（这个测试做什么）:
 *   读三处 CSS 源文件，断言 hinted 几何绑在 [data-hint-tone]，原点色不覆盖 hinted。
 */

import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, test } from 'vitest';

const here = dirname(fileURLToPath(import.meta.url));
const webSrc = resolve(here, '../../..');

function readCss(relFromSrc: string): string {
  return readFileSync(resolve(webSrc, relFromSrc), 'utf8');
}

describe('HintStatusDot CSS specificity contract', () => {
  test('hinted size is bound to data-hint-tone so a later single class cannot shrink it', () => {
    const css = readCss('components/primitives/HintStatusDot/HintStatusDot.module.css');
    expect(css).toMatch(/\.hinted\[data-hint-tone\][\s\S]{0,280}min-width:\s*16px/);
    expect(css).toMatch(/\.hinted\[data-hint-tone\][\s\S]{0,280}height:\s*16px/);
  });

  test('origin status colors yield when the dot is hinted', () => {
    const workbench = readCss('pages/Workbench/Workbench.module.css');
    const rail = readCss(
      'components/domain/WorkbenchProjectRail/WorkbenchProjectRail.module.css',
    );
    expect(workbench).toMatch(/\.sessionDot:not\(\[data-hint-tone\]\)/);
    expect(workbench).toMatch(
      /\.sessionDot\[data-status='running'\]:not\(\[data-hint-tone\]\)/,
    );
    expect(workbench).toMatch(
      /\.sessionDot\[data-status='disconnected'\]:not\(\[data-hint-tone\]\)/,
    );
    expect(workbench).toMatch(
      /\.worktreeChip[\s\S]{0,180}grid-template-columns:\s*auto minmax\(0,\s*1fr\) auto/,
    );
    expect(workbench).toMatch(/\.worktreeDot:not\(\[data-hint-tone\]\)/);
    expect(workbench).toMatch(
      /\.worktreeDot\[data-tone='warning'\]:not\(\[data-hint-tone\]\)/,
    );
    expect(workbench).toMatch(
      /\.worktreeDot\[data-tone='danger'\]:not\(\[data-hint-tone\]\)/,
    );
    expect(rail).toMatch(/\.projectStatusDot:not\(\[data-hint-tone\]\)/);
    expect(rail).toMatch(
      /\.projectStatusDot\[data-active='true'\]:not\(\[data-hint-tone\]\)/,
    );
  });
});
