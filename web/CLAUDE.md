# web/ - React 前端

## 概述

基于 React + TypeScript + Vite 的前端界面，宿主为 **Tauri 2**，通过 `invoke()` IPC 调用 Rust 后端命令（`src-tauri/`）。迁移自 PyQt + aiohttp，前端已无任何本地 HTTP 调用。

## 开发命令

- `npm run dev` — 启动 Vite 开发服务器（端口 5173）
- `npm run build` — 打包到 dist/（tsc 类型检查 + vite 构建）
- `npm run lint` — ESLint 检查
- `npx playwright test tests/screenshot-overlay.spec.ts --project=chromium` — 截图 Overlay 工具条时序回归测试（会自动拉起 Vite）
- `npx tsx src/pages/Settings/HealthPanel.test.ts && npx tsx src/pages/Settings/shortcutRecorder.test.ts && npx tsx src/pages/Settings/settingsState.test.ts` — Settings 健康提醒布局/时间选择、快捷键录制与状态/payload 回归测试
- 完整开发（前端 + Rust）：仓库根 `./web/node_modules/.bin/tauri dev`（自动拉起 vite + cargo run + 热重载）

## 架构

- **API 客户端**: `src/api/client.ts` — 基于 `@tauri-apps/api/core` 的 `invoke` 薄封装；Rust 后端 reject 的错误（`{error:"消息"}`）经 `normalizeError` 规整为带 `message` 的 `Error`，无 HTTP status 概念
- **API 模块**: `src/api/prompts.ts`、`promptOptimizer.ts`、`scratchpad.ts`、`githubTrending.ts`、`ccHistory.ts`、`claudeMd.ts`、`config.ts`、`devices.ts`、`transfer.ts`、`ssh.ts`、`health.ts`、`workbench.ts` — 各资源方法调 `invoke('命令名', args)`，命令名对应 Rust `#[tauri::command]`，参数 camelCase。`workbench.ts` 按 projects / sessions / files 分组封装工作台本机项目、Claude Code PTY 会话和项目文件树命令；普通 Vite 浏览器调试环境缺 Tauri IPC 时，页面只显示友好不可用提示。`promptOptimizer.ts` 封装 `optimize_prompt(prompt)`，返回 `optimizedZh/optimizedEn`，不保存历史；`scratchpad.ts` 封装多页面速记本 7 命令：`listPages` / `getPage` / `createPage` / `updatePageContent` / `renamePage` / `deletePage` / `sync`（对应 `list_scratchpad_pages` / `get_scratchpad_page` / `create_scratchpad_page` / `update_scratchpad_page_content` / `rename_scratchpad_page` / `delete_scratchpad_page` / `sync_scratchpad`）。`githubTrending.ts` 封装 Github热门页 GitHub Trending 5 命令：`list` / `getConfig` / `getDefaultConfig` / `updateConfig` / `testClaudeCli`；`config.ts` 的 `configApi` 封装当前配置 `get`、偏好默认值 `getDefaults`、配置更新 `update`，以及云端同步 5 命令：`getCloudSyncConfig` / `getDefaultCloudSyncConfig` / `updateCloudSyncConfig(payload)` / `triggerCloudSync` / `testCloudSync`。
- **Settings AI tab**: 配置本机 Claude Code CLI。CLI 路径与模型同时影响 GitHub 项目解说和 PromptOptimizer；`aiEnabled` 与 `cacheTtlHours` 只作用于 GitHub 项目解说，PromptOptimizer 始终是单次调用且不缓存。
- **页面**: `src/pages/` — Home（GitHub Trending Weekly Top 25 项目列表，桌面双列瀑布流，奇偶排名分列以形成错落卡片流并避免按行 grid 留空；窄屏单列按原始顺序展示；卡片排名与标题同行且卡片自身高度不强制等高，卡片内部按头部/简介/Claude 解说/指标分区保留明确间距，展示原始简介 + 本地 Claude Code CLI 生成的中英文解说，仓库外链经 `@tauri-apps/plugin-opener` 打开系统浏览器）/ Prompts / CcHistory / Scratchpad / PromptOptimizer / ClaudeMd / ClaudeCodeAssets / Workbench（`/workbench` 项目工作台：入口来自全局左侧栏“设置”菜单项下方的 `WorkbenchProjectRail` 项目文件夹列表，不再占用一个独立主导航项；主区域按“工作台标题 + Claude Code sessions badge / 项目上下文 / 会话 tabs / 深色终端面板”组织，宽屏右侧检查器只显示当前会话状态和可交互项目文件夹，窄屏检查器排到首屏终端之后；文件树支持刷新、新建文件/文件夹、重命名、删除确认、复制相对路径和基础元信息，不做文件内容预览；项目请求回写前需比对当前 projectId，避免旧请求污染新项目 UI）/ Health / Settings / Transfer / Devices / SSH
- **路由**: `src/App.tsx` — React Router，`/` → Home，`/prompts` → Prompts，`/cc-history` → CcHistory，`/workbench` → Workbench，`/scratchpad` → Scratchpad，`/prompt-optimizer` → PromptOptimizer，`/claude-md` → ClaudeMd，`/ssh` → SSH，`/health` → Health 等
- **侧栏导航**: `AppShell` 顺序固定为 Github热门(`/`) → Prompt库 → Claude历史 → 速记本 → Prompt优化 → 文件传输 → CLAUDE.MD → Claude Code → 设备 → 健康提醒 → 设置；主导航不再提供“工作台”项。`WorkbenchProjectRail` 紧跟“设置”项下方，负责加载/添加/选择/移除项目文件夹，点击项目后选择当前项目并跳转 `/workbench`；项目状态由 `WorkbenchProjectsProvider` 共享给侧栏和 Workbench 页面。`/` 仍映射 Home 页面。
- **品牌图标**: `src/assets/app-icon.png` 是应用内品牌图标（AppShell 侧栏与 Welcome 页共用），由根目录 `scripts/cc-partner-icon.png` 缩放生成；源 PNG 必须保留透明外圈，避免 Dock/托盘/侧栏出现多余白底；不要在组件里硬编码 `cc`/`CP` 文本 logo。Tauri 系统/打包图标仍由 `src-tauri/icons/` 提供。
- **区域截图选区页**: `src/pages/Screenshot/Overlay.tsx` — 独立于 AppShell/OnboardingGuard，路由 `/screenshot-overlay?display={i}`，由 Tauri 选区窗口（每屏一个透明置顶窗口）直接加载。**微信截图风格 + 三态状态机**（`mode: idle | selecting | editing`）：
  - **idle**：整屏半透明黑色遮罩；mousedown（左键）进 selecting 开始框选。
  - **selecting**：拖拽框选，四块遮罩挖洞 + 蓝色虚线边框（选区内清晰）；mouseup 有效选区（宽高≥10）进 editing。
  - **editing**：mouseup 后立即进入 editing 并渲染 `ScreenshotToolbar`（矩形/箭头工具、6 色板 `COLORS`、撤销/确认/取消）与外侧 outline 选区框；工具条和选区框先完成首帧绘制，再调 `get_region_snapshot` 抓**纯桌面选区** PNG 作 canvas 背景底图（选区框画在 crop 外侧，选区外遮罩不进入 crop，避免快照捕获前闪烁/空白帧）；快照加载后 canvas 用 `useAnnotationCanvas` hook（`src/pages/Screenshot/useAnnotationCanvas.ts`）重绘「快照底图 + 全部标注」。确认时 canvas.toDataURL 合成「桌面选区 + 标注」→ `save_clipboard_image` 写剪贴板（**所见即所得，Rust 不画标注**）；选区过小 / ESC / 右键 → cancel。窗口真透明（onMount 强制 html/body `background:transparent` 覆盖全局主题底色，否则白屏）。坐标用逻辑像素 + `window.devicePixelRatio` 传 Rust 换算物理像素。
  - **hooks 必须在 early return 之前**（项目规则 20）。详见 `src-tauri/CLAUDE.md` M6 节
