//! user_mirror/preview — 源 vs 目标用户级 inventory 的 replace-plus-delete diff
//!
//! Business Logic（为什么需要这个模块）:
//!     镜像 apply 前必须让用户看到将写入、替换、清空、删除或停用的条目；
//!     preview 只对比元数据，不写盘、不走 LAN。
//!
//! Code Logic（这个模块做什么）:
//!     `diff_inventories` 按 logical_id / (kind, native_id) 对号入座生成 plan；
//!     `preview_from_two_inventories` 补上 plan_token 与 15 分钟 TTL。

use super::models::{
    UserMirrorAgentInventoryDto, UserMirrorAgentPlanDto, UserMirrorChangeOp, UserMirrorDirection,
    UserMirrorFileChangeDto, UserMirrorInventoryDto, UserMirrorNativeFileFactDto,
    UserMirrorPlanDto, UserMirrorPortableChangeDto, UserMirrorPortableItemDto,
    USER_MIRROR_PLAN_TTL_MINUTES,
};
use crate::agent_hub::models::AgentTarget;
use crate::agent_hub::portable_inventory::PortableAssetKind;
use chrono::{Duration, Utc};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

/// 对比源/目标 inventory，生成无 token/TTL 的 replace-plus-delete plan。
///
/// Business Logic（为什么需要这个函数）:
///     预览必须是确定性纯函数：源有的覆盖，目标多出的 Skill/Command 删除、
///     Plugin 停用、MCP 删除；凭据条数来自源端 inventory，禁止在 diff 里回显 secret。
///
/// Code Logic（这个函数做什么）:
///     按 Agent 对号；portable 键 `(kind, native_id)`，native 键 `logical_id`；
///     `plan_token`/`expires_at` 留空，由 `preview_from_two_inventories` 填充。
pub fn diff_inventories(
    source: &UserMirrorInventoryDto,
    dest: &UserMirrorInventoryDto,
    source_device_id: &str,
    dest_device_id: &str,
    direction: UserMirrorDirection,
) -> UserMirrorPlanDto {
    let source_agents = index_agents(source);
    let dest_agents = index_agents(dest);
    let agents = all_targets(source, dest)
        .into_iter()
        .map(|target| {
            diff_agent(
                target,
                source_agents.get(&target).copied(),
                dest_agents.get(&target).copied(),
            )
        })
        .collect();
    let (remote_inventory_snapshot_hash, local_inventory_snapshot_hash) =
        snapshot_hashes(source, dest, direction);
    UserMirrorPlanDto {
        plan_token: String::new(),
        expires_at: String::new(),
        direction,
        source_device_id: source_device_id.to_string(),
        destination_device_id: dest_device_id.to_string(),
        remote_inventory_snapshot_hash,
        local_inventory_snapshot_hash,
        credential_bearing_count: source.credential_bearing_count,
        has_credential_bearing_assets: source.credential_bearing_count > 0,
        agents,
        blocking_reasons: Vec::new(),
        peer_device_ids: Vec::new(),
        // preview 不带选择；apply 时由 request.selection 合并进内存 plan。
        selection: None,
    }
}

/// 用两份已构建 inventory 生成带 token 与 TTL 的 preview plan（无 LAN）。
///
/// Business Logic（为什么需要这个函数）:
///     apply 必须绑定短期 plan；本 task 尚未写 SQLite / 拉远端，本地两份快照
///     仍要能产出可过期的 plan_token，供后续 ledger 落库。
///
/// Code Logic（这个函数做什么）:
///     调用 `diff_inventories` 后写入 UUID `plan_token` 与 now+15min 的 `expires_at`。
pub fn preview_from_two_inventories(
    source: &UserMirrorInventoryDto,
    dest: &UserMirrorInventoryDto,
    source_device_id: &str,
    dest_device_id: &str,
    direction: UserMirrorDirection,
) -> UserMirrorPlanDto {
    let mut plan = diff_inventories(source, dest, source_device_id, dest_device_id, direction);
    plan.plan_token = Uuid::now_v7().to_string();
    plan.expires_at = (Utc::now() + Duration::minutes(USER_MIRROR_PLAN_TTL_MINUTES)).to_rfc3339();
    plan
}

