/**
 * check-css-tokens.test.mjs — design token contract 的 fixture 测试。
 *
 * Business Logic:
 *   保证 CSS token 守卫对「已定义 / 嵌套 fallback / 未知语义 token /
 *   运行时 allowlist / 深色主题缺失 / reduced-motion / 正文对比度 /
 *   未评审 meta 文本色」给出稳定诊断，避免静默失效或后续回归。
 *
 * Code Logic:
 *   使用临时 fixture 字符串调用 analyzeCssTokenContract，
 *   并对 reduced-motion 规则做静态断言。
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  analyzeCssTokenContract,
  contrastRatio,
  MIN_NORMAL_TEXT_CONTRAST,
  REQUIRED_TEXT_CONTRAST_PAIRS,
} from './check-css-tokens.mjs';

const scriptDir = dirname(fileURLToPath(import.meta.url));
const globalsCssPath = resolve(scriptDir, '../src/styles/globals.css');
const realTokensPath = resolve(scriptDir, '../src/styles/tokens.css');

/**
 * 构造最小 tokens 源，覆盖浅色/深色主题中的 canonical 示例。
 *
 * Business Logic:
 *   测试需要可控的 token 定义源，而不是依赖真实 tokens.css 的完整内容。
 *
 * Code Logic:
 *   返回包含 :root 与 [data-theme="dark"] 的 CSS 字符串。
 *
 * @param {object} [opts]
 * @param {boolean} [opts.omitDarkAccent]
 * @param {boolean} [opts.omitFgMutedReadable]
 * @param {boolean} [opts.weakReadable]
 * @returns {string}
 */
function buildTokensSource(opts = {}) {
  const darkAccent = opts.omitDarkAccent ? '' : '  --accent: #d97757;\n';
  const lightReadable = opts.omitFgMutedReadable
    ? ''
    : opts.weakReadable
      ? '  --fg-muted-readable: #87867f;\n'
      : '  --fg-muted-readable: #5e5d59;\n';
  const darkReadable = opts.omitFgMutedReadable
    ? ''
    : opts.weakReadable
      ? '  --fg-muted-readable: #787671;\n'
      : '  --fg-muted-readable: #a8a59e;\n';
  return `
:root {
  --bg: #f5f4ed;
  --surface: #faf9f5;
  --fg: #141413;
  --fg-2: #3d3d3a;
  --muted: #5e5d59;
${lightReadable}  --meta: #87867f;
  --border: #f0eee6;
  --border-soft: #e8e6dc;
  --accent: #c96442;
  --warn: #eab308;
  --space-2: 8px;
  --motion-base: 200ms;
}
[data-theme="dark"] {
  --bg: #1f1d1b;
  --surface: #272522;
  --fg: #faf9f5;
  --fg-2: #d8d6cf;
  --muted: #a8a59e;
${darkReadable}  --meta: #787671;
  --border: #2e2b29;
  --border-soft: #262422;
${darkAccent}  --warn: #facc15;
}
`;
}

