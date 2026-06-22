# src-tauri/ - Tauri 2 + Rust 后端

## 概述

Claude Partner 的桌面宿主与全部后端逻辑，从 PyQt6 + Python 迁移而来。Tauri 2 主进程用 Rust 实现配置/存储/网络/同步/传输/截图/权限/更新等全部能力；前端复用 `web/` 的 React。

## 通信架构（核心，务必遵守）

- **本地前端 ↔ Rust**：Tauri `invoke()` IPC（`#[tauri::command]`）。无本地端口暴露、无 CORS、无启动端口竞态。前端 `web/src/api/` 底层走 `@tauri-apps/api/core` 的 `invoke`，组件层无感知。
- **跨设备 P2P**：axum HTTP server（`port=0` 动态分配），供对端 `reqwest` 调用 `/api/health`、`/api/sync/{pull,push}`、`/api/transfer/{init,chunk,status}`。
- 两条通道共享同一份 `AppState`（`Arc<RwLock<...>>`），由 `app.manage()` 注入命令层、`with_state()` 注入 axum。

## 目录结构（随 M1–M9 里程碑逐步落地）

```
src/
├── main.rs / lib.rs   — Tauri Builder + 命令注册 + setup 装配（load config → init_db → AppState）
├── state.rs           — AppState（config: RwLock + db pool + prompt_repo + device_id）[已实现]
├── error.rs           — AppError（thiserror + serde 成 {error:"msg"}）              [已实现]
├── config.rs          — AppConfig：读旧 ~/.claude-partner/config.json，缺失生成默认 [已实现]
├── commands/          — #[tauri::command]：prompts（list/get/create/update/delete/list_tags）+ config（get/update/get_version）+ ping [已实现 Prompt/配置/版本，设备/传输/同步/更新/权限待后续里程碑]
├── models/prompt.rs   — PromptRow（snake_case，DB/同步）+ PromptDto（camelCase，前端） [已实现]
├── storage/prompt_repo.rs — sqlx 运行期 query（非宏），list/get/create/update/soft_delete/list_tags [已实现]
├── sync/              — 向量时钟 + LWW 合并 + engine              [M4]
├── net/               — mdns-sd 发现 + axum server + reqwest client [已实现 M3]
├── transfer/          — 分块传输 + SHA256 + 断点续传              [M5]
├── screenshot/        — xcap 抓屏 + 透明选区窗口                  [M6]
├── permissions/       — macOS 权限 FFI（CGPreflight/CGRequest/CGEventTap） [M7 已实现]
├── hotkey.rs          — pynput→plugin 快捷键格式转换 + 注册/热更新  [M7 已实现]
├── tray.rs            — 系统托盘（Tauri 2 tray API）              [M7 已实现]
└── commands/updater.rs — 自动更新 5 命令（check/download/status/cancel/install，对齐前端 types.ts）[M8 已实现]
migrations/0001_init.sql — schema 文档（lib.rs 内联执行，全 CREATE TABLE IF NOT EXISTS 兼容旧库）
```

## M1 已落地行为约定（移植自 Python，逐方法对照）

- **数据库初始化**：`init_db` 用 `SqliteConnectOptions` 开 `create_if_missing` + `pragma(journal_mode=WAL)`，`max_connections(1)` 单连接语义；不用 `sqlx::migrate!`（对旧库无 _sqlx_migrations 表有坑），改 lib.rs 内联两条 `CREATE TABLE IF NOT EXISTS`。运行期 `sqlx::query`（非宏）规避 DATABASE_URL 编译期要求。
- **JSON 字段**：`tags`/`vector_clock` 用 `serde_json::to_string`/`from_str`，紧凑标准 JSON，与 Python `json.dumps(ensure_ascii=False)` 互通。
- **datetime**：`created_at`/`updated_at` 以 `String` 透传，兼容旧库有无时区偏移两种格式；新建用 `Utc::now().to_rfc3339()`（对照 Python `datetime.now(timezone.utc).isoformat()`）。
- **vector_clock 维护**：create 初始化 `{device_id:1}`；update/delete 自增 `vector_clock[device_id] += 1`（CRDT 语义）。
- **delete 是软删除**：`soft_delete` 设 `deleted=1` + `updated_at=now` + 写回推进后的 vector_clock（修正了 Python handler 自增 clock 却未落库的 bug）。
- **PromptDto** 比 Row 多 `tag`（tags[0] 投影，兼容旧前端），对照 Python `_prompt_to_frontend_dict`。
- **httpPort**：M1 未实际监听 HTTP，`get_config` 返回配置值（0）；M3 axum 接入后改为真实端口。

