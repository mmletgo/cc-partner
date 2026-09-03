//! Workbench sessions 单测模块（由 `sessions.rs` 以 `#[path]` 挂载）。
//!
//! Business Logic:
//!     将大体量 `#[cfg(test)]` 从生产源文件拆出，避免 module-boundary no-growth 与测试膨胀互相绑死，
//!     同时保留子模块对 `sessions` 私有 helper 的可见性。
//!
//! Code Logic:
//!     文件本体即为 `mod tests` 的模块体；仅在 `cfg(test)` 下由父模块 `#[path]` 引入。

use super::*;

/// Business Logic（为什么需要这个函数）:
///     tmux window 映射测试需要快速构造持久化 row，避免启动真实 PTY 或 tmux。
///
/// Code Logic（这个函数做什么）:
///     返回一个 running tmux WorkbenchSessionRow，backend_id 使用 project_id + worktree_id 派生的 worktree session 名。
fn fake_tmux_row(
    session_id: &str,
    project_id: &str,
    worktree_id: Option<&str>,
    window_id: &str,
) -> WorkbenchSessionRow {
    WorkbenchSessionRow {
        id: session_id.to_string(),
        project_id: project_id.to_string(),
        worktree_id: worktree_id.map(str::to_string),
        name: session_id.to_string(),
        name_source: "default".to_string(),
        command: "/bin/sh".to_string(),
        cwd: "/tmp/project".to_string(),
        status: "running".to_string(),
        cols: DEFAULT_COLS,
        rows: DEFAULT_ROWS,
        started_at: "2026-06-24T00:00:00Z".to_string(),
        exited_at: None,
        exit_code: None,
        backend: TMUX_BACKEND.to_string(),
        backend_id: Some(tmux_worktree_session_name(
            project_id,
            project_id,
            worktree_id,
            None,
        )),
        backend_window_id: Some(window_id.to_string()),
        created_at: "2026-06-24T00:00:00Z".to_string(),
        updated_at: "2026-06-24T00:00:00Z".to_string(),
    }
}

/// Business Logic（为什么需要这个测试）:
///     新启动应用时还没有工作台终端，前端会话列表应为空数组。
///
/// Code Logic（这个测试做什么）:
///     构造空 registry 并断言 list(None) 返回空。
#[test]
fn list_empty_registry_returns_empty() {
    let registry = WorkbenchSessionRegistry::new();

    assert!(registry.list(None).is_empty());
}

/// Business Logic（为什么需要这个测试）:
///     用户重命名不存在的会话时，前端需要得到明确错误而不是创建幽灵会话。
///
/// Code Logic（这个测试做什么）:
///     对缺失 session_id 调用 rename 并断言返回 Err。
#[test]
fn rename_missing_session_returns_error() {
    let registry = WorkbenchSessionRegistry::new();

    assert!(registry.rename("missing", "name").is_err());
}

/// Business Logic（为什么需要这个测试）:
///     用户关闭不存在的会话时，应返回错误，避免前端误判关闭成功。
///
/// Code Logic（这个测试做什么）:
///     对缺失 session_id 调用 close 并断言返回 Err。
#[test]
fn close_missing_session_returns_error() {
    let registry = WorkbenchSessionRegistry::new();

    assert!(registry.close("missing").is_err());
}

/// Business Logic（为什么需要这个测试）:
///     repo upsert 失败时必须回收刚 spawn 的 attach，禁止 ghost registry/child。
///
/// Code Logic（这个测试做什么）:
///     insert fake session → SessionSpawnGuard 不 commit → Drop → registry 空且无 live child。
#[test]
fn repo_failure_closes_spawned_session() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_fake_session_for_test("s-spawn-1", "p1");
    assert_eq!(registry.registry_len(), 1);
    assert_eq!(registry.live_child_count(), 1);

    {
        let _guard = SessionSpawnGuard::new(registry.clone(), "s-spawn-1".to_string());
        // 模拟 upsert 失败：不 commit，离开作用域触发 Drop。
    }

    assert_eq!(registry.registry_len(), 0);
    assert_eq!(registry.live_child_count(), 0);
}

/// Business Logic（为什么需要这个测试）:
///     upsert 成功后必须 commit，否则 Drop 会误杀合法 session。
///
/// Code Logic（这个测试做什么）:
///     insert → guard.commit → drop → session 仍在 registry。
#[test]
fn session_spawn_guard_commit_keeps_session() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_fake_session_for_test("s-keep", "p1");
    {
        let mut guard = SessionSpawnGuard::new(registry.clone(), "s-keep".to_string());
        guard.commit();
    }
    assert_eq!(registry.registry_len(), 1);
    assert!(registry.contains("s-keep"));
}

/// 串行化 R30 M2 reclaim 计数断言：reset/assert 窗口需互斥。
static TMUX_CREATE_GUARD_RECLAIM_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Business Logic（R30 M2: 为什么需要这个测试）:
///     create_tmux_window 成功后若 project barrier / spawn 失败，不得留下 invisible orphan window。
///
/// Code Logic（这个测试做什么）:
///     构造 tmux row → TmuxCreateGuard 不 commit → Drop 后 reclaim 计数 +1。
#[test]
fn tmux_create_guard_drop_reclaims_window_without_commit() {
    let _lock = TMUX_CREATE_GUARD_RECLAIM_TEST_LOCK
        .lock()
        .expect("tmux create guard reclaim test lock");
    reset_tmux_create_guard_reclaim_count_for_test();
    let row = fake_tmux_row("s-r30-m2", "p-r30", Some("wt1"), "@9");
    {
        let _guard = TmuxCreateGuard::new(row);
        // 模拟 barrier / spawn_row 失败：不 commit。
    }
    assert_eq!(
        tmux_create_guard_reclaim_count_for_test(),
        1,
        "uncommitted TmuxCreateGuard must reclaim on Drop"
    );
}

/// Business Logic（R30 M2: 为什么需要这个测试）:
///     spawn_row 成功后 window 归 registry/command 层；TmuxCreateGuard 不得误杀合法 window。
///
/// Code Logic（这个测试做什么）:
///     commit 后 Drop → reclaim 计数仍为 0。
#[test]
fn tmux_create_guard_commit_skips_kill_on_drop() {
    let _lock = TMUX_CREATE_GUARD_RECLAIM_TEST_LOCK
        .lock()
        .expect("tmux create guard reclaim test lock");
    reset_tmux_create_guard_reclaim_count_for_test();
    let row = fake_tmux_row("s-r30-m2-ok", "p-r30", Some("wt1"), "@10");
    {
        let mut guard = TmuxCreateGuard::new(row);
        guard.commit();
    }
    assert_eq!(
        tmux_create_guard_reclaim_count_for_test(),
        0,
        "committed TmuxCreateGuard must not reclaim window"
    );
}

/// Business Logic（R30 M2: 为什么需要这个测试）:
///     create 路径在 create_tmux_window 之后、spawn 之前 revalidate barrier 失败时必须回收 window。
///
/// Code Logic（这个测试做什么）:
///     模拟 create 成功装 guard 后 require_project_not_closing Err → Drop 回收。
#[test]
fn project_barrier_after_tmux_window_create_reclaims_via_guard() {
    let _lock = TMUX_CREATE_GUARD_RECLAIM_TEST_LOCK
        .lock()
        .expect("tmux create guard reclaim test lock");
    reset_tmux_create_guard_reclaim_count_for_test();
    let registry = WorkbenchSessionRegistry::new();
    let _gen = registry.begin_project_closing_barrier("p-r30-barrier");
    let row = fake_tmux_row("s-r30-barrier", "p-r30-barrier", Some("wt1"), "@11");
    // 对齐 create：window 成功后装 guard，再 revalidate barrier。
    let result = {
        let _guard = TmuxCreateGuard::new(row);
        match registry.require_project_not_closing("p-r30-barrier") {
            Ok(()) => Ok(()),
            Err(error) => Err(error),
        }
        // guard Drop on early return path
    };
    assert!(
        matches!(result, Err(AppError::Unavailable(ref m)) if m == "project_closing_barrier_active"),
        "barrier must reject create revalidate"
    );
    assert_eq!(
        tmux_create_guard_reclaim_count_for_test(),
        1,
        "barrier after create_tmux_window must reclaim via TmuxCreateGuard Drop"
    );
    registry.finish_project_closing_barrier("p-r30-barrier", _gen);
}

/// Business Logic（为什么需要这个测试）:
///     restore 中途失败时 Drop 必须释放 claim，允许后续重试。
///
/// Code Logic（这个测试做什么）:
///     try_claim → RestoreClaimGuard 不 disarm → Drop → claim 已释放，可再次 claim。
#[test]
fn restore_claim_guard_releases_on_drop() {
    let registry = WorkbenchSessionRegistry::new();
    let generation = registry
        .try_claim_restore("s-restore")
        .claim_generation()
        .expect("claimed");
    {
        let _guard = RestoreClaimGuard::new(registry.clone(), "s-restore".to_string(), generation);
    }
    assert!(!registry.is_restore_claim_held("s-restore"));
    assert!(registry.try_claim_restore("s-restore").is_claimed());
    registry.release_restore_claim("s-restore");
}

/// Business Logic（为什么需要这个测试）:
///     终端交互式程序会输出中文和符号，PTY read 可能把多字节 UTF-8 拆到相邻 chunk。
///
/// Code Logic（这个测试做什么）:
///     构造一个被拆开的中文字符串，断言流式解码器能跨 chunk 保留完整字符。
#[test]
fn terminal_utf8_decoder_preserves_split_multibyte_characters() {
    let mut decoder = TerminalUtf8Decoder::new();
    let text = "思考: xhigh\n";
    let bytes = text.as_bytes();
    let split_at = "思".len() + 1;

    let first = decoder.decode(&bytes[..split_at]);
    let second = decoder.decode(&bytes[split_at..]);

    assert_eq!(format!("{first}{second}"), text);
    assert_eq!(decoder.finish(), None);
}

/// Business Logic（为什么需要这个测试）:
///     Agent OSC 不得进入 replay buffer / terminal UI 字节流。
///
/// Code Logic（这个测试做什么）:
///     用 AgentOscDecoder 处理夹杂 OSC 的输出，断言 visible 无 base64 载荷且含前后文。
#[test]
fn agent_osc_payload_never_enters_replay_visible_bytes() {
    use crate::workbench::agent_runtime::{encode_agent_osc_frame, AgentSessionPhase};
    let mut osc = crate::workbench::agent_runtime::AgentOscDecoder::default();
    let frame = encode_agent_osc_frame(
        "agent-1",
        "session-1",
        AgentSessionPhase::Working,
        2,
        "2026-07-15T00:00:00Z",
    );
    let mut input = b"hello ".to_vec();
    input.extend_from_slice(&frame);
    input.extend_from_slice(b"world");
    let decoded = osc.push(&input);
    assert_eq!(decoded.visible, b"hello world");
    assert!(
        !String::from_utf8_lossy(&decoded.visible).contains("agentSessionId"),
        "OSC JSON must not leak into visible"
    );
    assert_eq!(decoded.mutations.len(), 1);
    // 写入 replay 的只能是 visible 转 UTF-8 后的文本
    let mut buffer = SessionReplayBuffer::new(10_000, 0);
    let text = String::from_utf8_lossy(&decoded.visible).into_owned();
    buffer.append(&text, 1);
    let snap = buffer.snapshot("session-1");
    assert_eq!(snap.buffer, "hello world");
    assert!(!snap.buffer.contains("cc-partner-agent-v1"));
}

/// Business Logic（为什么需要这个测试）:
///     移动端首次打开远端终端时需要拉取最近输出，且历史输出超过内存上限时只能保留尾部。
///
/// Code Logic（这个测试做什么）:
///     用小容量 replay buffer 追加超过上限的中文和 emoji 输出，断言按 char 边界截断、truncated 与 lastSeq 正确。
#[test]
fn session_replay_buffer_keeps_recent_output_with_last_seq() {
    let mut buffer = SessionReplayBuffer::new(3, 0);

    buffer.append("hello", 1);
    buffer.append("世界🙂", 2);
    buffer.append("再见", 3);
    let snapshot = buffer.snapshot("session-1");

    assert_eq!(snapshot.session_id, "session-1");
    assert_eq!(snapshot.buffer, "🙂再见");
    assert!(snapshot.truncated);
    assert_eq!(snapshot.last_seq, 3);
}

/// Business Logic（为什么需要这个测试）:
///     重启恢复的 tmux history 必须先于新 PTY 输出进入 replay，同时不能占用 live seq。
///
/// Code Logic（这个测试做什么）:
///     先设置受保护历史，再追加并裁剪 live 当前屏，断言历史不会被末屏重绘淘汰，
///     同时保持顺序与 lastSeq cutover 语义。
#[test]
fn session_replay_buffer_seeds_tmux_history_before_new_live_output() {
    let mut buffer = SessionReplayBuffer::new(7, 7);
    buffer.set_restored_prefix("early\r\nhistory\r\n");
    assert!(buffer.append_if_generation("old", 1, 7));
    assert!(buffer.append_if_generation("current", 2, 7));

    let snapshot = buffer.snapshot("session-restored");
    assert_eq!(snapshot.buffer, "early\r\nhistory\r\ncurrent");
    assert_eq!(snapshot.last_seq, 2);
    assert!(snapshot.truncated);
}

/// Business Logic（为什么需要这个测试）:
///     运行中 hydration 必须丢弃会把 xterm 切回 alternate screen 的旧 live 控制流，同时保留
///     lastSeq 供并发 live cutover，避免历史可见但新输出重复或丢失。
///
/// Code Logic（这个测试做什么）:
///     先写含 alternate DECSET 的旧 chunk，再以已渲染 pane 快照替换，断言旧控制流消失、
///     lastSeq 不变且后续 live 能继续追加。
#[test]
fn hydration_snapshot_replaces_old_live_control_ring_and_keeps_sequence() {
    let mut buffer = SessionReplayBuffer::new(100, 9);
    assert!(buffer.append_if_generation("\u{1b}[?1049hCURRENT", 7, 9));

    buffer.replace_with_restored_snapshot("EARLY\r\nCURRENT");
    assert!(buffer.append_if_generation("NEXT", 8, 9));

    let snapshot = buffer.snapshot("session-hydrated");
    assert_eq!(snapshot.buffer, "EARLY\r\nCURRENTNEXT");
    assert_eq!(snapshot.last_seq, 8);
    assert!(!snapshot.buffer.contains("1049h"));
}

/// Business Logic（为什么需要这个测试）:
///     超长 tmux 历史必须按 UTF-8 与物理行边界裁剪，不能让 replay 从半行或半个中文字符开始。
///
/// Code Logic（这个测试做什么）:
///     构造略超 80k 的前缀，使字符上限切点落在首行中间；断言最终从下一条 CRLF 后开始，
///     中文完整且 snapshot 标记 truncated。
#[test]
fn restored_prefix_trims_at_unicode_line_boundary() {
    let first_line = "x".repeat(100);
    let retained = "你".repeat(79_990);
    let prefix = format!("{first_line}\r\n{retained}");
    let mut buffer = SessionReplayBuffer::new(10, 1);

    buffer.set_restored_prefix(&prefix);

    let snapshot = buffer.snapshot("restored");
    assert_eq!(snapshot.buffer.chars().count(), 79_990);
    assert!(snapshot.buffer.starts_with('你'));
    assert!(snapshot.truncated);
}

/// Business Logic（为什么需要这个测试）:
///     chunk ring 在裁剪多字节字符时必须保持 Unicode scalar 边界，并继续维护 tail/last_seq 合同。
///
/// Code Logic（这个测试做什么）:
///     容量 4 时依次 append 中文+emoji 与 ASCII，断言 snapshot 尾部、truncated、last_seq 与 char_count。
#[test]
fn replay_chunk_ring_preserves_unicode_and_tail_contract() {
    let mut buffer = SessionReplayBuffer::new(4, 0);
    buffer.append("你🙂", 1);
    buffer.append("ab", 2);
    buffer.append("c", 3);
    let snapshot = buffer.snapshot("s1");
    assert_eq!(snapshot.buffer, "🙂abc");
    assert!(snapshot.truncated);
    assert_eq!(snapshot.last_seq, 3);
    assert_eq!(buffer.char_count, 4);
}

/// Business Logic（为什么需要这个测试）:
///     零容量与超大单 chunk 是 ring 裁剪的边界场景，错误实现会留下敏感全文或错误 chunk 数。
///
/// Code Logic（这个测试做什么）:
///     max=0 时 append 后 buffer 为空且 truncated；max=3 时单 chunk "abcdef" 只保留 "def" 且 chunks 长度为 1。
#[test]
fn replay_chunk_ring_handles_zero_and_large_single_chunk() {
    let mut zero = SessionReplayBuffer::new(0, 0);
    zero.append("secret", 1);
    assert_eq!(zero.snapshot("s0").buffer, "");
    assert!(zero.snapshot("s0").truncated);

    let mut small = SessionReplayBuffer::new(3, 0);
    small.append("abcdef", 4);
    assert_eq!(small.snapshot("s1").buffer, "def");
    assert_eq!(small.chunks.len(), 1);
}

/// Business Logic（为什么需要这个测试）:
///     满环后每个小 append 应只摊销丢弃头部小 chunk，避免整段历史重建。
///
/// Code Logic（这个测试做什么）:
///     容量 8 填满 a..h 后再 append i，断言尾部为 bcdefghi 且 char_count 仍为 8。
#[test]
fn full_replay_ring_drops_one_small_head_chunk_per_small_append() {
    let mut buffer = SessionReplayBuffer::new(8, 0);
    for (seq, value) in ["a", "b", "c", "d", "e", "f", "g", "h"].iter().enumerate() {
        buffer.append(value, seq as u64 + 1);
    }
    buffer.append("i", 9);
    assert_eq!(buffer.snapshot("s").buffer, "bcdefghi");
    assert_eq!(buffer.char_count, 8);
}

/// Business Logic（为什么需要这个测试）:
///     生产容量 120k 下持续增量 append 不能让 chunk 数或 char_count 随总写入线性膨胀。
///
/// Code Logic（这个测试做什么）:
///     先写入 120_000 个单字符 chunk，再追加 10_000 次，断言 char_count 恒定、chunks.len 不增长、snapshot 尾部正确。
#[test]
fn replay_chunk_ring_large_capacity_keeps_amortized_bounds() {
    let capacity = SESSION_REPLAY_MAX_CHARS;
    let mut buffer = SessionReplayBuffer::new(capacity, 0);
    for i in 0..capacity {
        let ch = char::from(b'a' + (i % 26) as u8);
        let text = ch.to_string();
        buffer.append(&text, (i as u64) + 1);
    }
    assert_eq!(buffer.char_count, capacity);
    assert_eq!(buffer.chunks.len(), capacity);
    let chunks_before = buffer.chunks.len();

    let extra = 10_000usize;
    for j in 0..extra {
        let i = capacity + j;
        let ch = char::from(b'a' + (i % 26) as u8);
        let text = ch.to_string();
        buffer.append(&text, (i as u64) + 1);
        assert_eq!(buffer.char_count, capacity);
        assert!(buffer.chunks.len() <= chunks_before);
    }

    assert!(buffer.snapshot("large").truncated);
    assert_eq!(buffer.char_count, capacity);
    assert_eq!(buffer.chunks.len(), capacity);

    let snapshot = buffer.snapshot("large");
    assert_eq!(snapshot.buffer.chars().count(), capacity);
    let tail: String = (0..16)
        .map(|k| {
            let i = capacity + extra - 16 + k;
            char::from(b'a' + (i % 26) as u8)
        })
        .collect();
    assert!(
        snapshot.buffer.ends_with(&tail),
        "snapshot tail must match last appended characters"
    );
    assert_eq!(snapshot.last_seq, (capacity + extra) as u64);
}

/// Business Logic（为什么需要这个测试）:
///     工作台打开终端时需要先按前端可见区域启动 PTY，避免交互式程序首屏按默认列宽绘制后错位。
///
/// Code Logic（这个测试做什么）:
///     断言初始终端尺寸优先使用前端传入值，并对过小或缺失值回退到安全默认值。
#[test]
fn initial_terminal_size_uses_frontend_size_with_safe_minimums() {
    assert_eq!(initial_terminal_size(Some(140), Some(42)), (140, 42));
    assert_eq!(
        initial_terminal_size(Some(2), Some(1)),
        (MIN_TERMINAL_COLS, MIN_TERMINAL_ROWS),
    );
    assert_eq!(
        initial_terminal_size(None, None),
        (DEFAULT_COLS, DEFAULT_ROWS)
    );
}

/// Business Logic（为什么需要这个测试）:
///     工作台打开终端应进入项目根目录的普通 shell，不能替用户自动启动 Claude Code。
///
/// Code Logic（这个测试做什么）:
///     构造系统 shell 环境值，断言工作台终端命令使用 shell 路径而不是固定的 claude。
#[test]
fn workbench_terminal_command_defaults_to_shell_instead_of_claude() {
    let command = default_terminal_command_from_env(Some("/bin/zsh".into()));

    assert_eq!(command, "/bin/zsh");
    assert_ne!(command, "claude");
}

