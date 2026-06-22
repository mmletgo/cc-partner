# web/ - React 前端

## 概述

基于 React + TypeScript + Vite 的前端界面，宿主为 **Tauri 2**，通过 `invoke()` IPC 调用 Rust 后端命令（`src-tauri/`）。迁移自 PyQt + aiohttp，前端已无任何本地 HTTP 调用。

## 开发命令

- `npm run dev` — 启动 Vite 开发服务器（端口 5173）
- `npm run build` — 打包到 dist/（tsc 类型检查 + vite 构建）
- `npm run lint` — ESLint 检查
- 完整开发（前端 + Rust）：仓库根 `./web/node_modules/.bin/tauri dev`（自动拉起 vite + cargo run + 热重载）

## 架构

- **API 客户端**: `src/api/client.ts` — 基于 `@tauri-apps/api/core` 的 `invoke` 薄封装；Rust 后端 reject 的错误（`{error:"消息"}`）经 `normalizeError` 规整为带 `message` 的 `Error`，无 HTTP status 概念
- **API 模块**: `src/api/prompts.ts`、`ccHistory.ts`、`claudeMd.ts`、`config.ts`、`devices.ts`、`transfer.ts` — 各资源方法调 `invoke('命令名', args)`，命令名对应 Rust `#[tauri::command]`，参数 camelCase。后端命令随 M1–M8 里程碑逐步落地，详见 `src-tauri/CLAUDE.md`。`config.ts` 的 `configApi` 还封装云端同步 4 命令：`getCloudSyncConfig` / `updateCloudSyncConfig(payload)` / `triggerCloudSync` / `testCloudSync`（对应 Rust `get_cloud_sync_config` / `update_cloud_sync_config` / `trigger_cloud_sync_cmd` / `test_cloud_sync`），类型 `CloudSyncConfig` / `CloudSyncResult` / `TestCloudSyncResult` 在 `lib/types.ts`
- **页面**: `src/pages/` — Home（最近 Prompts）/ Prompts（列表管理，自定义多标签 + 动态标签筛选）/ CcHistory（从本地 Claude Code session 采集的用户输入 prompt，按项目分组的时间线，可搜索/复制/转存为 Prompt/删除；后端采集与同步见 `src-tauri/CLAUDE.md`「Claude Code 历史采集与同步」节）/ Scratchpad（速记本，纯内存临时记事）/ ClaudeMd（应用内编辑 user 级 `~/.claude/CLAUDE.md`，手动同步到局域网设备）/ Settings（偏好中心：基本/快捷键/同步与存储/**云端同步**独立 Card，GitHub 私有仓库跨网络同步，有自己的测试/应用/立即同步按钮，不混入底部统一 Save）/ Transfer / Devices
- **路由**: `src/App.tsx` — React Router，`/` → Home，`/prompts` → Prompts，`/cc-history` → CcHistory，`/scratchpad` → Scratchpad，`/claude-md` → ClaudeMd 等
- **区域截图选区页**: `src/pages/Screenshot/Overlay.tsx` — 独立于 AppShell/OnboardingGuard，路由 `/screenshot-overlay?display={i}`，由 Tauri 选区窗口（每屏一个透明置顶窗口）直接加载。**微信截图风格 + 三态状态机**（`mode: idle | selecting | editing`）：
  - **idle**：整屏半透明黑色遮罩；mousedown（左键）进 selecting 开始框选。
  - **selecting**：拖拽框选，四块遮罩挖洞 + 蓝色虚线边框（选区内清晰）；mouseup 有效选区（宽高≥10）进 editing。
  - **editing**：进编辑前先 `hiding=true` 隐藏遮罩/边框 + 双 rAF 等渲染透明，再调 `get_region_snapshot` 抓**纯桌面选区** PNG 作 canvas 背景底图（避免把蓝色边框/遮罩抓入快照）；canvas 用 `useAnnotationCanvas` hook（`src/pages/Screenshot/useAnnotationCanvas.ts`）重绘「快照底图 + 全部标注」+ `ScreenshotToolbar` 组件（`src/pages/Screenshot/ScreenshotToolbar.tsx`，矩形/箭头工具、6 色板 `COLORS`、撤销/确认/取消）；确认时 canvas.toDataURL 合成「桌面选区 + 标注」→ `save_clipboard_image` 写剪贴板（**所见即所得，Rust 不画标注**）；选区过小 / ESC / 右键 → cancel。窗口真透明（onMount 强制 html/body `background:transparent` 覆盖全局主题底色，否则白屏）。坐标用逻辑像素 + `window.devicePixelRatio` 传 Rust 换算物理像素。
  - **hooks 必须在 early return 之前**（项目规则 20）。详见 `src-tauri/CLAUDE.md` M6 节
- **macOS 权限流程**: 首次启动 `OnboardingGuard`(`App.tsx`)检测权限未就绪 → **主动引导**：对未授权项调 `invoke('request_permission')`（screenCapture 传 `openSettings=false` 仅弹系统框、inputMonitoring 传 `true` 开设置面板）→ 跳 `/welcome`(`usePermissions` 轮询 `invoke('check_permissions')`，`PermissionCard` 点击触发同一授权流程；**后端命令 M7 实现**)；完成引导写 `localStorage cp-permission-onboarded`。截图快捷键/托盘触发时若屏幕录制未授权，后端 `start_region_capture` 不抓空白图，而是显示主窗口 + emit `screenshot:permission-needed`，前端顶层 `PermissionNeededListener`(`App.tsx`)监听后跳 `/welcome`。平时侧栏底部 `PermissionStatusBadge`(AppShell)常驻兜底，未授权时可点击触发同一授权流程；设置页「权限管理」Card(复用 `PermissionCard` + 共享 util `lib/permissionEntries.tsx` 的 `mapPermissions`，Welcome 与 Settings 共用避免重复)提供随时查看状态 / 单项重新授权的常驻入口
- **自定义 Hook**: `src/hooks/` — `useTheme`（浅/深主题切换与跨组件同步）、`useAppVersion`（应用版本号，统一经 `invoke('get_version')` 获取，**禁止前端硬编码版本号**，唯一权威来源是 `tauri.conf.json` 的 `version`）、`useLanguage`（中英文切换，复刻 useTheme 的 localStorage + 自定义事件同步范式）、`usePermissions`（macOS 权限状态轮询 + 请求授权，导出 `PERMISSION_ONBOARDED_KEY` 供 OnboardingGuard/Welcome 共享）
- **i18n**: `src/i18n/` — react-i18next 多 namespace（en/zh）；语言检测 localStorage(`cp-lang`) > `navigator.language` > en；切换器在 Sidebar 底部。**禁止在组件里硬编码用户可见中/英文字面量**，一律走 `src/i18n/locales/{en,zh}/<ns>.json` + `t('<ns>:<key>')`。详见下方「i18n 国际化」。

> 迁移到 Tauri 后已移除 Vite `dynamicApiProxy` 插件与 `~/.claude-partner/backend.port` 机制——前端走 invoke，无需 HTTP 代理。

## 打包部署

- `npm run build` 输出到 `dist/`，由 Tauri 打包嵌入应用（`tauri.conf.json` 的 `frontendDist=../web/dist`）
- 生产构建：仓库根 `./web/node_modules/.bin/tauri build`（产出 dmg/nsis/appimage 等）

## i18n 国际化

- **库**: react-i18next + i18next，初始化在 `src/i18n/index.ts`（`declare module` 类型扩展，`t()` 的 key 编译期校验，拼错即 tsc 报错）
- **命名空间**: `common`（动作/状态枚举/方向，跨页共享）、`nav`、各页面一个（home/prompts/ccHistory/transfer/devices/scratchpad/claudeMd/welcome/settings）
- **写法约定**（i18next v26 类型硬要求）:
  - 组件内 `const { t } = useTranslation([用到的所有ns数组]);`，所有 `t('ns:key')` **带 ns 前缀**
  - 模块级 helper 接收 `t: TFunction<'ns'>`，内部调用**省略 ns 前缀**
  - 插值 `{{var}}`；英文复数 en `{key}` + `{key}_other`，zh 只 `{key}`
- **切换/持久化**: `useLanguage` hook（localStorage `cp-lang` + `cp-lang-change` 自定义事件 + `storage` 事件跨标签/多窗口同步）；首次按系统语言推断
- **术语保留英文**: Prompt / GitHub / KB·MB 等在 en/zh 资源都保留英文
- **DesignSystem 页**: dev-only，不纳入 i18n（保持英文）
- **新增页面文案**: 在 `src/i18n/locales/{en,zh}/<页ns>.json` 加 key + 组件 `useTranslation(['<页ns>','common'])` + `t('<页ns>:key')`；改完 `npm run build`（tsc 校验 key）
