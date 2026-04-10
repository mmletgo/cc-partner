# UI 模块

## 概述

PyQt6 界面层，使用 Tab 布局组织多个功能面板。通过 qasync 桥接 asyncio 和 Qt 事件循环。

## 文件说明

- `__init__.py` → 导出 MainWindow, PromptPanel, WelcomeWindow
- `main_window.py` → 主窗口（QTabWidget 五个 Tab：Prompt 管理 / 文件传输 / 设备列表 / 速记本 / 设置）
- `prompt_panel.py` → Prompt 管理面板 + 编辑弹窗（PromptEditDialog）
- `transfer_panel.py` → 文件传输管理面板（设备选择、发送文件、传输任务列表、拖拽发送）
- `device_panel.py` → 设备列表面板（在线设备卡片列表、实时更新）
- `scratchpad_panel.py` → 速记本面板（临时 Markdown 笔记，退出清空，一键复制）
- `settings_panel.py` → 设置面板（设备名称、接收目录、快捷键配置）
- `tray.py` → 系统托盘图标（右键菜单、双击显示窗口、设备计数提示）
- `update_dialog.py` → 版本更新对话框（展示新版本信息、下载进度、安装操作）
- `welcome_window.py` → macOS 权限引导欢迎页（深色背景、权限卡片列表、定时轮询状态）
- `widgets/` → 可复用 UI 组件，有独立 CLAUDE.md

## 主窗口 (MainWindow)

- Tab 布局：Prompt 管理 | 文件传输（占位） | 设备列表（占位） | 速记本 | 设置
- 窗口标题: "Claude Partner"，默认 900x600，最小 600x400
- 配色：Apple 风格主题，蓝色系强调色，支持深色/浅色模式自动跟随系统，统一使用 theme.py 模块

## Prompt 管理面板 (PromptPanel)

### 布局
- 顶部工具栏：搜索框（300ms 防抖）+ 标签筛选下拉框 + 新建按钮
- 中间：QScrollArea 内使用 FlowLayout 网格布局展示 PromptCard 卡片（每行 2-3 张，根据窗口宽度自适应换行）
- 空状态提示

### 功能
- 搜索和标签筛选可以同时生效（先按标签筛选再匹配关键词）
- 标签筛选下拉框含 "全部标签" + 所有已有标签
- 新建/编辑通过 PromptEditDialog 弹窗
- 删除有确认对话框（软删除）
- 复制将内容写入系统剪贴板

### 异步集成
- 同步信号槽中使用 `asyncio.ensure_future()` 启动异步操作
- 所有 repo 调用都是 async/await

### PromptEditDialog
- 编辑模式：传入 Prompt 预填数据，vector_clock 递增本设备计数器
- 新建模式：prompt=None，生成新 UUID，vector_clock = {device_id: 1}

## 文件传输面板 (TransferPanel)

### 布局
- 顶部操作栏：目标设备下拉框（QComboBox）+ "发送文件" 按钮
- 中间：QScrollArea 传输任务列表
- 空状态提示（支持拖拽提示）