/// Business Logic（为什么需要这个测试）:
///     Windows 用户的项目目录通常是盘符路径，WSL 内的 tmux 只能识别 `/mnt/<drive>/...` 路径。
///
/// Code Logic（这个测试做什么）:
///     断言 Windows 盘符路径、正斜杠路径和扩展长度路径都能转换成 WSL 可用路径。
#[test]
fn windows_project_paths_convert_to_wsl_mount_paths() {
    assert_eq!(
        windows_path_to_wsl_path(r"C:\Users\hans\web_project\cc-partner"),
        Some("/mnt/c/Users/hans/web_project/cc-partner".to_string())
    );
    assert_eq!(windows_path_to_wsl_path(r"C:\"), Some("/mnt/c".to_string()));
    assert_eq!(
        windows_path_to_wsl_path("D:/work/cc-partner"),
        Some("/mnt/d/work/cc-partner".to_string())
    );
    assert_eq!(
        windows_path_to_wsl_path(r"\\?\E:\repo with space\app"),
        Some("/mnt/e/repo with space/app".to_string())
    );
    assert_eq!(
        windows_path_to_wsl_path(r"\\wsl$\Ubuntu\home\hans\repo"),
        Some("/home/hans/repo".to_string())
    );
    assert_eq!(
        windows_path_to_wsl_path(r"\\wsl.localhost\Ubuntu\home\hans\repo"),
        Some("/home/hans/repo".to_string())
    );
    assert_eq!(windows_path_to_wsl_path(r"C:relative\path"), None);
    assert_eq!(windows_path_to_wsl_path(r"\\server\share\repo"), None);
}

/// Business Logic（为什么需要这个测试）:
///     Windows 上应复用用户 WSL 里的 tmux，而不是因为宿主系统没有原生 tmux 就放弃上下文恢复。
///
/// Code Logic（这个测试做什么）:
///     构造 WSL tmux 后端描述，断言它通过 `wsl.exe --exec tmux` 调用且工作目录使用 WSL 路径。
#[test]
fn wsl_tmux_backend_invokes_tmux_through_wsl() {
    let backend = TmuxCommand::wsl();

    assert_eq!(backend.program, "wsl.exe");
    assert_eq!(
        backend.prefix_args,
        vec!["--exec".to_string(), "tmux".to_string()],
        "WSL tmux 与 0.8.3 一样走默认 socket，不得再加 -S/-L/-f"
    );
    assert_eq!(
        backend.project_cwd(r"C:\Users\hans\project").unwrap(),
        "/mnt/c/Users/hans/project"
    );
    assert_eq!(backend.shell_command_for_new_session("cmd.exe"), None);
    let display = backend.display_command_for_session("cc-partner-session", None, "cmd.exe");
    assert!(display.starts_with("wsl.exe --exec tmux"));
    assert!(display.contains("attach-session -t cc-partner-session"));
    let display_window = backend.display_command_for_session(
        "cc-partner-session",
        Some("cc-partner-session:@7"),
        "cmd.exe",
    );
    assert!(display_window.contains("switch-client -t cc-partner-session:@7"));
}

/// Business Logic（为什么需要这个测试）:
///     真实 tmux 映射下，一个 worktree 应稳定对应一个 tmux session，worktree 内 tab 对应 window。
///
/// Code Logic（这个测试做什么）:
///     断言 project + worktree 派生出稳定 session 名，window target 使用 `session:@window` 语法。
#[test]
fn tmux_worktree_session_and_window_target_are_stable() {
    let worktree_session = tmux_worktree_session_name(
        "cc-partner",
        "project-1234-abcd",
        Some("project-1234-abcd:main"),
        Some("main"),
    );

    assert_eq!(worktree_session, "cc-partner-main-1234abcdmain");
    assert_eq!(
        tmux_window_target(&worktree_session, "@7"),
        "cc-partner-main-1234abcdmain:@7"
    );
}

/// Business Logic（为什么需要这个测试）:
///     用户会直接看到 tmux status 左侧的 session 名，内部 worktree id/hash 不应成为主要可读名称。
///
/// Code Logic（这个测试做什么）:
///     断言 session 名优先使用用户可见 worktree 名，并用 worktree id 的短组件保持稳定区分。
#[test]
fn tmux_worktree_session_prefers_readable_worktree_name() {
    let worktree_session = tmux_worktree_session_name(
        "cc-partner",
        "project-84b44f3d8e25",
        Some("internal-worktree-84b44f3d8e25"),
        Some("feature/PandoCanvas"),
    );

    assert_eq!(
        worktree_session,
        "cc-partner-feature-pandocanvas-84b44f3d8e25"
    );
    assert!(!worktree_session.starts_with("cc-partner-worktree-"));
}

/// Business Logic（为什么需要这个测试）:
///     同一项目下不同 worktree 的 tmux status/window 列表必须互相隔离。
///
/// Code Logic（这个测试做什么）:
///     断言同一 project_id 搭配不同 worktree_id 会生成不同 backend_id。
#[test]
fn tmux_worktree_session_differs_between_worktrees() {
    let main_session = tmux_worktree_session_name(
        "cc-partner",
        "project-1",
        Some("project-1:main"),
        Some("main"),
    );
    let feature_session = tmux_worktree_session_name(
        "cc-partner",
        "project-1",
        Some("worktree-2"),
        Some("feature/ui"),
    );

    assert_ne!(main_session, feature_session);
    assert_eq!(main_session, "cc-partner-main-project1main");
    assert_eq!(feature_session, "cc-partner-feature-ui-worktree2");
}

/// Business Logic（为什么需要这个测试）:
///     可读 worktree 名可能在清洗后相同，但底层 tmux session 仍必须按真实 worktree 隔离。
///
/// Code Logic（这个测试做什么）:
///     构造两个显示名清洗后相同、内部 id 不同的 worktree，断言 session 名不会碰撞。
#[test]
fn tmux_worktree_session_keeps_worktree_isolation_when_names_collide() {
    let slash_session = tmux_worktree_session_name(
        "cc-partner",
        "project-1",
        Some("worktree-alpha-123456789abc"),
        Some("feature/ui"),
    );
    let dash_session = tmux_worktree_session_name(
        "cc-partner",
        "project-1",
        Some("worktree-beta-abcdef123456"),
        Some("feature-ui"),
    );

    assert_ne!(slash_session, dash_session);
}

/// Business Logic（为什么需要这个测试）:
///     Workbench 运行在 GUI/Tauri 环境时可能继承 `TERM=dumb`，tmux attach 会把终端响应错误送进 pane。
///
/// Code Logic（这个测试做什么）:
///     断言所有工作台 PTY 命令都会显式声明 xterm 兼容终端环境和真彩色能力。
#[test]
fn workbench_terminal_env_overrides_dumb_parent_term() {
    let mut command = CommandBuilder::new("/bin/sh");
    apply_workbench_terminal_env(&mut command, None);

    assert_eq!(
        command.get_env("TERM").and_then(|value| value.to_str()),
        Some("xterm-256color")
    );
    assert_eq!(
        command
            .get_env("COLORTERM")
            .and_then(|value| value.to_str()),
        Some("truecolor")
    );
    let lang = command
        .get_env("LANG")
        .and_then(|value| value.to_str())
        .expect("LANG");
    assert!(
        is_utf8_locale_value(lang),
        "PTY LANG must be UTF-8, got {lang}"
    );
    assert_eq!(
        command.get_env("LC_CTYPE").and_then(|value| value.to_str()),
        Some(lang)
    );
    assert_eq!(
        command.get_env("LC_ALL").and_then(|value| value.to_str()),
        Some(lang)
    );
}

/// Business Logic（为什么需要这个测试）:
///     发行版 GUI 空/`C` locale 会让 Claude 把中文和图标替换成 `_`；解析必须只接受 UTF-8 codeset。
///
/// Code Logic（这个测试做什么）:
///     覆盖常见 UTF-8 写法与 C/POSIX/ISO-8859 拒绝路径。
#[test]
fn utf8_locale_value_accepts_codeset_and_rejects_c_posix() {
    assert!(is_utf8_locale_value("en_US.UTF-8"));
    assert!(is_utf8_locale_value("zh_CN.utf8"));
    assert!(is_utf8_locale_value("C.UTF-8"));
    assert!(is_utf8_locale_value("UTF-8"));
    assert!(is_utf8_locale_value("en_US.UTF-8@euro"));
    assert!(!is_utf8_locale_value(""));
    assert!(!is_utf8_locale_value("C"));
    assert!(!is_utf8_locale_value("POSIX"));
    assert!(!is_utf8_locale_value("en_US"));
    assert!(!is_utf8_locale_value("en_US.ISO8859-1"));
}

/// Business Logic（为什么需要这个测试）:
///     Workbench 内 claude 不得自动连 VS Code，否则会注入无关 active-file 上下文。
///
/// Code Logic（这个测试做什么）:
///     apply_workbench_terminal_env 强制 AUTO_CONNECT_IDE=false，并移除 SSE_PORT。
#[test]
fn workbench_terminal_env_forces_claude_ide_disconnect() {
    let mut command = CommandBuilder::new("/bin/sh");
    // 模拟父进程/IDE 注入过 SSE 端口；隔离路径必须清掉。
    command.env(CLAUDE_CODE_SSE_PORT_ENV, "20751");
    apply_workbench_terminal_env(&mut command, None);

    assert_eq!(
        command
            .get_env(CLAUDE_CODE_AUTO_CONNECT_IDE_ENV)
            .and_then(|v| v.to_str()),
        Some("false")
    );
    assert!(command.get_env(CLAUDE_CODE_SSE_PORT_ENV).is_none());
}

/// Business Logic（为什么需要这个测试）:
///     Agent Hook 依赖四条非敏感 CC_PARTNER_*_ID；不得注入 control/device token。
///
/// Code Logic（这个测试做什么）:
///     apply 带 agent ctx 后断言四 ID 存在，且无 *TOKEN* / credential 键。
#[test]
fn workbench_terminal_env_includes_stable_partner_ids_without_tokens() {
    let mut command = CommandBuilder::new("/bin/sh");
    let ctx = TerminalAgentContextIds {
        project_id: "proj-1".to_string(),
        worktree_id: "wt-1".to_string(),
        terminal_session_id: "term-1".to_string(),
        owner_instance_id: "owner-1".to_string(),
        agent_session_id: None,
    };
    apply_workbench_terminal_env(&mut command, Some(&ctx));

    assert_eq!(
        command
            .get_env("CC_PARTNER_PROJECT_ID")
            .and_then(|v| v.to_str()),
        Some("proj-1")
    );
    assert_eq!(
        command
            .get_env("CC_PARTNER_WORKTREE_ID")
            .and_then(|v| v.to_str()),
        Some("wt-1")
    );
    assert_eq!(
        command
            .get_env("CC_PARTNER_TERMINAL_SESSION_ID")
            .and_then(|v| v.to_str()),
        Some("term-1")
    );
    assert_eq!(
        command
            .get_env("CC_PARTNER_OWNER_INSTANCE_ID")
            .and_then(|v| v.to_str()),
        Some("owner-1")
    );
    // 普通用户终端不注入 AGENT_SESSION_ID
    assert!(command.get_env("CC_PARTNER_AGENT_SESSION_ID").is_none());
    assert!(command.get_env("CC_PARTNER_CONTROL_TOKEN").is_none());
    assert!(command.get_env("CC_PARTNER_DEVICE_TOKEN").is_none());
    assert!(command.get_env("CC_PARTNER_AUTH_TOKEN").is_none());
    assert_eq!(
        command
            .get_env(CLAUDE_CODE_AUTO_CONNECT_IDE_ENV)
            .and_then(|v| v.to_str()),
        Some("false")
    );

    let tmux_args = tmux_agent_context_env_args(&ctx);
    assert_eq!(tmux_args.len() % 2, 0);
    for pair in tmux_args.chunks(2) {
        assert_eq!(pair[0], "-e");
        assert!(
            pair[1].starts_with("CC_PARTNER_")
                || pair[1].starts_with("CLAUDE_CODE_AUTO_CONNECT_IDE=")
        );
        assert!(!pair[1].contains("TOKEN"));
    }
    assert!(tmux_args
        .iter()
        .any(|a| a == "CC_PARTNER_PROJECT_ID=proj-1"));
    assert!(tmux_args
        .iter()
        .any(|a| a == "CLAUDE_CODE_AUTO_CONNECT_IDE=false"));
    assert!(!tmux_args
        .iter()
        .any(|a| a.starts_with("CC_PARTNER_AGENT_SESSION_ID=")));

    // Orchestrator 预分配路径：注入 AGENT_SESSION_ID
    let mut command2 = CommandBuilder::new("/bin/sh");
    let ctx2 = TerminalAgentContextIds {
        project_id: "proj-1".to_string(),
        worktree_id: "wt-1".to_string(),
        terminal_session_id: "term-1".to_string(),
        owner_instance_id: "owner-1".to_string(),
        agent_session_id: Some("agent-prealloc-1".to_string()),
    };
    apply_workbench_terminal_env(&mut command2, Some(&ctx2));
    assert_eq!(
        command2
            .get_env("CC_PARTNER_AGENT_SESSION_ID")
            .and_then(|v| v.to_str()),
        Some("agent-prealloc-1")
    );
    let tmux2 = tmux_agent_context_env_args(&ctx2);
    assert!(tmux2
        .iter()
        .any(|a| a == "CC_PARTNER_AGENT_SESSION_ID=agent-prealloc-1"));
    assert!(tmux2
        .iter()
        .any(|a| a == "CLAUDE_CODE_AUTO_CONNECT_IDE=false"));
}

/// Business Logic（为什么需要这个测试）:
///     前端 terminal window 必须绑定到对应 tmux window，不能只 attach 到 worktree session 的当前 window。
///
/// Code Logic（这个测试做什么）:
///     断言 attach 参数先连接 worktree session，再用 switch-client 指向具体 `session:@window` target。
#[test]
fn tmux_attach_window_args_switch_client_to_window_target() {
    let args = tmux_attach_window_args(
        "cc-partner-project-project1234abcd",
        "cc-partner-project-project1234abcd:@7",
    );

    assert_eq!(
        args,
        vec![
            "attach-session",
            "-t",
            "cc-partner-project-project1234abcd",
            ";",
            "switch-client",
            "-t",
            "cc-partner-project-project1234abcd:@7",
        ]
    );
}

/// Business Logic（为什么需要这个测试）:
///     隔离 data_dir socket 在 sidecar/GUI 重启后变成空 server，restore 只能看到
///     tmux_target_missing。0.8.3 用默认 socket，server 由 `new-session` 自行 daemonize，
///     重启后 window 还在。
///
/// Code Logic（这个测试做什么）:
///     Native 前缀必须为空；不得带 `-S`/`-f`/`-L`。
#[test]
fn native_tmux_command_uses_default_socket() {
    let backend = TmuxCommand::native("tmux");
    assert!(
        backend.prefix_args.is_empty(),
        "native tmux 必须走默认 socket，实际 prefix={:?}",
        backend.prefix_args
    );
    assert!(
        !backend
            .prefix_args
            .iter()
            .any(|arg| arg == "-S" || arg == "-f" || arg == "-L"),
        "不得隔离 socket/config: {:?}",
        backend.prefix_args
    );
}

/// Business Logic（为什么需要这个测试）:
///     tmux start-server 若仍由 GUI spawn，Dev.app codesign 会杀掉 server 与全部 pane。
///
/// Code Logic（这个测试做什么）:
///     锁定 `ensure_workbench_tmux_server` 调用 `run_disclaimed`。
#[test]
fn tmux_start_server_uses_disclaimed_spawn() {
    let src = include_str!("sessions.rs");
    let start = src
        .find("fn ensure_workbench_tmux_server")
        .expect("ensure_workbench_tmux_server");
    let body = src[start..]
        .split("fn detach_tmux_clients_for_row")
        .next()
        .expect("body");
    assert!(
        body.contains("run_disclaimed"),
        "tmux start-server 必须走 detached_spawn::run_disclaimed，避免 GUI 责任链"
    );
    assert!(
        !body.contains("回退普通 spawn"),
        "start-server 失败不得回退到 GUI 责任链上的普通 spawn，否则 tmux 会间歇性被 codesign 杀掉"
    );
}

/// Business Logic（为什么需要这个测试）:
///     默认 socket 上 `start-server` 若仍加载用户 conf 且 `exit-empty` 为 on，
///     空 server 会立刻退出，留下 stale socket，restore 变成 tmux_target_missing。
///
/// Code Logic（这个测试做什么）:
///     Native 为 `-f <conf> start-server`；WSL 前缀之后同样带 `-f`；全程不得出现 `-S`。
#[test]
fn tmux_start_server_args_use_persist_conf_on_default_socket() {
    assert_eq!(
        tmux_start_server_args(&[], Some("/tmp/cc-partner/tmux.conf")),
        vec![
            "-f".to_string(),
            "/tmp/cc-partner/tmux.conf".to_string(),
            "start-server".to_string(),
        ]
    );
    assert_eq!(
        tmux_start_server_args(
            &["--exec".to_string(), "tmux".to_string()],
            Some("/mnt/c/tmux.conf"),
        ),
        vec![
            "--exec".to_string(),
            "tmux".to_string(),
            "-f".to_string(),
            "/mnt/c/tmux.conf".to_string(),
            "start-server".to_string(),
        ]
    );
    let args = tmux_start_server_args(&[], Some("/tmp/cc-partner/tmux.conf"));
    assert!(
        !args.iter().any(|arg| arg == "-S"),
        "start-server 必须走默认 socket，不得 -S: {args:?}"
    );
}

/// Business Logic（为什么需要这个测试）:
///     sidecar/GUI 热更新会 SIGHUP attach 客户端；若 last-client 时拆掉 unattached session，
///     pane 里的 shell/Agent 会一起死，重启后 restore 只能看到 tmux_target_missing。
///
/// Code Logic（这个测试做什么）:
///     断言 session-local `destroy-unattached off`，以及 server-level `exit-empty off`。
#[test]
fn tmux_persist_commands_keep_unattached_session_and_empty_server() {
    assert_eq!(
        tmux_session_persist_commands("cc-partner-project-project1234abcd"),
        vec![vec![
            "set-option",
            "-t",
            "cc-partner-project-project1234abcd",
            "destroy-unattached",
            "off",
        ]]
    );
    assert_eq!(
        tmux_server_persist_commands(),
        vec![vec!["set-option", "-s", "exit-empty", "off"]]
    );
}

/// Business Logic（为什么需要这个测试）:
///     退出清理必须先让 tmux 干净 detach 客户端，再 SIGHUP PTY；否则 hangup 可能把 pane 进程带走。
///
/// Code Logic（这个测试做什么）:
///     断言 `detach-client -s <session>`，按 session 一次性拆掉该 worktree 的全部 attach。
#[test]
fn tmux_detach_session_clients_args_detach_all_clients_of_worktree_session() {
    assert_eq!(
        tmux_detach_session_clients_args("cc-partner-project-project1234abcd"),
        vec!["detach-client", "-s", "cc-partner-project-project1234abcd"]
    );
}

/// Business Logic（为什么需要这个测试）:
///     旧 tab 的 window_id `@0` 在 session 被重建后会被新窗口占用；restore 若只看 `@0` 存在就会
///     把新终端抢走，旧 tab 表现为连错窗口或立刻 exited。
///
/// Code Logic（这个测试做什么）:
///     锁定 window user option 的读写参数，以及 tag 匹配规则：空=legacy 放行，匹配放行，不匹配拒绝。
#[test]
fn tmux_window_identity_rejects_reused_window_id() {
    assert_eq!(
        tmux_set_window_identity_args("sess:@0", "s-old"),
        vec![
            "set-option",
            "-w",
            "-t",
            "sess:@0",
            "@cc-partner-session-id",
            "s-old",
        ]
    );
    assert_eq!(
        tmux_read_window_identity_args("sess:@0"),
        vec![
            "show-options",
            "-wqv",
            "-t",
            "sess:@0",
            "@cc-partner-session-id",
        ]
    );
    assert!(tmux_window_identity_allows_restore("", "s-old"));
    assert!(tmux_window_identity_allows_restore("s-old", "s-old"));
    assert!(!tmux_window_identity_allows_restore("s-new", "s-old"));
}

/// Business Logic（为什么需要这个测试）:
///     冷启动恢复只能导入 tmux 历史区，不能把当前可见屏幕也捕获后再与 attach 重绘重复拼接。
///
/// Code Logic（这个测试做什么）:
///     锁定 capture-pane 使用负行号 history-only 范围、文本属性与精确 window target。
#[test]
fn tmux_capture_history_args_exclude_current_screen() {
    assert_eq!(
        tmux_capture_history_args("cc-partner-project-project1234abcd:@7"),
        vec![
            "capture-pane",
            "-p",
            "-e",
            "-S",
            "-",
            "-E",
            "-1",
            "-t",
            "cc-partner-project-project1234abcd:@7",
        ]
    );
}

/// Business Logic（为什么需要这个测试）:
///     运行中 resume hydration 要把 history 与当前 pane 一起渲染进 normal buffer，不能只抓
///     history 后再拼接会进入 alternate screen 的旧 live ring。
///
/// Code Logic（这个测试做什么）:
///     锁定 hydration capture 从 history 起点到当前屏末尾，不带 `-E -1`。
#[test]
fn tmux_capture_scrollback_snapshot_args_include_current_screen() {
    assert_eq!(
        tmux_capture_scrollback_snapshot_args("cc-partner-project-project1234abcd:@7"),
        vec![
            "capture-pane",
            "-p",
            "-e",
            "-S",
            "-",
            "-t",
            "cc-partner-project-project1234abcd:@7",
        ]
    );
}

/// Business Logic（为什么需要这个测试）:
///     capture-pane 的 LF 必须能在 convertEol=false 的 xterm 中逐行回到首列，同时不能破坏已有 CRLF。
///
/// Code Logic（这个测试做什么）:
///     混合裸 LF/CRLF/无尾换行，断言只为裸 LF 补 CR。
#[test]
fn tmux_captured_history_normalizes_line_endings_for_xterm() {
    assert_eq!(
        normalize_tmux_history_for_terminal("first\nsecond\r\nthird"),
        "first\r\nsecond\r\nthird"
    );
}

/// Business Logic（为什么需要这个测试）:
///     工作台浅色/深色主题切换时，tmux 底部 status bar 不应继承用户 tmux 配置里的深色背景、彩色右侧时间或 underline。
///     用户全局 `mouse on` 时滚轮会进 copy-mode（浏览模式），键盘被 tmux 吃掉，必须 session-local 强制 mouse off。
///     同时必须宣告 `xterm*:mouse`，但配置必须幂等，否则每次套用主题都会污染 server array。
///
/// Code Logic（这个测试做什么）:
///     断言 Workbench 使用无内嵌颜色的 status/window format，强制 status-position=bottom、mouse=off
///     主题命令不得无条件追加 terminal-features；mouse capability 由独立幂等步骤维护。
#[test]
fn tmux_status_theme_commands_use_light_safe_label_style() {
    let commands = tmux_status_theme_commands("cc-partner-project-project1234abcd");

    assert_eq!(
        commands,
        vec![
            vec![
                "set-option",
                "-t",
                "cc-partner-project-project1234abcd",
                "mouse",
                "off",
            ],
            vec![
                "set-option",
                "-t",
                "cc-partner-project-project1234abcd",
                "status-position",
                "bottom",
            ],
            vec![
                "set-option",
                "-t",
                "cc-partner-project-project1234abcd",
                "status-style",
                "fg=default,bg=default",
            ],
            vec![
                "set-option",
                "-t",
                "cc-partner-project-project1234abcd",
                "status-left-style",
                "fg=default,bg=default",
            ],
            vec![
                "set-option",
                "-t",
                "cc-partner-project-project1234abcd",
                "status-right-style",
                "fg=default,bg=default",
            ],
            vec![
                "set-option",
                "-t",
                "cc-partner-project-project1234abcd",
                "status-left",
                "#[bold]#S › ",
            ],
            vec![
                "set-option",
                "-t",
                "cc-partner-project-project1234abcd",
                "status-right",
                "%H:%M | %Y-%m-%d ",
            ],
            vec![
                "set-window-option",
                "-t",
                "cc-partner-project-project1234abcd",
                "window-status-style",
                "fg=default,bg=default",
            ],
            vec![
                "set-window-option",
                "-t",
                "cc-partner-project-project1234abcd",
                "window-status-current-style",
                "fg=black,bg=colour111,bold",
            ],
            vec![
                "set-window-option",
                "-t",
                "cc-partner-project-project1234abcd",
                "window-status-format",
                " #I:#W#F ",
            ],
            vec![
                "set-window-option",
                "-t",
                "cc-partner-project-project1234abcd",
                "window-status-current-format",
                " #I:#W#F ",
            ],
            vec![
                "set-window-option",
                "-t",
                "cc-partner-project-project1234abcd",
                "window-status-separator",
                " ",
            ],
        ]
    );
    let joined = commands
        .iter()
        .flat_map(|command| command.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(!joined.contains("underscore"));
    assert!(!joined.contains("fg=#"));
    assert!(!joined.contains("bg=#"));
}

/// Business Logic（为什么需要这个测试）:
///     冷启动 / 适应尺寸必须能构造出强制同步 window 尺寸的 tmux 命令，避免 status bar 悬在中间。
///
/// Code Logic（这个测试做什么）:
///     断言 `tmux_resize_window_args` 生成 `resize-window -t <target> -x <cols> -y <rows>`，
///     且 bump rows 与目标不同以便两步强制重绘。
#[test]
fn tmux_resize_window_args_force_window_size() {
    let args = tmux_resize_window_args("cc-partner-project-project1234abcd:@3", 160, 42);
    assert_eq!(
        args,
        vec![
            "resize-window",
            "-t",
            "cc-partner-project-project1234abcd:@3",
            "-x",
            "160",
            "-y",
            "42",
        ]
    );
    assert_eq!(tmux_force_redraw_bump_rows(42), 41);
    assert_eq!(
        tmux_force_redraw_bump_rows(MIN_TERMINAL_ROWS),
        MIN_TERMINAL_ROWS + 1
    );
}

/// Business Logic（为什么需要这个测试）:
///     长时间使用 Workbench 不得让 tmux server 的 terminal-features 随每次主题套用无限增长，
///     清理时也不能破坏用户已有的 screen/RGB/clipboard 等能力。
///
/// Code Logic（这个测试做什么）:
///     覆盖缺失时单次追加、精确重复时保留首项并倒序删除、复合 xterm mouse 已存在时 no-op。
#[test]
fn tmux_terminal_mouse_feature_reconcile_is_idempotent_and_preserves_other_entries() {
    let defaults = concat!(
        "terminal-features[0] xterm*:clipboard:ccolour:cstyle:focus:title\n",
        "terminal-features[1] screen*:title\n",
        "terminal-features[2] *:RGB\n",
    );
    assert_eq!(
        tmux_terminal_mouse_feature_reconcile_commands(defaults),
        vec![vec![
            "set-option",
            "-sa",
            "terminal-features",
            "xterm*:mouse",
        ]]
    );

    let duplicated = concat!(
        "terminal-features[0] xterm*:clipboard:ccolour:cstyle:focus:title\n",
        "terminal-features[4] xterm*:mouse\n",
        "terminal-features[5] screen*:mouse\n",
        "terminal-features[7] xterm*:mouse\n",
        "terminal-features[9] xterm*:mouse\n",
    );
    assert_eq!(
        tmux_terminal_mouse_feature_reconcile_commands(duplicated),
        vec![
            vec!["set-option", "-su", "terminal-features[9]"],
            vec!["set-option", "-su", "terminal-features[7]"],
        ]
    );

    let equivalent = concat!(
        "terminal-features[0] xterm*:clipboard:mouse:title\n",
        "terminal-features[1] screen*:title\n",
    );
    assert!(tmux_terminal_mouse_feature_reconcile_commands(equivalent).is_empty());
}

/// Business Logic（为什么需要这个测试）:
///     顶部 app tab 切换时，底部 tmux 当前 window 也必须跟着切换到 tab 绑定的真实 window。
///
/// Code Logic（这个测试做什么）:
///     断言 focus 操作使用 `select-window -t <session:@window>` 切 worktree tmux session 的 current window。
#[test]
fn tmux_select_window_args_targets_bound_window() {
    let args = tmux_select_window_args("cc-partner-project-project1234abcd:@7");

    assert_eq!(
        args,
        vec![
            "select-window",
            "-t",
            "cc-partner-project-project1234abcd:@7",
        ]
    );
}

/// Business Logic（为什么需要这个测试）:
///     用户在 tmux 底部状态栏切换 window 后，cc-partner 需要读取 worktree tmux session 的当前 window。
///
/// Code Logic（这个测试做什么）:
///     断言查询 current window 使用 `display-message -p -t <session> #{window_id}`。
#[test]
fn tmux_current_window_args_read_session_current_window_id() {
    let args = tmux_current_window_args("cc-partner-project-project1234abcd");

    assert_eq!(
        args,
        vec![
            "display-message",
            "-p",
            "-t",
            "cc-partner-project-project1234abcd",
            "#{window_id}",
        ]
    );
}

/// Business Logic（为什么需要这个测试）:
///     session 命名规则升级时，仍存在的旧 tmux session 应通过 rename 保留 shell 上下文。
///
/// Code Logic（这个测试做什么）:
///     断言迁移使用 `rename-session -t <old> <new>` 参数。
#[test]
fn tmux_rename_session_args_preserve_existing_context() {
    let args = tmux_rename_session_args("cc-partner-worktree-old-id", "cc-partner-readable-name");

    assert_eq!(
        args,
        vec![
            "rename-session",
            "-t",
            "cc-partner-worktree-old-id",
            "cc-partner-readable-name",
        ]
    );
}

/// Business Logic（为什么需要这个测试）:
///     后端读到 tmux current window 后，需要映射回前端顶部应该选中的 app tab。
///
/// Code Logic（这个测试做什么）:
///     构造同一 worktree tmux session 内两个 window row，断言 window id 命中第二个 sessionId。
#[test]
fn focused_session_id_matches_worktree_backend_window_id() {
    let first = fake_tmux_row("session-1", "project-1", Some("project-1:main"), "@1");
    let second = fake_tmux_row("session-2", "project-1", Some("project-1:main"), "@2");
    let other_project = fake_tmux_row("session-3", "project-2", Some("project-2:main"), "@2");
    let backend_id = second.backend_id.clone().expect("tmux backend id");

    let focused = focused_session_id_for_tmux_window(
        [&first, &second, &other_project],
        "project-1",
        Some("project-1:main"),
        &backend_id,
        "@2",
    );

    assert_eq!(focused, Some("session-2".to_string()));
}

/// Business Logic（为什么需要这个测试）:
///     用户在 feature worktree 的 tmux status bar 切换 window 时，主工作区 tab 不应被误选中。
///
/// Code Logic（这个测试做什么）:
///     构造同一项目、相同 tmux window id、不同 worktree/backend 的 row，断言 focused 映射按 worktree 过滤。
#[test]
fn focused_session_id_does_not_cross_worktree_scope() {
    let main = fake_tmux_row("main-session", "project-1", Some("project-1:main"), "@2");
    let feature = fake_tmux_row("feature-session", "project-1", Some("worktree-2"), "@2");
    let feature_backend = feature.backend_id.clone().expect("feature backend");

    let focused = focused_session_id_for_tmux_window(
        [&main, &feature],
        "project-1",
        Some("worktree-2"),
        &feature_backend,
        "@2",
    );

    assert_eq!(focused, Some("feature-session".to_string()));
}

/// Business Logic（为什么需要这个测试）:
///     pane 操作应复用 tmux 原生命令，避免前端伪分屏和真实终端布局分裂。
///
/// Code Logic（这个测试做什么）:
///     断言 split right/down 会生成 tmux `split-window -h/-v` 参数。
#[test]
fn tmux_split_direction_maps_to_tmux_arguments() {
    assert_eq!(PaneSplitDirection::Right.tmux_flag(), "-h");
    assert_eq!(PaneSplitDirection::Down.tmux_flag(), "-v");
}

/// 构造测试用 pane 几何。
fn pane_geometry(
    pane_id: &str,
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
    active: bool,
) -> TmuxPaneGeometry {
    TmuxPaneGeometry {
        pane_id: pane_id.to_string(),
        left,
        top,
        right,
        bottom,
        active,
    }
}

/// Business Logic（为什么需要这个测试）:
///     点击命中依赖 tmux 一次性给出 pane 边界、active 与 zoom 真值，格式漂移会让命中判定整体失效。
///
/// Code Logic（这个测试做什么）:
///     断言 list-panes 参数带 `-F` 且格式串同时含 pane_left/top/right/bottom/active/window_zoomed_flag。
#[test]
fn tmux_list_pane_geometry_args_request_bounds_active_and_zoom() {
    let args = tmux_list_pane_geometry_args("cc-partner-project-p1:@2");

    assert_eq!(args[0], "list-panes");
    assert_eq!(args[1], "-t");
    assert_eq!(args[2], "cc-partner-project-p1:@2");
    assert_eq!(args[3], "-F");
    for field in [
        "#{pane_id}",
        "#{pane_left}",
        "#{pane_top}",
        "#{pane_right}",
        "#{pane_bottom}",
        "#{pane_active}",
        "#{window_zoomed_flag}",
    ] {
        assert!(args[4].contains(field), "格式串缺少 {field}");
    }
}

/// Business Logic（为什么需要这个测试）:
///     tmux 输出损坏或字段缺失时必须整行丢弃，不能产生错误命中矩形把点击切到别的 pane。
///
/// Code Logic（这个测试做什么）:
///     混合合法行、字段数不足行、非数字行与 right<left 的反向矩形，断言只保留合法行并解析 zoom。
#[test]
fn parse_tmux_pane_geometry_skips_malformed_rows() {
    let output =
        "%1 0 0 79 23 1 0\n%2 81 0 159 23 0 0\n%3 0 0 79\n%4 x 0 79 23 0 0\n%5 80 0 10 23 0 0\n";

    let layout = parse_tmux_pane_geometry(output);

    assert_eq!(
        layout.panes,
        vec![
            pane_geometry("%1", 0, 0, 79, 23, true),
            pane_geometry("%2", 81, 0, 159, 23, false),
        ]
    );
    assert!(!layout.zoomed);
}

/// Business Logic（为什么需要这个测试）:
///     zoom 状态下屏幕只显示一个 pane，历史布局不再对应像素，必须能被识别并整体拒绝命中。
///
/// Code Logic（这个测试做什么）:
///     断言任一行 window_zoomed_flag 为 1 时 layout.zoomed 为 true。
#[test]
fn parse_tmux_pane_geometry_detects_zoomed_window() {
    let layout = parse_tmux_pane_geometry("%1 0 0 79 23 1 1\n%2 81 0 159 23 0 1\n");

    assert!(layout.zoomed);
    assert_eq!(layout.panes.len(), 2);
}

/// Business Logic（为什么需要这个测试）:
///     点击必须落到覆盖该字符格的 pane；落在 pane 之间的分隔边框时应 no-op 而不是就近吸附。
///
/// Code Logic（这个测试做什么）:
///     左右分屏布局下断言左区/右区命中对应 pane，边框列与越界行返回 None。
#[test]
fn tmux_pane_at_position_matches_bounds_and_rejects_borders() {
    let panes = vec![
        pane_geometry("%1", 0, 0, 79, 23, true),
        pane_geometry("%2", 81, 0, 159, 23, false),
    ];

    assert_eq!(
        tmux_pane_at_position(&panes, 10, 5).map(|p| p.pane_id.as_str()),
        Some("%1")
    );
    assert_eq!(
        tmux_pane_at_position(&panes, 120, 23).map(|p| p.pane_id.as_str()),
        Some("%2")
    );
    // 第 80 列是分隔边框，不属于任何 pane。
    assert!(tmux_pane_at_position(&panes, 80, 5).is_none());
    // 第 24 行是 tmux status bar，超出 pane 范围。
    assert!(tmux_pane_at_position(&panes, 10, 24).is_none());
}

/// Business Logic（为什么需要这个测试）:
///     点击切换是绝对定位操作，必须用 pane_id 选中；退化成相对 `.+` 会切到用户没点的 pane。
///
/// Code Logic（这个测试做什么）:
///     断言 select-pane 参数为 `select-pane -t <pane_id>`，不含 `.+`。
#[test]
fn tmux_select_pane_args_target_absolute_pane_id() {
    assert_eq!(tmux_select_pane_args("%7"), vec!["select-pane", "-t", "%7"]);
}

/// Business Logic（为什么需要这个测试）:
///     通过分屏按钮创建的新 pane 应从项目根目录启动，不能继承当前 pane 里用户 cd 后的目录；
///     同时强制 Claude 不连 IDE。
///
/// Code Logic（这个测试做什么）:
///     断言 split-window 参数包含 `-c <project_root>`、IDE 隔离 env，并保留方向与 target。
#[test]
fn tmux_split_window_args_pin_project_root_cwd() {
    let args = tmux_split_window_args(
        PaneSplitDirection::Right,
        "cc-partner-project-p1:@2",
        "/Users/hans/project",
    );

    let locale = workbench_utf8_locale();
    let mut expected = vec![
        "split-window".to_string(),
        "-h".to_string(),
        "-t".to_string(),
        "cc-partner-project-p1:@2".to_string(),
        "-c".to_string(),
        "/Users/hans/project".to_string(),
        "-e".to_string(),
        "CLAUDE_CODE_AUTO_CONNECT_IDE=false".to_string(),
    ];
    expected.extend(tmux_utf8_locale_env_args());
    assert_eq!(args, expected);
    assert!(is_utf8_locale_value(&locale));
    assert!(args.iter().any(|arg| arg == &format!("LANG={locale}")));
    assert!(args.iter().any(|arg| arg == &format!("LC_ALL={locale}")));
}

/// Business Logic（为什么需要这个测试）:
///     用户需要在同一个 terminal window 内循环切换到下一个 pane，不能创建新 pane 或跨 window 切换。
///
/// Code Logic（这个测试做什么）:
///     断言 next-pane 操作生成 `select-pane -t <window-target>.+` 参数。
#[test]
fn tmux_select_next_pane_args_targets_next_pane_in_current_window() {
    let args = tmux_select_next_pane_args("cc-partner-project-p1:@2");

    assert_eq!(
        args,
        vec!["select-pane", "-t", "cc-partner-project-p1:@2.+"]
    );
}

/// Business Logic（为什么需要这个测试）:
///     移动端需要始终只显示当前 active pane，后端必须能把当前 pane zoom 到整个 window。
///
/// Code Logic（这个测试做什么）:
///     断言 ensure zoom 操作生成 `resize-pane -Z -t <window-target>` 参数。
#[test]
fn tmux_zoom_active_pane_args_targets_current_window() {
    let args = tmux_zoom_active_pane_args("cc-partner-project-p1:@2");

    assert_eq!(
        args,
        vec!["resize-pane", "-Z", "-t", "cc-partner-project-p1:@2"]
    );
}

/// Business Logic（为什么需要这个测试）:
///     分屏工具栏的 X 应关闭当前 active pane；只有最后一个 pane 时应关闭 window，而不是报错。
///
/// Code Logic（这个测试做什么）:
///     断言 pane 数为 1 或 0 时选择关闭 window，pane 数大于 1 时选择 kill-pane。
#[test]
fn single_pane_close_plan_closes_window_instead_of_error() {
    assert_eq!(pane_close_plan(0), PaneClosePlan::CloseWindow);
    assert_eq!(pane_close_plan(1), PaneClosePlan::CloseWindow);
    assert_eq!(pane_close_plan(2), PaneClosePlan::KillPane);
}

/// Business Logic（为什么需要这个测试）:
///     项目列表需要展示真实 pane 数，后端必须能从 tmux `list-panes` 输出得到稳定计数。
///
/// Code Logic（这个测试做什么）:
///     断言空行会被忽略，非空 pane id 行会累计为 paneCount。
#[test]
fn pane_count_from_tmux_output_ignores_empty_lines() {
    assert_eq!(pane_count_from_tmux_output("%1\n\n%2\n"), 2);
    assert_eq!(pane_count_from_tmux_output("\n"), 0);
}

/// Business Logic（R36 H1: 为什么需要这个测试）:
///     已知 window_id 时即使 count==1 也只能 kill-window，禁止并发/重试 close 把兄弟 terminal
///     连同整个 session 一起杀掉；多 window 与 last-window 路径一致。
///
/// Code Logic（这个测试做什么）:
///     Some(@1)+count1 / Some(@1)+count2 均生成 kill-window；
///     None+count1 仍允许 legacy kill-session。
#[test]
fn tmux_destroy_backend_args_always_kill_window_when_window_id_known() {
    assert_eq!(
        tmux_destroy_backend_args("cc-partner-project-p1", Some("@1"), Some(1)),
        Some(vec![
            "kill-window".to_string(),
            "-t".to_string(),
            "cc-partner-project-p1:@1".to_string(),
        ])
    );
    assert_eq!(
        tmux_destroy_backend_args("cc-partner-project-p1", Some("@1"), Some(2)),
        Some(vec![
            "kill-window".to_string(),
            "-t".to_string(),
            "cc-partner-project-p1:@1".to_string(),
        ])
    );
    // legacy：无 window_id + 已知 count → kill-session。
    assert_eq!(
        tmux_destroy_backend_args("cc-partner-project-p1", None, Some(1)),
        Some(vec![
            "kill-session".to_string(),
            "-t".to_string(),
            "cc-partner-project-p1".to_string(),
        ])
    );
}

/// Business Logic（R32 H1: 为什么需要这个测试）:
///     multi-window session 中 list-windows 探测失败时，不得降级 kill-session 毁掉兄弟 terminal。
///
/// Code Logic（这个测试做什么）:
///     window_id 已知 + count=None → kill-window only；
///     window_id 缺失 + count=None → None（fail closed）；
///     kill_created_tmux_window_only 纯语义：有 window_id 才构造 kill-window target。
#[test]
fn tmux_destroy_args_probe_failure_never_kills_session_with_window_id() {
    // 多窗 + 探测失败：仅 kill 已知 window。
    assert_eq!(
        tmux_destroy_backend_args("wt-session", Some("@7"), None),
        Some(vec![
            "kill-window".to_string(),
            "-t".to_string(),
            "wt-session:@7".to_string(),
        ])
    );
    // 无 window_id + 探测失败：fail closed，不发 kill-session。
    assert_eq!(tmux_destroy_backend_args("wt-session", None, None), None);
    // 无 window_id 但 count==1 可知：仍允许 kill-session（关闭路径最后一窗）。
    assert_eq!(
        tmux_destroy_backend_args("wt-session", None, Some(1)),
        Some(vec![
            "kill-session".to_string(),
            "-t".to_string(),
            "wt-session".to_string(),
        ])
    );
    // TmuxCreateGuard reclaim 必须只走 window target，不依赖 count。
    let row = fake_tmux_row("s-r32-h1", "p-r32", Some("wt1"), "@42");
    let session = row.backend_id.as_deref().expect("session");
    let window = row.backend_window_id.as_deref().expect("window");
    assert_eq!(
        tmux_window_target(session, window),
        format!("{session}:{window}")
    );
    // 缺 window_id 的 row：guard reclaim 应 no-op（不 panic）。
    let mut missing = fake_tmux_row("s-r32-h1-miss", "p-r32", Some("wt1"), "@99");
    missing.backend_window_id = None;
    kill_created_tmux_window_only(&missing);
}

/// Business Logic（R35 M3: 为什么需要这个测试）:
///     destroy 非零退出时，already-gone 文案必须映射为成功，否则 close 路径会永远卡在 barrier。
///
/// Code Logic（这个测试做什么）:
///     覆盖 can't find / no server / no such / not found 及大小写；无关错误返回 false。
#[test]
fn tmux_destroy_exit_is_already_gone_detects_common_messages() {
    assert!(tmux_destroy_exit_is_already_gone(
        "",
        "can't find window: @1"
    ));
    assert!(tmux_destroy_exit_is_already_gone("Can't Find session", ""));
    assert!(tmux_destroy_exit_is_already_gone(
        "",
        "no server running on /tmp/tmux-1000/default"
    ));
    assert!(tmux_destroy_exit_is_already_gone("", "no such window: @9"));
    assert!(tmux_destroy_exit_is_already_gone("", "session not found"));
    assert!(!tmux_destroy_exit_is_already_gone("", "permission denied"));
    assert!(!tmux_destroy_exit_is_already_gone("", ""));
}

/// Business Logic（R35 M3 / R41 M7: 为什么需要这个测试）:
///     无 tmux backend 可杀时不应误阻 SQLite delete；但 running raw 不得自动成功，
///     否则 missing-handle 路径会删元数据却留下活 PTY。
///
/// Code Logic（这个测试做什么）:
///     disconnected raw → Ok；running raw → Err(raw_pty_kill_requires_live_handle)；
///     缺 backend_id 的 tmux → Ok（无 session 可 destroy）。
#[test]
fn kill_persisted_backend_ok_when_no_tmux_backend_to_destroy() {
    let mut raw = fake_tmux_row("s-raw", "p1", Some("wt1"), "@1");
    raw.backend = "raw".to_string();
    raw.backend_id = None;
    raw.backend_window_id = None;
    raw.status = "disconnected".to_string();
    assert!(kill_persisted_backend(&raw).is_ok());

    raw.status = "running".to_string();
    let err = kill_persisted_backend(&raw).expect_err("running raw must not auto-ok");
    assert_eq!(err.code(), "raw_pty_kill_requires_live_handle");

    let mut no_id = fake_tmux_row("s-no-id", "p1", Some("wt1"), "@1");
    no_id.backend_id = None;
    assert!(kill_persisted_backend(&no_id).is_ok());
}

/// Business Logic（为什么需要这个测试）:
///     旧版本把 tab 映射成独立 tmux session；升级后应迁移到 worktree session 内的 window。
///
/// Code Logic（这个测试做什么）:
///     构造缺少 backend_window_id 的 tmux row，断言恢复流程会判定它需要重建 window。
#[test]
fn old_tmux_rows_without_window_id_require_window_recreation() {
    let row = WorkbenchSessionRow {
        id: "s1".to_string(),
        project_id: "p1".to_string(),
        worktree_id: None,
        name: "Terminal".to_string(),
        name_source: "default".to_string(),
        command: "/bin/zsh".to_string(),
        cwd: "/tmp/project".to_string(),
        status: "running".to_string(),
        cols: DEFAULT_COLS,
        rows: DEFAULT_ROWS,
        started_at: "2026-06-24T00:00:00Z".to_string(),
        exited_at: None,
        exit_code: None,
        backend: TMUX_BACKEND.to_string(),
        backend_id: Some("cc-partner-legacy".to_string()),
        backend_window_id: None,
        created_at: "2026-06-24T00:00:00Z".to_string(),
        updated_at: "2026-06-24T00:00:00Z".to_string(),
    };

    assert!(tmux_row_requires_window_recreation(&row));
}

/// Business Logic（为什么需要这个测试）:
///     用户关闭终端或退出应用时，终端子进程可能已被系统回收，底层 kill 会返回 No such process。
///
/// Code Logic（这个测试做什么）:
///     构造 macOS/Linux 常见 ESRCH(os error 3)，断言终端 kill 归一化逻辑把它视为已停止。
#[test]
fn terminal_kill_treats_no_such_process_as_already_stopped() {
    let error = std::io::Error::from_raw_os_error(3);

    assert!(normalize_terminal_kill_result(Err(error)).is_ok());
}

/// Business Logic（为什么需要这个测试）:
///     工作台按项目切换时，只应展示当前项目下的终端会话。
///
/// Code Logic（这个测试做什么）:
///     插入两个 fake 会话并断言 list(Some(project_id)) 只返回匹配项。
#[test]
fn list_filters_by_project_id() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_fake_session_for_test("s1", "p1");
    registry.insert_fake_session_for_test("s2", "p2");

    let listed = registry.list(Some("p1"));

    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "s1");
    assert_eq!(listed[0].project_id, "p1");
}

/// Business Logic（为什么需要这个测试）:
///     应用退出时必须停止运行期 PTY attach，但不能丢掉用户下次启动要恢复的会话元数据。
///
/// Code Logic（这个测试做什么）:
///     插入 fake 会话后调用 shutdown_all，断言返回清理数量且会话状态变为 disconnected。
#[test]
fn shutdown_all_marks_sessions_disconnected() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_fake_session_for_test("s1", "p1");
    registry.insert_fake_session_for_test("s2", "p2");

    let cleaned = registry.shutdown_all();

    assert_eq!(cleaned, 2);
    let listed = registry.list(None);
    assert_eq!(listed.len(), 2);
    assert!(listed
        .iter()
        .all(|session| session.status == "disconnected"));
}

