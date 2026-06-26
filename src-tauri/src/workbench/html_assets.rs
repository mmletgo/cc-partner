//! workbench/html_assets.rs — Workbench HTML/Markdown 预览资源读取
//!
//! Business Logic（为什么需要这个模块）:
//!     Workbench HTML iframe 和 Markdown WYSIWYG 预览需要展示项目内相对资源，这些资源需要由后端安全读取后内联。
//!
//! Code Logic（这个模块做什么）:
//!     以 active worktree 根、当前文档路径和资源相对路径为输入，只读解析根内既有文件，
//!     拒绝外链、绝对路径、根外路径和跨根 symlink，并返回 data URL 供前端改写 srcDoc。

use crate::error::AppError;
use crate::workbench::models::WorkbenchHtmlAssetDto;
use crate::workbench::projects::resolve_project_path;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// 单个 HTML 预览资源的最大字节数。
///
/// Business Logic（为什么需要这个常量）:
///     HTML 预览会把资源读入内存并转换为 data URL，必须限制大小避免大型文件阻塞 Workbench。
///
/// Code Logic（这个常量做什么）:
///     以字节为单位定义 10MB 只读资源硬上限。
pub const MAX_HTML_ASSET_BYTES: u64 = 10 * 1024 * 1024;

/// Business Logic（为什么需要这个函数）:
///     HTML/CSS 资源 URL 允许带 query/hash 做缓存标记，但磁盘路径只应使用其中的 pathname 部分。
///
/// Code Logic（这个函数做什么）:
///     去除首个 `?` 或 `#` 及其之后的内容，返回用于文件系统解析的路径片段。
fn strip_query_and_fragment(reference: &str) -> &str {
    let end = reference
        .char_indices()
        .find_map(|(index, character)| matches!(character, '?' | '#').then_some(index))
        .unwrap_or(reference.len());
    &reference[..end]
}

/// Business Logic（为什么需要这个函数）:
///     后端不能把 `http:`、`data:`、`file:` 或 Windows 盘符这类绝对引用当成项目内文件读取。
///
/// Code Logic（这个函数做什么）:
///     检查字符串开头是否符合 URL scheme 语法；`C:\foo` 也会作为 scheme 被拒绝。
fn has_url_scheme(reference: &str) -> bool {
    let Some(colon_index) = reference.find(':') else {
        return false;
    };
    let scheme = &reference[..colon_index];
    let mut chars = scheme.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && chars.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

/// Business Logic（为什么需要这个函数）:
///     HTML 预览资源命令只服务项目内相对资源，不应处理外链、锚点或绝对路径。
///
/// Code Logic（这个函数做什么）:
///     去除空白并校验 URL scheme、协议相对 URL、fragment-only、根路径和平台绝对路径。
fn validate_asset_reference(reference: &str) -> Result<&str, AppError> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return Err(AppError::generic("HTML 预览资源路径不能为空"));
    }
    if trimmed.starts_with('#')
        || trimmed.starts_with("//")
        || trimmed.starts_with('\\')
        || has_url_scheme(trimmed)
    {
        return Err(AppError::generic("HTML 预览资源只能使用项目内相对路径"));
    }

    let path_part = strip_query_and_fragment(trimmed).trim();
    if path_part.is_empty() {
        return Err(AppError::generic("HTML 预览资源只能使用项目内相对路径"));
    }

    let relative = Path::new(path_part);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
    {
        return Err(AppError::generic("HTML 预览资源只能使用项目内相对路径"));
    }
    Ok(path_part)
}

/// Business Logic（为什么需要这个函数）:
///     前端只需要知道项目内相对路径，不能暴露本机绝对路径。
///
/// Code Logic（这个函数做什么）:
///     将 canonical 资源路径剥离 canonical root 前缀，并统一用 `/` 作为分隔符。
fn relative_path(root: &Path, path: &Path) -> Result<String, AppError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| AppError::generic("不能访问项目目录之外的路径"))?;
    Ok(relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/"))
}

