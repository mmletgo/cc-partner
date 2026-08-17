# cc-partner

**Local-first 多设备项目工作台** — 在本机与局域网内，用桌面端、手机浏览器与无头后端一起推进同一份代码与自动化任务。

cc-partner 以 **Workbench** 为核心：本机与远端项目、worktree、终端、文件与 Git 在同一界面协作；手机可打开 Mobile Workbench；Orchestrator 负责自动编排并让执行过程可见；无 GUI 机器可用 headless 后端接入同一局域网。文件传输、截图、Prompt、草稿本与健康提醒是围绕工作台的配套能力，而不是产品主叙事。

**固定局域网语义（无调用者身份校验）**：业务 API 不对 peer 做账号/配对/token 鉴权；合法 loopback/LAN 地址范围内的任何可达设备均可读取、写入和执行。系统不验证调用者身份。产品只有这一种局域网行为，不提供可切换的暴露/只读模式或逐设备权限。

**核心体验入口**：`/` 仍是 GitHub Trending（探索）；“继续工作”进 `/workbench`。GUI 首次启动 LAN listener 前会展示本机地址候选、首选端口 62116（占用递增）、mDNS UDP 5353 与无身份校验风险并要求确认。侧栏按 Explore/Work/Knowledge/System 分组；普通小字对比度 ≥4.5:1。

## 功能一览

### 1. Workbench 项目工作台（核心）

**项目管理**

- 本机与局域网远端项目共用同一套 Workbench：侧栏 Work 组项目入口、每项目终端 window/pane 统计；零项目时只展示聚焦空态 CTA，有项目未选中时提供“继续工作”启动摘要
- 添加远端项目时可浏览对端目录（home、桌面、文档、下载、常用代码目录等），自动建议项目名并一键在远端打开；远端项目经本机代理复用同一套 worktree / 终端 / 文件 / Git / 浏览器预览 / 自动化
- 数据默认落在本机 `~/.cc-partner/`（可用 `CC_PARTNER_DATA_DIR` 绝对路径隔离）；桌面端通过 Tauri 本地 IPC 驱动，跨设备能力走局域网 P2P，不依赖公网中转

**Git worktree 与提交**

- worktree 创建（`feature`/`fix`/`chore` 等前缀模板 + 后缀输入）、切换、移除；注入实时 `git status` 摘要（clean / dirty / conflict）
- **AI commit**：不手写 commit message 时由 Claude Code 按 staged diff 与项目上下文自动生成并提交；无可提交改动时静默刷新状态
- **一键合并**：功能 worktree 在隔离的临时 integration worktree 中 `--no-ff` 合并，冲突由 Claude CLI 自动解决（改动被强制限制在原冲突路径内），双父校验通过后才发布 main 并清理源分支；合并前自动关闭该 worktree 下所有终端
- **主 worktree collect-merge**：在主工作区直接把散落的未使用本地分支收集合并进主分支，不关终端、不删主工作区
- Git 提交树：`--topo-order` 提交 DAG 多 lane 图形，紧密相邻的提交行 + 本地/远端/tag badge

**终端（tmux）**

- Project → Worktree → Window → Pane 层级：前端 tab 即 tmux window、分屏即 pane；支持左右/上下分屏、切换/zoom pane、重命名、关闭、全屏与「适应尺寸」
- 应用重启后重新 attach 恢复原 window 的 shell / Claude 上下文；无 tmux 时回退普通 PTY（不承诺恢复语义）
- 断线/事件 Gap 后按 ring buffer replay 补历史；终端输入走流式通道，断线未确认输入不重放；tmux status bar 样式跟随应用浅/深主题

**⌘K Claude session 搜索**

- 终端页 ⌘K / Ctrl+K 打开 Command Palette：按标题与对话内容搜索当前 worktree 的 Claude Code 历史 session，预览最近对话，一键在新终端窗口 resume（本机与远端项目均支持；远端命令在对端执行）