/// 按 AgentTarget 索引 inventory，重复 target 后者覆盖。
fn index_agents(
    inventory: &UserMirrorInventoryDto,
) -> BTreeMap<AgentTarget, &UserMirrorAgentInventoryDto> {
    inventory
        .agents
        .iter()
        .map(|agent| (agent.target, agent))
        .collect()
}

/// 源与目标出现过的 Agent 并集（catalog 序）。
fn all_targets(
    source: &UserMirrorInventoryDto,
    dest: &UserMirrorInventoryDto,
) -> BTreeSet<AgentTarget> {
    source
        .agents
        .iter()
        .chain(dest.agents.iter())
        .map(|agent| agent.target)
        .collect()
}

/// Pull 的 remote 是源、local 是目标；Push 相反。
fn snapshot_hashes(
    source: &UserMirrorInventoryDto,
    dest: &UserMirrorInventoryDto,
    direction: UserMirrorDirection,
) -> (String, String) {
    match direction {
        UserMirrorDirection::Pull => (
            source.inventory_snapshot_hash.clone(),
            dest.inventory_snapshot_hash.clone(),
        ),
        UserMirrorDirection::Push => (
            dest.inventory_snapshot_hash.clone(),
            source.inventory_snapshot_hash.clone(),
        ),
    }
}

/// 单个 Agent 的 native + portable replace-plus-delete。
fn diff_agent(
    target: AgentTarget,
    source: Option<&UserMirrorAgentInventoryDto>,
    dest: Option<&UserMirrorAgentInventoryDto>,
) -> UserMirrorAgentPlanDto {
    let source_items = source.map(|agent| agent.items.as_slice()).unwrap_or(&[]);
    let dest_items = dest.map(|agent| agent.items.as_slice()).unwrap_or(&[]);
    let source_natives = source
        .map(|agent| agent.native_files.as_slice())
        .unwrap_or(&[]);
    let dest_natives = dest
        .map(|agent| agent.native_files.as_slice())
        .unwrap_or(&[]);

    let source_portable = index_portables(source_items);
    let dest_portable = index_portables(dest_items);

    let portable_upserts = source_items
        .iter()
        .map(|item| {
            let op = if dest_portable.contains_key(&(item.kind, item.native_id.as_str())) {
                UserMirrorChangeOp::Replace
            } else {
                UserMirrorChangeOp::Write
            };
            portable_change(item, op)
        })
        .collect();

    let mut portable_deletes = Vec::new();
    let mut plugin_disables = Vec::new();
    let mut mcp_deletes = Vec::new();
    for item in dest_items {
        if source_portable.contains_key(&(item.kind, item.native_id.as_str())) {
            continue;
        }
        match item.kind {
            PortableAssetKind::Skill | PortableAssetKind::Command => {
                portable_deletes.push(portable_change(item, UserMirrorChangeOp::Delete));
            }
            PortableAssetKind::Plugin => {
                plugin_disables.push(portable_change(item, UserMirrorChangeOp::Disable));
            }
            PortableAssetKind::Mcp => {
                mcp_deletes.push(portable_change(item, UserMirrorChangeOp::Delete));
            }
        }
    }

    UserMirrorAgentPlanDto {
        target,
        instruction_writes: diff_native_files(source_natives, dest_natives),
        portable_upserts,
        portable_deletes,
        plugin_disables,
        mcp_deletes,
    }
}

fn index_portables(
    items: &[UserMirrorPortableItemDto],
) -> BTreeMap<(PortableAssetKind, &str), &UserMirrorPortableItemDto> {
    items
        .iter()
        .map(|item| ((item.kind, item.native_id.as_str()), item))
        .collect()
}