## M3 已落地行为约定（移植自 Python network/，逐方法对照）

- **mDNS service type**：`_claude-partner._tcp.local.`（`net/mod.rs::SERVICE_TYPE`），与 Python `discovery.py` 完全一致，迁移期 Rust 版与旧 Python 版可互发现。
- **TXT 记录字段**：`device_id`、`device_name`（与 Python discovery.py 一致；**port 不在 TXT**，走 mDNS SRV record，与 Python 相同）。
- **服务实例名**：`{device_id}`（不含 type 后缀，mdns-sd `ServiceInfo::new` 的 `my_name`）；**host_name** = `cp-{device_id}.local.`（对照 Python `server_name`，避免系统 hostname 解析到多 IP）。
- **本机过滤**：`ServiceResolved` 时比对 TXT `device_id` 与本机 device_id，一致则忽略（与 Python `_on_service_state_change` 过滤逻辑一致）。本机设备不入 devices 表。
- **本机 IP 探测**：`local_lan_ip` 用 UDP socket "连接" 8.8.8.8 探测出站接口 IP（对照 Python `_get_local_ip` 回退方案）；探测失败回退 `enable_addr_auto` 让 mdns-sd 自动更新接口地址。
- **事件循环**：用 mdns-sd re-export 的 `Receiver<ServiceEvent>`，`recv()` 阻塞等待（daemon shutdown 后 channel 断开自然退出）；Resolved → 写 devices 表，Removed(`fullname` 去 type 后缀得 device_id) → 剔除。
- **动态端口**：axum `TcpListener::bind(("0.0.0.0", 0))`，`local_addr().port()` 取实际端口回填 `AppState.actual_http_port: AtomicU16`；mDNS 注册用该端口（启动顺序：先 axum 拿端口 → 再 mDNS 注册）。
- **/api/health**：`GET` 返回 `{ok, device_id, device_name, http_port, ts}`（字段名 snake_case，对照 Python `handle_health`，供对端 peer_client 解析；对端仅判 status==200）。
- **body limit**：axum `DefaultBodyLimit::max(2MB)`（对照 Python `client_max_size=2MB`），M5 chunk（1MB）+ 开销。
- **AppState 共享**：axum `with_state(state.clone())` 与 invoke `State<'_, AppState>` 共享同一份 `Arc`；devices = `Arc<RwLock<HashMap<String, Device>>>`，actual_http_port = `Arc<AtomicU16>`，discovery = `Arc<Mutex<Option<ServiceDaemon>>>`。
- **peer_client**：`reqwest::Client`（rustls-tls，无系统 OpenSSL 依赖）；`health(addr, port)` GET `/api/health`，10s 超时，status==200 返回 true；`sync_pull/sync_push`（M4）POST 对端 `/api/sync/{pull,push}`，入参出参字段对照 Python（summaries/prompts/vector_clock/accepted），snake_case；transfer 方法预留（TODO M5）。
- **启动容错**：axum/mDNS 启动失败不阻断应用（本地功能仍可用），仅 `tracing::error` 记录。

## M4 已落地行为约定（移植自 Python sync/，逐方法对照）