**文件工作区**

- 文件树（刷新、新建文件/文件夹、重命名、删除确认、复制相对路径）+ 中心区文件 tab：代码高亮编辑（CodeMirror 按语言动态加载）、Markdown WYSIWYG / 源码 / 分屏、HTML 源码 / sandbox 预览 / 分屏（相对资源转 data URL）、CSV 只读表格、SQLite 只读表预览（不执行用户 SQL）、图片预览、JSON/TOML/YAML 格式化与保存前语义校验
- 文本保存用 baseHash 乐观锁；所有路径限制在 active worktree 根内，拒绝 `..` 与跨根 symlink；本机与远端项目均支持

**浏览器预览工作区**

- 自动发现项目所在设备的 loopback dev server（按「记住的目标 → 终端输出 → 项目配置 → 端口探测 → 手动输入」排序去重），仅允许 http(s) + 显式端口 + 回环地址
- 预览走不可预测 previewId 代理：HTTP 流式转发、重定向安全改写、WebSocket 桥接；iframe 强制 sandbox（不含 `allow-same-origin`，预览 JS 无法访问 cc-partner 同源 API）；远端项目预览经 owning device 中继且对浏览器保持同源
- **一键自动验证**：内置 managed Chromium 经 CDP 对当前 live preview 执行真实冒烟（页面截图、console 错误数、断言失败数），产出验证 evidence；不提供任意脚本/selector 输入

**工作台周边**

- Workbench 内 Prompt 优化浮层：可配置快捷键（默认轻按 Control 单键）与填入语种（中/英），优化文本由 Claude Code 加载当前 worktree 的 CLAUDE.md 上下文后流式写入运行中的终端
- 多屏卫星窗：最多 4 个独立 Workbench 卫星窗口，同一项目同时只属于一个窗口；工作区布局持久化，重启后安全恢复（safe-attach 只接管已存在的 tmux，不凭空新建）
- Agent 使用统计：仅元数据（不含 prompt/响应/终端字节/凭据），24h/7d/30d 汇总 + 最近会话列表，可一键清除；可经 P2P 汇总进 LAN Fleet
- LAN Fleet：Settings 只读聚合各远端项目的 Agent phase、Attention 计数、终端、Git 状态与 Orchestrator 运行/重试情况；侧栏项目卡片只显示异常 badge，不占二级入口

### 2. Mobile Workbench

- 桌面/后端 HTTP 服务提供 `/mobile` 浏览器入口（合法 LAN peer 无访问 token；同一可达网络任意设备可读写执行）
- 全局侧栏手机按钮展示可复制访问链接与二维码（多网段入口，标记 wifi/有线、默认出站 IP 优先），并固定展示无身份校验风险提示
- 导航双模式：全局按 Projects / Inbox / Tools / System 分组，进入项目后切换为 Workbench 工具 + Shortcuts；可进入 worktree、terminal、browser、files、git、prompt 与自动化面板
- 手机可作为二级代理管理远端项目（手机 → 本机 → 远端设备）：终端（真实 xterm + replay + pane 操作 + 全屏 overlay + 软键盘适配）、worktree 快速切换 bottom sheet、Git 提交/推送/合并、文件浏览保存、Prompt 优化流式入终端、自动化看板与 outbox 处理

### 3. Orchestrator 自动编排与可见执行

