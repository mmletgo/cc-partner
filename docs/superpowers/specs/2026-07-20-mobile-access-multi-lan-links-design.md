# Mobile Access 多局域网链接选择 Design

- 日期：2026-07-20
- 状态：方案已确认，待转入实现计划
- 范围：桌面端 `MobileAccessCard` 访问弹层 + `/api/mobile/access-info` / `get_mobile_access_info` 候选地址收集

## 1. 背景

桌面端侧栏 footer 的手机入口会打开共享 Dialog，内嵌 `MobileAccessCard`：展示风险文案、局域网 `/mobile` URL、复制/刷新、二维码。

现状链路：

1. 后端 `local_lan_ip()` 通过 UDP 连接 `8.8.8.8:80` **只探测一个默认出站 IP**。
2. `build_mobile_access_info` 把候选 IP 过滤 loopback 后生成 `urls: string[]`。
3. 前端 `MobileAccessCard` 会 map 全部 `urls`，但二维码与「复制链接」只使用 `selectPrimaryMobileUrl`（列表第一条）。

一台设备常同时属于多个可达网段（有线 + Wi‑Fi、公司网 + 家庭网、VPN 等）。手机若处在非默认出站网段，扫当前二维码会失败。用户要求：

- 列出多个局域网对应链接；
- 可切换选择显示特定局域网的链接与二维码；
- 交互为顶部网段分段/芯片，下方只显示当前选中项。

## 2. 已确认决策

| 项 | 决策 |
| --- | --- |
| 交互 | **分段芯片（Segmented / chips）**：顶部列出网段选项，点选后下方只显示该网段 URL + 二维码；复制/刷新作用在当前选中项 |
| 芯片标签 | **角色 + IP**；角色仅 `wifi` / `wired`；推断不出则 **只显示 IP**，不加「其他」 |
| 默认选中 | 优先 **默认出站**（现有 `local_lan_ip()`）对应条目；不存在则列表第一项 |
| 地址过滤 | **全量枚举 + 基础黑名单**：排除 loopback、link-local、明显虚拟桥/容器/VM 接口；保留物理/Wi‑Fi/有线与常见 VPN |
| 方案 | 后端输出结构化 `entries` + 派生 `urls`；前端芯片切换选中 URL |
| 持久化 | 不记住上次选择（打开弹层每次按默认出站规则） |
| 鉴权 | 不改动：仍无调用者身份校验；保留现有固定风险文案 |

## 3. 目标

1. 枚举本机所有「适合手机扫码」的局域网访问入口，而不是只返回一个出站 IP。
2. 桌面弹层支持在多个网段间切换；当前选中项驱动 URL 展示、复制与二维码。
3. 芯片标签在可识别时显示「Wi‑Fi / 有线 · IP」，否则仅 IP。
4. 默认选中与现有「默认出站」语义一致。
5. 兼容既有 `MobileAccessInfo.urls` 消费者与固定风险文案合同。
6. 单元测试覆盖：接口过滤、角色推断、默认标记、前端选中与标签渲染逻辑。

## 4. 非目标

1. 不做 token / 登录 / 配对 / 按网段 ACL。
2. 不做公网访问、NAT 穿透、mDNS 主机名扫码。
3. 不在设置页单独做第二套多网段 UI（设置页若复用 `MobileAccessCard` 则自然继承）。
4. 不记忆用户上次选中网段。
5. 不保证虚拟网卡/Docker/VM 地址一定被排除干净（黑名单启发式；误留可展示，误删可后续补规则）。
6. 不改动手机端 `/mobile` SPA 本身。
7. 不改变 HTTP 监听地址（仍为 `0.0.0.0` + 实际端口）。

## 5. 设计原则

