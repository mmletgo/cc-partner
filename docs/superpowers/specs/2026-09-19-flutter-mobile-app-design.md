# Flutter 原生移动客户端

- 日期：2026-09-19
- 状态：已实现（P0–P6 工作台面板与交互已落地；2026-09-19 完成第一轮与网页 `/mobile` 对齐；2026-09-20 完成第二轮对齐：终端会话关闭/窗格/全屏/SGR 触控滚动转发/惰性历史 hydration/内建 Git 动作与 hook 修复、壳层实验开关与中文分组、Git Sync 与 mutation 对账、worktree 状态卡与前缀创建自动开终端、自动化任务块/runtime 快照条/experiments/执行现场、传输恢复与对账轮询、Attention summary/freshness/分组/聚焦、文件元信息、项目失败重试；推送点开导航等原生集成保持 NOT VERIFIED）
- 上位文档：
  - [`2026-06-29-mobile-workbench-design.md`](./2026-06-29-mobile-workbench-design.md)
  - [`docs/prd.md`](../../prd.md) §2.15 移动端 Workbench、§2.17 Inbox、§2.20 Provider
  - [`docs/p2p-protocol.md`](../../p2p-protocol.md)

## 1. 文档地位

本 Spec 定义 **cc-partner 的正式 iOS/Android 客户端**。它不替换、不修改网页 `/mobile` SPA。浏览器入口继续给扫码、应急和未装 App 的手机使用。

冲突优先级：

1. 本文件中的已确认决策
2. 本文件
3. 现有 `/mobile` 与 Workbench 领域 Spec（协议与业务语义）
4. 更早文档

实现时：网页 `web/src/mobile/` **零改动**（除非后续另开评审）。工作台协议复用现有 `/api/mobile/*` 与 `/api/workbench/*`。本轮唯一新增的 LAN 协议是推送登记。

## 2. 已确认决策

| # | 决策 | 说明 |
|---|------|------|
| D1 | 正式客户端 | 桌面图标、前后台、原生相册/文件/推送；不是「把网页加到主屏幕」 |
| D2 | 仅局域网操作 | 工作台 HTTP/WS 只打同一可达网里的 PC；不提供公网穿透、VPN 内置或远程操作通道 |
| D3 | 分发 | 给 cc-partner 用户的内部包 / TestFlight；第一版不上 App Store / Play |
| D4 | 无登录 | 固定 LAN 边界不变；不新增身份鉴权、配对码、LAN 模式开关 |
| D5 | Flutter 全自绘 | 一套代码出 iOS/Android；工作台用 Dart 自绘，不内嵌 `/mobile` WebView 当壳 |
| D6 | 网页冻结 | 不改 `web/src/mobile/` 来迁就 App |
| D7 | 多 PC 地址簿 | 手填 / 粘贴 URL / 扫桌面现有二维码；同时只连一台做工作台 |
| D8 | 文件富预览 | 对标桌面文件工作区：代码 / Markdown / HTML / 图片 / CSV / SQLite |
| D9 | HTML 预览 | 对标桌面 `WorkbenchHtmlPreview`（源码/预览/分栏 + 资源 data URL + 空 sandbox）；不是 Browser 工作区 |
| D10 | Provider | 只操作当前 PC 上已有 cc-switch provider；不在手机上安装 CLI |
| D11 | 推送 | App 被杀也能收到；地址簿里**每一台**已保存 PC 都能发；APNs/FCM 密钥只在推送中转 |
| D12 | 第一版不做 | 项目笔记、手机 mDNS 发现、跨手机同步地址簿、非图片 attach-to-agent。自动化看板与 Browser live preview 已纳入与网页 `/mobile` 对齐的工作台面板 |

## 3. 用户结果

完成后，用户能够：

