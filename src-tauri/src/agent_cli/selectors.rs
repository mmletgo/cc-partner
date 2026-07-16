//! agent_cli/selectors.rs — 精确 selector 与有界 stdin 输入。
//!
//! Business Logic（为什么需要这个模块）:
//!     v1 只接受 id/path/branch 精确匹配；正文只经 stdin，避免进入 argv/日志。
//!
//! Code Logic（这个模块做什么）:
//!     解析前缀 selector；对候选列表做精确 resolve；读有界 stdin JSON。

use crate::agent_cli::output::CliError;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};

/// stdin 最大 1 MiB。
pub const MAX_STDIN_BYTES: usize = 1024 * 1024;
/// terminal send body 最大 256 KiB。
pub const MAX_TERMINAL_BODY_BYTES: usize = 256 * 1024;

/// 项目选择器。
///
/// Business Logic（为什么需要这个枚举）:
///     禁止 name/active；只允许 id 或规范化 path。
///
/// Code Logic（这个枚举做什么）:
///     Id / Path 两种精确形式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectSelector {
    Id(String),
    Path(String),
}

/// worktree 选择器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeSelector {
    Id(String),
    Branch(String),
}

/// 通用实体 `id:<id>` 选择器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntitySelector {
    pub id: String,
}

/// 解析 project selector 字符串。
///
/// Business Logic（为什么需要这个函数）:
///     clap/dispatch 需要统一前缀规则。
///
/// Code Logic（这个函数做什么）:
///     `id:` / `path:`；拒绝 name/active/current/recent。
pub fn parse_project_selector(raw: &str) -> Result<ProjectSelector, CliError> {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("id:") {
        let id = rest.trim();
        if id.is_empty() {
            return Err(CliError::usage(
                "invalid_selector",
                "project id after id: must be non-empty",
            ));
        }
        return Ok(ProjectSelector::Id(id.to_string()));
    }
    if let Some(rest) = trimmed.strip_prefix("path:") {
        let path = rest.trim();
        if path.is_empty() {
            return Err(CliError::usage(
                "invalid_selector",
                "project path after path: must be non-empty",
            ));
        }
        return Ok(ProjectSelector::Path(path.to_string()));
    }
    Err(CliError::usage(
        "invalid_selector",
        "project must be id:<id> or path:<canonicalPath>",
    ))
}

/// 解析 worktree selector。
///
/// Business Logic（为什么需要这个函数）:
///     worktree 仅 id 或精确 branch 名。
///
/// Code Logic（这个函数做什么）:
///     `id:` / `branch:`。
pub fn parse_worktree_selector(raw: &str) -> Result<WorktreeSelector, CliError> {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("id:") {
        let id = rest.trim();
        if id.is_empty() {
            return Err(CliError::usage(
                "invalid_selector",
                "worktree id after id: must be non-empty",
            ));
        }
        return Ok(WorktreeSelector::Id(id.to_string()));
    }
    if let Some(rest) = trimmed.strip_prefix("branch:") {
        let branch = rest.trim();
        if branch.is_empty() {
            return Err(CliError::usage(
                "invalid_selector",
                "worktree branch after branch: must be non-empty",
            ));
        }
        return Ok(WorktreeSelector::Branch(branch.to_string()));
    }
    Err(CliError::usage(
        "invalid_selector",
        "worktree must be id:<id> or branch:<exactBranch>",
    ))
}

/// 解析 entity `id:` selector。
///
/// Business Logic（为什么需要这个函数）:
///     session/agent/task/experiment/run 统一 id: 前缀。
///
/// Code Logic（这个函数做什么）:
///     要求 `id:` 前缀且非空。
pub fn parse_entity_selector(raw: &str) -> Result<EntitySelector, CliError> {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("id:") {
        let id = rest.trim();
        if id.is_empty() {
            return Err(CliError::usage(
                "invalid_selector",
                "entity id after id: must be non-empty",
            ));
        }
        return Ok(EntitySelector { id: id.to_string() });
    }
    Err(CliError::usage(
        "invalid_selector",
        "entity must be id:<entityId>",
    ))
}

/// 项目候选行（供 resolve）。
#[derive(Debug, Clone)]
pub struct ProjectCandidate {
    pub id: String,
    pub path: String,
}

/// worktree 候选行。
#[derive(Debug, Clone)]
pub struct WorktreeCandidate {
    pub id: String,
    pub branch: String,
}

/// 规范化 path 字符串用于精确比较。
///
/// Business Logic（为什么需要这个函数）:
///     path selector 必须在 canonicalize 后比较，避免 `./` 与绝对路径漂移。
///
/// Code Logic（这个函数做什么）:
///     优先 `std::fs::canonicalize`；失败则做字面规范化（去多余 `/`）。
pub fn normalize_path_for_match(path: &str) -> String {
    let p = Path::new(path);
    if let Ok(canon) = p.canonicalize() {
        return path_to_cmp_string(&canon);
    }
    // 不存在的路径：用组件规范化
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                let _ = out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    path_to_cmp_string(&out)
}

