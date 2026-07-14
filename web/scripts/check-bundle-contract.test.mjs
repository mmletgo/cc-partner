/**
 * check-bundle-contract.test.mjs — bundle 合同 fixture 与源码边界测试。
 *
 * Business Logic:
 *   保证 initial graph 静态闭包、gzip 预算、mobile 禁止重型依赖的诊断稳定，
 *   并锁住 Workbench 编辑器 / 移动重面板只通过动态 import 引入。
 *
 * Code Logic:
 *   用内存 fixture 调用闭包/预算/forbidden helper；再对源文件做静态 import 断言。
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';
import {
  analyzeBundleContract,
  collectStaticClosure,
  extractStylesheetHrefs,
  findForbiddenModules,
  formatBudgetKiB,
  MOBILE_FORBIDDEN_PATTERNS,
  normalizeCssFiles,
  sumGzipBytes,
} from './check-bundle-contract.mjs';

const scriptDir = dirname(fileURLToPath(import.meta.url));
const webRoot = resolve(scriptDir, '..');

/**
 * 构造最小 chunk graph fixture。
 *
 * Business Logic:
 *   测试需要可控的 entry/import/dynamicImport 关系，而不是依赖真实构建产物。
 *
 * Code Logic:
 *   返回 {entries,chunks} 结构，与 Vite 插件写出的合同一致。
 */
function buildContractFixture() {
  return {
    entries: {
      main: 'assets/main.js',
      mobile: 'assets/mobile.js',
    },
    chunks: {
      'assets/main.js': {
        fileName: 'assets/main.js',
        isEntry: true,
        name: 'main',
        facadeModuleId: '/src/main.tsx',
        imports: ['assets/shared.js'],
        dynamicImports: ['assets/route-health.js'],
        moduleIds: ['/src/main.tsx', '/src/App.tsx'],
        codeBytes: 100,
      },
      'assets/mobile.js': {
        fileName: 'assets/mobile.js',
        isEntry: true,
        name: 'mobile',
        facadeModuleId: '/src/mobile/main.tsx',
        imports: ['assets/shared.js'],
        dynamicImports: ['assets/mobile-terminal.js'],
        moduleIds: ['/src/mobile/main.tsx', '/src/mobile/MobileWorkbench.tsx'],
        codeBytes: 100,
      },
      'assets/shared.js': {
        fileName: 'assets/shared.js',
        isEntry: false,
        name: 'shared',
        facadeModuleId: null,
        imports: [],
        dynamicImports: [],
        moduleIds: ['/src/styles/tokens.css'],
        codeBytes: 50,
      },
      'assets/route-health.js': {
        fileName: 'assets/route-health.js',
        isEntry: false,
        name: 'Health',
        facadeModuleId: '/src/pages/Health/Health.tsx',
        imports: ['assets/shared.js'],
        dynamicImports: [],
        moduleIds: [
          '/src/pages/Health/Health.tsx',
          '/src/pages/Health/StatsChart.tsx',
          '/node_modules/recharts/es6/index.js',
        ],
        codeBytes: 9000,
      },
      'assets/mobile-terminal.js': {
        fileName: 'assets/mobile-terminal.js',
        isEntry: false,
        name: 'MobileTerminalPanel',
        facadeModuleId: '/src/mobile/components/MobileTerminalPanel.tsx',
        imports: ['assets/shared.js'],
        dynamicImports: [],
        moduleIds: [
          '/src/mobile/components/MobileTerminalPanel.tsx',
          '/node_modules/@xterm/xterm/lib/xterm.js',
        ],
        codeBytes: 8000,
      },
    },
  };
}

/**
 * 生成固定长度的伪 JS 源码（便于断言 gzip 大小）。
 *
 * Business Logic:
 *   gzip 结果依赖内容可重复性；测试用确定字符串避免 flaky。
 *
 * Code Logic:
 *   重复填充 char 到指定字节长度。
 *
 * @param {number} byteLength
 * @param {string} [char]
 * @returns {string}
 */
function makeSource(byteLength, char = 'a') {
  return char.repeat(byteLength);
}

