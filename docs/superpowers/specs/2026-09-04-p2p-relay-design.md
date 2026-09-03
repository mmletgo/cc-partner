# 远程项目中转访问（跳板机）设计方案

> 状态：设计稿，待评审
> 日期：2026-09-04
> 范围：Workbench 远程项目全链路（项目/文件/Git/终端/事件流/浏览器预览）经中转设备访问；Transfer P2P 传输与 sync 同步列为后续扩展

## 1. 背景与目标

### 1.1 问题场景

当前远程项目要求发起方 A 与目标方 C **IP 直连可达**（mDNS 发现或 manual_peers 手动 IP）。实际局域网中存在 A、C 互相不可达、但共享一台可达邻居设备 B 的拓扑：

- 公司网络按 VLAN/AP 隔离，B 是横跨两个网段的机器（双网卡 / 接了两个 Wi-Fi）
- 家里访客网络与主网络隔离，某台台式机同时接了路由器 LAN 口和访客网
- A 与 C 之间有防火墙策略，但都允许访问 B

此时 A 打不开 C 上的远程项目。本方案引入**中转（relay/跳板）**：A 的对 C 流量经 B 转发，`A → B → C`。

### 1.2 与现有机制的关系（互补，不替代）

| 机制 | 适用场景 | 与本方案关系 |
|------|---------|-------------|
| mDNS 自动发现 | 同一广播域 | 不变，仍是一切直连的基础 |
| manual_peers（手动 IP + Tailscale overlay） | A、C 之间 IP 可达但 mDNS 不可见 | 本方案的 via 候选来源之一；跳板设备 B 本身可以是 mDNS 设备或 manual peer |
| 移动端二级代理（`/api/mobile/workbench/*`） | 手机 → 本机 A → 远端 C | 复用其验证过的"本机代访远端"模式；手机场景下自动组合成三跳 `手机 → A → B → C`，零额外开发 |

### 1.3 目标

1. A 经 B 打开并完整操作 C 的远程项目：项目列表、文件读写、Git/worktree、终端（HTTP 控制 + WS 输入 + NDJSON 事件流）、浏览器预览、Orchestrator
2. **C 零感知、零改动**：C 不需要升级版本、不需要任何配置，老版本 C 也能被中转
3. 链路自动迁移：C 一旦对 A 直连可达（进入 A 的 mDNS 表），自动回到直连，已打开的远程项目 shortcut 无需重建
4. 严守项目固定 LAN 边界：不引入身份鉴权 / LAN 权限 capability token / 可切换 LAN 模式

## 2. 可复用的现有资产

| 资产 | 位置 | 复用方式 |
|------|------|---------|
| 地址解析单点 `device_base_url` | `src-tauri/src/commands/workbench/common.rs:987` | 唯一注入"中转路由层"的位置 |
| request_id 跨跳透传预留 | `workbench/remote_client.rs:157` `with_forwarded_request_id`（现 dead_code，注释即"留给未来 relay 接入"） | B 转发时直接启用 |
| expected-device fail-closed 绑定 | `remote_client.rs:192` `ensure_expected_device_binding` + `net/lan_guard.rs:716` guard | 语义保持，透传即天然工作 |
| 远端 WS 桥接模式 | `workbench/browser_proxy.rs`（remote_relay，axum WS server + tungstenite client） | 提取为通用 WS relay |
| NDJSON 流式客户端 | `net/peer_client.rs:315` `open_ndjson_stream`（no-timeout stream client 先例） | 流式转发的超时模型参照 |
| per-device 终端 WS 链路缓存 | `workbench/terminal_input.rs:351` `peer_link_for_device` | 出站 URL 走统一路由解析后自动经 B |
| manual_peers 周期探测模式 | `net/manual_peers.rs:110`（15s health 探测、failure threshold、overlay 白名单） | A 侧影子设备探测节奏与数据流参照 |
| LAN fleet 可达性聚合 | `workbench/lan_fleet/collector.rs:382`（semaphore=3、5s 超时、`FleetReachability`） | B 端 `/api/relay/peers` 的可达性模型参照 |
| `remote:<device>:<inner>` 复合 ID | `workbench/remote_ids.rs:48` | 不变；shortcut 只存 target device_id，天然与链路解耦 |

## 3. 核心设计

