# Backend CLI Design

## Goal

让远端设备不必完整启动 cc-partner GUI，只要运行独立命令行后端，就能被本机 cc-partner 发现并提供现有远端 Workbench、文件、Git、终端、Prompt 优化、Orchestrator 和移动端代理能力。GUI 仍保持当前桌面体验，但启动和关闭时要能管理这个独立后端进程。

## Scope

首版覆盖：

- 新增独立二进制 `cc-partner-backend`。
- 命令支持 `start`、`stop`、`status`，内部可有隐藏 `serve` 子命令承载常驻服务。
- 后端服务复用现有 Rust 后端能力：配置、SQLite、axum HTTP、mDNS、Workbench、Orchestrator、云同步和 CC 历史采集。
- GUI 启动时检测独立后端是否已运行；未运行则自动启动。
- GUI 关闭主窗口时弹窗询问“仅关闭 GUI”或“前后端都关闭”。
- 远端设备运行 `cc-partner-backend start` 后，本机能像对方完整启动 GUI 一样发现并操作远端功能。

首版不做：

- 多用户权限模型、远程登录认证、公网暴露。
- systemd/LaunchAgent/Windows Service 常驻安装器。
- 旧 GUI 内嵌后端和独立后端同时服务同一个配置库。
- 完全脱离 Tauri 类型体系重写纯 axum 服务。

## Command UX

用户面对的命令：

```bash
cc-partner-backend start
cc-partner-backend stop
cc-partner-backend status
```

行为契约：

- `start`：如果后端未运行，后台拉起服务并返回实际 HTTP 端口；如果已运行，直接返回已运行状态和端口。
- `stop`：优先通过本机控制端点优雅停止；控制端点失败时根据 pid 文件做进程存在性检查，必要时返回明确错误，不静默成功。
- `status`：读取控制文件并调用 `/api/health`，返回 running/stopped/stale/error、pid、端口、设备名和设备 ID。
- 隐藏 `serve`：由 `start` 或 GUI 自动拉起，前台常驻运行；不作为普通用户文档主入口。

进程控制文件放在 `~/.cc-partner/`：

- `backend.pid`：当前服务进程 pid。
- `backend-control.json`：`pid`、`port`、`deviceId`、`startedAt`、`controlToken`。

`controlToken` 只用于本机 stop，生成随机值并只写入用户目录；控制端点仅绑定 loopback 或要求 token，不能被局域网设备关闭。

## Architecture

推荐结构是“共享后端 runtime + GUI/CLI 两个入口”：

```text
src-tauri/src/backend/runtime.rs
  -> build AppState
  -> start HTTP server
  -> start mDNS discovery
  -> start background tasks
  -> shutdown cleanup

src-tauri/src/bin/cc-partner-backend.rs
  -> parse start|stop|status|serve
  -> manage pid/control files
  -> call backend runtime in serve

src-tauri/src/lib.rs
  -> Tauri GUI plugins/window/tray/hotkey
  -> ensure external backend process is running
  -> use commands against local shared state only when still needed
```

首版为了降低风险，GUI 仍可在同进程构造 `AppState` 给 Tauri IPC 命令使用，但 HTTP/mDNS 远端支持必须由独立后端承担。实施时应逐步把现有 `lib.rs` 内联初始化拆到 `backend/runtime.rs`，保证 CLI 和 GUI 不复制启动逻辑。

## Runtime Boundaries

共享 runtime 负责：

- 加载 `AppConfig`，执行旧 `~/.claude-partner` 到 `~/.cc-partner` 迁移。
- 初始化 SQLite 和全部 schema/repo。
- 构造 `AppState`。
- 启动 axum HTTP server，回填 `actual_http_port`。
- 启动 mDNS discovery，宣告实际 HTTP 端口。
- 启动 CC 历史采集、云同步、Orchestrator scheduler、Orchestrator remote outbox。
- 支持关闭时停止 discovery、取消后台任务、关闭 Workbench PTY attach。

GUI-only 逻辑留在 Tauri 入口：

- 系统托盘。
- 全局截图快捷键。
- 原生权限引导、截图 overlay、健康 overlay。
- 自动更新安装与重启。
- 前端事件通知和系统通知。

需要特别处理的现有依赖：

- `AppState.app_handle` 目前是 `tauri::AppHandle`，部分 HTTP route 用它读取移动端静态资源或 emit GUI 事件。
- 设计上应引入轻量 adapter，例如 `BackendUiHandle`，让 CLI runtime 可用 no-op/event-log 实现，GUI runtime 用 Tauri 实现。
- `/mobile` 静态资源在 CLI 中不能依赖 Tauri asset resolver；应优先读 `web/dist` 或随 CLI bundle 放置的资源目录，保留现有 dev fallback。
- 远端 Workbench 终端事件在 CLI 中仍要通过 HTTP NDJSON broadcast 工作；GUI emit 事件只作为本机前端增强，不应阻断远端能力。

