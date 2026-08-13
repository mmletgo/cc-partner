# cc-partner - 跨平台局域网协作工具

> 开发指令分层：根 `AGENTS.md`（组件/token/验证入口）→ `web/CLAUDE.md`（前端）→ `src-tauri/CLAUDE.md`（后端/协议/发版/smoke）。本文只保留跨目录概览。

## 项目概述

支持 Mac/Windows/Ubuntu 的 **local-first 多设备项目工作台**（Tauri 2 + Rust + React）。核心能力：

1. **Workbench** — 本机/局域网远端项目、worktree、终端、文件、Git、自动化
2. **Mobile Workbench** — 局域网 `/mobile` 浏览器远程操作（无调用者身份校验；可达网络任意设备可读写执行）
3. **Orchestrator** — 项目级任务队列、可见 Runner、验证 evidence 与 full-auto 交付
4. **Agent Hub** — `/agent-hub` 统一管理 Claude/Codex/OpenCode 用户级与项目级指令（公共/适配/独有三槽 + AI 辅助修订）与可移植资产
5. **独立后端 CLI** — `cc-partner-backend start|status|doctor|stop|supervise`（远端可无 GUI）
6. **全局 Inbox（Attention）** — 实时阻塞投影；只导航到权威界面
7. **辅助工具** — 文件传输、区域截图、Prompt/速记本/SSH 目标同步、GitHub 周热门、自动更新

## 技术栈

- 桌面宿主: **Tauri 2**（Rust 主进程 + backend sidecar）
- 后端: Rust（axum + reqwest + mdns-sd + sqlx/SQLite + tracing 等）
- 前端: React 19 + TypeScript + Vite（`web/`）
- 存储: SQLite `~/.cc-partner/data.db`（可 `CC_PARTNER_DATA_DIR` 隔离）

## 代码结构

```
src-tauri/   → Rust 后端，见 src-tauri/CLAUDE.md
web/         → React 前端，见 web/CLAUDE.md
scripts/     → bump-version / prepare-tauri-sidecar / check-p2p-route-inventory / check-quality-traceability / check-docs 等门禁
.github/     → ci.yml · cross-platform-smoke.yml · release-tauri.yml · docs.yml
docs/        → prd + 设计文档；质量证据矩阵 `docs/development/quality-matrix.json`（L0–L3 ID）
AGENTS.md    → 根层开发指南（组件清单与跨目录陷阱）
```

**一键启动**：`./start.sh`（dev / build / web / help）。macOS `dev` 经 `scripts/macos-dev-cargo-runner.sh` 固定组装到 `~/Applications/cc-partner (Dev).app`（`com.cc-partner.app.dev`），与发布版 `/Applications/cc-partner.app` 分开展示/授权；无固定签名时可在系统设置中手动添加。

## 核心架构

### 双通道通信（务必遵守）

- **本地前端 ↔ Rust**：Tauri `invoke()` IPC。**无**桌面前端本地 HTTP API 端口。
- **跨设备 P2P / mobile**：axum HTTP + reqwest + mDNS。**首选 TCP 62116**，占用则 **+1**；`config.http_port=0` 表示用首选，**不是** OS `port=0` 临时绑定。实际端口以 UI 或 `/api/health` 的 `http_port` 为准。mDNS UDP **5353**。
- 共享 `AppState`（`app.manage` + axum `with_state`）。

### 入口与配置

- GUI：`src-tauri/src/lib.rs` setup → 构建 AppState/窗口 → **LAN 风险披露已确认**后才 ensure sidecar + browse-only mDNS（bootstrap 按 flavor：`gui-bootstrap.json`（release）/ `gui-bootstrap.dev.json`（dev），仅 version/timestamp；未确认跳过 ensure/start）
- Headless：`cc-partner-backend serve`（由 `start` detach）advertise+browse；**不读** gui-bootstrap
- 配置：`~/.cc-partner/config.json`（设备 ID、首选端口、路径、快捷键等）

### Prompt 同步（摘要）

向量时钟 + 严格领先覆盖 / 并发 LWW；时间戳相等 device_id 字典序 tie-break。手动 `trigger_sync` 返回 per-device/domain 真值（仅全成功设备计入 `synced`/`succeeded_devices`）。

### 自动更新（摘要）

`check_update` / `download_update` / `install_update`；endpoint = GitHub Releases `latest.json`（minisign）。签名 secret 与三段式 release 细节见 `src-tauri/CLAUDE.md` M8/M9。

### CI / 发版（摘要）

| Workflow | 作用 |
| --- | --- |
| `ci.yml` | Ubuntu：web lint/build/test/e2e + cargo fmt/clippy/test |
| `cross-platform-smoke.yml` | macOS/Windows：CLI/PTY/doctor/logs smoke；**不**验证 WSL/tmux/GUI/多机 mDNS |
| `release-tauri.yml` | tag `v*`：sidecar 准备 + **原生 tauri build** 矩阵 + publish + 独立 `latest.json`（**不是 tauri-action**） |
| `docs.yml` | 文档/矩阵路径变更：`check-quality-traceability` + `check-docs`（Node only；含 E2E/L2/L3 ID 存在性） |

发版：`node scripts/bump-version.mjs <ver>` → 提交 → `git tag v<ver> && git push origin v<ver>`。

## 关键陷阱

- **端口措辞**：禁止写「port=0 动态分配」；写首选 62116 + 占用递增 + health 实际端口
- **Release 措辞**：禁止写 tauri-action 构建；写三段式原生 CLI
- **Runner 措辞**：禁止 `npx --yes` / 文档中的 `npx tsx` 单文件测试；用 `npm test` / `npm run test:e2e`
- **数据兼容**：`~/.cc-partner` 与旧 `~/.claude-partner` 迁移；`CREATE TABLE IF NOT EXISTS`
- **日志**：`tracing` only，禁止 `tauri-plugin-log`
- **macOS 透明窗 / 权限 FFI**：见 `src-tauri/CLAUDE.md` M6/M7
- **Hooks 顺序**：React hooks 必须在 early return 前（`AGENTS.md` §5.8）
