//! portable_store — Hub 自有「本机一份」Skill/Command 库
//!
//! Business Logic（为什么需要这个模块）:
//!     Skill/Command 只能有一份真树；各 Agent 用指向 store 的软链表示附加。
//!     MCP 不进仓库：启停/卸载改各家配置 leaf，跨 Agent 走已有 Pull。
//!     禁止把 `~/.agents` 当 Claude/Grok 的统一库，也禁止跟随逃逸 symlink。
//!
//! Code Logic（这个模块做什么）:
//!     用户级 `<data_dir>/portable-store/`；项目级
//!     `<data_dir>/project-portable-store/<hubProjectId>/`；
//!     分类/创建/拆除 Skill/Command 软链；读写 manifest。
//!     仍创建 `mcp/` 以免打散遗留 JSON，但不再写入或投影。

mod actions;
mod manifest;
mod symlink;

pub use actions::{
    execute_skill_or_command_store, observed_or_native_store_mount, should_use_store_semantics,
};
pub use manifest::{
    load_manifest, remove_manifest_attachment, remove_manifest_entry, store_id_from_canonical,
    upsert_manifest_entry, ManifestAttachment, PortableStoreKind, PortableStoreManifest,
    PortableStoreManifestEntry,
};
pub use symlink::{
    attach_store_link, classify_store_link, classify_store_link_with_ancestors, create_store_link,
    is_under_portable_store, restore_escape_into_store, unlink_if_store_link, unlink_store_link,
    StoreLinkClass,
};

use crate::{agent_hub::models::ScopeKind, error::AppError};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub use symlink::migrate_native_into_store;

/// portable-store 根目录：`<data_dir>/portable-store`。
///
/// Business Logic: 跟 `CC_PARTNER_DATA_DIR` 走，不抢 `~/.agents`。
/// Code Logic: 只拼路径，不创建目录。
pub fn portable_store_root(data_dir: &Path) -> PathBuf {
    data_dir.join("portable-store")
}

/// 解析当前进程的用户级 portable-store 根；data_dir 失败则 None。
///
/// Business Logic: 扫描时拿不到数据根就不能白名单跟随，必须 fail-closed。
/// Code Logic: `config::data_dir()` 成功才返回根路径。
pub fn current_portable_store_root() -> Option<PathBuf> {
    crate::config::data_dir()
        .ok()
        .map(|dir| portable_store_root(&dir))
}

/// 项目级 Skill/Command 仓库根：`<data_dir>/project-portable-store/<hubProjectId>`。
///
/// Business Logic: 项目 Agent 的仓库不得混入用户级 portable-store。
/// Code Logic: 与用户仓库并列，避免 `is_under_portable_store` 把项目真树当成用户库。
pub fn portable_project_store_root(data_dir: &Path, hub_project_id: &str) -> PathBuf {
    data_dir
        .join("project-portable-store")
        .join(hub_project_id.trim())
}

/// 按 scope 选择仓库根；项目级缺合法 hub id 时返回 None，不回退用户库。
pub fn try_portable_store_root_for_scope(
    data_dir: &Path,
    scope_kind: ScopeKind,
    hub_project_id: Option<&str>,
) -> Option<PathBuf> {
    if scope_kind == ScopeKind::Project {
        let id = hub_project_id.map(str::trim).filter(|s| !s.is_empty())?;
        validate_store_native_id(id).ok()?;
        return Some(portable_project_store_root(data_dir, id));
    }
    Some(portable_store_root(data_dir))
}

/// 按 scope 选择仓库根；非法/空的项目 id 回退用户库。
pub fn portable_store_root_for_scope(
    data_dir: &Path,
    scope_kind: ScopeKind,
    hub_project_id: Option<&str>,
) -> PathBuf {
    try_portable_store_root_for_scope(data_dir, scope_kind, hub_project_id)
        .unwrap_or_else(|| portable_store_root(data_dir))
}

