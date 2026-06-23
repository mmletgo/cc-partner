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
- **API 模块**: `src/api/prompts.ts`、`githubTrending.ts`、`ccHistory.ts`、`claudeMd.ts`、`config.ts`、`devices.ts`、`transfer.ts`、`ssh.ts`、`health.ts` — 各资源方法调 `invoke('命令名', args)`，命令名对应 Rust `#[tauri::command]`，参数 camelCase。后端命令随 M1–M10 里程碑逐步落地，详见 `src-tauri/CLAUDE.md`。`githubTrending.ts` 封装首页 GitHub Trending 4 命令：`list` / `getConfig` / `updateConfig` / `testClaudeCli`（对应 `list_github_trending_repos` / `get_github_trending_config` / `update_github_trending_config` / `test_claude_cli`）；`config.ts` 的 `configApi` 还封装云端同步 4 命令：`getCloudSyncConfig` / `updateCloudSyncConfig(payload)` / `triggerCloudSync` / `testCloudSync`。
- **页面**: `src/pages/` — Home（GitHub Trending Weekly Top 25 项目列表，桌面双列瀑布流，奇偶排名分列以形成错落卡片流并避免按行 grid 留空；窄屏单列按原始顺序展示；卡片排名与标题同行且卡片自身高度不强制等高，展示原始简介 + 本地 Claude Code CLI 生成的中英文解说，仓库外链经 `@tauri-apps/plugin-opener` 打开系统浏览器）/ Prompts（列表管理，自定义多标签 + 动态标签筛选）/ CcHistory（从本地 Claude Code session 采集的用户输入 prompt，按项目分组的时间线，可搜索/复制/转存为 Prompt/删除；后端采集与同步见 `src-tauri/CLAUDE.md`「Claude Code 历史采集与同步」节）/ Scratchpad（速记本，纯内存临时记事）/ ClaudeMd（应用内编辑 user 级 `~/.claude/CLAUDE.md`，手动推送本机配置到局域网设备和 GitHub 云端，不拉取远端覆盖本机）/ Health（健康提醒状态页：当前相位/开关监测/暂停恢复/今日活跃统计 + app 排行图表 + 小时分布 + 完整配置表单）/ Settings（偏好中心：基本/快捷键/同步与存储/**云端同步**独立 Card、**GitHub Trending / Claude 解说**独立 Card，均有自己的测试/应用按钮，不混入底部统一 Save）/ Transfer / Devices / SSH（局域网设备 SSH 目标管理 + 用户名/端口配置 + 一键复制 ssh 命令 + 三端配置指南，配置跨设备同步）
- **路由**: `src/App.tsx` — React Router，`/` → Home，`/prompts` → Prompts，`/cc-history` → CcHistory，`/scratchpad` → Scratchpad，`/claude-md` → ClaudeMd，`/ssh` → SSH，`/health` → Health 等
- **品牌图标**: `src/assets/app-icon.png` 是应用内品牌图标（AppShell 侧栏与 Welcome 页共用），由根目录 `scripts/cc-partner-icon.png` 缩放生成；源 PNG 必须保留透明外圈，避免 Dock/托盘/侧栏出现多余白底；不要在组件里硬编码 `cc`/`CP` 文本 logo。Tauri 系统/打包图标仍由 `src-tauri/icons/` 提供。
- **区域截图选区页**: `src/pages/Screenshot/Overlay.tsx` — 独立于 AppShell/OnboardingGuard，路由 `/screenshot-overlay?display={i}`，由 Tauri 选区窗口（每屏一个透明置顶窗口）直接加载。**微信截图风格 + 三态状态机**（`mode: idle | selecting | editing`）：
  - **idle**：整屏半透明黑色遮罩；mousedown（左键）进 selecting 开始框选。
  - **selecting**：拖拽框选，四块遮罩挖洞 + 蓝色虚线边框（选区内清晰）；mouseup 有效选区（宽高≥10）进 editing。
  - **editing**：进编辑前先 `hiding=true` 隐藏遮罩/边框 + 双 rAF 等渲染透明，再调 `get_region_snapshot` 抓**纯桌面选区** PNG 作 canvas 背景底图（避免把蓝色边框/遮罩抓入快照）；canvas 用 `useAnnotationCanvas` hook（`src/pages/Screenshot/useAnnotationCanvas.ts`）重绘「快照底图 + 全部标注」+ `ScreenshotToolbar` 组件（`src/pages/Screenshot/ScreenshotToolbar.tsx`，矩形/箭头工具、6 色板 `COLORS`、撤销/确认/取消）；确认时 canvas.toDataURL 合成「桌面选区 + 标注」→ `save_clipboard_image` 写剪贴板（**所见即所得，Rust 不画标注**）；选区过小 / ESC / 右键 → cancel。窗口真透明（onMount 强制 html/body `background:transparent` 覆盖全局主题底色，否则白屏）。坐标用逻辑像素 + `window.devicePixelRatio` 传 Rust 换算物理像素。
  - **hooks 必须在 early return 之前**（项目规则 20）。详见 `src-tauri/CLAUDE.md` M6 节
- **健康提醒页**: `src/pages/Health/Health.tsx`（路由 `/health`，样式 `Health.module.css`）— 久坐监测状态展示 + 控制。后端 daemon 每分钟采样键鼠活跃度推进工作/休息状态机，连续工作达阈值触发久坐提醒（后端 daemon 与状态机见 `src-tauri/CLAUDE.md`「健康提醒」节）。`healthApi`(`src/api/health.ts`)封装命令（`get_health_config`/`get_health_status`/`toggle_health_enabled`/`toggle_health_paused`/`snooze_reminder`/`skip_reminder`/`update_health_config`/`get_activity_stats`/`get_activity_detail`）。页面并行取 status + stats + detail（startOfDay 起，startOfDay 取**本地当日 0 点**秒级 ts：`new Date(); d.setHours(0,0,0,0); floor(getTime()/1000)`，非 UTC 0 点），每 30s 轮询刷新；开关 enabled / 暂停 paused / 配置 update 走**乐观更新 + 失败回滚**（await 前记 prev、await 失败回滚到 prev + console.error，避免本地状态与后端不一致）；detail 经 `StatsChart`(`src/pages/Health/StatsChart.tsx`)用 recharts 渲染「app 使用时长 top8 横向柱状图」+「24 小时活跃分布纵向柱状图」（ResponsiveContainer + BarChart，layout="vertical" 实现横向柱）。**完整配置表单**经 `Settings`(`src/pages/Health/Settings.tsx`，样式 `Settings.module.css`)渲染——受控表单覆盖 `HealthConfig` 全部字段(enabled/workWindowSeconds/breakSeconds/notifyEnabled/reminderFullscreen/recordWindowTitle/waterEnabled/waterIntervalSeconds/dndStart/dndEnd/retainDays)，每字段 onChange 调 `update(patch)`：`setCfg({...cfg, ...patch})` + `updateConfig(完整对象)`，**绝不能只传部分字段**(后端整体覆盖式回写会清零未传字段，如 waterEnabled/reminderFullscreen)。分钟输入 workWindowSeconds/breakSeconds/waterIntervalSeconds ↔ 分钟整数 ×60 双向换算；dndStart/dndEnd 用 `<input type="time">`，空串 ↔ null。hooks 全部在 early return 之前（项目规则 20）。i18n namespace `health`（相位/状态/按钮/图表标题/配置项文案 noData 文案）。
- **健康提醒系统通知监听**: `App.tsx` 顶层挂 `HealthReminderListener`（与 `PermissionNeededListener` 同层、AppShell 之外）— 后端触发久坐提醒时 emit `health:reminder`（载荷 `{workWindowSeconds}`），本组件 `listen('health:reminder', ...)` 收到后用 `@tauri-apps/plugin-notification` 的 `sendNotification` 弹原生系统通知（标题/正文走 i18n `health:reminderTitle`/`health:reminderBody`，随当前语言切换）。
- **全屏健康提醒遮罩页**: `src/pages/HealthOverlay.tsx`（路由 `/health-overlay?display={i}`，独立于 AppShell/OnboardingGuard，与 `/screenshot-overlay` 同层）— 后端开启 `reminder_fullscreen` 后，久坐提醒 emit 后由 Rust `open_health_overlay` 每屏建一个透明置顶遮罩窗口直接加载本页。窗口真透明，onMount 强制 html/body `background:transparent` 覆盖全局主题底色（否则白屏，与截图 Overlay 同理）。页面用半透明黑色蒙层盖整屏，中央展示提醒文案 + 推迟 5/10 分钟 / 跳过按钮（i18n 复用 `health` namespace 的 reminderTitle/reminderBody/snooze5/snooze10/skip），点击调 `snooze_reminder`/`skip_reminder` + `close_health_overlay` 关闭全部遮罩窗口（推迟/跳过失败仍强关遮罩避免困住用户）。
- **应用内健康提醒 toast**（`src/pages/Health/ReminderToast.tsx` + `WaterToast.tsx`，共用样式 `HealthToast.module.css`）：常驻渲染于 `AppShell`（主窗口内，任意页面下都能弹），与系统通知/全屏遮罩互补，给用户一个应用内可直接操作的悬浮卡。`ReminderToast` listen `health:reminder` → 显示久坐提醒卡（推迟 5/10 分钟 `healthApi.snooze` / 跳过 `healthApi.skip`，操作后关闭）；`WaterToast` listen `health:water` → 显示喝水提醒卡（「已喝水」`healthApi.recordWater` 后关闭）。两组件 hooks（useTranslation/useState/useEffect）全部在 `if (!visible) return null` 之前（项目规则 20）。i18n 复用 `health` namespace（reminderTitle/reminderBody/snooze5/snooze10/skip/waterTitle/waterBody/drank）。
- **macOS 权限流程**: 首次启动 `OnboardingGuard`(`App.tsx`)检测权限未就绪 → **主动引导**：对未授权项调 `invoke('request_permission')`（screenCapture 传 `openSettings=false` 仅弹系统框、inputMonitoring 传 `true` 开设置面板）→ 跳 `/welcome`(`usePermissions` 轮询 `invoke('check_permissions')`，`PermissionCard` 点击触发同一授权流程；**后端命令 M7 实现**)；完成引导写 `localStorage cp-permission-onboarded`。截图快捷键/托盘触发时若屏幕录制未授权，后端 `start_region_capture` 不抓空白图，而是显示主窗口 + emit `screenshot:permission-needed`，前端顶层 `PermissionNeededListener`(`App.tsx`)监听后跳 `/welcome`。平时侧栏底部 `PermissionStatusBadge`(AppShell)常驻兜底，未授权时可点击触发同一授权流程；设置页「权限管理」Card(复用 `PermissionCard` + 共享 util `lib/permissionEntries.tsx` 的 `mapPermissions`，Welcome 与 Settings 共用避免重复)提供随时查看状态 / 单项重新授权的常驻入口。健康提醒键鼠采样需 macOS Accessibility 权限，复用同一权限流程（侧栏 `PermissionStatusBadge` / 设置页权限 Card 引导授权）
- **自定义 Hook**: `src/hooks/` — `useTheme`（浅/深主题切换与跨组件同步）、`useAppVersion`（应用版本号，统一经 `invoke('get_version')` 获取，**禁止前端硬编码版本号**，唯一权威来源是 `tauri.conf.json` 的 `version`）、`useLanguage`（中英文切换，复刻 useTheme 的 localStorage + 自定义事件同步范式）、`usePermissions`（macOS 权限状态轮询 + 请求授权，导出 `PERMISSION_ONBOARDED_KEY` 供 OnboardingGuard/Welcome 共享）
- **i18n**: `src/i18n/` — react-i18next 多 namespace（en/zh）；语言检测 localStorage(`cp-lang`) > `navigator.language` > en；切换器在 Sidebar 底部。**禁止在组件里硬编码用户可见中/英文字面量**，一律走 `src/i18n/locales/{en,zh}/<ns>.json` + `t('<ns>:<key>')`。详见下方「i18n 国际化」。

> 迁移到 Tauri 后已移除 Vite `dynamicApiProxy` 插件与 `~/.cc-partner/backend.port` 机制——前端走 invoke，无需 HTTP 代理。

## 打包部署

- `npm run build` 输出到 `dist/`，由 Tauri 打包嵌入应用（`tauri.conf.json` 的 `frontendDist=../web/dist`）
- 生产构建：仓库根 `./web/node_modules/.bin/tauri build`（产出 dmg/nsis/appimage 等）

## i18n 国际化

- **库**: react-i18next + i18next，初始化在 `src/i18n/index.ts`（`declare module` 类型扩展，`t()` 的 key 编译期校验，拼错即 tsc 报错）
- **命名空间**: `common`（动作/状态枚举/方向，跨页共享）、`nav`、各页面一个（home/prompts/ccHistory/transfer/devices/scratchpad/claudeMd/welcome/settings/ssh/health）
- **写法约定**（i18next v26 类型硬要求）:
  - 组件内 `const { t } = useTranslation([用到的所有ns数组]);`，所有 `t('ns:key')` **带 ns 前缀**
  - 模块级 helper 接收 `t: TFunction<'ns'>`，内部调用**省略 ns 前缀**
  - 插值 `{{var}}`；英文复数 en `{key}` + `{key}_other`，zh 只 `{key}`
- **切换/持久化**: `useLanguage` hook（localStorage `cp-lang` + `cp-lang-change` 自定义事件 + `storage` 事件跨标签/多窗口同步）；首次按系统语言推断
- **术语保留英文**: Prompt / GitHub / KB·MB 等在 en/zh 资源都保留英文
- **DesignSystem 页**: dev-only，不纳入 i18n（保持英文）
- **新增页面文案**: 在 `src/i18n/locales/{en,zh}/<页ns>.json` 加 key + 组件 `useTranslation(['<页ns>','common'])` + `t('<页ns>:key')`；改完 `npm run build`（tsc 校验 key）
