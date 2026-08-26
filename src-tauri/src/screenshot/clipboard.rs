//! screenshot/clipboard.rs — Linux 图形剪贴板可达性与 Agent 贴图回退
//!
//! Business Logic（为什么需要这个模块）:
//!     远端 Linux（SSH / headless / 过期 X11 forwarding）没有可用的 X11/Wayland 剪贴板时，
//!     `arboard::Clipboard::new()` 会报 X11 server 错误，Agent TUI 贴图整条链路失败。
//!     必须先判断显示套接字是否真的可达，不可达时改走临时 PNG + 身份表路径语法，
//!     不能把 Ctrl+V 发给读不到图的 CLI，也不能一律打 `@路径`。
//!
//! Code Logic（这个模块做什么）:
//!     纯函数解析 DISPLAY/Wayland 套接字、分类剪贴板错误、决定 Ctrl+V 还是文件 mention；
//!     生产路径在无显示时跳过 arboard（避免 X11 连接超时）。

use std::path::{Path, PathBuf};
#[cfg(all(unix, not(target_os = "macos")))]
use std::time::Duration;

use arboard::{Clipboard, ImageData};

use crate::agent_catalog::HeadlessImagePasteKind;
use crate::error::AppError;
use crate::screenshot::capture::{decode_image_data_url_to_rgba, MAX_CLIPBOARD_IMAGE_BYTES};

/// Linux X11 剪贴板在 set_image 后必须继续服务 SelectionRequest。
#[cfg(all(unix, not(target_os = "macos")))]
const LINUX_CLIPBOARD_HOLD: Duration = Duration::from_millis(1500);

/// 打开或写入系统剪贴板失败的分类。
///
/// Business Logic（为什么需要这个枚举）:
///     显示服务器缺失是可回退的环境限制；解码失败等其它错误必须原样上抛。
///
/// Code Logic（这个枚举做什么）:
///     `DisplayUnavailable` 触发文件 mention；`Other` 保留原始说明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardWriteFailure {
    /// 没有可达的 X11/Wayland 剪贴板。
    DisplayUnavailable,
    /// 其它剪贴板错误（解码后的写入/转换失败等）。
    Other(String),
}

/// Agent TUI 贴图在 owning device 上的执行计划。
///
/// Business Logic（为什么需要这个枚举）:
///     有图形剪贴板时保持 Ctrl+V 合同；无显示时不能假装写成功。
///
/// Code Logic（这个枚举做什么）:
///     `OsClipboardCtrlV` 写 OS 剪贴板后发 `\x16`；`HeadlessFile` 按身份表语法注入路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentImagePastePlan {
    /// 已写入 OS 剪贴板，随后注入 Ctrl+V。
    OsClipboardCtrlV,
    /// 无图形剪贴板：PNG 已落盘，按 `kind` 向 PTY 注入路径。
    HeadlessFile {
        path: PathBuf,
        kind: HeadlessImagePasteKind,
    },
}

/// 从 arboard / x11rb 错误文案判断是否属于显示服务器不可用。
///
/// Business Logic（为什么需要这个函数）:
///     SSH 远端常见报错是 `X11 server` / connection timed out；必须与 PNG 解码失败区分。
///
/// Code Logic（这个函数做什么）:
///     大小写不敏感匹配 x11/wayland/display/clipboard not supported/no screen。
pub fn classify_clipboard_error(message: &str) -> ClipboardWriteFailure {
    let lower = message.to_ascii_lowercase();
    if lower.contains("x11")
        || lower.contains("wayland")
        || lower.contains("display")
        || lower.contains("clipboard not supported")
        || lower.contains("no screen")
    {
        ClipboardWriteFailure::DisplayUnavailable
    } else {
        ClipboardWriteFailure::Other(message.to_string())
    }
}

/// 根据剪贴板写入结果决定 Agent 贴图策略。
///
/// Business Logic（为什么需要这个函数）:
///     显示服务器缺失时仍要把图交给 Agent；其它剪贴板错误不得改走文件回退掩盖真故障。
///
/// Code Logic（这个函数做什么）:
///     Ok → Ctrl+V；DisplayUnavailable → HeadlessFile；Other → Bad。
pub fn agent_image_paste_plan(
    clipboard: Result<(), ClipboardWriteFailure>,
    file_path: PathBuf,
    kind: HeadlessImagePasteKind,
) -> Result<AgentImagePastePlan, AppError> {
    match clipboard {
        Ok(()) => Ok(AgentImagePastePlan::OsClipboardCtrlV),
        Err(ClipboardWriteFailure::DisplayUnavailable) => Ok(AgentImagePastePlan::HeadlessFile {
            path: file_path,
            kind,
        }),
        Err(ClipboardWriteFailure::Other(msg)) => {
            Err(AppError::Bad(format!("打开剪贴板失败: {msg}")))
        }
    }
}

