//! workbench/terminal_authority.rs — 终端 stream authority 合成
//!
//! Business Logic（为什么需要这个模块）:
//!     remote 终端的 reader seq 属于远端进程世代，本机 event_bus owner 属于本地 sidecar 世代。
//!     若 remote stream 只 stamp 本机 bus owner，远端 backend 重启后 sessionId 不变、seq 从 0 重起，
//!     前端会因 lastSeq 仍高而永久丢弃新低 seq（无 Gap，因为本机 bus seq 仍单调），终端静默冻结。
//!
//! Code Logic（这个模块做什么）:
//!     为 remote session 合成 `localBusOwner + remoteStreamOwner` 复合 authority；
//!     local session 仅返回 local bus owner。格式与判定集中在此，供 live/replay/resync 共用。

/// remote 与 local owner 之间的分隔符（ASCII unit separator，不会出现在 UUID/普通 owner 文本中）。
pub const TERMINAL_AUTHORITY_SEPARATOR: char = '\u{001f}';

/// 合成 remote 终端的复合 stream authority。
///
/// Business Logic（为什么需要这个函数）:
///     live enrichment、replay cutover 与 Gap resync 必须使用同一复合 authority，
///     这样远端 backend 重启（remote owner 变化）会触发前端 authority cutover 重置 lastSeq。
///
/// Code Logic（这个函数做什么）:
///     返回 `"{local_bus_owner}\u{001f}{remote_stream_owner}"`。
pub fn compose_remote_terminal_authority(local_bus_owner: &str, remote_stream_owner: &str) -> String {
    format!("{local_bus_owner}{TERMINAL_AUTHORITY_SEPARATOR}{remote_stream_owner}")
}

/// 判断 sessionId 是否是 remote workbench session。
///
/// Business Logic（为什么需要这个函数）:
///     只有 remote 会话需要把远端 stream 世代并入 authority；local 会话保持简单 local owner。
///
/// Code Logic（这个函数做什么）:
///     trim 后若以 `remote:` 前缀开头则返回 true。
pub fn is_remote_workbench_session_id(session_id: &str) -> bool {
    session_id.trim_start().starts_with("remote:")
}

/// 计算终端 stream 应 stamp 给前端的 authority。
///
/// Business Logic（为什么需要这个函数）:
///     前端按 authority 分代比较 lastSeq。local 会话用本机 bus owner；
///     remote 会话用 local+remote 复合，使远端重启可 cutover；
///     旧 peer 未携带 remote owner 时降级为 local-only（mixed-version 兼容）。
///
/// Code Logic（这个函数做什么）:
///     - local session → `local_bus_owner`
///     - remote session + 非空 remote_stream_owner → `compose_remote_terminal_authority`
///     - remote session + 缺失/空 remote owner（legacy peer）→ `local_bus_owner` only
pub fn terminal_stream_authority(
    session_id: &str,
    local_bus_owner: &str,
    remote_stream_owner: Option<&str>,
) -> String {
    if !is_remote_workbench_session_id(session_id) {
        return local_bus_owner.to_string();
    }
    match remote_stream_owner.map(str::trim).filter(|s| !s.is_empty()) {
        Some(remote) => compose_remote_terminal_authority(local_bus_owner, remote),
        None => local_bus_owner.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     复合 authority 格式必须稳定，live/replay/resync 与前端测试共享同一分隔约定。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 compose 使用 unit separator 拼接两端 owner。
    #[test]
    fn compose_uses_unit_separator() {
        let composed = compose_remote_terminal_authority("owner-local", "owner-remote");
        assert_eq!(
            composed,
            format!("owner-local{TERMINAL_AUTHORITY_SEPARATOR}owner-remote")
        );
        assert!(composed.contains('\u{001f}'));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     local session 不得被合成进 remote 世代，否则本机 PTY 会错误触发 authority cutover。
    ///
    /// Code Logic（这个测试做什么）:
    ///     local sessionId 即使带 remote owner 仍只返回 local bus owner。
    #[test]
    fn local_session_keeps_simple_local_owner() {
        assert_eq!(
            terminal_stream_authority("s1", "owner-1", Some("owner-remote")),
            "owner-1"
        );
        assert_eq!(
            terminal_stream_authority("s1", "owner-1", None),
            "owner-1"
        );
        assert!(!is_remote_workbench_session_id("s1"));
        assert!(!is_remote_workbench_session_id("  s1"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     remote session 必须把远端 stream owner 并入 authority，才能在远端重启后重置 lastSeq。
    ///
    /// Code Logic（这个测试做什么）:
    ///     remote session + 非空 remote owner → 复合；空/缺失 → 降级 local-only。
    #[test]
    fn remote_session_composes_when_remote_owner_present() {
        assert!(is_remote_workbench_session_id("remote:device-a:s1"));
        assert!(is_remote_workbench_session_id("  remote:device-a:s1"));
        assert_eq!(
            terminal_stream_authority("remote:device-a:s1", "owner-1", Some("owner-remote")),
            compose_remote_terminal_authority("owner-1", "owner-remote")
        );
        // legacy peer / missing owner：mixed-version 降级为 local bus only
        assert_eq!(
            terminal_stream_authority("remote:device-a:s1", "owner-1", None),
            "owner-1"
        );
        assert_eq!(
            terminal_stream_authority("remote:device-a:s1", "owner-1", Some("")),
            "owner-1"
        );
        assert_eq!(
            terminal_stream_authority("remote:device-a:s1", "owner-1", Some("   ")),
            "owner-1"
        );
    }
}