### 3.1 总体链路

```
手机(可选)          A（发起方）              B（中转设备）           C（目标设备）
  │  同源 HTTP/WS       │                       │                      │
  ├──────────────────►│  base_url 路由解析      │                      │
  │  /api/mobile/*    │  C 不可直连、via=B      │                      │
  │                    │  ──► http://B:port/api/relay/{C}/api/...      │
  │                    ├──────────────────────►│ 剥 /api/relay/{C} 前缀 │
  │                    │   普通业务请求          ├─────────────────────►│ C 的既有路由
  │                    │                        │  B 直连表查 C 地址     │ /api/workbench/* 等
  │                    │◄──────────────────────┤◄─────────────────────┤
```

对 C 而言，B 就是一个普通 LAN peer 发起的普通请求（socket IP 是 B 的内网 IP、Host 是 C 自己的地址、原生客户端无 Origin、`X-Cc-Partner-Expected-Device-Id: C` 由 A 发出并透传后恰好匹配）——**C 上所有现有中间件（lan_socket_gate / browser_request_guard / expected_device_id_guard / body limit / 错误信封）原样生效，无需任何感知**。

### 3.2 关键决策一：A 侧只改 base_url，不改任何业务调用

A 打开 C 的远程项目时，本地 shortcut row 仍是 `kind=remote, target_device_id=C`，owner helper、`RemoteWorkbenchClient`、ID 映射、事件桥、终端链路全部不动。唯一变化在地址解析：

```
device_base_url(state, "C") 的解析顺序：
  1. state.devices 直连表命中且 online ──────────────► http://{C.host}:{C.port}   （现状，永远优先）
  2. relay 影子表命中（C 经 via=B 可见且 B 可达）────► http://{B.host}:{B.port}/api/relay/{C}
  3. 都未命中 ──────────────────────────────────────► 失败："远端设备不在线"（fail-closed，现状语义）
```

由此获得的性质：

- **所有既有客户端自动获得中转能力**：`RemoteWorkbenchClient`（workbench）、`PeerClient`（NDJSON 事件桥）、`terminal_input` 的 WS 链路、`browser_proxy` 的 remote_relay、orchestrator remote client——只要它们的出站 URL 构造收敛到同一解析函数（实现时需逐一排查收敛点，见 §11 阶段 2）。
- **链路免费迁移**：C 的 shortcut 只存 device_id，不存链路；C 进入 A 的直连表后下一次解析自动走直连。
- **幂等键、request_id、错误信封全部原样**：幂等键本来就在请求 body 里（`clientRequestId` 等），透明转发即透传；错误信封由 C 生成、B 原样回传、A 的 `parse_peer_response` 原样解析。

### 3.3 关键决策二：B 端做"透明字节转发器"，不做逐路由代理

mobile 二级代理是逐路由手写 handler（复用 owner helper），那是因为手机场景下"本机同时是项目 owner"。跳板场景 B 不是 owner、不持有任何 Workbench 状态，逐路由代理既不可能也不必要。B 只做**方法 + 路径 + body 的透明搬运**：

- 一次实现覆盖 C 的全部现有及未来路由（受 §4.4 白名单约束）
- B 不解析 body、不缓存、不改写语义，仅剥路径前缀、透传 headers/body、回传 status/body

### 3.4 关键决策三：单跳硬限制（拓扑深度 = 1）

- B 的转发器出站 URL **只从 B 自己的直连表**（mDNS + manual_peers）解析目标地址，转发出的请求是普通 `/api/...` 路径、不可能再命中任何设备的 relay 前缀 → 结构上杜绝 `A → B → D → C` 多跳与环路
- B 拒绝 `target_device_id == B 自身`（防自引用）
- A 侧影子设备的 via 只能是 A 的直连设备（影子设备不能当跳板）

多跳链路（via 链）协议上不预留，确有需求时另行设计。

## 4. B 端协议设计（中转设备）

### 4.1 能力与端点

新增能力 token：**`net.relay.v1`**（能力声明 token，表示"本设备支持转发功能"，与 `workbench.dependency-install.v1` 同性质；**不是**权限 token，对全部 LAN peer 平等开放，符合固定 LAN 边界）。