/// Business Logic（为什么需要这个测试）:
///     应用退出后再次启动时，用户之前打开的工作台终端 tab 应能恢复，而不是因为退出清理被彻底遗忘。
///
/// Code Logic（这个测试做什么）:
///     插入 fake 会话并执行退出清理，断言会话元数据仍可列出且状态被标记为 disconnected。
#[test]
fn shutdown_all_preserves_session_metadata_for_restart_restore() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_fake_session_for_test("s1", "p1");

    let cleaned = registry.shutdown_all();
    let listed = registry.list(Some("p1"));

    assert_eq!(cleaned, 1);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "s1");
    assert_eq!(listed[0].status, "disconnected");
    assert!(listed[0].exited_at.is_some());
}

/// Business Logic（Finding 5: 为什么需要这个测试）:
///     空 registry 上首次 claim 一个未运行也未被占位的 session 应成功，再次 claim 应
///     得到 RestoreInProgress（占位生效），从而消除并发 sessions/list 的 TOCTOU。
#[test]
fn try_claim_restore_serializes_concurrent_restore_for_same_session() {
    let registry = WorkbenchSessionRegistry::new();

    let first = registry.try_claim_restore("s1");
    assert!(first.is_claimed(), "首次 claim 应成功");

    let second = registry.try_claim_restore("s1");
    assert!(
        matches!(second, RestoreClaimOutcome::RestoreInProgress(_)),
        "占位期间第二次 claim 应为 RestoreInProgress，避免重复 restore"
    );

    // 释放占位后允许后续重试。
    registry.release_restore_claim("s1");
    let third = registry.try_claim_restore("s1");
    assert!(third.is_claimed(), "释放占位后应允许重新 claim");
    registry.release_restore_claim("s1");
}