fn portable_change(
    item: &UserMirrorPortableItemDto,
    op: UserMirrorChangeOp,
) -> UserMirrorPortableChangeDto {
    UserMirrorPortableChangeDto {
        kind: item.kind,
        native_id: item.native_id.clone(),
        display_name: item.display_name.clone(),
        op,
        credential_bearing: item
            .mcp_credential
            .as_ref()
            .is_some_and(|credential| credential.present),
    }
}

/// native 身份键 `logical_id`：源有且目标缺/哈希不同 → Write/Replace；目标有源无 → Clear。
fn diff_native_files(
    source: &[UserMirrorNativeFileFactDto],
    dest: &[UserMirrorNativeFileFactDto],
) -> Vec<UserMirrorFileChangeDto> {
    let source_map = index_natives(source);
    let dest_map = index_natives(dest);
    let ids: BTreeSet<&str> = source_map.keys().chain(dest_map.keys()).copied().collect();
    let mut writes = Vec::new();
    for logical_id in ids {
        let src = source_map.get(logical_id).copied();
        let dst = dest_map.get(logical_id).copied();
        let src_exists = src.is_some_and(|file| file.exists);
        let dst_exists = dst.is_some_and(|file| file.exists);
        let source_hash = src.and_then(|file| file.content_hash.clone());
        let dest_hash = dst.and_then(|file| file.content_hash.clone());
        let op = if src_exists && dst_exists {
            if native_hashes_match(&source_hash, &dest_hash) {
                continue;
            }
            UserMirrorChangeOp::Replace
        } else if src_exists {
            UserMirrorChangeOp::Write
        } else if dst_exists {
            UserMirrorChangeOp::Clear
        } else {
            continue;
        };
        writes.push(UserMirrorFileChangeDto {
            logical_id: logical_id.to_string(),
            op,
            source_hash,
            dest_hash,
        });
    }
    writes
}

fn index_natives(
    files: &[UserMirrorNativeFileFactDto],
) -> BTreeMap<&str, &UserMirrorNativeFileFactDto> {
    files
        .iter()
        .map(|file| (file.logical_id.as_str(), file))
        .collect()
}

/// 仅在两侧都有确定 hash 且相等时视为相同；缺 hash（超大文件）必须 Replace。
fn native_hashes_match(source: &Option<String>, dest: &Option<String>) -> bool {
    matches!((source, dest), (Some(left), Some(right)) if left == right)
}

#[cfg(test)]
mod tests {
    use super::{diff_inventories, preview_from_two_inventories};
    use crate::agent_hub::models::AgentTarget;
    use crate::agent_hub::portable_inventory::PortableAssetKind;
    use crate::agent_hub::user_mirror::models::{
        UserMirrorAgentInventoryDto, UserMirrorChangeOp, UserMirrorDirection,
        UserMirrorInventoryDto, UserMirrorMcpCredentialFactDto, UserMirrorNativeFileFactDto,
        UserMirrorPortableItemDto, UserMirrorSlotHashesDto, USER_MIRROR_PLAN_TTL_MINUTES,
    };
    use chrono::{DateTime, Duration, Utc};

    fn empty_slots() -> UserMirrorSlotHashesDto {
        UserMirrorSlotHashesDto {
            common: None,
            adapted: None,
            exclusive: None,
        }
    }

    fn portable_item(kind: PortableAssetKind, native_id: &str) -> UserMirrorPortableItemDto {
        UserMirrorPortableItemDto {
            kind,
            native_id: native_id.to_string(),
            display_name: native_id.to_string(),
            content_hash: Some(format!("hash-{native_id}")),
            tree_hash: None,
            actual_enabled: Some(true),
            mcp_credential: (kind == PortableAssetKind::Mcp).then_some(
                UserMirrorMcpCredentialFactDto {
                    present: false,
                    hash: None,
                },
            ),
            warnings: Vec::new(),
        }
    }

