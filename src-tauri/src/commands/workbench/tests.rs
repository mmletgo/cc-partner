//! Workbench 命令单测。
#![allow(dead_code)]
#![allow(unused_imports)]

use super::common::*;
use super::git::{
    build_commit_message_instruction, build_merge_conflict_resolution_instruction,
    content_has_conflict_markers, map_remote_merge_result_value, merge_conflict_resolution_schema,
    safe_merge_resolution_path, validate_merge_resolution_path, workbench_commit_message_schema,
};
use super::sessions::should_attempt_session_zoom;
use super::*;
use crate::models::device::Device;
use crate::workbench::git as workbench_git;
use crate::workbench::models::{
    WorkbenchDetectedFileType, WorkbenchGitStatusDto, WorkbenchProjectDto, WorkbenchProjectRow,
    WorkbenchSessionDto, WorkbenchSessionRow, WorkbenchWorktreeDto, WorkbenchWorktreeRow,
};
use chrono::Utc;
use std::collections::HashMap;
use std::path::Path;

/// Business Logic（为什么需要这个测试）:
///     远端 Workbench 命令只能调用当前已发现且在线的设备，离线时需要返回稳定中文错误。
///
/// Code Logic（这个测试做什么）:
///     用内存 HashMap 构造一个设备，断言 helper 返回 base URL；缺失设备返回“远端设备不在线”。
#[test]
fn device_base_url_from_devices_returns_url_and_offline_error() {
    let mut devices = HashMap::new();
    devices.insert(
        "device-a".to_string(),
        Device {
            id: "device-a".to_string(),
            name: "Remote Mac".to_string(),
            host: "192.168.1.9".to_string(),
            port: 14210,
            last_seen: Utc::now(),
            online: true,
            proto_version: 0,
            capabilities: Vec::new(),
        },
    );

    let url = device_base_url_from_devices(&devices, "device-a").unwrap();
    let missing = device_base_url_from_devices(&devices, "missing").unwrap_err();

    assert_eq!(url, "http://192.168.1.9:14210");
    assert_eq!(missing.to_string(), "远端设备不在线");
}

/// Business Logic（为什么需要这个测试）:
///     移动端 zoom-pane 是单 pane 展示增强，不应让 raw PTY 或 disconnected window 出现额外错误。
///
/// Code Logic（这个测试做什么）:
///     构造 session row，断言只有 running tmux window 需要调用 registry ensure-zoom。
#[test]
fn session_zoom_attempt_only_for_running_tmux_window() {
    let mut row = WorkbenchSessionRow {
        id: "session-1".to_string(),
        project_id: "project-1".to_string(),
        worktree_id: Some("worktree-1".to_string()),
        name: "Terminal".to_string(),
        name_source: "default".to_string(),
        command: "shell".to_string(),
        cwd: "/repo".to_string(),
        status: "running".to_string(),
        cols: 80,
        rows: 24,
        started_at: "2026-07-05T00:00:00Z".to_string(),
        exited_at: None,
        exit_code: None,
        backend: "tmux".to_string(),
        backend_id: Some("cc-partner-project".to_string()),
        backend_window_id: Some("@2".to_string()),
        created_at: "2026-07-05T00:00:00Z".to_string(),
        updated_at: "2026-07-05T00:00:00Z".to_string(),
    };

    assert!(should_attempt_session_zoom(&row));

    row.status = "disconnected".to_string();
    assert!(!should_attempt_session_zoom(&row));

    row.status = "running".to_string();
    row.backend = "pty".to_string();
    assert!(!should_attempt_session_zoom(&row));

    row.backend = "tmux".to_string();
    row.backend_window_id = None;
    assert!(!should_attempt_session_zoom(&row));
}

