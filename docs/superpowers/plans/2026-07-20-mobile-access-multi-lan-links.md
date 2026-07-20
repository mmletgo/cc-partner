# Mobile Access 多局域网链接选择 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让桌面端 Mobile Workbench 访问弹层列出本机多个局域网入口，用户可用分段芯片切换选中网段，使 URL / 复制 / 二维码三者始终一致。

**Architecture:** 后端用 `if-addrs` 枚举网卡地址，经黑名单过滤与 wifi/wired 角色启发式后，产出结构化 `entries` + 兼容字段 `urls`；`get_mobile_access_info` 与 `GET /api/mobile/access-info` 共用组装路径。前端 `MobileAccessCard` 在 `entries.length >= 2` 时渲染 radiogroup 芯片，默认选中 `isDefault`（对应 `local_lan_ip()`），否则第一项；选中项驱动 URL 行、复制与 QR。

**Tech Stack:** Rust (axum, Tauri command, if-addrs 0.13), React 19 + TypeScript, Vitest, i18next, qrcode

**Spec:** `docs/superpowers/specs/2026-07-20-mobile-access-multi-lan-links-design.md`

## Global Constraints

- 对话与用户可见文案：中文为主，i18n 同步 en
- 新增/修改 Rust 与 TS 函数须有中文 Business Logic / Code Logic docstring（项目规范）
- 风险文案合同不变：中文须含「同一可达网络中的任何设备均可读取、写入和执行；系统不验证调用者身份」；禁止「安全/已认证/可信设备」
- 角色仅 `wifi` | `wired`；推断失败只显示 IP，不加「其他」
- 默认选中：优先默认出站 IP（`local_lan_ip()`）；不记忆跨弹层选择
- `urls` 必须与 `entries[].url` 同序派生；两端 API 语义一致
- `local_lan_ip()` 单 IP 语义保留给 mDNS / 防火墙依赖，不改为多地址
- 不引入鉴权；不改 `/mobile` SPA；不改 HTTP 监听策略
- 代码改动无需向后兼容旧 DTO 客户端（同仓同步发版），但同仓前端必须同 PR 适配
- Python 类型规范不适用；TS 保持严格类型
- hooks 必须在 early return 之前
- 验证用 `npm test -- <path>` / `cargo test <filter> --lib`，禁止文档式 `npx tsx` 单文件 runner 作为主路径
- 完成功能后更新相关目录 `CLAUDE.md` 需求描述与 `docs/prd.md` 中 access-info 一句

## File map

| File | Responsibility |
| --- | --- |
| `src-tauri/Cargo.toml` | 直接依赖 `if-addrs = "0.13"` |
| `src-tauri/src/net/discovery.rs` | `is_blocked_mobile_interface_name`、`infer_mobile_access_role`、`list_mobile_access_candidates` 及单测 |
| `src-tauri/src/mobile/mod.rs` | `MobileAccessRole` / `MobileAccessEntryDto` / 扩展 `MobileAccessInfoDto`；`MobileAccessCandidate`；重写 `build_mobile_access_info`；扩展单测 |
| `src-tauri/src/commands/mobile.rs` | 改用候选列表 + 默认出站标记 |
| `src-tauri/src/net/routes/mobile.rs` | 同上 |
| `web/src/lib/types/core.ts` | `MobileAccessRole` / `MobileAccessEntry` / 扩展 `MobileAccessInfo` |
| `web/src/components/domain/MobileAccessCard/mobileAccessSelection.ts` | 纯函数：entries 解析、默认选中、芯片文案 |
| `web/src/components/domain/MobileAccessCard/mobileQr.ts` | 保留 QR；`selectPrimaryMobileUrl` 可改为基于 entries 或保留给兼容 |
| `web/src/components/domain/MobileAccessCard/MobileAccessCard.tsx` | 芯片 UI + 选中态 |
| `web/src/components/domain/MobileAccessCard/MobileAccessCard.module.css` | 芯片条样式 |
| `web/src/components/domain/MobileAccessCard/mobileAccessCard.test.ts` | 选择纯函数 + 风险文案合同 + 芯片合同 |
| `web/src/i18n/locales/zh/settings.json` | `networkGroupLabel` / `roleWifi` / `roleWired` |
| `web/src/i18n/locales/en/settings.json` | 同上 |
| `web/CLAUDE.md` | MobileAccessCard 多网段需求句 |
| `src-tauri/CLAUDE.md` | access-info entries 需求句 |
| `docs/prd.md` | access-info 说明补「多网段 entries」 |

---

### Task 1: 接口黑名单、角色启发式与候选枚举（Rust）

