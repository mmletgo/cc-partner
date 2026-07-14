/**
 * check-i18n-jsx.test.mjs — i18n JSX AST 合同的 fixture 测试。
 *
 * Business Logic:
 *   保证守卫对「已翻译表达式 / 中文 JSXText / 英文 aria-label /
 *   纯符号 / 品牌 allowlist / 测试与 DesignSystem 排除」给出稳定诊断。
 *
 * Code Logic:
 *   使用内存 fixture 调用 analyzeI18nJsxContract / shouldScanTsxPath /
 *   ALLOWED_LITERAL_COPY，不依赖真实业务源码。
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import {
  ALLOWED_LITERAL_COPY,
  analyzeI18nJsxContract,
  analyzeTsxI18nLiterals,
  isAllowedLiteral,
  shouldScanTsxPath,
} from './check-i18n-jsx.mjs';

describe('ALLOWED_LITERAL_COPY', () => {
  it('exports the finite brand/tech allowlist', () => {
    assert.deepEqual([...ALLOWED_LITERAL_COPY], [
      'cc-partner',
      'Claude Code',
      'Git',
      'GitHub',
      'HTML',
      'JSON',
      'SQLite',
      'tmux',
      'WSL',
    ]);
  });

  it('allows pure punctuation and brand terms', () => {
    assert.equal(isAllowedLiteral('·'), true);
    assert.equal(isAllowedLiteral('✓'), true);
    assert.equal(isAllowedLiteral('GitHub'), true);
    assert.equal(isAllowedLiteral('Hello'), false);
    assert.equal(isAllowedLiteral('矩形'), false);
  });
});

describe('shouldScanTsxPath', () => {
  it('includes production tsx and excludes test/stories/DesignSystem', () => {
    assert.equal(shouldScanTsxPath('src/pages/Home/Home.tsx'), true);
    assert.equal(shouldScanTsxPath('src/pages/Home/Home.test.tsx'), false);
    assert.equal(shouldScanTsxPath('src/pages/Home/Home.stories.tsx'), false);
    assert.equal(
      shouldScanTsxPath('src/pages/Workbench/testing/workbenchTestHarness.tsx'),
      false,
    );
    assert.equal(
      shouldScanTsxPath('src/pages/DesignSystem/DesignSystem.tsx'),
      false,
    );
    assert.equal(shouldScanTsxPath('src/lib/util.ts'), false);
  });
});

describe('analyzeTsxI18nLiterals', () => {
  it('passes translated expression children and attrs', () => {
    const source = `
export function Ok() {
  return (
    <section aria-label={t('nav:primary')}>
      <span title={t('common:action.save')}>{t('workbench:title')}</span>
    </section>
  );
}
`;
    assert.deepEqual(analyzeTsxI18nLiterals('src/ok.tsx', source), []);
  });

  it('flags Chinese JSXText with file:line:column', () => {
    const source = `export function Bad() {\n  return <span>添加项目</span>;\n}\n`;
    const diagnostics = analyzeTsxI18nLiterals('src/bad.tsx', source);
    assert.equal(diagnostics.length, 1);
    assert.match(diagnostics[0], /^src\/bad\.tsx:\d+:\d+$/);
    assert.equal(diagnostics[0], 'src/bad.tsx:2:16');
  });

  it('flags English aria-label string literal', () => {
    const source = `export function Bad() {\n  return <nav aria-label="primary" />;\n}\n`;
    const diagnostics = analyzeTsxI18nLiterals('src/nav.tsx', source);
    assert.equal(diagnostics.length, 1);
    assert.match(diagnostics[0], /^src\/nav\.tsx:2:\d+$/);
  });

  it('passes symbol-only JSXText and title', () => {
    const source = `export function Ok() {\n  return <button title="✓">·</button>;\n}\n`;
    assert.deepEqual(analyzeTsxI18nLiterals('src/sym.tsx', source), []);
  });

  it('passes brand allowlist literals', () => {
    const source = `export function Ok() {\n  return <span title="GitHub">tmux</span>;\n}\n`;
    assert.deepEqual(analyzeTsxI18nLiterals('src/brand.tsx', source), []);
  });
});

describe('analyzeI18nJsxContract', () => {
  it('skips excluded paths even if content would fail', () => {
    const diagnostics = analyzeI18nJsxContract([
      {
        path: 'src/pages/DesignSystem/DesignSystem.tsx',
        content: 'export function D() { return <span>Hardcoded English</span>; }\n',
      },
      {
        path: 'src/pages/Home/Home.test.tsx',
        content: 'export function T() { return <span>测试文案</span>; }\n',
      },
      {
        path: 'src/pages/Home/Home.tsx',
        content: 'export function H() { return <span>Hardcoded English</span>; }\n',
      },
    ]);
    assert.equal(diagnostics.length, 1);
    assert.match(diagnostics[0], /^src\/pages\/Home\/Home\.tsx:1:\d+$/);
  });
});
