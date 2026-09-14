# 多地址设备自动延迟选路 — 设计（2026-09-14）

## 问题

同一设备（如 r9000p：LAN `192.168.6.17:62116` + Tailscale `100.113.214.69:62116`）有多个
可达网络地址。现状 `state.devices` 以 device_id 为键只存**一个** `Device{host,port}`，
`manual_peers.rs::upsert_device` 按 id 整体覆盖——两个源都探到同一设备时，地址每 15s
周期互相覆盖抖动，且无法按延迟择优。

## 目标

一个逻辑设备支持 N 个候选地址；对它的每次请求自动使用当前延迟最低的健康地址；
探测失败立即切换备选；不引入直连失败 fallback 中转（relay 三段解析语义不变）。

## 设计

### 1. Device 模型（models/device.rs）

`Device` 新增 `addresses: Vec<DeviceAddress>`：

```rust
pub struct DeviceAddress {
    pub host: String,
    pub port: u16,
    /// 最近健康探测 RTT（EMA 平滑）；None = 尚未测量（如 mDNS 事件刚发现）。
    pub rtt_ms: Option<f64>,
    /// 连续失败次数（达到 FAILURE_THRESHOLD 移除该行）。
    pub fail_count: u32,
    pub last_healthy: DateTime<Utc>,
}
```

- 保留 `host`/`port` 字段 = **当前活跃地址**（由 `select_active()` 计算），DTO（address/port）、
  `Device::base_url()`、`collect_device_dtos_with_shadows` 等全部消费方无需改动。
- `report_health(host, port, rtt_ms, health)`：upsert 地址行（rtt 用 EMA：`0.7*old + 0.3*new`；
  新行 rtt = 测量值），更新 name/proto/caps/last_seen，然后 `select_active()`。
- `report_failure(host)`：fail_count += 1；达 `FAILURE_THRESHOLD`(3) 移除该行；然后 `select_active()`。
- `select_active()`（核心选路，纯函数可测）：
  1. 候选 = `addresses` 中 `fail_count == 0` 的行；
  2. 活跃行（现 host/port）若仍在候选中且满足粘滞：`rtt_active <= best_rtt * SWITCH_RATIO`，保持；
  3. 否则切到 rtt 最小的候选（`None` 视为 +∞，即测过的优先于未测的）；
  4. 无候选 → `online = false`（所有地址都连续失败时才判定设备离线）。
- `SWITCH_RATIO = 0.6`（防抖：新路径 RTT 必须显著优于现路径才切换；LAN ~0.5ms vs Tailscale
  ~5–200ms 差距远超阈值，切换是即时的；反过来 LAN 断开时活跃行 fail_count>0 立即失格）。

### 2. 探测循环（net/manual_peers.rs）

- 候选集扩充：`manual_peers` ∪ Tailscale peers ∪ **state.devices 中已有地址行**
  （mDNS 发现的地址也会被纳入每轮 RTT 测量）。
- 每个地址独立探测并以 `Instant` 计时 RTT；成功 → `Device::report_health(...)`；
  失败 → `Device::report_failure(host)`。按 (device_id, host) 归组——两地址报同一 device_id
  自然合并为一个 Device 的两行，不再互相覆盖。
- `remove_device_by_host` 语义改为行级移除；全部行移除才删 Device 条目。
- `fail_counts` HashMap（按 base_url）保留为「连续失败计数」驱动行级 fail_count 的补充，
  或直接合并进行内 fail_count（实现取简）。

### 3. mDNS 发现（net/discovery.rs）

upsert 改为 `report_health(host, port, rtt=None)` 语义：mDNS 事件只登记地址行（未测量），
RTT 由下一轮探测循环补测；不再整体覆盖 Device。

### 4. 消费方（不改语义）

- `Device::base_url()` / `DeviceDto.address` = 活跃地址，GUI/Workbench/同步零改动。
- relay 三段解析（直连表命中 → 影子）不动：本特性只在直连表内部多地址择优，
  **不引入直连失败 fallback 中转**（保持 AGENTS.md relay 约定）。
- 长连接（终端 WS / NDJSON 流）在建立时选路，已建立的流不迁移。

### 5. 越界说明（v1 不做）

- 请求内连接失败即时切换备选地址（现靠探测周期 ≤15s 收敛切换；活跃行失败即刻失格，
  最坏 15s 内完成切换）。
- GUI 展示多地址/RTT 明细（DTO 已可扩展，后续再说）。

## 测试

- `select_active` 纯函数单测：LAN/Tailscale 双行择优、SWITCH_RATIO 粘滞、失败行失格、
  全部失败 → offline、未测量行排序。
- 探测归并：两地址同 device_id → 一个 Device 两行（async，构造 AppState）。
- relay_three_node_smoke 全套必须保持通过（单地址节点行为不变）。
- discovery upsert 合并语义测试。

## 部署注意

发布后，需要多地址的设备在 config.json `manual_peers` 里把两个地址都列上
（同 device_id 自动归并），或同网段由 mDNS 自动发现；无需其它配置。