1. **结构化优先，兼容字符串列表**：`entries` 是权威列表；`urls` 由 `entries[].url` 按相同顺序派生，旧代码仍可只读 `urls`。
2. **探测与展示分离**：`local_lan_ip()` 继续服务 mDNS/默认出站语义；多网段枚举走新的接口枚举函数。
3. **角色可选、IP 必有**：角色只是展示增强，不得阻塞 URL 生成。
4. **过滤集中在后端**：前端不依赖接口名/黑名单；只消费 DTO。
5. **UI 单一选中源**：一个 `selectedEntryId` 状态同时驱动 URL 行、复制、二维码。

## 6. 数据模型

### 6.1 后端 DTO

```rust
// camelCase 序列化
pub struct MobileAccessEntryDto {
    /// 稳定展示 id：优先 `host`（IP 字符串）；同一 host 不应重复出现
    pub id: String,
    /// 完整访问 URL，如 http://192.168.3.13:62116/mobile
    pub url: String,
    /// 归一化 host（IPv6 无方括号；URL 内 IPv6 仍用 format_url_host）
    pub host: String,
    /// 可选角色：仅 "wifi" | "wired"；未知则省略 / null
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<MobileAccessRole>,
    /// 是否对应当前默认出站探测结果
    pub is_default: bool,
}

pub enum MobileAccessRole {
    #[serde(rename = "wifi")]
    Wifi,
    #[serde(rename = "wired")]
    Wired,
}

pub struct MobileAccessInfoDto {
    pub device_name: String,
    pub port: u16,
    /// 与 entries 同序派生，兼容旧前端
    pub urls: Vec<String>,
    pub entries: Vec<MobileAccessEntryDto>,
}
```

前端 TypeScript：

```ts
export type MobileAccessRole = 'wifi' | 'wired';

export interface MobileAccessEntry {
  id: string;
  url: string;
  host: string;
  role?: MobileAccessRole | null;
  isDefault: boolean;
}

export interface MobileAccessInfo {
  deviceName: string;
  port: number;
  urls: string[];
  entries: MobileAccessEntry[];
}
```

### 6.2 兼容与迁移

- **不需要** DB/配置迁移。
- 旧测试若只构造 `{ deviceName, port, urls }`，实现期同步加上 `entries`（与 `urls` 对齐）。
- 若运行时收到无 `entries` 的异常响应：前端用 `urls` 合成临时 entries（`id=host`，`role` 空，`isDefault` 仅第一条 true），保证 UI 不崩。**生产路径后端必须始终填 entries。**

### 6.3 排序

1. `is_default == true` 的条目优先（最多一条；若探测 IP 未出现在枚举结果中，则不强制插入伪造条目）。
2. 其余按 host 字符串字典序，保证刷新稳定。
3. 同一 host 去重（归一化 trim 后）。

## 7. 后端：候选地址收集与角色

### 7.1 新函数（建议位置 `src-tauri/src/net/discovery.rs` 或 `src-tauri/src/mobile/`）

```text
list_mobile_access_candidates() -> Vec<MobileAccessCandidate>
// MobileAccessCandidate { host: String, role: Option<MobileAccessRole>, ifa_name: String }
```

实现要点：

1. 使用 **`if-addrs`**（已在 lockfile 传递依赖；需在 `src-tauri/Cargo.toml` **直接声明**依赖，禁止依赖传递可用性）。
2. 遍历 `if_addrs::get_if_addrs()`（或当前 crate API 等价物）。
3. 对每个地址：
   - 跳过 loopback 接口 / loopback IP；
   - 跳过 link-local：IPv4 `169.254.0.0/16`，IPv6 `fe80::/10`；
   - 跳过未指定地址 `0.0.0.0` / `::`；
   - 按接口名黑名单跳过虚拟接口（大小写不敏感、子串/前缀匹配，见 7.2）；
   - IPv4 与非 link-local IPv6 均可保留（IPv6 URL 继续用既有 `format_url_host`）。