| Method | Path | Retry class | 说明 |
|--------|------|------------|------|
| GET | `/api/relay/peers` | read-only | B 报告自己直连可见、且支持被中转访问的设备清单（DTO：`[{device_id, device_name, proto_version, capabilities, online}]`；**不含地址**——地址解析只发生在 B） |
| ANY | `/api/relay/{device_id}/*path` | no-transport-retry（继承目标路由语义，B 不自动重试） | 透明转发到 `{device_id}` 设备的 `*path` |
| GET | `/api/relay/{device_id}/api/workbench/terminal-input-stream` | no-transport-retry | WS upgrade 桥接（子协议 `cc-partner.terminal-input.v1` 透传），单独注册 |

`/api/relay/peers` 的内容 = B 的 `state.devices` 直连表过滤（online、具备 `workbench.projects.v1` 基础能力、非 B 自身）。C 端**不需要**任何新 token。

### 4.2 转发语义

**请求方向（A → B → C）：**

1. B 校验 `device_id` 在自己直连表且 online；未命中 → 404 信封 `unavailable` + `domain_code=relay_target_offline`（fail-closed，与 `device_base_url_from_devices` 同语义）
2. `*path` 受白名单约束（§4.4），不在白名单 → 403 信封 `forbidden` + `domain_code=relay_path_not_allowed`
3. 出站请求：剥 `/api/relay/{device_id}` 前缀得到 `*path`，方法不变，透传全部 headers（含 `X-CC-Request-Id`、`X-Cc-Partner-Expected-Device-Id`、`X-Chunk-Offset`、`Content-Type`、query string），剥除 hop-by-hop headers
4. **body 流式转发**：`reqwest::Body::wrap(axum body data stream)`，B 不缓冲（避免 32 MiB 级请求在 B 内存放大）
5. request_id 透传：B 出站沿用入站 `X-CC-Request-Id`（启用 `with_forwarded_request_id` 的预留语义），全链一个 ID 可追溯
6. 连接超时用 `PeerTimeoutClass::Health`(3s)；**不设总超时**（NDJSON 长流、上传都需要），靠 C 端各路由自身超时沿流传回终止

**响应方向（C → B → A）：**

1. C 的 status、headers、body 原样回传；错误信封（含 header 与 body 内 `request_id`）原样透传，A 侧 `parse_peer_response` 的 v1 校验（header/body request_id 一致性）自然通过
2. body 同样流式：`axum::body::Body::wrap_stream(resp.bytes_stream())`
3. C 连接失败 → B 生成 502 信封 `unavailable` + `domain_code=relay_target_unreachable`，并顺带把该 target 在 B 直连表的 `online` 置 false（加速收敛）

**WS 桥接（仅 terminal-input-stream）：**

- B 以 axum WS server 接受 A 的 upgrade（透传子协议协商头），以 tungstenite client 连 C 的 `/api/workbench/terminal-input-stream`（带 `X-Cc-Partner-Expected-Device-Id: C`）
- 双向帧透传，复用从 `browser_proxy.rs` remote_relay 提取的通用 WS 桥；断线时两侧同时关闭，重连由 A 侧既有机制（`peer_link_for_device` 缓存 + 上层退避）负责
- 终端输入的 ACK 语义（PTY write 后才 Ack、断线不重放未 ACK 输入）在 C 端，透传不破坏

**浏览器预览：** A 的 `browser_proxy` 对 C 的 preview 会话（`create_remote_relay(C)`）出站 URL 同样经统一路由解析，自动变成"B 转发 C 的 proxy"；其内部对上游 dev server 的 WS/HTTP 桥在 A 侧完成，B 只透传 A↔C 之间的 proxy 流量。

### 4.3 资源上限与防护

- relay 路由 body limit：全局 32 MiB（与 `DefaultBodyLimit` 现状一致）；`/api/transfer/chunk` 等更严格的 per-route limit 由 C 端最终强制，B 端不做路径级细分（MVP），后续可按 `*path` 前缀映射收紧
- 转发并发上限：全局 semaphore 8、per-target 4（参照 lan_fleet collector 的 semaphore 模式），超限 → 503 信封 `unavailable` + `domain_code=relay_busy`，防止 B 被当作流量放大器
- B 侧 config 开关：`~/.cc-partner/config.json` 增加 `relay_enabled: bool`（默认 true）；关闭后不宣告 `net.relay.v1`、不注册 relay 路由
- 中间件：relay 路由照常经过 `lan_socket_gate` / `browser_request_guard` / body limit / `envelope_fallback`，与其它业务 API 同一边界