/// Business Logic（为什么需要这个函数）:
///     跨平台路径比较需要稳定字符串。
///
/// Code Logic（这个函数做什么）:
///     to_string_lossy；Windows 下统一小写盘符风格不在此强制。
fn path_to_cmp_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

/// 在候选列表中精确解析 project。
///
/// Business Logic（为什么需要这个函数）:
///     0 命中 not_found；多命中 conflict；禁止模糊。
///
/// Code Logic（这个函数做什么）:
///     Id 精确；Path 经 normalize 后精确匹配。
pub fn resolve_exact_project<'a>(
    selector: &ProjectSelector,
    rows: &'a [ProjectCandidate],
) -> Result<&'a ProjectCandidate, CliError> {
    match selector {
        ProjectSelector::Id(id) => {
            let hits: Vec<_> = rows.iter().filter(|r| r.id == *id).collect();
            match hits.as_slice() {
                [one] => Ok(*one),
                [] => Err(CliError::not_found("project not found")),
                _ => Err(CliError::conflict("multiple projects share the same id")),
            }
        }
        ProjectSelector::Path(path) => {
            let want = normalize_path_for_match(path);
            let hits: Vec<_> = rows
                .iter()
                .filter(|r| normalize_path_for_match(&r.path) == want)
                .collect();
            match hits.as_slice() {
                [one] => Ok(*one),
                [] => Err(CliError::not_found("project path not found")),
                _ => Err(CliError::conflict(
                    "multiple projects match the same canonical path",
                )),
            }
        }
    }
}

/// 在候选列表中精确解析 worktree。
///
/// Business Logic（为什么需要这个函数）:
///     branch 多 worktree 必须 conflict，不得静默选第一个。
///
/// Code Logic（这个函数做什么）:
///     Id/Branch 精确匹配。
pub fn resolve_exact_worktree<'a>(
    selector: &WorktreeSelector,
    rows: &'a [WorktreeCandidate],
) -> Result<&'a WorktreeCandidate, CliError> {
    match selector {
        WorktreeSelector::Id(id) => {
            let hits: Vec<_> = rows.iter().filter(|r| r.id == *id).collect();
            match hits.as_slice() {
                [one] => Ok(*one),
                [] => Err(CliError::not_found("worktree not found")),
                _ => Err(CliError::conflict("multiple worktrees share the same id")),
            }
        }
        WorktreeSelector::Branch(branch) => {
            let hits: Vec<_> = rows.iter().filter(|r| r.branch == *branch).collect();
            match hits.as_slice() {
                [one] => Ok(*one),
                [] => Err(CliError::not_found("worktree branch not found")),
                _ => Err(CliError::conflict("ambiguous_selector")),
            }
        }
    }
}

/// 校验 body 参数必须是字面 `-`。
///
/// Business Logic（为什么需要这个函数）:
///     禁止在 argv 中传 Prompt/terminal 正文。
///
/// Code Logic（这个函数做什么）:
///     仅允许 `-`。
pub fn require_stdin_dash(input_json: &str) -> Result<(), CliError> {
    if input_json.trim() == "-" {
        Ok(())
    } else {
        Err(CliError::usage(
            "invalid_input",
            "body-bearing flags must use --input-json - (stdin only)",
        ))
    }
}