**Files:**
- Modify: `src-tauri/Cargo.toml`（`[dependencies]` 增加 `if-addrs = "0.13"`）
- Modify: `src-tauri/src/net/discovery.rs`（在 `local_lan_ip` 附近新增可测纯函数 + `list_mobile_access_candidates`）
- Test: `src-tauri/src/net/discovery.rs` 内 `#[cfg(test)]`

**Interfaces:**
- Consumes: `if_addrs::get_if_addrs`, `std::net::IpAddr`
- Produces:
  - `pub fn is_blocked_mobile_interface_name(name: &str) -> bool`
  - `pub fn infer_mobile_access_role(interface_name: &str) -> Option<&'static str>` 返回 `"wifi"` | `"wired"` | `None`（字符串便于跨模块；Task 2 再映射 enum）**或** 直接返回后续 `MobileAccessRole`——实现时 **优先** 在 `mobile` 模块放 role enum，discovery 只返回 `Option<String>` role 标签 `"wifi"`/`"wired"`，避免 discovery↔mobile 循环依赖。**定案：** discovery 产出：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MobileAccessCandidate {
    pub host: String,
    /// "wifi" | "wired"；未知为 None
    pub role: Option<&'static str>,
    pub ifa_name: String,
}

pub fn is_blocked_mobile_interface_name(name: &str) -> bool;
pub fn infer_mobile_access_role(interface_name: &str) -> Option<&'static str>;
pub fn list_mobile_access_candidates() -> Vec<MobileAccessCandidate>;
```

- [ ] **Step 1: 在 Cargo.toml 加入直接依赖**

在 `src-tauri/Cargo.toml` 的 `[dependencies]` 中（建议放在 `hostname` 附近）加入：

```toml
if-addrs = "0.13"
```

运行：

```bash
cd src-tauri && cargo check -q
```

Expected: 成功（lock 已有 0.13.4，直接依赖应解析稳定）

- [ ] **Step 2: 写失败单测（黑名单 + 角色）**

在 `src-tauri/src/net/discovery.rs` 的 `#[cfg(test)] mod tests`（若无则新建）追加：

```rust
#[test]
fn blocked_mobile_interface_names_cover_virtual_and_loopback() {
    for name in [
        "lo", "lo0", "docker0", "br-abc", "veth123", "cni0", "flannel.1",
        "vmnet1", "vboxnet0", "virbr0", "vEthernet (WSL)", "awdl0", "llw0",
        "bridge0", "gif0", "stf0", "ap1",
    ] {
        assert!(
            is_blocked_mobile_interface_name(name),
            "expected blocked: {name}"
        );
    }
    for name in ["en0", "en1", "eth0", "wlan0", "wlp2s0", "utun4", "Ethernet", "Wi-Fi"] {
        assert!(
            !is_blocked_mobile_interface_name(name),
            "expected allowed: {name}"
        );
    }
}

#[test]
fn infer_mobile_access_role_wifi_wired_or_none() {
    assert_eq!(infer_mobile_access_role("wlan0"), Some("wifi"));
    assert_eq!(infer_mobile_access_role("wlp2s0"), Some("wifi"));
    assert_eq!(infer_mobile_access_role("Wi-Fi"), Some("wifi"));
    assert_eq!(infer_mobile_access_role("eth0"), Some("wired"));
    assert_eq!(infer_mobile_access_role("enp0s3"), Some("wired"));
    assert_eq!(infer_mobile_access_role("Ethernet"), Some("wired"));
    assert_eq!(infer_mobile_access_role("以太网"), Some("wired"));
    assert_eq!(infer_mobile_access_role("utun4"), None);
    // macOS en0 → wifi；其它平台 en0 在本函数内用 cfg(target_os = "macos") 分支
    #[cfg(target_os = "macos")]
    assert_eq!(infer_mobile_access_role("en0"), Some("wifi"));
    #[cfg(not(target_os = "macos"))]
    assert_eq!(infer_mobile_access_role("en0"), None);
}
```

- [ ] **Step 3: 跑测确认失败**

```bash
cd src-tauri && cargo test blocked_mobile_interface_names_cover_virtual_and_loopback infer_mobile_access_role_wifi_wired_or_none --lib
```

Expected: FAIL（函数未定义）

- [ ] **Step 4: 实现黑名单、角色与 list**

在 `local_lan_ip` 旁实现（docstring 用项目中文格式）：