/// 按身份表把临时 PNG 路径编成 PTY 输入。
///
/// Business Logic（为什么需要这个函数）:
///     Claude/Gemini/Grok/Cursor/OpenCode 用 `@路径`；Codex 认 bracketed paste 的文件路径；
///     Pi 把绝对路径插入编辑器。
///
/// Code Logic（这个函数做什么）:
///     `AtFileMention` → `@{path} `；`TypedAbsolutePath` → ` {path} `；
///     `BracketedPathPaste` → CSI 200~ path CSI 201~。
pub fn headless_image_paste_input(kind: HeadlessImagePasteKind, path: &Path) -> String {
    match kind {
        HeadlessImagePasteKind::AtFileMention => format!("@{} ", path.display()),
        HeadlessImagePasteKind::TypedAbsolutePath => format!(" {} ", path.display()),
        HeadlessImagePasteKind::BracketedPathPaste => {
            format!("\u{1b}[200~{}\u{1b}[201~", path.display())
        }
    }
}

/// 从 `$DISPLAY` 解析 X11 unix 套接字路径。
///
/// Business Logic（为什么需要这个函数）:
///     SSH X11 forwarding 常留下 `localhost:10.0`，会话结束后套接字消失，arboard 仍去连它。
///
/// Code Logic（这个函数做什么）:
///     取最后一个 `:` 后的显示号（忽略 `.screen`），拼 `{x11_dir}/X{n}`；解析失败返回 None。
#[cfg(any(test, target_os = "linux"))]
pub fn x11_unix_socket_path(display: &str, x11_dir: &Path) -> Option<PathBuf> {
    let after_colon = display.rsplit_once(':')?.1;
    let number = after_colon.split('.').next()?;
    let display_num: u32 = number.parse().ok()?;
    Some(x11_dir.join(format!("X{display_num}")))
}

/// 当前环境里的 Wayland / X11 剪贴板套接字是否真的存在。
///
/// Business Logic（为什么需要这个函数）:
///     仅看 `$DISPLAY` 会被过期 SSH forwarding 骗过，必须核对套接字文件。
///
/// Code Logic（这个函数做什么）:
///     Wayland：`{xdg_runtime_dir}/{wayland_display}` 存在即可达。
///     X11：`x11_unix_socket_path(display, x11_dir)` 存在即可达。
#[cfg(any(test, target_os = "linux"))]
pub fn linux_clipboard_display_reachable(
    wayland_display: Option<&str>,
    xdg_runtime_dir: Option<&Path>,
    display: Option<&str>,
    x11_dir: &Path,
    exists: impl Fn(&Path) -> bool,
) -> bool {
    if let (Some(name), Some(runtime)) = (wayland_display, xdg_runtime_dir) {
        if !name.is_empty() && exists(&runtime.join(name)) {
            return true;
        }
    }
    if let Some(display) = display {
        if let Some(socket) = x11_unix_socket_path(display, x11_dir) {
            if exists(&socket) {
                return true;
            }
        }
    }
    false
}