/// 当前进程下用户库 + 已存在的项目库根。
///
/// Business Logic: 分类软链时必须认出项目仓库，否则项目附加会被当成逃逸链。
/// Code Logic: 用户根恒在；`project-portable-store/*` 仅收录合法 id 目录。
pub fn current_portable_store_roots() -> Vec<PathBuf> {
    let Ok(data) = crate::config::data_dir() else {
        return Vec::new();
    };
    list_portable_store_roots(&data)
}

/// 列出 data_dir 下全部 portable-store 根（用户 + 项目）。
pub fn list_portable_store_roots(data_dir: &Path) -> Vec<PathBuf> {
    let mut roots = vec![portable_store_root(data_dir)];
    let projects = data_dir.join("project-portable-store");
    let Ok(entries) = fs::read_dir(&projects) else {
        return roots;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if validate_store_native_id(name).is_ok() {
            roots.push(path);
        }
    }
    roots
}

/// 确保指定 store 根布局存在。
///
/// Business Logic: 迁移/附加前必须有 skills/commands；保留 mcp/ 以免打散遗留凭据文件。
/// Code Logic: `create_dir_all` 三个子目录；不写 manifest（按需创建）；不写新 MCP JSON。
pub fn ensure_store_layout(store_root: &Path) -> Result<PathBuf, AppError> {
    fs::create_dir_all(store_root.join("skills"))?;
    fs::create_dir_all(store_root.join("commands"))?;
    fs::create_dir_all(store_root.join("mcp"))?;
    Ok(store_root.to_path_buf())
}

/// 确保用户级 store 布局存在。
pub fn ensure_portable_store_layout(data_dir: &Path) -> Result<PathBuf, AppError> {
    ensure_store_layout(&portable_store_root(data_dir))
}

/// Skill 真树路径：`portable-store/skills/<id>`。
pub fn store_skill_dir(store_root: &Path, native_id: &str) -> PathBuf {
    store_root.join("skills").join(native_id)
}

/// Command 真文件路径：`portable-store/commands/<id>.md`。
pub fn store_command_file(store_root: &Path, native_id: &str) -> PathBuf {
    store_root.join("commands").join(format!("{native_id}.md"))
}

/// 遗留 MCP 目录 JSON 路径：`portable-store/mcp/<id>.json`。
///
/// Business Logic: 旧版曾把 leaf 复制进仓库；现不再写入或投影，只用来识别遗留文件。
pub fn store_mcp_file(store_root: &Path, native_id: &str) -> PathBuf {
    store_root.join("mcp").join(format!("{native_id}.json"))
}

/// 由 kind + native id 生成稳定 storeId。
///
/// Business Logic: 同一份真树在各 Agent 盘点里必须对上，才能去重与「仍被其他路径加载」。
/// Code Logic: `skill:<id>` / `command:<id>` / `mcp:<id>`。
pub fn store_id_for(kind: PortableStoreKind, native_id: &str) -> String {
    format!("{}:{native_id}", kind.as_str())
}