```rust
/// 判断网卡名是否应从移动端扫码候选中排除。
///
/// Business Logic（为什么需要这个函数）:
///     Docker/VM/桥接/loopback 地址对手机浏览器通常不可达或噪音过大，进入二维码列表会误导用户。
///
/// Code Logic（这个函数做什么）:
///     将接口名小写并去掉多余空白后，按前缀/全名黑名单匹配；命中返回 true。
pub fn is_blocked_mobile_interface_name(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase().replace(' ', "");
    if n.is_empty() {
        return true;
    }
    const EXACT: &[&str] = &["lo", "lo0"];
    if EXACT.contains(&n.as_str()) {
        return true;
    }
    const PREFIXES: &[&str] = &[
        "docker", "br-", "veth", "cni", "flannel", "cbridge",
        "vmnet", "vbox", "virbr", "hyper-v", "vethernet",
        "awdl", "llw", "ap", "bridge", "gif", "stf", "p2p",
    ];
    PREFIXES.iter().any(|p| n.starts_with(p))
}

/// 从接口名启发式推断 wifi/wired 角色。
///
/// Business Logic（为什么需要这个函数）:
///     芯片标签在可识别时显示「Wi‑Fi / 有线 · IP」，帮助用户选择手机所在网段。
///
/// Code Logic（这个函数做什么）:
///     小写匹配常见 wifi/wired 命名；macOS 仅 `en0` 视为 wifi；无法判断返回 None。
pub fn infer_mobile_access_role(interface_name: &str) -> Option<&'static str> {
    let n = interface_name.trim().to_ascii_lowercase();
    if n.is_empty() {
        return None;
    }
    // wifi
    if n.contains("wi-fi")
        || n.contains("wifi")
        || n.contains("wlan")
        || n.contains("airport")
        || n.starts_with("wl")
    {
        return Some("wifi");
    }
    #[cfg(target_os = "macos")]
    if n == "en0" {
        return Some("wifi");
    }
    // wired（中文「以太网」在 to_ascii_lowercase 后仍保留）
    let raw = interface_name.trim();
    if raw.contains("以太网")
        || n.contains("ethernet")
        || n.starts_with("eth")
        || n.starts_with("enp")
        || n.starts_with("ens")
        || n.starts_with("eno")
        || n.starts_with("em")
    {
        return Some("wired");
    }
    None
}

/// 枚举可供手机扫码的局域网候选地址。
///
/// Business Logic（为什么需要这个函数）:
///     单默认出站 IP 无法覆盖多网卡场景；手机可能连在非默认网段。
///
/// Code Logic（这个函数做什么）:
///     调用 if-addrs 枚举接口，过滤 loopback/link-local/未指定/黑名单接口，
///     生成 host + 可选 role；失败返回空 Vec（调用方可回退 local_lan_ip）。
pub fn list_mobile_access_candidates() -> Vec<MobileAccessCandidate> {
    let Ok(ifaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for iface in ifaces {
        if is_blocked_mobile_interface_name(&iface.name) {
            continue;
        }
        if iface.is_loopback() {
            continue;
        }
        // if-addrs Interface 提供 addr; 使用 is_link_local
        let ip = iface.ip();
        match ip {
            IpAddr::V4(v4) if v4.is_link_local() || v4.is_unspecified() || v4.is_loopback() => {
                continue;
            }
            IpAddr::V6(v6)
                if v6.is_loopback()
                    || v6.is_unspecified()
                    || (v6.segments()[0] & 0xffc0) == 0xfe80 =>
            {
                continue;
            }
            _ => {}
        }
        // 额外：部分平台 link_local 方法
        if matches!(&iface.addr, if_addrs::IfAddr::V4(a) if a.is_link_local())
            || matches!(&iface.addr, if_addrs::IfAddr::V6(a) if a.is_link_local())
        {
            continue;
        }
        let host = ip.to_string();
        if !seen.insert(host.clone()) {
            continue;
        }
        out.push(MobileAccessCandidate {
            host,
            role: infer_mobile_access_role(&iface.name),
            ifa_name: iface.name,
        });
    }
    out
}
```

注意：按实际 `if-addrs` API 调整 `iface.ip()` / `is_loopback` / `IfAddr::is_link_local` 调用（crate 的 `Interface` 有 `ip()`、`is_loopback()`，`IfAddr` 有 `is_link_local()`）。实现后编译修通。

- [ ] **Step 5: 跑测确认通过**

```bash
cd src-tauri && cargo test blocked_mobile_interface_names_cover_virtual_and_loopback infer_mobile_access_role_wifi_wired_or_none --lib
```

Expected: PASS

可选冒烟（非门禁）：

```bash
cd src-tauri && cargo test list_mobile_access --lib
```

可加一个不断言具体 IP 的测试：`list_mobile_access_candidates` 不 panic，且结果中无 `127.0.0.1`。