## GUI Lifecycle

GUI 启动：

1. 读取 `~/.cc-partner/backend-control.json`。
2. 调 `/api/health` 校验后端是否存活且设备 ID 与配置一致。
3. 未运行或控制文件 stale 时，调用 `cc-partner-backend start`。
4. 后端启动失败时，GUI 仍可打开，但 Settings/依赖环境展示“后端未运行”的错误和重试入口。

GUI 关闭：

- 拦截主窗口 `CloseRequested`。
- 弹窗提供三个动作：
  - “仅关闭 GUI”：关闭/隐藏主窗口，不调用 backend stop。
  - “前后端都关闭”：调用 `cc-partner-backend stop`，成功后退出 GUI。
  - “取消”：保留窗口。
- 托盘“退出”复用同一关闭策略，不能直接绕过后端清理。

如果 GUI 是由用户打开来管理已有 CLI 后端，默认关闭动作不应杀掉后端；必须尊重用户选择。

## Remote Behavior

远端设备运行：

```bash
cc-partner-backend start
```

后：

- mDNS 宣告与完整 GUI 当前一致。
- 本机 `Devices` 能发现远端。
- Workbench 远端目录选择、打开远端项目、worktree、terminal、files、git、browser preview、Prompt 优化和 Orchestrator remote task view 继续走现有 `/api/workbench/*`、`/api/mobile/workbench/*`、`/api/orchestrator/*` routes。
- 对远端来说，功能语义等价于“完整 cc-partner 已启动但没有打开 GUI”。

不可用能力：

- 需要远端真实窗口交互的能力不作为 headless 后端目标，例如远端截图 overlay、远端 GUI 设置弹窗。
- 若某 route 当前必须 emit GUI 事件，应降级为 no-op 或 HTTP event，而不是让请求失败。

## Error Handling

- PID 文件存在但进程不存在：`status` 返回 stale，`start` 清理后重启。
- 端口被占用：沿用现有 `start_http_server` 的首选端口递增策略，并把实际端口写入控制文件和 mDNS。
- 后端启动过程中 DB 初始化失败：`start` 返回非 0，并打印明确错误。
- `stop` token 不匹配：返回非 0，不停止服务。
- GUI 自动启动后端失败：GUI 继续运行并展示错误，不直接退出。

## Packaging

Cargo 新增 bin target：

```toml
[[bin]]
name = "cc-partner-backend"
path = "src/bin/cc-partner-backend.rs"
```

打包要求：

- 本地开发可通过 `cargo run --bin cc-partner-backend -- start` 验证。
- Tauri bundle 需要把 CLI 放入应用资源或 sidecar，GUI 能定位并启动它。
- Release 产物应包含用户可直接执行的 CLI。不同平台的安装路径可不同，但前端/GUI 后端定位逻辑必须从 Tauri sidecar 或相邻二进制解析，不能硬编码开发路径。

## Testing

Rust 单元测试：

- control file 读写与 stale 判断。
- start/status 对已有运行实例的幂等行为。
- stop token 校验。
- backend runtime shutdown 清理函数能重复调用。
- HTTP server 端口选择与控制文件端口一致。

集成验证：

- `cargo test` 覆盖新增 backend lifecycle 纯逻辑。
- `cargo run --bin cc-partner-backend -- start` 后，`cc-partner-backend status` 返回 running。
- `curl http://127.0.0.1:<port>/api/health` 返回当前设备。
- `cc-partner-backend stop` 后 status 返回 stopped/stale。
- GUI 启动时后端未运行会自动拉起。
- GUI 关闭选择“仅关闭 GUI”后，`status` 仍为 running。
- GUI 关闭选择“前后端都关闭”后，`status` 为 stopped。

## Documentation

实现完成后更新：

- `src-tauri/CLAUDE.md`：记录后端 CLI、runtime 边界、Rust 验证命令。
- 根 `AGENTS.md`：只补顶层项目概览/目录地图里的后端 CLI 一行，不写实现流水账。
- `docs/prd.md` 如已有远端支持/Workbench 启动语义，应同步改成独立后端模式。

## Open Decisions

- `start` 默认后台 daemonize；`serve` 作为内部前台命令。
- CLI 首版不安装系统服务。
- GUI 关闭默认不隐式杀后端，必须由用户在弹窗中明确选择。
- CLI 控制端点只用于本机生命周期管理，局域网远端不能调用 stop。