4. 角色推断（仅正向识别，失败则 `None`）：
   - **wifi**：接口名匹配 `wi-fi`/`wifi`/`wlan`/`airport`/`en0`（macOS 常见内建 Wi‑Fi 为 `en0`——**仅当平台为 macOS 且名称为 `en0` 时**视为 wifi 启发式；Windows/Linux 不把 `en0` 当 wifi）；或接口名含 `wl` 前缀（Linux `wlp*`/`wlan*`）。
   - **wired**：接口名匹配 `ethernet`/`eth`/`enp`/`ens`/`em`/`eno`，或 macOS `en1+`/`en` 数字接口在非 wifi 判定时视为 wired 的保守策略 **不做**（避免误标）；Windows `以太网`/`Ethernet` 文本名匹配。
   - 无法判定 → `role = None`（芯片只显示 IP）。
5. **不要**把 Docker/VM 黑名单没命中的接口强行标成 wifi/wired。

> 角色启发式允许误判为 `None`，**不允许**把明显虚拟接口标成 wifi/wired。宁可只显示 IP。

### 7.2 接口名黑名单（基础）

匹配接口名（小写）前缀或全名，至少包括：

| 模式 | 说明 |
| --- | --- |
| `lo`, `lo0` | loopback |
| `docker*`, `br-*`, `veth*`, `cni*`, `flannel*`, `cbridge*` | 容器/CNI |
| `vmnet*`, `vbox*`, `vboxnet*`, `virbr*`, `hyper-v*`, `vethernet*`（含空格变体归一化后） | 常见 VM |
| `awdl*`, `llw*`, `ap*`（macOS Apple Wireless Direct / 本地） | 苹果点对点，手机浏览器通常不可用 |
| `utun*` **不默认拉黑** | 用户可能经 VPN 访问；保留地址，角色 `None` |
| `bridge*`（macOS） | 系统桥，噪音大 → 拉黑 |
| `gif*`, `stf*`, `p2p*` | 隧道/点对点噪音 |

黑名单集中常量表，单测可对表断言。后续误杀/漏杀只改表，不改 UI。

### 7.3 默认出站标记

1. 调用既有 `local_lan_ip()` 得到 `Option<IpAddr>`。
2. 若某 entry 的 host 解析后等于该 IP → `is_default = true`（仅第一条匹配，防重复）。
3. 若探测失败或探测 IP 不在列表中：所有 `is_default = false`；前端回退到列表第一项。

### 7.4 组装

`build_mobile_access_info` 签名扩展为接收结构化候选（或在函数内调用 `list_mobile_access_candidates`）：

```text
build_mobile_access_info(config, port, candidates) -> MobileAccessInfoDto
```

- 过滤/归一化 host（复用 `normalize_mobile_host`）；
- 生成 `url`；
- 填充 `entries` + 派生 `urls`；
- 排序见 6.3。

`get_mobile_access_info`（Tauri command）与 `GET /api/mobile/access-info` 共用同一组装路径，禁止一端枚举、一端仍只取单 IP。

### 7.5 mDNS / 其它调用方

- `local_lan_ip()` **保持单 IP 行为**（mDNS advertise、防火墙依赖检测等不改语义）。
- `local_address_candidates()`（gui_startup）若仍只服务 sidecar 启动信息，可不强制改多地址；**仅 mobile access 路径**切换到全量枚举。若后续需要多地址上报，另开任务。

## 8. 前端：MobileAccessCard

### 8.1 状态

| 状态 | 含义 |
| --- | --- |
| `info` | 最近一次 access-info |
| `selectedId` | 当前芯片 id |
| `qrSvg` / `loading` / `copying` / `copied` / `error` | 既有 |

### 8.2 选中规则

纯函数（放 `mobileQr.ts` 或并列 `mobileAccessSelection.ts`）：

```ts
function resolveMobileAccessEntries(info: MobileAccessInfo | null): MobileAccessEntry[]
// entries 优先；否则由 urls 合成

function selectDefaultMobileAccessEntryId(entries: MobileAccessEntry[]): string | null
// isDefault 优先，否则第一项

function formatMobileAccessChipLabel(entry: MobileAccessEntry, t): string
// wifi → t('mobileAccess.roleWifi', { ip: entry.host })
// wired → t('mobileAccess.roleWired', { ip: entry.host })
// else → entry.host
```