describe('collectStaticClosure', () => {
  it('follows static imports and excludes dynamicImports', () => {
    const contract = buildContractFixture();
    const mainClosure = collectStaticClosure('assets/main.js', contract.chunks);
    assert.deepEqual([...mainClosure].sort(), ['assets/main.js', 'assets/shared.js']);

    const mobileClosure = collectStaticClosure('assets/mobile.js', contract.chunks);
    assert.deepEqual([...mobileClosure].sort(), ['assets/mobile.js', 'assets/shared.js']);
    assert.equal(mobileClosure.has('assets/mobile-terminal.js'), false);
    assert.equal(mainClosure.has('assets/route-health.js'), false);
  });

  it('handles missing chunk ids without throwing', () => {
    const closure = collectStaticClosure('assets/missing.js', {});
    assert.deepEqual([...closure], []);
  });
});

describe('sumGzipBytes', () => {
  it('sums gzip sizes of listed chunk files', () => {
    const files = {
      'assets/main.js': makeSource(4000, 'm'),
      'assets/shared.js': makeSource(2000, 's'),
    };
    const total = sumGzipBytes(['assets/main.js', 'assets/shared.js'], (fileName) =>
      Buffer.from(files[fileName], 'utf8'),
    );
    const expected =
      gzipSync(Buffer.from(files['assets/main.js'], 'utf8')).byteLength +
      gzipSync(Buffer.from(files['assets/shared.js'], 'utf8')).byteLength;
    assert.equal(total, expected);
  });
});

describe('findForbiddenModules', () => {
  it('detects forbidden packages inside mobile initial module ids', () => {
    const hits = findForbiddenModules(
      [
        '/src/mobile/MobileWorkbench.tsx',
        '/node_modules/@xterm/xterm/lib/xterm.js',
        '/node_modules/recharts/es6/index.js',
        '/node_modules/@uiw/react-codemirror/esm/index.js',
        '/node_modules/@codemirror/view/dist/index.js',
        '/node_modules/codemirror/dist/index.js',
        '/node_modules/@tiptap/react/dist/index.js',
        '/src/mobile/components/MobileProjectPanel.tsx',
      ],
      MOBILE_FORBIDDEN_PATTERNS,
    );
    assert.equal(hits.length, 6);
    assert.ok(hits.some((h) => h.includes('@xterm')));
    assert.ok(hits.some((h) => h.includes('recharts')));
    assert.ok(hits.some((h) => h.includes('@uiw/react-codemirror')));
    assert.ok(hits.some((h) => h.includes('@codemirror')));
    assert.ok(hits.some((h) => h.includes('/codemirror/')));
    assert.ok(hits.some((h) => h.includes('@tiptap')));
  });
});

describe('extractStylesheetHrefs', () => {
  it('extracts relative stylesheet hrefs and ignores non-css links', () => {
    const html = `
      <html><head>
        <link rel="stylesheet" href="/assets/main.css" />
        <link href="assets/extra.css" rel="stylesheet">
        <link rel="modulepreload" href="/assets/main.js" />
        <link rel="stylesheet" href="https://cdn.example/x.css" />
        <link rel="icon" href="/favicon.ico" />
      </head></html>
    `;
    assert.deepEqual(extractStylesheetHrefs(html), ['assets/main.css', 'assets/extra.css']);
  });
});

describe('normalizeCssFiles', () => {
  it('dedupes and strips leading slashes', () => {
    assert.deepEqual(normalizeCssFiles(['/assets/a.css', 'assets/a.css', '', null, 'assets/b.css']), [
      'assets/a.css',
      'assets/b.css',
    ]);
  });
});

