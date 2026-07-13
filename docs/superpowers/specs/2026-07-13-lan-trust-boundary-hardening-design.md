# LAN 无鉴权信任边界最小加固设计

- 日期：2026-07-13
- 最后修订：2026-07-14
- 状态：已按最终产品决定重写，待实施
- 适用范围：P2P HTTP、`/mobile`、Workbench Browser Preview、mDNS、桌面风险提示、独立 backend CLI

## 1. 固定产品语义

cc-partner 只支持本机与局域网访问。产品只有一种 LAN 行为，不提供暴露模式、只读模式或逐设备权限：

- 合法 loopback/LAN socket peer 调用 P2P、Mobile、Workbench 和 Orchestrator 业务 API 时，不需要账号、配对、token、cookie、session、签名或设备身份；
- 所有业务查询、写入和执行动作直接允许，后端不再做读写授权判断；
- 同一可达网络中的不同人员和设备能力完全相同；
- `/api/backend/control/stop` 是本机进程生命周期接口，不是 LAN 业务 API，继续使用 loopback socket peer 与现有 control-file token 双重约束。

网络范围、浏览器请求来源和资源上限检查是部署边界与请求完整性保护，不是身份鉴权。实现和文档不得把通过检查的 peer 称为“已认证”“可信设备”或“安全设备”。

## 2. 目标与非目标

### 2.1 目标

1. 使用真实 TCP socket peer IP 把业务请求限制在支持的 loopback/LAN 地址范围，完全忽略代理来源 header。
2. 保持现有单 listener、首选端口 62116 与占用递增行为，由 socket gate 在 handler 前拒绝不支持的 peer；不为此建立暴露模式运行时。
3. 让 mDNS、Mobile URL、防火墙指引和 doctor 始终使用实际端口与真实 LAN 地址，不把 wildcard listener 描述为“只绑定 LAN”。
4. 通过 Host、Origin、Content-Type 与 WebSocket upgrade 检查降低跨站请求和 DNS rebinding 风险，同时兼容无 Origin 的 native P2P 与 opaque preview iframe 的受限 `Origin: null`。
5. 保留现有全局及领域资源上限，只给已确认的重型入口保留明确上限，不创建全路由策略矩阵。
6. 在 Settings、Mobile 访问卡和 doctor 中准确展示无鉴权风险。

### 2.2 明确不做

- 不增加 LAN 业务访问令牌、登录态、设备注册、配对、角色、信任列表或撤销列表。
- 不提供任何可切换的 LAN 暴露模式、只读模式或其它权限分级。
- 不按路由建立读写授权表或只读 gate。
- 不新增 mode config、schema migration、configured/effective runtime、mode capability token 或 mode UI。
- 不把 mDNS 发现结果、Host、Origin、previewId 或私网 IP 当作设备身份。
- 不自动修改防火墙、路由器、VPN、TLS、端口映射或系统网络配置。
- 不支持互联网远程访问、云中继、反向代理或用户自行建立的公网隧道。

## 3. 威胁模型与剩余风险

### 3.1 本设计覆盖

- 全局可路由、unspecified、multicast、文档保留或无法判定的 socket peer 在进入业务 handler 前被拒绝。
- `Forwarded`、`X-Forwarded-For`、`X-Real-IP` 等 header 不能改变 peer scope。
- 恶意网页不能借任意 Host、跨站 Origin、simple-request Content-Type 或跨站 WebSocket 直接操作业务 API。
- DNS rebinding 域名不能被请求 Host 动态加入允许范围。
- opaque sandbox preview 的 `Origin: null` 只能访问已存在、未过期 previewId 对应的 proxy 命名空间，不能访问其它 `/api/*`。
- 远端 peer 即使获得 control token，也不能调用 backend stop。
- 超大请求体和超出领域上限的 transfer、文件保存、preview proxy 请求被拒绝。
- 日志与错误诊断不记录 Prompt、终端内容、文件正文或完整 URL query。

### 3.2 本设计不覆盖

- 支持地址范围内的恶意、被入侵或误操作设备；这些设备本来就拥有全部业务能力。
- 能发起原生 HTTP 请求、可以省略或伪造 Origin 的恶意程序。
- 浏览器扩展、已被控制的浏览器或同机恶意进程。
- 明文 HTTP 的窃听、篡改、ARP/NDP 欺骗、恶意路由器或无线 AP 配置错误。
- VPN、容器桥接或隧道把远端流量呈现为私网/ULA 地址；IP 范围是支持边界，不是物理同网证明。
- 用户主动端口转发、反向代理或关闭系统防火墙。

固定风险文案：

> 同一可达网络中的任何设备均可读取、写入和执行；系统不验证调用者身份。