行为：

1. `loadAccessInfo` 成功后：`selectedId = selectDefault...`；若刷新后旧 `selectedId` 仍存在则**保留**（同一次打开会话内网段不乱跳）；关闭 Dialog 再开由父级卸载/挂载决定——`MobileAccessCard` 每次 mount 重新 load，因此跨打开不记忆（符合决策）。
2. 用户点击芯片 → 更新 `selectedId`，清空 `copied`，触发二维码重渲染。
3. **仅一条** entry 时：仍可显示单芯片或隐藏芯片条（实现选：**条目 ≥ 2 才显示芯片条**；1 条时布局与现在接近，只显示 URL+二维码）。
4. 复制始终复制 **选中 entry 的 url**；无选中则 no-op。
5. 风险文案、loading/empty/error 区域保持现有合同。

### 8.3 交互与无障碍

- 芯片容器：`role="radiogroup"`，`aria-label` 使用 i18n `mobileAccess.networkGroupLabel`。
- 单个芯片：`role="radio"`，`aria-checked`，键盘 ←/→ 或 Tab+Space/Enter 切换（最少支持点击 + 基本键盘激活）。
- 当前选中芯片视觉：accent 边框/背景，与 secondary 按钮区分。
- 选中 URL 仍用 mono `<code>`；二维码 `aria-label` 不变。
- compact 模式（AppShell Dialog）：芯片可横向滚动（`overflow-x: auto`），避免撑破窄弹层。

### 8.4 布局（相对截图）

```
标题 + 描述
警告文案
[ 芯片1 ] [ 芯片2 ] [ 芯片3 ]   ← 仅 entries.length >= 2
[ 选中 URL                    ]
[ 复制链接 ] [ 刷新 ]
[ 二维码 ]
```

compact 下二维码可仍在 URL 下方（现有 compact 单列 grid）。

### 8.5 i18n（zh/en `settings.json` → `mobileAccess`）

新增键（文案可微调，语义固定）：

| key | zh 示例 | en 示例 |
| --- | --- | --- |
| `networkGroupLabel` | 选择局域网 | Select network |
| `roleWifi` | Wi‑Fi · {{ip}} | Wi‑Fi · {{ip}} |
| `roleWired` | 有线 · {{ip}} | Ethernet · {{ip}} |

既有 `title`/`description`/`warning`/`copy`/`refresh`/`qrLabel` 等保持；`warning` 固定句合同测试不得削弱。

## 9. 文件影响面

| 区域 | 文件 | 变更 |
| --- | --- | --- |
| 后端模型 | `src-tauri/src/mobile/mod.rs` | DTO 扩展、`build_mobile_access_info`、单测 |
| 发现 | `src-tauri/src/net/discovery.rs`（或 mobile 子模块） | `list_mobile_access_candidates`、黑名单、角色、单测 |
| 依赖 | `src-tauri/Cargo.toml` | 直接依赖 `if-addrs` |
| HTTP | `src-tauri/src/net/routes/mobile.rs` | 改用新候选列表 |
| Command | `src-tauri/src/commands/mobile.rs` | 同上 |
| 类型 | `web/src/lib/types/core.ts` | `MobileAccessEntry` / 扩展 `MobileAccessInfo` |
| API | `web/src/api/mobile.ts` | 类型透传即可（若有 decoder 则同步） |
| UI | `MobileAccessCard.tsx` / `.module.css` / `mobileQr.ts`（+ selection helper） | 芯片 + 选中态 |
| i18n | `web/src/i18n/locales/{zh,en}/settings.json` | 新键 |
| 测试 | `mobileAccessCard.test.ts`、mobile 模块 Rust tests | 扩展 |
| 文档 | 本 spec；实现后更新相关 `CLAUDE.md` 需求描述（非 changelog） | 按项目规范 |
| PRD | 若 `docs/prd.md` 描述 mobile 访问入口，同步「多网段可选」一句 | 有则改 |

**不改**：`/mobile` SPA、鉴权、端口策略、mDNS 单 IP advertise。

