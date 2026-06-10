# web/ - React 前端

## 概述

基于 React + TypeScript + Vite 的前端界面，通过 fetch 调用后端 aiohttp HTTP API。

## 开发命令

- `npm run dev` — 启动 Vite 开发服务器（端口 5173）
- `npm run build` — 打包到 dist/
- `npm run lint` — ESLint 检查

## 架构

- **API 客户端**: `src/api/client.ts` — BASE_URL 为空字符串，使用 `window.location.origin` 构建请求 URL
- **API 模块**: `src/api/prompts.ts`, `src/api/config.ts` 等 — 各资源 RESTful 调用
- **页面**: `src/pages/` — Home（最近 Prompts）/ Prompts（列表管理）/ Settings / Transfer / Devices
- **路由**: `src/App.tsx` — React Router，`/` → Home，`/prompts` → Prompts 等

## Vite 代理（开发模式）

自定义 Vite 插件 `dynamicApiProxy` 将 `/api` 请求代理到后端：
- 每次请求读取 `~/.claude-partner/backend.port` 获取后端动态端口
- 使用 Node.js 内置 `http` 模块实现代理，无额外依赖

**重要**: 不能使用 `connect` 的 path-based mounting（`server.middlewares.use('/api', handler)`），
在 Vite 8 中该写法不会拦截 `/api` 请求。必须用 `server.middlewares.use((req, res, next) => { if (req.url?.startsWith('/api')) ... })` 手动检查 URL 前缀。

## 打包部署

- `npm run build` 输出到 `dist/`，后端通过 `HTTPServer.serve_static()` 提供静态文件服务
- 后端 SPA 回退路由 `/{path:.*}` 返回 `index.html`，支持前端路由刷新