/// Business Logic（为什么需要这个函数）:
///     HTML 资源必须按当前 HTML 文档的目录解析，才能支持 `./style.css` 与 `../assets/logo.png`。
///
/// Code Logic（这个函数做什么）:
///     canonicalize worktree root 与 HTML 文档，校验文档和资源都在 root 内，资源必须是既有文件。
fn resolve_html_asset_path(
    root: &Path,
    document_path: &str,
    asset_reference: &str,
) -> Result<(PathBuf, PathBuf), AppError> {
    let canonical_root = root
        .canonicalize()
        .map_err(|error| AppError::generic(format!("项目路径不可访问: {error}")))?;
    if !canonical_root.is_dir() {
        return Err(AppError::generic("项目路径必须是文件夹"));
    }

    let document = resolve_project_path(&canonical_root, document_path)?;
    if !document.is_file() {
        return Err(AppError::generic("HTML 预览文档必须是项目内文件"));
    }
    let document_parent = document.parent().unwrap_or(&canonical_root);
    let asset_relative = validate_asset_reference(asset_reference)?;
    let candidate = document_parent.join(asset_relative);
    let asset = candidate
        .canonicalize()
        .map_err(|error| AppError::generic(format!("HTML 预览资源不可访问: {error}")))?;

    if !asset.starts_with(&canonical_root) {
        return Err(AppError::generic("不能访问项目目录之外的路径"));
    }
    if !asset.is_file() {
        return Err(AppError::generic("HTML 预览资源必须是项目内文件"));
    }

    Ok((canonical_root, asset))
}

/// Business Logic（为什么需要这个函数）:
///     data URL 需要准确 MIME，浏览器才能正确渲染 CSS、图片、字体和媒体资源。
///
/// Code Logic（这个函数做什么）:
///     根据文件扩展名返回常见 Web 资源 MIME，未知扩展回退为 application/octet-stream。
fn mime_for_path(path: &Path) -> String {
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "html" | "htm" => "text/html",
        "json" | "map" => "application/json",
        "txt" | "text" => "text/plain",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// Business Logic（为什么需要这个函数）:
///     CSS 二次资源重写需要读取 text/css 内容；SVG 等文本资源也可以作为调试信息返回给前端。
///
/// Code Logic（这个函数做什么）:
///     对文本类 MIME 尝试 UTF-8 解码，二进制或非 UTF-8 返回 None。
fn text_for_asset(mime: &str, bytes: &[u8]) -> Option<String> {
    let is_text = mime.starts_with("text/")
        || matches!(
            mime,
            "application/json" | "image/svg+xml" | "text/javascript"
        );
    if !is_text {
        return None;
    }
    String::from_utf8(bytes.to_vec()).ok()
}

/// Business Logic（为什么需要这个函数）:
///     前端 HTML 预览需要一个可直接写入 src/href/url() 的内联资源表示。
///
/// Code Logic（这个函数做什么）:
///     安全解析资源路径、限制大小、读取字节并生成 base64 data URL；文本类资源额外返回 UTF-8 文本。
pub fn preview_html_asset(
    root: &Path,
    document_path: &str,
    asset_reference: &str,
) -> Result<WorkbenchHtmlAssetDto, AppError> {
    let (canonical_root, asset_path) =
        resolve_html_asset_path(root, document_path, asset_reference)?;
    let metadata = fs::metadata(&asset_path)?;
    if metadata.len() > MAX_HTML_ASSET_BYTES {
        return Err(AppError::generic(format!(
            "HTML 预览资源超过 {} 字节上限",
            MAX_HTML_ASSET_BYTES
        )));
    }

    let bytes = fs::read(&asset_path)?;
    let mime = mime_for_path(&asset_path);
    let data_url = format!("data:{mime};base64,{}", STANDARD.encode(&bytes));
    let text = text_for_asset(&mime, &bytes);

    Ok(WorkbenchHtmlAssetDto {
        path: relative_path(&canonical_root, &asset_path)?,
        mime,
        size: metadata.len(),
        data_url,
        text,
    })
}
