//! models/device.rs — 设备数据模型
//!
//! Business Logic（为什么需要这个模块）:
//!     P2P 局域网协作需要跟踪每个对端设备的连接信息（IP、端口）和在线状态，
//!     以便进行文件传输（M5）和 Prompt 同步（M4）。对照 Python `models/device.py`。
//!     同一逻辑设备可能有多个可达网络地址（如 LAN IP + Tailscale IP）；多源发现
//!     （mDNS / manual_peers / Tailscale）上报的地址按 device_id 归并为多行，由
//!     延迟择优自动选路，不再互相覆盖抖动。
//!
//! Code Logic（这个模块做什么）:
//!     - `Device`：内部使用的设备实体。`addresses` 为全部候选地址行（每地址一行），
//!       `host`/`port` 保留为「当前活跃地址」（由 `select_active()` 计算写回），
//!       因此 `base_url()` / DTO（address/port）等消费方形状不变。
//!     - `DeviceAddress`：单条候选地址（host/port/rtt/fail_count/last_healthy）。
//!     - `DeviceDto`：返回前端的 DTO（camelCase），对照前端 `web/src/lib/types.ts`。
//!       字段 address 对应内部 host（前端命名沿用旧 Python `/api/devices` 的 `address`）。

use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// 地址行连续失败多少次后从 Device 移除该行（防瞬时网络抖动反复增删）。
///
/// Business Logic: 单次探测超时可能只是网络抖动；连续达到阈值才判定该地址不可达并移除，
///     避免设备条目被反复增删。
pub const FAILURE_THRESHOLD: u32 = 3;

/// 活跃地址粘滞阈值：`rtt_active <= best_rtt * SWITCH_RATIO` 时保持现活跃地址（防抖）。
///
/// Business Logic: 新路径 RTT 必须显著优于现路径才值得切换；LAN ~0.5ms vs Tailscale
///     ~5–200ms 差距远超该阈值，切换是即时的，而量级相近的路径之间不来回抖动。
pub const SWITCH_RATIO: f64 = 0.6;

/// RTT EMA 历史权重：`new = RTT_EMA_OLD_WEIGHT * old + (1 - RTT_EMA_OLD_WEIGHT) * measured`。
pub const RTT_EMA_OLD_WEIGHT: f64 = 0.7;

/// 设备的一个候选网络地址行（mDNS / manual_peers / Tailscale 多源上报按地址归并）。
///
/// Business Logic: 同一逻辑设备（如家内 LAN IP + Tailscale IP）有多条可达路径，
///     每条路径独立跟踪 RTT 与连续失败，供 `select_active` 自动择优。
#[derive(Debug, Clone)]
pub struct DeviceAddress {
    /// IP / 主机名（点分十进制优先）
    pub host: String,
    /// HTTP 端口（对端 health 报告的实际监听端口）
    pub port: u16,
    /// 最近健康探测 RTT（EMA 平滑，毫秒）；None = 尚未测量（如 mDNS 事件刚发现）。
    pub rtt_ms: Option<f64>,
    /// 连续失败次数（达到 `FAILURE_THRESHOLD` 移除该行）。
    pub fail_count: u32,
    /// 最近一次健康上报时间（UTC）。
    #[cfg_attr(not(test), allow(dead_code))]
    pub last_healthy: DateTime<Utc>,
}

impl DeviceAddress {
    /// 构造一条未测量（rtt=None、fail_count=0）的新地址行。
    ///
    /// Business Logic: mDNS 事件 / 首次 health 成功登记新地址时，行从「未测量、无失败」
    ///     的初始状态起步，RTT 由后续探测补测。
    ///
    /// Code Logic: 以当前 UTC 时间填充 `last_healthy`，其余字段取初始值。
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_string(),
            port,
            rtt_ms: None,
            fail_count: 0,
            last_healthy: Utc::now(),
        }
    }
}