- 安装一份 Flutter App（iOS TestFlight 或 Android 内部包），在地址簿里保存多台 PC 的 `http://IP:端口`。
- 扫描桌面现有「手机访问」二维码（内容仍是 `http://…/mobile`）或手填主机/端口，把该 PC 加进列表并切换。
- 在当前 PC 上使用与 `/mobile` 同语义的工作台：项目、待处理、终端、文件（含富预览）、Git/worktree、自动化、浏览器 live preview、传输、Provider、设置。
- 用系统相册给 Agent 贴图，用系统文件选择器走主机中转传输。
- 在 App 被杀掉时，收到地址簿里任一台已配置推送的 PC 的待处理/Agent 等输入通知；点开后若仍在该局域网则切到对应 PC 并导航到目标。
- 继续用手机浏览器打开 `/mobile`，行为与现在一致。

## 4. 范围

### 4.1 包含

- 顶层 Flutter 工程 `mobile/`（iOS + Android）。
- 本地地址簿、当前服务器、每台 PC 上次工作台位置。
- 对当前 `baseUrl` 的原生 HTTP/WebSocket 客户端（省略 Origin，Host 为该入口主机和实际端口）。
- 工作台面板：项目、待处理、终端、文件工作区、worktree/Git、自动化、浏览器、传输、Provider、设置。
- 推送登记 LAN 路由 + PC 侧触发 + 互联网中转发送。
- 桌面 Settings 增加「移动推送中转」配置（URL + 凭据）；未配置则不发送。

### 4.2 不包含

- 修改网页 `/mobile` UI 或给它加 Capacitor/JS 桥。
- 用 WebView 打开 `/mobile` 充当工作台（HTML 文件预览的独立 WebView 除外，见 §8）。
- 公网工作台、内置 VPN、把推送当远程操作通道。
- 身份登录、设备配对、可切换 LAN 模式。
- 项目笔记。自动化看板与 Browser 工作区已与网页 `/mobile` 对齐。
- 桌面 Finder「attach 非图片文件进 Agent」（网页 `/mobile` 也没有）。
- 手机作为独立 P2P 节点 / mDNS 广告。
- 上架 App Store / Google Play。

## 5. 架构

```
[Flutter App]
  ├─ 地址簿 / 扫码 / 推送 token（本地）
  ├─ 当前 PC：原生 HTTP + WS  →  http://<lan-ip>:<port>/api/...
  └─ APNs / FCM ← 推送中转 ← 各已登记 PC（仅通知，不含终端字节）

[PC cc-partner-backend]
  ├─ 现有 /api/mobile/* 、 /api/workbench/* （网页 /mobile 与 App 共用）
  ├─ 新增 /api/mobile/push/register|unregister
  └─ Attention 变化 → 中转 notify（需 config 中转）

[网页 /mobile]
  └─ 不变
```

三个客户端，一份局域网协议：

| 客户端 | UI | 传输 |
|---|---|---|
| 桌面 Tauri | `web/` invoke | 本机 sidecar |
| 手机浏览器 | `web/src/mobile/` | 同源 HTTP |
| Flutter App | `mobile/` Dart | 原生 HTTP，指向地址簿当前项 |

LAN 守卫：原生请求允许无 Origin；普通浏览器跨域仍拒绝。App 不得给业务请求伪造网页 Origin。Host 必须落在该 PC 的 allow-list 且端口为实际监听端口。

工作台流量只走 LAN。互联网例外仅推送中转：PC → 中转 → APNs/FCM，载荷只有类别、项目名、导航 id，不含终端输出、路径、Prompt、文件内容。

## 6. 地址簿与切服务器

本地存储（非 SQLite 也可；实现用 `shared_preferences` + 文件，或 `sqflite` 一条表）。

**ServerRecord**

| 字段 | 含义 |
|---|---|
| `id` | 手机侧稳定 UUID |
| `name` | 用户标签；空则显示 PC `deviceName` |
| `host` | 主机或 IP |
| `port` | 默认 `62116` |
| `baseUrl` | 规范化 `http://host:port`（去尾斜杠） |
| `pcDeviceId` | 最近一次 health / access-info 得到的设备 id，可空 |
| `deviceName` | 最近探测名 |
| `protocolVersion` | 最近 health |
| `capabilities` | 最近 health 列表 |
| `lastHealth` | `online` / `unreachable` / `unsupported` |
| `lastUsedAt` | |
| `lastLocation` | `{projectId, panel, worktreeId, sessionId}` 可空 |