- **向量时钟（sync/vector_clock.rs，纯算法）**：`compare(a,b) -> ClockOrder{Before,After,Equal,Concurrent}`，遍历 a∪b 的 key，缺省 0，用两个 bool 标记 a>b / b>a 分量；两者皆真=Concurrent，仅 a 真=After（a 领先），仅 b 真=Before，皆否=Equal。`merge` 逐 key 取 max。`increment` 克隆后 device_id 计数器 +1。全部与 Python `vector_clock.py` 逐字等价。配 8 个单测覆盖四种关系 + increment + merge。
- **LWW 合并（sync/merger.rs）**：`should_update(local,remote)` 照搬 Python（remote 严格领先→true，并发→`remote.updated_at > local.updated_at`）。`merge_prompt` 始终合并双方向量时钟（保留因果历史），胜出方取内容。并发且时间戳相等时用 **device_id 字典序 tie-break**（较 Python 纯 LWW 更确定，避免双端抖动）。deleted prompt 照常参与同步传播。配 7 个单测。
- **同步流程（sync/engine.rs）**：`trigger_sync(state) -> SyncResult{accepted,synced,note}`，遍历 `devices` 全部在线对端，逐个 `sync_with_peer`（失败不阻断其他对端）。单对端：health 检查 → 本端 summaries（含 deleted）→ sync_pull 拿回对端需给的 prompts，逐条 merge_prompt 后 bulk_upsert（仅变化才写）→ sync_push 本端独有/领先而对端没有的 prompts。
- **sync 路由（net/routes/sync.rs）**：`POST /api/sync/pull` body `{summaries:[{id,vector_clock}]}`，返回本端"对端没有 / 本端领先 / 并发"的完整 PromptRow `{prompts:[...]}`；`POST /api/sync/push` body `{prompts:[PromptRow]}`，逐条 merge_prompt 后 bulk_upsert，返回 `{accepted:<count>}`。**字段 snake_case**（PromptRow 默认序列化），与 Python `Prompt.to_dict()` 互通，不是 camelCase。
- **PromptRepo 同步方法**：`get_all_for_sync()`（含 deleted 全量）、`bulk_upsert(&[PromptRow])`（INSERT OR REPLACE，upsert 前不做合并决策）。
- **trigger_sync 命令（commands/sync.rs）**：前端 `invoke('trigger_sync')` → 返回 `{accepted,synced,note}`，前端 `promptsApi.sync()` 取 `synced`。在 `lib.rs` invoke_handler 注册。
- **错误处理**：`AppError` 新增 `axum::IntoResponse` 实现（500 + `{"error":"..."}`），使 sync handler 的 `Result<Json<_>, AppError>` 可作 axum handler 返回；与 Python handler error 响应一致。
- **tracing 初始化**：`lib.rs run()` 开头 `tracing_subscriber::fmt().with_env_filter(EnvFilter::try_from_default_env().or("info")).try_init()`，输出到 stderr，axum/mDNS/sync 的 `tracing::info!/error!` 日志生效（Cargo.toml tracing-subscriber 加 `env-filter` feature）。
- **依赖**：`axum 0.7`（features=macros）、`tower 0.5`、`reqwest 0.12`（default-features=false，features=json/rustls-tls）、`mdns-sd 0.11`。

## M6 已落地行为约定（移植自 Python screenshot/，对照 overlay.py + capture.py）

- **抓屏本体**：`xcap = "0.4"` 跨平台抓屏。`Monitor::all()` 枚举显示器，顺序单进程内稳定；`monitor.capture_image()` 返回 `image::RgbaImage`（**物理像素**，Retina 即逻辑 ×scale_factor）。`monitor.x()/y()/width()/height()` 均为物理像素、`scale_factor()` 返回 `f32`。抓屏前不做权限预检（M7 实现 check_permissions），macOS 未授权屏幕录制返回空白图——由 M7 兜底。
- **选区覆盖窗口（每屏一个）**：macOS 不允许单窗口跨屏（与 Python 一致），`overlay::start_region_capture` 枚举 `Monitor::all()`，**每个显示器建一个** `WebviewWindowBuilder` 窗口，`decorations(false).transparent(true).always_on_top(true).focused(true).skip_taskbar(true).resizable(false).accept_first_mouse(true)`，label = `screenshot-overlay-{i}`，url = `/screenshot-overlay?display={i}`。窗口几何对齐该显示器：xcap 物理几何 ÷ scale_factor → 逻辑像素喂 `set_position/set_size`（Tauri 窗口几何按逻辑像素）。`close_all_overlays` 按前缀 `screenshot-overlay-` 关闭全部。
- **透明窗口前置条件**：`transparent(true)` 需 `tauri` crate 开 `macos-private-api` feature + `tauri.conf.json` 设 `app.macOSPrivateApi: true`（两者必须匹配，否则 build.rs 报 allowlist 错误）。
- **DPR 坐标转换**：前端 React Overlay 把逻辑像素坐标 + `window.devicePixelRatio` 传给 Rust；`capture::crop_and_copy` 用 `(v as f64 * dpr).round()` 换算物理像素 rect 后 `image::imageops::crop_imm` 裁剪（image 0.25 的 crop_imm 直接返回 `SubImage`，非 Result，`.to_image()` 拷出独立 RgbaImage）。裁剪前 clamp 到帧边界防越界。与 Python `int(selection.x() * dpr)` 语义一致。
- **剪贴板写入**：`arboard = "3"`（features=image-data）直接写图片，比 tauri-plugin-clipboard-manager 更直接。`ImageData{width,height,bytes: RGBA 连续缓冲.into()}` → `Clipboard::new()?.set_image()`。RGBA bytes 由 `RgbaImage::into_raw()` 提供（已连续）。对照 Python `clipboard.setPixmap`。
- **背景图**：`capture::snapshot_to_png_base64` 把整屏帧 PNG 编码 + base64（`data:image/png;base64,...`）返回前端 Overlay 作全屏 `<img>` 背景，让用户"像在直接框选屏幕"（overlay 本身透明）。前端 Overlay onMount（窗口已显示后）调 `get_display_snapshot`。
- **命令层**（`commands/screenshot.rs`，lib.rs invoke_handler 注册 4 个）：
  - `start_region_capture(app)` → 每屏建 overlay 窗口
  - `get_display_snapshot(display) -> String` → 该屏 PNG base64 背景
  - `crop_and_copy(app, display, x, y, w, h, dpr)` → 裁剪写剪贴板 + emit `region-capture:result` {ok:true} + 关全部 overlay
  - `cancel_region_capture(app)` → emit `region-capture:result` {cancelled:true} + 关全部 overlay