### 4.4 转发路径白名单

为避免把双向同步类流量意外引入中转拓扑、并缩小攻击面，`*path` 仅允许：

```
/api/health
/api/workbench/*
/api/orchestrator/*
```

明确排除（并返回 `relay_path_not_allowed`）：`/api/sync/*`、`/api/prompts/*`、`/api/transfer/*`（transfer 中转列为 §10 后续扩展，届时单独评审其 960 KiB chunk 语义）、`/api/mobile/*`、`/api/backend/control/*`（control 平面本就禁止跨设备）。

### 4.5 `expected_device_id_guard` 的语义扩展

现状：入站请求带 `X-Cc-Partner-Expected-Device-Id` 且 ≠ 本机 device_id → 409。中转请求该 header 值是 **C** 而非 B，会被误杀。扩展为（仍 fail-closed）：

- 非 relay 路径：语义不变（必须等于本机 device_id）
- relay 路径：必须等于 URL 中的 `{device_id}`（即被中转目标），否则 409 `device_id_mismatch`

不满足"缺失即放行"的现状宽松度不变（LAN 无调用者身份校验的边界不动，这只是防错机写保护）。

## 5. A 侧设计（发起方）

### 5.1 配置模型

`~/.cc-partner/config.json` 新增（结构化持久化，迁移逻辑沿用现有 config 兼容规则）：

```jsonc
{
  "relay": {
    "enabled": true,             // B 侧角色：本机是否允许被用作跳板（默认 true）
    "via_device_ids": ["B"],     // A 侧角色：允许作为跳板的直连设备（UI 勾选或 CLI 添加）
    "ignored_target_ids": []     // 可选：从影子列表里显式忽略的目标（V1 可不实现 UI，仅留字段）
  }
}
```

配置写入有三条等价途径，落同一份 config：**GUI 设置页**（§7.1）、**headless CLI 子命令**（§8）、直接改文件（重启生效，不推荐）。经 control plane `update-config` 写入可热生效：影子探测任务与 manual peer 探测循环均周期重读内存配置（15s），无需重启 backend。

用户只需指定"信任哪些跳板"，**不需要逐个配置目标设备**——目标经跳板自动发现（§5.2）。若 via 设备从 A 的直连表消失，其名下影子设备全部转 offline。

### 5.2 影子设备（经 B 可见的 C）

- A 对每个 via 设备周期（15s，与 manual_peers 同节奏）调用 `GET {B}/api/relay/peers`
- 命中的目标合成为**影子条目**，存独立内存表 `AppState.relay_shadow_devices: HashMap<target_id, RelayShadowDevice>`（**不污染 mDNS 直连表**）：

```rust
struct RelayShadowDevice {
    target_device_id: String,   // = C
    via_device_id: String,      // = B
    device_name: String,
    proto_version: u32,
    capabilities: Vec<String>,
    online: bool,               // B 报告的 C 可达性 && B 自身直连可达
    last_seen: DateTime<Utc>,
}
```

- 探测任务复用 manual_peers 的数据流形态（周期 health、连续失败阈值移除、stop 时清理）
- 排除规则：影子目标不得是 A 自身、不得与 A 直连表重复（直连优先，重复即丢弃）

### 5.3 路由解析收敛

`device_base_url_from_devices` 改造为三段解析（§3.2），并在实现阶段排查收敛所有对 C 出站连接的 URL 构造点，确保统一走该函数：

| 出站通道 | 构造点 | 处理 |
|---------|--------|------|
| workbench HTTP | `RemoteWorkbenchClient`（`remote_client.rs:2075` `endpoint_url`） | base 注入点 |
| health/capability 预检 | `ensure_expected_device_binding` | base 注入点（health 经 B 转发后返回 C 的 device_id，绑定校验天然通过） |
| 事件 NDJSON | `remote_events.rs` → `PeerClient::open_ndjson_stream` | base 注入点 |
| 终端输入 WS | `terminal_input.rs:374` `connect_peer_link` | WS URL 同一 base |
| browser preview | `browser_proxy.rs` `create_remote_relay` | 上游 URL 同一 base |

