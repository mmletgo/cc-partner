# cc-partner

**Local-first 多设备项目工作台** — 在本机与局域网内，用桌面端、手机浏览器与无头后端一起推进同一份代码与自动化任务。

cc-partner 以 **Workbench** 为核心：本机与远端项目、worktree、终端、文件与 Git 在同一界面协作；手机可打开 Mobile Workbench；Orchestrator 负责自动编排并让执行过程可见；无 GUI 机器可用 headless 后端接入同一局域网。文件传输、截图、Prompt 与草稿本是围绕工作台的配套能力，而不是产品主叙事。

**固定局域网语义（无调用者身份校验）**：业务 API 不对 peer 做账号/配对/token 鉴权；合法 loopback/LAN 地址范围内的任何可达设备均可读取、写入和执行。系统不验证调用者身份。产品只有这一种局域网行为，不提供可切换的暴露/只读模式或逐设备权限。

**核心体验入口**：`/` 仍是 GitHub Trending（探索）；“继续工作”进 `/workbench`。GUI 首次启动 LAN listener 前会展示本机地址候选、首选端口 62116（占用递增）、mDNS UDP 5353 与无身份校验风险并要求确认。侧栏按 Explore/Work/Knowledge/Connect/System 分组；普通小字对比度 ≥4.5:1（`--fg-muted-readable`）。

## 功能一览

### 1. Local-first 多设备 Workbench

- 本机与局域网远端项目共用同一套 Workbench：项目、worktree、终端 session、文件树与 Git
- 无项目时只展示聚焦空态 CTA；有项目未选中时提供“继续工作”启动摘要
- 数据默认落在本机 `~/.cc-partner/`（可用 `CC_PARTNER_DATA_DIR` 绝对路径隔离）
- 桌面端通过 Tauri 本地 IPC 驱动；跨设备能力走局域网 P2P，不依赖公网中转

### 2. Mobile Workbench

- 桌面/后端 HTTP 服务提供 `/mobile` 浏览器入口（合法 LAN peer 无访问 token；同一可达网络任意设备可读写执行）
- 全局侧栏手机按钮展示可复制访问链接与二维码，并固定展示无身份校验风险提示
- 导航按 Projects / Attention / Work / Automation / More 分组；可进入 worktree、terminal、files、git、prompt 与自动化面板；远端项目经本机代理到 owning device

### 3. Orchestrator 自动编排与可见执行

- 在任务 worktree 与可见终端中运行自动化（Runner 现场可观察、可接管）
- 全局 Inbox（Attention）实时投影当前阻塞项：Human Review、Blocked、failed remote outbox、tmux 依赖问题
- Inbox 只负责导航到权威界面；具体动作（如 failed outbox 的 Retry/Discard、依赖安装）在对应面板完成

### 4. Headless 后端 CLI

远端或不想开桌面窗口时，独立二进制 `cc-partner-backend` 提供同一局域网服务与健康检查：

```text
cc-partner-backend start
cc-partner-backend status
cc-partner-backend doctor
cc-partner-backend doctor --json
cc-partner-backend stop
```

开发态可用：

```bash
cargo run --locked --bin cc-partner-backend -- start
cargo run --locked --bin cc-partner-backend -- status
cargo run --locked --bin cc-partner-backend -- doctor
cargo run --locked --bin cc-partner-backend -- doctor --json
cargo run --locked --bin cc-partner-backend -- stop
```

- `start` / `stop` / `status`：生命周期。`status` 输出本机运行态 JSON（不含控制 token）
- `doctor`：人类可读健康检查（状态、依赖、路径、日志位置）
- `doctor --json`：stdout **仅**单行机器可读 JSON 快照（`schemaVersion=1`）；tracing/错误说明写 stderr
- **doctor 退出码**：`healthy → 0`，`degraded → 1`，`unhealthy` 或检查无法完成 → `2`
- 正常 **stopped** 后端对 doctor 是信息态（healthy/0），不是错误
- 可选依赖缺失（如未装 tmux）通常是 degraded/1
- 诊断日志本地落盘：`~/.cc-partner/logs/backend.log`（及最多 3 个历史文件），**不上传**、无 telemetry

### 5. Agent-first 控制 CLI（`cc-partner`）