- **前端选区页**：`web/src/pages/Screenshot/Overlay.tsx`，独立于 AppShell/OnboardingGuard，App.tsx 加路由 `/screenshot-overlay`（顶层，不在守卫内）。鼠标拖动绘制半透明遮罩（四块拼接挖洞）+ 蓝色虚线选区边框；mouseup 宽高≥10 才调 crop_and_copy，否则 cancel；ESC/右键 → cancel。hooks 在 early return 之前（项目规则 20）。
- **权限（capabilities）**：选区窗口 label 前缀 `screenshot-overlay-*` 需在 `capabilities/default.json` 的 `windows` 列表加入通配，否则 overlay 页 invoke 被拒；同时加 `core:event:default`（供 emit/listen region-capture:result）。

## M7 已落地行为约定（移植自 Python `hotkey/listener.py` + `ui/tray.py` + `ui/permissions.py`）

- **macOS 权限 FFI（permissions/mod.rs，对照 Python permissions.py 四函数）**：
  - `check_screen_capture_access`：FFI 调 `CGPreflightScreenCaptureAccess`（10.15+ 符号）。
  - `check_input_monitoring_access`：用 `CGEventTapCreate(kCGHIDEventTap + kCGHeadInsertEventTap + kCGEventTapOptionListenOnly + kCGEventMaskBit(kCGEventKeyDown))` 探测，返回 NULL 即无权限；探测成功立即 `CFMachPortInvalidate` 释放。
  - `request_permission(type)`：screenCapture 调 `CGRequestScreenCaptureAccess`（仅「未决定」弹框，requested=true）+ `open` 打开 Privacy_ScreenCapture 面板；inputMonitoring 无系统 request API，直接打开 Privacy_ListenEvent 面板。
  - **不显式 `#[link]`**：CoreGraphics 作为 macOS framework 已被 Tauri 依赖链（core-graphics/xcap）通过 `-framework CoreGraphics` 链接，符号在链接期已可见；写 `#[link(name="CoreGraphics",kind="dylib")]` 反而会找 `libCoreGraphics.dylib` 报 `library not found`。
  - **非 macOS 一律 granted=true**（对照 Python 非打包行为；Tauri 不区分打包/开发，故开发态 macOS 也真实检测，与 Python 仅打包检测略有差异——开发期需先授权截图/输入监控才能用）。
  - `check_permissions() -> {screenCapture:{granted}, inputMonitoring:{granted}}`；`request_permission` 返回 `{ok, requested, opened}`，与前端 `PermissionsStatus`/`PermissionType` 约定一致。