/// 一次健康上报携带的设备元数据（来源：mDNS TXT 提示或对端 `/api/health` 响应）。
///
/// Business Logic: 地址行由多个发现源共享上报；name/proto/caps 属于设备级元数据，
///     与具体地址行解耦，聚合为一个参数避免 report_health 参数列表膨胀。
#[derive(Debug, Clone, Copy)]
pub struct DeviceHealthMeta<'a> {
    /// 设备显示名
    pub name: &'a str,
    /// 协议版本提示（非权威）
    pub proto_version: u32,
    /// 能力清单提示（非权威）
    pub capabilities: &'a [String],
}

/// 设备实体（内部使用，对照 Python `models/device.py` 的 Device dataclass）。
///
/// Business Logic: mDNS 发现 / overlay 探测的每个对端实例用一个 Device 表示，存入
///     AppState 的 devices 表。host 用 String 保存 IP（与 Python 一致，统一 IPv4 点分十进制）。
///     `host`/`port` 是**当前活跃地址**（由 `select_active()` 在多候选中择优写回），
///     消费方（`base_url()`、DTO、relay 解析）只读这两个字段即自动用上最优路径。
///     proto_version / capabilities 为发现层提示的非权威拷贝，仅用于发现层预筛；
///     调用对端新路由前必须以对端 health 返回的 `PeerProtocolInfo` 为准（见 net::protocol）。
#[derive(Debug, Clone)]
pub struct Device {
    /// 设备唯一标识（UUID，来自对端 TXT 记录的 device_id）
    pub id: String,
    /// 设备显示名（来自 TXT 记录的 device_name）
    pub name: String,
    /// 当前活跃地址 host（由 `select_active()` 从 `addresses` 择优写回）
    pub host: String,
    /// 当前活跃地址端口
    pub port: u16,
    /// 最后发现时间（UTC）
    pub last_seen: DateTime<Utc>,
    /// 是否在线（存在健康候选地址行即 true；全部行失败/移除即 false）
    pub online: bool,
    /// 对端协议版本提示（来自 mDNS `proto` TXT 或 health；缺失/非法视为 v0）。非权威。
    #[cfg_attr(not(test), allow(dead_code))]
    pub proto_version: u32,
    /// 对端能力清单提示（来自 mDNS `caps` TXT 或 health；空 token 已剔除）。非权威。
    #[cfg_attr(not(test), allow(dead_code))]
    pub capabilities: Vec<String>,
    /// 全部候选地址行（每地址一行；多源发现按地址归并，不互相覆盖）
    pub addresses: Vec<DeviceAddress>,
}

impl Device {
    /// 构造对端访问的 base URL：`http://{host}:{port}`。
    ///
    /// Business Logic: peer_client 调对端 API 需要拼接 base URL，与 Python `Device.base_url()` 一致；
    ///     host/port 即当前活跃地址，多地址择优后消费方无感知。
    #[allow(dead_code)]
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    /// 构造带单条地址行的 Device（host/port 即活跃地址，online=true）。
    ///
    /// Business Logic: 首次发现一个对端（探测成功 / mDNS Resolved / 测试播种）时，
    ///     设备从「单地址、活跃、未测量」状态起步；后续多源上报经 `report_health` 合并成多行。
    ///
    /// Code Logic: 以 `DeviceAddress::new(host, port)` 初始化 `addresses` 单行，
    ///     `last_seen` 取当前 UTC 时间，元数据按入参填充。
    pub fn new(
        id: String,
        name: String,
        host: String,
        port: u16,
        proto_version: u32,
        capabilities: Vec<String>,
    ) -> Self {
        Self {
            id,
            name,
            host: host.clone(),
            port,
            last_seen: Utc::now(),
            online: true,
            proto_version,
            capabilities,
            addresses: vec![DeviceAddress::new(&host, port)],
        }
    }