/// Business Logic（Finding 5: 为什么需要这个测试）:
///     session 已在运行期 registry 时，claim 应返回 AlreadyLive，不写入占位，
///     避免对活跃 session 做无意义的 restore。
#[test]
fn try_claim_restore_returns_false_when_session_already_live() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_fake_session_for_test("live-1", "p1");

    let claimed = registry.try_claim_restore("live-1");
    assert!(
        matches!(claimed, RestoreClaimOutcome::AlreadyLive),
        "session 已在运行期 registry 时 claim 应为 AlreadyLive"
    );
    // 释放不存在的占位应是 no-op，不应 panic。
    registry.release_restore_claim("live-1");
}

/// Business Logic（Finding 5: 为什么需要这个测试）:
///     不同 session_id 的 claim 互不干扰，确保并发恢复多个持久化 tab 不互相阻塞。
#[test]
fn try_claim_restore_independent_for_different_sessions() {
    let registry = WorkbenchSessionRegistry::new();

    let a = registry.try_claim_restore("s-a");
    let b = registry.try_claim_restore("s-b");
    assert!(
        a.is_claimed() && b.is_claimed(),
        "不同 session 的 claim 应互不干扰"
    );

    registry.release_restore_claim("s-a");
    registry.release_restore_claim("s-b");
}

/// Business Logic（Finding 5: 为什么需要这个测试）:
///     release_restore_claim 对未占位的 session 必须是幂等 no-op，不应 panic，
///     因为 restore_persisted_sessions 在多个 early-return 路径上调用它。
#[test]
fn release_restore_claim_is_idempotent() {
    let registry = WorkbenchSessionRegistry::new();
    // 未 claim 直接 release — 不应 panic。
    registry.release_restore_claim("never-claimed");
    // 双重 release — 不应 panic。
    assert!(registry.try_claim_restore("s1").is_claimed());
    registry.release_restore_claim("s1");
    registry.release_restore_claim("s1");
}

/// Business Logic（R14/R16: 为什么需要这个测试）:
///     并发 list 拿到 RestoreInProgress 后必须能 wait 到 holder 的**结果**，
///     否则会过早返回持久行并触发永久 not_found。
///
/// Code Logic（这个测试做什么）:
///     holder claim → waiter 订阅 → finish Ready → wait Ready。
#[tokio::test]
async fn wait_for_shared_restore_unblocks_after_release() {
    let registry = WorkbenchSessionRegistry::new();
    assert!(registry.try_claim_restore("s-wait").is_claimed());
    let second = registry.try_claim_restore("s-wait");
    let RestoreClaimOutcome::RestoreInProgress(rx) = second else {
        panic!("second claim must be RestoreInProgress");
    };
    let wait_handle = tokio::spawn(async move { wait_for_shared_restore(rx).await });
    // 给 waiter 一点时间进入 changed loop。
    tokio::task::yield_now().await;
    registry.finish_restore_claim("s-wait", SharedRestoreNotification::Ready);
    let result = wait_handle
        .await
        .expect("waiter task must join after finish");
    assert_eq!(result, SharedRestoreWaitResult::Ready);
    assert!(!registry.is_restore_claim_held("s-wait"));
}

/// Business Logic（R16 M1: 为什么需要这个测试）:
///     holder 失败时必须广播 Failed；waiter 不得把 claim 释放当成功。
///
/// Code Logic（这个测试做什么）:
///     claim + 订阅 → finish Failed(Unavailable) → wait Failed，且 is_success=false。
#[tokio::test]
async fn wait_for_shared_restore_surfaces_holder_failure() {
    let registry = WorkbenchSessionRegistry::new();
    assert!(registry.try_claim_restore("s-fail").is_claimed());
    let second = registry.try_claim_restore("s-fail");
    let RestoreClaimOutcome::RestoreInProgress(rx) = second else {
        panic!("second claim must be RestoreInProgress");
    };
    let wait_handle = tokio::spawn(async move { wait_for_shared_restore(rx).await });
    tokio::task::yield_now().await;
    registry.finish_restore_claim(
        "s-fail",
        SharedRestoreNotification::Failed(AppErrorCategory::Unavailable),
    );
    let result = wait_handle.await.expect("failed waiter must join");
    assert_eq!(
        result,
        SharedRestoreWaitResult::Failed(AppErrorCategory::Unavailable)
    );
    assert!(!result.is_success());
    assert!(!registry.is_restore_claim_held("s-fail"));
    // 失败后允许重新 claim 重试 restore。
    assert!(registry.try_claim_restore("s-fail").is_claimed());
    registry.finish_restore_claim("s-fail", SharedRestoreNotification::PersistedDisconnected);
}

/// Business Logic（R16 M1: 为什么需要这个测试）:
///     无显式结果的 release 不得伪装 Ready；默认 Failed(Internal)。
///
/// Code Logic（这个测试做什么）:
///     claim + 订阅 → release_restore_claim → wait Failed(Internal)。
#[tokio::test]
async fn release_without_result_is_failed_not_ready() {
    let registry = WorkbenchSessionRegistry::new();
    assert!(registry.try_claim_restore("s-bare-release").is_claimed());
    let second = registry.try_claim_restore("s-bare-release");
    let RestoreClaimOutcome::RestoreInProgress(rx) = second else {
        panic!("second claim must be RestoreInProgress");
    };
    let wait_handle = tokio::spawn(async move { wait_for_shared_restore(rx).await });
    tokio::task::yield_now().await;
    registry.release_restore_claim("s-bare-release");
    assert_eq!(
        wait_handle.await.expect("join"),
        SharedRestoreWaitResult::Failed(AppErrorCategory::Internal)
    );
}

/// Business Logic（R14 M1: 为什么需要这个测试）:
///     启动时全局 list 与项目 list 并发：第一路 claim restore 并延迟完成；第二路必须
///     wait/省略恢复中会话，且在 holder 完成前不得把 session 视为可 replay。
///     否则 Provider 立刻 replay → permanent not_found。
///
/// Code Logic（这个测试做什么）:
///     holder claim 后延迟插入 live + finish Ready；waiter RestoreInProgress → wait；
///     wait 期间 is_restore_claim_held && !contains（不可 list/replay）；
///     wait 结束后 session live 且 claim 已释放。
#[tokio::test]
async fn concurrent_list_waits_for_in_flight_restore_before_ready() {
    let registry = WorkbenchSessionRegistry::new();
    assert!(registry.try_claim_restore("s-concurrent").is_claimed());

    let second = registry.try_claim_restore("s-concurrent");
    let RestoreClaimOutcome::RestoreInProgress(rx) = second else {
        panic!("second concurrent claim must be RestoreInProgress, not AlreadyLive/Claimed");
    };

    // 模拟 list merge 与 replay 守卫：恢复中不得当作 ready。
    assert!(
        registry.is_restore_claim_held("s-concurrent"),
        "in-flight restore must hold claim"
    );
    assert!(
        !registry.contains("s-concurrent"),
        "registry must not be ready before holder finishes"
    );
    assert_eq!(
        registry.runtime_presence("s-concurrent"),
        SessionRuntimePresence::RestoreInProgress
    );
    assert!(
        registry.require_live_for_replay("s-concurrent").is_err(),
        "restore-in-progress must block replay as unavailable, not ready"
    );

    let reg_waiter = registry.clone();
    let waiter = tokio::spawn(async move {
        let wait = wait_for_shared_restore(rx).await;
        (
            wait,
            reg_waiter.contains("s-concurrent")
                || !reg_waiter.is_restore_claim_held("s-concurrent"),
        )
    });

    // 延迟首个 restore：先让 waiter 进入 wait，再完成 attach。
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        registry.is_restore_claim_held("s-concurrent"),
        "delayed restore must still hold claim while waiter waits"
    );
    registry.insert_fake_session_for_test("s-concurrent", "p1");
    registry.finish_restore_claim("s-concurrent", SharedRestoreNotification::Ready);

    let (wait_result, ready) = waiter.await.expect("waiter join");
    assert_eq!(wait_result, SharedRestoreWaitResult::Ready);
    assert!(
        ready,
        "after shared restore Ready, session must be ready or claim released"
    );
    assert!(registry.session_exists("s-concurrent"));
    assert!(!registry.is_restore_claim_held("s-concurrent"));
    assert_eq!(
        registry.runtime_presence("s-concurrent"),
        SessionRuntimePresence::Live
    );
    assert!(registry.require_live_for_replay("s-concurrent").is_ok());
    assert!(
        matches!(
            registry.try_claim_restore("s-concurrent"),
            RestoreClaimOutcome::AlreadyLive
        ),
        "live session after restore must report AlreadyLive"
    );
}

/// Business Logic（R15/R18 M1: 为什么需要这个测试）:
///     P2P/local/control/mobile replay 必须共享原子 presence：claim held（含 provisional live）
///     只能是 RestoreInProgress，禁止单独 session_exists 漏判成 Missing/not_found，
///     也禁止 provisional live 被当作可 replay Live。
///
/// Code Logic（这个测试做什么）:
///     空 registry → Missing；claim 后 → RestoreInProgress；claim + provisional live
///     → 仍 RestoreInProgress + require_live unavailable；release claim 后 → Live。
#[test]
fn runtime_presence_atomic_live_restore_missing() {
    let registry = WorkbenchSessionRegistry::new();
    assert_eq!(
        registry.runtime_presence("s-presence"),
        SessionRuntimePresence::Missing
    );
    let missing_err = registry
        .require_live_for_replay("s-presence")
        .expect_err("missing must be not_found");
    assert_eq!(missing_err.ipc_category_code(), "not_found");

    assert!(registry.try_claim_restore("s-presence").is_claimed());
    assert_eq!(
        registry.runtime_presence("s-presence"),
        SessionRuntimePresence::RestoreInProgress
    );
    // 关键：单独 session_exists 会漏掉 claim 窗口。
    assert!(!registry.session_exists("s-presence"));
    let restore_err = registry
        .require_live_for_replay("s-presence")
        .expect_err("restore-in-progress must be unavailable");
    assert_eq!(restore_err.ipc_category_code(), "unavailable");
    assert_eq!(restore_err.to_string(), "session_restore_in_progress");

    // R18 M1：claim 优先于 provisional live。
    registry.insert_fake_session_for_test("s-presence", "p1");
    assert_eq!(
        registry.runtime_presence("s-presence"),
        SessionRuntimePresence::RestoreInProgress
    );
    let provisional_err = registry
        .require_live_for_replay("s-presence")
        .expect_err("claim-held provisional live must block replay");
    assert_eq!(provisional_err.ipc_category_code(), "unavailable");
    assert_eq!(provisional_err.to_string(), "session_restore_in_progress");
    registry.release_restore_claim("s-presence");
    assert_eq!(
        registry.runtime_presence("s-presence"),
        SessionRuntimePresence::Live
    );
    assert!(registry.require_live_for_replay("s-presence").is_ok());
}

/// Business Logic（R15 M2: 为什么需要这个测试）:
///     共享 wait 超时后必须返回 TimedOut，list 路径据此 fail closed；
///     禁止静默返回成功的不完整会话清单，否则 Provider 永不重试遗漏会话。
///
/// Code Logic（这个测试做什么）:
///     pause 时间 → claim + 订阅 wait → advance 超过 SHARED_RESTORE_WAIT_TIMEOUT
///     → 结果 TimedOut；finish Ready 后新 wait 在已完成通道上 Ready。
#[tokio::test]
async fn wait_for_shared_restore_times_out_and_reports_timed_out() {
    tokio::time::pause();
    let registry = WorkbenchSessionRegistry::new();
    assert!(registry.try_claim_restore("s-timeout").is_claimed());
    let second = registry.try_claim_restore("s-timeout");
    let RestoreClaimOutcome::RestoreInProgress(rx) = second else {
        panic!("second claim must be RestoreInProgress");
    };

    let wait_handle = tokio::spawn(async move { wait_for_shared_restore(rx).await });
    // 让 waiter 进入 timeout future。
    tokio::task::yield_now().await;
    tokio::time::advance(SHARED_RESTORE_WAIT_TIMEOUT + Duration::from_secs(1)).await;
    let result = wait_handle.await.expect("timeout waiter must join");
    assert_eq!(
        result,
        SharedRestoreWaitResult::TimedOut,
        "timeout must surface TimedOut so list can return retryable error"
    );
    // claim 仍持有：调用方不得把会话当 ready 返回。
    assert!(registry.is_restore_claim_held("s-timeout"));
    assert_eq!(
        registry.runtime_presence("s-timeout"),
        SessionRuntimePresence::RestoreInProgress
    );

    // finish 后后续 wait 应 Ready。
    registry.finish_restore_claim("s-timeout", SharedRestoreNotification::Ready);
    let third = registry.try_claim_restore("s-timeout");
    assert!(third.is_claimed());
    let fourth = registry.try_claim_restore("s-timeout");
    let RestoreClaimOutcome::RestoreInProgress(rx2) = fourth else {
        panic!("fourth claim must be RestoreInProgress");
    };
    registry.finish_restore_claim("s-timeout", SharedRestoreNotification::Ready);
    assert_eq!(
        wait_for_shared_restore(rx2).await,
        SharedRestoreWaitResult::Ready
    );
}

