# Claude Partner

**跨平台局域网协作工具** - 专为 Claude Code 用户打造的多设备协作助手

Claude Partner 让你在局域网内的多台电脑之间**传文件**、**截图**、**管理和同步 Prompt**，一切自动完成，无需任何配置。

## 功能一览

### 局域网文件传输
在同一 Wi-Fi / 局域网下的电脑之间快速传输文件：
- 支持**任意大小**的文件
- 传输**断了能续传**，不用从头来
- 自动校验文件完整性（SHA256）
- 可以直接**拖拽文件**到窗口发送
- 实时显示传输进度

### 区域截图
一键框选屏幕区域截图，自动复制到剪贴板：
- 按快捷键即可触发截图
- 鼠标拖拽选择截图区域
- 截图自动复制到剪贴板
- 截完后直接 `Ctrl+V` 粘贴到 Claude Code

### Prompt 管理
集中管理你常用的 Prompt（提示词）：
- 创建、编辑、删除 Prompt
- 给 Prompt 添加标签分类
- 按标签筛选、文本搜索
- 一键复制 Prompt 到剪贴板

### 设备自动发现
打开就能用，零配置：
- 自动发现同一局域网内的其他 Claude Partner
- 实时显示在线设备
- 设备上下线有通知

### Prompt 跨设备同步
你的 Prompt 在所有设备上保持一致：
- 新设备上线自动同步
- 修改后实时推送到其他设备
- 基于向量时钟，不会丢数据

### 自动更新
有新版本时自动提示，一键完成升级：
- 启动后自动检查 GitHub Releases 是否有新版本
- 每 4 小时定时检查，也可通过托盘菜单手动检查
- 发现新版本后显示更新内容和下载进度
- 下载完成后一键安装并重启，无需手动操作

## 下载安装

前往 [Releases](https://github.com/mmletgo/claude-partner/releases) 页面下载对应你系统的版本：

| 系统 | 文件名 | 说明 |
|------|--------|------|
| Ubuntu / Linux (x86_64) | `ClaudePartner-ubuntu-x86_64.tar.gz` | 安装包（含安装脚本和图标） |
| macOS (Apple Silicon) | `ClaudePartner-macos-arm64.dmg` | DMG 安装镜像 |
| Windows (即将支持) | - | 开发中 |

### Ubuntu / Linux 安装步骤

1. **下载并解压**

   从 [Releases](https://github.com/mmletgo/claude-partner/releases) 页面下载 `ClaudePartner-ubuntu-x86_64.tar.gz`，然后解压：
   ```bash
   tar xzf ClaudePartner-ubuntu-x86_64.tar.gz
   cd ClaudePartner-ubuntu-x86_64
   ```

2. **运行安装脚本**

   安装脚本会将程序安装到 `/opt/claude-partner/`，并注册桌面图标：
   ```bash
   sudo bash install.sh
   ```

3. **启动程序**

   安装完成后，可以从应用菜单中找到并启动 **Claude Partner**，也可以通过命令行启动：
   ```bash
   /opt/claude-partner/ClaudePartner
   ```

### macOS 安装步骤

1. 从 [Releases](https://github.com/mmletgo/claude-partner/releases) 页面下载 `ClaudePartner-macos-arm64.dmg`
2. 双击打开 DMG 文件，将 **ClaudePartner** 拖入 Applications 文件夹
3. 首次打开时，如果提示"无法验证开发者"，前往 **系统设置 → 隐私与安全性**，点击"仍要打开"

## 使用指南

### 第一次打开

启动后，Claude Partner 会在系统托盘（屏幕右上角或右下角）显示图标。点击托盘图标即可打开主界面。

程序会自动：
- 在后台启动网络服务
- 搜索局域网内的其他 Claude Partner 设备
- 显示已发现的在线设备

### 传输文件

1. 确保两台电脑都打开了 Claude Partner，并且在**同一个局域网**（同一个 Wi-Fi）
2. 在「文件传输」面板中，从下拉框选择目标设备
3. 点击「选择文件」按钮，或者直接**拖拽文件**到窗口
4. 等待传输完成，接收方会收到通知
5. 接收的文件默认保存在 `~/claude-partner-files/`（可在设置中修改）

### 截图

1. 按下全局快捷键触发截图（可在设置面板中查看和修改快捷键）
2. 屏幕会变暗，用鼠标拖拽选择你想截取的区域
3. 松开鼠标后，截图自动复制到剪贴板
4. 在 Claude Code 或其他地方直接 `Ctrl+V` 粘贴
5. 按 `ESC` 可以取消截图

### 管理 Prompt

1. 在「Prompt」面板中点击「新建」按钮
2. 输入标题、内容，添加标签
3. 点击 Prompt 卡片上的复制按钮，一键复制内容
4. 用顶部的标签筛选或搜索框查找 Prompt
5. 如果有其他设备在线，Prompt 会自动同步过去

## 从源码运行

如果你想自己从源码运行或参与开发：

### 环境要求

- Python 3.11 或更高版本
- pip

### 安装步骤

```bash
# 克隆仓库
git clone https://github.com/mmletgo/claude-partner.git
cd claude-partner

# 创建虚拟环境（推荐）
python3 -m venv .venv
source .venv/bin/activate

# 安装依赖
pip install -e .

# 运行
claude-partner
```

### 自行构建可执行文件

```bash
# 安装开发依赖
pip install -e ".[dev]"

# 构建
python scripts/build.py
```

构建产物会输出到 `release/` 目录。

### 发版流程

1. 更新 `src/claude_partner/__init__.py` 和 `pyproject.toml` 中的版本号
2. 提交代码并推送 tag：
```bash
git add .
git commit -m "release v0.2.0"
git tag v0.2.0
git push origin master
git push origin v0.2.0
```
3. GitHub Actions 会自动构建三平台并创建 Release

## 工作原理

Claude Partner 使用 **P2P 架构**，每个实例既是服务端也是客户端：

1. **设备发现**：通过 mDNS 协议在局域网内自动广播和发现其他设备，无需手动输入 IP
2. **文件传输**：基于 HTTP 分块传输，每块 1MB，支持断点续传和 SHA256 完整性校验
3. **Prompt 同步**：使用向量时钟追踪版本，自动合并，冲突时以最后修改者为准（Last-Writer-Wins）

所有数据存储在本地 SQLite 数据库中，配置文件在 `~/.claude-partner/` 目录下。

## 常见问题

### 两台电脑互相看不到？
- 确认两台电脑在**同一个局域网**（同一个 Wi-Fi 或同一个路由器下）
- 检查防火墙是否阻止了 mDNS（UDP 5353 端口）和 HTTP 连接
- 如果使用了 VPN，可能会影响局域网发现，尝试断开 VPN

### 文件传输失败？
- 检查磁盘空间是否充足
- 确认网络连接稳定
- 程序支持断点续传，重新发送即可从断点继续

### 截图快捷键无效？
- 某些 Linux 桌面环境需要授予应用全局快捷键权限
- 检查是否有其他软件占用了相同的快捷键
- 可以通过设置面板修改快捷键

### 程序无法启动？
- Ubuntu 用户确保已安装必要的系统库：
  ```bash
  sudo apt install libxcb-xinerama0 libxcb-cursor0
  ```
- 确认已通过 `sudo bash install.sh` 正确安装

## 技术栈

- **GUI**: PyQt6
- **异步网络**: aiohttp + qasync
- **设备发现**: zeroconf (mDNS)
- **本地存储**: SQLite + aiosqlite
- **打包**: PyInstaller

## 开源协议

MIT License