- 看板式任务管理：Backlog / Todo / InProgress / Rework / HumanReview / Done 泳道（仅相邻泳道拖拽）；创建任务可选「创建到 Backlog / 创建到 Todo / 创建并启动」，支持 AI 把简单 Prompt 完善为标题/目标/验收标准
- **WORKFLOW 文档**：项目根 `WORKFLOW.md`（YAML front matter）可覆盖 Prompt 模板、验证命令、Runner 超时与活跃泳道；应用内提供模板预览、校验与 CAS 保存（本机、远端代理、Mobile 均可）
- **可见 Runner**：任务在确定性路径的专用 worktree（`agent/<task-id>-<slug>` 分支）内以 Workbench 可见终端运行 Claude，现场可观察、可接管；终端中出现 `ORCHESTRATOR_DEV_DONE` 哨兵行即进入验证阶段
- **验证 evidence 与自动修复**：逐条执行项目/全局验证命令写入 evidence；Claude verifier 评审冻结的 diff 快照，不通过自动进入修复循环（复用同一 worktree 现场），基础设施故障则 Blocked
- **full-auto 交付**：Settings 四开关（自动 commit / push 任务分支 / merge main / push main）全开时验证通过后自动交付；任一关闭则停在 Human Review 等用户复核
- **候选实验（experiments）**：显式开启后一次创建 2–8 个候选任务并行跑同一目标；硬门通过后唯一高分自动胜出交付，平局/零候选产生 NeedsDecision 等人工批准；失败候选绝不 commit/push/merge
- **远端任务与 outbox**：remote shortcut 项目上的任务经本机代理到远端；远端离线时创建进入 pending outbox，恢复后自动补发；失败项可人工 Retry/Discard
- **运营通知**：任务进入 HumanReview/Blocked/Done、outbox 失败、Agent 需要输入/失败、实验待决策时发出隐私安全的系统通知（不含任务标题/目标/diff）
- 运行时快照状态条：调度器开关、workflow 来源、槽位占用、运行中/重试任务与最近事件；远端项目呈 live/offline/unsupported/unavailable 四态

### 4. Attention 全局 Inbox

- `/attention` 实时聚合所有需要人介入的事项：Human Review、Blocked 任务、failed remote outbox、tmux 依赖缺失、Agent 需要输入/失败、实验待决策、Agent Hub 冲突/投影阻塞
- 条目按「需要你的决定 / 运行受阻 / 环境受阻」分组；Inbox 只负责导航到权威界面，具体动作（Retry/Discard、依赖安装、Deliver 等）在对应面板完成；无已读/稍后/忽略状态
- 桌面 Tauri 命令与 Mobile `/api/mobile/attention` 共用聚合；远端条目离线时回退最近镜像并标注缓存

### 5. Agent Hub 多 CLI 指令与资产

- `/agent-hub` 统一管理本机用户级、局域网远端设备与项目级 Claude / Codex / OpenCode 指令和可移植资产（旧 `/claude-md` 页重定向至此）
- 提示词固定**公共 / 适配 / 独有**三槽编辑（canonical 文档带修订历史与 CAS 保存）；合成预览按 公共→适配→独有 顺序渲染
- **AI 辅助提示词修改**（调用本机 Claude CLI）：原始指令「分析拆解」为三槽；把当前 agent 的适配正文「适配到其他 Agent」；按用户给定方向改写当前槽并保存 Canonical——均不自动写入 Agent 原始文件
- **可移植资产**：Skill / Command / Agent / MCP 的扫描、enable/disable/uninstall/恢复（物理移动文件，资产启停不漂移）；行内动作组 + 详情 Drawer 预览确认流程
- **LAN Pull / Snapshot Push**：从局域网对端同目标 Agent 拉取资产（清单对比、8MiB 分块断点续传、冲突可预览替换）；快照导出为可读目录归档并原子导入；多目标 LAN 推送失败进 Attention
- **Git device-lane 备份**：Agent Hub 资产经 Cloud Sync 写入 `agent-hub/devices/<deviceId>/`；拉取远端 lane 后需 inspect/preview/confirm 才导入，不自动 import
- 后台 Agent Hub 服务支持登录自启动与崩溃监督重启（`supervise`）

### 6. 配套工具：传输 · 截图 · Prompt · 速记本 · Claude 历史