/// Business Logic（为什么需要这个测试）:
///     打开远端项目会在本机保存一个快捷方式，该快捷方式必须稳定复用同一 ID 并标记为 remote。
///
/// Code Logic（这个测试做什么）:
///     用远端返回的项目 DTO 和已有 row 构造本地快捷方式，断言 kind、id 和 created_at 复用规则。
#[test]
fn build_remote_project_shortcut_row_preserves_remote_kind_and_stable_id() {
    let remote = WorkbenchProjectDto {
        id: "remote-side-local-id".to_string(),
        name: "Remote App".to_string(),
        kind: "local".to_string(),
        device_id: "remote-device".to_string(),
        device_name: "Remote Mac".to_string(),
        path: "/Users/hans/web_project/app".to_string(),
        last_opened_at: "2026-06-25T00:00:00Z".to_string(),
        created_at: "2026-06-25T00:00:00Z".to_string(),
        updated_at: "2026-06-25T00:00:00Z".to_string(),
    };
    let existing = WorkbenchProjectRow {
        id: crate::workbench::remote_ids::remote_project_id(
            "device-a",
            "/Users/hans/web_project/app",
        ),
        name: "Old Name".to_string(),
        kind: "remote".to_string(),
        device_id: "device-a".to_string(),
        device_name: "Old Device".to_string(),
        path: "/Users/hans/web_project/app".to_string(),
        last_opened_at: "2026-06-24T00:00:00Z".to_string(),
        created_at: "2026-06-24T00:00:00Z".to_string(),
        updated_at: "2026-06-24T00:00:00Z".to_string(),
    };

    let row = build_remote_project_shortcut_row(
        "device-a",
        Some("Current Device"),
        &remote,
        Some(&existing),
        "2026-06-26T00:00:00Z",
    );

    assert_eq!(row.kind, "remote");
    assert_eq!(row.id, existing.id);
    assert_eq!(row.created_at, existing.created_at);
    assert_eq!(row.device_name, "Current Device");
    assert_eq!(row.last_opened_at, "2026-06-26T00:00:00Z");
}

/// Business Logic（为什么需要这个测试）:
///     远端 worktree 返回给前端时需要映射 worktree id，但 worktree.projectId 必须仍指向本机 remote shortcut。
///
/// Code Logic（这个测试做什么）:
///     构造远端返回的 local worktree DTO，经过映射 helper 后断言 id 带 remote 前缀、project_id 保持本机项目 ID。
#[test]
fn map_remote_worktree_dtos_prefixes_worktree_id_and_keeps_local_project_id() {
    let items = vec![WorkbenchWorktreeDto {
        id: "inner-main".to_string(),
        project_id: "inner-project".to_string(),
        name: "main".to_string(),
        branch: Some("main".to_string()),
        base_branch: None,
        path: "/remote/repo".to_string(),
        is_main: true,
        status: WorkbenchGitStatusDto::default(),
        created_at: "2026-06-26T00:00:00Z".to_string(),
        updated_at: "2026-06-26T00:00:00Z".to_string(),
    }];

    let mapped = map_remote_worktree_dtos("device-a", "remote:device-a:project-hash", items);

    assert_eq!(mapped[0].id, "remote:device-a:inner-main");
    assert_eq!(mapped[0].project_id, "remote:device-a:project-hash");
}

/// Business Logic（为什么需要这个测试）:
///     远端 terminal session 返回给本机前端时，sessionId 和 worktreeId 都必须带设备前缀，
///     但 projectId 应保持本机 remote shortcut 项目 ID 以便页面按项目过滤。
///
/// Code Logic（这个测试做什么）:
///     构造远端 session DTO，经过映射 helper 后断言 session/worktree 使用 remote entity ID。
#[test]
fn map_remote_session_dtos_prefixes_session_and_worktree_ids() {
    let items = vec![WorkbenchSessionDto {
        id: "inner-session".to_string(),
        project_id: "inner-project".to_string(),
        worktree_id: Some("inner-worktree".to_string()),
        name: "Remote App".to_string(),
        name_source: "default".to_string(),
        command: "/bin/zsh".to_string(),
        cwd: "/remote/repo".to_string(),
        status: "running".to_string(),
        cols: 120,
        rows: 36,
        started_at: "2026-06-26T00:00:00Z".to_string(),
        exited_at: None,
        exit_code: None,
        supports_panes: true,
        pane_count: 2,
    }];

    let mapped = map_remote_session_dtos("device-a", "remote:device-a:project-hash", items);

    assert_eq!(mapped[0].id, "remote:device-a:inner-session");
    assert_eq!(mapped[0].project_id, "remote:device-a:project-hash");
    assert_eq!(
        mapped[0].worktree_id.as_deref(),
        Some("remote:device-a:inner-worktree")
    );
}