/// 把 data URL 解码后写成 PNG 文件，供无图形剪贴板时给 Agent `@路径`。
///
/// Business Logic（为什么需要这个函数）:
///     headless owning device 不能写 OS 剪贴板，但 Agent 仍能读本地图片文件。
///
/// Code Logic（这个函数做什么）:
///     解码 RGBA → PNG → `{paste_dir}/{unix_millis}.png`；目录不存在则创建。
pub fn persist_paste_image_png(paste_dir: &Path, data_url: &str) -> Result<PathBuf, AppError> {
    let (width, height, raw) = decode_image_data_url_to_rgba(data_url)?;
    let rgba = image::RgbaImage::from_raw(
        u32::try_from(width).map_err(|_| AppError::Bad("粘贴图片宽度非法".into()))?,
        u32::try_from(height).map_err(|_| AppError::Bad("粘贴图片高度非法".into()))?,
        raw,
    )
    .ok_or_else(|| AppError::Bad("粘贴图片尺寸与像素不匹配".into()))?;
    std::fs::create_dir_all(paste_dir)?;
    let path = paste_dir.join(format!("{}.png", uuid::Uuid::new_v4()));
    let mut buf = std::io::Cursor::new(Vec::with_capacity(64 * 1024));
    image::DynamicImage::ImageRgba8(rgba)
        .write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| AppError::Bad(format!("PNG 编码失败: {e}")))?;
    let png = buf.into_inner();
    if png.len() > MAX_CLIPBOARD_IMAGE_BYTES {
        return Err(AppError::Bad("粘贴图片过大".into()));
    }
    std::fs::write(&path, png)?;
    Ok(path)
}

/// Agent 贴图临时 PNG 目录（位于 data_dir 下，不进用户项目树）。
///
/// Business Logic（为什么需要这个函数）:
///     回退文件不得污染 git 工作区。
///
/// Code Logic（这个函数做什么）:
///     `{data_dir}/tmp/paste-images`。
pub fn paste_image_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("tmp").join("paste-images")
}

/// 尝试把图片写入 OS 剪贴板。
///
/// Business Logic（为什么需要这个函数）:
///     有图形会话时 Agent 从 OS 剪贴板读图；无 X11/Wayland 套接字时不得去连已经死掉的 DISPLAY。
///
/// Code Logic（这个函数做什么）:
///     Linux 先探测套接字（必要时改用 wayland-0 / :0）；不可达直接 DisplayUnavailable。
///     成功则 set_image，并在 Linux 上短暂持有 Clipboard 以便 CLI 来读。
pub fn write_os_clipboard_image(data_url: &str) -> Result<(), ClipboardWriteFailure> {
    #[cfg(target_os = "linux")]
    {
        prepare_linux_clipboard_env();
        if !current_linux_clipboard_display_reachable() {
            return Err(ClipboardWriteFailure::DisplayUnavailable);
        }
    }
    write_arboard_image(data_url)
}

/// 为 Agent 贴图选择 Ctrl+V 或临时文件 mention。
///
/// Business Logic（为什么需要这个函数）:
///     SSH/headless owning device 没有图形剪贴板时，仍要把图交给正在跑的 Agent TUI。
///
/// Code Logic（这个函数做什么）:
///     先校验并尝试 OS 剪贴板；DisplayUnavailable 则把 PNG 写到 data_dir 临时目录。
pub fn prepare_agent_image_paste(
    data_url: &str,
    data_dir: &Path,
    kind: HeadlessImagePasteKind,
) -> Result<AgentImagePastePlan, AppError> {
    decode_image_data_url_to_rgba(data_url)?;
    let clipboard = write_os_clipboard_image(data_url);
    let fallback_path = match &clipboard {
        Err(ClipboardWriteFailure::DisplayUnavailable) => {
            let path = persist_paste_image_png(&paste_image_dir(data_dir), data_url)?;
            tracing::warn!(
                path = %path.display(),
                kind = ?kind,
                "owning device 无图形剪贴板，Agent 贴图改为临时 PNG 文件注入"
            );
            path
        }
        _ => paste_image_dir(data_dir).join("unused.png"),
    };
    agent_image_paste_plan(clipboard, fallback_path, kind)
}

/// 把计划变成 PTY 输入字节。
///
/// Business Logic（为什么需要这个函数）:
///     会话层只负责 write_input；计划层决定发 `\x16` 还是该 Agent 的路径语法。
///
/// Code Logic（这个函数做什么）:
///     Ctrl+V 计划返回 C0 SYN；HeadlessFile 走 `headless_image_paste_input`。
pub fn agent_image_paste_input(plan: &AgentImagePastePlan) -> String {
    match plan {
        AgentImagePastePlan::OsClipboardCtrlV => "\u{0016}".to_string(),
        AgentImagePastePlan::HeadlessFile { path, kind } => headless_image_paste_input(*kind, path),
    }
}

