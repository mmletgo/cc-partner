//! 影子设备（经跳板可见的远端目标）状态与纯规则。
//!
//! Business Logic（为什么需要这个模块）:
//!     设备 A 无法直连设备 C 时，A 从跳板 B 的 `/api/relay/peers` 报告里"看到"C。
//!     这类目标的可达性依赖 B（B 可达 && B 报告 C online），与 mDNS 直连表性质不同，
//!     必须独立成表，且直连表命中时永远优先（解析在 `device_base_url` 收敛）。
//!
//! Code Logic（这个模块做什么）:
//!     `RelayShadowDevice` 影子条目；表挂在 `RelayRuntime`（避免新增 AppState 字段
//!     波及全部装配点）；写入只来自影子探测任务（按 via 整批替换、via 失效整批下线）；
//!     纯函数负责"目标是否值得成为影子"的排除规则（非本机、不与直连表重复）。

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::models::device::Device;
use crate::state::AppState;

/// 影子设备条目：A 视角下"经 via 跳板可见的 target"。
///
/// Business Logic（为什么需要这个结构）:
///     设备列表需要把经跳板可见的目标与直连设备并列展示（带「经 X 中转」标记与
///     在线状态），路由解析需要 target → via 的映射来构造 `http://{via}/api/relay/{target}`；
///     两者共用同一份影子条目，避免列表与解析各算一套。
///
/// Code Logic（这个结构做什么）:
///     `target_device_id` 是表主键（同一 target 当前只经一个 via 呈现）；`online`
///     语义 = via 自身直连可达 && via 报告 target online；`last_seen` 仅供诊断。
#[derive(Debug, Clone)]
pub struct RelayShadowDevice {
    /// 被中转访问的目标设备 device_id（表主键）。
    pub target_device_id: String,
    /// 提供中转的跳板设备 device_id（必须 ∈ config.relay.via_device_ids）。
    pub via_device_id: String,
    /// 目标设备名（跳板报告，仅展示）。
    pub device_name: String,
    /// 目标协议版本提示（跳板转述，非权威；权威值以经跳板 health 为准）。
    pub proto_version: u32,
    /// 目标能力提示（跳板转述，非权威）。
    pub capabilities: Vec<String>,
    /// via 直连可达 && via 报告 target online。
    pub online: bool,
    /// 最近一次跳板报告确认时间。
    pub last_seen: DateTime<Utc>,
}

/// 影子设备表：target_device_id → 条目。
pub type RelayShadowTable = HashMap<String, RelayShadowDevice>;

/// 用跳板 `GET /api/relay/peers` 的一轮报告整批替换该 via 名下的影子条目。
///
/// Business Logic（为什么需要这个函数）:
///     影子列表必须跟随跳板的最新视图（目标下线后 A 侧最迟一个探测周期内置灰），
///     "按 via 整批替换"天然处理新增/消失/改名，不需要逐条 diff。
///
/// Code Logic（这个函数做什么）:
///     先移除表中所有 `via_device_id` 的旧条目，再把报告里通过 `is_shadow_eligible`
///     排除规则的目标以 online=true、last_seen=now 写入。锁内无 await。
pub fn replace_shadows_for_via(state: &AppState, via_device_id: &str, reported: Vec<Device>) {
    let now = Utc::now();
    let mut shadows = state.relay.shadow_devices.write().expect("影子表写锁中毒");
    shadows.retain(|_, shadow| shadow.via_device_id != via_device_id);
    for device in reported {
        if !is_shadow_eligible(
            &device.id,
            state.device_id.as_str(),
            &shadows,
            &state.devices.read().expect("devices 读锁中毒"),
        ) {
            continue;
        }
        shadows.insert(
            device.id.clone(),
            RelayShadowDevice {
                target_device_id: device.id,
                via_device_id: via_device_id.to_string(),
                device_name: device.name,
                proto_version: device.proto_version,
                capabilities: device.capabilities,
                online: true,
                last_seen: now,
            },
        );
    }
}

/// 跳板失效（直连表消失/探测失败超阈）时把其名下影子全部置 offline。
///
/// Business Logic（为什么需要这个函数）:
///     影子 online 是复合语义（via 可达 && via 报告 online）；via 自身从 A 的直连表
///     消失时，无论 via 报告什么，经它的链路都不可用，必须立即整体下线。
///
/// Code Logic（这个函数做什么）:
///     遍历影子表，`via_device_id` 匹配的条目 `online=false`。锁内无 await。
pub fn mark_via_offline(state: &AppState, via_device_id: &str) {
    let mut shadows = state.relay.shadow_devices.write().expect("影子表写锁中毒");
    for shadow in shadows.values_mut() {
        if shadow.via_device_id == via_device_id {
            shadow.online = false;
        }
    }
}

