# Updater 模块

## 概述

自动更新模块，从 GitHub Releases 检测、下载和安装新版本。支持 macOS/Windows/Linux 三平台。

## 文件说明

- `__init__.py` → 导出 UpdateChecker, UpdateInfo, UpdateDownloader, UpdateInstaller
- `checker.py` → 版本检查器（GitHub Releases API + 语义化版本比较）
- `downloader.py` → 异步下载器（流式下载 + 进度报告 + 取消支持）
- `installer.py` → 三平台安装器（DMG/EXE/TAR.GZ 自动替换重启）

## 版本检查 (UpdateChecker)

### 工作流程
1. GET `https://api.github.com/repos/mmletgo/claude-partner/releases/latest`
2. 解析 tag_name 为 SemanticVersion（纯标准库实现，不依赖 packaging）
3. 与当前 __version__ 比较（SemanticVersion.__gt__）
4. 从 assets 中匹配当前平台的下载文件（子串匹配 platform_suffix）
5. 发射 update_available / update_not_available / check_failed 信号

### SemanticVersion
- 解析 "v1.2.3" 或 "1.2.3" 格式，缺失段默认为 0
- 支持 __gt__ 和 __eq__ 比较

### 平台匹配关键字（与 build.py 命名一致）
- macOS arm64: "macos-arm64"
- macOS x86_64: "macos-x86_64"
- Windows: "windows-x86_64"
- Linux x86_64: "ubuntu-x86_64"
- Linux arm64: "ubuntu-aarch64"

### 配置
- API 超时: 15 秒
- 自动检查间隔: 4 小时 (UPDATE_CHECK_INTERVAL)
- 首次检查延迟: 3 秒（启动后）
- aiohttp session 懒初始化（复用 PeerClient 模式）

## 下载器 (UpdateDownloader)

### 工作流程
1. 创建 `~/.claude-partner/updates/` 目录
2. 以 `.downloading` 后缀创建临时文件
3. 流式读取远程文件，64KB chunk 分块写入
4. 实时报告进度百分比（已下载 / Content-Length）
5. 下载完成后原子重命名（去 .downloading 后缀）
6. 支持 cancel() 取消（下一个 chunk 检测标记）

### 配置
- 下载超时: total=600s, sock_read=120s
- Chunk 大小: 64KB
- 临时文件后缀: .downloading

## 安装器 (UpdateInstaller)

### macOS (.dmg)
1. `hdiutil attach -nobrowse` 静默挂载 DMG
2. 从挂载卷找到 .app → 删除 /Applications/ 旧版 → shutil.copytree 复制新版
3. `hdiutil detach` 卸载
4. `open` 启动新版本 → `os._exit(0)`

### Windows (.exe)
1. 写 .cmd 脚本到临时目录
2. 脚本逻辑: tasklist 循环等待 PID 消失 → copy /Y 覆盖 → start 启动 → del 自身
3. DETACHED_PROCESS 标志启动脚本 → `os._exit(0)`

### Linux (.tar.gz)
1. 解压到临时目录 → 找到 ClaudePartner 可执行文件
2. 写 shell 脚本: kill -0 等待 PID → cp 覆盖 → chmod +x → nohup 启动
3. start_new_session=True 启动脚本 → `os._exit(0)`

## 依赖
- `aiohttp` — HTTP 客户端（API 请求和文件下载）
- `PyQt6.QtCore` — QObject 基类和 pyqtSignal 信号
- `claude_partner.config.CONFIG_DIR` — 更新文件存放路径