describe('analyzeCssTokenContract', () => {
  it('passes defined token usage', () => {
    const diagnostics = analyzeCssTokenContract(
      [
        {
          path: 'ok.css',
          content: '.card { color: var(--fg); background: var(--surface); }',
        },
      ],
      buildTokensSource(),
    );
    assert.deepEqual(diagnostics, []);
  });

  it('flags every name in nested fallback when undefined', () => {
    const diagnostics = analyzeCssTokenContract(
      [
        {
          path: 'nested.css',
          content: '.row {\n  background: var(--bg-1, var(--bg-2));\n}\n',
        },
      ],
      buildTokensSource(),
    );
    assert.deepEqual(diagnostics, [
      'nested.css:2 --bg-1',
      'nested.css:2 --bg-2',
    ]);
  });

  it('reports unknown semantic tokens with file:line --token format', () => {
    const diagnostics = analyzeCssTokenContract(
      [
        {
          path: 'pages/x.module.css',
          content: '.title {\n  color: var(--fg-muted);\n  border-color: var(--border-subtle);\n}\n',
        },
      ],
      buildTokensSource(),
    );
    assert.deepEqual(diagnostics, [
      'pages/x.module.css:2 --fg-muted',
      'pages/x.module.css:3 --border-subtle',
    ]);
  });

  it('allows only documented runtime structural variables', () => {
    const diagnostics = analyzeCssTokenContract(
      [
        {
          path: 'Workbench.module.css',
          content: `
.panel {
  left: var(--prompt-panel-left, var(--space-2));
  top: var(--prompt-panel-top);
  stroke: var(--git-graph-color);
  height: var(--mobile-shell-height);
  color: var(--mystery-runtime);
}
`,
        },
      ],
      buildTokensSource(),
    );
    assert.deepEqual(diagnostics, ['Workbench.module.css:7 --mystery-runtime']);
  });

  it('detects color tokens missing dark theme values', () => {
    const diagnostics = analyzeCssTokenContract(
      [
        {
          path: 'ok.css',
          content: '.btn { color: var(--accent); }',
        },
      ],
      buildTokensSource({ omitDarkAccent: true }),
    );
    assert.deepEqual(diagnostics, ['tokens.css:0 --accent']);
  });

  it('ignores var() mentions inside CSS comments', () => {
    const diagnostics = analyzeCssTokenContract(
      [
        {
          path: 'comment.css',
          content: '/* use var(--xxx) always */\n.ok { color: var(--fg); }\n',
        },
      ],
      buildTokensSource(),
    );
    assert.deepEqual(diagnostics, []);
  });

  it('flags missing or weak fg-muted-readable contrast pairs', () => {
    const missing = analyzeCssTokenContract([], buildTokensSource({ omitFgMutedReadable: true }));
    assert.ok(
      missing.some((d) => d.includes('--fg-muted-readable') && d.includes('unresolved')),
      `expected unresolved readable diagnostics, got: ${missing.join(' | ')}`,
    );

    const weak = analyzeCssTokenContract([], buildTokensSource({ weakReadable: true }));
    assert.ok(
      weak.some((d) => d.includes('contrast') && d.includes('--fg-muted-readable')),
      `expected weak contrast diagnostics, got: ${weak.join(' | ')}`,
    );
  });

  it('rejects color: var(--meta) outside reviewed allowlist', () => {
    const diagnostics = analyzeCssTokenContract(
      [
        {
          path: 'src/pages/Home/Home.module.css',
          content: '.metaLabel {\n  color: var(--meta);\n}\n',
        },
      ],
      buildTokensSource(),
    );
    assert.ok(
      diagnostics.some((d) => d.includes('meta-color not allowlisted')),
      `expected meta allowlist failure, got: ${diagnostics.join(' | ')}`,
    );
  });

  it('allows reviewed placeholder meta color without contrast requirement on meta', () => {
    const diagnostics = analyzeCssTokenContract(
      [
        {
          path: 'src/components/primitives/Input/Input.module.css',
          content: '.input::placeholder {\n  color: var(--meta);\n}\n',
        },
      ],
      buildTokensSource(),
    );
    assert.deepEqual(diagnostics, []);
  });

  it('real tokens.css meets required normal-text contrast pairs', () => {
    const tokensSource = readFileSync(realTokensPath, 'utf8');
    const diagnostics = analyzeCssTokenContract([], tokensSource);
    const contrastFails = diagnostics.filter((d) => d.includes('contrast'));
    assert.deepEqual(contrastFails, []);
    assert.ok(REQUIRED_TEXT_CONTRAST_PAIRS.length >= 8);
    assert.equal(MIN_NORMAL_TEXT_CONTRAST, 4.5);
    // meta must remain below body requirement on surface (decorative only)
    assert.ok(contrastRatio('#87867f', '#faf9f5') < 4.5);
    assert.ok(contrastRatio('#787671', '#272522') < 4.5);
  });
});

describe('reduced-motion contract', () => {
  it('globals.css disables shimmer-active motion under prefers-reduced-motion', () => {
    const globals = readFileSync(globalsCssPath, 'utf8');
    assert.match(
      globals,
      /@media\s*\(\s*prefers-reduced-motion:\s*reduce\s*\)/,
      'globals.css must declare prefers-reduced-motion media query',
    );
    const blockMatch = globals.match(
      /@media\s*\(\s*prefers-reduced-motion:\s*reduce\s*\)\s*\{([\s\S]*?)\n\}/,
    );
    assert.ok(blockMatch, 'reduced-motion block must be present');
    const block = blockMatch[1];
    assert.match(block, /animation-duration:\s*0\.01ms/);
    assert.match(block, /animation-iteration-count:\s*1/);
    assert.match(block, /transition-duration:\s*0\.01ms/);
    assert.match(block, /scroll-behavior:\s*auto/);
    // Near-zero duration means shimmer keyframes cannot remain visibly active.
    assert.doesNotMatch(
      block,
      /animation-duration:\s*(?!0\.01ms)\d/,
      'reduced-motion must zero-out animation duration so shimmer cannot stay active',
    );
  });
});