- [ ] **Step 6: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/net/discovery.rs
git commit -m "$(cat <<'EOF'
feat(net): enumerate multi-LAN mobile access candidates

Add if-addrs-based interface listing with virtual-NIC blacklist and
wifi/wired role heuristics for Mobile Workbench QR selection.

Co-Authored-By: Claude <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: 扩展 MobileAccessInfoDto 与 build_mobile_access_info

**Files:**
- Modify: `src-tauri/src/mobile/mod.rs`
- Test: 同文件 `tests` 模块（更新既有断言 + 新用例）

**Interfaces:**
- Consumes: `MobileAccessCandidate` from `crate::net::discovery`（或为避免层耦合，在 mobile 内定义同构 candidate 并由调用方转换——**定案：mobile 定义 `MobileAccessCandidate` 结构，discovery 的 list 返回与之兼容的字段；推荐把 `MobileAccessCandidate` 放在 `mobile/mod.rs`，discovery 的 list 返回后由 command/route 组装，或 discovery 返回裸 `(host, role, name)`。为减少改动：**`list_mobile_access_candidates` 放在 discovery，返回 discovery 的 struct；mobile 的 `build` 接受 `Vec<MobileAccessCandidate>` 通过 re-export 或 duplicate 最小字段。**

**定案（实现必须遵守）：**

```rust
// mobile/mod.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MobileAccessCandidate {
    pub host: String,
    pub role: Option<MobileAccessRole>,
    pub ifa_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MobileAccessRole {
    Wifi,
    Wired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileAccessEntryDto {
    pub id: String,
    pub url: String,
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<MobileAccessRole>,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileAccessInfoDto {
    pub device_name: String,
    pub port: u16,
    pub urls: Vec<String>,
    pub entries: Vec<MobileAccessEntryDto>,
}

pub fn build_mobile_access_info(
    config: &AppConfig,
    port: u16,
    candidates: Vec<MobileAccessCandidate>,
    default_host: Option<&str>,
) -> MobileAccessInfoDto;
```

discovery 的 list 可继续返回 `role: Option<&'static str>`，在 command/route 映射为 `MobileAccessRole`：

```rust
fn map_role(raw: Option<&str>) -> Option<MobileAccessRole> {
    match raw {
        Some("wifi") => Some(MobileAccessRole::Wifi),
        Some("wired") => Some(MobileAccessRole::Wired),
        _ => None,
    }
}
```

若 Task 1 已在 discovery 定义了同名 struct，**本 Task 删除 discovery 版，统一到 mobile**，discovery 只保留 `is_blocked_*` / `infer_*` / list 返回 `Vec<(String /*host*/, Option<&'static str> role, String ifa_name)>` 或 mobile 的 candidate。实现者选一种并保证编译；推荐 **list 放 discovery，candidate DTO 放 mobile，list 映射一次**。

- [ ] **Step 1: 更新既有测试为新签名并添加 entries 用例（先写期望）**

将 `access_info_filters_loopback_urls` 等改为传入 `MobileAccessCandidate`，断言含 `entries`：

```rust
#[test]
fn access_info_builds_entries_marks_default_and_sorts() {
    let config = test_config();
    let candidates = vec![
        MobileAccessCandidate {
            host: "10.0.0.5".into(),
            role: Some(MobileAccessRole::Wired),
            ifa_name: "eth0".into(),
        },
        MobileAccessCandidate {
            host: "192.168.1.23".into(),
            role: Some(MobileAccessRole::Wifi),
            ifa_name: "wlan0".into(),
        },
        MobileAccessCandidate {
            host: "127.0.0.1".into(),
            role: None,
            ifa_name: "lo".into(),
        },
    ];
    let info = build_mobile_access_info(&config, 14203, candidates, Some("192.168.1.23"));
    assert_eq!(info.port, 14203);
    assert_eq!(info.urls.len(), 2);
    assert_eq!(info.entries.len(), 2);
    // default first
    assert_eq!(info.entries[0].host, "192.168.1.23");
    assert!(info.entries[0].is_default);
    assert_eq!(info.entries[0].role, Some(MobileAccessRole::Wifi));
    assert_eq!(info.entries[0].url, "http://192.168.1.23:14203/mobile");
    assert_eq!(info.urls[0], info.entries[0].url);
    assert_eq!(info.entries[1].host, "10.0.0.5");
    assert!(!info.entries[1].is_default);
}
```

同步改写原 loopback/trim/dedup/ipv6 测试的构造参数与 `assert_eq!(info, MobileAccessInfoDto { ..., entries: ... })`。

- [ ] **Step 2: 跑测确认失败**

```bash
cd src-tauri && cargo test mobile::tests --lib
```

