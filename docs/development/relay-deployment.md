# 中转访问（跳板）部署手册

面向运维的中文手册：用命令行把局域网里三台机器（发起方 A、跳板 B、目标 C）配通中转访问（跳板机）功能。也可作为替用户执行部署的 AI agent 的操作脚本依据。

- 设计依据：[`../superpowers/specs/2026-09-04-p2p-relay-design.md`](../superpowers/specs/2026-09-04-p2p-relay-design.md)（协议、错误 domain_code、B 端部署与 CLI 设计）
- 现有后端生命周期 / 端口 / doctor 基础操作：[`backend-operations.md`](backend-operations.md)
- 产品行为与固定风险：[`../prd.md`](../prd.md)

> **版本标注**：本文中的 `devices` / `relay` / `peers` CLI 子命令随中转功能版本发布。当前已发布版本的 CLI 仅支持 `start|serve|stop|status|supervise|doctor|version`；在未包含中转功能的版本上执行未知子命令会输出用法并返回退出码 2。

## 1. 拓扑与前提

### 1.1 三台机器的角色

```
A（发起方）            B（跳板 / 中转设备）          C（目标设备）
  │                      │                            │
  │  A ↔ B 直连可达      │      B ↔ C 直连可达        │
  ├─────────────────────►├───────────────────────────►│
  │      A 对 C 的流量经 B 转发：A → B → C             │
```

| 角色 | 典型形态 | 说明 |
| --- | --- | --- |
| A（发起方） | 桌面 GUI（macOS / Windows / Linux） | 打开并操作 C 上的远程项目；也可 headless（用 CLI 配置跳板） |
| B（跳板 / 中转设备） | headless Linux 小主机（最典型）；也可以是有人用的 GUI 机器 | 需横跨 A、C 两个网段（双网卡 / 接两个 Wi-Fi），运行 cc-partner 提供 `/api/relay/*` 转发 |
| C（目标设备） | GUI 或 headless 均可 | 存放远程项目；**零中转配置、零感知**，老版本也能被中转 |

三台机器都必须**安装并运行 cc-partner**（B、C 可只跑 headless 后端 CLI）。B 只做「方法 + 路径 + body 的透明字节转发」，不解析业务、不缓存。

### 1.2 网络前提

- **A ↔ B 直连可达**：mDNS 自动发现，或 `manual_peers` 手动 IP（含 Tailscale overlay 地址）。
- **B ↔ C 直连可达**：同上两种方式任选其一。
- 三个前提缺一不可（设计稿 §6）：
  1. B 上有 cc-partner 的 HTTP server 提供 `/api/relay/*` 转发路由（能力声明 `net.relay.v1`）；
  2. B 自己的发现表（mDNS browse 或 manual_peers）里有 C 的地址——转发目标的地址来源只在 B；
  3. A 的直连表里有 B——这正是「B 是 A、C 共同可达邻居」的定义。

**仅 SSH 可达不满足前提。** 链路上完全没有 SSH 参与：A → B 与 B → C 走的都是 HTTP（首选 TCP 62116）。SSH 服务器提供不了上面三个环节；`ssh -L` 隧道也不纳入产品方案（手机端 `/mobile` 无法使用、mDNS 不过隧道、故障无法按 `relay_*` domain_code 区分）。若 B 与 A 之间连 62116 都不可达（只有 22 端口），本方案不适用。

### 1.3 端口与防火墙前提

| 用途 | 协议 | 值 |
| --- | --- | --- |
| P2P HTTP（A→B、B→C、`/api/relay/*`） | TCP | 首选 **62116**，被占用自动 +1；实际端口以 `GET /api/health` 的 `http_port` 为准 |
| 设备发现（mDNS） | UDP | **5353**（服务类型 `_cc-partner._tcp.local.`） |

应用**不会**修改宿主机防火墙规则，需手动放行。Ubuntu/ufw 示例（macOS / Windows 写法见 [`backend-operations.md`](backend-operations.md)）：

```bash
sudo ufw allow 62116/tcp comment 'cc-partner P2P HTTP'
sudo ufw allow 5353/udp comment 'cc-partner mDNS'
sudo ufw reload
```

VLAN / 访客网隔离拓扑中，mDNS 广播通常不跨网段——这正是 B 需要手动 peer（§3.1 步骤 5）或使用 Tailscale overlay 的原因。放行端口意味着同一可达网络中的任何设备都能调用业务 API，这是产品固定语义（见 §6）。

## 2. 版本矩阵