另存：`activeServerId`、`mobileDeviceId`（安装后生成一次，用于推送登记）。

**添加**

- 手填 host + port。
- 粘贴 `http(s)://host:port/...`：只取 host/port；路径 `/mobile` 丢弃。
- 扫描桌面现有二维码：同样只取 host/port。
- 保存前 `GET {baseUrl}/api/health`。成功则写入设备名、`device_id`（若响应有）、capabilities。失败允许**强制保存**，`lastHealth=unreachable`，不能把强制保存画成在线，并展示探测失败原因。
- 规范化后的 `host:port` 去重；重复则更新已有行并选中。
- 拒绝空 host。`https` 第一版不作为正式入口（现网 `/mobile` 是明文 HTTP）。模拟器可用 `10.0.2.2` / 本机调试地址；产品文案不把 `127.0.0.1` 当正式推荐。

**切换**

- 同时只有一台当前服务器。
- 切换：拆掉该机全部 HTTP/WS，清空内存终端缓冲与文件脏态（脏文件先走与 `/mobile` 相同的保存/丢弃拦截），再按目标 `lastLocation` 连接。
- 其它 PC **不**保持工作台热连接。
- 推送登记与工作台分离：不因切到 A 而 unregister B。

**探测**

- 打开地址簿、进入前台、下拉刷新：对全部已保存项做 health（有界并发 3，best-effort）；切网后回到前台同样再探。
- 协议过旧或缺关键能力：`unsupported`，可进设置/地址簿，工作台面板 fail-closed。

**不做：** 手机 mDNS browse、iCloud 同步地址簿、把多条网卡 IP 自动合成一条（用户可手动加 wifi/有线两个入口）。

列表页与设置页固定风险文案：同一可达网络中的任何设备均可读取、写入和执行；系统不验证调用者身份。不得写「已认证/可信/安全设备」。

## 7. 工作台信息架构

对齐 `/mobile` 双模式，不另发明导航。

- **全局：** 项目、待处理、传输、设置、Provider。
- **项目内：** 终端、浏览器、文件、Git、worktrees、自动化；快捷入口：待处理、传输、设置。
- 当前 PC 是主机：项目列表、本机目录浏览、经主机跳到其它电脑，与现在 `/mobile` 相同。
- 切地址簿 = 换主机，A 的项目不能带到 B。

面板与网页 `/mobile` 的语义对齐点见 §2 表；项目笔记仍不进主导航。

Provider：先 `GET /api/health` 看 `provider-manager.v1`，没有则 unsupported。只操作当前 PC。缺 CLI 只读提示。DTO 不带 API key。不在手机触发 `install-cli`。

## 8. 文件工作区

协议：现有 `files/list-dir`、`open`、`save-text`、SQLite 选表。不新开文件协议。

| 类型 | 第一版 |
|---|---|
| 代码 | 语法高亮编辑器，可保存（对标 CodeMirror，用 Flutter 编辑器） |
| Markdown | 源码 / 渲染 / 分栏 |
| HTML | 对标桌面 `WorkbenchHtmlPreview`：源码 / 预览 / 分栏；相对资源改写为 data URL；预览用**仅服务于该文件**的 WKWebView / Android WebView，`javaScriptEnabled=false`，对齐 `sandbox=""` + `referrerPolicy=no-referrer`；失败留在源码并说明 |
| 图片 | 只读预览 |
| CSV | 完整表格横滑，不截 12 行 |
| SQLite | 选表 + 行预览，对标 `WorkbenchSqlitePreview` |
| 其它 | 只读说明 |

脏文件切换项目/worktree：先保存或丢弃，对齐现有 mobile dirty guard。

HTML 预览 WebView **不是** App 壳，禁止导航到 `/mobile` 或任意 http(s) 工作台。这是文件预览引擎例外，不推翻 D5。