Expected: FAIL（结构/签名不匹配）

- [ ] **Step 3: 实现 DTO 与 build**

核心逻辑：

1. `normalize_mobile_host` 过滤每个 candidate.host  
2. host 去重（HashSet）  
3. 生成 `url = format!("http://{}:{port}/mobile", format_url_host(&host))`  
4. `id = host.clone()`  
5. `is_default = default_host` 归一化后与 host 相等（仅第一个 true）  
6. 排序：`is_default` desc，然后 `host` asc  
7. `urls = entries.iter().map(|e| e.url.clone()).collect()`

- [ ] **Step 4: 跑测确认通过**

```bash
cd src-tauri && cargo test mobile::tests --lib
```

Expected: PASS（含 `access_info_filters_loopback_urls` 等全部）

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/mobile/mod.rs src-tauri/src/net/discovery.rs
git commit -m "$(cat <<'EOF'
feat(mobile): return structured multi-LAN access entries

Extend MobileAccessInfoDto with entries (url/host/role/isDefault) while
keeping urls derived in the same order for existing consumers.

Co-Authored-By: Claude <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: 接线 command 与 HTTP route

**Files:**
- Modify: `src-tauri/src/commands/mobile.rs`
- Modify: `src-tauri/src/net/routes/mobile.rs`

**Interfaces:**
- Consumes: `list_mobile_access_candidates`, `local_lan_ip`, `build_mobile_access_info`, `MobileAccessCandidate`, `MobileAccessRole`
- Produces: 相同 invoke/HTTP 路径，JSON 多 `entries` 字段

- [ ] **Step 1: 抽出共享组装（可选内联）**

两处改为：

```rust
fn collect_mobile_access_info(state: &AppState) -> MobileAccessInfoDto {
    let config = state.config.read().expect("config 读锁中毒").clone();
    let port = state.actual_http_port.load(Ordering::SeqCst);
    let default_ip = local_lan_ip().map(|ip| ip.to_string());
    let mut candidates: Vec<MobileAccessCandidate> = list_mobile_access_candidates()
        .into_iter()
        .map(|c| MobileAccessCandidate {
            host: c.host, // 按 Task1/2 实际字段映射
            role: match c.role {
                Some("wifi") => Some(MobileAccessRole::Wifi),
                Some("wired") => Some(MobileAccessRole::Wired),
                _ => None,
            },
            ifa_name: c.ifa_name,
        })
        .collect();
    if candidates.is_empty() {
        if let Some(ip) = default_ip.clone() {
            candidates.push(MobileAccessCandidate {
                host: ip,
                role: None,
                ifa_name: String::new(),
            });
        }
    }
    build_mobile_access_info(&config, port, candidates, default_ip.as_deref())
}
```

若不想在两处复制，可放 `mobile/mod.rs`：

```rust
pub fn mobile_access_info_from_state(config: &AppConfig, port: u16) -> MobileAccessInfoDto
```

内部完成 list + fallback + build。**推荐此法**，command/route 只读 state 后调用。

- [ ] **Step 2: 更新 command/route 文档注释**（说明多网段 entries）

- [ ] **Step 3: 编译 + 相关测试**

```bash
cd src-tauri && cargo test mobile::tests access_info --lib
```

Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/commands/mobile.rs src-tauri/src/net/routes/mobile.rs src-tauri/src/mobile/mod.rs
git commit -m "$(cat <<'EOF'
feat(mobile): wire multi-LAN candidates into access-info APIs

Use interface enumeration for get_mobile_access_info and
GET /api/mobile/access-info, falling back to local_lan_ip when empty.

Co-Authored-By: Claude <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: 前端类型与选择纯函数

**Files:**
- Modify: `web/src/lib/types/core.ts`（`MobileAccessInfo` 段）
- Create: `web/src/components/domain/MobileAccessCard/mobileAccessSelection.ts`
- Modify: `web/src/components/domain/MobileAccessCard/mobileAccessCard.test.ts`
- Modify: `web/src/components/domain/MobileAccessCard/mobileQr.ts`（可选：保留 `selectPrimaryMobileUrl` 并标注兼容；新逻辑走 selection 模块）

**Interfaces:**
- Produces:

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

export function resolveMobileAccessEntries(info: MobileAccessInfo | null | undefined): MobileAccessEntry[];
export function selectDefaultMobileAccessEntryId(entries: MobileAccessEntry[]): string | null;
export function formatMobileAccessChipLabel(
  entry: MobileAccessEntry,
  labels: { wifi: (ip: string) => string; wired: (ip: string) => string },
): string;
export function resolveSelectedMobileAccessEntry(
  entries: MobileAccessEntry[],
  selectedId: string | null,
): MobileAccessEntry | null;
```

- [ ] **Step 1: 写失败单测**

在 `mobileAccessCard.test.ts` 增加：

```ts
import {
  formatMobileAccessChipLabel,
  resolveMobileAccessEntries,
  resolveSelectedMobileAccessEntry,
  selectDefaultMobileAccessEntryId,
} from './mobileAccessSelection';