/// Business Logic（为什么需要这个测试）:
///     remote rename 这类 session-id-only 返回 DTO 也必须恢复本机 remote shortcut projectId。
///
/// Code Logic（这个测试做什么）:
///     调用底层 session 映射 helper 并传入 local_project_id，断言 project_id 不退化为 inner project 的 remote entity。
#[test]
fn map_remote_session_dtos_with_project_uses_shortcut_for_remote_rename() {
    let items = vec![WorkbenchSessionDto {
        id: "inner-session".to_string(),
        project_id: "inner-project".to_string(),
        worktree_id: None,
        name: "Renamed".to_string(),
        name_source: "default".to_string(),
        command: "/bin/zsh".to_string(),
        cwd: "/remote/repo".to_string(),
        status: "running".to_string(),
        cols: 120,
        rows: 36,
        started_at: "2026-06-26T00:00:00Z".to_string(),
        exited_at: None,
        exit_code: None,
        supports_panes: true,
        pane_count: 1,
    }];

    let mapped = map_remote_session_dtos_with_project(
        "device-a",
        Some("remote:device-a:project-hash"),
        items,
    );

    assert_eq!(mapped[0].id, "remote:device-a:inner-session");
    assert_eq!(mapped[0].project_id, "remote:device-a:project-hash");
    assert_ne!(mapped[0].project_id, "remote:device-a:inner-project");
}

/// Business Logic（为什么需要这个测试）:
///     session-id-only terminal commands 必须确认 remote session 属于当前设备，避免把 A 设备会话输入写到 B 设备。
///
/// Code Logic（这个测试做什么）:
///     解析 device-a 的 session 成功，解析 device-b 的 session 返回稳定中文错误。
#[test]
fn remote_inner_session_id_validates_device() {
    let inner = remote_inner_session_id("device-a", "remote:device-a:inner-session").unwrap();
    assert_eq!(inner, "inner-session");

    let error = remote_inner_session_id("device-a", "remote:device-b:inner-session")
        .expect_err("device mismatch should be rejected");
    assert_eq!(error.to_string(), "远端 session 不属于当前设备");
}

/// Business Logic（为什么需要这个测试）:
///     本机收到远端 worktreeId 后必须确认它属于当前远端项目的设备，避免把 A 设备 ID 转发给 B 设备。
///
/// Code Logic（这个测试做什么）:
///     用 device-a 解析 device-b 的远端 worktree id，断言 helper 返回设备不匹配错误。
#[test]
fn remote_inner_worktree_id_rejects_device_mismatch() {
    let error = remote_inner_worktree_id(
        "device-a",
        Some("remote:device-b:inner-worktree".to_string()),
    )
    .expect_err("device mismatch should be rejected");

    assert_eq!(error.to_string(), "远端 worktree 不属于当前设备");
}

/// Business Logic（为什么需要这个测试）:
///     远端项目未指定 worktreeId 时表示使用远端主工作区，网关不应伪造或强制要求前端传 ID。
///
/// Code Logic（这个测试做什么）:
///     传入 None，断言 helper 也返回 None，供 remote client 发送空 worktreeId。
#[test]
fn remote_inner_worktree_id_allows_none_for_main_worktree() {
    let value = remote_inner_worktree_id("device-a", None).unwrap();

    assert!(value.is_none());
}

/// Business Logic（为什么需要这个测试）:
///     本机 worktree id 不能被误转发给远端设备，否则远端会在自己的数据库里查找不存在或错误的行。
///
/// Code Logic（这个测试做什么）:
///     传入未带 remote 前缀的本机 id，断言 helper 返回格式错误。
#[test]
fn remote_inner_worktree_id_rejects_unprefixed_local_id() {
    let error = remote_inner_worktree_id("device-a", Some("local-worktree-id".to_string()))
        .expect_err("unprefixed local id should be rejected");

    assert_eq!(error.to_string(), "远端 worktree ID 格式无效");
}