/// Linux：若当前 DISPLAY/Wayland 套接字已死，改用本机图形会话的默认套接字。
#[cfg(target_os = "linux")]
fn prepare_linux_clipboard_env() {
    if current_linux_clipboard_display_reachable() {
        return;
    }
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        let sock = Path::new(&runtime).join("wayland-0");
        if sock.exists() {
            std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
            return;
        }
    }
    if Path::new("/tmp/.X11-unix/X0").exists() {
        std::env::set_var("DISPLAY", ":0");
        if std::env::var_os("XAUTHORITY").is_none() {
            if let Ok(home) = std::env::var("HOME") {
                let xauth = Path::new(&home).join(".Xauthority");
                if xauth.exists() {
                    std::env::set_var("XAUTHORITY", xauth);
                }
            }
        }
    }
}

/// Linux：当前进程环境里的剪贴板显示套接字是否存在。
#[cfg(target_os = "linux")]
fn current_linux_clipboard_display_reachable() -> bool {
    let x11_dir = PathBuf::from("/tmp/.X11-unix");
    let wayland = std::env::var("WAYLAND_DISPLAY").ok();
    let runtime = std::env::var("XDG_RUNTIME_DIR").ok().map(PathBuf::from);
    let display = std::env::var("DISPLAY").ok();
    linux_clipboard_display_reachable(
        wayland.as_deref(),
        runtime.as_deref(),
        display.as_deref(),
        &x11_dir,
        |p| p.exists(),
    )
}

/// 解码 data URL 并用 arboard 写入。
fn write_arboard_image(data_url: &str) -> Result<(), ClipboardWriteFailure> {
    let (width, height, raw) = decode_image_data_url_to_rgba(data_url)
        .map_err(|e| ClipboardWriteFailure::Other(e.to_string()))?;
    let img_data = ImageData {
        width,
        height,
        bytes: raw.into(),
    };
    let mut cb = Clipboard::new().map_err(|e| classify_clipboard_error(&e.to_string()))?;
    cb.set_image(img_data)
        .map_err(|e| classify_clipboard_error(&e.to_string()))?;
    hold_clipboard_for_agent_read(cb);
    Ok(())
}

