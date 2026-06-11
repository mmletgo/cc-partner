# UI 模块

## 概述

PyQt6 仅作为**宿主外壳**：通过 `QWebEngineView` 嵌入 `web/` React 前端渲染全部界面，再用 qasync 桥接 asyncio 与 Qt 事件循环。旧版 PyQt6 原生面板（MainWindow/PromptPanel/TransferPanel 等）已废弃移除。

界面交互（列表、搜索、筛选、同步、设置）全部由 React 前端通过 fetch 调用 aiohttp HTTP API 完成，本模块不再承载业务界面。

## 文件说明

- `__init__.py` → 导出 WebMainWindow, SystemTray
- `web_main_window.py` → 主窗口（QWebEngineView 嵌入 React 前端）
- `tray.py` → 系统托盘（右键菜单、双击显示窗口、设备计数提示）
- `theme.py` → Qt 全局样式表（深色/浅色跟随系统）+ 应用图标绘制
- `permissions.py` → macOS 权限状态检查（供 /api/permissions 端点调用）

## 主窗口 (WebMainWindow)

- QWebEngineView 加载前端：优先级 CP_FRONTEND_URL > Vite dev server(localhost:5173) > 后端静态资源 > 本地 dist
- 启用 LocalStorage / Javascript / 远程访问
- 关闭窗口只隐藏到托盘（`closeEvent`），由托盘菜单"退出"真正终止
- `force_close()` 用于应用退出时强制关闭
- React 前端通过 CSS prefers-color-scheme 自行适配深浅色，无需后端通知

## 系统托盘 (SystemTray)

- 图标：macOS 44x44 template image（黑色"CP"+setIsMask）；其他平台 64x64 蓝底白字
- 右键菜单：显示主窗口 / 截图 / 退出（"检查更新"已移除，前端设置页接管）
- 双击托盘 → show_window_requested 信号
- update_device_count(count) → 更新 toolTip"N 个设备在线"

## 权限检查 (permissions.py)

- `check_screen_capture_access() -> bool`：Quartz.CGPreflightScreenCaptureAccess 检查屏幕录制权限
- `check_input_monitoring_access() -> bool`：尝试创建 CGEventTap 检查输入监控权限
- 仅 macOS 打包环境真实检测，其他环境返回 True
- 被 app.py `_check_permissions_status` 调用，结果经 /api/permissions 返回前端设置页

## 依赖

- PyQt6 / PyQt6-WebEngine（QWebEngineView 渲染前端）
- qasync（asyncio-Qt 桥接）
- `claude_partner.config.AppConfig`
- `claude_partner.network.discovery.DeviceDiscovery`（设备计数）
- `claude_partner.screenshot.capture.ScreenshotManager`（托盘截图）