test('resolveMobileAccessEntries prefers entries and falls back to urls', () => {
  const fromEntries = resolveMobileAccessEntries({
    deviceName: 'd',
    port: 1,
    urls: ['http://1.1.1.1:1/mobile'],
    entries: [
      {
        id: '192.168.1.2',
        url: 'http://192.168.1.2:1/mobile',
        host: '192.168.1.2',
        role: 'wifi',
        isDefault: true,
      },
    ],
  });
  assertEqual(fromEntries.length, 1, 'entries length');
  assertEqual(fromEntries[0]?.host, '192.168.1.2', 'host');

  const fromUrls = resolveMobileAccessEntries({
    deviceName: 'd',
    port: 1,
    urls: ['http://10.0.0.2:1/mobile', ''],
    entries: [],
  });
  assertEqual(fromUrls.length, 1, 'url fallback length');
  assertEqual(fromUrls[0]?.host, '10.0.0.2', 'url host');
  assertEqual(fromUrls[0]?.isDefault, true, 'first url default');
});

test('selectDefaultMobileAccessEntryId prefers isDefault', () => {
  const id = selectDefaultMobileAccessEntryId([
    { id: 'a', url: 'u1', host: 'a', isDefault: false },
    { id: 'b', url: 'u2', host: 'b', isDefault: true },
  ]);
  assertEqual(id, 'b', 'default id');
});

test('formatMobileAccessChipLabel uses role or bare ip', () => {
  const labels = {
    wifi: (ip: string) => `Wi‑Fi · ${ip}`,
    wired: (ip: string) => `有线 · ${ip}`,
  };
  assertEqual(
    formatMobileAccessChipLabel(
      { id: '1', url: 'u', host: '1.2.3.4', role: 'wifi', isDefault: false },
      labels,
    ),
    'Wi‑Fi · 1.2.3.4',
    'wifi label',
  );
  assertEqual(
    formatMobileAccessChipLabel(
      { id: '1', url: 'u', host: '1.2.3.4', isDefault: false },
      labels,
    ),
    '1.2.3.4',
    'bare ip',
  );
});

test('resolveSelectedMobileAccessEntry keeps selection or falls back', () => {
  const entries = [
    { id: 'a', url: 'u1', host: 'a', isDefault: true },
    { id: 'b', url: 'u2', host: 'b', isDefault: false },
  ];
  assertEqual(resolveSelectedMobileAccessEntry(entries, 'b')?.id, 'b', 'keep');
  assertEqual(resolveSelectedMobileAccessEntry(entries, 'missing')?.id, 'a', 'fallback default');
});
```

- [ ] **Step 2: 跑测确认失败**

```bash
cd web && npm test -- src/components/domain/MobileAccessCard/mobileAccessCard.test.ts
```

Expected: FAIL（模块不存在）

- [ ] **Step 3: 实现 types + mobileAccessSelection.ts**

`resolveMobileAccessEntries`：若 `info.entries?.length` 用 entries；否则从非空 `urls` 解析 host（`new URL(url).hostname` 去掉 IPv6 方括号），`id=host`，第一条 `isDefault=true`。

`selectDefaultMobileAccessEntryId`：`entries.find(e => e.isDefault)?.id ?? entries[0]?.id ?? null`。

`formatMobileAccessChipLabel`：role wifi/wired 调 labels，否则 `entry.host`。

`resolveSelectedMobileAccessEntry`：id 命中返回；否则 default/first。

- [ ] **Step 4: 跑测确认通过**

```bash
cd web && npm test -- src/components/domain/MobileAccessCard/mobileAccessCard.test.ts
```

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add web/src/lib/types/core.ts web/src/components/domain/MobileAccessCard/mobileAccessSelection.ts web/src/components/domain/MobileAccessCard/mobileAccessCard.test.ts
git commit -m "$(cat <<'EOF'
feat(web): add mobile access multi-LAN selection helpers

Introduce MobileAccessEntry types and pure functions for default
selection and chip labels used by MobileAccessCard.

Co-Authored-By: Claude <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: MobileAccessCard UI、样式与 i18n

**Files:**
- Modify: `web/src/components/domain/MobileAccessCard/MobileAccessCard.tsx`
- Modify: `web/src/components/domain/MobileAccessCard/MobileAccessCard.module.css`
- Modify: `web/src/i18n/locales/zh/settings.json`（`mobileAccess` 对象）
- Modify: `web/src/i18n/locales/en/settings.json`
- Modify: `web/src/components/domain/MobileAccessCard/mobileAccessCard.test.ts`（芯片合同：源码含 radiogroup / entries.length >= 2）

**Interfaces:**
- Consumes: `getMobileAccessInfo`, selection helpers, `renderMobileQrSvg`
- Produces: 用户可见分段芯片 + 单 URL/QR

- [ ] **Step 1: 增加 i18n 键**

`zh/settings.json` → `mobileAccess`：

```json
"networkGroupLabel": "选择局域网",
"roleWifi": "Wi‑Fi · {{ip}}",
"roleWired": "有线 · {{ip}}"
```

`en/settings.json`：

```json
"networkGroupLabel": "Select network",
"roleWifi": "Wi‑Fi · {{ip}}",
"roleWired": "Ethernet · {{ip}}"
```

- [ ] **Step 2: 更新 MobileAccessCard 状态逻辑**

要点（hooks 全部在任何 early return 之前——本组件当前无 early return，保持）：

```tsx
const entries = useMemo(() => resolveMobileAccessEntries(info), [info]);
const [selectedId, setSelectedId] = useState<string | null>(null);

