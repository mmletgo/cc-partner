# Claude Partner - 跨平台局域网协作工具

## 项目概述

支持 Mac/Windows/Ubuntu 三端的桌面工具，基于 **Tauri 2 + Rust 后端 + React 前端**。核心功能：
1. **局域网文件传输** - 任意大小文件分块传输，支持断点续传
2. **区域截图** - 框选截图后保存到剪贴板，可直接粘贴到 Claude Code
3. **Prompt 管理** - 记录/复制/打标签/按标签筛选的文本管理
4. **P2P 自动互联** - 每个实例既是服务端也是客户端，局域网内 mDNS 自动发现
5. **Prompt 同步** - 基于向量时钟的跨设备 Prompt 数据同步
6. **自动更新** - 从 GitHub Releases 自动检测、下载和安装新版本
7. **CLAUDE.md 编辑与同步** - 应用内编辑 user 级 `~/.claude/CLAUDE.md`，复用向量时钟同步到局域网设备（文件为权威源，DB 存同步元数据，对账纳入应用外编辑）
8. **SSH 配置管理** - 列出局域网设备 IP + 手填 IP，为每个目标配置用户名/端口并一键复制 ssh 命令；配置基于向量时钟跨设备同步；附 mac/ubuntu/windows 三端配置指南

## 技术栈

- 桌面宿主: **Tauri 2**（Rust 主进程）
- 后端: Rust（axum HTTP server + reqwest peer client + mdns-sd 发现 + sqlx/SQLite + xcap 抓屏 + arboard 剪贴板 + sha2 校验 + tracing 日志）
- 前端 GUI: React 19 + TypeScript + Vite（`web/` 目录）
- Tauri 插件: global-shortcut（截图快捷键）、updater（自动更新）、process（安装后重启）、dialog
- 本地存储: SQLite（sqlx，直接读写旧 `~/.claude-partner/data.db`）

## 代码结构

```
src-tauri/   → Rust 后端（Tauri 主进程：配置/存储/网络/同步/传输/截图/权限/托盘/快捷键/更新），见 src-tauri/CLAUDE.md
web/         → React 前端（复用迁移前代码，通过 @tauri-apps/api invoke 调 Rust），见 web/CLAUDE.md
scripts/     → bump-version.mjs（发版版本号同步）+ icon 图标源
.github/     → workflows/ci.yml（PR/push 质量门禁）+ release-tauri.yml（tag 三平台发版打包）
docs/        → 需求/设计文档
uiux/        → 设计稿参考资源
```

**一键启动**：`./start.sh`（默认 dev 开发热重载；`build` 生产构建；`web` 仅前端 Vite；`help` 查看用法）。脚本会自检 Node/Rust 工具链并按需 `npm install`。

## 核心架构

### 双通道通信（务必遵守）
- **本地前端 ↔ Rust**：Tauri `invoke()` IPC（`#[tauri::command]`）。无本地端口暴露、无 CORS、无启动端口竞态。
- **跨设备 P2P**：axum HTTP server（`port=0` 动态分配），供对端 reqwest 调用 `/api/health`、`/api/sync/{pull,push}`、`/api/transfer/{init,chunk,status}`。
- 两条通道共享同一份 `AppState`（`Arc<RwLock<...>>`），由 `app.manage()` 注入命令层、`with_state()` 注入 axum。

### 应用入口（`src-tauri/src/lib.rs`）
Tauri Builder + setup 装配。启动顺序：tracing 初始化 → load config（`~/.claude-partner/config.json`，缺失生成默认）→ init_db（WAL + CREATE TABLE IF NOT EXISTS，兼容旧库）→ AppState → axum HTTP server（动态端口）→ mDNS 注册 → 系统托盘 → 全局快捷键。关闭时反向清理（`RunEvent::Exit` 注销 mDNS）。

### 配置管理（`src-tauri/src/config.rs`）
设备 ID（UUID，首次生成持久化）、HTTP 服务端口（动态分配）、文件接收保存路径、数据库路径、截图快捷键。配置存 `~/.claude-partner/config.json`。

### Prompt 同步策略
向量时钟 `{device_id: counter}`，修改时递增本设备计数器；同步比较：严格领先 → 覆盖；并发 → LWW（最后修改者胜），并发且时间戳相等时用 device_id 字典序 tie-break（较纯 LWW 更确定）。手动触发（前端「同步」按钮 → `invoke('trigger_sync')`）。

### 自动更新（`commands/updater.rs` + tauri-plugin-updater）
前端「检查更新」→ `check_update`；有更新「下载」→ `download_update`（emit `update:download-progress`）；「安装并重启」→ `install_update`（`spawn_blocking` 跑 `update.install` + `app.request_restart()`）。endpoint 指向 GitHub Releases 的 `latest.json`（M9 CI 产出，minisign 签名校验）。

### CI/CD 流程

两套 GitHub Actions workflow 分工（均在 `.github/workflows/`）：

- **ci.yml**（PR / push `master` 触发，Linux 单平台）：前端 lint + build（tsc+vite）、Rust `cargo fmt --check` + `cargo clippy -- -D warnings` + `cargo test`，严格门禁阻断不合规合并。纯文档/设计稿改动不触发。带 npm + cargo 缓存。
- **release-tauri.yml**（push `v*` tag 触发）：矩阵构建 macOS（Apple Silicon + Intel 双架构）/ Windows / Linux 三平台安装包，签名后上传 Release + 产出 `latest.json`（各平台签名下载 URL，供 updater）。带 npm + cargo 缓存加速。

### 发版流程
1. `node scripts/bump-version.mjs <新版本号>`（同步 `tauri.conf.json` + `Cargo.toml` + `web/package.json`；**版本号单一来源是 `tauri.conf.json` 的 version**）
2. 提交改动
3. `git tag v<版本号> && git push origin v<版本号>` 触发 `release-tauri.yml`
4. CI 用 tauri-action 矩阵构建 macOS(aarch64 + x86_64 双架构) / Windows / Linux 三平台，签发 Release 并产出 `latest.json`（含各平台签名下载 URL）

> **macOS 暂用 ad-hoc 签名**（`signingIdentity: "-"`，开发/测试免 Apple Developer ID）。正式分发需后续配 Apple Developer ID + notarization。
> **updater 端到端校验**需在 repo Settings → Secrets 配 `TAURI_SIGNING_PRIVATE_KEY`（`~/.tauri/claude-partner.updater.key` 内容，空密码免配 PASSWORD）。未配则 CI 不签名、latest.json 无 signature，updater 校验失败。

## 关键陷阱

- **数据兼容**：直接读写旧 `~/.claude-partner/data.db`，建表全用 `CREATE TABLE IF NOT EXISTS` 保用户数据；`tags`/`vector_clock` 是标准 JSON TEXT；`datetime` 兼容有无时区偏移两种格式。
- **向量时钟 tie-break 差异**：Rust merger 在并发且时间戳相等时用 device_id 字典序 tie-break，较迁移前纯 LWW 更确定（避免双端抖动）——极端并发场景行为有细微差异，属预期。
- **日志用 `tracing`/`tracing-subscriber`，禁止引入 `tauri-plugin-log`**（与 tracing_subscriber 冲突 panic）。
- **macOS 透明窗口**：`transparent(true)` 需 tauri crate 开 `macos-private-api` feature 且 `tauri.conf.json` 设 `app.macOSPrivateApi: true`（两者必须匹配）。
- **macOS 权限 FFI 不写 `#[link]`**：CoreGraphics framework 已被 Tauri 依赖链链接，显式 `#[link(name="CoreGraphics")]` 反而报 library not found。