- **权限命令（commands/permissions.rs）**：`check_permissions`（无 state）、`request_permission(type)` 两个 invoke，lib.rs 注册。
- **全局快捷键（hotkey.rs + tauri-plugin-global-shortcut 2）**：
  - **格式转换** `hotkey_pynput_to_plugin`：config 存 pynput 格式（`<cmd>+<shift>+s`，macOS；`<ctrl>...` 其他），转插件格式 `CommandOrControl+Shift+S`。`<cmd>/<ctrl>/<win>` → `CommandOrControl`（插件按平台解析 macOS=Command / Win/Linux=Ctrl），`<shift>` → `Shift`，`<alt>/<option>` → `Option`，普通键大写。配 3 个单测。
  - **注册**：v2 的 `on_shortcut(shortcut, handler)` 需随快捷键传入 handler（不是 Builder 全局 handler）；`register_screenshot_hotkey(app, hotkey, handler)` 先 `unregister_all` 再 `on_shortcut`。handler = `screenshot_handler`，按下时直接调 `screenshot::overlay::start_region_capture`（Rust 直接起 overlay，不依赖前端 emit）。
  - **热更新**：`commands::config::update_config` 加 `app: AppHandle` 参数，screenshotHotkey 变更后 `register_screenshot_hotkey(app, new_hotkey, screenshot_handler)` 重注册。
  - setup 里读 `config.screenshot_hotkey` 注册。
- **系统托盘（tray.rs，对照 tray.py）**：`TrayIconBuilder` id=`main-tray`，图标用 `app.default_window_icon()`（复用 icons/），tooltip=`Claude Partner`。菜单三项：显示主窗口 / 截图（直接调 overlay::start_region_capture）/ 退出（`app.exit(0)`）。**左键单击托盘**显示主窗口（Python 是双击；Tauri 2 托盘 Click 事件更顺手，行为等价）。需 `tauri` crate 开 `tray-icon` feature。
- **关闭钩子（lib.rs）**：`.build(...)` 后链 `.run(|app_handle, event| {...})`，在 `RunEvent::Exit` 调 `discovery::stop_discovery(&state)` 优雅注销 mDNS（对照 Python 关闭清理顺序）。`stop_discovery` 之前的 `#[allow(dead_code)]` 已移除。
- **error.rs 扩展**：新增 `AppError::Tauri(#[from] tauri::Error)`（托盘/菜单 API 返回 tauri::Error）+ `AppError::generic()` 便捷构造。

## M8 已落地行为约定（自动更新器，用 tauri-plugin-updater 替换 Python 自研 checker/downloader/installer）

- **插件**：`tauri-plugin-updater = "2"`（check/download/install + 签名校验 + 三平台自带替换脚本，**不再写 DMG/CMD/sh 脚本**）+ `tauri-plugin-process = "2"`（rust 侧用 `app.request_restart()`，前端 restart 命令同源）。lib.rs 注册 `.plugin(tauri_plugin_updater::Builder::new().build())` + `.plugin(tauri_plugin_process::init())`。**禁止引入 tauri-plugin-log**（与 tracing_subscriber 冲突 panic，见 M4 踩坑）。
- **capabilities**：`capabilities/default.json` 加 `updater:default` + `process:default`。
- **tauri.conf.json**：加 `plugins.updater`：`pubkey`（minisign 公钥 base64）、`endpoints: ["https://github.com/mmletgo/claude-partner/releases/latest/download/latest.json"]`（M9 CI 产出）、`windows.installMode: "passive"`。端到端更新需 M9 latest.json + 签名产物，M8 只实现命令层。
- **签名密钥**：`npx tauri signer generate -w ~/.tauri/claude-partner.updater.key --password ""`（空密码，免 CI 配置）。私钥路径 `~/.tauri/claude-partner.updater.key`（**不进 git**），公钥已入 tauri.conf.json。**M9 CI 需配 secret `TAURI_SIGNING_PRIVATE_KEY_PATH`（或 `TAURI_SIGNING_PRIVATE_KEY`）**；空密码则 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` 可省。
- **返回类型严格对齐前端 `web/src/lib/types.ts`（camelCase）**：
  - `UpdateCheckResult`（`commands/updater.rs::UpdateCheckResult`，`#[serde(rename_all="camelCase")]`）：`{hasUpdate, version?, body?, downloadUrl?, filename?, size?, error?}`。有更新：`{hasUpdate:true, version, body, downloadUrl(=update.download_url), filename(从 url 路径末段解析), size:Some(0)(check 阶段无 content_length)}`；无更新：全 None；检查异常：`{hasUpdate:false, error}`。
  - `UpdateDownloadStatus`（`UpdateDownloadStatus`）：字段**全非可选**（前端 types.ts 定义 error/filePath/url/filename 为 string、size 为 number），故用 String/u64。`status` 枚举 serde lowercase 对齐 `'idle'|'downloading'|'completed'|'failed'|'cancelled'`；progress 0.0~1.0。filePath 恒空串（updater 下载到内存非文件）。
  - download/cancel/install 返回 `{ok: boolean, error?: string}`（serde_json::Value）。
