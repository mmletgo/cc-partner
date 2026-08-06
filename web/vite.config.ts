import { defineConfig, type Plugin } from 'vite';
import react from '@vitejs/plugin-react';
import path from 'path';

/**
 * 从 HTML 中提取入口 stylesheet href（相对 dist 路径）。
 *
 * Business Logic（为什么需要这个函数）:
 *   首载预算必须计入入口 HTML 直接引用的 CSS；CSS 增长也要触发 CI 门禁。
 *
 * Code Logic（这个函数做什么）:
 *   扫描 `<link rel="stylesheet" href="...">`（属性顺序不限），去掉 leading `/`，
 *   忽略绝对 URL / data URL。
 *
 * @param {string} html
 * @returns {string[]}
 */
function extractEntryStylesheetHrefs(html: string): string[] {
  /** @type {string[]} */
  const hrefs: string[] = [];
  const linkTagRe = /<link\b[^>]*>/gi;
  let match: RegExpExecArray | null;
  while ((match = linkTagRe.exec(html)) !== null) {
    const tag = match[0];
    if (!/\brel\s*=\s*["']stylesheet["']/i.test(tag)) {
      continue;
    }
    const hrefMatch = tag.match(/\bhref\s*=\s*["']([^"']+)["']/i);
    if (!hrefMatch) {
      continue;
    }
    let href = hrefMatch[1].trim();
    if (!href || /^(?:https?:)?\/\//i.test(href) || href.startsWith('data:')) {
      continue;
    }
    href = href.replace(/^\//, '');
    if (href && !hrefs.includes(href)) {
      hrefs.push(href);
    }
  }
  return hrefs;
}

/**
 * 把构建产物 HTML 文件名映射到入口名。
 *
 * Business Logic（为什么需要这个函数）:
 *   Vite MPA 入口 HTML 是 index.html/mobile.html，合同按 main/mobile 命名预算。
 *
 * Code Logic（这个函数做什么）:
 *   index.html → main；mobile.html → mobile；其它 basename 去 .html 后若为 main/mobile 则采用。
 *
 * @param {string} fileName
 * @returns {string | null}
 */
function entryNameFromHtmlFileName(fileName: string): string | null {
  const base = fileName.split(/[/\\]/).pop() ?? '';
  if (base === 'index.html') {
    return 'main';
  }
  if (base === 'mobile.html') {
    return 'mobile';
  }
  const name = base.replace(/\.html$/i, '');
  if (name === 'main' || name === 'mobile') {
    return name;
  }
  return null;
}

/**
 * 写出可机器读取的 chunk graph 合同文件。
 *
 * Business Logic（为什么需要这个插件）:
 *   CI 需要在构建后验证 desktop/mobile initial graph 的 gzip 预算与 mobile 禁止依赖，
 *   因此构建产物必须自带 entries/chunks 图，而不是事后猜 chunk 名；预算含入口 HTML
 *   直接引用的 CSS，故合同同时写出 entryStyles。
 *
 * Code Logic（这个插件做什么）:
 *   在 generateBundle（enforce:post，尽量晚于 HTML CSS 注入）遍历 Rollup 输出：
 *   - chunk：fileName、entry facade、static imports、dynamicImports、moduleIds、codeBytes
 *   - html asset：解析 stylesheet href，按 main/mobile 写入 entryStyles
 *   写出 `.vite/cc-bundle-contract.json`。
 */
function ccBundleContractPlugin(): Plugin {
  return {
    name: 'cc-bundle-contract',
    // 晚于 Vite HTML 插件注入 CSS <link>，保证 entryStyles 完整
    enforce: 'post',
    generateBundle(_options, bundle) {
      /** @type {Record<string, string>} */
      const entries: Record<string, string> = {};
      /** @type {Record<string, object>} */
      const chunks: Record<
        string,
        {
          fileName: string;
          isEntry: boolean;
          name: string | undefined;
          facadeModuleId: string | null;
          imports: string[];
          dynamicImports: string[];
          moduleIds: string[];
          codeBytes: number;
        }
      > = {};
      /** @type {Record<string, string[]>} */
      const entryStyles: Record<string, string[]> = {};

      for (const [fileName, output] of Object.entries(bundle)) {
        if (output.type === 'chunk') {
          const moduleIds = Object.keys(output.modules ?? {});
          chunks[fileName] = {
            fileName,
            isEntry: Boolean(output.isEntry),
            name: output.name,
            facadeModuleId: output.facadeModuleId ?? null,
            imports: [...(output.imports ?? [])],
            dynamicImports: [...(output.dynamicImports ?? [])],
            moduleIds,
            codeBytes: Buffer.byteLength(output.code ?? '', 'utf8'),
          };

          if (output.isEntry && output.name) {
            entries[output.name] = fileName;
          }
          continue;
        }

        if (output.type === 'asset' && fileName.endsWith('.html')) {
          const entryName = entryNameFromHtmlFileName(fileName);
          if (!entryName) {
            continue;
          }
          const source =
            typeof output.source === 'string'
              ? output.source
              : Buffer.from(output.source as Uint8Array).toString('utf8');
          entryStyles[entryName] = extractEntryStylesheetHrefs(source);
        }
      }

      this.emitFile({
        type: 'asset',
        fileName: '.vite/cc-bundle-contract.json',
        source: `${JSON.stringify({ entries, chunks, entryStyles }, null, 2)}\n`,
      });
    },
  };
}

/**
 * Vite 配置（Tauri 版本）
 *
 * 迁移到 Tauri 后前端不再有任何本地 HTTP 调用：全部走 invoke() IPC，
 * 因此删除了 dynamicApiProxy 插件与读取 ~/.cc-partner/backend.port 的逻辑。
 *
 * 生产 sourcemap 默认关闭；仅 CC_PARTNER_SOURCEMAP=1 时生成 hidden map（无 sourceMappingURL），
 * 供受控 CI artifact 使用，不进入默认 release 包。
 */
export default defineConfig({
  plugins: [react(), ccBundleContractPlugin()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  server: {
    // 桌面 Tauri devUrl 与 backend 开发态 /mobile 反代共用本端口。
    // 手机扫码入口仍是 backend 端口（首选 62116）的 /mobile；HMR client
    // 默认使用页面 location.host/port，经 backend WebSocket 桥接到本 Vite。
    port: 5173,
    strictPort: true,
  },
  build: {
    outDir: 'dist',
    assetsDir: 'assets',
    sourcemap: process.env.CC_PARTNER_SOURCEMAP === '1' ? 'hidden' : false,
    rollupOptions: {
      input: {
        main: path.resolve(__dirname, 'index.html'),
        mobile: path.resolve(__dirname, 'mobile.html'),
      },
    },
  },
  css: {
    modules: {
      localsConvention: 'camelCase',
      generateScopedName: '[name]__[local]__[hash:base64:5]',
    },
  },
});