## 4. Socket peer 范围

所有 HTTP 请求从 axum `ConnectInfo<SocketAddr>` 读取真实 peer。生产 server 必须使用带 connect info 的 serve 入口；中间件不得读取或信任代理来源 header。

允许范围：

- IPv4 loopback：`127.0.0.0/8`；
- IPv4 private：`10.0.0.0/8`、`172.16.0.0/12`、`192.168.0.0/16`；
- IPv4 link-local：`169.254.0.0/16`；
- IPv6 loopback：`::1`；
- IPv6 ULA：`fc00::/7`；
- IPv6 link-local：`fe80::/10`。

IPv4-mapped IPv6 先还原为 IPv4 再判断。IPv4/IPv6 global unicast、unspecified、multicast、文档保留地址和缺少 peer address 的请求返回统一 403 错误信封，并在 handler 前终止。

实现只需要 `Loopback / Lan / Denied` 三种内部 scope；`Loopback` 与 `Lan` 对所有业务 API 权限相同，仅 backend stop 要求 `Loopback`。PrivateV4、link-local、ULA 等细分类只用于纯函数测试和脱敏诊断，不得演变成权限模式。

## 5. Listener、端口与 mDNS

本轮保留 `http_server.rs` 当前单 listener 和端口选择模型：配置端口无效时使用 62116，占用则递增。listener 可以继续使用 wildcard bind 以兼容网络变化与现有跨平台行为；LAN-only 的强制边界由 socket gate 提供。

约束如下：

- runtime、日志、doctor 和 UI 必须如实称其为 `0.0.0.0:<actualPort>` wildcard listener，不得声称 socket 只绑定了 LAN interface；
- mDNS 只使用支持范围内的实际 LAN 地址和 HTTP server 返回的实际端口；
- Mobile URL 只输出支持范围内、非 loopback 的 LAN 地址；不得输出公网/GUA、unspecified 或任意未解析 hostname；
- 防火墙指引只展示 UDP 5353 与实际 TCP 端口，并提示用户将规则限定为 Private/Home/LAN profile；
- 不新增多 listener 编排、网卡变化 watcher、pending restart、mode runtime 或 listener 配置迁移。

## 6. HTTP Host、Origin 与 Content-Type

### 6.1 Host

所有 `/api/*`、`/mobile`、`/assets/*` 和 preview proxy 请求都必须携带受控 Host 与实际端口。允许的 host 仅包括：

- 属于第 4 节范围的字面 IP；IPv6 使用标准方括号 URL 形式；
- `localhost`；
- 当前进程按既有 mDNS 命名规则发布的自身 `.local` hostname。

端口必须等于 `AppState.actual_http_port`。不根据请求 Host 学习或扩充允许列表；任意域名、错误端口和解析到 LAN 的攻击者域名均拒绝。

### 6.2 Origin 决策

| 请求类型 | Origin 缺失 | 精确 `http://<Host>` | `Origin: null` | 其它 Origin |
| --- | --- | --- | --- | --- |
| `/mobile` navigation / 静态资源 | 允许 | 允许 | 仅正常资源加载语义下允许 | 拒绝 |
| 普通 `/api/*` GET/HEAD | 允许 | 允许 | 拒绝 | 拒绝 |
| 普通 `/api/*` 写请求 | 允许 native P2P | 允许同源浏览器 | 拒绝 | 拒绝 |
| 普通 API WebSocket | 允许 native client | 允许同源浏览器 | 拒绝 | 拒绝 |
| preview proxy HTTP/WebSocket | 允许 navigation/native | 允许 | 仅有效 preview session 允许 | 拒绝 |

preview iframe 当前 sandbox 明确不包含 `allow-same-origin`，其脚本发出的 fetch/form/WebSocket 可能使用 opaque origin，即 `Origin: null`。因此：

1. 全局 guard 只把 preview proxy path 标记为可能的 opaque-origin 例外，不直接把 `null` 当作普遍合法来源；
2. `browser_proxy.rs` 必须先用现有 registry 查到未过期 previewId，再允许该 path 下的 `Origin: null` HTTP 或 WebSocket；
3. 未知/过期 previewId、越出 desktop/mobile proxy prefix 的 path 或其它 `/api/*` 的 `Origin: null` 一律拒绝；
4. iframe 继续禁止 `allow-same-origin`，不得为了简化 Origin 校验而放宽 sandbox。

### 6.3 Content-Type 与 CORS