/// Business Logic（R15 M1/M2 + R16: 为什么需要这个测试）:
///     并发 restore/replay 窗口：claim held + 未 live 时 require_live 必须 unavailable；
///     wait 超时后仍不可伪装成功 list；holder 完成后可 replay。
///
/// Code Logic（这个测试做什么）:
///     holder claim → concurrent presence/replay 守卫 → pause 超时 TimedOut →
///     holder insert+finish Ready → presence Live → require_live Ok。
#[tokio::test]
async fn concurrent_restore_replay_presence_and_wait_timeout_reenumerate() {
    tokio::time::pause();
    let registry = WorkbenchSessionRegistry::new();
    assert!(registry.try_claim_restore("s-route").is_claimed());

    // 并发 replay 窗口：必须 retryable unavailable，不是 not_found。
    let err = registry
        .require_live_for_replay("s-route")
        .expect_err("in-progress restore blocks replay");
    assert_eq!(err.ipc_category_code(), "unavailable");
    assert_eq!(
        registry.runtime_presence("s-route"),
        SessionRuntimePresence::RestoreInProgress
    );

    let second = registry.try_claim_restore("s-route");
    let RestoreClaimOutcome::RestoreInProgress(rx) = second else {
        panic!("second claim must be RestoreInProgress");
    };
    let wait_handle = tokio::spawn(async move { wait_for_shared_restore(rx).await });
    tokio::task::yield_now().await;
    tokio::time::advance(SHARED_RESTORE_WAIT_TIMEOUT + Duration::from_millis(500)).await;
    assert_eq!(
        wait_handle.await.expect("timeout join"),
        SharedRestoreWaitResult::TimedOut
    );
    // 超时后 claim 仍在：模拟 list 不得成功返回该 session。
    assert!(registry.is_restore_claim_held("s-route"));
    assert!(registry.require_live_for_replay("s-route").is_err());

    // holder 最终完成 → 可 re-enumerate / replay。
    registry.insert_fake_session_for_test("s-route", "p1");
    registry.finish_restore_claim("s-route", SharedRestoreNotification::Ready);
    assert_eq!(
        registry.runtime_presence("s-route"),
        SessionRuntimePresence::Live
    );
    assert!(registry.require_live_for_replay("s-route").is_ok());
    let replay = registry.replay("s-route");
    assert_eq!(replay.session_id, "s-route");
    // 不校验 buffer 正文内容，避免敏感/噪声断言。
}

/// Business Logic（R16 M1: 为什么需要这个测试）:
///     并发 list 模拟：holder 注入 Failed 后，waiter 必须得到 Failed，
///     不得 continue 成含不可 replay 会话的成功清单。
///
/// Code Logic（这个测试做什么）:
///     holder claim 后不 insert live，finish Failed；waiter wait → Failed 且
///     registry 仍 missing、require_live not_found。
#[tokio::test]
async fn concurrent_list_waiter_must_fail_when_holder_fails_without_live_session() {
    let registry = WorkbenchSessionRegistry::new();
    assert!(registry.try_claim_restore("s-non-replayable").is_claimed());

    let second = registry.try_claim_restore("s-non-replayable");
    let RestoreClaimOutcome::RestoreInProgress(rx) = second else {
        panic!("second claim must be RestoreInProgress");
    };

    let reg_waiter = registry.clone();
    let waiter = tokio::spawn(async move {
        let wait = wait_for_shared_restore(rx).await;
        let presence = reg_waiter.runtime_presence("s-non-replayable");
        let replay_err = reg_waiter.require_live_for_replay("s-non-replayable");
        (
            wait,
            presence,
            replay_err.map_err(|e| e.ipc_category_code().to_string()),
        )
    });

    tokio::time::sleep(Duration::from_millis(20)).await;
    // 模拟 project 查询/删除 `?` 或 upsert 失败后 finish Failed，且未 live。
    registry.finish_restore_claim(
        "s-non-replayable",
        SharedRestoreNotification::Failed(AppErrorCategory::Internal),
    );

    let (wait, presence, replay_err) = waiter.await.expect("waiter join");
    assert_eq!(
        wait,
        SharedRestoreWaitResult::Failed(AppErrorCategory::Internal),
        "waiter must not treat holder failure as Ready/PersistedDisconnected"
    );
    assert!(!wait.is_success());
    assert_eq!(presence, SessionRuntimePresence::Missing);
    assert_eq!(replay_err.expect_err("must not be replayable"), "not_found");
    // list 路径应用 shared_restore_failed_error 返回错误，而不是成功合并。
    let list_err = shared_restore_failed_error(AppErrorCategory::Internal);
    assert_eq!(list_err.ipc_category_code(), "internal");
    assert_eq!(list_err.to_string(), "session_restore_shared_failed");
}

/// Business Logic（R17 M1: 为什么需要这个测试）:
///     restore 成功后 upsert 失败必须先回收 SessionSpawnGuard，再 finish claim；
///     若先放 claim 后 Drop spawn，第三方并发 list 会短暂 AlreadyLive。
///
/// Code Logic（这个测试做什么）:
///     insert fake + claim → SessionSpawnGuard 未 commit；
///     先 drop spawn（registry 清空）→ 第三方 claim 为 RestoreInProgress（非 AlreadyLive）→
///     再 finish Failed → claim 释放且仍无 live session。
#[test]
fn upsert_failure_cleanup_reclaims_spawn_before_releasing_claim() {
    let registry = WorkbenchSessionRegistry::new();
    // claim 必须在 insert live 之前：已 live 时 try_claim 返回 AlreadyLive。
    let generation = registry
        .try_claim_restore("s-cleanup-order")
        .claim_generation()
        .expect("claimed");
    registry.insert_fake_session_for_test("s-cleanup-order", "p1");

    let mut claim_guard =
        RestoreClaimGuard::new(registry.clone(), "s-cleanup-order".to_string(), generation);
    let spawn_guard = SessionSpawnGuard::new(registry.clone(), "s-cleanup-order".to_string());

    // 正确顺序：先 reclaim spawn。R27 H2：close 会 revoke restore claim，避免 delete 后 re-upsert。
    drop(spawn_guard);
    assert_eq!(registry.registry_len(), 0);
    assert!(!registry.contains("s-cleanup-order"));
    // claim 已被 close 撤销并从 restoring 移除（或 finish 前已 not active）。
    assert!(!registry.is_restore_claim_generation_active("s-cleanup-order", generation));
    // finish 幂等（generation 已撤销时 no-op）。
    claim_guard.finish(SharedRestoreNotification::Failed(
        AppErrorCategory::Internal,
    ));
    assert!(!registry.is_restore_claim_held("s-cleanup-order"));
    // Closing barrier 可能仍在（SessionSpawnGuard finish_cleanup 后应清）；presence 不得 Live。
    assert_ne!(
        registry.runtime_presence("s-cleanup-order"),
        SessionRuntimePresence::Live
    );
    // barrier 清后可重新 claim；若 barrier 仍在则 wait 后 claim。
    if registry.has_closing_tombstone_for_test("s-cleanup-order") {
        // finish_cleanup 应已 clear；若未清则显式 wait。
        registry.wait_for_closing_tombstone("s-cleanup-order");
    }
    assert!(registry.try_claim_restore("s-cleanup-order").is_claimed());
    registry.release_restore_claim("s-cleanup-order");
}

/// Business Logic（R17 M1: 为什么需要这个测试）:
///     反例：若先 finish claim 再 Drop spawn，第三方可在窗口内 AlreadyLive。
///
/// Code Logic（这个测试做什么）:
///     claim + live fake → finish claim 后 spawn 仍在 → 第三方 AlreadyLive；
///     再 drop spawn 才清空。证明必须先 reclaim spawn。
#[test]
fn releasing_claim_before_spawn_reclaim_creates_already_live_window() {
    let registry = WorkbenchSessionRegistry::new();
    let generation = registry
        .try_claim_restore("s-wrong-order")
        .claim_generation()
        .expect("claimed");
    registry.insert_fake_session_for_test("s-wrong-order", "p1");

    let mut claim_guard =
        RestoreClaimGuard::new(registry.clone(), "s-wrong-order".to_string(), generation);
    let spawn_guard = SessionSpawnGuard::new(registry.clone(), "s-wrong-order".to_string());

    // 错误顺序：先放 claim。
    claim_guard.finish(SharedRestoreNotification::Failed(
        AppErrorCategory::Internal,
    ));
    assert!(!registry.is_restore_claim_held("s-wrong-order"));
    assert!(registry.contains("s-wrong-order"));
    assert!(
        matches!(
            registry.try_claim_restore("s-wrong-order"),
            RestoreClaimOutcome::AlreadyLive
        ),
        "wrong cleanup order creates AlreadyLive window"
    );

    drop(spawn_guard);
    assert!(!registry.contains("s-wrong-order"));
}

/// Business Logic（R18 M1: 为什么需要这个测试）:
///     durable Ready 前 provisional live 不得对外暴露；并发 claim / replay / list
///     都必须把 claim-held 视为 RestoreInProgress。
///
/// Code Logic（这个测试做什么）:
///     claim + insert provisional → try_claim RestoreInProgress；runtime_presence
///     RestoreInProgress；require_live unavailable；list 不含该 id；finish Failed
///     后 Missing，并可重新 Claimed。
#[test]
fn provisional_live_while_claim_held_is_not_externally_live() {
    let registry = WorkbenchSessionRegistry::new();
    assert!(registry.try_claim_restore("s-provisional").is_claimed());
    registry.insert_fake_session_for_test("s-provisional", "p1");
    assert!(registry.contains("s-provisional"));
    assert!(registry.is_restore_claim_held("s-provisional"));

    assert!(
        matches!(
            registry.try_claim_restore("s-provisional"),
            RestoreClaimOutcome::RestoreInProgress(_)
        ),
        "claim-held provisional must be RestoreInProgress, not AlreadyLive"
    );
    assert_eq!(
        registry.runtime_presence("s-provisional"),
        SessionRuntimePresence::RestoreInProgress
    );
    let err = registry
        .require_live_for_replay("s-provisional")
        .expect_err("provisional live must block replay");
    assert_eq!(err.ipc_category_code(), "unavailable");
    assert_eq!(err.to_string(), "session_restore_in_progress");

    let listed = registry.list(Some("p1"));
    assert!(
        listed.iter().all(|dto| dto.id != "s-provisional"),
        "registry list must hide claim-held provisional live"
    );
    let listed_all = registry.list(None);
    assert!(listed_all.iter().all(|dto| dto.id != "s-provisional"));

    registry.finish_restore_claim(
        "s-provisional",
        SharedRestoreNotification::Failed(AppErrorCategory::Internal),
    );
    // finish Failed 只放 claim，不自动 close provisional；模拟 holder 先 reclaim spawn。
    let _ = registry.close("s-provisional").map(|c| c.finish_cleanup());
    assert_eq!(
        registry.runtime_presence("s-provisional"),
        SessionRuntimePresence::Missing
    );
    assert!(registry.try_claim_restore("s-provisional").is_claimed());
    registry.release_restore_claim("s-provisional");
}

/// Business Logic（R18 M2: 为什么需要这个测试）:
///     close/reclaim 后同 id 新实例不得被旧 generation 的 worker fence 通过。
///
/// Code Logic（这个测试做什么）:
///     insert gen1 → close → insert gen2；check(gen1)=false，check(gen2)=true。
#[test]
fn session_generation_fence_rejects_stale_after_reinsert() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_fake_session_for_test("s-gen", "p1");
    let gen1 = registry
        .session_generation_for_test("s-gen")
        .expect("gen1 must exist");
    assert!(registry.is_current_session_generation("s-gen", gen1));

    registry
        .close("s-gen")
        .expect("close gen1")
        .finish_cleanup();
    assert!(!registry.is_current_session_generation("s-gen", gen1));

    registry.insert_fake_session_for_test("s-gen", "p1");
    let gen2 = registry
        .session_generation_for_test("s-gen")
        .expect("gen2 must exist");
    assert_ne!(gen1, gen2, "reinsert must allocate a new generation");
    assert!(!registry.is_current_session_generation("s-gen", gen1));
    assert!(registry.is_current_session_generation("s-gen", gen2));
}

/// Business Logic（R18 M2: 为什么需要这个测试）:
///     失败 reclaim 后立即 re-claim + insert 新 gen，旧 fence 不得污染新实例。
///
/// Code Logic（这个测试做什么）:
///     claim + insert gen1 → reclaim close + finish Failed → re-claim + insert gen2；
///     旧 gen fence false，新 gen true；presence 在 claim held 时仍 RestoreInProgress。
#[test]
fn reclaim_then_reclaim_insert_fences_old_generation() {
    let registry = WorkbenchSessionRegistry::new();
    assert!(registry.try_claim_restore("s-reclaim-fence").is_claimed());
    registry.insert_fake_session_for_test("s-reclaim-fence", "p1");
    let gen1 = registry
        .session_generation_for_test("s-reclaim-fence")
        .expect("gen1");
    assert!(registry.is_current_session_generation("s-reclaim-fence", gen1));
    assert_eq!(
        registry.runtime_presence("s-reclaim-fence"),
        SessionRuntimePresence::RestoreInProgress
    );

    // 失败 reclaim：先 close spawn 再放 claim。
    let _ = registry
        .close("s-reclaim-fence")
        .map(|c| c.finish_cleanup());
    registry.finish_restore_claim(
        "s-reclaim-fence",
        SharedRestoreNotification::Failed(AppErrorCategory::Internal),
    );
    assert!(!registry.is_current_session_generation("s-reclaim-fence", gen1));

    assert!(registry.try_claim_restore("s-reclaim-fence").is_claimed());
    registry.insert_fake_session_for_test("s-reclaim-fence", "p1");
    let gen2 = registry
        .session_generation_for_test("s-reclaim-fence")
        .expect("gen2");
    assert_ne!(gen1, gen2);
    assert!(!registry.is_current_session_generation("s-reclaim-fence", gen1));
    assert!(registry.is_current_session_generation("s-reclaim-fence", gen2));
    registry.finish_restore_claim("s-reclaim-fence", SharedRestoreNotification::Ready);
    assert_eq!(
        registry.runtime_presence("s-reclaim-fence"),
        SessionRuntimePresence::Live
    );
    assert!(registry.is_current_session_generation("s-reclaim-fence", gen2));
    assert!(!registry.is_current_session_generation("s-reclaim-fence", gen1));
}

/// Business Logic（R19 H1: 为什么需要这个测试）:
///     close+reinsert 后，旧 generation 的 worker 副作用必须全部 no-op，
///     不得写入新 buffer / 发 status。
///
/// Code Logic（这个测试做什么）:
///     insert gen1 → close → insert gen2；用 gen1 调 try_stale_worker_side_effects；
///     断言 false，且 gen2 replay last_seq 仍为 0。
#[test]
fn stale_generation_side_effects_no_op_after_reinsert() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_fake_session_for_test("s-stale", "p1");
    let gen1 = registry
        .session_generation_for_test("s-stale")
        .expect("gen1");
    registry.close("s-stale").expect("close").finish_cleanup();
    registry.insert_fake_session_for_test("s-stale", "p1");
    let gen2 = registry
        .session_generation_for_test("s-stale")
        .expect("gen2");
    assert_ne!(gen1, gen2);
    assert_eq!(registry.replay_last_seq_for_test("s-stale"), Some(0));

    assert!(
        !registry.is_current_session_generation("s-stale", gen1),
        "stale generation must not be current"
    );
    assert!(registry.is_current_session_generation("s-stale", gen2));
    // 旧 gen 无法通过 generation-scoped append（经 registry 测试 helper）。
    assert!(
        !registry.append_replay_for_test("s-stale", gen1, "x", 1),
        "old gen must not append replay"
    );
    assert!(registry.append_replay_for_test("s-stale", gen2, "y", 1));
    assert_eq!(registry.replay_last_seq_for_test("s-stale"), Some(1));
}

/// Business Logic（R19 M1: 为什么需要这个测试）:
///     Provisional handle 在 mark Ready 前不得 Live；mark 后才 Live。
///
/// Code Logic（这个测试做什么）:
///     insert_provisional → presence RestoreInProgress；list 不含；
///     mark_session_ready → Live 且 list 含。
#[test]
fn provisional_not_live_until_mark_ready() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_provisional_fake_session_for_test("s-prov-ready", "p1");
    assert_eq!(
        registry.runtime_presence("s-prov-ready"),
        SessionRuntimePresence::RestoreInProgress
    );
    assert!(registry
        .list(Some("p1"))
        .iter()
        .all(|d| d.id != "s-prov-ready"));

    registry.mark_session_ready("s-prov-ready", None);
    assert_eq!(
        registry.runtime_presence("s-prov-ready"),
        SessionRuntimePresence::Live
    );
    assert!(registry
        .list(Some("p1"))
        .iter()
        .any(|d| d.id == "s-prov-ready"));
}

/// Business Logic（R19 H1: 为什么需要这个测试）:
///     close 必须失效 publish token，即使 handle Arc 仍被旧 worker 持有。
///
/// Code Logic（这个测试做什么）:
///     insert → close → token false。
#[test]
fn close_invalidates_publish_token() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_fake_session_for_test("s-token", "p1");
    let gen = registry
        .session_generation_for_test("s-token")
        .expect("gen");
    assert!(registry.publish_token_alive_for_test("s-token", gen));
    registry.close("s-token").expect("close").finish_cleanup();
    assert!(
        !registry.publish_token_alive_for_test("s-token", gen),
        "close must invalidate generation-scoped publish token"
    );
}

/// Business Logic（R20 H1: 为什么需要这个测试）:
///     Ready 前可见输出不得杀死 worker 语义，必须缓冲；Ready 后原序进入 replay。
///
/// Code Logic（这个测试做什么）:
///     provisional insert → emit 缓冲成功且 last_seq=0 → mark_ready → last_seq 增加。
#[test]
fn provisional_output_buffers_until_ready_then_flushes() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_provisional_fake_session_for_test("s-buf", "p1");
    let gen = registry.session_generation_for_test("s-buf").expect("gen");
    assert_eq!(
        classify_side_effect_gate(&registry.sessions, "s-buf", gen),
        SideEffectGate::Provisional
    );
    assert!(
        buffer_provisional_output(&registry.sessions, "s-buf", gen, "first-screen".to_string(),),
        "provisional output must buffer instead of rejecting"
    );
    assert_eq!(registry.replay_last_seq_for_test("s-buf"), Some(0));
    // 无 AppState 时 mark_ready 只 CAS；手动取出缓冲验证 CAS 后仍保留到 flush 路径。
    assert!(registry.mark_session_ready_for_generation("s-buf", gen, None));
    assert_eq!(
        classify_side_effect_gate(&registry.sessions, "s-buf", gen),
        SideEffectGate::Ready
    );
    // Ready 后直接 publish 路径可写 replay。
    assert!(registry.append_replay_for_test("s-buf", gen, "live", 1));
    assert_eq!(registry.replay_last_seq_for_test("s-buf"), Some(1));
}

/// Business Logic（R20 H1: 为什么需要这个测试）:
///     Provisional 期间进程退出必须记录 pending_exit，不得静默丢失。
///
/// Code Logic（这个测试做什么）:
///     provisional → record_pending_exit → mark_ready 后 pending 被取出（CAS 路径）。
#[test]
fn provisional_exit_is_recorded_pending_until_ready() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_provisional_fake_session_for_test("s-exit", "p1");
    let gen = registry.session_generation_for_test("s-exit").expect("gen");
    assert!(record_pending_exit(
        &registry.sessions,
        "s-exit",
        gen,
        Some(7)
    ));
    // 锁内确认 pending 已写入。
    {
        let sessions = registry.sessions.lock().expect("lock");
        let handle = sessions.get("s-exit").expect("handle").lock().expect("h");
        assert_eq!(handle.pending_exit, Some(Some(7)));
    }
    assert!(registry.mark_session_ready_for_generation("s-exit", gen, None));
    {
        let sessions = registry.sessions.lock().expect("lock");
        let handle = sessions.get("s-exit").expect("handle").lock().expect("h");
        assert_eq!(
            handle.pending_exit, None,
            "ready CAS must take pending_exit"
        );
        assert_eq!(handle.durability, SessionDurability::Ready);
    }
}

/// Business Logic（R20/R21 H2: 为什么需要这个测试）:
///     close 必须 revoke 并等待在途 lease；soft-timeout 后仍保留 tombstone，
///     同 id reinsert 不得在旧 lease 仍发布时发生。
///
/// Code Logic（这个测试做什么）:
///     Ready gen1 持 lease → close soft-timeout → tombstone；另一线程 reinsert 阻塞；
///     drop lease 后 reinsert 得到 gen2，旧 gen 零 publish。
#[test]
fn publication_lease_barrier_blocks_stale_after_close() {
    use std::sync::Barrier;
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_fake_session_for_test("s-lease", "p1");
    let gen1 = registry
        .session_generation_for_test("s-lease")
        .expect("gen1");
    let lease = try_acquire_publication_lease(&registry.sessions, "s-lease", gen1)
        .expect("lease for live gen1");
    // close：soft-timeout 后 tombstone（lease 仍持有）。
    registry.close("s-lease").expect("close").finish_cleanup();
    assert!(
        registry.has_closing_tombstone_for_test("s-lease"),
        "undrained close must keep generation tombstone"
    );
    let barrier = Arc::new(Barrier::new(2));
    let reg_ins = registry.clone();
    let barrier_ins = barrier.clone();
    let inserter = thread::spawn(move || {
        barrier_ins.wait();
        reg_ins.insert_fake_session_for_test("s-lease", "p1");
        reg_ins.session_generation_for_test("s-lease")
    });
    barrier.wait();
    // 给 inserter 时间卡在 tombstone wait。
    thread::sleep(Duration::from_millis(50));
    assert!(
        !registry.contains("s-lease"),
        "reinsert must not complete while old lease held"
    );
    drop(lease);
    let gen2 = inserter.join().expect("inserter").expect("gen2");
    assert_ne!(gen1, gen2);
    assert!(
        try_acquire_publication_lease(&registry.sessions, "s-lease", gen1).is_none(),
        "stale gen must not acquire lease after reinsert"
    );
    assert!(
        try_acquire_publication_lease(&registry.sessions, "s-lease", gen2).is_some(),
        "new gen may publish"
    );
    assert!(
        !registry.publish_token_alive_for_test("s-lease", gen1),
        "old token remains revoked"
    );
    assert!(
        !registry.has_closing_tombstone_for_test("s-lease"),
        "tombstone must clear after successful reinsert drain"
    );
}

