# Workbench Browser Preview Design

## Goal

Workbench 首版内置浏览器以“项目开发预览”为核心：用户在当前 worktree 里启动 dev server 后，可以在桌面 Workbench、远端 Workbench shortcut 和 `/mobile` 移动端中自动打开对应预览，不需要复制 URL 或手动区分本机/远端 `localhost`。

## Scope

首版覆盖：

- 桌面 Workbench 新增“预览/浏览器”工作区，与终端、文件预览同级。
- 移动端 `/mobile` 新增“预览”面板，支持本机项目和远端项目。
- 自动发现当前 worktree 的 dev server 地址。
- 本机、远端、移动端通过同一套后端预览代理访问 dev server。
- 支持刷新、重新检测、候选地址选择、手动兜底输入、外部浏览器打开。
- 支持 HTTP 静态资源、常规 SPA 路由和 dev server WebSocket/HMR。

首版不做：

- 通用网页浏览器能力，例如收藏夹、历史记录、多个任意网页 tab、下载管理、扩展插件。
- 任意公网/内网开放代理。
- 完整 DevTools 嵌入；调试仍可通过外部浏览器打开预览地址。
- 移动端编辑浏览器地址栏式自由浏览；移动端只面向当前项目预览。

## Product Behavior

用户在桌面 Workbench 中看到中心工作区入口：

- 终端
- 文件预览
- 预览

进入“预览”时：

1. 如果当前 `projectId + worktreeId` 已有记住的预览目标，直接创建代理 URL 并打开。
2. 否则读取当前 worktree 的终端输出，解析最近出现的本地 dev server URL。
3. 如果终端输出没有 URL，读取项目文件推断候选端口。
4. 后端在项目所在设备上探测候选地址。
5. 一个候选可用时自动打开；多个候选可用时展示选择器；无候选时展示手动输入。

远端项目语义：

- 远端终端里出现的 `http://localhost:5173` 指向远端设备，不指向本机。
- 本机桌面和手机端都不能直接打开这个 `localhost`。
- 本机 cc-partner 先识别 remote shortcut，再把发现/探测/代理请求转发给 owning device。
- owning device 访问自己的 `127.0.0.1:<port>`，再把响应流回本机或手机。

移动端语义：

- `/mobile` 主导航新增“预览”，位置建议在“终端”之后、“文件”之前。
- 移动端复用当前 active project/worktree。
- 移动端可以重新检测和刷新预览，但 UI 比桌面更克制，不提供复杂 tab 管理。

## Architecture

推荐架构是“后端预览代理 + 前端 iframe 宿主”，不采用 Tauri 子 WebView 作为首版主路径。

原因：

- 桌面、远端和移动端可以共享同一套代理 URL。
- 移动端普通浏览器无法使用 Tauri WebView API。
- 远端 dev server 的 `localhost` 需要在 owning device 上解析，代理天然适合这一点。
- iframe 加载同源代理 URL，避免目标 dev server 自身 CSP/X-Frame-Options 直接限制嵌入。

桌面路径：

```text
Workbench React Browser Workspace
  -> Tauri invoke: discover/create preview
  -> local axum HTTP route: /api/workbench/browser/proxy/:previewId/*
  -> local dev server or remote owning device
```

移动端路径：

```text
/mobile React Preview Panel
  -> HTTP: /api/mobile/workbench/browser/discover
  -> HTTP iframe URL: /api/mobile/workbench/browser/proxy/:previewId/*
  -> local device proxy
  -> local dev server or remote owning device
```

远端路径：

```text
Desktop or phone
  -> current device route
  -> remote-aware command/helper
  -> owning device /api/workbench/browser/*
  -> owning device 127.0.0.1:<dev-port>
```

## Backend Design

Add a new Workbench browser module under `src-tauri/src/workbench/`.

Suggested files:

- `src-tauri/src/workbench/browser.rs`
  - Discovery rules, terminal URL parsing, project file heuristics, target validation, probe logic.