- 普通 `/api/*` 非 GET/HEAD 请求拒绝 `application/x-www-form-urlencoded`、`multipart/form-data` 和 `text/plain`，避免恶意网页使用 simple request 触发业务动作；
- JSON、transfer chunk 等现有客户端格式保持不变；preview proxy 必须允许 dev server 所需的任意业务 Content-Type，但仍受 preview session 和 body 上限约束；
- CORS 默认不返回 wildcard、不反射任意 Origin、不允许 credentials；
- Host/Origin 失败返回稳定 403 错误信封，不记录原始敏感 query 或 body。

## 7. Backend stop 生命周期边界

`POST /api/backend/control/stop` 必须同时满足：

1. 真实 socket peer 为 loopback；
2. 请求中的 `controlToken` 与现有 control file 完全一致。

两个条件缺一不可。该 token 不得复用到任何 LAN 业务 API，也不得进入 health、mDNS、UI、doctor JSON 或日志。业务 API 不能因为 stop 的存在而引入通用授权接口。

## 8. 资源与代理边界

不建立逐路由预算目录。保留并验证现有最小边界：

- axum `DefaultBodyLimit` 全局绝对上限 32 MiB；
- transfer chunk 使用现有 `CHUNK_SIZE = 960 KiB`，HTTP route 与 receiver 都拒绝超限 chunk；
- Workbench 文本读写继续使用 `MAX_EDITABLE_TEXT_BYTES = 5 MiB`，JSON 转义开销仍由 32 MiB 外层上限容纳；
- browser preview proxy 继续使用 `PROXY_BODY_LIMIT_BYTES = 32 MiB`、有效 preview registry、固定上游 target、禁用外部 redirect 跟随和现有 path/location 重写约束；
- SQLite、HTML asset、image、CSV 等领域上限继续由现有模块负责，不复制到 HTTP 路由表；
- 现有 remote request timeout 分类继续保留；不为所有路由新增独立 semaphore、timeout class 或 retry class。

未知路由由 axum router 正常返回 404/405，再由现有错误信封 fallback 统一格式。GET/HEAD handler 必须无业务副作用；该不变量由路由 review 和代表性测试维护，不引入第二套路由权限分类。

## 9. 产品提示、Doctor 与日志

- `MobileAccessCard` 在二维码附近展示固定风险文案；只要存在合法 LAN URL 就正常展示，不做模式分支。
- `LanFirewallDependencyCard` 继续展示 LAN IP、实际 HTTP 端口、mDNS 5353 和人工防火墙指引，并增加同一固定风险说明。
- doctor 文本显示 wildcard listener、实际端口、mDNS/防火墙提示与固定风险文案；不得输出 control token。
- 不新增 LAN mode 设置面板、mode DTO、读写按钮禁用或 configured/effective 状态。
- 接受/拒绝日志最多记录 request id、规范化 route/path 类别、peer scope、拒绝原因、status 和 duration；不得记录 Prompt、terminal data、文件正文、Authorization/Cookie 或完整 query。

## 10. 协议、兼容与回滚

- 不新增 LAN 权限 capability token；socket/浏览器 guard 不要求客户端协商。
- 新旧 native peer 都继续无凭据调用现有业务路由；无 Origin 请求保持兼容。
- 不新增配置字段、schema migration 或旧配置默认逻辑。
- 回滚旧 binary 可能移除 socket/Host/Origin guard；发布说明必须如实提示回滚会恢复旧暴露行为，并要求复核防火墙。
- `docs/p2p-protocol.md` 的路由、幂等与 retry 语义继续作为协议清单，不增加 effect/browser policy/body budget 全字段矩阵。

## 11. 发布与验收

发布顺序：socket peer gate 与 stop 隔离 → Host/Origin/preview guard → 资源上限回归 → UI/doctor 风险提示 → 协议与运维文档 → 跨平台与真实设备 evidence。

验收条件：

- 合法 loopback/LAN peer 不携带任何身份凭据即可完成 P2P、Mobile、Workbench 和 Orchestrator 的读写执行。
- 公网、不可判定或伪造 forwarded 来源在 handler 前失败。
- 恶意 Host、错误端口、跨站 Origin、普通 API `Origin: null`、跨站 WebSocket 与 form/text write 失败。
- 同源 mobile、无 Origin native P2P、有效 preview session 的 opaque `Origin: null` HTTP/WebSocket 正常工作；其它 null-origin API 仍失败。
- backend stop 同时要求 loopback peer 与现有 control-file token。
- 32 MiB 全局上限、960 KiB chunk、5 MiB 文本和 32 MiB proxy 上限有边界测试。
- Settings/doctor 使用固定风险文案并展示实际 LAN IP、端口和 mDNS 信息，不出现模式或认证状态。
- macOS、Windows、Ubuntu 自动化通过；真实双机与手机覆盖发现、Mobile、P2P、Workbench/Orchestrator 写执行和 preview。未执行的真实设备项目明确标记 `NOT VERIFIED`。