**不做直连失败自动 fallback 到中转**：链路选择由"直连表是否命中"静态决定，避免健康抖动导致同一请求行为不确定。中转命中期间 C 若恢复直连，下一轮 mDNS upsert 后自动切回。

### 5.4 设备列表 DTO

`DeviceDto` 增加可选字段 `viaDeviceId?: string`、`viaDeviceName?: string`；`list_devices` 合并直连表 + 影子表（影子条目带 via 信息）。online 语义：影子条目 = B 可达 && B 报告 C online。

## 6. B 端部署与运行要求

**B 必须安装并运行 cc-partner（GUI 版或 headless 后端 CLI 均可），不能只是"SSH 能通"。** 链路上完全没有 SSH 参与：A → B 走的是 HTTP（首选 TCP 62116），B → C 走的也是 HTTP。SSH 服务器提供不了本方案依赖的三个环节：

1. B 上要有 cc-partner 的 axum HTTP server 提供 `/api/relay/*` 转发路由（能力 `net.relay.v1`）
2. B 必须能"看到"C —— B 自己的 mDNS browse / manual_peers 直连表里有 C 的地址（转发目标的地址来源）
3. A 必须能直连 B（A 的直连表里有 B）—— 这正是"B 是 A、C 共同可达邻居"的定义

### 6.1 部署形态

| 形态 | 适用场景 | 操作 |
|------|---------|------|
| GUI 桌面版（macOS/Win/Linux） | B 是平时有人用的机器 | 正常安装运行即可；默认 `relay.enabled=true`，装好即具备被用作跳板的能力，零配置 |
| Headless 后端 CLI | B 是台式机/小主机/无显示器服务器（最典型跳板机） | 安装包自带 CLI，执行 `cc-partner-backend start`（已有 start/status/doctor/stop/supervise；会 advertise mDNS + browse，不读 GUI bootstrap）。运维检查用 `cc-partner-backend doctor` 确认 UDP 5353 + TCP 62116 放行 |

Linux 远端分发已有现成管道：`scripts/docker-build-backend-linux.mjs` 在 Docker（rust:1.95-bookworm）内交叉编译 `x86_64-unknown-linux-gnu --locked`，产物 `src-tauri/target-linux/release/cc-partner-backend`；远端 Ubuntu 24.04 需要 webkit2gtk/gtk 运行库，随二进制部署 `web/dist`（`CC_PARTNER_WEB_DIST` 指定）。scp 二进制 + web/dist → `cc-partner-backend start`（或配 systemd/supervise 常驻）。

版本矩阵：**A、B 都需 ≥ 首个包含 `net.relay.v1` 的版本；C 任意版本（含老版本）**。

### 6.2 B 端设置

- 默认零配置（`relay.enabled=true`）。被用作跳板不需要 B 侧任何人机交互
- GUI 版：Settings → 依赖环境页的「中转访问」卡片提供开关（关掉即不宣告能力、不注册路由）与当前中转连接数展示
- Headless 版：`cc-partner-backend relay allow off`（§8），通常不需要动
- 风险告知：B 的操作者应知晓本机会为邻居设备转发 Workbench 流量（B 可见明文流量，与全 P2P 明文 HTTP 同等级）

### 6.3 为什么不采用 SSH 隧道方案（对比记录）

理论上 A 上 `ssh -L 62116:{C_ip}:62116 user@B` + manual_peers 指向 `127.0.0.1` 也能拼出可达性，但不纳入产品方案：需要 B 开 SSH + 密钥管理 + 每台 A 手动起隧道/保活；手机端（`/mobile`）无法使用；mDNS 不过隧道、设备列表永远"不可见"；B 故障与 C 故障在错误里无法区分（无 `relay_*` domain_code）；无能力协商。若用户的 B 与 A 之间连 62116 都不可达（只有 22 端口），则不满足"共同可达邻居"前提，本方案不适用。

## 7. 前端设计

### 7.1 设置跳板（一次性配置，A 端）

位置：**Settings（设置）→「依赖环境」tab → 新增「中转访问（跳板）」卡片**（与 LanFirewallDependencyCard、WorkbenchDependencyCard 同页，均为网络依赖性质；不新开 tab）。卡片内容：

