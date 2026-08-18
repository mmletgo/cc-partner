//! portable_store — Hub 自有「本机一份」可移植资产库
//!
//! Business Logic（为什么需要这个模块）:
//!     Skill/Command 只能有一份真树；各 Agent 用指向 store 的软链表示附加。
//!     MCP 不能软链，目录里放 0600 JSON，再投影进各 Agent 配置 leaf。
//!     禁止把 `~/.agents` 当 Claude/Grok 的统一库，也禁止跟随逃逸 symlink。
//!
//! Code Logic（这个模块做什么）:
//!     布局 `<data_dir>/portable-store/{skills,commands,mcp,manifest.json}`；
//!     分类/创建/拆除 store 软链；读写 manifest 与 MCP JSON。

mod actions;
mod manifest;
mod mcp;
mod symlink;

pub use actions::{execute_mcp_store, execute_skill_or_command_store, should_use_store_semantics};
pub use manifest::{
    load_manifest, remove_manifest_attachment, remove_manifest_entry, store_id_from_canonical,
    upsert_manifest_entry, ManifestAttachment, PortableStoreKind, PortableStoreManifest,
    PortableStoreManifestEntry,
};
pub use mcp::{read_mcp_store_json, write_mcp_store_json};
pub use symlink::{
    attach_store_link, classify_store_link, create_store_link, is_under_portable_store,
    unlink_if_store_link, unlink_store_link, StoreLinkClass,
};

use crate::error::AppError;
use std::fs;
use std::path::{Path, PathBuf};

pub use symlink::migrate_native_into_store;

/// portable-store 根目录：`<data_dir>/portable-store`。
///
/// Business Logic: 跟 `CC_PARTNER_DATA_DIR` 走，不抢 `~/.agents`。
/// Code Logic: 只拼路径，不创建目录。
pub fn portable_store_root(data_dir: &Path) -> PathBuf {
    data_dir.join("portable-store")
}

/// 解析当前进程的 portable-store 根；data_dir 失败则 None。
///
/// Business Logic: 扫描时拿不到数据根就不能白名单跟随，必须 fail-closed。
/// Code Logic: `config::data_dir()` 成功才返回根路径。
pub fn current_portable_store_root() -> Option<PathBuf> {
    crate::config::data_dir()
        .ok()
        .map(|dir| portable_store_root(&dir))
}

/// 确保 store 布局存在。
///
/// Business Logic: 迁移/附加前必须有 skills/commands/mcp 目录。
/// Code Logic: `create_dir_all` 三个子目录；不写 manifest（按需创建）。
pub fn ensure_portable_store_layout(data_dir: &Path) -> Result<PathBuf, AppError> {
    let root = portable_store_root(data_dir);
    fs::create_dir_all(root.join("skills"))?;
    fs::create_dir_all(root.join("commands"))?;
    fs::create_dir_all(root.join("mcp"))?;
    Ok(root)
}

/// Skill 真树路径：`portable-store/skills/<id>`。
pub fn store_skill_dir(store_root: &Path, native_id: &str) -> PathBuf {
    store_root.join("skills").join(native_id)
}

/// Command 真文件路径：`portable-store/commands/<id>.md`。
pub fn store_command_file(store_root: &Path, native_id: &str) -> PathBuf {
    store_root.join("commands").join(format!("{native_id}.md"))
}

