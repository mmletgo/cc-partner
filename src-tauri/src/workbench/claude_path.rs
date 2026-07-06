//! claude_path — Claude Code projects 目录路径编码共享 helper。
//!
//! Business Logic（为什么需要这个模块）:
//!     Claude Code 把每次会话 transcript 写到 `~/.claude/projects/<encoded-cwd>/<session>.jsonl`，
//!     其中 `<encoded-cwd>` 由工作目录路径编码而来。Orchestrator 的 runtime association 和 Workbench
//!     的 Claude session 搜索都需要按这套规则定位 transcript 目录，因此编码逻辑必须单点共享，
//!     避免两处实现漂移；放在 workbench 模块下是因为 orchestrator 允许依赖 workbench，反之不允许。
//!
//! Code Logic（这个模块做什么）:
//!     提供 `encode_claude_project_path`，把路径分隔符与非安全字符转成 `-`，保留常见 ASCII 文件名字符。

/// Business Logic（为什么需要这个函数）:
///     多个功能（Orchestrator runtime association、Workbench Claude session 搜索与 resume）都要按
///     Claude Code 的磁盘布局定位 `~/.claude/projects/<encoded-cwd>/` 下的 transcript 文件，
///     必须复用同一套编码规则，否则会扫描到错误目录。
///
/// Code Logic（这个函数做什么）:
///     把路径分隔符（`/` 和 `\`）与非安全字符转成 `-`，保留 ASCII 字母数字以及 `-`、`_`、`.`；
///     空路径统一回退为 `-`，与 Claude Code 自身的目录命名保持一致。
pub fn encode_claude_project_path(path: &str) -> String {
    let encoded: String = path
        .chars()
        .map(|ch| {
            if ch == '/' || ch == '\\' {
                '-'
            } else if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    if encoded.is_empty() {
        "-".to_string()
    } else {
        encoded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     Claude session 搜索和 Orchestrator runtime association 都依赖 `/Users/hans/foo` 这类典型
    ///     Unix 项目路径稳定编码成 `-Users-hans-foo`，编码漂移会导致扫描到错误的 transcript 目录。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言常见 Unix 绝对路径编码后以 `-` 连接各目录段，与 Claude Code 磁盘布局一致。
    #[test]
    fn encodes_unix_path_with_dashes_between_segments() {
        assert_eq!(encode_claude_project_path("/Users/hans/foo"), "-Users-hans-foo");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Windows 项目路径用反斜杠分隔且盘符带冒号，远端项目 resume 也可能遇到 Windows cwd，
    ///     编码必须把反斜杠和冒号都转成 `-`，保证生成的目录名跨平台一致。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 Windows 路径的 `C:` 与首个 `\` 分别编码为 `-`/`-`，得到 `C--Users-...` 形态。
    #[test]
    fn encodes_windows_backslash_path_like_forward_slash() {
        assert_eq!(
            encode_claude_project_path(r"C:\Users\hans\foo"),
            "C--Users-hans-foo"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     项目路径可能含空格、中文或其它特殊字符，Claude Code 目录名不允许这些字符，
    ///     编码必须把它们折叠成 `-`，保证生成的目录名跨平台安全。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言空格、中文、`@` 等非安全字符全部转成 `-`，同时保留 `-`、`_`、`.`。
    #[test]
    fn folds_unsafe_characters_into_dashes() {
        assert_eq!(
            encode_claude_project_path("/Users/hans/my project@v2"),
            "-Users-hans-my-project-v2"
        );
        assert_eq!(
            encode_claude_project_path("/Users/hans/中文目录"),
            "-Users-hans-----"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     空 cwd 不应产生空目录名，Claude Code 也不会为空路径创建无名目录；
    ///     回退为 `-` 保证后续路径拼接仍合法。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言空字符串路径编码为 `-`；同时验证纯空白字符会被逐个转成 `-`
    ///     （与 Claude Code 磁盘布局一致，不做 trim）。
    #[test]
    fn empty_path_falls_back_to_single_dash() {
        assert_eq!(encode_claude_project_path(""), "-");
        assert_eq!(encode_claude_project_path("   "), "---");
    }
}