- **健康提醒页**: `src/pages/Health/Health.tsx`（路由 `/health`，样式 `Health.module.css`）— 状态监控页：标题说明 + 状态概览 Card + 今日活跃指标网格 + 活动统计 Card；头部「配置」按钮跳转 `/settings?tab=health`（**完整配置表单已迁移至设置页健康提醒 tab，由 `HealthPanel` 渲染，本页不再渲染配置表单**）。后端 daemon 每分钟采样键鼠活跃度推进工作/休息状态机，连续工作达阈值触发久坐提醒（后端 daemon 与状态机见 `src-tauri/CLAUDE.md`「健康提醒」节）。`healthApi`(`src/api/health.ts`)封装命令（`get_health_config`/`get_default_health_config`/`get_health_status`/`toggle_health_enabled`/`toggle_health_paused`/`snooze_reminder`/`skip_reminder`/`update_health_config`/`get_activity_stats`/`get_activity_detail`/`record_water`/`skip_water_reminder`/`snooze_water_reminder`/`close_health_overlay`）。页面并行取 status + stats + detail（startOfDay 起，startOfDay 取**本地当日 0 点**秒级 ts：`new Date(); d.setHours(0,0,0,0); floor(getTime()/1000)`，非 UTC 0 点），每 30s 轮询刷新；开关 enabled / 暂停 paused 走**乐观更新 + 失败回滚**（await 前记 prev、await 失败回滚到 prev + console.error，避免本地状态与后端不一致）。状态概览用 `Pill` + `ProgressBar` 表达 phase、监测开关、暂停/贪睡、当前工作进度、活跃/休息指标。detail 经 `StatsChart`(`src/pages/Health/StatsChart.tsx` + `StatsChart.module.css`)用 recharts 渲染「app 使用时长 top8 横向柱状图」+「24 小时活跃分布纵向柱状图」，图表颜色、tooltip、空态和容器样式必须走 design token。hooks 全部在 early return 之前（项目规则 20）。i18n namespace `health`（相位/状态/按钮/图表标题/配置项文案 noData 文案）。
- **Tauri 顶层事件监听**: `App.tsx` 顶层挂 `PermissionNeededListener` 与 `HealthReminderListener`；注册 `@tauri-apps/api/event.listen` 前必须先通过 `canListenToTauriEvents()` 检测 `window.__TAURI_INTERNALS__.transformCallback`，普通 Vite/Playwright 浏览器环境缺少 Tauri internals 时跳过注册，避免调试路由白屏。Tauri 桌面环境下照常监听截图权限与健康提醒事件。
- **健康提醒系统通知监听**: `App.tsx` 顶层 `HealthReminderListener`（与 `PermissionNeededListener` 同层、AppShell 之外）— 后端触发久坐提醒时 emit `health:reminder`（载荷 `{workWindowSeconds}`），本组件 `listen('health:reminder', ...)` 收到后用 `@tauri-apps/plugin-notification` 的 `sendNotification` 弹原生系统通知（标题/正文走 i18n `health:reminderTitle`/`health:reminderBody`，随当前语言切换）。
- **全屏健康提醒遮罩页**: `src/pages/HealthOverlay.tsx`（路由 `/health-overlay?display={i}&type=reminder|water`，独立于 AppShell/OnboardingGuard，与 `/screenshot-overlay` 同层）— 健康监测启用后，久坐/喝水提醒触发时由 Rust `open_health_overlay` 每屏建一个透明置顶遮罩窗口直接加载本页，建窗时按提醒类型带 `type` query。窗口真透明，onMount 强制 html/body `background:transparent` 覆盖全局主题底色（否则白屏，与截图 Overlay 同理）。页面用半透明黑色蒙层盖整屏，按 `type` 渲染：
  - **type=reminder**（默认）：久坐提醒文案 + 推迟 5/10 分钟 / 跳过 / **开始休息** 按钮（推迟/跳过复用 `close(snoozeMin?)`，调 `snooze_reminder`/`skip_reminder`）；点击「开始休息」进入 `resting` 态，从配置 `breakSeconds`（onMount 经 `healthApi.getStatus()` 读取 `HealthStatus.breakSeconds`）每秒倒数，到 0 自动 `skip_reminder` + 关闭遮罩；resting 态显示「休息中…」+ MM:SS 倒计时 + 「按 ESC 关闭」提示（无操作按钮）。
  - **type=water**：喝水提醒文案 + 「已饮水」(`record_water`) / 「跳过」(`skip_water_reminder`) / 「延迟 5 分钟」/「延迟 10 分钟」(`snooze_water_reminder {minutes}`) 按钮，每个动作 try/catch + closeOverlay。
  - **ESC**：`window.addEventListener('keydown')` 任意态（actions/resting）直接 `close_health_overlay`，不调业务命令。
  - i18n 复用 `health` namespace（reminderTitle/reminderBody/snooze5/snooze10/skip/startRest/resting/escToClose/waterTitle/waterBody/drank/skipWater/snoozeWater5/snoozeWater10）；模块级子组件用 `TFunction<'health'>` 接收 `t`，内部调用省略 ns 前缀。业务命令失败仍强关遮罩避免困住用户。hooks 全部在 early return 之前（项目规则 20）。