/// 跳板被用户从 via 列表移除时清理其名下全部影子条目。
///
/// Business Logic（为什么需要这个函数）:
///     用户撤销对跳板的信任后，经它可见的目标不应继续出现在设备列表或参与解析。
///
/// Code Logic（这个函数做什么）:
///     移除表中所有 `via_device_id` 匹配的条目。锁内无 await。
pub fn remove_via(state: &AppState, via_device_id: &str) {
    let mut shadows = state.relay.shadow_devices.write().expect("影子表写锁中毒");
    shadows.retain(|_, shadow| shadow.via_device_id != via_device_id);
}

/// 目标是否值得成为影子条目（排除规则，纯函数）。
///
/// Business Logic（为什么需要这个函数）:
///     直连永远优先、本机不经中转访问自己、同一 target 不得经多个跳板重复出现；
///     探测写入与设备列表合并共用同一规则，避免两处口径漂移。
///
/// Code Logic（这个函数做什么）:
///     拒绝：target == 本机；target 已在直连表（键集合比对）；target 已是其它 via
///     名下的影子（先到先得）。`shadows` 参数为当前表（调用方已持有读/写锁），
///     `direct_devices` 为直连表快照。
pub fn is_shadow_eligible(
    target_device_id: &str,
    self_device_id: &str,
    shadows: &RelayShadowTable,
    direct_devices: &HashMap<String, Device>,
) -> bool {
    if target_device_id == self_device_id {
        return false;
    }
    if direct_devices.contains_key(target_device_id) {
        return false;
    }
    // 同一 target 只呈现一条影子（先到先得；replace_shadows_for_via 已先清空同 via 旧条目，
    // 此处命中的必属其它 via）。
    !shadows.contains_key(target_device_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::relay_shadow_probe::test_support::build_test_state;

    /// 固定测试身份：A（本机）/ 跳板 B1、B2 / 目标 C、D。
    const SELF_ID: &str = "shadow-rule-self";
    const VIA_B1: &str = "relay-rule-B1";
    const VIA_B2: &str = "relay-rule-B2";
    const TARGET_C: &str = "target-rule-C";
    const TARGET_D: &str = "target-rule-D";

    /// 构造一条直连表 Device。
    fn device(id: &str, host: &str, port: u16, online: bool) -> Device {
        Device {
            id: id.to_string(),
            name: format!("device-{id}"),
            host: host.to_string(),
            port,
            last_seen: Utc::now(),
            online,
            proto_version: 1,
            capabilities: Vec::new(),
        }
    }

    /// 构造一份跳板报告（全部视作在线目标）。
    fn report_of(ids: &[&str]) -> Vec<Device> {
        ids.iter()
            .map(|id| device(id, "10.0.0.1", 62116, true))
            .collect()
    }

    /// Business Logic（为什么需要这个测试）:
    ///     排除规则是影子表口径的唯一防线：本机不得经中转访问自己、直连已有目标
    ///     不得被影子重复呈现（直连永远优先）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     `is_shadow_eligible` 纯函数断言：本机拒绝、直连表已有目标拒绝、
    ///     陌生目标放行；`replace_shadows_for_via` 端到端复验同样的过滤结果。
    #[test]
    fn shadow_eligibility_excludes_self_and_direct_duplicates() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let state = rt.block_on(build_test_state(SELF_ID, vec![VIA_B1.to_string()]));
        device_insert(&state, device(TARGET_D, "10.0.0.9", 62116, true));

        let shadows = state.relay.shadow_devices.read().unwrap();
        let devices = state.devices.read().unwrap();
        assert!(!is_shadow_eligible(SELF_ID, SELF_ID, &shadows, &devices));
        assert!(!is_shadow_eligible(TARGET_D, SELF_ID, &shadows, &devices));
        assert!(is_shadow_eligible(TARGET_C, SELF_ID, &shadows, &devices));
        drop(shadows);
        drop(devices);

        replace_shadows_for_via(&state, VIA_B1, report_of(&[SELF_ID, TARGET_D, TARGET_C]));
        let shadows = state.relay.shadow_devices.read().unwrap();
        assert_eq!(shadows.len(), 1, "仅陌生目标 C 入表: {shadows:?}");
        assert!(shadows.contains_key(TARGET_C));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     同一 target 只能呈现一条影子（先到先得）：后到的跳板报告不得抢走已有
    ///     影子的归属，避免列表与解析的 via 随探测顺序抖动。
    ///
    /// Code Logic（这个测试做什么）:
    ///     B1 先写入 C；B2 随后报告 [C, D]；断言 C 仍属 B1、D 归 B2。
    #[test]
    fn replace_shadows_keeps_first_wins_across_vias() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let state = rt.block_on(build_test_state(
            SELF_ID,
            vec![VIA_B1.to_string(), VIA_B2.to_string()],
        ));

        replace_shadows_for_via(&state, VIA_B1, report_of(&[TARGET_C]));
        replace_shadows_for_via(&state, VIA_B2, report_of(&[TARGET_C, TARGET_D]));

        let shadows = state.relay.shadow_devices.read().unwrap();
        assert_eq!(shadows.len(), 2, "C 保持 B1、D 新增 B2: {shadows:?}");
        assert_eq!(shadows.get(TARGET_C).unwrap().via_device_id, VIA_B1);
        assert_eq!(shadows.get(TARGET_D).unwrap().via_device_id, VIA_B2);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     跳板自身不可达时其名下影子必须整批下线（复合 online 语义），但不得误伤
    ///     其它跳板名下的影子。
    ///
    /// Code Logic（这个测试做什么）:
    ///     B1、B2 各写一条 online 影子；`mark_via_offline(B1)` 后断言仅 B1 的下线。
    #[test]
    fn mark_via_offline_only_affects_matching_via() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let state = rt.block_on(build_test_state(
            SELF_ID,
            vec![VIA_B1.to_string(), VIA_B2.to_string()],
        ));
        replace_shadows_for_via(&state, VIA_B1, report_of(&[TARGET_C]));
        replace_shadows_for_via(&state, VIA_B2, report_of(&[TARGET_D]));

        mark_via_offline(&state, VIA_B1);

        let shadows = state.relay.shadow_devices.read().unwrap();
        assert!(!shadows.get(TARGET_C).unwrap().online, "B1 名下应下线");
        assert!(shadows.get(TARGET_D).unwrap().online, "B2 名下不受影响");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户撤销对跳板的信任后其名下影子必须全部清理，其它跳板影子保留。
    ///
    /// Code Logic（这个测试做什么）:
    ///     B1、B2 各写一条影子；`remove_via(B1)` 后断言只剩 B2 的影子。
    #[test]
    fn remove_via_drops_only_matching_via_shadows() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let state = rt.block_on(build_test_state(
            SELF_ID,
            vec![VIA_B1.to_string(), VIA_B2.to_string()],
        ));
        replace_shadows_for_via(&state, VIA_B1, report_of(&[TARGET_C]));
        replace_shadows_for_via(&state, VIA_B2, report_of(&[TARGET_D]));

        remove_via(&state, VIA_B1);

        let shadows = state.relay.shadow_devices.read().unwrap();
        assert_eq!(shadows.len(), 1, "仅剩 B2 名下影子: {shadows:?}");
        assert!(shadows.contains_key(TARGET_D));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     整批替换要同时处理"新增/改名/消失"：同名目标属性被最新报告刷新，
    ///     消失目标被移除。
    ///
    /// Code Logic（这个测试做什么）:
    ///     首轮 B1 报告 C；第二轮报告 D（C 消失）；断言 C 被移除、D 入表。
    #[test]
    fn replace_shadows_batch_replaces_stale_entries() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let state = rt.block_on(build_test_state(SELF_ID, vec![VIA_B1.to_string()]));
        replace_shadows_for_via(&state, VIA_B1, report_of(&[TARGET_C]));

        replace_shadows_for_via(&state, VIA_B1, report_of(&[TARGET_D]));

        let shadows = state.relay.shadow_devices.read().unwrap();
        assert_eq!(shadows.len(), 1, "C 消失后应被移除: {shadows:?}");
        assert!(shadows.contains_key(TARGET_D));
    }

    /// 把 Device 写入直连表（测试播种）。
    fn device_insert(state: &AppState, entry: Device) {
        state
            .devices
            .write()
            .unwrap()
            .insert(entry.id.clone(), entry);
    }
}