| 节点 | 版本要求 |
| --- | --- |
| A（发起方） | ≥ 首个包含能力声明 `net.relay.v1` 的版本 |
| B（跳板） | ≥ 首个包含能力声明 `net.relay.v1` 的版本 |
| C（目标） | **任意版本**（含老版本）；`net.relay.v1` 只要求 B 支持 |

确认版本：

```bash
cc-partner-backend version
cc-partner-backend doctor --json   # stdout JSON 里的 version 字段
```

升级 B 时先 `cc-partner-backend stop` 再替换二进制，随后重新 `start`。

## 3. 部署步骤（按 B → C → A 顺序）

### 3.0 CLI 命令速查（随中转功能版本发布）

| 命令 | 用途 |
| --- | --- |
| `cc-partner-backend devices [--json]` | 直连设备表快照（device_id / device_name / host / port / online / proto_version / capabilities / source）；**获取跳板 device_id 的途径** |
| `cc-partner-backend relay status [--json]` | relay 配置（enabled / via 列表）+ 每个跳板的可达性 + 全部影子设备及其在线状态 |
| `cc-partner-backend relay via add <device_id\|device_name>` | A 侧：添加跳板（写 `config.relay.via_device_ids`）；按 device_id 精确添加时 backend 未运行也可用；按 device_name 匹配需 backend 运行中，多匹配 / 零匹配会报错并列出候选 |
| `cc-partner-backend relay via remove <device_id>` | A 侧：移除跳板 |
| `cc-partner-backend relay allow on\|off` | B 侧：允许本机被用作跳板（写 `config.relay.enabled`，默认 on） |
| `cc-partner-backend peers add <host>[:port]` | 发现兜底：手动 peer（写 `config.manual_peers`），port 缺省 62116 |
| `cc-partner-backend peers remove <host>[:port]` | 移除手动 peer |
| `cc-partner-backend peers list [--json]` | 列出手动 peer |

CLI 契约与现有 lifecycle 命令一致：严格参数解析；退出码 **0** 成功 / **1** 失败 / **2** 用法错误；查询命令 stdout 输出单行 JSON（可直接 `jq`），日志只进 stderr。

### 3.1 B（跳板，headless Linux 小主机）

**步骤 1：构建 Linux 二进制（在开发机仓库根执行）**

```bash
node scripts/docker-build-backend-linux.mjs
# 产物：src-tauri/target-linux/release/cc-partner-backend
```

脚本在 Docker（`rust:1.95-bookworm`，linux/amd64 via QEMU）内 `cargo build --release --bin cc-partner-backend --locked` 交叉编译 x86_64 Linux 产物，glibc 兼容 Ubuntu 24.04。需要本机装有 Docker。

**步骤 2：分发到 B 并安装运行库**

```bash
scp src-tauri/target-linux/release/cc-partner-backend user@B:/usr/local/bin/cc-partner-backend
scp -r web/dist user@B:~/cc-partner/web-dist
```

在 B 上（Ubuntu 24.04）安装运行库并准备数据目录：

```bash
sudo apt-get install -y libgtk-3-0 libwebkit2gtk-4.1-0   # webkit2gtk/gtk 运行库
mkdir -p ~/.cc-partner
```

`web/dist` 是 headless 模式服务 `/mobile` 等静态页面的资源目录，用环境变量 `CC_PARTNER_WEB_DIST` 指向它（例如写入 systemd 单元或 shell profile：`export CC_PARTNER_WEB_DIST=$HOME/cc-partner/web-dist`）。数据与日志默认在 `~/.cc-partner`，可用 `CC_PARTNER_DATA_DIR` 隔离。

**步骤 3：放行防火墙**（§1.3，UDP 5353 + TCP 62116）。

**步骤 4：启动并自检**

```bash
cc-partner-backend start
cc-partner-backend doctor
```

`doctor` 应确认：本机后端 health 正常、数据 / 数据库 / 日志路径可用、mDNS 可用、依赖（git / tmux 等）状态。退出码 `0` 健康 / `1` 降级 / `2` 不健康。若 mDNS 报警，先确认 UDP 5353 已放行。

**步骤 5：确认 B 能看到 C**

```bash
cc-partner-backend devices --json
```

若 C 不在列表（B、C 跨网段，mDNS 不可见是常态），添加手动 peer：

```bash
cc-partner-backend peers add 192.168.2.30        # C 的 IP，端口缺省 62116
cc-partner-backend devices --json                # 复查 C 已在列且 online
```

**步骤 6：中转开关**

默认 `relay allow on`，**无需任何配置**，装好运行即具备被用作跳板的能力。仅当需要临时停用时执行 `cc-partner-backend relay allow off`（恢复用 `on`）。

### 3.2 C（目标机）