/// MCP 目录 JSON：`portable-store/mcp/<id>.json`。
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
    fn mcp_json_is_mode_0600_on_unix() {
        let (_tmp, data) = isolated_data_dir();
        let root = ensure_portable_store_layout(&data).expect("layout");
        let path = store_mcp_file(&root, "private");
        let value =
            serde_json::json!({"command": "uvx", "args": ["mcp"], "env": {"TOKEN": "s3cret"}});
        write_mcp_store_json(&path, &value).expect("write");
        let read = read_mcp_store_json(&path).expect("read");
        assert_eq!(read["env"]["TOKEN"], "s3cret");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
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

    #[test]
    fn mcp_detach_codex_leaves_claude_json() {
        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_tmp, data) = isolated_data_dir();
        std::env::set_var("CC_PARTNER_DATA_DIR", &data);
        let root = ensure_portable_store_layout(&data).unwrap();
        let store_file = store_mcp_file(&root, "private-api");
        write_mcp_store_json(
            &store_file,
            &serde_json::json!({"command": "uvx", "args": ["svc"]}),
        )
        .unwrap();
        let claude_json = data.join("claude.json");
        fs::write(
            &claude_json,
            r#"{"mcpServers":{"private-api":{"command":"uvx","args":["svc"]}}}"#,
        )
        .unwrap();
        let codex_toml = data.join("config.toml");
        fs::write(
            &codex_toml,
            "[mcp_servers.private-api]\ncommand = \"uvx\"\nargs = [\"svc\"]\n",
        )
        .unwrap();
        crate::agent_hub::portable_store::execute_mcp_store(
            crate::agent_hub::models::AgentTarget::Codex,
            crate::agent_hub::portable_actions::models::PortableAssetActionKind::Detach,
            "private-api",
            &codex_toml,
            true,
            None,
        )
        .expect("detach");
        let toml_after = fs::read_to_string(&codex_toml).unwrap();
        assert!(
            !toml_after.contains("private-api"),
            "codex leaf must be removed: {toml_after}"
        );
        let claude_after = fs::read_to_string(&claude_json).unwrap();
        assert!(
            claude_after.contains("private-api"),
            "claude json must stay: {claude_after}"
        );
        std::env::remove_var("CC_PARTNER_DATA_DIR");
    }

    #[test]
    fn mcp_destroy_clears_claude_and_codex_leaves() {
        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (_tmp, data) = isolated_data_dir();
        std::env::set_var("CC_PARTNER_DATA_DIR", &data);
        let claude_home = data.join("claude-home");
        fs::create_dir_all(&claude_home).unwrap();
        let claude_json = claude_home.join(".claude.json");
        fs::write(
            &claude_json,
            r#"{"mcpServers":{"private-api":{"command":"uvx","args":["svc"]}}}"#,
        )
        .unwrap();
        let codex_home = data.join("codex-home");
        fs::create_dir_all(&codex_home).unwrap();
        let codex_toml = codex_home.join("config.toml");
        fs::write(
            &codex_toml,
            "[mcp_servers.private-api]\ncommand = \"uvx\"\nargs = [\"svc\"]\n",
        )
        .unwrap();
        let prev_claude = std::env::var_os("CLAUDE_CONFIG_DIR");
        let prev_codex = std::env::var_os("CODEX_HOME");
        std::env::set_var("CLAUDE_CONFIG_DIR", &claude_home);
        std::env::set_var("CODEX_HOME", &codex_home);
        let root = ensure_portable_store_layout(&data).unwrap();
        let store_file = store_mcp_file(&root, "private-api");
        write_mcp_store_json(
            &store_file,
            &serde_json::json!({"command": "uvx", "args": ["svc"]}),
        )
        .unwrap();
        crate::agent_hub::portable_store::execute_mcp_store(
            crate::agent_hub::models::AgentTarget::Codex,
            crate::agent_hub::portable_actions::models::PortableAssetActionKind::DestroyStore,
            "private-api",
            &codex_toml,
            true,
            None,
        )
        .expect("destroy");
        assert!(!store_file.exists(), "destroy must remove store MCP json");
        let claude_after = fs::read_to_string(&claude_json).unwrap();
        assert!(
            !claude_after.contains("private-api"),
            "claude leaf must clear: {claude_after}"
        );
        let toml_after = fs::read_to_string(&codex_toml).unwrap();
        assert!(
            !toml_after.contains("private-api"),
            "codex leaf must clear: {toml_after}"
        );
        match prev_claude {
            Some(value) => std::env::set_var("CLAUDE_CONFIG_DIR", value),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }
        match prev_codex {
            Some(value) => std::env::set_var("CODEX_HOME", value),
            None => std::env::remove_var("CODEX_HOME"),
        }
        std::env::remove_var("CC_PARTNER_DATA_DIR");
    }
}