- `src-tauri/src/workbench/browser_proxy.rs`
  - HTTP and WebSocket proxy handlers, response header filtering, proxy session registry.
- `src-tauri/src/workbench/browser_models.rs`
  - DTOs for preview targets, discovery results, proxy sessions and errors.

Add remote protocol DTOs in `src-tauri/src/workbench/remote_protocol.rs`.

Add commands in `src-tauri/src/commands/workbench.rs`:

- `discover_workbench_browser_preview(projectId, worktreeId?)`
- `create_workbench_browser_preview(projectId, worktreeId?, targetUrl)`
- `remember_workbench_browser_preview(projectId, worktreeId?, targetUrl)`
- `forget_workbench_browser_preview(projectId, worktreeId?)`

Add HTTP routes:

- `/api/workbench/browser/discover`
- `/api/workbench/browser/create`
- `/api/workbench/browser/proxy/:previewId/*path`
- `/api/mobile/workbench/browser/discover`
- `/api/mobile/workbench/browser/create`
- `/api/mobile/workbench/browser/proxy/:previewId/*path`

The `/api/workbench/browser/*` routes are the P2P/remote gateway routes. They must accept only local projects on the owning device, matching existing Workbench route guards. The `/api/mobile/workbench/browser/*` routes are remote-aware and may proxy to the owning device, matching existing mobile Workbench behavior.

## Discovery Rules

Discovery returns a structured result:

```ts
interface WorkbenchBrowserDiscovery {
  status: 'ready' | 'multiple' | 'empty' | 'error';
  rememberedTarget: WorkbenchBrowserTarget | null;
  candidates: WorkbenchBrowserTarget[];
  selectedTarget: WorkbenchBrowserTarget | null;
  message: string | null;
}
```

Candidate sources:

1. Terminal output/replay for current worktree sessions.
   - Parse `http://localhost:<port>`, `http://127.0.0.1:<port>`, `http://0.0.0.0:<port>`, LAN URLs, and common `Local:` / `Network:` output lines.
   - Prefer URLs found in sessions bound to the active worktree.
   - Prefer newest output over older output.

2. Project file heuristics.
   - `package.json` scripts: `vite`, `next dev`, `astro dev`, `nuxt dev`, `svelte-kit dev`, `webpack serve`, `vite --host`, `--port`, `-p`.
   - `.env`, `.env.local`, `.env.development`: `PORT=`, `VITE_PORT=`, `HOST=`.
   - Config defaults:
     - Vite: `5173`
     - Next.js: `3000`
     - Astro: `4321`
     - Vite preview: `4173`
     - Storybook: `6006`

3. Safe probe candidates.
   - Probe only loopback targets on the project owning device by default.
   - Normalize `localhost`, `0.0.0.0`, and `::1` to loopback access from the owning device.
   - Treat LAN URLs from terminal output as candidates only if their host matches the owning device or is user-confirmed.

Probe behavior:

- Short timeout, approximately 800-1500ms per candidate.
- `GET /` first; fallback to `HEAD /` only if needed.
- Accept HTTP status codes below 500 as “reachable” for preview.
- Keep enough metadata to show source: terminal, package script, env, known default, manual.

## Proxy Security

The preview proxy must not become an open proxy.

Required restrictions:

- Proxy sessions are created only through Workbench browser create/discover APIs.
- Each proxy session stores `projectId`, `worktreeId`, owning device, normalized target origin and creation time.
- Default allowed target origins are loopback hosts on the owning device: `127.0.0.1`, `localhost`, `[::1]`.
- Manual non-loopback target requires explicit user action and should be labeled as external/LAN.
- Proxy IDs are random, unguessable values.
- Proxy sessions expire after inactivity and extend on successful requests.
- Request paths are appended to the stored target origin; clients cannot pass arbitrary upstream URLs through query params.
- Strip or rewrite hop-by-hop headers.
- Avoid forwarding sensitive cc-partner auth/state headers to dev servers.
- Response headers that block iframe embedding, especially `X-Frame-Options` and restrictive `Content-Security-Policy`, should be removed or rewritten for the proxy response because the proxy is a trusted local preview surface.