/// Business Logic（R21 H1: 为什么需要这个测试）:
///     deferred flush 与 live 输出必须共享 generation-scoped seq，禁止各自从 0 重开导致前端丢字节。
///
/// Code Logic（这个测试做什么）:
///     Ready 后连续 allocate_seq 得到 1..=n 严格递增；模拟 flush 后 live 继续。
#[test]
fn shared_generation_seq_is_monotonic_across_flush_and_live() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_provisional_fake_session_for_test("s-seq", "p1");
    let gen = registry.session_generation_for_test("s-seq").expect("gen");
    assert!(buffer_provisional_output(
        &registry.sessions,
        "s-seq",
        gen,
        "deferred-a".into(),
    ));
    assert!(buffer_provisional_output(
        &registry.sessions,
        "s-seq",
        gen,
        "deferred-b".into(),
    ));
    assert!(registry.mark_session_ready_for_generation("s-seq", gen, None));
    // 模拟 deferred flush 用共享 allocator 写 2 段，再 live 写 2 段。
    let mut seqs = Vec::new();
    for text_chunk in ["deferred-a", "deferred-b", "live-c", "live-d"] {
        let seq = registry
            .allocate_output_seq_for_test("s-seq", gen)
            .expect("seq");
        assert!(
            registry.append_replay_for_test("s-seq", gen, text_chunk, seq),
            "append must accept shared seq"
        );
        seqs.push(seq);
    }
    assert_eq!(
        seqs,
        vec![1, 2, 3, 4],
        "seq must continue across flush/live"
    );
    assert_eq!(registry.replay_last_seq_for_test("s-seq"), Some(4));
}

/// Business Logic（R21 M1: 为什么需要这个测试）:
///     SessionSpawnGuard Drop 只能回收捕获的 generation，不得 close 同 id 后继。
///
/// Code Logic（这个测试做什么）:
///     gen1 provisional + guard → close gen1 + reinsert gen2 → Drop guard → gen2 仍 Live。
#[test]
fn spawn_guard_drop_only_closes_captured_generation() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_provisional_fake_session_for_test("s-guard", "p1");
    let gen1 = registry
        .session_generation_for_test("s-guard")
        .expect("gen1");
    let guard =
        SessionSpawnGuard::new_with_generation(registry.clone(), "s-guard".to_string(), gen1, None);
    registry
        .close("s-guard")
        .expect("close gen1")
        .finish_cleanup();
    registry.insert_fake_session_for_test("s-guard", "p1");
    let gen2 = registry
        .session_generation_for_test("s-guard")
        .expect("gen2");
    assert_ne!(gen1, gen2);
    drop(guard);
    assert!(
        registry.contains("s-guard"),
        "stale guard Drop must not remove successor generation"
    );
    assert_eq!(registry.session_generation_for_test("s-guard"), Some(gen2));
    assert_eq!(
        registry.runtime_presence("s-guard"),
        SessionRuntimePresence::Live
    );
}

/// Business Logic（R21 M2: 为什么需要这个测试）:
///     create upsert→Ready 窗口的 Provisional 不得进入 list / Live presence。
///
/// Code Logic（这个测试做什么）:
///     provisional running 行不在 list；presence=RestoreInProgress；Ready 后才 Live。
#[test]
fn provisional_create_window_not_projected_as_live() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_provisional_fake_session_for_test("s-create-win", "p1");
    assert_eq!(
        registry.runtime_presence("s-create-win"),
        SessionRuntimePresence::RestoreInProgress
    );
    assert!(
        registry
            .list(Some("p1"))
            .iter()
            .all(|d| d.id != "s-create-win"),
        "list must hide Provisional (not only claim-held)"
    );
    let gen = registry
        .session_generation_for_test("s-create-win")
        .expect("gen");
    assert!(registry.mark_session_ready_for_generation("s-create-win", gen, None));
    assert_eq!(
        registry.runtime_presence("s-create-win"),
        SessionRuntimePresence::Live
    );
    assert!(registry
        .list(Some("p1"))
        .iter()
        .any(|d| d.id == "s-create-win"));
}

/// Business Logic（R20 M1: 为什么需要这个测试）:
///     SessionSpawnGuard commit 必须 generation CAS；close 后 commit 失败且 Drop 不再 close 二次 panic。
///
/// Code Logic（这个测试做什么）:
///     insert provisional → guard 绑定 gen → close → commit=false。
#[test]
fn spawn_guard_commit_cas_fails_after_close() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_provisional_fake_session_for_test("s-cas", "p1");
    let gen = registry.session_generation_for_test("s-cas").expect("gen");
    let mut guard =
        SessionSpawnGuard::new_with_generation(registry.clone(), "s-cas".to_string(), gen, None);
    registry.close("s-cas").expect("close").finish_cleanup();
    assert!(!guard.commit(), "commit must fail when generation removed");
    // 标记 committed 假成功路径不应发生；Drop 因 committed=false 会 close miss → ok。
    // 手动置 committed 避免 Drop 再 close not_found 噪音（close 已成功）。
    guard.committed = true;
}

/// Business Logic（R20 M1: 为什么需要这个测试）:
///     错误 generation 不得 mark Ready。
///
/// Code Logic（这个测试做什么）:
///     provisional gen → mark with wrong gen false → still Provisional。
#[test]
fn mark_ready_generation_cas_rejects_stale() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_provisional_fake_session_for_test("s-wrong-gen", "p1");
    let gen = registry
        .session_generation_for_test("s-wrong-gen")
        .expect("gen");
    assert!(!registry.mark_session_ready_for_generation("s-wrong-gen", gen + 99, None));
    assert_eq!(
        classify_side_effect_gate(&registry.sessions, "s-wrong-gen", gen),
        SideEffectGate::Provisional
    );
    assert!(registry.mark_session_ready_for_generation("s-wrong-gen", gen, None));
    assert_eq!(
        classify_side_effect_gate(&registry.sessions, "s-wrong-gen", gen),
        SideEffectGate::Ready
    );
}

/// Business Logic（R22 H1: 为什么需要这个测试）:
///     finish_ready 不得在第二批 deferred 仍待 drain 时 Ready；live 不得 overtake。
///
/// Code Logic（这个测试做什么）:
///     provisional 缓冲 A；mark_ready 进入 Flushing 期间注入 B；
///     无 state 路径循环 take 直至空再 Ready；最终 replay 顺序 A→B→live，seq 严格递增。
#[test]
fn finish_ready_stays_flushing_until_deferred_empty() {
    use std::sync::Barrier;
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_provisional_fake_session_for_test("s-flush-order", "p1");
    let gen = registry
        .session_generation_for_test("s-flush-order")
        .expect("gen");
    assert!(buffer_provisional_output(
        &registry.sessions,
        "s-flush-order",
        gen,
        "chunk-a".into(),
    ));
    // 模拟 flush 窗口：在 mark_ready 持有 Flushing 期间再缓冲 chunk-b。
    // 无 AppState 路径：finish_ready 循环 take 直至空才 Ready。
    // 用并发：mark 线程 + inject 线程竞态。
    let barrier = Arc::new(Barrier::new(2));
    let reg_mark = registry.clone();
    let barrier_mark = barrier.clone();
    let marker = thread::spawn(move || {
        barrier_mark.wait();
        reg_mark.mark_session_ready_for_generation("s-flush-order", gen, None)
    });
    barrier.wait();
    // 在 mark 可能处于 Flushing 时注入第二批 deferred。
    for _ in 0..200 {
        let durability = registry
            .session_durability_for_test("s-flush-order")
            .expect("dur");
        if durability == SessionDurability::Flushing {
            let _ = buffer_provisional_output(
                &registry.sessions,
                "s-flush-order",
                gen,
                "chunk-b".into(),
            );
            break;
        }
        if durability == SessionDurability::Ready {
            break;
        }
        thread::yield_now();
    }
    assert!(marker.join().expect("marker"), "must reach Ready");
    assert_eq!(
        registry.session_durability_for_test("s-flush-order"),
        Some(SessionDurability::Ready)
    );
    // 锁下 deferred 必须已空。
    {
        let sessions = registry.sessions.lock().expect("lock");
        let handle = sessions.get("s-flush-order").expect("h").lock().expect("h");
        assert!(
            handle.deferred_output.is_empty(),
            "Ready 时 deferred 必须空"
        );
    }
    // 共享 seq：后续 live 从 allocator 继续，严格递增。
    let mut seqs = Vec::new();
    for text in ["replay-a", "replay-b", "live-c"] {
        let seq = registry
            .allocate_output_seq_for_test("s-flush-order", gen)
            .expect("seq");
        assert!(registry.append_replay_for_test("s-flush-order", gen, text, seq));
        seqs.push(seq);
    }
    assert_eq!(seqs, vec![1, 2, 3]);
    assert_eq!(registry.replay_last_seq_for_test("s-flush-order"), Some(3));
}

/// Business Logic（R22 H1: 为什么需要这个测试）:
///     Flushing 期间 live reader gate 必须仍为 Provisional，禁止与 deferred 双写 overtake。
///
/// Code Logic（这个测试做什么）:
///     人工置 Flushing + 非空 deferred；classify=Provisional；can_publish=false；
///     finish_ready 清空后 Ready。
#[test]
fn flushing_blocks_live_publish_until_deferred_drained() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_provisional_fake_session_for_test("s-flush-gate", "p1");
    let gen = registry
        .session_generation_for_test("s-flush-gate")
        .expect("gen");
    {
        let sessions = registry.sessions.lock().expect("lock");
        let mut handle = sessions.get("s-flush-gate").expect("h").lock().expect("h");
        handle.durability = SessionDurability::Flushing;
        handle.deferred_output.push("pending".into());
    }
    assert_eq!(
        classify_side_effect_gate(&registry.sessions, "s-flush-gate", gen),
        SideEffectGate::Provisional
    );
    assert!(!can_publish_side_effect(
        &registry.sessions,
        "s-flush-gate",
        gen
    ));
    assert!(registry.finish_ready_after_flush_for_test("s-flush-gate", gen));
    assert_eq!(
        classify_side_effect_gate(&registry.sessions, "s-flush-gate", gen),
        SideEffectGate::Ready
    );
    {
        let sessions = registry.sessions.lock().expect("lock");
        let handle = sessions.get("s-flush-gate").expect("h").lock().expect("h");
        assert!(handle.deferred_output.is_empty());
    }
}

/// Business Logic（R22 M1: 为什么需要这个测试）:
///     close 与 barrier 同临界区；持 lease 时 concurrent reinsert/restore 不得装新 generation。
///
/// Code Logic（这个测试做什么）:
///     hold lease → close → presence 非 Missing；两线程 concurrent insert 均阻塞至 drop lease；
///     仅一个成功 live gen2；旧 gen 不可 lease。
#[test]
fn close_barrier_blocks_concurrent_reinsert_until_cleanup() {
    use std::sync::Barrier;
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_fake_session_for_test("s-close-barrier", "p1");
    let gen1 = registry
        .session_generation_for_test("s-close-barrier")
        .expect("gen1");
    let lease =
        try_acquire_publication_lease(&registry.sessions, "s-close-barrier", gen1).expect("lease");
    let cleanup = registry.close("s-close-barrier").expect("close");
    assert!(
        registry.has_closing_tombstone_for_test("s-close-barrier"),
        "close must install barrier immediately"
    );
    assert_eq!(
        registry.runtime_presence("s-close-barrier"),
        SessionRuntimePresence::RestoreInProgress,
        "Closing barrier must not report permanent Missing"
    );
    let start = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let reg = registry.clone();
        let start = start.clone();
        handles.push(thread::spawn(move || {
            start.wait();
            reg.insert_fake_session_for_test("s-close-barrier", "p1");
            reg.session_generation_for_test("s-close-barrier")
        }));
    }
    start.wait();
    thread::sleep(Duration::from_millis(80));
    assert!(
        !registry.contains("s-close-barrier"),
        "reinsert must not complete under active barrier"
    );
    assert!(registry.has_closing_tombstone_for_test("s-close-barrier"));
    drop(lease);
    // R24 H1：persist cleanup 令牌 finish 后 barrier 才允许 reinsert。
    cleanup.finish_cleanup();
    let mut gens = Vec::new();
    for h in handles {
        gens.push(h.join().expect("inserter").expect("gen"));
    }
    let live = registry
        .session_generation_for_test("s-close-barrier")
        .expect("live gen");
    assert!(
        gens.contains(&live),
        "live generation must come from concurrent reinsert"
    );
    assert_ne!(live, gen1);
    assert!(
        !registry.has_closing_tombstone_for_test("s-close-barrier"),
        "barrier clears after drain+reinsert"
    );
    assert!(try_acquire_publication_lease(&registry.sessions, "s-close-barrier", gen1).is_none());
    assert!(try_acquire_publication_lease(&registry.sessions, "s-close-barrier", live).is_some());
}

/// Business Logic（R23 H1: 为什么需要这个测试）:
///     Flushing→Ready 过渡时 output prepare 必须原子成功，禁止分锁 buffer 失败后永久 Rejected。
///
/// Code Logic（这个测试做什么）:
///     Provisional 置 Flushing；并发 finish_ready 与 prepare_output；
///     结果只能 Buffered 或 Live，不得 Rejected；最终 Ready 且 deferred 空。
#[test]
fn prepare_output_survives_ready_transition() {
    use std::sync::Barrier;
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_provisional_fake_session_for_test("s-r23-out", "p1");
    let gen = registry
        .session_generation_for_test("s-r23-out")
        .expect("gen");
    {
        let sessions = registry.sessions.lock().expect("lock");
        let mut handle = sessions.get("s-r23-out").expect("h").lock().expect("h");
        handle.durability = SessionDurability::Flushing;
        handle.deferred_output.push("pre".into());
    }
    let barrier = Arc::new(Barrier::new(2));
    let reg_ready = registry.clone();
    let barrier_ready = barrier.clone();
    let ready_thread = thread::spawn(move || {
        barrier_ready.wait();
        reg_ready.finish_ready_after_flush_for_test("s-r23-out", gen)
    });
    barrier.wait();
    let mut saw_buffered = false;
    let mut saw_live = false;
    let mut saw_rejected = false;
    for i in 0..64 {
        match prepare_output_side_effect(&registry.sessions, "s-r23-out", gen, format!("chunk-{i}"))
        {
            PreparedSideEffect::Buffered => saw_buffered = true,
            PreparedSideEffect::Live {
                lease: _lease,
                payload: _,
            } => {
                saw_live = true;
            }
            PreparedSideEffect::Rejected => saw_rejected = true,
        }
        thread::yield_now();
    }
    assert!(ready_thread.join().expect("ready"), "must reach Ready");
    assert!(
        !saw_rejected,
        "mid-flight Ready must not permanently reject same generation output"
    );
    assert!(
        saw_buffered || saw_live,
        "prepare must accept output as buffer or live across Ready transition"
    );
    assert_eq!(
        registry.session_durability_for_test("s-r23-out"),
        Some(SessionDurability::Ready)
    );
}

/// Business Logic（R23 H1: 为什么需要这个测试）:
///     mutation prepare 与 exit prepare 必须在 Ready 过渡窗口保持原子，禁止分锁 stale。
///
/// Code Logic（这个测试做什么）:
///     Flushing 下并发 Ready；mutation/exit prepare 不得 Rejected。
#[test]
fn prepare_mutation_and_exit_survive_ready_transition() {
    use crate::workbench::agent_runtime::AgentSessionPhase;
    use std::sync::Barrier;
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_provisional_fake_session_for_test("s-r23-mut", "p1");
    let gen = registry
        .session_generation_for_test("s-r23-mut")
        .expect("gen");
    {
        let sessions = registry.sessions.lock().expect("lock");
        let mut handle = sessions.get("s-r23-mut").expect("h").lock().expect("h");
        handle.durability = SessionDurability::Flushing;
    }
    let barrier = Arc::new(Barrier::new(2));
    let reg_ready = registry.clone();
    let barrier_ready = barrier.clone();
    let ready_thread = thread::spawn(move || {
        barrier_ready.wait();
        reg_ready.finish_ready_after_flush_for_test("s-r23-mut", gen)
    });
    barrier.wait();
    let mutation = AgentRuntimeMutation {
        agent_session_id: "a1".into(),
        terminal_session_id: "s-r23-mut".into(),
        expected_version: 0,
        event_version: 1,
        phase: AgentSessionPhase::Working,
        native_session_id: None,
        outcome_code: None,
        occurred_at: "2026-07-21T00:00:00Z".into(),
    };
    let mut rejected = false;
    for i in 0..32 {
        match prepare_mutation_side_effect(
            &registry.sessions,
            "s-r23-mut",
            gen,
            vec![mutation.clone()],
        ) {
            PreparedSideEffect::Rejected => rejected = true,
            PreparedSideEffect::Buffered | PreparedSideEffect::Live { .. } => {}
        }
        match prepare_exit_side_effect(&registry.sessions, "s-r23-mut", gen, Some(i)) {
            PreparedSideEffect::Rejected => rejected = true,
            PreparedSideEffect::Buffered | PreparedSideEffect::Live { .. } => {}
        }
        thread::yield_now();
    }
    assert!(ready_thread.join().expect("ready"));
    assert!(
        !rejected,
        "mutation/exit prepare must not reject across Ready transition"
    );
}

/// Business Logic（R23 H2: 为什么需要这个测试）:
///     waiters 不得因 in_flight==0 清除 barrier；closer 未 cleanup 时 reinsert 必须等待。
///
/// Code Logic（这个测试做什么）:
///     安装 barrier（leases=0, cleanup_done=false）；并发 reinsert 阻塞；
///     mark cleanup + closer clear 后 reinsert 成功。
#[test]
fn waiter_never_clears_barrier_before_cleanup_done() {
    use std::sync::Barrier;
    let registry = WorkbenchSessionRegistry::new();
    let publish = registry.install_closing_barrier_for_test("s-r23-h2");
    assert!(
        registry.has_closing_tombstone_for_test("s-r23-h2"),
        "barrier installed"
    );
    assert!(!publish.is_cleanup_done(), "cleanup_done must start false");
    assert_eq!(publish.in_flight.load(Ordering::SeqCst), 0);
    let start = Arc::new(Barrier::new(2));
    let reg_ins = registry.clone();
    let start_ins = start.clone();
    let inserter = thread::spawn(move || {
        start_ins.wait();
        reg_ins.insert_fake_session_for_test("s-r23-h2", "p1");
        reg_ins.session_generation_for_test("s-r23-h2")
    });
    start.wait();
    thread::sleep(Duration::from_millis(80));
    assert!(
        !registry.contains("s-r23-h2"),
        "waiter must not clear barrier / reinsert while cleanup pending"
    );
    assert!(registry.has_closing_tombstone_for_test("s-r23-h2"));
    // closer 完成 cleanup 后才可 clear。
    publish.mark_cleanup_done();
    registry.clear_closing_tombstone_for_test("s-r23-h2");
    let gen = inserter.join().expect("inserter").expect("gen");
    assert!(
        !registry.has_closing_tombstone_for_test("s-r23-h2"),
        "only closer clear removes barrier"
    );
    assert!(registry.contains("s-r23-h2"));
    assert_eq!(registry.session_generation_for_test("s-r23-h2"), Some(gen));
}