独立二进制 **`cc-partner`**（**不是** Tauri `externalBin`，不替代 `cc-partner-backend`）面向 Agent/脚本：

```text
cc-partner [--device local|id:<deviceId>] [--json] <resource> <action>
```

- 资源：`project` / `worktree` / `session` / `agent` / `task` / `experiment` / `attention` / `fleet` / `browser` / `event`
- 选择器仅 `id:` / `path:` / `branch:` 精确匹配；禁止 `active/current/recent/name` 与自动 remote 选择
- Prompt、terminal 正文、browser fill 只能 `--input-json -`（stdin）；不得进 argv/日志/错误 envelope
- `--json`：stdout 单一 envelope `{schemaVersion:1,ok,data|error}`；`event follow` 为 JSONL
- exit code：`0` 成功 · `1` 内部 · `2` 用法 · `3` 未找到 · `4` 冲突 · `5` 不可用/超时 · `6` 能力不支持 · `7` 部分结果
- 本机经 loopback control token；远端必须 `--device id:<deviceId>`，**不**发送 control token
- non-replayable mutation（如 `session send`）连接丢失 → `outcomeUnknown=true`，不盲重放

```bash
cargo run --locked --bin cc-partner-cli -- --json project list
cargo run --locked --bin cc-partner-cli -- --json attention list
printf '%s' '{"data":"pwd\\n"}' | cargo run --locked --bin cc-partner-cli -- session send --session id:<sid> --input-json -
```

细节见 [`docs/development/backend-operations.md`](docs/development/backend-operations.md) 与设计 `docs/superpowers/specs/2026-07-15-agent-first-cli-design.md`。

### 6. 配套工具：文件传输 · 截图 · Prompt · 草稿

- **局域网文件传输**：分块传输、断点续传、SHA256 校验、拖拽发送
- **区域截图**：全局快捷键框选，复制到剪贴板，可粘贴到 Claude Code
- **Prompt 管理与同步**：标签/搜索/复制；向量时钟跨设备合并
- **草稿本与其它本地工具**：围绕工作台的文本与协作辅助（见应用内面板）

另含：设备 mDNS 发现、应用内自动更新、CLAUDE.md 编辑与推送、SSH 配置管理、GitHub 周热门首页等。完整产品行为以 [`docs/prd.md`](docs/prd.md) 为准。

## 下载安装

