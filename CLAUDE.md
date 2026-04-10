# Claude Partner - 跨平台局域网协作工具

## 项目概述

支持 Mac/Windows/Ubuntu 三端的桌面工具，核心功能：
1. **局域网文件传输** - 任意大小文件的分块传输，支持断点续传
2. **区域截图** - 框选截图后保存到剪贴板，可直接粘贴到 Claude Code
3. **Prompt 管理** - 记录/复制/打标签/按标签筛选的文本管理
4. **P2P 自动互联** - 每个实例既是服务端也是客户端，局域网内 mDNS 自动发现
5. **Prompt 同步** - 基于向量时钟的跨设备 Prompt 数据同步
6. **自动更新** - 从 GitHub Releases 自动检测、下载和安装新版本

## 技术栈

- GUI: PyQt6（macOS 原生偏好设置面板扁平风格，实心纯色背景 + 图标标签栏 + 轻柔阴影）
- 异步网络: aiohttp + qasync (asyncio-Qt桥接)
- 设备发现: zeroconf (mDNS)
- 本地存储: SQLite + aiosqlite
- 打包: PyInstaller

## 代码结构

```
src/claude_partner/
├── app.py          → 应用入口和生命周期管理，见下方说明
├── config.py       → 配置管理，见下方说明
├── models/         → 数据模型，有独立 CLAUDE.md
├── storage/        → SQLite 存储层，有独立 CLAUDE.md
├── network/        → 网络通信层（mDNS发现 + HTTP API），有独立 CLAUDE.md
├── sync/           → Prompt 同步引擎，有独立 CLAUDE.md
├── transfer/       → 文件传输模块，有独立 CLAUDE.md
├── screenshot/     → 截图功能，有独立 CLAUDE.md
├── updater/        → 自动更新模块（GitHub Releases 版本检查/下载/安装），有独立 CLAUDE.md
└── ui/             → PyQt6 界面，有独立 CLAUDE.md
```

## 核心架构

### 应用入口 (app.py)
- 使用 qasync 将 asyncio 事件循环集成到 Qt 事件循环
- 启动顺序：数据库初始化 → HTTP 服务端 → mDNS 注册 → 同步引擎 → UI → 自动更新检查
- 关闭时反向清理所有资源

### 配置管理 (config.py)
- 设备 ID（UUID，首次运行生成并持久化）
- HTTP 服务端口（动态分配）
- 文件接收保存路径
- 数据库路径
- 配置存储在 ~/.claude-partner/config.json

### P2P 网络协议
- mDNS 服务类型: `_claude-partner._tcp.local.`
- 每个实例注册 mDNS 服务（含 device_id, name, port）
- 发现对端后通过 HTTP API 通信
- API 端点: /api/health, /api/sync/pull, /api/sync/push, /api/transfer/*

### Prompt 同步策略
- 每个 Prompt 携带向量时钟 {device_id: counter}
- 修改时递增本设备计数器
- 同步时比较向量时钟：严格领先 → 覆盖；并发 → LWW (Last-Writer-Wins)
- 触发时机：对端上线、本地修改(500ms防抖)、定时30秒

### 文件传输协议
- 分块 HTTP 传输（1MB/块）
- 流程：init(元数据) → chunk(分块) → verify(SHA256校验)
- 支持断点续传：接收端告知已接收 offset

### 自动更新
- 启动后延迟 3 秒首次检查，之后每 4 小时定时检查
- 托盘菜单"检查更新..."支持手动触发
- GitHub Releases API 获取最新版本，语义化版本比较
- 自动匹配当前平台下载资源（macOS DMG / Windows EXE / Linux tar.gz）
- 流式下载到 `~/.claude-partner/updates/`，支持进度显示和取消
- 三平台安装策略：macOS 挂载 DMG 替换 .app / Windows CMD 脚本替换 EXE / Linux shell 脚本替换可执行文件
- 版本号定义在 `src/claude_partner/__init__.py` 的 `__version__`

### 发版流程
- 更新 `src/claude_partner/__init__.py` 和 `pyproject.toml` 中的版本号
- 推送 `v*` 格式的 tag（如 `git tag v0.2.0 && git push origin v0.2.0`）
- GitHub Actions 自动构建三平台（macOS arm64/x86_64、Windows、Linux）并创建 Release