    /// 登记一次健康上报（探测成功或 mDNS 发现）：upsert 地址行 + 刷新设备元数据 + 重新择优。
    ///
    /// Business Logic: 同一设备的多个发现源（探测循环 / mDNS）都经此合并；
    ///     每地址一行不互相覆盖，RTT 用 EMA 平滑（`0.7*old + 0.3*new`）抑制单次抖动，
    ///     `rtt_ms=None`（mDNS 事件无测量值）不冲掉已有测量。
    ///
    /// Code Logic: 按 host 定位地址行——命中则按 EMA 更新 rtt（None 保留旧值）、
    ///     重置 fail_count、刷新 port/last_healthy；未命中则追加新行。
    ///     设备级 name/proto/caps/last_seen 总是刷新，最后 `select_active()` 重选活跃地址。
    pub fn report_health(
        &mut self,
        host: &str,
        port: u16,
        rtt_ms: Option<f64>,
        meta: DeviceHealthMeta<'_>,
    ) {
        let now = Utc::now();
        self.name = meta.name.to_string();
        self.proto_version = meta.proto_version;
        self.capabilities = meta.capabilities.to_vec();
        self.last_seen = now;
        if let Some(row) = self.addresses.iter_mut().find(|a| a.host == host) {
            if let Some(rtt) = rtt_ms {
                row.rtt_ms = Some(match row.rtt_ms {
                    Some(old) => RTT_EMA_OLD_WEIGHT * old + (1.0 - RTT_EMA_OLD_WEIGHT) * rtt,
                    None => rtt,
                });
            }
            row.port = port;
            row.fail_count = 0;
            row.last_healthy = now;
        } else {
            self.addresses.push(DeviceAddress {
                host: host.to_string(),
                port,
                rtt_ms,
                fail_count: 0,
                last_healthy: now,
            });
        }
        self.select_active();
    }

    /// 登记一次探测失败：行级 fail_count+1，达 `FAILURE_THRESHOLD` 移除该行，重新择优。
    ///
    /// Business Logic: 探测失败按（host, port）归因到具体地址行——同一 host 配了多个
    ///     候选端口（如 manual_peers 里探一个未开 cc-partner 的 ghost 端口）时，
    ///     不得惩罚同 host 的健康行。全部行移除时由调用方删除 Device 条目。
    ///
    /// Code Logic: 定位 host+port 匹配的行，fail_count+1；达到阈值整行移除；
    ///     随后 `select_active()`（无健康候选则 online=false）。返回 true 表示
    ///     该设备已无任何地址行，调用方应从 devices 表删除条目。
    pub fn report_failure(&mut self, host: &str, port: u16) -> bool {
        if let Some(row) = self
            .addresses
            .iter_mut()
            .find(|a| a.host == host && a.port == port)
        {
            row.fail_count += 1;
            if row.fail_count >= FAILURE_THRESHOLD {
                self.addresses.retain(|a| a.host != host || a.port != port);
            }
        }
        self.select_active();
        self.addresses.is_empty()
    }

    /// 从候选地址行中重新选出活跃地址并写回 host/port（无健康候选则置 offline）。
    ///
    /// Business Logic: 消费方只读 host/port，选路结果必须落在原字段上才能零改动生效；
    ///     活跃行失败立即失格切换备选，全部地址失败才判定设备离线。
    ///
    /// Code Logic: 委托纯函数 `select_active_address` 得到应活跃的行——命中则把
    ///     该行 host/port 写回设备字段并置 online=true；无候选仅置 online=false
    ///     （保留最后活跃地址供展示）。
    pub fn select_active(&mut self) {
        match select_active_address(&self.addresses, &self.host) {
            Some(row) => {
                self.host = row.host.clone();
                self.port = row.port;
                self.online = true;
            }
            None => {
                self.online = false;
            }
        }
    }
}