// load 成功后：
// const next = await getMobileAccessInfo();
// setInfo(next);
// const nextEntries = resolveMobileAccessEntries(next);
// setSelectedId((prev) =>
//   prev && nextEntries.some((e) => e.id === prev)
//     ? prev
//     : selectDefaultMobileAccessEntryId(nextEntries),
// );

const selectedEntry = useMemo(
  () => resolveSelectedMobileAccessEntry(entries, selectedId),
  [entries, selectedId],
);
const primaryUrl = selectedEntry?.url ?? null;
```

二维码 effect 依赖 `primaryUrl`（已有模式）。

复制 `copyPrimaryUrl` 使用 `primaryUrl`。

JSX：在 warning 与 URL 之间，当 `entries.length >= 2`：

```tsx
<div
  className={styles.networkGroup}
  role="radiogroup"
  aria-label={t('mobileAccess.networkGroupLabel')}
  data-testid="mobile-access-network-group"
>
  {entries.map((entry) => {
    const selected = entry.id === selectedEntry?.id;
    return (
      <button
        key={entry.id}
        type="button"
        role="radio"
        aria-checked={selected}
        className={selected ? styles.networkChipSelected : styles.networkChip}
        data-testid={`mobile-access-network-${entry.id}`}
        onClick={() => {
          setSelectedId(entry.id);
          setCopied(false);
        }}
      >
        {formatMobileAccessChipLabel(entry, {
          wifi: (ip) => t('mobileAccess.roleWifi', { ip }),
          wired: (ip) => t('mobileAccess.roleWired', { ip }),
        })}
      </button>
    );
  })}
</div>
```

URL 列表改为**只显示选中 URL**（不要 map 全部 urls）：

```tsx
{primaryUrl ? <code className={styles.url}>{primaryUrl}</code> : null}
```

- [ ] **Step 3: CSS**

```css
.networkGroup {
  display: flex;
  flex-wrap: nowrap;
  gap: var(--space-2);
  overflow-x: auto;
  max-width: 100%;
  padding-bottom: var(--space-1);
}

.networkChip,
.networkChipSelected {
  flex: 0 0 auto;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border-soft);
  background: var(--surface-warm);
  color: var(--fg);
  font-size: var(--text-xs);
  line-height: var(--leading-normal);
  padding: var(--space-1) var(--space-2);
  cursor: pointer;
}