- **应用内健康提醒 toast**（`src/pages/Health/ReminderToast.tsx` + `WaterToast.tsx`，共用样式 `HealthToast.module.css`）：常驻渲染于 `AppShell`（主窗口内，任意页面下都能弹），与系统通知/全屏遮罩互补，给用户一个应用内可直接操作的悬浮卡。`ReminderToast` listen `health:reminder` → 显示久坐提醒卡（推迟 5/10 分钟 `healthApi.snooze` / 跳过 `healthApi.skip`，操作后关闭）；`WaterToast` listen `health:water` → 显示喝水提醒卡（「已喝水」`healthApi.recordWater` 后关闭）。两组件 hooks（useTranslation/useState/useEffect）全部在 `if (!visible) return null` 之前（项目规则 20）。i18n 复用 `health` namespace（reminderTitle/reminderBody/snooze5/snooze10/skip/waterTitle/waterBody/drank）。
- **macOS 权限流程**: 四条权限的真实消费者——屏幕录制（区域截图）、辅助功能（健康提醒活动窗口标题采样 active-win-pos-rs）、输入监控（健康提醒键鼠采样 device_query）、通知（系统通知，健康提醒久坐/喝水，@tauri-apps/plugin-notification）；全局快捷键（RegisterEventHotKey）无需任何 TCC 权限。首次启动 `OnboardingGuard`(`App.tsx`)检测三权限未全部就绪 → **仅 redirect 到 `/welcome`，不主动 request/openSettings**（避免首启自动弹出系统设置面板打扰用户）。Welcome 页 `usePermissions` 轮询 `invoke('check_permissions')`，权限卡片由 `mapPermissions`(`lib/permissionEntries.tsx`)渲染，用户点「去设置」逐项引导（screenCapture 弹原生框、accessibility/inputMonitoring 开对应设置面板）；完成引导写 `localStorage cp-permission-onboarded`。截图快捷键/托盘触发时若屏幕录制未授权，后端 `start_region_capture` 不抓空白图，而是显示主窗口 + emit `screenshot:permission-needed`，前端顶层 `PermissionNeededListener`(`App.tsx`)监听后跳 `/welcome`。平时侧栏底部 `PermissionStatusBadge`(AppShell)常驻兜底（三权限任一未授权即显示），点击触发批量授权；设置页「权限管理」Card(复用 `PermissionCard` + `mapPermissions`，Welcome 与 Settings 共用)提供随时查看状态 / 单项重新授权的常驻入口（**后端命令 M7 实现**）。通知权限为第 4 条，由前端 JS API 检测/请求（`lib/notification.ts` 的 `checkNotificationGranted`/`requestNotificationPermission`，macOS 调 `isPermissionGranted`/`requestPermission`，非 macOS 视为已授权），`usePermissions` 轮询合并进 `PermissionsStatus.notification`；**不阻塞「继续使用」**（`allGranted` 仍只看 3 个 TCC 权限），仅 macOS 引导
- **自定义 Hook**: `src/hooks/` — `useTheme`（浅/深主题切换与跨组件同步）、`useAppVersion`（应用版本号，统一经 `invoke('get_version')` 获取，**禁止前端硬编码版本号**，唯一权威来源是 `tauri.conf.json` 的 `version`）、`useLanguage`（中英文切换，复刻 useTheme 的 localStorage + 自定义事件同步范式）、`usePermissions`（macOS 权限状态轮询 + 请求授权：3 个 TCC 权限走 `configApi`、通知权限走 `lib/notification.ts` JS API 并合并进 `PermissionsStatus.notification`；`allGranted` 只看 3 个 TCC 权限不阻塞「继续使用」；导出 `PERMISSION_ONBOARDED_KEY` 供 OnboardingGuard/Welcome 共享）、`WorkbenchProjectsProvider` + `useWorkbenchProjects`（工作台项目列表与当前项目共享状态；Provider 文件只导出组件，Context/hook 放在 `workbenchProjectsContext.ts` 以满足 Fast Refresh）
- **i18n**: `src/i18n/` — react-i18next 多 namespace（en/zh）；语言检测 localStorage(`cp-lang`) > `navigator.language` > en；切换器在 Sidebar 底部。**禁止在组件里硬编码用户可见中/英文字面量**，一律走 `src/i18n/locales/{en,zh}/<ns>.json` + `t('<ns>:<key>')`。详见下方「i18n 国际化」。