- **5 个命令（commands/updater.rs，lib.rs invoke_handler 注册）**：
  - `check_update(app, state)`：`app.updater()?.check().await`（`use tauri_plugin_updater::UpdaterExt`）。`Some(update)` → 缓存 Update 到 `state.update_pending` + 返回元数据；`None` → hasUpdate:false；`Err` → hasUpdate:false + error。
  - `download_update(app, state, url?, filename?)`：从 `update_pending` 取 Update clone（跨命令复用同一 check 结果），spawn 异步任务跑 `update.download(on_chunk, on_finish)`，`on_chunk(chunk_len, content_length)` 累计 downloaded + 写状态 + emit `update:download-progress`（{progress, downloaded, total}）。完成存 bytes 到 `update_bytes` + 置 completed；失败置 failed；取消置 cancelled。**url/filename 入参仅兼容前端透传，实际用 Update 内的 download_url**。
  - `get_download_status(state)`：读 `update_status` 返回 `UpdateDownloadStatus`。
  - `cancel_download(state)`：`update_cancel_token.take().cancel()`（软中断，spawn 体内 is_cancelled 判定 Cancelled）+ `update_download_task.take().abort()`（强中断 reqwest 流）+ 主动置 cancelled 兜底。
  - `install_update(app, state)`：校验 status==completed → 取 `update_bytes` + `update_pending` Update → `spawn_blocking(move || update.install(&bytes))`（同步 fs/外部进程，避免阻塞 async 运行时）→ `app.request_restart()`。
- **updater 生命周期处理**：`Update` 是 `#[derive(Clone)]` owned 结构（无生命周期参数），check 后存入 `AppState.update_pending: Arc<Mutex<Option<Update>>>`，download/install 时 clone 取出——**不跨命令重新 check**，避免重复请求 endpoint 且保证 version 一致。
- **取消机制**：updater 的 `download(on_chunk, on_finish)` 无原生取消参数（on_chunk 是 FnMut 不可中断 reqwest 流），故 download 放进 `tauri::async_runtime::spawn`，JoinHandle 存 `update_download_task`，cancel 时 `abort()` 强制中断整个 future 树；辅以 `CancellationToken`（存 `update_cancel_token`）软中断让 spawn 体内 is_cancelled 区分 cancelled vs failed。
- **AppState 扩展**：`update_status: Arc<RwLock<UpdateDownloadStatus>>`、`update_pending: Arc<Mutex<Option<Update>>>`、`update_bytes: Arc<Mutex<Option<Vec<u8>>>>`、`update_download_task: Arc<Mutex<Option<tauri::async_runtime::JoinHandle<()>>>>`、`update_cancel_token: Arc<Mutex<Option<CancellationToken>>>`。

- `cargo build`（在 `src-tauri/` 下）—— M1 交付标准，必须通过
- `./web/node_modules/.bin/tauri dev` — 开发（启动 vite + `cargo run` + 热重载）
- `./web/node_modules/.bin/tauri build` — 生产打包（dmg/nsis/appimage 等）
- `cargo test`（单测）、`cargo clippy`（lint）


## M5 已落地行为约定（移植自 Python transfer/，逐方法对照）

- **传输协议等价（Rust↔Python 互通）**：三条 P2P 端点字段与 Python `protocol.py` 逐字一致——
  - `POST /api/transfer/init`：body `{transfer_id, filename, size, sha256, chunk_size}` → `{transfer_id, accepted, resume_offset}`
  - `POST /api/transfer/chunk/:id`：body=原始 bytes，**header `X-Chunk-Offset`**（缺省 0）→ `{success, received_bytes}`
  - `GET /api/transfer/status/:id` → `{transfer_id, status, progress, transferred_bytes, size, filename}`
  **`X-Chunk-Offset` header 是关键契约**，peer_client `transfer_chunk` 发送端设置、route handler 接收端解析。