    fn inventory_with_items(
        device: &str,
        target: AgentTarget,
        kind: PortableAssetKind,
        native_ids: &[&str],
    ) -> UserMirrorInventoryDto {
        UserMirrorInventoryDto {
            source_device_id: device.to_string(),
            inventory_snapshot_hash: format!("snap-{device}"),
            refreshed_at: "2026-08-23T00:00:00Z".into(),
            agents: vec![UserMirrorAgentInventoryDto {
                target,
                slots: empty_slots(),
                native_files: Vec::new(),
                items: native_ids
                    .iter()
                    .map(|id| portable_item(kind, id))
                    .collect(),
            }],
            credential_bearing_count: 0,
        }
    }

    fn fixture_inventory(
        device: &str,
        target: AgentTarget,
        overlapping: &[&str],
        extras: &[&str],
    ) -> UserMirrorInventoryDto {
        let mut ids = Vec::new();
        ids.extend_from_slice(overlapping);
        ids.extend_from_slice(extras);
        inventory_with_items(device, target, PortableAssetKind::Skill, &ids)
    }

    fn fixture_plugin(device: &str, extras: &[&str]) -> UserMirrorInventoryDto {
        inventory_with_items(
            device,
            AgentTarget::Claude,
            PortableAssetKind::Plugin,
            extras,
        )
    }

    fn fixture_mcp(device: &str, ids: &[&str]) -> UserMirrorInventoryDto {
        inventory_with_items(device, AgentTarget::Claude, PortableAssetKind::Mcp, ids)
    }