1. 功能一句话说明 + 固定风险提示（LAN 无身份校验模型下，中转意味着流量途经跳板设备，跳板可见明文流量——与现有 LAN 边界同等级，但链路上多了一台设备）
2. 「添加跳板设备」选择器：仅列出**本机直连在线设备**（mDNS + manual peers，带 StatusDot/设备名/地址）；选中即调用后端写入 `relay.via_device_ids`
3. 已添加跳板列表，每行 = 设备名 + 地址 + 在线状态 + 「可见 N 台设备」+ 移除按钮；行可展开显示该跳板报告的影子目标清单（名称/在线状态）
4. 约 15s（探测周期）后，影子设备开始出现在全局设备列表

### 7.2 使用中转（打开远程项目，A 端 Workbench）

- Workbench → 打开远端项目 → `WorkbenchRemoteProjectPicker` 设备列表 = 直连设备（现状渲染）+ 影子设备（名称行追加 Pill「经 {B 设备名} 中转」，复用现有 StatusDot 在线状态；直连设备与影子设备是同一列表，影子设备用 Pill 区分而非独立分组——若同一目标既直连可达又经 B 可见，只显示直连条目）
- 点选影子设备 → 浏览远端目录 → 打开项目，后续终端/Git/文件/预览操作与直连远程项目**完全一致，用户无感**
- 影子设备 offline 时置灰，提示「中转设备 {B} 不可达或目标已下线」
- 项目栏 shortcut 副标题追加「中转」标记；错误文案按 `relay_*` domain_code 区分「跳板故障」与「目标设备故障」

### 7.3 手机端（零配置）

手机连 A 的 `/mobile` → `MobileProjectPicker` 设备列表同样合并影子设备（带中转 Pill，`filterOnlineLanDevices` 同样过滤 offline 影子）→ 直接点选，三跳链路（手机 → A → B → C）自动成立。

### 7.4 B 端（跳板机自身）

默认无需任何界面操作。GUI 版用户可在 Settings → 依赖环境页查看「本机正被用作跳板（当前 N 个中转连接）」与关闭开关。

### 6.3 Workbench 界面标记

- 远程项目 shortcut 行的设备副标题追加「中转」标记（数据来自打开时的链路解析结果，随解析动态）
- 终端/事件流报错时，若当前项目走中转链路，错误文案提示"经 {B} 中转"帮助定位（错误信封的 `domain_code=relay_*` 已可区分 B 侧与 C 侧故障）

## 8. Headless CLI 配置命令（cc-partner-backend 扩展）

headless 设备（跳板机 B、目标机 C、以及无 GUI 的发起方 A）必须全程可用命令完成中转配置与排查，避免 SSH 上去手改 config.json。命令风格照抄现有 CLI 契约：手写 match 分发（`backend/cli.rs:117 dispatch()`）、严格参数解析、稳定退出码（0 成功 / 1 失败 / 2 用法错误）、stdout 单行 JSON（`--json` 可 jq）、tracing 只进 stderr。

### 8.1 命令集

```
# —— 查询（只读，需 backend 运行中；数据来自内存设备表/影子表）——
cc-partner-backend devices [--json]
    # 直连设备表快照：device_id/device_name/host/port/online/proto_version/capabilities/source(mdns|manual|overlay)
    # 这是获取跳板 device_id 的途径
cc-partner-backend relay status [--json]
    # relay 配置（enabled/via 列表）+ 每个跳板的可达性 + 全部影子设备及其在线状态

# —— A 侧：跳板管理（写 config.relay.via_device_ids）——
cc-partner-backend relay via add <device_id|device_name>
    # device_id 精确添加（backend 未运行也可用，直接落盘）；
    # 传 device_name 则在当前设备表按名匹配（需 backend 运行中），多匹配/零匹配报错列出候选
cc-partner-backend relay via remove <device_id>

# —— B 侧：允许被中转（写 config.relay.enabled，默认 on）——
cc-partner-backend relay allow on|off

# —— 发现兜底：手动 peer（写 config.manual_peers，B 看不到 C 时用）——
cc-partner-backend peers add <host>[:port]     # port 缺省 62116
cc-partner-backend peers remove <host>[:port]
cc-partner-backend peers list [--json]
```