/// 核心选路纯函数：从地址行集合中选出应作为活跃地址的行。
///
/// Business Logic: 每次请求自动使用延迟最低的健康地址——活跃行失败（fail_count>0）
///     立即失格，其余按 RTT 择优；写成纯函数便于单测（锁逻辑留在调用方）。
///
/// Code Logic:
///     1. 候选 = `fail_count == 0` 的行；
///     2. 活跃行若仍在候选中且满足粘滞（`rtt_active <= best_rtt * SWITCH_RATIO`）则保持；
///     3. 否则取 rtt 最小的候选（None 视为 +∞，即测过的优先于未测的；并列取先插入行）；
///     4. 无候选返回 None（调用方据此判定 offline）。
pub fn select_active_address<'a>(
    addresses: &'a [DeviceAddress],
    active_host: &str,
) -> Option<&'a DeviceAddress> {
    let candidates: Vec<&DeviceAddress> = addresses.iter().filter(|a| a.fail_count == 0).collect();
    if candidates.is_empty() {
        return None;
    }
    let best_rtt = candidates
        .iter()
        .filter_map(|a| a.rtt_ms)
        .fold(f64::INFINITY, f64::min);
    // 活跃行粘滞：仍在候选且不劣于阈值时保持（spec 公式原样；best 含活跃行自身，
    // 故该分支实际只在 rtt 相同/为 0 的边界成立，常态等价于直接取最小 rtt）。
    if let Some(active) = candidates.iter().find(|a| a.host == active_host) {
        if let Some(rtt) = active.rtt_ms {
            if rtt <= best_rtt * SWITCH_RATIO {
                return Some(active);
            }
        }
    }
    // None 视为 +∞；total_cmp 抗 NaN，min_by 并列返回先出现者（插入序稳定）。
    candidates.into_iter().min_by(|a, b| {
        a.rtt_ms
            .unwrap_or(f64::INFINITY)
            .total_cmp(&b.rtt_ms.unwrap_or(f64::INFINITY))
    })
}

/// 在 devices 表登记一次健康上报：设备不存在则创建（单地址行），存在则 `report_health` 合并。
///
/// Business Logic: 探测循环与 mDNS 事件共享同一张 devices 表；多源上报同一 device_id
///     必须归并为一个 Device 的多行，而不是整体覆盖抖动。
///
/// Code Logic: `entry` 占位（缺失时按本次上报构造 `Device::new`），再统一走
///     `report_health` 完成行级 upsert、EMA 与活跃地址择优。
pub fn upsert_device_health(
    devices: &mut HashMap<String, Device>,
    device_id: &str,
    host: &str,
    port: u16,
    rtt_ms: Option<f64>,
    meta: DeviceHealthMeta<'_>,
) {
    let device = devices.entry(device_id.to_string()).or_insert_with(|| {
        Device::new(
            device_id.to_string(),
            meta.name.to_string(),
            host.to_string(),
            port,
            meta.proto_version,
            meta.capabilities.to_vec(),
        )
    });
    device.report_health(host, port, rtt_ms, meta);
}

/// 按（host, port）向 devices 表上报一次探测失败：达阈值移除行，全部行移除才删 Device 条目。
///
/// Business Logic: 行级失败隔离——同一 host 的其它健康地址行（或其它设备）不受牵连；
///     只有当某设备的全部地址行都被移除时才判定其条目可删。
///
/// Code Logic: 找到第一个持有匹配地址行的设备并 `report_failure`；该设备行全空时
///     从表中删除条目并返回其 device_id（供日志），否则返回 None。
pub fn report_device_failure(
    devices: &mut HashMap<String, Device>,
    host: &str,
    port: u16,
) -> Option<String> {
    let drained = devices
        .iter_mut()
        .find(|(_, device)| {
            device
                .addresses
                .iter()
                .any(|a| a.host == host && a.port == port)
        })
        .and_then(|(id, device)| device.report_failure(host, port).then(|| id.clone()));
    if let Some(id) = &drained {
        devices.remove(id);
    }
    drained
}

/// 设备前端 DTO（camelCase，对照前端 types.ts 与旧 Python `/api/devices` 返回结构）。
///
/// Business Logic: 前端 TS 用 camelCase；旧 Python `/api/devices` 返回字段名为 `address`
///     （对应内部 host），此处保持一致避免前端改动。`isSelf` 标记是否本机（前端展示用）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceDto {
    pub id: String,
    pub name: String,
    /// IP 地址（前端字段名为 address，对应内部 host）
    pub address: String,
    pub port: u16,
    /// 最后发现时间 ISO 字符串
    pub last_seen: String,
    pub online: bool,
    /// 是否本机设备（list_devices 时对对端为 false，get_local_device 为 true）
    #[serde(default)]
    pub is_self: bool,
    /// 协议版本提示（mDNS 非权威；缺失/旧版为 0）。前端可用于灰显未支持能力的对端。
    #[serde(default)]
    pub proto_version: u32,
    /// 能力清单提示（mDNS 非权威；可能因 TXT 长度上限被裁剪，仅作预筛展示）。
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// 影子设备（经跳板中转可见）的中转来源 device_id；直连设备为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_device_id: Option<String>,
    /// 影子设备的中转来源设备名（仅展示用）；直连设备为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_device_name: Option<String>,
}

