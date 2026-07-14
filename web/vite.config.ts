import { defineConfig, type Plugin } from 'vite';
import react from '@vitejs/plugin-react';
import path from 'path';

/**
 * 写出可机器读取的 chunk graph 合同文件。
 *
 * Business Logic（为什么需要这个插件）:
 *   CI 需要在构建后验证 desktop/mobile initial graph 的 gzip 预算与 mobile 禁止依赖，
 *   因此构建产物必须自带 entries/chunks 图，而不是事后猜 chunk 名。
 *
 * Code Logic（这个插件做什么）:
 *   在 generateBundle 阶段遍历 Rollup chunk 输出，记录 fileName、entry facade、
 *   static imports、dynamicImports、moduleIds 与 raw code 字节数，写出
 *   `.vite/cc-bundle-contract.json`。
 */
function ccBundleContractPlugin(): Plugin {
  return {
    name: 'cc-bundle-contract',
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

      for (const [fileName, output] of Object.entries(bundle)) {
        if (output.type !== 'chunk') {
          continue;
        }

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
      }

      this.emitFile({
        type: 'asset',
        fileName: '.vite/cc-bundle-contract.json',
        source: `${JSON.stringify({ entries, chunks }, null, 2)}\n`,
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