### 8.2 实现路径：control plane 热生效，离线落盘

- **backend 运行中**（读到 `backend-control.json` 且 pid 存活）：CLI 经现有 `BackendControlClient`（loopback + token POST）操作——
  - 写命令走既有 `POST /api/backend/control/update-config`（CAS generation），需扩展 `RuntimeConfigPatch`（`config_runtime.rs:620`）allowlist 增加 `relay` 与 `manual_peers` 两个可选字段（`deny_unknown_fields` 下新增可选字段向后兼容）；`apply_to` 落盘 + 内存 swap 后，探测循环下一轮（≤15s）即按新配置工作，**无需重启**
  - 读命令走新增 `POST /api/backend/control/devices`（返回直连表 + 影子表 + relay 配置快照；loopback-only、token 鉴权、body limit 与其它 control 端点一致）
- **backend 未运行**：写命令直接走 `FsConfigStore::save_atomic` 落盘 config.json（复用 `AppConfig::validate` 既有校验，含 manual_peers 去重规则 `config.rs:1328`），下次启动生效；读命令报错 exit 1 并提示先 `start`
- control plane 新端点必须同步 `check-p2p-route-inventory.mjs` 对账与 `docs/p2p-protocol.md` local control plane 清单

### 8.3 目标部署实例（mac=发起方 / nas-vpn=跳板 / power-vpn=目标）

```
# 1) power-vpn（C，目标机，零中转配置）——经 SSH ProxyJump nas-vpn 登录
scp cc-partner-backend + web/dist 上去（docker-build-backend-linux.mjs 产物）
power-vpn$ cc-partner-backend start && cc-partner-backend doctor   # 确认 62116 对 nas-vpn 可达

# 2) nas-vpn（B，跳板）——默认 relay allow on，唯一要确认的是"能看到 power-vpn"
nas-vpn$ cc-partner-backend start
nas-vpn$ cc-partner-backend devices            # 看 power-vpn 是否在列（mDNS 或 Tailscale overlay 自动发现）
nas-vpn$ cc-partner-backend peers add <power-vpn-ip>   # 仅当上一步看不到时

# 3) mac（A，GUI 发起方）
Settings → 依赖环境 → 中转访问 → 添加跳板 nas-vpn
（或 CLI：cc-partner-backend relay via add <nas-vpn 的 device_id>）
→ 设备列表出现 power-vpn（经 nas-vpn 中转）→ Workbench 打开远程项目
```

后续运维排查同样命令化：`relay status` 区分"跳板不可达 / 目标下线 / 目标不支持"；`devices` 看链路两端视图。

## 9. 移动端三跳链路（零开发）

手机经 `/mobile` 访问 A，A 中转访问 C：

```
手机 ──同源 /api/mobile/*──► A ──/api/relay/{C}──► B ──► C
```

- 手机端前端、`httpWorkbenchTransport`、A 的 mobile handler 链全部不变（它们只依赖 device_id 与复合 ID）
- A 的事件桥、终端 RemoteAware 网关的出站连接已按 §5.3 收敛，自动经 B
- 需在 mobile 端验证的仅是长链路下的断线重连表现（Gap 帧游标续读已有，做实测确认）

## 10. 明确不做 / 后续扩展

1. **多跳链（via 链）**：结构上禁止（§3.4），不预留
2. **直连失败自动 fallback 中转**：不做（§5.3）
3. **Transfer P2P 传输中转**：后续扩展。`/api/transfer/*` 加入白名单 + transfer chunk 流式转发即可复用本转发器，但需单独评审双倍流量与 staging 语义，V1 不开
4. **sync / prompts 双向同步中转**：不做。双向同步引入三设备拓扑（A↔C 经 B）的向量时钟行为需单独设计，且不是"远程项目访问"诉求
5. **C 端任何改动**：无（老版本 C 兼容，`net.relay.v1` 只要求 B 支持）
6. **鉴权 / 加密 / LAN 权限 token**：禁止（固定 LAN 边界）；relay 不改变威胁模型等级（全 P2P 本就是明文 HTTP，中转只是多一跳）

## 11. 分阶段实施

### 阶段 1：B 端转发器（Rust，约 700 行含测试）