- **分块大小**：`transfer::CHUNK_SIZE = 960KB`（960*1024），与 Python 完全一致，低于 aiohttp 默认 1MB 限制兼容未自定义对端。axum `DefaultBodyLimit::max(2MB)` 容纳 chunk+开销（与 Python `client_max_size=2MB` 一致）。
- **SHA256 校验**：发送端发送前用 `sha2` crate 以 8KB 块流式计算（对照 Python `_calculate_sha256`），随 init 元数据下发；接收端收齐（`transferred >= size`）后流式校验 `.tmp`，不符标记 failed + 删 `.tmp`，通过则重命名落地。
- **断点续传**：接收端临时文件 `.{transfer_id}.tmp`（命名与 Python 一致）；init 返回该文件已存在大小作 `resume_offset`，发送端从该 offset seek 续传。接收端 OpenOptions 用 `create+write+read+truncate(false)` 保留旧内容（r+b 语义）。
- **取消机制**：`tokio_util::sync::CancellationToken`（Cargo.toml 加 `tokio-util = {version="0.7", features=["rt"]}`）。每任务在 `TransferRegistry` 内关联一个 token；`cancel_transfer` 命令触发 `cancel()`，发送循环每块前 `is_cancelled()` 检查；取消标记 cancelled + 写历史 + emit `transfer:cancelled`。
- **TransferRegistry**（`transfer/registry.rs`）：`Arc<RwLock<HashMap<String, Entry>>>`，Entry = `{task, cancel_token}`。提供 add/get/cancel_token/update_progress/set_status/mark_completed/failed/cancelled/cancel/remove/list。活跃任务表 + 每任务取消令牌的统一入口。
- **事件 emit 机制**：发送端 spawn 时传入 `AppHandle` clone，循环中 `emit("transfer:progress", {id, transferredBytes, size, progress})`；终态 emit `transfer:completed`/`transfer:failed`/`transfer:cancelled`（含 errorMessage）。接收端 axum handler 通过 `AppState.app_handle`（setup 时从 `app.handle()` 注入）emit `transfer:completed`/`transfer:failed`。前端 `listen(...)` 接收。
- **文件名冲突**：接收目录 `receive_dir` 下重名时 `resolve_filename` 加 `(1)`/`(2)` 后缀（对照 Python `_resolve_filename`：`file.txt → file (1).txt → file (2).txt`；无扩展名 → `README → README (1)`）。配 4 个单测。
- **TransferTask 模型**：`models/transfer.rs` 内部 snake_case（registry + transfer_history 表对齐），`TransferTaskDto` camelCase + 派生 progress 对齐前端 `web/src/lib/types.ts`。状态枚举 `TransferStatus` serde lowercase，方向枚举 `TransferDirection` serde lowercase（与 Python Enum.value 一致）。
- **transfer_history 持久化**：`storage/transfer_repo.rs`（INSERT OR REPLACE record / list 倒序 / update_status）。表 schema 由 lib.rs `TRANSFER_SCHEMA` 内联建表。任务进入终态（completed/failed/cancelled）后写历史并从 registry remove。
- **命令层**（`commands/transfer.rs`，lib.rs invoke_handler 注册）：
  - `list_transfers` → 合并 registry 活跃 + transfer_repo 历史（活跃优先，去重），按 created_at 倒序返回 `TransferTaskDto[]`（对照 Python `/api/transfer/tasks`）
  - `send_transfer(deviceId, filePath)` → 调 `start_sending`（spawn 异步任务），立即返回 `{accepted, deviceId, filePath, id}`（对照 Python `/api/transfer/send`）
  - `cancel_transfer(taskId)` → 触发 CancellationToken，返回 `{ok, id}`；任务不存在返回 404 AppError
- **AppState 扩展**：加 `transfer_repo: Arc<TransferRepo>`、`transfers: Arc<TransferRegistry>`、`app_handle: AppHandle`（axum 接收 handler emit 用）。
- **Send 边界注意**：标准 `RwLockReadGuard` 非 Send，跨 await 持有会破坏 `tokio::spawn` 的 Send 约束；`run_send_loop` 取 devices、`handle_init`/`finalize_transfer` 取 config 时，均在 await 前 clone 出字段释放 guard（已踩坑修复）。

## M9 已落地行为约定（打包发版：bundle 配置 + 版本号单一来源 + bump 脚本 + 三平台 CI workflow）