describe('analyzeBundleContract', () => {
  it('passes under-budget graphs without forbidden modules', () => {
    const contract = buildContractFixture();
    const files = {
      'assets/main.js': makeSource(1000, 'A'),
      'assets/mobile.js': makeSource(1000, 'B'),
      'assets/shared.js': makeSource(1000, 'C'),
    };
    const result = analyzeBundleContract(contract, {
      readFile: (fileName) => Buffer.from(files[fileName], 'utf8'),
      budgets: {
        main: 320 * 1024,
        mobile: 280 * 1024,
      },
    });
    assert.deepEqual(result.diagnostics, []);
    assert.ok(result.entryReports.main.gzipBytes > 0);
    assert.ok(result.entryReports.mobile.gzipBytes > 0);
    assert.equal(result.entryReports.main.cssGzipBytes, 0);
    assert.deepEqual(result.entryReports.main.cssFiles, []);
  });

  it('includes entry HTML CSS gzip in total and reports js/css breakdown', () => {
    const contract = buildContractFixture();
    contract.entryStyles = {
      main: ['assets/main.css'],
      mobile: ['assets/mobile.css'],
    };
    const files = {
      'assets/main.js': makeSource(1000, 'A'),
      'assets/mobile.js': makeSource(1000, 'B'),
      'assets/shared.js': makeSource(1000, 'C'),
      'assets/main.css': makeSource(2000, 'M'),
      'assets/mobile.css': makeSource(1500, 'N'),
    };
    const result = analyzeBundleContract(contract, {
      readFile: (fileName) => Buffer.from(files[fileName], 'utf8'),
      budgets: {
        main: 320 * 1024,
        mobile: 280 * 1024,
      },
    });
    assert.deepEqual(result.diagnostics, []);
    const main = result.entryReports.main;
    const expectedJs = sumGzipBytes(['assets/main.js', 'assets/shared.js'], (f) =>
      Buffer.from(files[f], 'utf8'),
    );
    const expectedCss = sumGzipBytes(['assets/main.css'], (f) => Buffer.from(files[f], 'utf8'));
    assert.equal(main.jsGzipBytes, expectedJs);
    assert.equal(main.cssGzipBytes, expectedCss);
    assert.equal(main.gzipBytes, expectedJs + expectedCss);
    assert.deepEqual(main.cssFiles, ['assets/main.css']);
  });

  it('fails budget when CSS alone exceeds limit', () => {
    const contract = buildContractFixture();
    const noisy = Array.from({ length: 4000 }, (_, i) => String.fromCharCode(32 + (i % 90))).join('');
    const files = {
      'assets/main.js': makeSource(20, 'A'),
      'assets/mobile.js': makeSource(20, 'B'),
      'assets/shared.js': makeSource(20, 'C'),
      'assets/main.css': noisy,
    };
    const result = analyzeBundleContract(contract, {
      readFile: (fileName) => Buffer.from(files[fileName], 'utf8'),
      entryStyles: {
        main: ['assets/main.css'],
        mobile: [],
      },
      budgets: {
        main: 30,
        mobile: 280 * 1024,
      },
    });
    assert.equal(result.diagnostics.length, 1);
    assert.match(result.diagnostics[0], /main initial graph over budget/i);
    assert.match(result.diagnostics[0], /css=/);
    assert.ok(result.entryReports.main.cssGzipBytes > 0);
    // JS 小样本本身远小于 noisy CSS；总超预算由 CSS 主导
    assert.ok(result.entryReports.main.cssGzipBytes > result.entryReports.main.jsGzipBytes);
    assert.ok(result.entryReports.main.gzipBytes > 30);
  });

  it('reports over-budget diagnostics with entry name and sizes', () => {
    const contract = buildContractFixture();
    // 用高熵内容降低 gzip 压缩比，确保超过 10 字节预算
    const noisy = Array.from({ length: 4000 }, (_, i) => String.fromCharCode(32 + (i % 90))).join('');
    const files = {
      'assets/main.js': noisy,
      'assets/mobile.js': makeSource(100, 'B'),
      'assets/shared.js': noisy,
    };
    const result = analyzeBundleContract(contract, {
      readFile: (fileName) => Buffer.from(files[fileName], 'utf8'),
      budgets: {
        main: 10,
        mobile: 280 * 1024,
      },
    });
    assert.equal(result.diagnostics.length, 1);
    assert.match(result.diagnostics[0], /main initial graph over budget/i);
    assert.match(result.diagnostics[0], /budget=10B/);
  });

  it('reports forbidden module diagnostics for mobile initial closure only', () => {
    const contract = buildContractFixture();
    // 把 xterm 放进 mobile 静态 shared 边，模拟泄漏
    contract.chunks['assets/shared.js'].moduleIds = [
      '/src/styles/tokens.css',
      '/node_modules/@xterm/xterm/lib/xterm.js',
    ];
    const files = {
      'assets/main.js': makeSource(100, 'A'),
      'assets/mobile.js': makeSource(100, 'B'),
      'assets/shared.js': makeSource(100, 'C'),
    };
    const result = analyzeBundleContract(contract, {
      readFile: (fileName) => Buffer.from(files[fileName], 'utf8'),
      budgets: {
        main: 320 * 1024,
        mobile: 280 * 1024,
      },
    });
    assert.ok(
      result.diagnostics.some((d) => /mobile initial graph forbidden module/i.test(d)),
      `expected forbidden diagnostic, got: ${result.diagnostics.join('\n')}`,
    );
    // main 也共享 shared，但 forbidden 只对 mobile 入口强制
    assert.equal(
      result.diagnostics.some((d) => /main initial graph forbidden/i.test(d)),
      false,
    );
  });
});