/// 校验 native id，拒绝路径穿越。
///
/// Business Logic: store 路径由 id 拼接，`.` / `..` / 分隔符会逃出 store。
/// Code Logic: 非空、无分隔符、不是 `.`/`..`。
pub fn validate_store_native_id(native_id: &str) -> Result<(), AppError> {
    let trimmed = native_id.trim();
    if trimmed.is_empty()
        || trimmed == "."
        || trimmed == ".."
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains('\0')
    {
        return Err(AppError::validation(
            "PORTABLE_STORE_INVALID_NATIVE_ID".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::targets::portable::DATA_DIR_ENV_LOCK;

    fn isolated_data_dir() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        fs::create_dir_all(&data).expect("data");
        (tmp, data)
    }

    #[test]
    fn layout_creates_three_leaves() {
        let (_tmp, data) = isolated_data_dir();
        let root = ensure_portable_store_layout(&data).expect("layout");
        assert!(root.join("skills").is_dir());
        assert!(root.join("commands").is_dir());
        assert!(root.join("mcp").is_dir());
        assert_eq!(root, data.join("portable-store"));
    }

    #[test]
    fn store_id_is_kind_prefixed() {
        assert_eq!(store_id_for(PortableStoreKind::Skill, "foo"), "skill:foo");
        assert_eq!(
            store_id_for(PortableStoreKind::Command, "release"),
            "command:release"
        );
        assert_eq!(store_id_for(PortableStoreKind::Mcp, "api"), "mcp:api");
    }

    #[test]
    fn reject_path_escape_native_id() {
        assert!(validate_store_native_id("../x").is_err());
        assert!(validate_store_native_id("a/b").is_err());
        assert!(validate_store_native_id("foo").is_ok());
    }

    #[test]
    fn classify_follows_only_store_targets() {
        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_tmp, data) = isolated_data_dir();
        std::env::set_var("CC_PARTNER_DATA_DIR", &data);
        let root = ensure_portable_store_layout(&data).expect("layout");
        let skill = store_skill_dir(&root, "foo");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "---\nname: foo\n---\n").unwrap();

        let native = data.join("native").join("skills");
        fs::create_dir_all(&native).unwrap();
        let link = native.join("foo");
        create_store_link(&skill, &link).expect("link");

        match classify_store_link(&link) {
            StoreLinkClass::StoreLink { store_id, .. } => {
                assert_eq!(store_id, "skill:foo");
            }
            other => panic!("expected store link, got {other:?}"),
        }

        let escape_dir = data.join("escape-target");
        fs::create_dir_all(&escape_dir).unwrap();
        fs::write(escape_dir.join("SKILL.md"), "secret").unwrap();
        let escape = native.join("evil");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&escape_dir, &escape).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&escape_dir, &escape).unwrap();
        assert!(matches!(
            classify_store_link(&escape),
            StoreLinkClass::EscapeLink
        ));

        let regular = native.join("plain");
        fs::create_dir_all(&regular).unwrap();
        assert!(matches!(
            classify_store_link(&regular),
            StoreLinkClass::Regular
        ));
        std::env::remove_var("CC_PARTNER_DATA_DIR");
    }

    #[test]
    fn project_store_root_is_isolated_from_user_store() {
        let (_tmp, data) = isolated_data_dir();
        let user = portable_store_root(&data);
        let project = portable_project_store_root(&data, "hub-abc");
        assert_ne!(user, project);
        assert!(!project.starts_with(user.join("skills")));
        let layout = ensure_store_layout(&project).expect("project layout");
        assert!(layout.join("skills").is_dir());
        assert_eq!(
            portable_store_root_for_scope(
                &data,
                crate::agent_hub::models::ScopeKind::Project,
                Some("hub-abc")
            ),
            project
        );
        assert_eq!(
            portable_store_root_for_scope(&data, crate::agent_hub::models::ScopeKind::User, None),
            user
        );
        assert!(try_portable_store_root_for_scope(
            &data,
            crate::agent_hub::models::ScopeKind::Project,
            None
        )
        .is_none());
    }

    #[test]
    fn classify_follows_project_store_targets() {
        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_tmp, data) = isolated_data_dir();
        std::env::set_var("CC_PARTNER_DATA_DIR", &data);
        let root =
            ensure_store_layout(&portable_project_store_root(&data, "hub-abc")).expect("layout");
        let skill = store_skill_dir(&root, "foo");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "---\nname: foo\n---\n").unwrap();

        let native = data.join("proj").join("skills");
        fs::create_dir_all(&native).unwrap();
        let link = native.join("foo");
        create_store_link(&skill, &link).expect("link");

        match classify_store_link(&link) {
            StoreLinkClass::StoreLink { store_id, .. } => {
                assert_eq!(store_id, "skill:foo");
            }
            other => panic!("expected project store link, got {other:?}"),
        }
        std::env::remove_var("CC_PARTNER_DATA_DIR");
    }

    #[test]
    fn leftover_mcp_json_is_kept_and_classified() {
        let (_tmp, data) = isolated_data_dir();
        let root = ensure_portable_store_layout(&data).expect("layout");
        let path = store_mcp_file(&root, "private-api");
        fs::write(&path, r#"{"command":"uvx","env":{"TOKEN":"s3cret"}}"#).unwrap();
        ensure_portable_store_layout(&data).expect("layout again");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            r#"{"command":"uvx","env":{"TOKEN":"s3cret"}}"#
        );
        let canonical = fs::canonicalize(&path).unwrap();
        assert_eq!(
            store_id_from_canonical(&canonical, &root).as_deref(),
            Some("mcp:private-api")
        );
    }

    #[test]
    fn unlink_store_link_leaves_real_tree() {
        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_tmp, data) = isolated_data_dir();
        std::env::set_var("CC_PARTNER_DATA_DIR", &data);
        let root = ensure_portable_store_layout(&data).expect("layout");
        let skill = store_skill_dir(&root, "keep");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "body").unwrap();
        let link = data.join("claude-skills").join("keep");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        create_store_link(&skill, &link).expect("link");
        unlink_if_store_link(&link).expect("unlink");
        assert!(!link.exists());
        assert!(skill.join("SKILL.md").is_file());
        std::env::remove_var("CC_PARTNER_DATA_DIR");
    }

    #[test]
    fn migrate_then_detach_keeps_store_tree() {
        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_tmp, data) = isolated_data_dir();
        std::env::set_var("CC_PARTNER_DATA_DIR", &data);
        let native = data.join("claude").join("skills").join("foo");
        fs::create_dir_all(&native).unwrap();
        fs::write(native.join("SKILL.md"), "---\nname: foo\n---\nbody").unwrap();
        let outcome = crate::agent_hub::portable_store::execute_skill_or_command_store(
            crate::agent_hub::models::AgentTarget::Claude,
            crate::agent_hub::portable_actions::models::PortableAssetActionKind::MigrateToStore,
            crate::agent_hub::portable_inventory::PortableAssetKind::Skill,
            "foo",
            &native,
            None,
        )
        .expect("migrate");
        assert_eq!(
            outcome,
            crate::agent_hub::portable_actions::targets::TargetActionRawOutcome::Applied
        );
        assert!(fs::symlink_metadata(&native)
            .unwrap()
            .file_type()
            .is_symlink());
        let store_tree = store_skill_dir(&portable_store_root(&data), "foo");
        assert!(store_tree.join("SKILL.md").is_file());
        crate::agent_hub::portable_store::execute_skill_or_command_store(
            crate::agent_hub::models::AgentTarget::Claude,
            crate::agent_hub::portable_actions::models::PortableAssetActionKind::Disable,
            crate::agent_hub::portable_inventory::PortableAssetKind::Skill,
            "foo",
            &native,
            None,
        )
        .expect("disable");
        assert!(!native.exists());
        assert!(store_tree.join("SKILL.md").is_file());
        std::env::remove_var("CC_PARTNER_DATA_DIR");
    }

    fn write_skill(dir: &Path, version: Option<&str>, body: &str) {
        fs::create_dir_all(dir).unwrap();
        let version_line = version
            .map(|v| format!("version: {v}\n"))
            .unwrap_or_default();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: foo\n{version_line}---\n{body}"),
        )
        .unwrap();
    }

    fn migrate_foo(
        native: &Path,
    ) -> crate::agent_hub::portable_actions::targets::TargetActionRawOutcome {
        crate::agent_hub::portable_store::execute_skill_or_command_store(
            crate::agent_hub::models::AgentTarget::Claude,
            crate::agent_hub::portable_actions::models::PortableAssetActionKind::MigrateToStore,
            crate::agent_hub::portable_inventory::PortableAssetKind::Skill,
            "foo",
            native,
            None,
        )
        .expect("migrate")
    }

    fn assert_store_link(native: &Path) {
        assert!(
            fs::symlink_metadata(native)
                .unwrap()
                .file_type()
                .is_symlink(),
            "native should become a store symlink"
        );
    }

    fn set_mtime(path: &Path, ago_secs: u64) {
        use std::time::{Duration, SystemTime};
        let file = fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(ago_secs))
            .unwrap();
    }

    #[test]
    fn migrate_same_content_attaches_without_duplicating() {
        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_tmp, data) = isolated_data_dir();
        std::env::set_var("CC_PARTNER_DATA_DIR", &data);
        let store = store_skill_dir(&ensure_portable_store_layout(&data).unwrap(), "foo");
        write_skill(&store, Some("1.0.0"), "same-body");
        let native = data.join("claude").join("skills").join("foo");
        write_skill(&native, Some("1.0.0"), "same-body");
        assert_eq!(
            migrate_foo(&native),
            crate::agent_hub::portable_actions::targets::TargetActionRawOutcome::Applied
        );
        assert_store_link(&native);
        assert_eq!(
            fs::read_to_string(store.join("SKILL.md")).unwrap(),
            "---\nname: foo\nversion: 1.0.0\n---\nsame-body"
        );
        std::env::remove_var("CC_PARTNER_DATA_DIR");
    }

    #[test]
    fn migrate_keeps_newer_frontmatter_version_and_deletes_old() {
        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_tmp, data) = isolated_data_dir();
        std::env::set_var("CC_PARTNER_DATA_DIR", &data);
        let store = store_skill_dir(&ensure_portable_store_layout(&data).unwrap(), "foo");
        write_skill(&store, Some("1.0.0"), "old-body");
        let native = data.join("claude").join("skills").join("foo");
        write_skill(&native, Some("2.0.0"), "new-body");
        assert_eq!(
            migrate_foo(&native),
            crate::agent_hub::portable_actions::targets::TargetActionRawOutcome::Applied
        );
        assert_store_link(&native);
        let text = fs::read_to_string(store.join("SKILL.md")).unwrap();
        assert!(text.contains("version: 2.0.0"));
        assert!(text.contains("new-body"));
        assert!(!text.contains("old-body"));
        std::env::remove_var("CC_PARTNER_DATA_DIR");
    }

    #[test]
    fn migrate_keeps_store_when_store_version_is_newer() {
        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_tmp, data) = isolated_data_dir();
        std::env::set_var("CC_PARTNER_DATA_DIR", &data);
        let store = store_skill_dir(&ensure_portable_store_layout(&data).unwrap(), "foo");
        write_skill(&store, Some("2.1.0"), "store-new");
        let native = data.join("claude").join("skills").join("foo");
        write_skill(&native, Some("1.9.0"), "native-old");
        assert_eq!(
            migrate_foo(&native),
            crate::agent_hub::portable_actions::targets::TargetActionRawOutcome::Applied
        );
        assert_store_link(&native);
        let text = fs::read_to_string(store.join("SKILL.md")).unwrap();
        assert!(text.contains("version: 2.1.0"));
        assert!(text.contains("store-new"));
        assert!(!text.contains("native-old"));
        std::env::remove_var("CC_PARTNER_DATA_DIR");
    }

    #[test]
    fn migrate_without_version_keeps_newer_mtime() {
        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_tmp, data) = isolated_data_dir();
        std::env::set_var("CC_PARTNER_DATA_DIR", &data);
        let store = store_skill_dir(&ensure_portable_store_layout(&data).unwrap(), "foo");
        write_skill(&store, None, "older-mtime");
        set_mtime(&store.join("SKILL.md"), 120);
        let native = data.join("claude").join("skills").join("foo");
        write_skill(&native, None, "newer-mtime");
        assert_eq!(
            migrate_foo(&native),
            crate::agent_hub::portable_actions::targets::TargetActionRawOutcome::Applied
        );
        assert_store_link(&native);
        let text = fs::read_to_string(store.join("SKILL.md")).unwrap();
        assert!(text.contains("newer-mtime"));
        assert!(!text.contains("older-mtime"));
        std::env::remove_var("CC_PARTNER_DATA_DIR");
    }

    fn sample_item(
        native_id: &str,
        source_path: &Path,
    ) -> crate::agent_hub::portable_inventory::PortableInventoryItemDto {
        use crate::agent_hub::{
            models::ScopeKind,
            portable_inventory::{
                PortableAssetOwner, PortableInventoryItemCapabilitiesDto,
                PortableInventoryManagementState, PortableInventorySourceOrigin,
                PortableOriginKind,
            },
        };
        crate::agent_hub::portable_inventory::PortableInventoryItemDto {
            inventory_item_id: format!("id-{native_id}"),
            target: crate::agent_hub::models::AgentTarget::Claude,
            loaded_by: crate::agent_hub::models::AgentTarget::Claude,
            owned_by: PortableAssetOwner::Claude,
            origin_kind: PortableOriginKind::Native,
            native_output_candidate: true,
            kind: crate::agent_hub::portable_inventory::PortableAssetKind::Skill,
            native_id: native_id.into(),
            display_name: native_id.into(),
            description: None,
            version: None,
            scope_id: "user".into(),
            scope_kind: ScopeKind::User,
            project_id: None,
            project_opted_in: false,
            source_path: Some(source_path.to_string_lossy().into_owned()),
            source_origin: PortableInventorySourceOrigin::Standalone,
            parent_plugin_inventory_item_id: None,
            actual_enabled: Some(false),
            content_hash: Some("h".into()),
            tree_hash: None,
            canonical_asset_id: None,
            canonical_revision_id: None,
            management_state: PortableInventoryManagementState::Unmanaged,
            desired_presence: None,
            desired_enabled: None,
            materialization_status: None,
            capabilities: PortableInventoryItemCapabilitiesDto::default(),
            warnings: vec![],
            mcp_credential: None,
            store: Default::default(),
        }
    }

    /// Business Logic: 与仓库同内容的 ~/.agents 真树在卸下时删除，不得再变成「迁入便携仓库」。
    #[test]
    fn detach_same_content_leftover_real_tree_keeps_store() {
        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_tmp, data) = isolated_data_dir();
        std::env::set_var("CC_PARTNER_DATA_DIR", &data);
        let store_tree = store_skill_dir(&ensure_portable_store_layout(&data).unwrap(), "foo");
        write_skill(&store_tree, Some("1.0.0"), "canonical");
        let leftover = data.join("agents").join("skills").join("foo");
        write_skill(&leftover, Some("1.0.0"), "canonical");
        let outcome = crate::agent_hub::portable_store::execute_skill_or_command_store(
            crate::agent_hub::models::AgentTarget::Codex,
            crate::agent_hub::portable_actions::models::PortableAssetActionKind::Detach,
            crate::agent_hub::portable_inventory::PortableAssetKind::Skill,
            "foo",
            &leftover,
            None,
        )
        .expect("detach leftover");
        assert_eq!(
            outcome,
            crate::agent_hub::portable_actions::targets::TargetActionRawOutcome::Applied
        );
        assert!(
            !leftover.exists(),
            "same-content leftover must be removed on detach"
        );
        assert!(
            store_tree.join("SKILL.md").is_file(),
            "portable-store tree must remain"
        );
        std::env::remove_var("CC_PARTNER_DATA_DIR");
    }

    #[test]
    fn migrate_from_disabled_source_path_attaches_native_link() {
        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_tmp, data) = isolated_data_dir();
        std::env::set_var("CC_PARTNER_DATA_DIR", &data);
        let disabled = data
            .join("claude-assets")
            .join("disabled")
            .join("skills")
            .join("foo");
        write_skill(&disabled, Some("1.0.0"), "from-disabled");
        let native = data.join("claude").join("skills").join("foo");
        assert!(!native.exists());
        let item = sample_item("foo", &disabled);
        let outcome = crate::agent_hub::portable_store::execute_skill_or_command_store(
            crate::agent_hub::models::AgentTarget::Claude,
            crate::agent_hub::portable_actions::models::PortableAssetActionKind::MigrateToStore,
            crate::agent_hub::portable_inventory::PortableAssetKind::Skill,
            "foo",
            &native,
            Some(&item),
        )
        .expect("migrate");
        assert_eq!(
            outcome,
            crate::agent_hub::portable_actions::targets::TargetActionRawOutcome::Applied
        );
        assert_store_link(&native);
        let store_tree = store_skill_dir(&portable_store_root(&data), "foo");
        let text = fs::read_to_string(store_tree.join("SKILL.md")).unwrap();
        assert!(text.contains("from-disabled"));
        assert!(
            !disabled.exists(),
            "disabled copy must be moved, not left as a second tree"
        );
        std::env::remove_var("CC_PARTNER_DATA_DIR");
    }

    #[test]
    fn migrate_missing_source_returns_typed_error() {
        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_tmp, data) = isolated_data_dir();
        std::env::set_var("CC_PARTNER_DATA_DIR", &data);
        let native = data.join("claude").join("skills").join("foo");
        let outcome = crate::agent_hub::portable_store::execute_skill_or_command_store(
            crate::agent_hub::models::AgentTarget::Claude,
            crate::agent_hub::portable_actions::models::PortableAssetActionKind::MigrateToStore,
            crate::agent_hub::portable_inventory::PortableAssetKind::Skill,
            "foo",
            &native,
            None,
        )
        .expect("typed failure, not IO Err");
        match outcome {
            crate::agent_hub::portable_actions::targets::TargetActionRawOutcome::Failed {
                code,
                ..
            } => {
                assert_eq!(code, "PORTABLE_ASSET_ACTION_SOURCE_MISSING");
            }
            other => panic!("expected SOURCE_MISSING, got {other:?}"),
        }
        std::env::remove_var("CC_PARTNER_DATA_DIR");
    }

    #[test]
    fn restore_escape_copies_source_into_store_and_relinks_native() {
        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_tmp, data) = isolated_data_dir();
        std::env::set_var("CC_PARTNER_DATA_DIR", &data);
        let real = data.join("agents").join("skills").join("escape-skill");
        fs::create_dir_all(real.join("nested")).unwrap();
        fs::write(real.join("SKILL.md"), "---\nname: escape-skill\n---\nbody").unwrap();
        fs::write(real.join("nested/data.txt"), "payload").unwrap();
        let native = data.join("claude").join("skills").join("escape-skill");
        fs::create_dir_all(native.parent().unwrap()).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &native).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&real, &native).unwrap();
        let outcome = crate::agent_hub::portable_store::execute_skill_or_command_store(
            crate::agent_hub::models::AgentTarget::Claude,
            crate::agent_hub::portable_actions::models::PortableAssetActionKind::MaterializeEscapeLink,
            crate::agent_hub::portable_inventory::PortableAssetKind::Skill,
            "escape-skill",
            &native,
            None,
        )
        .expect("restore");
        assert_eq!(
            outcome,
            crate::agent_hub::portable_actions::targets::TargetActionRawOutcome::Applied
        );
        assert!(fs::symlink_metadata(&native)
            .unwrap()
            .file_type()
            .is_symlink());
        let store_tree = store_skill_dir(&portable_store_root(&data), "escape-skill");
        assert!(store_tree.join("SKILL.md").is_file());
        assert!(real.join("SKILL.md").is_file(), "source must remain");
        assert_eq!(
            fs::canonicalize(&native).unwrap(),
            fs::canonicalize(&store_tree).unwrap()
        );
        std::env::remove_var("CC_PARTNER_DATA_DIR");
    }
}