- `src-tauri/src/net/relay.rs`：转发器核心（HTTP 透明转发 + 流式 body + semaphore + 白名单 + 错误信封）+ 通用 WS 桥（从 `browser_proxy.rs` 提取复用，`browser_proxy` 改为引用提取物）
- `net/routes/relay.rs`：`peers` / 转发 / WS 三路由 handler
- `protocol.rs`：`net.relay.v1` capability + config `relay_enabled`
- `lan_guard.rs`：`expected_device_id_guard` 的 relay 语义扩展（§4.5）
- `http_server.rs`：路由注册 + body limit + 并发 semaphore
- 单测：前缀剥离、header 透传（request_id / expected-device / chunk-offset）、白名单拒绝、target offline/unreachable 信封、guard 409 用例、流式不缓冲（大 body 峰值内存断言）

### 阶段 2：A 侧路由、影子设备与 headless CLI（Rust，约 750 行）

- `config`：`relay` 字段 + 迁移
- `state`：`relay_shadow_devices` 表
- 影子探测任务（复用 manual_peers 数据流形态）：周期拉 `/api/relay/peers`、合成/老化影子条目
- `device_base_url` 三段解析 + §5.3 五个出站通道 URL 构造收敛（含 health 预检）
- `list_devices` 合并影子条目 + `DeviceDto.viaDeviceId/viaDeviceName`
- **headless CLI（§8）**：`backend/cli.rs` 新增 `devices` / `relay status|via add|via remove|allow` / `peers add|remove|list` 子命令；`RuntimeConfigPatch` 扩展 `relay` + `manual_peers` 字段（热生效）；control plane 新增 `POST /api/backend/control/devices` 只读端点
- 单测：解析优先级（直连 > 影子 > 失败）、影子排除规则（自身/重复）、via 失效联动 offline、config 迁移、CLI 参数解析与退出码、patch 热生效（探测循环读到新配置）

### 阶段 3：前端（约 300 行）

- Picker 影子设备渲染（Pill 标记、offline 置灰）
- Settings 中转管理区块 + 风险提示文案
- 项目行/错误文案中转标记
- Vitetst：DTO 合并渲染、Picker 分组、Settings 交互；i18n 文案（`check:i18n` 通过）

### 阶段 4：集成验证与文档

- 三节点集成测试（单进程三端口 harness，参照 `lan_trust_boundary_harness.rs` 模式）：A(配 via=B) + B + C，覆盖 health 绑定、项目 list/open、文件读写、git commits、终端 write + WS 输入 + NDJSON 事件续读、browser preview、错误路径（C 下线→B 信封；B 下线→A 报远端不在线）
- 真机三设备 LAN 验收（跨网段拓扑）：按项目惯例标注 NOT VERIFIED 边界，进 `quality-matrix.json`
- 文档：`docs/p2p-protocol.md`（capability + 3 条路由 inventory 行 + retry class + relay 专项合同节）、根/`src-tauri`/`web` 三层 AGENTS.md、`docs/prd.md`

### 实施约束提醒

- 新路由必须过 `node scripts/check-p2p-route-inventory.mjs` 对账（router 字面量与 inventory 表一致）
- capability 加入 `server_protocol_info()` 字典序全集
- Workbench 页面改动遵守 hooks-before-early-return 与 1200 行硬顶；Settings 新区块走 controllers/views 拆分约定
- 测试命令统一 `./scripts/cc-partner-cargo.sh test --locked ...`

## 12. 风险与开放问题

| 风险 | 缓解 |
|------|------|
| `expected_device_id_guard` 扩展引入回归 | 保持 fail-closed 语义 + 单测覆盖非 relay 路径全部现状用例 |
| NDJSON/WS 长流经 B 断连 | A 侧 bridge 已有 Gap 帧 + 游标续读 + 指数退避，透传不破坏；集成测试覆盖断流重连 |
| B 端双倍流量与延迟 | 并发 semaphore + 文档明示；性能非本方案验收目标 |
| 影子设备信息陈旧（B 报告滞后于 C 实际下线） | 每次 binding 前的 health 预检即时失败兜底（fail-closed），影子表仅用于列表展示与初始解析 |
| 路径通配与 axum 路由优先级冲突（`*path` 吞掉 `/api/relay/peers`） | 精确路由先注册（axum 已有先例：`fs/*` 与固定路由共存），单测覆盖 |