部署方式与 B 完全相同（§3.1 步骤 1–5：构建产物、scp 二进制与 `web/dist`、运行库、防火墙、`start` + `doctor`、必要时手动 peer 让 C 看到 B 所在网段的地址也可）。

- **C 零中转配置**：不装任何 relay 开关、不加任何中转设置，老版本 C 也能被中转。
- 自检重点是「C 的 62116 对 B 可达」。在 **B** 上验证：

```bash
curl -sS "http://192.168.2.30:62116/api/health"
```

返回 JSON 且 `ok: true` 即通；`http_port` 字段是 C 的实际监听端口（若 62116 被占自动 +1，后续手动 peer 请写实际端口）。不通时优先排查 C 的防火墙与 C 上后端进程（`cc-partner-backend status`）。

### 3.3 A（发起方）

**GUI（常规路径）**：Settings → 「依赖环境」tab → 「中转访问（跳板）」卡片 → 「添加跳板设备」选择器（只列出本机直连在线设备）→ 选中 B。约 15s（探测周期）后，B 名下的影子设备（C）开始出现在全局设备列表。

**headless（或脚本化）**：

```bash
cc-partner-backend devices --json                     # 拿到 B 的 device_id
cc-partner-backend relay via add <B 的 device_id>      # 或按设备名：relay via add nas-vpn
```

不需要（也无法）逐个配置目标设备——目标 C 经跳板自动发现。误加用 `relay via remove <device_id>` 移除。若 via 设备从 A 的直连表消失，其名下影子设备全部转为 offline。

## 4. 验证清单

1. **B 侧视图**：`cc-partner-backend devices --json` 显示 C 在列且 `online: true`。
2. **A 侧 relay 状态**：

   ```bash
   cc-partner-backend relay status --json
   ```

   预期内容：relay 配置（enabled / via 列表）中包含 B；B 的可达性为可达；影子设备清单中出现 C 且在线状态为 online。
3. **能力声明**：`curl -sS "http://<B 的 IP>:62116/api/health"`，响应 `capabilities` 数组包含 `net.relay.v1`（仅含中转功能的版本才有）。
4. **打开远程项目**：Workbench → 打开远端项目 → 设备列表出现 C，名称行带「经 {B 设备名} 中转」标记 → 浏览远端目录 → 打开项目。后续文件读写、Git / worktree、终端（输入 + 事件流）、浏览器预览、Orchestrator 与直连远程项目操作完全一致，用户无感。
5. **手机三跳**：手机连 A 的 `/mobile` 入口 → 项目选择器同样出现 C（带中转标记）→ 直接点选，链路 `手机 → A → B → C` 自动成立，零额外配置。
6. **链路自动迁移（回归确认）**：C 一旦对 A 直连可达（进入 A 的 mDNS 表），下一次解析自动切回直连，已打开的远程项目 shortcut 无需重建。

## 5. 故障排查（按 domain_code）

错误信封的 `domain_code` 可区分「跳板故障」与「目标设备故障」。先在 A 上 `relay status` / `devices` 看两端视图，再按表处置；B 侧日志在 `~/.cc-partner/logs/backend.log`（轮转 `.1`–`.3`）。

| domain_code | 现象 | 处置 |
| --- | --- | --- |
| `relay_target_offline` | B 的直连表里没有该目标或已标记离线（HTTP 404） | 在 B 上 `cc-partner-backend devices --json` 看 C 是否在列 / online；不在则 `peers add <C 的 IP>[:端口]`；仍离线则到 C 上 `cc-partner-backend status` 确认进程存活 |
| `relay_target_unreachable` | B 连接 C 失败（HTTP 502），B 会顺带把该目标置为离线以加速收敛 | 在 B 上 `curl -sS "http://<C 的 IP>:<实际端口>/api/health"` 验证；排查 C 防火墙（TCP 实际端口）与 C 进程；恢复后由 B 的周期探测自动回到在线 |
| `relay_busy` | B 的转发并发达到上限（全局 8、单目标 4，HTTP 503） | 稍候重试（现有请求排空后自愈）；减少同时打开的终端 / 预览会话数；或更换 / 新增跳板 |
| `relay_disabled` | B 侧中转已被关闭：不宣告 `net.relay.v1`、不注册 `/api/relay/*` 路由，A 侧表现为该跳板名下影子设备不可用 | 登录 B 执行 `cc-partner-backend relay allow on`；GUI 版在 Settings → 依赖环境 → 中转访问卡片打开开关 |
| `relay_path_not_allowed` | 转发路径不在白名单（HTTP 403）。白名单仅 `/api/health`、`/api/workbench/*`、`/api/orchestrator/*` | 文件传输、Prompt/速记同步等流量本就不经中转（见 §6），属预期；若 Workbench / Orchestrator 业务操作被拒，带上 request_id 到 B 侧日志定位具体路径 |
| `device_id_mismatch` | 请求携带的 expected-device 头与目标不符（HTTP 409）：中转路径必须等于 URL 中的目标 device_id，非中转路径必须等于本机 device_id | 多为手工构造请求写错了 ID：用 `devices --json` 核对目标真实 device_id；正常客户端不会触发 |