### 功能
- 目标设备下拉框：显示设备名(IP:端口)，userData 存 (device_id, base_url) 元组
- 发送文件：QFileDialog 选择文件 → asyncio.ensure_future 发起异步传输
- 拖拽发送：dragEnterEvent 检查 hasUrls()，dropEvent 获取文件路径并发送
- 传输任务列表：每项含方向图标、文件名、进度条、大小、状态、取消按钮
- 不同状态不同背景色（Apple 柔和色系）：传输中蓝色(#E8F0FE)、完成绿色(#E6F4EA)、失败红色(#FDE7E7)、取消橙色(#FEF7E0)
- 所有内联样式已替换为 theme 模块调用

### TransferItemWidget
- 文件大小自动选择 B/KB/MB/GB 单位
- QProgressBar 使用 theme.progress_bar_style()（Apple 蓝色渐变）
- 取消按钮使用 theme.button_danger_compact_style()
- 卡片带 apply_glass_shadow 玻璃阴影效果
- 终态（完成/失败/取消）自动隐藏取消按钮

### 信号连接
- FileSender: progress_updated, transfer_completed, transfer_failed
- FileReceiver: progress_updated, transfer_completed(带 saved_path), transfer_failed

## 速记本面板 (ScratchpadPanel)

### 布局
- 顶部标题行："速记本" 标题 + 字数统计
- 提示文字："临时记录你的想法，内容不会保存，退出应用时自动清空"
- 工具栏：复制全部按钮（主按钮样式）+ 清空按钮（危险按钮样式）
- 编辑区：QPlainTextEdit，等宽字体，支持 Markdown 格式书写

### 功能
- 内容仅存于内存，不持久化，退出应用自动清空
- 复制全部：一键复制编辑区全部内容到系统剪贴板（空内容不触发）
- 清空：弹出确认对话框后清空编辑区
- 实时字数统计

## 设备列表面板 (DevicePanel)

### 布局
- 标题行："在线设备" + 计数标签
- 可滚动的设备卡片列表

### DeviceCard
- 使用 theme.card_style() + apply_glass_shadow 玻璃阴影
- 在线状态指示灯（Apple Green #34C759 / 灰色圆点，border-radius 实现）
- 设备名称 + IP:端口（使用 theme 颜色常量）
- 在线/离线文字标签

### 功能
- add_device: 新设备上线添加卡片（已存在则更新）
- remove_device: 设备下线移除卡片
- 无设备时显示提示 "暂无发现其他设备，请确保在同一局域网"

## 欢迎引导页 (WelcomeWindow)

### 触发条件
- 仅 macOS 打包后（frozen）且权限缺失时显示
- 权限齐全 / 非 macOS / Terminal 运行时直接进入主窗口

### 布局
- 深色背景（#1D1D1F），无边框窗口，固定 480x520
- 居中布局：应用图标 → 标题 → 说明文字 → 权限卡片列表 → 弹簧 → 按钮
- 两个权限卡片：屏幕录制 + 输入监控
- 底部：主按钮"继续使用"（权限齐全后启用）+ 灰色"暂时跳过"

### 权限卡片 (PermissionCard)
- 每行：权限名 + 说明文字 + 状态圆点（绿✓/红●）+ "去设置"按钮
- 未授权：红色圆点 + "去设置"按钮（蓝色），点击打开系统设置
- 已授权：绿色圆点 + "已授权"文字，隐藏按钮

### 权限轮询
- QTimer 每 2 秒调用 check_screen_capture_access / check_input_monitoring_access
- 全部授权后发射 all_permissions_granted 信号，启用"继续使用"按钮

### 辅助函数
- `check_screen_capture_access() -> bool`：检查屏幕录制权限
- `check_input_monitoring_access() -> bool`：检查输入监控权限（CGEventTap）
- `request_screen_capture()`：调用 Quartz.CGRequestScreenCaptureAccess()
- `request_input_monitoring()`：打开系统设置输入监控页面
- `needs_welcome() -> bool`：判断是否需要显示欢迎页

### 信号
- `all_permissions_granted`：权限全部获得或用户点击"继续使用"
- `skip_requested`：用户点击"暂时跳过"

## 系统托盘 (SystemTray)

### 图标
- macOS：44x44px（22pt@2x Retina）template image，黑色 "CP" 文字 + `setIsMask(True)` 让系统自动适配明暗模式
- 其他平台：64x64 QPixmap，Apple 蓝(theme.ACCENT #007AFF)圆形背景，白色 "CP" 文字
- 不依赖外部图标文件

### 右键菜单
- 显示主窗口 → show_window_requested 信号
- 截图 → screenshot_requested 信号
- 检查更新... → check_update_requested 信号
- 分隔线
- 退出 → quit_requested 信号

### 交互
- 双击托盘图标 → show_window_requested 信号
- update_device_count(count) → 更新 toolTip "Claude Partner - N 个设备在线"

## 依赖
- `claude_partner.models.prompt.Prompt`
- `claude_partner.models.device.Device`
- `claude_partner.models.transfer.TransferTask, TransferStatus, TransferDirection`
- `claude_partner.storage.prompt_repo.PromptRepository`
- `claude_partner.config.AppConfig`
- `claude_partner.transfer.sender.FileSender`
- `claude_partner.transfer.receiver.FileReceiver`
- `claude_partner.ui.widgets` (FlowLayout, TagWidget, PromptCard)

## 版本更新对话框 (UpdateDialog)

### 状态机
- IDLE → DOWNLOADING → READY_TO_INSTALL
- DOWNLOADING → CANCELLED / FAILED
- FAILED → DOWNLOADING（重试）

### 布局
- QVBoxLayout（spacing=16, margins=24,24,24,24）
- 标题："新版本 {version} 可用" — theme.label_title_style()
- 更新内容（可选）："更新内容：" + QTextEdit（只读, max 120px）— theme.input_style()
- 文件大小："安装包大小: X.X MB" — theme.label_caption_style()
- 进度区域（初始隐藏）：QProgressBar + QLabel 状态文字
- 按钮行：次要按钮（稍后再说/取消）+ 主按钮（立即更新/安装并重启/重试）

### 信号
- download_requested() — 用户点击"立即更新"
- install_requested(str) — 用户点击"安装并重启"，参数为下载文件路径

### 外部接口
- show_download_state() — 切换到下载状态
- set_download_progress(float) — 更新进度 0.0~1.0
- set_download_completed(str) — 下载完成
- set_download_failed(str) — 下载失败
- get_download_url() → str — 获取下载 URL
- get_download_filename() → str — 获取下载文件名