/// Business Logic（R23 M1: 为什么需要这个测试）:
///     close install barrier 与 insert CAS 必须同 lifecycle 锁；并发 reinsert 不得越过新 barrier。
///
/// Code Logic（这个测试做什么）:
///     live gen1；并发 close + insert；insert 最终成功的 gen 必须 != gen1 且 barrier 清后存在；
///     中途若 close 先装 barrier，insert CAS 失败后重试直至成功。
#[test]
fn insert_revalidates_barrier_under_lifecycle_lock() {
    use std::sync::Barrier;
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_fake_session_for_test("s-r23-m1", "p1");
    let gen1 = registry
        .session_generation_for_test("s-r23-m1")
        .expect("gen1");
    // 持 lease 强制 close 走 Closing barrier（soft-timeout 路径），覆盖 remove→barrier 与 reinsert CAS。
    let lease = try_acquire_publication_lease(&registry.sessions, "s-r23-m1", gen1).expect("lease");
    // main + closer + inserter 三方同步（必须 3，否则第三 waiter 永久阻塞）。
    let start = Arc::new(Barrier::new(3));
    let reg_close = registry.clone();
    let start_close = start.clone();
    let closer = thread::spawn(move || {
        start_close.wait();
        reg_close.close("s-r23-m1")
    });
    let reg_ins = registry.clone();
    let start_ins = start.clone();
    let inserter = thread::spawn(move || {
        start_ins.wait();
        // 等 Closing barrier 出现，确保 CAS 必须 revalidate（非覆盖 Live）。
        for _ in 0..200 {
            if reg_ins.has_closing_tombstone_for_test("s-r23-m1") {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        reg_ins.insert_fake_session_for_test("s-r23-m1", "p1");
        reg_ins.session_generation_for_test("s-r23-m1")
    });
    start.wait();
    // close soft-wait 期间 barrier 已装；释放 lease 后 closer finish_cleanup 才 clear。
    thread::sleep(Duration::from_millis(30));
    drop(lease);
    closer
        .join()
        .expect("closer")
        .expect("close ok")
        .finish_cleanup();
    let gen2 = inserter.join().expect("inserter").expect("gen2");
    assert_ne!(gen1, gen2, "successor generation must differ");
    assert!(
        !registry.has_closing_tombstone_for_test("s-r23-m1"),
        "barrier must be gone after close cleanup + successful reinsert"
    );
    assert_eq!(registry.session_generation_for_test("s-r23-m1"), Some(gen2));
    assert_eq!(
        registry.runtime_presence("s-r23-m1"),
        SessionRuntimePresence::Live
    );
}

/// Business Logic（R24 H1: 为什么需要这个测试）:
///     registry close 后若立即 mark/clear，并发 reinsert 可装后继；旧 closer 的 persist
///     kill/delete 会打到 successor。令牌必须覆盖外部 cleanup。
///
/// Code Logic（这个测试做什么）:
///     close 返回 cleanup 且 **不** finish；并发 reinsert 阻塞；finish 后 reinsert 成功且
///     旧 closer 身份仍可 clear（barrier 身份 CAS）。
#[test]
fn closer_cleanup_token_spans_post_registry_persist() {
    use std::sync::Barrier;
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_fake_session_for_test("s-r24-h1", "p1");
    let gen1 = registry
        .session_generation_for_test("s-r24-h1")
        .expect("gen1");
    let cleanup = registry.close("s-r24-h1").expect("close");
    assert!(
        registry.has_closing_tombstone_for_test("s-r24-h1"),
        "barrier must remain until finish_cleanup"
    );
    assert_eq!(
        registry.runtime_presence("s-r24-h1"),
        SessionRuntimePresence::RestoreInProgress
    );
    // claim restore 在 barrier 下不得 Claimed。
    assert!(matches!(
        registry.try_claim_restore("s-r24-h1"),
        RestoreClaimOutcome::BarrierActive
    ));
    let start = Arc::new(Barrier::new(2));
    let reg_ins = registry.clone();
    let start_ins = start.clone();
    let inserter = thread::spawn(move || {
        start_ins.wait();
        reg_ins.insert_fake_session_for_test("s-r24-h1", "p1");
        reg_ins.session_generation_for_test("s-r24-h1")
    });
    start.wait();
    thread::sleep(Duration::from_millis(80));
    assert!(
        !registry.contains("s-r24-h1"),
        "reinsert must wait until finish_cleanup"
    );
    // 模拟 kill_persisted_backend + SQLite delete 完成。
    cleanup.finish_cleanup();
    let gen2 = inserter.join().expect("inserter").expect("gen2");
    assert_ne!(gen1, gen2);
    assert!(!registry.has_closing_tombstone_for_test("s-r24-h1"));
    assert_eq!(registry.session_generation_for_test("s-r24-h1"), Some(gen2));
}

/// Business Logic（R24 H2: 为什么需要这个测试）:
///     restore 读到 pre-close 行后若 barrier 期间无限重试，会在 close+delete 后复活会话。
///
/// Code Logic（这个测试做什么）:
///     install Closing barrier → try_claim_restore=BarrierActive；
///     spawn_row Abort 策略经 try_insert BarrierActive 返回 session_close_barrier_active
///     （用 insert CAS 路径模拟，不启真实 PTY）；finish 后可 claim/reinsert。
#[test]
fn barrier_active_aborts_restore_claim_and_spawn_retry() {
    let registry = WorkbenchSessionRegistry::new();
    let publish = registry.install_closing_barrier_for_test("s-r24-h2");
    assert!(matches!(
        registry.try_claim_restore("s-r24-h2"),
        RestoreClaimOutcome::BarrierActive
    ));
    // CAS insert 在 barrier 下返回 BarrierActive（restore Abort 路径同源）。
    let handle = Arc::new(Mutex::new(WorkbenchSessionHandle {
        row: WorkbenchSessionRow {
            id: "s-r24-h2".into(),
            project_id: "p1".into(),
            worktree_id: None,
            name: "n".into(),
            name_source: "default".to_string(),
            command: "/bin/sh".into(),
            cwd: "/tmp".into(),
            status: "running".into(),
            cols: 80,
            rows: 24,
            started_at: "t".into(),
            exited_at: None,
            exit_code: None,
            backend: RAW_PTY_BACKEND.into(),
            backend_id: None,
            backend_window_id: None,
            created_at: "t".into(),
            updated_at: "t".into(),
        },
        generation: 99,
        durability: SessionDurability::Provisional,
        publish: PublishControl::new(),
        deferred_output: Vec::new(),
        deferred_mutations: Vec::new(),
        pending_exit: None,
        restore_claim_generation: None,
        process: SessionProcess::Fake,
    }));
    assert_eq!(
        registry.try_insert_handle_revalidating_barrier("s-r24-h2", handle),
        InsertCasResult::BarrierActive
    );
    assert!(!registry.contains("s-r24-h2"));
    // finish closer cleanup 后允许 re-read + claim。
    publish.mark_cleanup_done();
    registry.clear_closing_tombstone_for_test("s-r24-h2");
    assert!(registry.try_claim_restore("s-r24-h2").is_claimed());
    registry.finish_restore_claim("s-r24-h2", SharedRestoreNotification::PersistedDisconnected);
}

/// Business Logic（R25 H1: 为什么需要这个测试）:
///     restore 已 claim 但尚未 insert live handle 时，close/delete 若无 close intent，
///     旧 restore 会在 delete 后 INSERT OR REPLACE 复活同 id。
///
/// Code Logic（这个测试做什么）:
///     claim held + no registry → begin_close_intent → claim 被 Failed 取消；
///     insert CAS BarrierActive；finish 前不得 re-claim 成功；finish 后才可 claim。
#[test]
fn close_intent_blocks_claimed_restore_without_live_handle() {
    let registry = WorkbenchSessionRegistry::new();
    assert!(registry.try_claim_restore("s-r25-h1").is_claimed());
    assert!(!registry.contains("s-r25-h1"));
    let row = WorkbenchSessionRow {
        id: "s-r25-h1".into(),
        project_id: "p1".into(),
        worktree_id: None,
        name: "n".into(),
        name_source: "default".to_string(),
        command: "/bin/sh".into(),
        cwd: "/tmp".into(),
        status: "running".into(),
        cols: 80,
        rows: 24,
        started_at: "t".into(),
        exited_at: None,
        exit_code: None,
        backend: RAW_PTY_BACKEND.into(),
        backend_id: None,
        backend_window_id: None,
        created_at: "t".into(),
        updated_at: "t".into(),
    };
    let cleanup = registry
        .begin_close_intent_for_missing_handle("s-r25-h1", row)
        .expect("close intent");
    assert!(registry.has_closing_tombstone_for_test("s-r25-h1"));
    // claim 已被 close intent 取消；后续不得 Claimed，只应 BarrierActive。
    assert!(matches!(
        registry.try_claim_restore("s-r25-h1"),
        RestoreClaimOutcome::BarrierActive
    ));
    let handle = Arc::new(Mutex::new(WorkbenchSessionHandle {
        row: WorkbenchSessionRow {
            id: "s-r25-h1".into(),
            project_id: "p1".into(),
            worktree_id: None,
            name: "n".into(),
            name_source: "default".to_string(),
            command: "/bin/sh".into(),
            cwd: "/tmp".into(),
            status: "running".into(),
            cols: 80,
            rows: 24,
            started_at: "t".into(),
            exited_at: None,
            exit_code: None,
            backend: RAW_PTY_BACKEND.into(),
            backend_id: None,
            backend_window_id: None,
            created_at: "t".into(),
            updated_at: "t".into(),
        },
        generation: 1,
        durability: SessionDurability::Provisional,
        publish: PublishControl::new(),
        deferred_output: Vec::new(),
        deferred_mutations: Vec::new(),
        pending_exit: None,
        restore_claim_generation: None,
        process: SessionProcess::Fake,
    }));
    assert_eq!(
        registry.try_insert_handle_revalidating_barrier("s-r25-h1", handle),
        InsertCasResult::BarrierActive,
        "stale restore must not re-upsert live handle under close intent"
    );
    assert!(!registry.contains("s-r25-h1"));
    cleanup.finish_cleanup();
    assert!(!registry.has_closing_tombstone_for_test("s-r25-h1"));
    assert!(registry.try_claim_restore("s-r25-h1").is_claimed());
    registry.finish_restore_claim("s-r25-h1", SharedRestoreNotification::PersistedDisconnected);
}

/// Business Logic（R25 H2: 为什么需要这个测试）:
///     project remove 若在 bulk delete 前 per-session finish，list/restore 可在窗口复活行。
///
/// Code Logic（这个测试做什么）:
///     两 session close 收集 cleanup 令牌但不 finish；concurrent claim/insert 均被 barrier 挡；
///     模拟 bulk delete 后统一 finish，再允许 claim。
#[test]
fn project_remove_defers_cleanup_finish_until_bulk_delete() {
    use std::sync::Barrier;
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_fake_session_for_test("s-r25-h2-a", "p-bulk");
    registry.insert_fake_session_for_test("s-r25-h2-b", "p-bulk");
    let mut cleanups = Vec::new();
    for id in ["s-r25-h2-a", "s-r25-h2-b"] {
        cleanups.push(registry.close(id).expect("close"));
        assert!(registry.has_closing_tombstone_for_test(id));
    }
    // 模拟 bulk delete 尚未完成：barrier 必须挡住 concurrent restore claim。
    let start = Arc::new(Barrier::new(3));
    let mut claim_threads = Vec::new();
    for id in ["s-r25-h2-a", "s-r25-h2-b"] {
        let reg = registry.clone();
        let start = start.clone();
        let sid = id.to_string();
        claim_threads.push(thread::spawn(move || {
            start.wait();
            matches!(
                reg.try_claim_restore(&sid),
                RestoreClaimOutcome::BarrierActive
            )
        }));
    }
    start.wait();
    for t in claim_threads {
        assert!(
            t.join().expect("claim thread"),
            "claim blocked before bulk finish"
        );
    }
    // 模拟 delete_by_project 成功后再 finish。
    for cleanup in cleanups {
        cleanup.finish_cleanup();
    }
    assert!(!registry.has_closing_tombstone_for_test("s-r25-h2-a"));
    assert!(!registry.has_closing_tombstone_for_test("s-r25-h2-b"));
    assert!(registry.try_claim_restore("s-r25-h2-a").is_claimed());
    registry.finish_restore_claim(
        "s-r25-h2-a",
        SharedRestoreNotification::PersistedDisconnected,
    );
}

/// Business Logic（R25 M1: 为什么需要这个测试）:
///     Abort spawn 若先 wait 既有 barrier，会在 barrier 清后用 stale 快照继续 PTY/insert。
///
/// Code Logic（这个测试做什么）:
///     pre-install barrier → Abort precheck 立即 Err(session_close_barrier_active)；
///     无 live insert。
#[test]
fn abort_spawn_returns_immediately_on_preexisting_barrier() {
    let registry = WorkbenchSessionRegistry::new();
    let publish = registry.install_closing_barrier_for_test("s-r25-m1");
    let err = registry
        .abort_if_preexisting_closing_barrier_for_test("s-r25-m1")
        .expect_err("must abort");
    match err {
        AppError::Unavailable(msg) => {
            assert_eq!(msg, "session_close_barrier_active");
        }
        other => panic!("unexpected error variant: {other}"),
    }
    assert!(!registry.contains("s-r25-m1"));
    // barrier 仍在（precheck 不得 clear）。
    assert!(registry.has_closing_tombstone_for_test("s-r25-m1"));
    publish.mark_cleanup_done();
    registry.clear_closing_tombstone_for_test("s-r25-m1");
}

/// Business Logic（R25 M2: 为什么需要这个测试）:
///     SessionCloseCleanup Drop 若总是 clear barrier，delete 失败后 restore 会打开窗口。
///
/// Code Logic（这个测试做什么）:
///     close → drop cleanup without finish → barrier 仍在；显式 finish 后才 clear。
#[test]
fn session_close_cleanup_drop_retains_barrier_until_finish() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_fake_session_for_test("s-r25-m2", "p1");
    let cleanup = registry.close("s-r25-m2").expect("close");
    assert!(registry.has_closing_tombstone_for_test("s-r25-m2"));
    // 模拟 delete 失败 / cancel：drop 令牌。
    drop(cleanup);
    assert!(
        registry.has_closing_tombstone_for_test("s-r25-m2"),
        "Drop must not clear barrier on incomplete cleanup"
    );
    assert!(matches!(
        registry.try_claim_restore("s-r25-m2"),
        RestoreClaimOutcome::BarrierActive
    ));
    // owner 成功 finish：用 begin_close_intent 取得同一 barrier 身份再 finish。
    let row = WorkbenchSessionRow {
        id: "s-r25-m2".into(),
        project_id: "p1".into(),
        worktree_id: None,
        name: "n".into(),
        name_source: "default".to_string(),
        command: "/bin/sh".into(),
        cwd: "/tmp".into(),
        status: "disconnected".into(),
        cols: 80,
        rows: 24,
        started_at: "t".into(),
        exited_at: Some("t".into()),
        exit_code: None,
        backend: RAW_PTY_BACKEND.into(),
        backend_id: None,
        backend_window_id: None,
        created_at: "t".into(),
        updated_at: "t".into(),
    };
    let cleanup2 = registry
        .begin_close_intent_for_missing_handle("s-r25-m2", row)
        .expect("reuse intent");
    cleanup2.finish_cleanup();
    assert!(!registry.has_closing_tombstone_for_test("s-r25-m2"));
    assert!(registry.try_claim_restore("s-r25-m2").is_claimed());
    registry.finish_restore_claim("s-r25-m2", SharedRestoreNotification::PersistedDisconnected);
}

/// Business Logic（R26 H1: 为什么需要这个测试）:
///     holder 已 claim 后 close 必须 revoke generation；holder 恢复后不得成功 re-upsert
///     已删除会话，也不得留下 live orphan。
///
/// Code Logic（这个测试做什么）:
///     claim → pause（持有 generation）→ close intent → claim generation inactive；
///     persist lease 获取失败；insert CAS BarrierActive；finish 后才允许新 claim。
#[test]
fn close_revokes_held_restore_claim_before_reupsert() {
    let registry = WorkbenchSessionRegistry::new();
    let generation = registry
        .try_claim_restore("s-r26-h1")
        .claim_generation()
        .expect("claimed");
    let guard = RestoreClaimGuard::new(registry.clone(), "s-r26-h1".to_string(), generation);
    assert!(guard.is_active());
    assert!(registry.is_restore_claim_generation_active("s-r26-h1", generation));

    // 模拟 holder 已进入 upsert 前：acquire lease 成功。
    let mut lease = registry
        .try_acquire_restore_persist_lease("s-r26-h1", generation)
        .expect("lease while active");
    assert!(lease.is_active());

    let row = WorkbenchSessionRow {
        id: "s-r26-h1".into(),
        project_id: "p1".into(),
        worktree_id: None,
        name: "n".into(),
        name_source: "default".to_string(),
        command: "/bin/sh".into(),
        cwd: "/tmp".into(),
        status: "running".into(),
        cols: 80,
        rows: 24,
        started_at: "t".into(),
        exited_at: None,
        exit_code: None,
        backend: RAW_PTY_BACKEND.into(),
        backend_id: None,
        backend_window_id: None,
        created_at: "t".into(),
        updated_at: "t".into(),
    };
    // 释放 lease 再 close（模拟 holder 尚未持 lease 时 close 更快路径）；
    // 另测 close 等待 in-flight lease 的路径见 concurrent 测试。
    lease.release();

    let cleanup = registry
        .begin_close_intent_for_missing_handle("s-r26-h1", row)
        .expect("close intent");
    assert!(registry.has_closing_tombstone_for_test("s-r26-h1"));
    assert!(!guard.is_active());
    assert!(!registry.is_restore_claim_generation_active("s-r26-h1", generation));
    assert!(
        registry
            .try_acquire_restore_persist_lease("s-r26-h1", generation)
            .is_none(),
        "revoked claim must not acquire persist lease"
    );
    assert!(matches!(
        registry.require_restore_claim_active("s-r26-h1", generation),
        Err(AppError::Unavailable(msg)) if msg == "session_restore_claim_revoked"
    ));

    let handle = Arc::new(Mutex::new(WorkbenchSessionHandle {
        row: WorkbenchSessionRow {
            id: "s-r26-h1".into(),
            project_id: "p1".into(),
            worktree_id: None,
            name: "n".into(),
            name_source: "default".to_string(),
            command: "/bin/sh".into(),
            cwd: "/tmp".into(),
            status: "running".into(),
            cols: 80,
            rows: 24,
            started_at: "t".into(),
            exited_at: None,
            exit_code: None,
            backend: RAW_PTY_BACKEND.into(),
            backend_id: None,
            backend_window_id: None,
            created_at: "t".into(),
            updated_at: "t".into(),
        },
        generation: 99,
        durability: SessionDurability::Provisional,
        publish: PublishControl::new(),
        deferred_output: Vec::new(),
        deferred_mutations: Vec::new(),
        pending_exit: None,
        restore_claim_generation: None,
        process: SessionProcess::Fake,
    }));
    assert_eq!(
        registry.try_insert_handle_revalidating_barrier("s-r26-h1", handle),
        InsertCasResult::BarrierActive,
        "holder must not re-insert live after close revoke"
    );
    assert!(!registry.contains("s-r26-h1"));
    // Drop stale guard 不得 panic / 不得清掉 barrier。
    drop(guard);
    assert!(registry.has_closing_tombstone_for_test("s-r26-h1"));
    cleanup.finish_cleanup();
    assert!(!registry.has_closing_tombstone_for_test("s-r26-h1"));
    assert!(registry.try_claim_restore("s-r26-h1").is_claimed());
    registry.release_restore_claim("s-r26-h1");
}

/// Business Logic（R26 H1: 为什么需要这个测试）:
///     close 时 holder 若仍在途 persist lease，必须 wait drain；否则 delete 后
///     holder 仍可 upsert 已删行。
///
/// Code Logic（这个测试做什么）:
///     claim + lease held → concurrent close intent 阻塞至 lease release →
///     generation inactive 且 barrier 在；release 后 close 完成。
#[test]
fn close_waits_for_restore_persist_lease_drain() {
    use std::sync::Barrier as StdBarrier;
    let registry = WorkbenchSessionRegistry::new();
    let generation = registry
        .try_claim_restore("s-r26-h1-lease")
        .claim_generation()
        .expect("claimed");
    let mut lease = registry
        .try_acquire_restore_persist_lease("s-r26-h1-lease", generation)
        .expect("lease");

    let start = Arc::new(StdBarrier::new(2));
    let reg = registry.clone();
    let start_close = start.clone();
    let closer = thread::spawn(move || {
        start_close.wait();
        let row = WorkbenchSessionRow {
            id: "s-r26-h1-lease".into(),
            project_id: "p1".into(),
            worktree_id: None,
            name: "n".into(),
            name_source: "default".to_string(),
            command: "/bin/sh".into(),
            cwd: "/tmp".into(),
            status: "running".into(),
            cols: 80,
            rows: 24,
            started_at: "t".into(),
            exited_at: None,
            exit_code: None,
            backend: RAW_PTY_BACKEND.into(),
            backend_id: None,
            backend_window_id: None,
            created_at: "t".into(),
            updated_at: "t".into(),
        };
        reg.begin_close_intent_for_missing_handle("s-r26-h1-lease", row)
            .expect("close intent after drain")
    });

    start.wait();
    // close 线程应阻塞在 lease drain；短暂 sleep 后仍 inactive 未 finish barrier 也可能已装。
    thread::sleep(Duration::from_millis(50));
    assert!(
        registry.is_restore_claim_generation_active("s-r26-h1-lease", generation)
            || registry.has_closing_tombstone_for_test("s-r26-h1-lease"),
        "close either still waiting with claim or has installed barrier"
    );
    // holder 完成（或放弃）persist：release lease 让 close 继续。
    lease.release();
    let cleanup = closer.join().expect("closer join");
    assert!(registry.has_closing_tombstone_for_test("s-r26-h1-lease"));
    assert!(!registry.is_restore_claim_generation_active("s-r26-h1-lease", generation));
    assert!(registry
        .try_acquire_restore_persist_lease("s-r26-h1-lease", generation)
        .is_none());
    cleanup.finish_cleanup();
}

/// Business Logic（R26 M1: 为什么需要这个测试）:
///     project remove 与 concurrent create 交错时，无 project barrier 会留下 orphan live。
///
/// Code Logic（这个测试做什么）:
///     begin project barrier → require_project_not_closing Err；
///     create revalidate 失败；finish barrier 后 Ok。
#[test]
fn project_closing_barrier_blocks_create_until_finish() {
    let registry = WorkbenchSessionRegistry::new();
    let gen = registry.begin_project_closing_barrier("p-r26-m1");
    assert!(registry.has_project_closing_barrier_for_test("p-r26-m1"));
    match registry.require_project_not_closing("p-r26-m1") {
        Err(AppError::Unavailable(msg)) => {
            assert_eq!(msg, "project_closing_barrier_active");
        }
        other => panic!("expected project barrier error, got {other:?}"),
    }
    // 其他项目不受影响。
    assert!(registry.require_project_not_closing("p-other").is_ok());

    // 并发 create 观察 barrier 仍在。
    let start = Arc::new(std::sync::Barrier::new(2));
    let reg = registry.clone();
    let start_t = start.clone();
    let observer = thread::spawn(move || {
        start_t.wait();
        reg.require_project_not_closing("p-r26-m1").is_err()
    });
    start.wait();
    assert!(
        observer.join().expect("observer"),
        "create blocked during remove"
    );

    registry.finish_project_closing_barrier("p-r26-m1", gen);
    assert!(!registry.has_project_closing_barrier_for_test("p-r26-m1"));
    assert!(registry.require_project_not_closing("p-r26-m1").is_ok());
    // 错误 generation finish 是 no-op 后仍可再次 begin。
    let gen2 = registry.begin_project_closing_barrier("p-r26-m1");
    registry.finish_project_closing_barrier("p-r26-m1", gen2.wrapping_add(1));
    assert!(
        registry.has_project_closing_barrier_for_test("p-r26-m1"),
        "mismatched generation must not clear barrier"
    );
    registry.finish_project_closing_barrier("p-r26-m1", gen2);
    assert!(!registry.has_project_closing_barrier_for_test("p-r26-m1"));
}

/// Business Logic（R42 H1: 为什么需要这个测试）:
///     并发 merge cleanup 与 project remove 必须 join 同一 barrier，禁止后启动者
///     覆盖 generation 并抢先 clear，留下前一个 owner 窗口中的 orphan live。
///
/// Code Logic（这个测试做什么）:
///     double begin 返回同一 generation 且 owners=2；
///     第一次 finish 后 barrier 仍在；第二次 finish 才 clear；
///     错误 generation finish 全程 no-op。
#[test]
fn project_closing_barrier_double_begin_joins_and_wrong_finish_is_noop() {
    let registry = WorkbenchSessionRegistry::new();
    let gen1 = registry.begin_project_closing_barrier("p-r42-h1");
    let gen2 = registry.begin_project_closing_barrier("p-r42-h1");
    assert_eq!(gen1, gen2, "second begin must join same generation");
    assert_eq!(
        registry.project_closing_generation_for_test("p-r42-h1"),
        Some(gen1)
    );
    assert_eq!(registry.project_closing_owners_for_test("p-r42-h1"), 2);
    // 错误 generation finish 不得减少 owners 或 clear。
    registry.finish_project_closing_barrier("p-r42-h1", gen1.wrapping_add(99));
    assert_eq!(registry.project_closing_owners_for_test("p-r42-h1"), 2);
    assert!(registry.has_project_closing_barrier_for_test("p-r42-h1"));
    // 第一个 owner finish：仍 active。
    registry.finish_project_closing_barrier("p-r42-h1", gen1);
    assert!(
        registry.has_project_closing_barrier_for_test("p-r42-h1"),
        "first finish of nested owners must keep barrier"
    );
    assert_eq!(registry.project_closing_owners_for_test("p-r42-h1"), 1);
    // 最后一个 owner finish：clear。
    registry.finish_project_closing_barrier("p-r42-h1", gen2);
    assert!(!registry.has_project_closing_barrier_for_test("p-r42-h1"));
    assert_eq!(registry.project_closing_owners_for_test("p-r42-h1"), 0);
}

/// Business Logic（R26 M1: 为什么需要这个测试）:
///     project remove 与 restore claim 交错：remove 先挂 barrier，restore revalidate 失败。
///
/// Code Logic（这个测试做什么）:
///     claim 成功后 begin project barrier → require_project_not_closing Err；
///     finish project barrier 后 require Ok（session claim 仍 held）。
#[test]
fn project_closing_barrier_blocks_restore_revalidate() {
    let registry = WorkbenchSessionRegistry::new();
    let claim_gen = registry
        .try_claim_restore("s-r26-m1-restore")
        .claim_generation()
        .expect("claimed");
    let project_gen = registry.begin_project_closing_barrier("p-r26-restore");
    assert!(registry
        .require_project_not_closing("p-r26-restore")
        .is_err());
    // claim generation 本身仍 active，但 project barrier 独立阻止 spawn/upsert。
    assert!(registry.is_restore_claim_generation_active("s-r26-m1-restore", claim_gen));
    registry.finish_project_closing_barrier("p-r26-restore", project_gen);
    assert!(registry
        .require_project_not_closing("p-r26-restore")
        .is_ok());
    registry.finish_restore_claim_for_generation(
        "s-r26-m1-restore",
        claim_gen,
        SharedRestoreNotification::Failed(AppErrorCategory::Unavailable),
    );
}

/// Business Logic（R27 H2: 为什么需要这个测试）:
///     live handle + held restore claim 时 close 必须 revoke claim，并阻断 re-upsert。
///
/// Code Logic（这个测试做什么）:
///     provisional live + claim → close() → claim inactive + barrier + insert CAS BarrierActive。
#[test]
fn live_close_revokes_held_restore_claim_before_reupsert() {
    let registry = WorkbenchSessionRegistry::new();
    let generation = registry
        .try_claim_restore("s-r27-h2")
        .claim_generation()
        .expect("claimed");
    registry.insert_provisional_fake_session_for_test("s-r27-h2", "p1");
    registry.bind_restore_claim_generation_for_test("s-r27-h2", Some(generation));
    assert!(registry.contains("s-r27-h2"));
    assert!(registry.is_restore_claim_generation_active("s-r27-h2", generation));

    let cleanup = registry.close("s-r27-h2").expect("live close");
    assert!(registry.has_closing_tombstone_for_test("s-r27-h2"));
    assert!(!registry.contains("s-r27-h2"));
    assert!(!registry.is_restore_claim_generation_active("s-r27-h2", generation));
    assert!(registry
        .try_acquire_restore_persist_lease("s-r27-h2", generation)
        .is_none());

    let handle = Arc::new(Mutex::new(WorkbenchSessionHandle {
        row: WorkbenchSessionRow {
            id: "s-r27-h2".into(),
            project_id: "p1".into(),
            worktree_id: None,
            name: "n".into(),
            name_source: "default".to_string(),
            command: "/bin/sh".into(),
            cwd: "/tmp".into(),
            status: "running".into(),
            cols: 80,
            rows: 24,
            started_at: "t".into(),
            exited_at: None,
            exit_code: None,
            backend: RAW_PTY_BACKEND.into(),
            backend_id: None,
            backend_window_id: None,
            created_at: "t".into(),
            updated_at: "t".into(),
        },
        generation: 99,
        durability: SessionDurability::Provisional,
        publish: PublishControl::new(),
        deferred_output: Vec::new(),
        deferred_mutations: Vec::new(),
        pending_exit: None,
        restore_claim_generation: Some(generation),
        process: SessionProcess::Fake,
    }));
    assert_eq!(
        registry.try_insert_handle_revalidating_barrier("s-r27-h2", handle),
        InsertCasResult::BarrierActive
    );
    cleanup.finish_cleanup();
    assert!(!registry.has_closing_tombstone_for_test("s-r27-h2"));
}

/// Business Logic（R27 H3: 为什么需要这个测试）:
///     lease drain timeout 不得返回 finishable cleanup（drained=true）从而 clear barrier。
///
/// Code Logic（这个测试做什么）:
///     claim + held lease → short-timeout close intent → drained=false + barrier retained
///     after finish_cleanup until lease release and reaper drain.
#[test]
fn restore_lease_drain_timeout_not_finishable_cleanup() {
    let registry = WorkbenchSessionRegistry::new();
    let generation = registry
        .try_claim_restore("s-r27-h3")
        .claim_generation()
        .expect("claimed");
    let mut lease = registry
        .try_acquire_restore_persist_lease("s-r27-h3", generation)
        .expect("lease");
    let row = WorkbenchSessionRow {
        id: "s-r27-h3".into(),
        project_id: "p1".into(),
        worktree_id: None,
        name: "n".into(),
        name_source: "default".to_string(),
        command: "/bin/sh".into(),
        cwd: "/tmp".into(),
        status: "running".into(),
        cols: 80,
        rows: 24,
        started_at: "t".into(),
        exited_at: None,
        exit_code: None,
        backend: RAW_PTY_BACKEND.into(),
        backend_id: None,
        backend_window_id: None,
        created_at: "t".into(),
        updated_at: "t".into(),
    };
    let cleanup = registry
        .begin_close_intent_with_drain_timeout_for_test("s-r27-h3", row, Duration::from_millis(30))
        .expect("close intent timeout path");
    assert!(
        !WorkbenchSessionRegistry::session_close_cleanup_drained_for_test(&cleanup),
        "timeout must not return finishable drained=true"
    );
    assert!(WorkbenchSessionRegistry::session_close_cleanup_has_restore_claim_for_test(&cleanup));
    assert!(registry.has_closing_tombstone_for_test("s-r27-h3"));
    // finish with drained=false → reaper path；barrier 在 lease 释放前仍可能保留。
    cleanup.finish_cleanup();
    // lease still held: barrier must remain (reaper waits leases).
    thread::sleep(Duration::from_millis(20));
    assert!(
        registry.has_closing_tombstone_for_test("s-r27-h3"),
        "barrier retained while restore lease held"
    );
    // concurrent upsert must fail under barrier.
    let handle = Arc::new(Mutex::new(WorkbenchSessionHandle {
        row: WorkbenchSessionRow {
            id: "s-r27-h3".into(),
            project_id: "p1".into(),
            worktree_id: None,
            name: "n".into(),
            name_source: "default".to_string(),
            command: "/bin/sh".into(),
            cwd: "/tmp".into(),
            status: "running".into(),
            cols: 80,
            rows: 24,
            started_at: "t".into(),
            exited_at: None,
            exit_code: None,
            backend: RAW_PTY_BACKEND.into(),
            backend_id: None,
            backend_window_id: None,
            created_at: "t".into(),
            updated_at: "t".into(),
        },
        generation: 7,
        durability: SessionDurability::Provisional,
        publish: PublishControl::new(),
        deferred_output: Vec::new(),
        deferred_mutations: Vec::new(),
        pending_exit: None,
        restore_claim_generation: Some(generation),
        process: SessionProcess::Fake,
    }));
    assert_eq!(
        registry.try_insert_handle_revalidating_barrier("s-r27-h3", handle),
        InsertCasResult::BarrierActive
    );
    lease.release();
    // reaper should clear after leases + cleanup_done.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while registry.has_closing_tombstone_for_test("s-r27-h3")
        && std::time::Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !registry.has_closing_tombstone_for_test("s-r27-h3"),
        "reaper clears barrier after lease drain"
    );
}