.networkChipSelected {
  border-color: var(--accent, var(--fg));
  background: color-mix(in oklab, var(--accent, var(--fg)) 12%, var(--surface));
}
```

若项目 token 无 `--accent`，用已有 accent/primary 变量（查看其它 chip 样式，例如 Button primary 边框色）。

- [ ] **Step 4: 合同测试**

在 `mobileAccessCard.test.ts` 断言：

- 卡片源码包含 `role="radiogroup"` 与 `entries.length >= 2`（或 `>= 2`）条件；
- 不再对全部 `info?.urls` 做展示 map（可断言存在 `resolveSelectedMobileAccessEntry` / 单 `primaryUrl` 展示模式）；
- 既有 warning 合同仍通过。

- [ ] **Step 5: 跑测**

```bash
cd web && npm test -- src/components/domain/MobileAccessCard/mobileAccessCard.test.ts src/api/mobile.test.ts
```

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add web/src/components/domain/MobileAccessCard web/src/i18n/locales/zh/settings.json web/src/i18n/locales/en/settings.json
git commit -m "$(cat <<'EOF'
feat(web): multi-LAN chip selector on MobileAccessCard

Show segmented network chips when multiple LAN URLs exist and bind
copy/QR to the selected entry with wifi/wired labels.

Co-Authored-By: Claude <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: 文档与 CLAUDE.md / PRD 同步

**Files:**
- Modify: `web/CLAUDE.md`（MobileAccessCard 段）
- Modify: `src-tauri/CLAUDE.md`（移动端访问信息段）
- Modify: `docs/prd.md`（`/api/mobile/access-info` 行说明）

- [ ] **Step 1: 更新 web/CLAUDE.md**

将 MobileAccessCard 描述改为包含：后端返回 `entries`（id/url/host/role?/isDefault）与派生 `urls`；≥2 条时分段芯片切换；默认 `isDefault`/`local_lan_ip`；角色仅 wifi/wired 否则纯 IP；复制/QR 跟随选中项。

- [ ] **Step 2: 更新 src-tauri/CLAUDE.md**

将 `{deviceName, port, urls}` 改为 `{deviceName, port, urls, entries}`；说明 `list_mobile_access_candidates` + 黑名单 + 角色启发式 + `isDefault` 出站标记；验证命令增加 multi-entry 测试名。

- [ ] **Step 3: 更新 docs/prd.md**

`/api/mobile/access-info` 说明改为：返回多网段 `entries`（及兼容 `urls`）供桌面弹层切换局域网链接/二维码（无身份鉴权）。

- [ ] **Step 4: Commit**

```bash
git add web/CLAUDE.md src-tauri/CLAUDE.md docs/prd.md
git commit -m "$(cat <<'EOF'
docs: document multi-LAN mobile access entries

Update CLAUDE.md and PRD so access-info multi-entry selection matches
the implemented MobileAccessCard behavior.

Co-Authored-By: Claude <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: 端到端子集验证

**Files:** 无新文件

- [ ] **Step 1: Rust 测试**

```bash
cd src-tauri && cargo test mobile::tests blocked_mobile_interface_names infer_mobile_access_role --lib
```

Expected: PASS

- [ ] **Step 2: 前端测试**

```bash
cd web && npm test -- src/components/domain/MobileAccessCard/mobileAccessCard.test.ts src/api/mobile.test.ts
```

Expected: PASS

- [ ] **Step 3: 类型/编译（若本地可跑）**

```bash
cd web && npm run build
```

Expected: 成功（或至少 tsc 无 MobileAccess 相关错误）

- [ ] **Step 4: 手工验收清单（报告给用户，不强制自动化）**

1. 单网卡：无芯片条，复制/QR 正常  
2. 多网卡：芯片切换后 URL/QR/剪贴板一致  
3. 默认选中为出站网段  
4. 风险文案仍在  

- [ ] **Step 5: 若有未提交文档小修则 commit；否则结束**

---

## Spec coverage self-check

| Spec 要求 | Task |
| --- | --- |
| 分段芯片交互 | Task 5 |
| 角色 wifi/wired 否则纯 IP | Task 1 + 4 + 5 |
| 默认出站 | Task 2 + 3 + 4 |
| 全量枚举 + 黑名单 | Task 1 |
| entries + urls 派生 | Task 2 |
| command/HTTP 一致 | Task 3 |
| 单条隐藏芯片 | Task 5 |
| 风险文案合同 | Task 5 测试 |
| 刷新保留同会话选中 | Task 5 load 逻辑 |
| 枚举失败回退 local_lan_ip | Task 3 |
| CLAUDE/PRD | Task 6 |
| 测试 | Task 1–5, 7 |
| 不改 mDNS 单 IP | Task 1 明确不改 `local_lan_ip` |
| 不改鉴权 /mobile SPA | 全任务未触及 |

## Placeholder / type consistency self-check

- 无 TBD/TODO 实现步骤  
- `MobileAccessRole`：Rust `Wifi/Wired` serde camelCase → JSON `"wifi"`/`"wired"` 与 TS 联合类型一致  
- `isDefault` / `is_default` camelCase 一致  
- `build_mobile_access_info(..., default_host: Option<&str>)` 在 Task 2/3 一致  
- discovery list 与 mobile candidate 映射在 Task 3 写清  

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-20-mobile-access-multi-lan-links.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — 每个 Task 派发新 subagent，Task 间审查，迭代快  
2. **Inline Execution** — 本会话用 executing-plans 按 Task 批量执行并设检查点  

Which approach?