describe('formatBudgetKiB', () => {
  it('formats byte budgets as integer KiB labels', () => {
    assert.equal(formatBudgetKiB(320 * 1024), '320 KiB');
    assert.equal(formatBudgetKiB(280 * 1024), '280 KiB');
  });
});

describe('source dynamic import boundaries', () => {
  it('WorkbenchFileWorkspace loads Code/Markdown/HTML editors dynamically', () => {
    const src = readFileSync(
      resolve(webRoot, 'src/components/domain/WorkbenchFileWorkspace/WorkbenchFileWorkspace.tsx'),
      'utf8',
    );
    assert.match(src, /lazy\s*\(/);
    assert.match(src, /import\s*\(\s*['"][^'"]*WorkbenchCodeEditor['"]\s*\)/);
    assert.match(src, /import\s*\(\s*['"][^'"]*WorkbenchMarkdownEditor['"]\s*\)/);
    assert.match(src, /import\s*\(\s*['"][^'"]*WorkbenchHtmlPreview['"]\s*\)/);
    assert.doesNotMatch(src, /import\s+\{\s*WorkbenchCodeEditor\s*\}/);
    assert.doesNotMatch(src, /import\s+\{\s*WorkbenchMarkdownEditor\s*\}/);
    assert.doesNotMatch(src, /import\s+\{\s*WorkbenchHtmlPreview\s*\}/);
  });

  it('MobileWorkbench loads heavy panels dynamically', () => {
    const src = readFileSync(resolve(webRoot, 'src/mobile/MobileWorkbench.tsx'), 'utf8');
    assert.match(src, /lazy\s*\(/);
    for (const panel of [
      'MobileTerminalPanel',
      'MobileFilesPanel',
      'MobileAutomationPanel',
      'MobileBrowserPanel',
      'MobileGitPanel',
      'MobileWorktreePanel',
      'MobilePromptPanel',
      'MobileSettingsPanel',
    ]) {
      assert.match(
        src,
        new RegExp(`import\\s*\\(\\s*['"][^'"]*${panel}['"]\\s*\\)`),
        `expected dynamic import for ${panel}`,
      );
      assert.doesNotMatch(
        src,
        new RegExp(`import\\s+\\{[^}]*\\b${panel}\\b[^}]*\\}\\s+from`),
        `static value import of ${panel} must not remain`,
      );
    }
    // 轻量 shell / 默认 panels 保持同步
    assert.match(src, /import\s+\{\s*MobileProjectPanel\s*\}\s+from/);
    assert.match(src, /import\s+\{\s*MobileAttentionPanel\s*\}\s+from/);
    assert.match(src, /import\s+\{\s*MobileWorkbenchShell\s*\}\s+from/);
  });

  it('Health keeps StatsChart local and App does not eager-import StatsChart', () => {
    const health = readFileSync(resolve(webRoot, 'src/pages/Health/Health.tsx'), 'utf8');
    const app = readFileSync(resolve(webRoot, 'src/App.tsx'), 'utf8');
    assert.match(health, /import\s+\{\s*StatsChart\s*\}\s+from\s+['"]\.\/StatsChart['"]/);
    assert.doesNotMatch(app, /StatsChart/);
    assert.match(app, /lazyNamed\s*\(\s*\(\)\s*=>\s*import\s*\(\s*['"]\.\/pages\/Health['"]/);
  });
});