Browser 工作区（dev server live preview，脚本开启）与 HTML 文件预览（脚本关闭）必须分两条通道，禁止混用。

## 9. 终端

不新开终端 API。当前 PC：

- 输出：`GET /api/workbench/events` NDJSON（heartbeat、`gap`）
- 回放：`sessions.replay`（含 `refreshHistory=true` 拉 tmux 历史）
- 输入：`/api/mobile/workbench/terminal-input-stream`，子协议 `cc-partner.terminal-input.v1`
- 贴图：`POST /api/mobile/workbench/sessions/paste-image`

画面用 `xterm.dart`（或同等 Canvas 终端），`TERM` 仍由 PC 侧 PTY 决定。颜色、备用屏幕、鼠标追踪按真机对 Agent TUI 验收；对不上的项在设置写明限制，**禁止**改网页 xterm 来迁就 App。

输入 ACK 只表示 PTY write+flush，不挡下一帧。断线时未 ACK 输入结果未知：**禁止自动重放**，禁止回退 `/sessions/write`。

额外按键、`/` 长按滑出、方向键连发：对齐 [`2026-08-06-mobile-terminal-extra-keys-design.md`](./2026-08-06-mobile-terminal-extra-keys-design.md) 与 [`2026-08-20-mobile-slash-popup-keys-design.md`](./2026-08-20-mobile-slash-popup-keys-design.md)。

输出：完整 NDJSON 行；15s heartbeat；35s 无帧拆连接再带 `afterOwnerInstanceId`+`afterSequence` 重连。收到 `gap` 必须停 live → 列 session → replay → 权威快照覆盖。从后台回前台立刻 abort 半开流并重建输入 WS，不重放未 ACK。

会话：切窗口 hidden 保活；进程内最多 8 个终端，LRU，当前窗口永不淘汰。切地址簿拆全部。每台 PC 持久化 `lastLocation`。

复制：长按自管选区 → 系统剪贴板，对齐 [`2026-08-23-mobile-terminal-copy-paste-image-design.md`](./2026-08-23-mobile-terminal-copy-paste-image-design.md)。贴图：折叠操作最上方按钮打开系统相册，选完立刻 POST，不预览、不裁剪。

滚动/history hydration：对齐 PRD「移动端终端滚动恢复」语义（首次向历史滑动才 `refreshHistory`；失败不卡死）。

## 10. 相册、文件选择、传输

手机不是 P2P 节点。走 `/api/mobile/devices` 与 `/api/mobile/transfer/*`。任务 JSON 不得带主机 path。

| 入口 | 用途 |
|---|---|
| 系统相册 | 终端贴图 → paste-image |
| 系统文件选择器 | 传输发送 → init/chunk/complete |

文件夹不作为发送源。选出文件后立刻上传；发送按钮只作失败兜底。目标列表主机合成「这台电脑」置顶 + 当前对端，不含虚拟「手机」。幂等与 chunk 合同同现网；禁止传输层盲重试 chunk。

下载：已完成 Receive，以及电脑发给 `cc-partner-mobile-inbox` 的 Send+completed，用系统保存面板写入用户可见位置（iOS Files / Android 存储）。不提供 Open/Reveal。其它电脑之间的 Send 不可下载。

相册权限与文件权限分离。切服务器即换主机上的任务；不跨主机续传同一 staging id。

## 11. 推送

### 11.1 能力与路由

新能力 `mobile.push.v1`，与下列路由同发，写入 `server_protocol_info()` / `docs/p2p-protocol.md`（实现时走现有 7 步清单）。协议协商，不是鉴权。

| 方法 | 路径 | 幂等 | 说明 |
|---|---|---|---|
| POST | `/api/mobile/push/register` | naturally-idempotent | upsert 该 `mobileDeviceId` 的 token |
| POST | `/api/mobile/push/unregister` | naturally-idempotent | 按 `mobileDeviceId` 删除 |

Register 体（camelCase）：