/// Business Logic（为什么需要这个测试）:
///     只接收 worktreeId 的命令必须先识别 remote worktree，否则会错误查询本机 SQLite 并报 NotFound。
///
/// Code Logic（这个测试做什么）:
///     调用命令目标解析 helper，断言 remote:<deviceId>:<inner> 被归类为远端目标并保留 inner worktreeId。
#[test]
fn worktree_command_target_routes_remote_id_before_local_repo_lookup() {
    let target = worktree_command_target("remote:device-a:inner-worktree").unwrap();

    assert_eq!(
        target,
        WorktreeCommandTarget::Remote {
            device_id: "device-a".to_string(),
            inner_worktree_id: "inner-worktree".to_string(),
        }
    );
}

/// Business Logic（为什么需要这个测试）:
///     远端 merge 命令返回值里的 worktreeId 必须仍是本机前端持有的 remote worktree id。
///
/// Code Logic（这个测试做什么）:
///     构造远端 merge JSON 返回值，经过映射 helper 后断言 worktreeId 加上设备前缀且阶段列表保持可反序列化。
#[test]
fn map_remote_merge_result_value_prefixes_worktree_id() {
    let value = serde_json::json!({
        "ok": true,
        "worktreeId": "inner-worktree",
        "stages": [
            {"id": "checkSource", "status": "completed", "message": "ok"}
        ]
    });

    let mapped = map_remote_merge_result_value("device-a", value).unwrap();

    assert!(mapped.ok);
    assert_eq!(mapped.worktree_id, "remote:device-a:inner-worktree");
    assert_eq!(mapped.stages.len(), 1);
    assert_eq!(mapped.stages[0].id, "checkSource");
}

/// Business Logic（为什么需要这个测试）:
///     保存命令必须按后端真实文件名判断能力，防止调用者把只读 CSV 伪装成 text 后覆盖。
///
/// Code Logic（这个测试做什么）:
///     直接校验保存类型 helper，断言 data.csv 被识别为 Csv 并拒绝文本保存。
#[test]
fn validate_save_file_type_rejects_csv_even_if_caller_wanted_text() {
    let error =
        validate_save_file_type("data.csv", "a,b\n1,2\n").expect_err("csv should be readonly");

    assert!(error.to_string().contains("不支持文本保存"));
}

/// Business Logic（为什么需要这个测试）:
///     JSON 配置保存前必须由后端做语义校验，避免写入无效配置导致项目工具链损坏。
///
/// Code Logic（这个测试做什么）:
///     以 .json 文件名触发结构化校验，断言非法 JSON 被拒绝。
#[test]
fn validate_save_file_type_rejects_invalid_json() {
    let error = validate_save_file_type("config.json", "{bad json")
        .expect_err("invalid json should be rejected");

    assert!(error.to_string().contains("JSON 格式无效"));
}

/// Business Logic（为什么需要这个测试）:
///     YAML 配置保存前必须由后端做语义校验，避免 Workbench 写入无效结构化配置。
///
/// Code Logic（这个测试做什么）:
///     以 .yaml 文件名触发结构化校验，断言非法 YAML 被拒绝。
#[test]
fn validate_save_file_type_rejects_invalid_yaml() {
    let error = validate_save_file_type("config.yaml", "name: [")
        .expect_err("invalid yaml should be rejected");

    assert!(error.to_string().contains("YAML"));
}

/// Business Logic（为什么需要这个测试）:
///     Markdown 是 Workbench 文件编辑器的可保存文本类型，应继续允许正常保存。
///
/// Code Logic（这个测试做什么）:
///     以 .md 文件名调用保存类型校验，断言返回 Markdown 且无错误。
#[test]
fn validate_save_file_type_allows_markdown() {
    let detected_type = validate_save_file_type("note.md", "# Note\n").expect("markdown ok");

    assert_eq!(detected_type, WorkbenchDetectedFileType::Markdown);
}

