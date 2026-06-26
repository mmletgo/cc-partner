//! workbench/remote_ids.rs — Workbench remote ID mapping

use sha2::{Digest, Sha256};

const REMOTE_PREFIX: &str = "remote";

/// 远端实体 ID 解析结果。
///
/// Business Logic（为什么需要这个结构）:
///     Workbench 远端项目、worktree 和 terminal session 需要在本机 UI 中复用同一套 ID 通道，
///     因此解析后必须明确知道归属设备和远端内部 ID。
///
/// Code Logic（这个结构做什么）:
///     保存 `remote:<device_id>:<inner_id>` 拆分出的设备 ID 与远端实体原始 ID。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntityId {
    pub device_id: String,
    pub inner_id: String,
}

/// 生成稳定的远端项目 ID。
///
/// Business Logic（为什么需要这个函数）:
///     同一个局域网设备上的同一路径应在本机 Workbench 中稳定映射为同一个项目 ID，
///     便于后续列表刷新、tab 关联和数据库记录复用。
///
/// Code Logic（这个函数做什么）:
///     使用 `device_id + NUL + path` 计算 SHA256，并拼成 `remote:<device_id>:<hash>`。
#[allow(dead_code)]
pub fn remote_project_id(device_id: &str, path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(device_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(path.as_bytes());
    let digest = hasher.finalize();
    format!("{REMOTE_PREFIX}:{device_id}:{:x}", digest)
}

/// 封装远端实体 ID。
///
/// Business Logic（为什么需要这个函数）:
///     本机 UI 需要把远端 worktree/session 等实体 ID 与本地 ID 放在同一字段里传递，
///     所以前缀封装必须集中管理，避免各调用方手写格式。
///
/// Code Logic（这个函数做什么）:
///     把设备 ID 与远端内部 ID 拼成 `remote:<device_id>:<inner_id>`。
#[allow(dead_code)]
pub fn remote_entity_id(device_id: &str, inner_id: &str) -> String {
    format!("{REMOTE_PREFIX}:{device_id}:{inner_id}")
}

/// 解析远端实体 ID。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench gateway 后续需要判断一个项目、worktree 或 session 是否应转发到远端设备，
///     并取得远端真实实体 ID。
///
/// Code Logic（这个函数做什么）:
///     仅接受 `remote:<device_id>:<inner_id>`；本地 ID 或缺失任一字段时返回 `None`。
pub fn parse_remote_entity_id(value: &str) -> Option<RemoteEntityId> {
    let mut parts = value.splitn(3, ':');
    let prefix = parts.next()?;
    if prefix != REMOTE_PREFIX {
        return None;
    }
    let device_id = parts.next()?.to_string();
    let inner_id = parts.next()?.to_string();
    if device_id.is_empty() || inner_id.is_empty() {
        return None;
    }
    Some(RemoteEntityId {
        device_id,
        inner_id,
    })
}

/// 判断 ID 是否是远端实体 ID。
///
/// Business Logic（为什么需要这个函数）:
///     调用方经常只需要分流本地/远端 ID，不关心解析后的字段。
///
/// Code Logic（这个函数做什么）:
///     复用 `parse_remote_entity_id` 的格式校验，返回布尔结果。
#[allow(dead_code)]
pub fn is_remote_id(value: &str) -> bool {
    parse_remote_entity_id(value).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     同一台远端设备上的同一路径必须稳定映射为同一个 Workbench 项目 ID，
    ///     否则刷新项目列表或恢复 tab 时会产生重复项目。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对相同 device_id 和 path 连续生成两次远端项目 ID，断言结果一致且包含远端前缀。
    #[test]
    fn remote_project_id_is_stable_for_device_and_path() {
        let first = remote_project_id("device-a", "/Users/hans/web_project/app");
        let second = remote_project_id("device-a", "/Users/hans/web_project/app");

        assert_eq!(first, second);
        assert!(first.starts_with("remote:device-a:"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Workbench gateway 后续需要从统一 ID 中识别目标设备和远端内部实体，
    ///     才能把项目、worktree 或 session 操作转发到正确设备。
    ///
    /// Code Logic（这个测试做什么）:
    ///     解析 `remote:<device_id>:<inner_id>` 格式字符串，断言拆出的 device_id 和 inner_id 正确。
    #[test]
    fn parse_remote_id_returns_device_and_inner_id() {
        let parsed = parse_remote_entity_id("remote:device-a:session-1").unwrap();

        assert_eq!(parsed.device_id, "device-a");
        assert_eq!(parsed.inner_id, "session-1");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     本机项目和终端仍使用原始本地 ID，不能被误判为远端实体并走网络转发。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入无 `remote:` 前缀的本地 ID，断言解析结果为 None。
    #[test]
    fn parse_local_id_returns_none() {
        assert!(parse_remote_entity_id("local-session").is_none());
    }
}