```json
{
  "mobileDeviceId": "uuid",
  "platform": "ios",
  "token": "apns-or-fcm-token",
  "appBuild": "1.0.0+1"
}
```

`platform` 为 `ios` | `android`。无 Origin 的原生请求允许。缺能力 = 这台 PC 不支持推送，App 标 `unsupported`，不假装已登记。

SQLite 表（实现时 migrations）：`mobile_push_registrations`，主键 `mobile_device_id`，列 `platform, token, app_build, updated_at`。不存终端内容。

### 11.2 登记策略

App 进入前台、token 刷新、网络变化：对地址簿里每一台 **health 在线且宣称 `mobile.push.v1`** 的 PC 有界并发 register。单台失败不影响工作台。删除地址簿条目时 best-effort unregister。

### 11.3 中转

只有中转持有 APNs/FCM 密钥。密钥不进 git、不进桌面/手机二进制。

PC `config.json`（实现时经 ConfigRuntime 事务写盘，脱敏诊断）：

```json
{
  "mobilePush": {
    "relayUrl": "https://push.example.internal/v1/notify",
    "relayToken": "(redacted)"
  }
}
```

未配置：register 仍成功，发送跳过；桌面 Settings 与 App 标明「这台 PC 未配置推送」。中转具体主机第一版只留配置，不绑定某一公有云；内部发版时再填。

PC → 中转 HTTPS POST，Authorization 用 `relayToken`。体包括 `platform, token, collapseId, payload`。`payload` 只含 `pcDeviceId, category, title, body, projectId?, sessionId?, worktreeId?`。collapse 按 `(pcDeviceId, mobileDeviceId)` 覆盖，避免刷屏。

中转失败：PC 打日志（无 token 全文），不重试风暴；下一次新的 Attention 变化再发。无效 token 删除该行。

### 11.4 触发

与手机 Inbox 同一集合（待处理 / Agent 等输入 / 失败等；**不含** tmux 依赖）。短时间合并（建议 ≥30s 或系统 collapse）。文案到类别和项目名为止。

### 11.5 点开

匹配地址簿 `pcDeviceId`（没有则退回 last-known `baseUrl`）→ 切服务器 → 按 Attention 只导航。不在局域网：提示连回该网，不改当前服务器以外的工作台连接。匹配不到：只展示通知。

LAN 无登录：同一网内其它设备也可以 register 自己的 token 收到该 PC 的通知。风险文案写明。不因此加鉴权。

## 12. 错误处理

| 场景 | 行为 |
|---|---|
| 当前 PC 不可达 | 工作台只读错误 + 回地址簿；不清地址簿 |
| health 无关键能力 | 该面板 unsupported，不猜旧接口 |
| 强制保存的离线 PC | 不能进入工作台写路径 |
| 输入 WS 断 | 不重放未 ACK；提示重连 |
| events `gap` | 必须 resync，禁止忽略 |
| 文件 save 冲突 / hash | 现有信封，保留草稿 |
| 传输 timeout | 对账 operation id，禁止 blind retry |
| 推送登记失败 | 该服务器行标记，不挡工作台 |
| 中转未配置 / 失败 | 不发推送，不回滚 Attention |
| 权限拒绝相册/文件 | 只禁用对应按钮 |
| HTML 资源改写失败 | 留源码 + 说明 |
| 切服务器时文件脏 | 先确认保存/丢弃 |

错误展示：阻断失败恰好一次 alert；可恢复用 status。不把 Host/Origin 拒绝说成「未授权登录」。

## 13. 测试

分层对齐 `docs/development/testing.md`。未跑的真机保持 `NOT VERIFIED`。

**Dart（mobile/）**

- 地址簿：URL 解析（含 `/mobile` 路径丢弃）、host:port 去重、强制保存 ≠ 在线。
- 切服务器：拆连接、不清其它 PC 的推送意图、`lastLocation` 读写。
- HTTP 客户端：不附加 Origin；Host 来自 baseUrl。
- 终端：gap 必须走 replay；未 ACK 输入不得重放；后台回前台 abort+重建 WS。
- 文件：dirty guard；HTML sandbox 标志（JS off）。
- 推送 payload：无终端字节；Inbox 过滤不含 tmux。

