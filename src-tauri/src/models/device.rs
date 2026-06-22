//! models/device.rs — 设备数据模型
//!
//! Business Logic（为什么需要这个模块）:
//!     P2P 局域网协作需要跟踪每个对端设备的连接信息（IP、端口）和在线状态，
//!     以便进行文件传输（M5）和 Prompt 同步（M4）。对照 Python `models/device.py`。
//!
//! Code Logic（这个模块做什么）:
//!     - `Device`：内部使用的设备实体，字段对照 Python Device dataclass
//!       （id/name/host/port/last_seen/online）。mDNS 发现事件写入此结构。
//!     - `DeviceDto`：返回前端的 DTO（camelCase），对照前端 `web/src/lib/types.ts`。
//!       字段 address 对应内部 host（前端命名沿用旧 Python `/api/devices` 的 `address`）。

use chrono::{DateTime, Utc};

/// 设备实体（内部使用，对照 Python `models/device.py` 的 Device dataclass）。
///
/// Business Logic: mDNS 发现的每个对端实例用一个 Device 表示，存入 AppState 的 devices 表。
///     host 用 String 保存 IP（与 Python 一致，统一 IPv4 点分十进制）。
#[derive(Debug, Clone)]
pub struct Device {
    /// 设备唯一标识（UUID，来自对端 TXT 记录的 device_id）
    pub id: String,
    /// 设备显示名（来自 TXT 记录的 device_name）
    pub name: String,
    /// IP 地址（点分十进制）
    pub host: String,
    /// HTTP 端口（来自 mDNS SRV record 的 port）
    pub port: u16,
    /// 最后发现时间（UTC）
    pub last_seen: DateTime<Utc>,
    /// 是否在线（发现即 true，移除即从表剔除）
    pub online: bool,
}

impl Device {
    /// 构造对端访问的 base URL：`http://{host}:{port}`。
    ///
    /// Business Logic: peer_client 调对端 API 需要拼接 base URL，与 Python `Device.base_url()` 一致。
    #[allow(dead_code)]
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
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
}

impl Device {
    /// 转换为前端 DTO（host → address，datetime → ISO 字符串）。
    ///
    /// Business Logic: 命令层返回前端前做字段名与格式转换。
    pub fn to_dto(&self, is_self: bool) -> DeviceDto {
        DeviceDto {
            id: self.id.clone(),
            name: self.name.clone(),
            address: self.host.clone(),
            port: self.port,
            last_seen: self.last_seen.to_rfc3339(),
            online: self.online,
            is_self,
        }
    }
}