## 6. 风险与边界

**固定风险声明**（[`../prd.md`](../prd.md) §1.3 原文）：同一可达网络中的任何设备均可读取、写入和执行；系统不验证调用者身份。

中转不改变这一边界，但把链路上参与的设备从两台变成三台，部署前应向所有相关设备的操作者说明：

- **跳板可见明文流量**：全 P2P 本就是明文 HTTP，B 转发的是 Workbench 明文流量，与现有边界同等级；B 的操作者应知晓本机会为邻居设备转发流量。
- **单跳硬限制（拓扑深度 = 1）**：B 只从自己的直连表解析转发目标，转发出站的请求不可能再命中任何中转前缀，结构上杜绝 `A → B → D → C` 多跳与环路；B 拒绝以自身为转发目标；A 侧影子设备的跳板只能是 A 的直连设备。多跳链路协议上不预留，确有需求另行设计。
- **transfer / sync 不经中转**：转发路径白名单仅 `/api/health`、`/api/workbench/*`、`/api/orchestrator/*`；`/api/transfer/*`、`/api/sync/*`、`/api/prompts/*`、`/api/mobile/*`、`/api/backend/control/*` 一律拒绝（`relay_path_not_allowed`）。文件传输与双向同步仍要求两端直连可达。
- **不做直连失败自动回落中转**：链路选择由「直连表是否命中」静态决定，避免健康抖动导致同一请求行为不确定。
- **资源上限**：中转请求沿用全局 body 上限 32 MiB，最终由 C 端各路由自身上限强制；B 端并发上限见 §5 `relay_busy`。

## 7. 日常运维

### 7.1 常驻运行

```bash
cc-partner-backend supervise    # 登录自启监督入口
```

`supervise` 从当前可执行文件直接 spawn `serve` 子进程（不经 shell），异常退出按 1→60s 指数退避自动重启（连续健康 10 分钟后重置退避）；执行 `cc-partner-backend stop`（退出码 0）会连带结束监督循环。也可改配 systemd 单元（`ExecStart=<二进制路径> serve`，配好 `CC_PARTNER_WEB_DIST`）。升级二进制：

```bash
cc-partner-backend stop && cp cc-partner-backend-new /usr/local/bin/cc-partner-backend && cc-partner-backend start
```

### 7.2 状态与停止

```bash
cc-partner-backend status    # 单行 JSON：{kind, control?: {pid, port}, error?}
cc-partner-backend doctor    # 人工可读健康报告；--json 供脚本解析
cc-partner-backend stop      # 本地控制路由优雅停止
```

`status` / `doctor` 不输出控制 token；控制文件 `~/.cc-partner/backend-control.json` 含 token，不要贴进工单。

### 7.3 配置热生效

- 通过 **GUI 设置页**或 **CLI 子命令**（`relay via add/remove`、`relay allow`、`peers add/remove`）修改配置时：backend 运行中会经本机 control plane `update-config` 写入并热生效——影子探测与手动 peer 探测循环周期重读内存配置（**15s 内**生效），**无需重启**。
- **直接手改 `~/.cc-partner/config.json` 需重启后端才生效，不推荐**；backend 未运行时 CLI 写命令直接落盘，下次 `start` 生效，读命令会报错并提示先 `start`。

### 7.4 日志

只看 `~/.cc-partner/logs/backend.log`（当前 5 MiB 上限，历史 `.1`–`.3`，`.1` 最新）；日志仅落盘，无上传。排查中转问题时关注 `relay` / 目标 device_id 相关错误与 `X-CC-Request-Id`（全链路一个 request id 透传，可跨 A、B、C 对账）。

## Related

- 设计方案与协议细节：[`../superpowers/specs/2026-09-04-p2p-relay-design.md`](../superpowers/specs/2026-09-04-p2p-relay-design.md)
- 后端生命周期 / 端口 / doctor / 防火墙：[`backend-operations.md`](backend-operations.md)
- 产品需求与固定风险：[`../prd.md`](../prd.md)
- 项目总览：[`../../README.md`](../../README.md)