- **局域网文件传输**：分块传输、全文 SHA256 校验、断点续传（对端支持 `transfer.resume.v1` 能力时）、取消、幂等对账（clientOperationId，丢 ACK 不重复落盘）；接收端重名自动加 `(1)` 后缀不覆盖；同设备桌面可在完成后「打开 / 在文件夹中显示」
- **区域截图**：全局快捷键（可配置）触发，每屏透明置顶选区窗口，微信截图风格三态交互（框选 → 编辑 → 确认）；编辑工具条支持矩形/箭头标注、6 色色板、撤销；确认后把「桌面选区 + 标注」合成 PNG 写入剪贴板，可直接粘贴到 Claude Code；`ESC`/右键取消
- **Prompt 管理与优化**：标签/搜索/复制、版本历史与恢复；独立 Prompt 优化页调用本机 Claude Code CLI 生成中英文优化版 Prompt；优化结果只用于展示复制，不入库不同步
- **速记本**：多页面自动保存文本，页面标题、版本历史、局域网与 GitHub 同步
- **Claude 历史**：后台自动采集本机 Claude Code / Codex / OpenCode 的用户输入 prompt，按 Git 主项目路径归组（含已删除 worktree 历史迁移）；支持设备筛选、项目列表、搜索、详情、删除与一键转存 Prompt 库
- **GitHub Trending 首页**：Weekly Top 25 双列瀑布流；可选启用本地 Claude CLI 生成中英文仓库解说（可配置 CLI 路径/模型/缓存 TTL）

### 7. 健康提醒

- **久坐监测**：每分钟采样前台键鼠活跃度，连续工作达阈值（默认 45 分钟）触发提醒（macOS 需辅助功能权限，未授权自动降级不崩溃）
- **喝水提醒**：按间隔（默认 1 小时）提醒，支持「已饮水」记录、跳过（不污染统计）与延迟
- **全屏休息遮罩**：每屏透明置顶遮罩（久坐/喝水两种文案），多屏同步；遮罩上「开始休息」进入倒计时（多屏同步、到点自动记录并关闭），中途退出不记录
- 系统通知（多语言，独立开关）+ 应用内悬浮卡；免打扰时段（可跨午夜）、手动暂停/恢复（托盘菜单可达）、贪睡、跳过
- **习惯统计**：今日饮水/休息次数与总休息时长 + 近 7 天 sparkline；活跃/闲置分钟、应用使用时长排行、窗口标题排行、24 小时活跃分布；明细按保留天数（默认 90 天）自动清理

### 8. 数据与同步

- 本地 SQLite（`~/.cc-partner/data.db`，WAL）为权威源；自动从旧 `~/.claude-partner` 目录迁移
- **向量时钟 CRDT 多设备同步**：Prompt 库、SSH 目标、速记本、Claude 历史四域跨设备合并（严格领先覆盖、并发 LWW、确定性 tie-break）；Settings 同步页按设备/领域展示 Succeeded/Partial/Unreachable
- **版本历史与冲突保留**：并发编辑的败方与编辑前版本入 `content_versions`（历史约 20 版/30 天，冲突至少 30 天）；Prompt 与速记本页面可查看并恢复历史版本（恢复为新版本，不覆盖旧历史）
- **可验证备份导出/恢复**：ZIP 导出带 checksum 与 zip-slip 校验；恢复前 inspect 预览、按域选择 Merge（向量时钟合并，冲突保留副本）或整域替换，恢复前自动保留回滚点、可一键 rollback
- **GitHub 私有仓库云同步**：把私有仓库当中心化对端同步四域数据（含软删除）；可配置仓库/分支/间隔（默认 600s）与自动开关，支持连接测试；多设备冲突语义与局域网一致
- **SSH 目标管理**：保存每个连接目标的用户名/端口/备注并跨设备同步，按操作系统渲染配置指南

### 9. Headless 后端 CLI

远端或不想开桌面窗口时，独立二进制 `cc-partner-backend` 提供同一局域网服务与健康检查：

