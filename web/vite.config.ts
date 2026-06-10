import { defineConfig, Plugin } from 'vite';
import react from '@vitejs/plugin-react';
import path from 'path';
import os from 'os';
import fs from 'fs';
import http from 'http';

/**
 * 读取后端端口文件 ~/.claude-partner/backend.port
 * 文件不存在或内容无效时返回 null
 */
function readBackendPort(): string | null {
  const portFilePath = path.join(os.homedir(), '.claude-partner', 'backend.port');
  try {
    const content = fs.readFileSync(portFilePath, 'utf-8').trim();
    if (/^\d+$/.test(content)) {
      return content;
    }
    return null;
  } catch {
    return null;
  }
}

/**
 * 自定义 Vite 插件：动态代理 /api 请求到后端端口
 * 每次请求时重新读取端口文件，支持后端端口动态变化
 * 使用 Node.js 内置 http 模块，无需额外依赖
 */
function dynamicApiProxy(): Plugin {
  return {
    name: 'dynamic-api-proxy',
    configureServer(server) {
      // 不使用 connect 的 path-based mounting（Vite 8 行为变化），
      // 改为手动检查 URL 前缀，确保 /api 请求被拦截
      server.middlewares.use((req, res, next) => {
        if (!(req.url?.startsWith('/api'))) {
          return next();
        }
        const port = readBackendPort();
        if (!port) {
          res.statusCode = 502;
          res.end('Backend not started. Port file not found.');
          return;
        }

        const options: http.RequestOptions = {
          hostname: '127.0.0.1',
          port: parseInt(port, 10),
          path: req.url,
          method: req.method,
          headers: req.headers,
        };

        const proxyReq = http.request(options, (proxyRes) => {
          const statusCode = proxyRes.statusCode ?? 502;
          res.writeHead(statusCode, proxyRes.headers);
          proxyRes.pipe(res);
        });

        proxyReq.on('error', (err: Error) => {
          if (!res.headersSent) {
            res.statusCode = 502;
            res.end(`Backend proxy error: ${err.message}`);
          } else {
            next(err);
          }
        });

        req.pipe(proxyReq);
      });
    },
  };
}

export default defineConfig({
  plugins: [react(), dynamicApiProxy()],
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
    sourcemap: true,
  },
  css: {
    modules: {
      localsConvention: 'camelCase',
      generateScopedName: '[name]__[local]__[hash:base64:5]',
    },
  },
});