WebSocket/HMR:

- Proxy `Upgrade: websocket` requests for the same preview session.
- Rewrite common HMR websocket URLs by preserving path and query under the same proxy origin.
- Keep this limited to the stored target origin.

## Frontend Design

Desktop:

- Extend `WorkbenchFileWorkspaceView` to include `'browser'`.
- Keep automation separate as today; do not model automation as workspace view.
- Add a browser layer alongside `terminalLayer` and `fileLayer`.
- Keep browser layer mounted when hidden so the iframe state is preserved during terminal/file switches.
- Add `WorkbenchBrowserWorkspace` domain component.
- Use existing `WorkbenchWorkspaceNav` for top controls.
- Add a centralized `BrowserIcon` or `GlobeIcon` to `web/src/lib/icons.tsx`.

Desktop browser workspace controls:

- Candidate/target selector.
- Current preview URL display as read-only text.
- Refresh iframe.
- Rediscover.
- Open externally.
- Manual URL/port entry when discovery fails.
- Return to terminal action.

Mobile:

- Extend `MobileWorkbenchPanel` with `'browser'`.
- Navigation order: projects, automation, terminal, browser, files, git, worktrees, prompt, settings.
- Add `MobileBrowserPanel`.
- Reuse HTTP transport, adding browser methods to `WorkbenchTransport` and `workbenchHttp.ts`.
- Mobile panel shows compact controls: status, refresh, rediscover, candidate select, iframe.

i18n:

- Add all visible strings under `web/src/i18n/locales/{zh,en}/workbench.json`.
- Do not hardcode Chinese or English labels in components.

React safety:

- All hooks stay before early returns.
- Browser iframe state should be keyed by preview session, not by every render, to avoid unnecessary reloads.

## Persistence

Remember the chosen target per project/worktree:

- Key: `projectId + worktreeId`.
- Store target origin and source metadata.
- For remote projects, remember against the local shortcut project/worktree IDs while preserving owning device identity.
- If a remembered target is no longer reachable, show stale status and offer rediscovery.

The proxy session itself should be in memory with TTL. Persistent storage should remember only the intended target, not a live proxy ID.

## Error Handling

Expected states:

- No active project.
- No active worktree.
- No terminal URL and no inferred candidates.
- Candidate found but not reachable.
- Multiple reachable candidates.
- Remote device offline.
- Proxy session expired.
- Dev server closed after preview opened.
- WebSocket proxy failed while HTTP page still loads.

UX rules:

- Detection failures are inline states, not blocking alerts.
- Remote offline should reuse existing remote offline language where possible.
- Manual entry is always available after detection failure.
- If an iframe request hits expired proxy session, the panel should offer “重新创建预览”.

## Tests

Rust unit tests:

- Terminal output URL parser handles Vite/Next/Astro/common localhost forms.
- Project file heuristics extract ports from package scripts and env files.
- Target validation rejects arbitrary upstream URLs by default.
- Remote gateway browser routes reject remote shortcut rows and accept local rows.
- Proxy session registry expires inactive sessions and does not expose arbitrary URL query proxying.

Frontend static/logic tests:

- `WorkbenchFileWorkspaceView` includes browser but still excludes automation.
- Desktop Workbench renders browser as workspace layer, not automation layer.
- Mobile panel order includes browser after terminal and before files.
- Browser transport has Tauri and HTTP adapters.
- i18n keys exist in zh/en.

Integration/manual verification:

- Local Vite project: run `npm run dev`, open preview, HMR works.
- Remote project: run dev server on remote device bound to localhost, open preview from desktop shortcut.
- Mobile: open `/mobile`, choose same project, open preview from phone.
- Stop dev server and confirm preview reports unreachable/retry state.

## Design Decision

Proceed with the preview proxy architecture for the first implementation. Tauri child WebView remains a possible later enhancement for desktop-only richer browser controls, but it should not be the core path because it would not solve mobile and would force a split implementation for remote projects.