## 10. 错误与空态

| 场景 | 行为 |
| --- | --- |
| 枚举失败 | 回退 `local_lan_ip()` 单元素候选；仍失败则 `entries=[]`，UI empty + 刷新 |
| 全部被黑名单过滤 | empty 文案 + 刷新 |
| 二维码生成失败 | 保留 URL/复制，error 提示 |
| 剪贴板不可用 | 既有 `copyUnavailable` |
| 刷新中 | 按钮 loading；保留上一帧 info 直至成功替换（避免闪空） |

## 11. 测试计划

### 11.1 Rust

1. 黑名单：docker0/veth/vmnet/awdl/lo 被过滤；`en0`/`wlan0`/`eth0`/`utun` 等按规则保留。
2. 角色：wifi/wired/None 样例接口名。
3. `build_mobile_access_info`：多候选 → 多 entries + urls 对齐；默认 IP 标记；去重；IPv6 方括号；loopback 仍过滤。
4. 排序：default 在前，其余字典序。

### 11.2 前端

1. `resolveMobileAccessEntries` / `selectDefaultMobileAccessEntryId` / `formatMobileAccessChipLabel` 纯函数单测。
2. 既有 `mobileAccessCard` 风险文案与「有 URL 时展示 QR 区域」合同保持。
3. 可选：组件级断言 chips 仅在 ≥2 条时出现（若项目对 MobileAccessCard 以源码合同测试为主，则延续源码/纯函数风格，不强行上 RTL 除非已有模式）。

### 11.3 手工 / 真机

1. 单网卡：弹层无芯片条，行为与现网一致。
2. 双网卡（如 Wi‑Fi + 有线）：两芯片，切换后 URL/二维码变化，复制内容为选中 URL。
3. 手机分别连不同网段扫码，仅对应网段可打开 `/mobile`。
4. 刷新后默认仍回到出站网段；同次会话中若刷新后旧网段仍在则保持选中。

## 12. 风险与缓解

| 风险 | 缓解 |
| --- | --- |
| macOS `en0` 未必永远是 Wi‑Fi | 仅作启发式；错了仍显示可用 IP；用户可点其它芯片 |
| 黑名单误杀 VPN/特殊桥 | `utun` 默认保留；表可迭代 |
| Windows 接口名为本地化语言 | 匹配 `ethernet`/`wi-fi`/`wlan` 等常见英/中片段；失败则纯 IP |
| DTO 扩展导致旧客户端 | 同仓桌面/移动同步发版；`urls` 仍在 |
| 芯片过多撑破 Dialog | 横向滚动 + compact 单列 |

## 13. 验收标准

1. 多局域网设备上，弹层可列出 ≥2 个非 loopback 可达地址（在黑名单外）。
2. 切换芯片后，展示 URL、二维码、复制内容三者一致且均为该网段。
3. 默认选中等于 `local_lan_ip()` 对应地址（若该地址在列表中）。
4. 角色可知时芯片为「Wi‑Fi · IP」或「有线 · IP」；不可知时仅为 IP。
5. 单地址时 UI 不出现无意义的多选芯片条。
6. 固定风险文案与「有合法 URL 就展示 QR」合同测试通过。
7. `get_mobile_access_info` 与 `GET /api/mobile/access-info` 返回同一结构语义。

## 14. 实现顺序建议

1. Cargo 直接依赖 `if-addrs` + `list_mobile_access_candidates` + 单测。
2. 扩展 `MobileAccessInfoDto` / `build_mobile_access_info` + command/route 接线 + 单测。
3. 前端类型 + selection 纯函数 + 单测。
4. `MobileAccessCard` UI/CSS/i18n。
5. 更新 CLAUDE.md 相关需求句（web / 若 backend 文档提及 access-info）与 PRD（如有）。
6. 按项目规范跑相关 `npm test` / `cargo test` 子集验证。

## 15. 开放问题

无。用户已确认交互 A、标签 C、默认 A、过滤 B，并授权直接写 spec + plan。