- **bundle 配置（tauri.conf.json）**：
  - `targets: "all"` —— Tauri 按当前构建平台自动选择本平台产物（macOS→dmg/app、Windows→nsis/msi、Linux→appimage/deb）。CI 三平台矩阵各跑本平台，故 `"all"` 等效于列全三平台且不会跨平台报错。
  - `macOS.signingIdentity: "-"` —— **ad-hoc 签名**（开发/测试用，免 Apple Developer ID）。**正式分发需后续配 Apple Developer ID 签名 + notarization**（M9 不做，用户后续配置）。
  - `windows.wix.language: ["en-US","zh-CN"]` —— MSI 安装包中英文双语。
  - `publisher: "Claude Partner"`、`category: "Productivity"` —— 安装包元数据。
  - `icon` 数组覆盖三平台（32x32.png/128x128.png/128x128@2x.png/icon.icns/icon.ico），无需额外生成。
- **版本号单一来源 + 同步**：`tauri.conf.json.version`（当前 0.5.0）是唯一来源。`Cargo.toml.version` **必须与之完全一致**（Tauri build 强制校验，不一致会告警/失败，M9 已将 Cargo.toml 从 0.1.0 同步到 0.5.0）。`web/package.json.version` 跟随同步（前端构建元数据一致）。
- **bump 脚本（`scripts/bump-version.mjs`）**：发版时统一升级三处版本号，避免漏改。用法 `node scripts/bump-version.mjs <新版本号>`（如 `0.6.0`），内部正则替换三文件 version 字段并回读校验，支持语义化版本含预发布号（如 `1.0.0-beta.1`）。**禁止手动改单个文件版本号**，必须走 bump 脚本。
- **CI workflow（`.github/workflows/release-tauri.yml`）**：
  - 触发：`push tags: ['v*']`。
  - 旧的 Python/PyInstaller `release.yml` 已于 M10 删除，现在仓库为纯 Tauri 结构，推 `v*` tag 只跑这一套 Tauri 构建。
  - 用 `tauri-apps/tauri-action@v0` 官方 action，矩阵 `macos-latest`(`--target aarch64-apple-darwin`) + `windows-latest` + `ubuntu-22.04`，`fail-fast: false`。
  - 步骤：checkout → setup-node 20 → Rust stable（macOS 装 aarch64 target）→ Linux 装 webkit2gtk-4.1-dev 等依赖 → `cd web && npm ci` → tauri-action 构建+签名+上传 Release。
  - tauri-action 自动生成 `latest.json`（含各平台签名后下载 URL + signature，供 M8 updater endpoint），并 merge 多平台矩阵结果。
  - `updaterJsonPreferNsis: true` —— Windows updater 用 nsis 安装包（非 msi）作下载源。
- **签名 secret（用户待配）**：tauri-action 引用 `${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}`。用户需把 `~/.tauri/claude-partner.updater.key` 的**内容**配到 repo 的同名 secret（Settings → Secrets and variables → Actions）。**M8 用空密码，故无需配 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`**。未配 secret 时 CI 构建不签名、latest.json 无 signature，updater 校验会失败。
- **发版流程**：1) `node scripts/bump-version.mjs <新版本号>`（同步 tauri.conf.json + Cargo.toml + web/package.json）；2) 提交；3) `git tag v<版本号> && git push origin v<版本号>` 触发 CI。

## 关键约定

- **数据兼容**：直接读写旧 `~/.claude-partner/data.db`，迁移 SQL 全用 `CREATE TABLE IF NOT EXISTS`，保用户数据。`tags`/`vector_clock` 仍是标准 JSON TEXT（与 Python `json.dumps` 互通）；`datetime` 需兼容有无时区偏移两种格式。
- **版本号单一来源**：`tauri.conf.json` 的 `version`；Rust 用 `env!("CARGO_PKG_VERSION")`；前端 `useAppVersion` 经 invoke 获取，禁止硬编码。发版时统一用 `scripts/bump-version.mjs` 同步三处（tauri.conf.json / Cargo.toml / web/package.json），详见 M9 节。
- **serde 对齐前端**：所有返回给前端的 struct 用 `#[serde(rename_all = "camelCase")]`。
- **迁移参照**：各模块移植自 Python 版（M10 已删除），算法逻辑（向量时钟、选区、分块协议）逐字等价；各 M1–M8 节的"对照 Python xxx"注释是迁移期的行为基线说明，保留作设计意图记录。
- **事件替代 Qt 信号**：后端 `app_handle.emit("transfer:progress", ...)` 等，前端 `listen(...)`。