/// Business Logic（为什么需要这个测试）:
///     HTML 文件在 Workbench 中既要能源码编辑也要能渲染预览，因此保存命令必须允许写回原始源码。
///
/// Code Logic（这个测试做什么）:
///     以 .html 文件名调用保存类型校验，断言返回 Html 且不触发结构化格式化校验。
#[test]
fn validate_save_file_type_allows_html() {
    let detected_type =
        validate_save_file_type("index.html", "<!doctype html><title>Preview</title>")
            .expect("html ok");

    assert_eq!(detected_type, WorkbenchDetectedFileType::Html);
}

/// Business Logic（为什么需要这个测试）:
///     HTML 预览中的 `../assets/logo.png` 这类资源必须按当前 HTML 文件位置解析，并以内联 data URL 返回给前端 iframe。
///
/// Code Logic（这个测试做什么）:
///     构造 docs/page.html 与 assets/logo.png，调用 HTML 资源预览 helper，断言路径、MIME 和 data URL 正确。
#[test]
fn html_preview_asset_resolves_relative_to_document_path() {
    let root = std::env::temp_dir().join(format!("cc-partner-html-asset-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("docs")).expect("create docs");
    std::fs::create_dir_all(root.join("assets")).expect("create assets");
    std::fs::write(
        root.join("docs/page.html"),
        "<img src=\"../assets/logo.png\">",
    )
    .expect("write html");
    std::fs::write(root.join("assets/logo.png"), [0x89, b'P', b'N', b'G']).expect("write image");

    let asset = crate::workbench::html_assets::preview_html_asset(
        &root,
        "docs/page.html",
        "../assets/logo.png",
    )
    .expect("preview html asset");

    assert_eq!(asset.path, "assets/logo.png");
    assert_eq!(asset.mime, "image/png");
    assert!(asset.data_url.starts_with("data:image/png;base64,"));

    let _ = std::fs::remove_dir_all(root);
}

/// Business Logic（为什么需要这个测试）:
///     HTML 预览资源只能来自 worktree 根内相对文件，不能把 http/data/blob 或绝对路径交给后端读取。
///
/// Code Logic（这个测试做什么）:
///     构造一个 HTML 文档后，用多种非项目内资源路径调用 helper，断言全部被拒绝。
#[test]
fn html_preview_asset_rejects_external_urls_and_absolute_paths() {
    let root = std::env::temp_dir().join(format!("cc-partner-html-asset-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("docs")).expect("create docs");
    std::fs::write(
        root.join("docs/page.html"),
        "<link rel=\"stylesheet\" href=\"style.css\">",
    )
    .expect("write html");

    for asset_path in [
        "https://example.com/style.css",
        "data:text/css,body{}",
        "blob:https://example.com/id",
        "#local-fragment",
        "/etc/passwd",
        "\\\\server\\share\\secret.css",
        "\\windows-root\\secret.css",
        "C:\\Users\\hans\\secret.txt",
    ] {
        let error =
            crate::workbench::html_assets::preview_html_asset(&root, "docs/page.html", asset_path)
                .expect_err("unsafe asset path should be rejected");
        assert!(
            error.to_string().contains("相对路径") || error.to_string().contains("项目目录之外"),
            "unexpected error for {asset_path}: {error}"
        );
    }

    let _ = std::fs::remove_dir_all(root);
}

/// Business Logic（为什么需要这个测试）:
///     项目内 symlink 若指向 worktree 根外，HTML 预览不能跟随读取，否则会泄露用户磁盘其他文件。
///
/// Code Logic（这个测试做什么）:
///     在 Unix 上创建指向根外 CSS 文件的 symlink，断言 HTML 资源 helper 拒绝读取。
#[cfg(unix)]
#[test]
fn html_preview_asset_rejects_symlink_pointing_outside_root() {
    use std::os::unix::fs::symlink;

    let root = std::env::temp_dir().join(format!("cc-partner-html-asset-{}", uuid::Uuid::new_v4()));
    let outside = std::env::temp_dir().join(format!(
        "cc-partner-html-asset-outside-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(root.join("docs")).expect("create docs");
    std::fs::write(
        root.join("docs/page.html"),
        "<link rel=\"stylesheet\" href=\"leak.css\">",
    )
    .expect("write html");
    std::fs::write(&outside, "body{color:red}").expect("write outside");
    symlink(&outside, root.join("docs/leak.css")).expect("create symlink");

    let error =
        crate::workbench::html_assets::preview_html_asset(&root, "docs/page.html", "leak.css")
            .expect_err("outside symlink should be rejected");

    assert!(error.to_string().contains("项目目录之外"));

    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_file(outside);
}

/// Business Logic（为什么需要这个测试）:
///     Workbench AI commit 必须让 Claude 基于 staged diff 生成提交信息，而不是泛泛猜测。
///
/// Code Logic（这个测试做什么）:
///     构造 staged changes，断言生成指令包含 stat、diff、截断提示和只返回 commit message 的约束。
#[test]
fn commit_message_instruction_contains_staged_diff_and_output_contract() {
    let changes = workbench_git::StagedCommitChanges {
        stat: "README.md | 1 +".to_string(),
        diff: "+hello".to_string(),
        truncated: true,
    };

    let instruction = build_commit_message_instruction(&changes);

    assert!(instruction.contains("README.md | 1 +"));
    assert!(instruction.contains("+hello"));
    assert!(instruction.contains("diff 已被截断"));
    assert!(instruction.contains("Return only"));
    assert!(instruction.contains("message"));
}

/// Business Logic（为什么需要这个测试）:
///     Claude CLI 结构化输出必须稳定落到单个 commit message 字段，避免前端解析自由文本。
///
/// Code Logic（这个测试做什么）:
///     读取 schema JSON，断言 required 包含 message 且 message 类型为 string。
#[test]
fn commit_message_schema_requires_message_string() {
    let schema = workbench_commit_message_schema();

    assert_eq!(schema["required"][0], "message");
    assert_eq!(schema["properties"]["message"]["type"], "string");
}

/// Business Logic（为什么需要这个测试）:
///     Claude Code 自动解决 merge 冲突时，后端需要稳定 JSON 契约来接收完整文件内容。
///
/// Code Logic（这个测试做什么）:
///     读取 schema JSON，断言顶层 required files，且每个 item 必须包含 path/content。
#[test]
fn merge_conflict_resolution_schema_requires_files_with_content() {
    let schema = merge_conflict_resolution_schema();

    assert_eq!(schema["required"][0], "files");
    assert_eq!(schema["properties"]["files"]["type"], "array");
    assert_eq!(
        schema["properties"]["files"]["items"]["required"][0],
        "path"
    );
    assert_eq!(
        schema["properties"]["files"]["items"]["required"][1],
        "content"
    );
}

/// Business Logic（为什么需要这个测试）:
///     前端需要按 projectId 过滤 merge 进度事件，防止其他项目的后台 merge 污染当前 UI。
///
/// Code Logic（这个测试做什么）:
///     构造事件 payload 并序列化为 JSON，断言 serde camelCase 输出包含 projectId/worktreeId。
#[test]
fn merge_progress_event_serializes_project_id_for_frontend_filtering() {
    let event = WorkbenchMergeProgressEvent {
        project_id: "project-1".to_string(),
        worktree_id: "worktree-1".to_string(),
        stage: WorkbenchMergeStageDto {
            id: MERGE_STAGE_CHECK_SOURCE.to_string(),
            status: "running".to_string(),
            message: "checking".to_string(),
        },
    };

    let value = serde_json::to_value(event).expect("serialize event");

    assert_eq!(value["projectId"], "project-1");
    assert_eq!(value["worktreeId"], "worktree-1");
    assert_eq!(value["stage"]["id"], MERGE_STAGE_CHECK_SOURCE);
}

/// Business Logic（为什么需要这个测试）:
///     Claude Code 需要看到每个冲突文件的相对路径和带 conflict marker 的原文，
///     才能返回可直接写回的解决后完整内容。
///
/// Code Logic（这个测试做什么）:
///     构造冲突文件输入，断言 prompt 包含路径、内容和禁止保留 conflict marker 的约束。
#[test]
fn merge_conflict_instruction_contains_files_and_output_contract() {
    let files = vec![MergeConflictFileInput {
        path: "README.md".to_string(),
        content: "<<<<<<< HEAD\nmain\n=======\nfeature\n>>>>>>> branch\n".to_string(),
    }];

    let instruction = build_merge_conflict_resolution_instruction(&files);

    assert!(instruction.contains("README.md"));
    assert!(instruction.contains("<<<<<<< HEAD"));
    assert!(instruction.contains("Return only"));
    assert!(instruction.contains("files"));
    assert!(instruction.contains("Do not leave conflict markers"));
    assert!(instruction.contains("|||||||"));
}

/// Business Logic（为什么需要这个测试）:
///     Claude 输出的 path 来自模型，后端写文件前必须防止绝对路径或 `..` 越界覆盖用户其他文件。
///
/// Code Logic（这个测试做什么）:
///     校验相对普通路径可用，绝对路径和父目录路径被拒绝。
#[test]
fn validate_merge_resolution_path_rejects_unsafe_paths() {
    assert!(validate_merge_resolution_path("src/lib.rs").is_ok());
    assert!(validate_merge_resolution_path("/tmp/evil").is_err());
    assert!(validate_merge_resolution_path("../evil").is_err());
}

/// Business Logic（为什么需要这个测试）:
///     自动冲突解决会写回 Claude Code 生成的文件内容，必须保证普通相对路径仍解析在 worktree 内。
///
/// Code Logic（这个测试做什么）:
///     构造临时根目录和普通文件，断言 safe_merge_resolution_path 返回 root 下路径。
#[test]
fn safe_merge_resolution_path_accepts_normal_path_inside_root() {
    let root = std::env::temp_dir().join(format!("cc-partner-safe-merge-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("src")).expect("create test root");
    std::fs::write(root.join("src/lib.rs"), "fn main() {}\n").expect("write file");

    let resolved = safe_merge_resolution_path(&root, "src/lib.rs").expect("resolve path");

    assert_eq!(resolved, root.canonicalize().unwrap().join("src/lib.rs"));

    let _ = std::fs::remove_dir_all(root);
}

/// Business Logic（为什么需要这个测试）:
///     冲突文件若是 symlink，直接写回会跟随链接覆盖工作区外文件，自动流程必须拒绝。
///
/// Code Logic（这个测试做什么）:
///     在 Unix 上创建指向外部文件的 symlink，断言 safe_merge_resolution_path 拒绝该路径。
#[cfg(unix)]
#[test]
fn safe_merge_resolution_path_rejects_symlink_file() {
    use std::os::unix::fs::symlink;

    let root = std::env::temp_dir().join(format!("cc-partner-safe-merge-{}", uuid::Uuid::new_v4()));
    let outside = std::env::temp_dir().join(format!(
        "cc-partner-safe-merge-outside-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).expect("create test root");
    std::fs::write(&outside, "outside\n").expect("write outside");
    symlink(&outside, root.join("conflicted.txt")).expect("create symlink");

    let error = safe_merge_resolution_path(&root, "conflicted.txt")
        .expect_err("symlink should be rejected");

    assert!(error.to_string().contains("符号链接"));

    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_file(outside);
}

/// Business Logic（为什么需要这个测试）:
///     Git 只要 `git add` 就可能把仍含 conflict marker 的文本标为已解决，后端必须先拦截。
///
/// Code Logic（这个测试做什么）:
///     断言常见 conflict marker 行会被识别，普通 Markdown 分隔线不会误判。
#[test]
fn content_has_conflict_markers_detects_git_markers() {
    assert!(content_has_conflict_markers("<<<<<<< HEAD\nx\n"));
    assert!(content_has_conflict_markers("||||||| base\nx\n"));
    assert!(content_has_conflict_markers("=======\n"));
    assert!(content_has_conflict_markers(">>>>>>> feature\n"));
    assert!(!content_has_conflict_markers("title\n---\nbody\n"));
}

/// Business Logic（为什么需要这个测试）:
///     用户选择已有 Git 项目后，Workbench 顶部必须自动显示磁盘上已有的 Git worktree。
///
/// Code Logic（这个测试做什么）:
///     构造 `git worktree list` 解析项，断言导入 row 使用稳定 id、分支名和路径。
#[test]
fn discovered_git_worktree_row_uses_stable_metadata() {
    let project = WorkbenchProjectRow {
        id: "project-1".to_string(),
        name: "Repo".to_string(),
        kind: "local".to_string(),
        device_id: "local".to_string(),
        device_name: "Mac".to_string(),
        path: "/repo/main".to_string(),
        last_opened_at: "2026-06-26T00:00:00Z".to_string(),
        created_at: "2026-06-26T00:00:00Z".to_string(),
        updated_at: "2026-06-26T00:00:00Z".to_string(),
    };
    let parsed = workbench_git::ParsedWorktree {
        path: "/repo/worktrees/feature-a".to_string(),
        branch: Some("feature/a".to_string()),
        is_main: false,
    };

    let first = discovered_git_worktree_row(&project, &parsed, None, "2026-06-26T01:00:00Z");
    let second = discovered_git_worktree_row(&project, &parsed, None, "2026-06-26T02:00:00Z");

    assert_eq!(first.id, second.id);
    assert_eq!(first.project_id, "project-1");
    assert_eq!(first.name, "feature/a");
    assert_eq!(first.branch.as_deref(), Some("feature/a"));
    assert_eq!(first.path, "/repo/worktrees/feature-a");
    assert!(!first.is_main);
}

/// Business Logic（为什么需要这个测试）:
///     已经由 cc-partner 创建过的 worktree 再次被 Git 发现时不能换 id，否则会重复显示。
///
/// Code Logic（这个测试做什么）:
///     构造相同 path 的既有 row，断言导入时复用既有 id 和 created_at。
#[test]
fn discovered_git_worktree_row_reuses_existing_row_for_same_path() {
    let project = WorkbenchProjectRow {
        id: "project-1".to_string(),
        name: "Repo".to_string(),
        kind: "local".to_string(),
        device_id: "local".to_string(),
        device_name: "Mac".to_string(),
        path: "/repo/main".to_string(),
        last_opened_at: "2026-06-26T00:00:00Z".to_string(),
        created_at: "2026-06-26T00:00:00Z".to_string(),
        updated_at: "2026-06-26T00:00:00Z".to_string(),
    };
    let existing = WorkbenchWorktreeRow {
        id: "existing-row".to_string(),
        project_id: "project-1".to_string(),
        name: "old name".to_string(),
        branch: Some("old".to_string()),
        base_branch: Some("main".to_string()),
        path: "/repo/worktrees/feature-a".to_string(),
        is_main: false,
        created_at: "2026-06-25T00:00:00Z".to_string(),
        updated_at: "2026-06-25T00:00:00Z".to_string(),
    };
    let parsed = workbench_git::ParsedWorktree {
        path: "/repo/worktrees/feature-a/".to_string(),
        branch: Some("feature/a".to_string()),
        is_main: false,
    };

    let row =
        discovered_git_worktree_row(&project, &parsed, Some(&existing), "2026-06-26T01:00:00Z");

    assert_eq!(row.id, "existing-row");
    assert_eq!(row.created_at, "2026-06-25T00:00:00Z");
    assert_eq!(row.updated_at, "2026-06-26T01:00:00Z");
    assert_eq!(row.name, "feature/a");
    assert_eq!(row.branch.as_deref(), Some("feature/a"));
    assert_eq!(row.path, "/repo/worktrees/feature-a");
}

/// Business Logic（为什么需要这个测试）:
///     Agent Hub project opt-in 依赖稳定的 Git remote fingerprint。
///
/// Code Logic（这个测试做什么）:
///     断言 workbench::projects::normalize_git_remote_fingerprint 规范化 https URL。
#[test]
fn agent_hub_git_remote_fingerprint_helper_is_stable() {
    use crate::workbench::projects::normalize_git_remote_fingerprint;
    assert_eq!(
        normalize_git_remote_fingerprint("https://GitHub.com/Org/Repo.git/"),
        "https://github.com/org/repo"
    );
}