前往 [Releases](https://github.com/mmletgo/cc-partner/releases) 下载对应系统包：

| 系统 | 文件 | 说明 |
|------|------|------|
| macOS (Apple Silicon / Intel) | `.dmg` | 按 CPU 架构分两个包 |
| Windows | `.exe`（NSIS）/ `.msi` | 安装程序 |
| Ubuntu / Linux | `.AppImage` / `.deb` | 直接运行或安装包 |

### macOS

1. 下载 `.dmg`，将 **cc-partner** 拖入 Applications
2. 若提示无法验证开发者：系统设置 → 隐私与安全性 → 「仍要打开」
3. 首次截图/全局快捷键：授予「屏幕录制」「输入监控」

### Windows

下载 `.exe` 或 `.msi`，按向导安装。Workbench 完整 tmux 上下文依赖 **默认 WSL 发行版内的 tmux**（见下方平台说明）。

### Linux

```bash
chmod +x cc-partner_*.AppImage
./cc-partner_*.AppImage

# 或
sudo dpkg -i cc-partner_*.deb
```

## 快速开始

### 桌面应用

安装后从系统菜单启动；托盘图标可打开主界面。程序会在后台拉起后端服务并搜索局域网设备。

### 从源码开发

```bash
git clone https://github.com/mmletgo/cc-partner.git
cd cc-partner

# 推荐：一键开发（检查 Node/Rust、按需 npm install、预构建 backend sidecar、tauri dev）
./start.sh
# macOS：自动用开发壳 cc-partner-dev.app（显示名「cc-partner (Dev)」、
# Bundle ID com.cc-partner.app.dev），与发布版在系统设置中分开展示/授权

# 或手动（裸 tauri dev 无开发壳；macOS 输入监控可能 fail-closed）
cd web && npm install
./node_modules/.bin/tauri dev
```

其它 `start.sh` 模式：`./start.sh build` 生产构建；`./start.sh web` 仅 Vite（无 Tauri / 无 invoke）；`./start.sh help`。

环境要求：Node.js 20+、Rust stable；macOS 需 Xcode CLT；Windows 需 MSVC + WebView2；Linux 需 webkit2gtk 等系统库（见常见发行版文档或 CI 安装步骤）。

### 无头后端

见上文 **Headless 后端 CLI**。打包后的 sidecar / PATH 中使用 `cc-partner-backend <cmd>`。端口、防火墙、路径与 `doctor` 细节见 [`docs/development/backend-operations.md`](docs/development/backend-operations.md)。

## 网络与架构

### 双通道（不要混用）

| 通道 | 机制 | 端口 |
|------|------|------|
| 本地桌面 UI ↔ Rust | Tauri `invoke()` / `#[tauri::command]` | **无前端本地 HTTP API 端口** |
| 跨设备 / 手机 P2P | axum HTTP + reqwest | 优先 TCP **62116**（占用则 +1） |
| 设备发现 | mDNS（`_cc-partner._tcp.local.`） | UDP **5353** |

实际监听端口以应用内移动端访问链接或 `GET /api/health` 的 `http_port` 为准，不是 OS 随机 `port=0` 的产品默认。

### 防火墙（手动示例，应用不会自动改防火墙）

若设备能发现但传输、Mobile Workbench 或打开远端项目失败，请确认同一局域网，并**手动**放行入站（建议限定 Private/Home/LAN profile）。应用不会自动修改主机防火墙。

| 用途 | 协议/端口 | 说明 |
|------|-----------|------|
| 设备发现 | UDP **5353** | mDNS（`_cc-partner._tcp.local.`） |
| P2P HTTP / Workbench / Mobile | TCP **实际端口**（首选 **62116**，占用则 +1） | 文件传输、同步、移动端与远端项目；以 `GET /api/health` 的 `http_port` 为准 |

**风险**：同一可达网络中的任何设备均可读取、写入和执行；系统不验证调用者身份。

**macOS**（系统防火墙通常按 App 放行；也可手动）：

```bash
sudo /usr/libexec/ApplicationFirewall/socketfilterfw --add /Applications/cc-partner.app
sudo /usr/libexec/ApplicationFirewall/socketfilterfw --unblockapp /Applications/cc-partner.app
```

**Windows**（管理员 PowerShell；端口请换成实际 `http_port` 如需要）：

```powershell
New-NetFirewallRule -DisplayName "cc-partner P2P HTTP" -Direction Inbound -Action Allow -Protocol TCP -LocalPort 62116
New-NetFirewallRule -DisplayName "cc-partner mDNS" -Direction Inbound -Action Allow -Protocol UDP -LocalPort 5353
```

**Ubuntu / Linux (ufw)**：

```bash
sudo ufw allow 62116/tcp comment 'cc-partner P2P HTTP'
sudo ufw allow 5353/udp comment 'cc-partner mDNS'
sudo ufw reload
```

验证：

```bash
curl http://<远端 IP>:<实际端口>/api/health
```

## 平台与依赖范围

### 产品支持的行为

| 能力 | 行为 |
|------|------|
| 桌面宿主 | macOS / Windows / Ubuntu（Tauri 2） |
| Workbench tmux | **macOS / Linux**：原生 `tmux`；**Windows**：默认 **WSL** 发行版内 `tmux` |
| tmux 不可用 | 可退回普通 PTY；**不承诺** window/pane 语义与重启后 shell 上下文恢复 |
| 数据与配置 | 默认 `~/.cc-partner/`；日志 `logs/backend.log`（5 MiB 轮转，最多 3 个历史） |

### 自动化验证 vs 产品行为

| 验证面 | 覆盖 |
|--------|------|
| Linux CI（`.github/workflows/ci.yml`） | 前端 lint/build/单测/e2e、Rust fmt/clippy/test |
| Cross-platform smoke（`.github/workflows/cross-platform-smoke.yml`） | macOS / Windows 上 backend CLI、doctor、native PTY 等 |

**Hosted runner 明确未验证（不得写成已由 smoke 证明）：**

- WSL + tmux
- GUI / WebView
- macOS 权限弹窗
- 多机 mDNS

Release 安装包由发版工作流产出，**不是**上述 smoke 的范围。完整本地命令 / CI 对照表见 [`docs/development/testing.md`](docs/development/testing.md)。

## 使用提示

### 第一次打开

托盘图标打开主界面；后台启动网络服务并发现设备。Workbench 需要 tmux 时，可在设置「依赖」页检测/安装预览；Inbox 会投影缺失类阻塞。

### 文件传输 / 截图 / Prompt

- 传输：同一局域网 → 选设备 → 选文件或拖拽；接收默认 `~/cc-partner-files/`（可改）
- 截图：全局快捷键 → 拖选 → 剪贴板；`ESC` 取消
- Prompt：新建/标签/搜索/复制；在线设备间按向量时钟合并

## 从源码构建与发版

### 生产构建（本地）

```bash
./start.sh build
# 或
cd web && npm install && ./node_modules/.bin/tauri build
```

正式发版前需准备 backend sidecar（CI 使用 `node scripts/prepare-tauri-sidecar.mjs`）。产物在 `src-tauri/target/release/bundle/`。

### CI

- **质量门禁** [`.github/workflows/ci.yml`](.github/workflows/ci.yml)：PR / `master` 推送（文档路径在 push 时可忽略）
- **跨平台 smoke** [`.github/workflows/cross-platform-smoke.yml`](.github/workflows/cross-platform-smoke.yml)：路径过滤 PR + 定时；见上文未覆盖项
- **发版** [`.github/workflows/release-tauri.yml`](.github/workflows/release-tauri.yml)：推送 `v*` tag

### 发版流程

1. 统一版本号（单一来源：`src-tauri/tauri.conf.json`）：

```bash
node scripts/bump-version.mjs <新版本号>
```

2. 提交并推送 tag：

```bash
git tag v<版本号>
git push origin v<版本号>
```

3. 发版工作流为 **三段式原生构建**（项目锁定的 Tauri CLI + sidecar 准备，见 workflow）：

   1. **build**：矩阵平台构建；先 `prepare-tauri-sidecar`，再 `tauri build`，收集安装包与 `.sig`
   2. **publish-release**：汇总 artifact 并发布 GitHub Release
   3. **assemble-latest-json**：从 Release 签名独立组装 `latest.json` 供应用内更新

细节、密钥与平台矩阵以 [`.github/workflows/release-tauri.yml`](.github/workflows/release-tauri.yml) 为准，本文不重复 secrets 历史。
## 常见问题

### 两台电脑互相看不到？

- 同一局域网；检查 UDP 5353 与实际 P2P TCP 端口
- VPN 可能干扰 mDNS

### 文件传输 / 手机 Workbench 失败？

- 磁盘空间与网络；防火墙是否按**实际端口**放行
- `curl http://<IP>:<port>/api/health` 是否可达

### 截图快捷键无效 / 截图空白？

- macOS：屏幕录制 + 输入监控权限
- 其它软件是否占用快捷键；可在设置中修改

### doctor 报 degraded？

- 多为可选依赖（tmux / WSL / git / Claude CLI 等）缺失；退出码 1 表示 degraded，不等于进程崩溃
- 日志：`~/.cc-partner/logs/backend.log`
- 退出码约定、JSON 快照与路径表见 [`docs/development/backend-operations.md`](docs/development/backend-operations.md)

## 技术栈

- **桌面宿主**: Tauri 2（Rust）
- **后端**: axum + reqwest + mdns-sd + sqlx (SQLite) + portable-pty / tmux + xcap + arboard
- **前端**: React 19 + TypeScript + Vite（`web/`）
- **打包/更新**: 锁定 Tauri CLI + tauri-plugin-updater + 三段式 release workflow

## 文档与开发

- 产品需求：[`docs/prd.md`](docs/prd.md)
- 质量门禁与平台矩阵：[`docs/development/testing.md`](docs/development/testing.md)
- 后端 CLI / 端口 / 日志 / doctor：[`docs/development/backend-operations.md`](docs/development/backend-operations.md)
- 仓库协作约定：[`AGENTS.md`](AGENTS.md)
- 前端指令：[`web/CLAUDE.md`](web/CLAUDE.md)
- 后端指令：[`src-tauri/CLAUDE.md`](src-tauri/CLAUDE.md)

## 开源协议

MIT License