```text
cc-partner-backend start
cc-partner-backend status
cc-partner-backend doctor
cc-partner-backend doctor --json
cc-partner-backend stop
cc-partner-backend supervise
```

开发态可用：

```bash
cargo run --locked --bin cc-partner-backend -- start
cargo run --locked --bin cc-partner-backend -- status
cargo run --locked --bin cc-partner-backend -- doctor
cargo run --locked --bin cc-partner-backend -- doctor --json
cargo run --locked --bin cc-partner-backend -- stop
cargo run --locked --bin cc-partner-backend -- supervise
```

- `start` / `stop` / `status`：生命周期。`status` 输出本机运行态 JSON（不含控制 token）
- `supervise`：登录自启动监督入口——spawn `serve` 并在异常退出后按 1→60s 指数退避重启，正常 `stop`（exit 0）后不再拉起
- `doctor`：人类可读健康检查（状态、mDNS、可选依赖 git/tmux/WSL/Claude CLI、路径、日志位置），并固定打印局域网无鉴权风险提示；输出全程隐私脱敏
- `doctor --json`：stdout **仅**单行机器可读 JSON 快照（`schemaVersion=1`）；tracing/错误说明写 stderr
- **doctor 退出码**：`healthy → 0`，`degraded → 1`，`unhealthy` 或检查无法完成 → `2`
- 正常 **stopped** 后端对 doctor 是信息态（healthy/0），不是错误
- 可选依赖缺失（如未装 tmux）通常是 degraded/1
- 诊断日志本地落盘：`~/.cc-partner/logs/backend.log`（5 MiB 轮转，最多 3 个历史文件），**不上传**、无 telemetry

### 10. Agent-first 控制 CLI（`cc-partner`）

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

## 桌面集成

- **系统托盘**：模板图标自动适配深浅色；菜单含显示主窗口、截图、暂停/恢复健康监测、退出；左键单击显示主窗口
- **全局快捷键**：截图快捷键可在设置中修改（热更新）；Workbench Prompt 优化快捷键为页面内偏好（默认轻按 Control 单键）
- **自动更新**：GitHub Releases `latest.json`（minisign 签名校验）；检查/下载/取消/安装/失败重试
- **开机自启动**（macOS LaunchAgent 方式）；**中英双语 UI**（i18n 全覆盖，无硬编码文案）
- **macOS 权限引导**：屏幕录制（截图/窗口标题）、辅助功能（健康键鼠采样）、通知三项权限的四态查询与显式请求/打开系统设置/重新打开应用引导；首次启动有 LAN 风险披露确认 + 权限 onboarding，设置中可重置重走

## 下载安装