    fn fixture_natives(
        device: &str,
        files: Vec<UserMirrorNativeFileFactDto>,
    ) -> UserMirrorInventoryDto {
        UserMirrorInventoryDto {
            source_device_id: device.to_string(),
            inventory_snapshot_hash: format!("snap-{device}"),
            refreshed_at: "2026-08-23T00:00:00Z".into(),
            agents: vec![UserMirrorAgentInventoryDto {
                target: AgentTarget::Claude,
                slots: empty_slots(),
                native_files: files,
                items: Vec::new(),
            }],
            credential_bearing_count: 0,
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     镜像是完整覆盖：源有的 Skill 必须出现在 upserts，目标多出的必须删除，
    ///     源端独有条目不得被当成「目标多余」删掉。
    ///
    /// Code Logic（这个测试做什么）:
    ///     源 keep+gone-on-src，目标 keep+dest-only-skill；断言 keep upsert、
    ///     dest-only-skill 删除、gone-on-src 不在 deletes。
    #[test]
    fn diff_inventories_replaces_overlap_and_deletes_dest_extras() {
        let source = fixture_inventory("src", AgentTarget::Claude, &["keep"], &["gone-on-src"]);
        let dest = fixture_inventory("dst", AgentTarget::Claude, &["keep"], &["dest-only-skill"]);
        let plan = diff_inventories(
            &source,
            &dest,
            "src-dev",
            "dst-dev",
            UserMirrorDirection::Pull,
        );
        let claude = plan
            .agents
            .iter()
            .find(|a| a.target == AgentTarget::Claude)
            .unwrap();
        assert!(claude.portable_upserts.iter().any(|change| {
            change.native_id == "keep" && change.op == UserMirrorChangeOp::Replace
        }));
        assert!(claude.portable_upserts.iter().any(|change| {
            change.native_id == "gone-on-src" && change.op == UserMirrorChangeOp::Write
        }));
        assert!(claude
            .portable_deletes
            .iter()
            .any(|change| change.native_id == "dest-only-skill"
                && change.op == UserMirrorChangeOp::Delete));
        assert!(!claude
            .portable_deletes
            .iter()
            .any(|change| change.native_id == "gone-on-src"));
        assert_eq!(plan.credential_bearing_count, 0);
        assert!(!plan.has_credential_bearing_assets);
        assert!(plan.blocking_reasons.is_empty());
        assert_eq!(plan.remote_inventory_snapshot_hash, "snap-src");
        assert_eq!(plan.local_inventory_snapshot_hash, "snap-dst");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     目标多出的 Plugin 只停用 viewing 标记，禁止在镜像里当成 Uninstall/Delete。
    ///
    /// Code Logic（这个测试做什么）:
    ///     源无 plugin、目标有 orphan-plug；断言 plugin_disables Disable，
    ///     portable_deletes 不含 Plugin。
    #[test]
    fn diff_inventories_plugin_extra_is_disable_not_delete() {
        let source = fixture_plugin("src", &[]);
        let dest = fixture_plugin("dst", &["orphan-plug"]);
        let plan = diff_inventories(&source, &dest, "s", "d", UserMirrorDirection::Pull);
        let claude = &plan.agents[0];
        assert!(claude.plugin_disables.iter().any(|change| {
            change.native_id == "orphan-plug" && change.op == UserMirrorChangeOp::Disable
        }));
        assert!(claude
            .portable_deletes
            .iter()
            .all(|change| change.kind != PortableAssetKind::Plugin));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     目标多出的 MCP server 必须整项删除；确认框凭据条数取自源 inventory。
    ///
    /// Code Logic（这个测试做什么）:
    ///     源 keep 且 credential_bearing_count=1，目标 keep+extra；断言 has_credential
    ///     且 extra 进入 mcp_deletes。
    #[test]
    fn diff_inventories_mcp_extra_is_delete_and_counts_credentials() {
        let mut source = fixture_mcp("s", &["keep"]);
        source.credential_bearing_count = 1;
        let dest = fixture_mcp("d", &["keep", "extra"]);
        let plan = diff_inventories(&source, &dest, "s", "d", UserMirrorDirection::Pull);
        assert!(plan.has_credential_bearing_assets);
        assert_eq!(plan.credential_bearing_count, 1);
        assert!(plan.agents[0]
            .mcp_deletes
            .iter()
            .any(|change| change.native_id == "extra" && change.op == UserMirrorChangeOp::Delete));
        assert!(plan.agents[0]
            .portable_deletes
            .iter()
            .all(|change| change.kind != PortableAssetKind::Mcp));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     源端原生文件不存在时，目标已有托管文件必须预览为 Clear，不能漏删或写成 Replace。
    ///
    /// Code Logic（这个测试做什么）:
    ///     同源 logical_id：源 exists=false，目标 exists=true；断言 instruction_writes Clear。
    #[test]
    fn diff_inventories_clears_dest_native_when_source_missing() {
        let source = fixture_natives(
            "s",
            vec![UserMirrorNativeFileFactDto {
                logical_id: "claude.native.CLAUDE.md".into(),
                content_hash: None,
                exists: false,
                size: 0,
            }],
        );
        let dest = fixture_natives(
            "d",
            vec![UserMirrorNativeFileFactDto {
                logical_id: "claude.native.CLAUDE.md".into(),
                content_hash: Some("old-dest".into()),
                exists: true,
                size: 12,
            }],
        );
        let plan = diff_inventories(&source, &dest, "s", "d", UserMirrorDirection::Pull);
        let claude = plan
            .agents
            .iter()
            .find(|a| a.target == AgentTarget::Claude)
            .unwrap();
        assert!(claude.instruction_writes.iter().any(|change| {
            change.logical_id == "claude.native.CLAUDE.md"
                && change.op == UserMirrorChangeOp::Clear
                && change.source_hash.is_none()
                && change.dest_hash.as_deref() == Some("old-dest")
        }));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     源文件存在时，目标缺失要 Write、内容不同要 Replace；相同 hash 不得制造无意义覆盖项。
    ///
    /// Code Logic（这个测试做什么）:
    ///     三个 logical_id：哈希不同 → Replace；仅源有 → Write；哈希相同 → 不进 instruction_writes。
    #[test]
    fn diff_inventories_writes_missing_and_replaces_changed_native() {
        let source = fixture_natives(
            "s",
            vec![
                UserMirrorNativeFileFactDto {
                    logical_id: "claude.native.CLAUDE.md".into(),
                    content_hash: Some("new".into()),
                    exists: true,
                    size: 3,
                },
                UserMirrorNativeFileFactDto {
                    logical_id: "claude.slot.adapted".into(),
                    content_hash: Some("same".into()),
                    exists: true,
                    size: 4,
                },
                UserMirrorNativeFileFactDto {
                    logical_id: "claude.slot.exclusive".into(),
                    content_hash: Some("src-only".into()),
                    exists: true,
                    size: 8,
                },
            ],
        );
        let dest = fixture_natives(
            "d",
            vec![
                UserMirrorNativeFileFactDto {
                    logical_id: "claude.native.CLAUDE.md".into(),
                    content_hash: Some("old".into()),
                    exists: true,
                    size: 3,
                },
                UserMirrorNativeFileFactDto {
                    logical_id: "claude.slot.adapted".into(),
                    content_hash: Some("same".into()),
                    exists: true,
                    size: 4,
                },
            ],
        );
        let plan = diff_inventories(&source, &dest, "s", "d", UserMirrorDirection::Pull);
        let writes = &plan.agents[0].instruction_writes;
        assert!(writes.iter().any(|change| {
            change.logical_id == "claude.native.CLAUDE.md"
                && change.op == UserMirrorChangeOp::Replace
        }));
        assert!(writes.iter().any(|change| {
            change.logical_id == "claude.slot.exclusive" && change.op == UserMirrorChangeOp::Write
        }));
        assert!(!writes
            .iter()
            .any(|change| change.logical_id == "claude.slot.adapted"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     带 TTL 的 preview 才能给 apply 绑定；纯 diff 的空 token 不得直接当正式 plan。
    ///
    /// Code Logic（这个测试做什么）:
    ///     `preview_from_two_inventories` 填 UUID token 与 now+15min expires_at，并抄写方向/设备。
    #[test]
    fn preview_from_two_inventories_fills_token_and_ttl() {
        let source = fixture_inventory("src", AgentTarget::Claude, &["keep"], &[]);
        let dest = fixture_inventory("dst", AgentTarget::Claude, &["keep"], &[]);
        let before = Utc::now();
        let plan = preview_from_two_inventories(
            &source,
            &dest,
            "src-dev",
            "dst-dev",
            UserMirrorDirection::Push,
        );
        let pure = diff_inventories(
            &source,
            &dest,
            "src-dev",
            "dst-dev",
            UserMirrorDirection::Push,
        );
        assert!(!plan.plan_token.is_empty());
        assert!(pure.plan_token.is_empty());
        assert_ne!(plan.plan_token, pure.plan_token);
        let expires = DateTime::parse_from_rfc3339(&plan.expires_at)
            .expect("rfc3339")
            .with_timezone(&Utc);
        let min = before + Duration::minutes(USER_MIRROR_PLAN_TTL_MINUTES - 1);
        let max = before + Duration::minutes(USER_MIRROR_PLAN_TTL_MINUTES + 1);
        assert!(expires >= min);
        assert!(expires <= max);
        assert_eq!(plan.direction, UserMirrorDirection::Push);
        assert_eq!(plan.source_device_id, "src-dev");
        assert_eq!(plan.destination_device_id, "dst-dev");
        assert_eq!(
            plan.remote_inventory_snapshot_hash,
            dest.inventory_snapshot_hash
        );
        assert_eq!(
            plan.local_inventory_snapshot_hash,
            source.inventory_snapshot_hash
        );
    }
}