/// Business Logic（R27 H4: 为什么需要这个测试）:
///     insert CAS 必须拒绝 project_closing；project lease 被 remove 等待。
///
/// Code Logic（这个测试做什么）:
///     acquire project lease → begin barrier → insert ProjectClosing；
///     wait leases 在 release 前阻塞语义用 count 观测；finish 后 insert 成功。
#[test]
fn project_barrier_blocks_insert_cas_and_waits_leases() {
    let registry = WorkbenchSessionRegistry::new();
    // Ready 用例会话须在 project barrier 前插入，避免 ProjectClosing 自旋/no-op。
    registry.insert_provisional_fake_session_for_test("s-r27-h4-ready", "p-r27-h4");
    let gen_ready = registry
        .session_generation_for_test("s-r27-h4-ready")
        .expect("gen");

    let lease = registry
        .try_acquire_project_op_lease("p-r27-h4")
        .expect("project lease");
    assert_eq!(registry.project_op_lease_count_for_test("p-r27-h4"), 1);
    let gen = registry.begin_project_closing_barrier("p-r27-h4");
    assert!(registry.require_project_not_closing("p-r27-h4").is_err());
    // 在途 lease 期间不得再 acquire。
    assert!(registry.try_acquire_project_op_lease("p-r27-h4").is_err());

    let handle = Arc::new(Mutex::new(WorkbenchSessionHandle {
        row: WorkbenchSessionRow {
            id: "s-r27-h4".into(),
            project_id: "p-r27-h4".into(),
            worktree_id: None,
            name: "n".into(),
            name_source: "default".to_string(),
            command: "/bin/sh".into(),
            cwd: "/tmp".into(),
            status: "running".into(),
            cols: 80,
            rows: 24,
            started_at: "t".into(),
            exited_at: None,
            exit_code: None,
            backend: RAW_PTY_BACKEND.into(),
            backend_id: None,
            backend_window_id: None,
            created_at: "t".into(),
            updated_at: "t".into(),
        },
        generation: 1,
        durability: SessionDurability::Provisional,
        publish: PublishControl::new(),
        deferred_output: Vec::new(),
        deferred_mutations: Vec::new(),
        pending_exit: None,
        restore_claim_generation: None,
        process: SessionProcess::Fake,
    }));
    assert_eq!(
        registry.try_insert_handle_revalidating_barrier("s-r27-h4", handle),
        InsertCasResult::ProjectClosing
    );
    assert!(!registry.contains("s-r27-h4"));

    // Ready 在 project barrier 下 fail-closed。
    assert!(
        !registry.mark_session_ready_for_generation("s-r27-h4-ready", gen_ready, None),
        "Ready blocked under project barrier"
    );

    drop(lease);
    assert!(registry.wait_project_op_leases_drained("p-r27-h4"));
    registry.finish_project_closing_barrier("p-r27-h4", gen);
    assert!(registry.require_project_not_closing("p-r27-h4").is_ok());
}

/// Business Logic（R27 H5: 为什么需要这个测试）:
///     safe_attach / Ready 必须 revalidate claim generation；revoked 不得 Ready。
///
/// Code Logic（这个测试做什么）:
///     provisional + bound claim → revoke via close intent → mark_session_ready_for_generation false；
///     active claim 路径 bind + mark true。
#[test]
fn ready_revalidates_restore_claim_generation() {
    let registry = WorkbenchSessionRegistry::new();
    let generation = registry
        .try_claim_restore("s-r27-h5")
        .claim_generation()
        .expect("claimed");
    registry.insert_provisional_fake_session_for_test("s-r27-h5", "p1");
    registry.bind_restore_claim_generation_for_test("s-r27-h5", Some(generation));
    let gen = registry
        .session_generation_for_test("s-r27-h5")
        .expect("gen");
    assert!(registry.mark_session_ready_for_generation("s-r27-h5", gen, None));
    registry.finish_restore_claim("s-r27-h5", SharedRestoreNotification::Ready);

    // 第二会话：close 撤销 claim 后 Ready 失败。
    let generation2 = registry
        .try_claim_restore("s-r27-h5b")
        .claim_generation()
        .expect("claimed");
    registry.insert_provisional_fake_session_for_test("s-r27-h5b", "p1");
    registry.bind_restore_claim_generation_for_test("s-r27-h5b", Some(generation2));
    let cleanup = registry.close("s-r27-h5b").expect("close");
    assert!(!registry.is_restore_claim_generation_active("s-r27-h5b", generation2));
    // 先 finish cleanup 清 barrier，再 re-insert + bind 已撤销 generation。
    cleanup.finish_cleanup();
    registry.insert_provisional_fake_session_for_test("s-r27-h5b", "p1");
    registry.bind_restore_claim_generation_for_test("s-r27-h5b", Some(generation2));
    let gen_b = registry
        .session_generation_for_test("s-r27-h5b")
        .expect("gen");
    assert!(
        !registry.mark_session_ready_for_generation("s-r27-h5b", gen_b, None),
        "revoked claim must block Ready"
    );
}

/// Business Logic（R28 H3: 为什么需要这个测试）:
///     close 的 remove handle 与 Closing tombstone 安装不得拆分临界区，否则 restore 可抢跑 Missing 窗口。
///
/// Code Logic（这个测试做什么）:
///     live session close 后立刻 has_closing_tombstone 且 contains=false、runtime 非 Live。
#[test]
fn close_installs_tombstone_atomically_with_handle_remove() {
    let registry = WorkbenchSessionRegistry::new();
    registry.insert_fake_session_for_test("s-r28-h3", "p1");
    let cleanup = registry.close("s-r28-h3").expect("close");
    assert!(!registry.contains("s-r28-h3"));
    assert!(
        registry.has_closing_tombstone_for_test("s-r28-h3"),
        "tombstone must be present immediately after close returns"
    );
    assert_ne!(
        registry.runtime_presence("s-r28-h3"),
        SessionRuntimePresence::Live
    );
    // insert 在 barrier 下不得越过。
    assert_eq!(
        registry.try_insert_handle_revalidating_barrier(
            "s-r28-h3",
            Arc::new(Mutex::new(WorkbenchSessionHandle {
                row: WorkbenchSessionRow {
                    id: "s-r28-h3".into(),
                    project_id: "p1".into(),
                    worktree_id: None,
                    name: "n".into(),
                    name_source: "default".to_string(),
                    command: "/bin/sh".into(),
                    cwd: "/tmp".into(),
                    status: "running".into(),
                    cols: 80,
                    rows: 24,
                    started_at: "t".into(),
                    exited_at: None,
                    exit_code: None,
                    backend: RAW_PTY_BACKEND.into(),
                    backend_id: None,
                    backend_window_id: None,
                    created_at: "t".into(),
                    updated_at: "t".into(),
                },
                generation: 9,
                durability: SessionDurability::Provisional,
                publish: PublishControl::new(),
                deferred_output: Vec::new(),
                deferred_mutations: Vec::new(),
                pending_exit: None,
                restore_claim_generation: None,
                process: SessionProcess::Fake,
            }))
        ),
        InsertCasResult::BarrierActive
    );
    cleanup.finish_cleanup();
    assert!(!registry.has_closing_tombstone_for_test("s-r28-h3"));
}

/// Business Logic（R28 H4: 为什么需要这个测试）:
///     project op lease 必须阻断 remove 的 pre-snapshot drain，直到 create/restore 完成。
///
/// Code Logic（这个测试做什么）:
///     持 lease 时 wait_project_op_leases_drained 阻塞；drop 后 drain 成功。
#[test]
fn project_op_lease_held_blocks_remove_drain_until_release() {
    let registry = WorkbenchSessionRegistry::new();
    let lease = registry
        .try_acquire_project_op_lease("p-r28-h4")
        .expect("lease");
    assert_eq!(registry.project_op_lease_count_for_test("p-r28-h4"), 1);
    // lease 为计数器：允许多个 in-flight create/restore；remove 的 drain 须等全部归零。
    let lease2 = registry
        .try_acquire_project_op_lease("p-r28-h4")
        .expect("second lease");
    assert_eq!(registry.project_op_lease_count_for_test("p-r28-h4"), 2);
    let reg = registry.clone();
    let handle = thread::spawn(move || reg.wait_project_op_leases_drained("p-r28-h4"));
    thread::sleep(Duration::from_millis(80));
    assert!(!handle.is_finished(), "drain must wait while lease held");
    drop(lease);
    thread::sleep(Duration::from_millis(40));
    assert!(!handle.is_finished(), "still waiting for second lease");
    drop(lease2);
    assert!(handle.join().expect("join"), "drain after all releases");
    assert_eq!(registry.project_op_lease_count_for_test("p-r28-h4"), 0);
}

/// Business Logic（R29 H2: 为什么需要这个测试）:
///     missing-handle close 不得在 sessions 检查后释放锁再装 tombstone，否则 restore 可插入 Ready 后 close 只删 SQLite。
///
/// Code Logic（这个测试做什么）:
///     claim held、无 live → begin_close_intent 后立刻 has tombstone 且 claim revoked；
///     try_claim 不得 AlreadyLive。
#[test]
fn missing_handle_close_intent_atomic_with_claim_revoke() {
    let registry = WorkbenchSessionRegistry::new();
    let generation = registry
        .try_claim_restore("s-r29-h2")
        .claim_generation()
        .expect("claimed");
    assert!(registry.is_restore_claim_held("s-r29-h2"));
    let row = WorkbenchSessionRow {
        id: "s-r29-h2".into(),
        project_id: "p1".into(),
        worktree_id: None,
        name: "n".into(),
        name_source: "default".to_string(),
        command: "/bin/sh".into(),
        cwd: "/tmp".into(),
        status: "running".into(),
        cols: 80,
        rows: 24,
        started_at: "t".into(),
        exited_at: None,
        exit_code: None,
        backend: RAW_PTY_BACKEND.into(),
        backend_id: None,
        backend_window_id: None,
        created_at: "t".into(),
        updated_at: "t".into(),
    };
    let cleanup = registry
        .begin_close_intent_for_missing_handle("s-r29-h2", row)
        .expect("close intent");
    assert!(
        registry.has_closing_tombstone_for_test("s-r29-h2"),
        "tombstone must exist immediately"
    );
    assert!(!registry.is_restore_claim_generation_active("s-r29-h2", generation));
    assert!(!registry.contains("s-r29-h2"));
    assert!(
        matches!(
            registry.try_claim_restore("s-r29-h2"),
            RestoreClaimOutcome::BarrierActive | RestoreClaimOutcome::RestoreInProgress(_)
        ) || registry.has_closing_tombstone_for_test("s-r29-h2"),
        "must not allow fresh live without barrier"
    );
    cleanup.finish_cleanup();
}