> 迁移到 Tauri 后已移除 Vite `dynamicApiProxy` 插件与 `~/.cc-partner/backend.port` 机制——前端走 invoke，无需 HTTP 代理。

## 打包部署

- `npm run build` 输出到 `dist/`，由 Tauri 打包嵌入应用（`tauri.conf.json` 的 `frontendDist=../web/dist`）
- 生产构建：仓库根 `./web/node_modules/.bin/tauri build`（产出 dmg/nsis/appimage 等）

## i18n 国际化

- **库**: react-i18next + i18next，初始化在 `src/i18n/index.ts`（`declare module` 类型扩展，`t()` 的 key 编译期校验，拼错即 tsc 报错）
- **命名空间**: `common`（动作/状态枚举/方向，跨页共享）、`nav`、各页面一个（home/prompts/ccHistory/transfer/devices/scratchpad/promptOptimizer/claudeMd/welcome/settings/ssh/health/workbench）
- **写法约定**（i18next v26 类型硬要求）:
  - 组件内 `const { t } = useTranslation([用到的所有ns数组]);`，所有 `t('ns:key')` **带 ns 前缀**
  - 模块级 helper 接收 `t: TFunction<'ns'>`，内部调用**省略 ns 前缀**
  - 插值 `{{var}}`；英文复数 en `{key}` + `{key}_other`，zh 只 `{key}`
- **切换/持久化**: `useLanguage` hook（localStorage `cp-lang` + `cp-lang-change` 自定义事件 + `storage` 事件跨标签/多窗口同步）；首次按系统语言推断
- **术语保留英文**: Prompt / GitHub / KB·MB 等在 en/zh 资源都保留英文
- **DesignSystem 页**: dev-only，不纳入 i18n（保持英文）
- **新增页面文案**: 在 `src/i18n/locales/{en,zh}/<页ns>.json` 加 key + 组件 `useTranslation(['<页ns>','common'])` + `t('<页ns>:key')`；改完 `npm run build`（tsc 校验 key）