**Rust**

- `mobile.push.v1` 与路由同发。
- register upsert / unregister。
- 无 Origin 的 LAN peer 可登记；hostile Host 拒绝。
- 发送路径：未配置中转不发；载荷无 path/prompt/terminal。
- 现有 `lan_trust_boundary_smoke` 覆盖新路由。

**文档 / 清单**

- `docs/p2p-protocol.md` + `check-p2p-route-inventory.mjs`
- `server_protocol_info()` 字典序
- PRD / 根 `AGENTS.md` 目录地图（`mobile/` 落地时）
- quality-matrix 增补 L3 行：真机终端、相册贴图、多 PC 切换、推送（未跑保持 NOT VERIFIED）

**明确不在 CI 宣称**

- 真实 APNs/FCM 投递
- 真实 iOS/Android 相册、文件、ATS、后台保活
- 双机 LAN + 手机的端到端（L3）

## 14. 仓库、工程与发版

```
cc-partner/
├── mobile/                 # Flutter 工程（实现时创建）
│   ├── lib/
│   ├── test/
│   ├── ios/
│   └── android/
├── web/                    # 桌面 + /mobile SPA，本轮不改 mobile UI
└── src-tauri/              # 推送登记 + config
```

- Flutter 当前稳定渠道；iOS 16+、Android 8+。
- iOS ATS：允许局域网明文 HTTP（`NSAllowsLocalNetworking` 及实现所需例外）。iOS 14+ 必须声明 `NSLocalNetworkUsageDescription`；建议同时声明 `NSBonjourServices` `_cc-partner._tcp`，启动时短时 browse 以弹出系统授权框（不把手机做成 mDNS 发现客户端）。Android cleartext 仅调试/内部包所需范围，不把任意域名改成明文。
- 内部包签名：Apple/Google 开发者账号与证书不入库；CI 可后补 TestFlight workflow，第一版允许本机构建。
- 根 `AGENTS.md` 目录地图在 `mobile/` 创建时加一行职责；分层指令可下沉 `mobile/AGENTS.md`。
- 版本号：手机包版本与桌面 `tauri.conf.json` 不必同一数字，但 Settings 应显示 App build，health 显示 PC 版本，便于排错。

推送中转是独立小服务（实现语言不限），不进本仓库也可；若进本仓库则放 `push-relay/` 并在本 Spec 补一节。第一版最小要求：HTTPS + token + APNs/FCM 发送 + 不记录 payload 正文。

## 15. 实现切分

整包太大，不在一份 2–5 分钟任务计划里一次做完。落地时按阶段各写一份 `docs/superpowers/plans/`：

| 阶段 | 可演示结果 |
|---|---|
| P0 | `mobile/` 工程、主题/i18n、HTTP 客户端、地址簿 CRUD、health、扫码解析 |
| P1 | 切服务器、项目列表/打开/移除、待处理只导航、设置与风险文案 |
| P2 | 终端（WS/events/gap/extra keys/复制/相册贴图） |
| P3 | 文件工作区（含 HTML 桌面标准预览） |
| P4 | worktree / Git |
| P5 | 传输 + 系统文件选择器 |
| P6 | Provider |
| P7 | `mobile.push.v1` + 中转 + 全地址簿登记 + 点开导航 |
| P8 | TestFlight / Android 内部包、L3 清单 |

每阶段单独可测。P0 即可解决「多 PC 入口」；P2 起才是日常工作台。

## 16. 实现时必须同步的文档

- `docs/prd.md`（本轮规划已写入 §2.21 草案）
- `docs/p2p-protocol.md` + 能力表（P7）
- 根 `AGENTS.md` 目录地图（`mobile/` 创建时）
- `docs/development/quality-matrix.json` L3 行
- 桌面 Settings 文案：推送中转配置 + 风险（同一网可登记 token）