impl Device {
    /// 转换为前端 DTO（host → address，datetime → ISO 字符串）。
    ///
    /// Business Logic: 命令层返回前端前做字段名与格式转换；address/port 即当前活跃地址。
    pub fn to_dto(&self, is_self: bool) -> DeviceDto {
        DeviceDto {
            id: self.id.clone(),
            name: self.name.clone(),
            address: self.host.clone(),
            port: self.port,
            last_seen: self.last_seen.to_rfc3339(),
            online: self.online,
            is_self,
            proto_version: self.proto_version,
            capabilities: self.capabilities.clone(),
            via_device_id: None,
            via_device_name: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一条可控地址行。
    fn row(host: &str, port: u16, rtt: Option<f64>, fail: u32) -> DeviceAddress {
        DeviceAddress {
            host: host.to_string(),
            port,
            rtt_ms: rtt,
            fail_count: fail,
            last_healthy: Utc::now(),
        }
    }

    /// 构造一台带 LAN+Tailscale 双行、活跃地址为 `active` 的测试设备。
    fn dual_device(active: &str) -> Device {
        let lan = "192.168.6.17";
        let ts = "100.113.214.69";
        let first = if active == lan { lan } else { ts };
        let second = if first == lan { ts } else { lan };
        let mut device = Device::new(
            "peer-1".into(),
            "r9000p".into(),
            first.to_string(),
            62116,
            1,
            Vec::new(),
        );
        device.addresses.push(DeviceAddress::new(second, 62116));
        device
    }

    /// Business Logic（为什么需要这个测试）:
    ///     多地址择优是本特性的核心：RTT 更低的健康地址必须成为活跃地址，
    ///     消费方经 host/port 自动走最优路径。
    ///
    /// Code Logic（这个测试做什么）:
    ///     LAN(0.5ms)+Tailscale(5ms) 双行，分别以两个方向为初始活跃，
    ///     断言 select_active 后活跃地址均为 LAN。
    #[test]
    fn select_active_prefers_lowest_rtt_candidate() {
        let mut device = dual_device("100.113.214.69");
        device.addresses[0].rtt_ms = Some(5.0);
        device.addresses[1].rtt_ms = Some(0.5);
        device.select_active();
        assert_eq!(device.host, "192.168.6.17");
        assert!(device.online);

        // 反向初始活跃：从 LAN 出发也应收敛到（保持）LAN。
        let mut device = dual_device("192.168.6.17");
        device.addresses[0].rtt_ms = Some(0.5);
        device.addresses[1].rtt_ms = Some(5.0);
        device.select_active();
        assert_eq!(device.host, "192.168.6.17");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     粘滞规则防路径抖动：活跃行仍是健康候选且满足 spec 粘滞公式时应保持，
    ///     不得因择优比较在相近路径间来回切换。
    ///
    /// Code Logic（这个测试做什么）:
    ///     活跃 LAN rtt=0.0、Tailscale rtt=10：0 <= 10*0.6 成立，断言走粘滞分支保持 LAN；
    ///     对照组 LAN rtt=0.5、Tailscale=5（0.5 > 5*0.6 不成立）同样收敛到最小 rtt 的 LAN，
    ///     并断言 SWITCH_RATIO 常量与 spec 一致。
    #[test]
    fn select_active_keeps_active_within_switch_ratio() {
        assert_eq!(SWITCH_RATIO, 0.6);
        let mut device = dual_device("192.168.6.17");
        device.addresses[0].rtt_ms = Some(0.0);
        device.addresses[1].rtt_ms = Some(10.0);
        device.select_active();
        assert_eq!(device.host, "192.168.6.17", "粘滞分支应保持现活跃地址");

        // 对照：不满足粘滞公式时按最小 rtt 收敛（仍应选中更快的 LAN）。
        let mut device = dual_device("192.168.6.17");
        device.addresses[0].rtt_ms = Some(0.5);
        device.addresses[1].rtt_ms = Some(5.0);
        device.select_active();
        assert_eq!(device.host, "192.168.6.17");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     探测失败必须立即切换备选：活跃行 fail_count>0 后不再入候选，
    ///     流量自动切到其余健康地址。
    ///
    /// Code Logic（这个测试做什么）:
    ///     LAN 行 fail_count=1（未达移除阈值）、Tailscale 健康：断言活跃切到 Tailscale
    ///     且 LAN 行仍保留（等它恢复）。
    #[test]
    fn select_active_disqualifies_failed_active_row() {
        let mut device = dual_device("192.168.6.17");
        device.addresses[0].rtt_ms = Some(0.5);
        device.addresses[1].rtt_ms = Some(5.0);
        device.addresses[0].fail_count = 1;
        device.select_active();
        assert_eq!(device.host, "100.113.214.69");
        assert_eq!(device.addresses.len(), 2, "未达阈值的失败行不移除");
        assert!(device.online);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     全部地址连续失败才判定设备离线；离线时保留最后活跃地址供 UI 展示。
    ///
    /// Code Logic（这个测试做什么）:
    ///     双行全部 fail_count>0：断言 select_active 后 online=false 且 host/port
    ///     保留最后活跃值。
    #[test]
    fn select_active_marks_offline_when_no_healthy_candidate() {
        let mut device = dual_device("192.168.6.17");
        device.addresses[0].fail_count = 1;
        device.addresses[1].fail_count = 2;
        device.select_active();
        assert!(!device.online);
        assert_eq!(device.host, "192.168.6.17", "离线时保留最后活跃地址");
        assert_eq!(device.port, 62116);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     未测量行（mDNS 刚发现）排在已测量行之后；全未测量时按插入序稳定取首行，
    ///     保证选路结果确定、不抖动。
    ///
    /// Code Logic（这个测试做什么）:
    ///     两个 None 行 + 一个 rtt=3 行 → 选中已测量行；两个 None 行 → 选中先插入行。
    #[test]
    fn select_active_orders_unmeasured_rows_after_measured() {
        let addresses = vec![
            row("10.0.0.1", 62116, None, 0),
            row("10.0.0.2", 62116, None, 0),
            row("10.0.0.3", 62116, Some(3.0), 0),
        ];
        let picked = select_active_address(&addresses, "10.0.0.1").unwrap();
        assert_eq!(picked.host, "10.0.0.3", "已测量行优先于未测量行");

        let addresses = vec![
            row("10.0.0.1", 62116, None, 0),
            row("10.0.0.2", 62116, None, 0),
        ];
        let picked = select_active_address(&addresses, "10.0.0.2").unwrap();
        assert_eq!(picked.host, "10.0.0.1", "全未测量时按插入序取首行");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     RTT EMA（0.7*old + 0.3*new）抑制单次探测抖动；健康上报还应重置失败计数
    ///     并保持活跃地址为该行。
    ///
    /// Code Logic（这个测试做什么）:
    ///     依次上报 rtt=100、rtt=20：断言行 rtt 变为 76（0.7*100+0.3*20），
    ///     fail_count 复位，host/port 为该地址。
    #[test]
    fn report_health_smooths_rtt_with_ema_and_resets_failure() {
        let mut device = Device::new(
            "peer-1".into(),
            "r9000p".into(),
            "192.168.6.17".into(),
            62116,
            1,
            Vec::new(),
        );
        device.addresses[0].fail_count = 2;
        let meta = DeviceHealthMeta {
            name: "r9000p",
            proto_version: 1,
            capabilities: &[],
        };
        device.report_health("192.168.6.17", 62116, Some(100.0), meta);
        device.report_health("192.168.6.17", 62116, Some(20.0), meta);
        let rtt = device.addresses[0].rtt_ms.unwrap();
        assert!(
            (rtt - 76.0).abs() < 1e-9,
            "EMA 应为 0.7*100+0.3*20=76，实际 {rtt}"
        );
        assert_eq!(device.addresses[0].fail_count, 0, "健康上报复位失败计数");
        assert_eq!(device.host, "192.168.6.17");
        assert!(device.online);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     失败阈值必须按（host, port）行级归因：同 host 的其它健康行不受牵连，
    ///     全部行移除才通知调用方删条目。
    ///
    /// Code Logic（这个测试做什么）:
    ///     双行设备对 (LAN, 62116) 连续失败 2 次 → 行保留；第 3 次 → LAN 行移除且活跃切 Tailscale；
    ///     再对 (LAN, ghost 端口) 失败 3 次 → 不影响 Tailscale 行；对 Tailscale 行失败 3 次 →
    ///     report_failure 返回 true（行全空）。
    #[test]
    fn report_failure_removes_row_at_threshold_and_signals_drained_device() {
        let mut device = dual_device("192.168.6.17");
        device.addresses[0].rtt_ms = Some(0.5);
        device.addresses[1].rtt_ms = Some(5.0);
        let lan = ("192.168.6.17", 62116);
        let ts = ("100.113.214.69", 62116);

        assert!(!device.report_failure(lan.0, lan.1));
        assert!(!device.report_failure(lan.0, lan.1));
        assert_eq!(device.addresses.len(), 2, "未达阈值不移除");
        assert!(
            !device.report_failure(lan.0, lan.1),
            "还有 Tailscale 行，不删条目"
        );
        assert_eq!(device.addresses.len(), 1);
        assert_eq!(device.host, ts.0, "活跃行失败后切到剩余健康行");

        // ghost 端口失败不惩罚同 host 已移除/其余行（此处 LAN 行已移除，纯 no-op）。
        assert!(!device.report_failure(lan.0, 9999));
        assert_eq!(device.addresses.len(), 1);

        // 对仅剩的 Tailscale 行连续失败：前两次不移除，第三次行全空并返回 true。
        assert!(!device.report_failure(ts.0, ts.1));
        assert!(!device.report_failure(ts.0, ts.1));
        assert!(
            device.report_failure(ts.0, ts.1),
            "最后一行移除应通知调用方删条目"
        );
        assert!(device.addresses.is_empty());
        assert!(!device.online);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     多源上报按 device_id 归并：map 层 upsert 必须合并为一个 Device 的多行，
    ///     且探测失败经 map 层归因后行全空才删除条目。
    ///
    /// Code Logic（这个测试做什么）:
    ///     `upsert_device_health` 两次不同 host 同 device_id → 一个 Device 两行、
    ///     活跃为低 RTT 行；`report_device_failure` 对低 RTT 行达阈值 → 行移除、条目保留；
    ///     对剩余行达阈值 → 条目被删除并返回 device_id。
    #[test]
    fn map_level_upsert_merges_rows_and_failure_drains_entry() {
        let mut devices = HashMap::new();
        let meta = DeviceHealthMeta {
            name: "r9000p",
            proto_version: 1,
            capabilities: &[],
        };
        upsert_device_health(
            &mut devices,
            "peer-1",
            "192.168.6.17",
            62116,
            Some(0.5),
            meta,
        );
        upsert_device_health(
            &mut devices,
            "peer-1",
            "100.113.214.69",
            62116,
            Some(5.0),
            meta,
        );
        assert_eq!(devices.len(), 1, "同 device_id 必须归并为一个 Device");
        let device = devices.get("peer-1").unwrap();
        assert_eq!(device.addresses.len(), 2);
        assert_eq!(device.host, "192.168.6.17", "活跃地址应为低 RTT 行");
        assert!(device.online);

        for _ in 0..FAILURE_THRESHOLD {
            report_device_failure(&mut devices, "192.168.6.17", 62116);
        }
        assert_eq!(devices.len(), 1, "仍有健康行时条目保留");
        let device = devices.get("peer-1").unwrap();
        assert_eq!(device.host, "100.113.214.69");

        for _ in 0..FAILURE_THRESHOLD {
            let drained = report_device_failure(&mut devices, "100.113.214.69", 62116);
            if drained.is_some() {
                break;
            }
        }
        assert!(devices.is_empty(), "全部行移除后条目应被删除");
    }
}