前往 [Releases](https://github.com/mmletgo/cc-partner/releases) 下载对应系统包：

| 系统 | 文件 | 说明 |
|------|------|------|
| macOS (Apple Silicon) | `.dmg` | ad-hoc 签名；首次打开及输入监控授权见下方 |
| Windows | `.exe`（NSIS） | 安装程序 |
| Ubuntu / Linux | `.AppImage` / `.deb` / `.rpm` | 直接运行或安装包 |

### macOS

当前没有 Apple Developer ID，因此公开 Release 的 Apple Silicon `.dmg` 使用 ad-hoc 签名且未公证。首次打开需要在 Finder 中右键选择“打开”并确认；输入监控仍可在系统设置的列表下方通过 `+` 手动添加当前 `.app`。固定自签名构建可让 TCC 权限在升级间更稳定，但与公开包不是不同产品版本。详见 [`docs/development/macos-internal-signing.md`](docs/development/macos-internal-signing.md)。

### Windows

下载 `.exe`，按向导安装。Workbench 完整 tmux 上下文依赖 **默认 WSL 发行版内的 tmux**（见下方平台说明）。

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
# macOS：统一生成 ~/Applications/cc-partner (Dev).app；检测到固定签名
# identity 时使用固定签名，否则使用可手动配置输入监控的 ad-hoc 签名。
# 详见 docs/development/macos-internal-signing.md
# 并行 git worktree：brew install sccache 后 start.sh 会启用跨树 rustc 缓存，
# 并 cargo clean 未在编译的其它 worktree（不要设共享 CARGO_TARGET_DIR）。

# 或手动（裸 tauri dev 不是可在系统设置中稳定定位的 `.app`，不适合权限调试）
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

若设备能发现但传输、Mobile Workbench 或打开远端项目失败，请确认同一局域网，并**手动**放行入站（建议限定 Private/Home/LAN profile）。应用不会自动修改主机防火墙；设置「依赖」页提供只读检测与各平台操作指引。

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
| 浏览器自动验证 | 内置 managed Chromium；Linux aarch64 打包缺浏览器资产时不可用，旧远端 peer 无该能力时提示 unsupported |
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
- 浏览器自动验证的全平台真实 Chromium、Agent Hub 真实 CLI 写盘 / 双主机 LAN 推送、1 GiB 双主机断点续传（由真机验收与 Release CI 另行覆盖）

Release 安装包由发版工作流产出，**不是**上述 smoke 的范围。完整本地命令 / CI 对照表见 [`docs/development/testing.md`](docs/development/testing.md)。

## 使用提示

### 第一次打开

托盘图标打开主界面；确认局域网风险披露后，后台启动网络服务并发现设备。Workbench 需要 tmux 时，可在设置「依赖」页检测/安装预览；Inbox 会投影缺失类阻塞；「依赖」页同时展示局域网防火墙端口状态与运行诊断（打开日志目录 / 复制脱敏摘要）。

### Workbench 工作流建议

- 添加本机项目 → 新建 worktree（选择前缀）→ 终端里跑 Claude Code → ⌘K 搜索恢复历史 session → AI commit → 一键合并回主工作区（或主工作区 collect-merge 收集分支）
- 起 dev server 后从终端工具栏进入浏览器预览，用「一键验证」拿截图/console/断言结果
- 自动化任务在「项目自动化」控制台创建与观察；full-auto 交付开关在 Settings 自动化 tab

### 文件传输 / 截图 / Prompt / 健康提醒

- 传输：同一局域网 → 选设备 → 选文件或拖拽；接收默认 `~/cc-partner-files/`（可改）；失败且双方支持续传时显示「继续传输」，否则「重新传输」
- 截图：全局快捷键 → 拖选 → 标注（矩形/箭头/色板）→ 确认进剪贴板；`ESC` 取消
- Prompt：新建/标签/搜索/复制；版本历史可恢复；在线设备间按向量时钟合并，云端同步可在 Settings 配置 GitHub 私有仓库
- 健康：Settings「健康提醒」tab 配置工作窗口/休息/喝水间隔与免打扰；健康页看习惯统计，「+1 杯」手动记录饮水

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

### 终端重启后没有恢复 shell 上下文？

- 检查 tmux 是否安装（设置「依赖」页）；Windows 需默认 WSL 发行版内有 tmux
- 无 tmux 时回退普通 PTY，不承诺恢复语义

### 浏览器预览或一键验证不可用？

- 预览只识别项目所在设备上的 http(s) + 显式端口 + 回环地址 dev server；没有 dev server 时可手动输入目标
- Linux aarch64 打包缺 managed Chromium 资产、或旧远端 peer 不支持验证能力时，对应功能展示不可用

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
- 仓库协作约定：[`AGENTS.md`](AGENTS.md)（[`CLAUDE.md`](CLAUDE.md) 为同内容软链）
- 前端指令：[`web/AGENTS.md`](web/AGENTS.md)（[`web/CLAUDE.md`](web/CLAUDE.md) 为同内容软链）
- 后端指令：[`src-tauri/AGENTS.md`](src-tauri/AGENTS.md)（[`src-tauri/CLAUDE.md`](src-tauri/CLAUDE.md) 为同内容软链）

## 开源协议

MIT License