/// 从 reader 读取有界字节。
///
/// Business Logic（为什么需要这个函数）:
///     超限必须在传输前失败，且错误不得包含正文。
///
/// Code Logic（这个函数做什么）:
///     读到 max+1；若超过 max 返回 usage；空缓冲返回 empty。
pub fn read_bounded(reader: &mut impl Read, max_bytes: usize) -> Result<Vec<u8>, CliError> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = reader
            .read(&mut chunk)
            .map_err(|_| CliError::usage("invalid_input", "failed to read stdin"))?;
        if n == 0 {
            break;
        }
        if buf.len() + n > max_bytes {
            return Err(CliError::usage(
                "input_too_large",
                "stdin exceeds allowed size limit",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    if buf.is_empty() {
        return Err(CliError::usage("empty_stdin", "stdin body is empty"));
    }
    Ok(buf)
}

/// 读取并反序列化 stdin JSON（≤1MiB）。
///
/// Business Logic（为什么需要这个函数）:
///     create/send/verify 的输入必须来自 stdin JSON。
///
/// Code Logic（这个函数做什么）:
///     read_bounded → serde_json；错误 message 不含 body。
pub fn read_input_json<T: DeserializeOwned>(reader: &mut impl Read) -> Result<T, CliError> {
    let bytes = read_bounded(reader, MAX_STDIN_BYTES)?;
    serde_json::from_slice(&bytes)
        .map_err(|_| CliError::usage("invalid_json", "stdin is not valid JSON for the command"))
}

/// 读取 terminal send 正文（最大 256KiB）。
///
/// Business Logic（为什么需要这个函数）:
///     terminal send 比通用 stdin 更严；仍不得进错误 envelope。
///
/// Code Logic（这个函数做什么）:
///     解析 JSON 对象中的 `data` 字符串字段，或原始字符串 JSON。
pub fn read_terminal_send_body(reader: &mut impl Read) -> Result<String, CliError> {
    let bytes = read_bounded(reader, MAX_TERMINAL_BODY_BYTES)?;
    // 尝试 JSON string 或 { "data": "..." }
    if let Ok(s) = serde_json::from_slice::<String>(&bytes) {
        return Ok(s);
    }
    #[derive(serde::Deserialize)]
    struct Body {
        data: String,
    }
    let body: Body = serde_json::from_slice(&bytes).map_err(|_| {
        CliError::usage(
            "invalid_json",
            "terminal send expects JSON string or {\"data\":string}",
        )
    })?;
    if body.data.len() > MAX_TERMINAL_BODY_BYTES {
        return Err(CliError::usage(
            "input_too_large",
            "terminal body exceeds 256KiB",
        ));
    }
    Ok(body.data)
}

/// 渲染错误时不得包含 secret 的断言辅助（测试用文档性包装）。
///
/// Business Logic（为什么需要这个函数）:
///     隐私测试复用统一入口。
///
/// Code Logic（这个函数做什么）:
///     检查 error message/code 不含 secret。
pub fn error_excludes_secret(error: &CliError, secret: &str) -> bool {
    !error.message.contains(secret) && !error.code.contains(secret)
}

/// 将 selector 序列化为稳定字符串（调试/测试，不含 body）。
impl Serialize for ProjectSelector {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Id(id) => serializer.serialize_str(&format!("id:{id}")),
            Self::Path(path) => serializer.serialize_str(&format!("path:{path}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_cli::output::render_failure;
    use std::io::Cursor;

    fn worktree(id: &str, branch: &str) -> WorktreeCandidate {
        WorktreeCandidate {
            id: id.into(),
            branch: branch.into(),
        }
    }

    #[test]
    fn exact_branch_with_multiple_worktrees_is_conflict() {
        let rows = vec![worktree("w1", "feature/x"), worktree("w2", "feature/x")];
        let error = resolve_exact_worktree(&WorktreeSelector::Branch("feature/x".into()), &rows)
            .unwrap_err();
        assert_eq!(error.code(), "ambiguous_selector");
    }

    #[test]
    fn exact_id_resolves_single_project() {
        let rows = vec![
            ProjectCandidate {
                id: "p1".into(),
                path: "/tmp/a".into(),
            },
            ProjectCandidate {
                id: "p2".into(),
                path: "/tmp/b".into(),
            },
        ];
        let hit = resolve_exact_project(&ProjectSelector::Id("p2".into()), &rows).unwrap();
        assert_eq!(hit.id, "p2");
    }

    #[test]
    fn wrong_prefix_is_usage() {
        assert_eq!(
            parse_project_selector("name:foo").unwrap_err().code(),
            "invalid_selector"
        );
        assert_eq!(
            parse_entity_selector("current").unwrap_err().code(),
            "invalid_selector"
        );
    }

    #[test]
    fn empty_stdin_is_usage() {
        let mut c = Cursor::new(b"");
        let err = read_input_json::<serde_json::Value>(&mut c).unwrap_err();
        assert_eq!(err.code(), "empty_stdin");
    }

    #[test]
    fn stdin_over_limit_rejected_without_body() {
        let secret = "SUPER_SECRET_PROMPT_BODY_xyz";
        let mut big = vec![b'a'; MAX_STDIN_BYTES + 10];
        big.extend_from_slice(secret.as_bytes());
        let mut c = Cursor::new(big);
        let err = read_bounded(&mut c, MAX_STDIN_BYTES).unwrap_err();
        assert_eq!(err.code(), "input_too_large");
        assert!(error_excludes_secret(&err, secret));
        let rendered = render_failure(err, true);
        assert!(!rendered.stdout.contains(secret));
    }

    #[test]
    fn terminal_body_over_256kib_rejected() {
        let big = vec![b'x'; MAX_TERMINAL_BODY_BYTES + 1];
        let mut c = Cursor::new(big);
        let err = read_bounded(&mut c, MAX_TERMINAL_BODY_BYTES).unwrap_err();
        assert_eq!(err.code(), "input_too_large");
    }

    #[test]
    fn malformed_json_rejected_without_echo() {
        let secret = "SUPER_SECRET_PROMPT_BODY_xyz";
        let mut c = Cursor::new(format!("{{\"goal\":\"{secret}\"").into_bytes());
        let err = read_input_json::<serde_json::Value>(&mut c).unwrap_err();
        assert_eq!(err.code(), "invalid_json");
        assert!(error_excludes_secret(&err, secret));
    }

    #[test]
    fn require_stdin_dash_rejects_inline_body() {
        let err = require_stdin_dash("{\"data\":\"pwd\"}").unwrap_err();
        assert_eq!(err.code(), "invalid_input");
    }

    #[test]
    fn missing_worktree_is_not_found() {
        let rows = vec![worktree("w1", "main")];
        let err =
            resolve_exact_worktree(&WorktreeSelector::Id("missing".into()), &rows).unwrap_err();
        assert_eq!(err.exit, crate::agent_cli::output::CliExitCode::NotFound);
    }
}