/// X11/Wayland 剪贴板由本进程托管；立刻 drop 会导致 CLI 读到空。
fn hold_clipboard_for_agent_read(cb: Clipboard) {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::thread::spawn(move || {
            std::thread::sleep(LINUX_CLIPBOARD_HOLD);
            drop(cb);
        });
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        let _ = cb;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_catalog::HeadlessImagePasteKind;
    use std::collections::HashSet;
    use std::path::PathBuf;

    /// Business Logic（为什么需要这个测试）:
    ///     用户在 SSH 远端贴图时看到的正是这条 arboard 文案，必须识别为可回退。
    #[test]
    fn x11_server_error_is_display_unavailable() {
        assert_eq!(
            classify_clipboard_error(
                "Unknown error while interacting with the clipboard: X11 server"
            ),
            ClipboardWriteFailure::DisplayUnavailable
        );
        assert_eq!(
            classify_clipboard_error(
                "Unknown error while interacting with the clipboard: X11 server connection timed out because it was unreachable"
            ),
            ClipboardWriteFailure::DisplayUnavailable
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     图片解码失败不得被当成无显示，否则会写坏文件并吞掉真错误。
    #[test]
    fn decode_failure_is_not_display_unavailable() {
        assert_eq!(
            classify_clipboard_error("图片解码失败: invalid png"),
            ClipboardWriteFailure::Other("图片解码失败: invalid png".into())
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     无 X11 时必须改走文件 mention，而不是继续发 Ctrl+V。
    #[test]
    fn display_unavailable_falls_back_to_file_mention() {
        let path = PathBuf::from("/tmp/cc-partner-paste.png");
        let plan = agent_image_paste_plan(
            Err(ClipboardWriteFailure::DisplayUnavailable),
            path.clone(),
            HeadlessImagePasteKind::AtFileMention,
        )
        .expect("fallback plan");
        assert_eq!(
            plan,
            AgentImagePastePlan::HeadlessFile {
                path,
                kind: HeadlessImagePasteKind::AtFileMention,
            }
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     图形剪贴板可用时保持原合同：写剪贴板 + Ctrl+V。
    #[test]
    fn clipboard_ok_uses_ctrl_v() {
        let plan = agent_image_paste_plan(
            Ok(()),
            PathBuf::from("/tmp/unused.png"),
            HeadlessImagePasteKind::AtFileMention,
        )
        .expect("ctrl-v plan");
        assert_eq!(plan, AgentImagePastePlan::OsClipboardCtrlV);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     非显示类剪贴板故障必须失败，不能静默改成文件路径。
    #[test]
    fn other_clipboard_error_does_not_fallback() {
        let err = agent_image_paste_plan(
            Err(ClipboardWriteFailure::Other(
                "写入剪贴板失败: ConversionFailure".into(),
            )),
            PathBuf::from("/tmp/unused.png"),
            HeadlessImagePasteKind::AtFileMention,
        )
        .expect_err("must not fallback");
        assert!(err.to_string().contains("ConversionFailure"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     无显示注入必须按身份表三种语法编码，Codex 尤其不能打成 `@路径`。
    #[test]
    fn headless_paste_input_encodes_per_agent_kind() {
        let path = Path::new("/tmp/paste.png");
        assert_eq!(
            headless_image_paste_input(HeadlessImagePasteKind::AtFileMention, path),
            "@/tmp/paste.png "
        );
        assert_eq!(
            headless_image_paste_input(HeadlessImagePasteKind::TypedAbsolutePath, path),
            " /tmp/paste.png "
        );
        assert_eq!(
            headless_image_paste_input(HeadlessImagePasteKind::BracketedPathPaste, path),
            "\u{1b}[200~/tmp/paste.png\u{1b}[201~"
        );
        assert_eq!(
            agent_image_paste_input(&AgentImagePastePlan::OsClipboardCtrlV),
            "\u{0016}"
        );
        assert_eq!(
            agent_image_paste_input(&AgentImagePastePlan::HeadlessFile {
                path: PathBuf::from("/tmp/paste.png"),
                kind: HeadlessImagePasteKind::BracketedPathPaste,
            }),
            "\u{1b}[200~/tmp/paste.png\u{1b}[201~"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     SSH forwarding 的 `localhost:10.0` 必须映射到 `X10`，才能判断套接字是否还在。
    #[test]
    fn x11_socket_path_parses_ssh_forwarded_display() {
        let dir = Path::new("/tmp/.X11-unix");
        assert_eq!(
            x11_unix_socket_path("localhost:10.0", dir),
            Some(dir.join("X10"))
        );
        assert_eq!(x11_unix_socket_path(":0", dir), Some(dir.join("X0")));
        assert_eq!(x11_unix_socket_path(":0.0", dir), Some(dir.join("X0")));
        assert!(x11_unix_socket_path("not-a-display", dir).is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     过期 SSH DISPLAY 不能当成剪贴板可用，否则会再撞 X11 server 超时。
    #[test]
    fn stale_ssh_forwarded_display_is_unreachable() {
        let x11_dir = PathBuf::from("/tmp/.X11-unix");
        let exists = |_p: &Path| false;
        assert!(!linux_clipboard_display_reachable(
            None,
            None,
            Some("localhost:10.0"),
            &x11_dir,
            exists,
        ));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     本机图形会话的 wayland-0 / X0 套接字存在时，应继续走 OS 剪贴板。
    #[test]
    fn live_wayland_or_x11_socket_is_reachable() {
        let runtime = PathBuf::from("/run/user/1000");
        let x11_dir = PathBuf::from("/tmp/.X11-unix");
        let mut live = HashSet::new();
        live.insert(runtime.join("wayland-0"));
        let exists = |p: &Path| live.contains(p);
        assert!(linux_clipboard_display_reachable(
            Some("wayland-0"),
            Some(runtime.as_path()),
            Some(":0"),
            &x11_dir,
            exists,
        ));

        live.clear();
        live.insert(x11_dir.join("X0"));
        let exists = |p: &Path| live.contains(p);
        assert!(linux_clipboard_display_reachable(
            None,
            None,
            Some(":0"),
            &x11_dir,
            exists,
        ));
    }

    /// 1×1 透明 PNG data URL。
    const ONE_PX_PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    /// Business Logic（为什么需要这个测试）:
    ///     无图形回退必须写出 Agent 能打开的真实 PNG，而不是空文件。
    #[test]
    fn persist_paste_image_png_writes_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = persist_paste_image_png(dir.path(), ONE_PX_PNG).expect("write png");
        assert!(path.starts_with(dir.path()));
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("png"));
        let bytes = std::fs::read(&path).expect("read png");
        assert!(bytes.starts_with(b"\x89PNG"));
    }
}
