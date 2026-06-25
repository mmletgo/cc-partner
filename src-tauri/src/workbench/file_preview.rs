//! workbench/file_preview.rs — 工作台文件预览能力
//!
//! Business Logic（为什么需要这个模块）:
//!     Workbench 文件查看器需要在 Rust 侧按扩展名给出权威文件类型，并为图片与 CSV 提供只读预览数据。
//!
//! Code Logic（这个模块做什么）:
//!     定义图片/CSV 预览大小上限、文件类型检测、类型能力映射、图片 data URL 预览与 CSV 表格预览。

#![allow(dead_code)]

use std::fs;
use std::path::Path;

use crate::error::AppError;
use base64::{engine::general_purpose::STANDARD, Engine as _};

use super::models::{
    WorkbenchCsvPreview, WorkbenchDetectedFileType, WorkbenchFileCapabilities, WorkbenchFileMode,
    WorkbenchImagePreview,
};

/// 单个图片预览文件的最大字节数。
///
/// Business Logic（为什么需要这个常量）:
///     图片预览会把文件读入内存并转换为 data URL，必须限制大小避免阻塞 Workbench。
///
/// Code Logic（这个常量做什么）:
///     以字节为单位定义 10MB 图片预览硬上限。
pub const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;

/// 单个 CSV 预览文件的最大字节数。
///
/// Business Logic（为什么需要这个常量）:
///     CSV 第一版只做轻量只读预览，超大表格应由专门工具打开。
///
/// Code Logic（这个常量做什么）:
///     以字节为单位定义 2MB CSV/TSV 预览硬上限。
pub const MAX_CSV_BYTES: u64 = 2 * 1024 * 1024;

/// Business Logic（为什么需要这个函数）:
///     前端文件查看器需要后端按文件名给出权威类型，避免 UI helper 和后端读写能力不一致。
///
/// Code Logic（这个函数做什么）:
///     提取文件名和扩展名，按图片、Markdown、JSON、TOML、CSV/TSV、SQLite、代码、文本、
///     已知二进制和未知类型返回 `WorkbenchDetectedFileType`；jsonc 明确归为 Unsupported。
pub fn detect_file_type(name: &str) -> WorkbenchDetectedFileType {
    let file_name = Path::new(name)
        .file_name()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_else(|| name.to_ascii_lowercase());

    if file_name.ends_with(".jsonc") {
        return WorkbenchDetectedFileType::Unsupported;
    }

    if is_known_code_name(&file_name) {
        return WorkbenchDetectedFileType::Code;
    }
    if is_known_text_name(&file_name) {
        return WorkbenchDetectedFileType::Text;
    }

    let Some(extension) = Path::new(&file_name)
        .extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
    else {
        return WorkbenchDetectedFileType::Unsupported;
    };

    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "svg" | "tif" | "tiff"
        | "avif" => WorkbenchDetectedFileType::Image,
        "md" | "markdown" | "mdx" => WorkbenchDetectedFileType::Markdown,
        "json" => WorkbenchDetectedFileType::Json,
        "toml" => WorkbenchDetectedFileType::Toml,
        "csv" | "tsv" => WorkbenchDetectedFileType::Csv,
        "db" | "sqlite" | "sqlite3" => WorkbenchDetectedFileType::Sqlite,
        "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "py" | "go" | "java" | "c" | "h"
        | "cc" | "cpp" | "cxx" | "hpp" | "cs" | "swift" | "kt" | "kts" | "php" | "rb" | "sh"
        | "bash" | "zsh" | "fish" | "ps1" | "sql" | "html" | "htm" | "css" | "scss" | "sass"
        | "less" | "vue" | "svelte" | "yml" | "yaml" | "xml" | "graphql" | "gql" | "proto"
        | "lua" | "pl" | "pm" | "r" | "dart" | "ex" | "exs" | "erl" | "hrl" | "clj" | "cljs"
        | "fs" | "fsx" | "scala" | "gradle" => WorkbenchDetectedFileType::Code,
        "txt" | "text" | "log" | "env" | "ini" | "conf" | "config" | "properties" | "lock"
        | "gitignore" | "dockerignore" | "editorconfig" | "gitattributes" => {
            WorkbenchDetectedFileType::Text
        }
        "pdf" | "zip" | "gz" | "tgz" | "tar" | "7z" | "rar" | "xz" | "bz2" | "dmg" | "pkg"
        | "exe" | "dll" | "so" | "dylib" | "bin" | "wasm" | "class" | "jar" | "o" | "a" | "mp3"
        | "mp4" | "mov" | "avi" | "mkv" | "wav" | "flac" => WorkbenchDetectedFileType::Binary,
        _ => WorkbenchDetectedFileType::Unsupported,
    }
}

/// Business Logic（为什么需要这个函数）:
///     文件工作区工具栏需要按类型禁用保存、格式化和编辑入口，避免只读文件被误写。
///
/// Code Logic（这个函数做什么）:
///     根据检测类型返回 preview/edit/format/validate 开关，以及默认模式与可用模式列表。
pub fn capabilities_for_type(kind: &WorkbenchDetectedFileType) -> WorkbenchFileCapabilities {
    match kind {
        WorkbenchDetectedFileType::Json | WorkbenchDetectedFileType::Toml => {
            editable_capabilities(true)
        }
        WorkbenchDetectedFileType::Markdown => WorkbenchFileCapabilities {
            can_preview: true,
            can_edit: true,
            can_format: false,
            must_validate_before_save: false,
            default_mode: WorkbenchFileMode::Wysiwyg,
            available_modes: vec![
                WorkbenchFileMode::Source,
                WorkbenchFileMode::Wysiwyg,
                WorkbenchFileMode::Split,
            ],
        },
        WorkbenchDetectedFileType::Code | WorkbenchDetectedFileType::Text => {
            editable_capabilities(false)
        }
        WorkbenchDetectedFileType::Image
        | WorkbenchDetectedFileType::Csv
        | WorkbenchDetectedFileType::Sqlite => readonly_preview_capabilities(true),
        WorkbenchDetectedFileType::Binary | WorkbenchDetectedFileType::Unsupported => {
            readonly_preview_capabilities(false)
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     用户打开图片文件时需要在 Workbench 内只读查看，不能按文本读取导致乱码。
///
/// Code Logic（这个函数做什么）:
///     拒绝超过 10MB 的图片，读取字节生成 base64 data URL，并用 image crate 解码 raster 图片宽高；
///     SVG 只返回 MIME 和 data URL，宽高保持 None。
pub fn preview_image_file(path: &Path) -> Result<WorkbenchImagePreview, AppError> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err(AppError::generic(format!(
            "图片超过 {} 字节上限，无法在 Workbench 中预览",
            MAX_IMAGE_BYTES
        )));
    }

    let bytes = fs::read(path)?;
    let mime = image_mime(path);
    let (width, height) = if image_extension(path).as_deref() == Some("svg") {
        (None, None)
    } else {
        let image = image::load_from_memory(&bytes)
            .map_err(|err| AppError::generic(format!("图片解码失败: {err}")))?;
        (Some(image.width()), Some(image.height()))
    };
    let data_url = format!("data:{mime};base64,{}", STANDARD.encode(bytes));

    Ok(WorkbenchImagePreview {
        data_url,
        mime,
        width,
        height,
    })
}

/// Business Logic（为什么需要这个函数）:
///     用户打开 CSV/TSV 文件时需要快速浏览表格前几行，第一版不提供写回能力。
///
/// Code Logic（这个函数做什么）:
///     拒绝超过 2MB 的表格文件，用 csv crate 以 flexible 模式读取 header 和最多 limit_rows 行；
///     空 header 自动生成 column_N，额外一行探测用于标记 truncated。
pub fn preview_csv_file(path: &Path, limit_rows: usize) -> Result<WorkbenchCsvPreview, AppError> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_CSV_BYTES {
        return Err(AppError::generic(format!(
            "CSV 超过 {} 字节上限，无法在 Workbench 中预览",
            MAX_CSV_BYTES
        )));
    }

    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .delimiter(csv_delimiter(path))
        .from_path(path)
        .map_err(csv_error)?;
    let headers = reader.headers().map_err(csv_error)?.clone();
    let has_usable_headers = headers.iter().any(|value| !value.trim().is_empty());

    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut max_width = 0usize;
    let mut truncated = false;

    for result in reader.records() {
        let record = result.map_err(csv_error)?;
        max_width = max_width.max(record.len());
        if rows.len() < limit_rows {
            rows.push(record.iter().map(ToOwned::to_owned).collect());
        } else {
            truncated = true;
            break;
        }
    }

    let column_count = headers.len().max(max_width);
    let columns = if has_usable_headers {
        columns_from_headers(&headers, column_count)
    } else {
        fallback_columns(column_count)
    };

    Ok(WorkbenchCsvPreview {
        columns,
        rows,
        truncated,
    })
}

/// Business Logic（为什么需要这个函数）:
///     JSON/TOML/Code/Text 共用编辑器模式能力，避免能力矩阵在多个 match 分支重复且不一致。
///
/// Code Logic（这个函数做什么）:
///     根据是否需要结构化格式化和保存前校验，返回 editor 单模式能力。
fn editable_capabilities(structured: bool) -> WorkbenchFileCapabilities {
    WorkbenchFileCapabilities {
        can_preview: true,
        can_edit: true,
        can_format: structured,
        must_validate_before_save: structured,
        default_mode: WorkbenchFileMode::Editor,
        available_modes: vec![WorkbenchFileMode::Editor],
    }
}

/// Business Logic（为什么需要这个函数）:
///     图片、CSV、SQLite、二进制和未知文件都应进入 viewer 模式，但只有前三者可预览。
///
/// Code Logic（这个函数做什么）:
///     返回不可编辑、不可格式化、viewer 单模式能力，并由参数控制 can_preview。
fn readonly_preview_capabilities(can_preview: bool) -> WorkbenchFileCapabilities {
    WorkbenchFileCapabilities {
        can_preview,
        can_edit: false,
        can_format: false,
        must_validate_before_save: false,
        default_mode: WorkbenchFileMode::Viewer,
        available_modes: vec![WorkbenchFileMode::Viewer],
    }
}

/// Business Logic（为什么需要这个函数）:
///     无扩展代码文件如 Makefile/Dockerfile 应打开代码编辑器，而不是被当成未知文件。
///
/// Code Logic（这个函数做什么）:
///     对常见无扩展代码文件名做小写精确匹配。
fn is_known_code_name(file_name: &str) -> bool {
    matches!(
        file_name,
        "makefile" | "dockerfile" | "containerfile" | "justfile" | "rakefile" | "gemfile"
    )
}

/// Business Logic（为什么需要这个函数）:
///     README/LICENSE/.env 等无扩展文本文件在项目里很常见，应允许按普通文本预览和编辑。
///
/// Code Logic（这个函数做什么）:
///     对常见无扩展或隐藏文本配置文件名做小写精确匹配。
fn is_known_text_name(file_name: &str) -> bool {
    matches!(
        file_name,
        "readme"
            | "license"
            | "licence"
            | "copying"
            | "authors"
            | "contributors"
            | "changelog"
            | "notice"
            | "todo"
            | ".env"
            | ".gitignore"
            | ".dockerignore"
            | ".npmrc"
            | ".nvmrc"
            | ".editorconfig"
            | ".gitattributes"
    )
}

/// Business Logic（为什么需要这个函数）:
///     图片 data URL 需要正确 MIME，浏览器才能按真实格式渲染。
///
/// Code Logic（这个函数做什么）:
///     根据小写扩展名返回常见图片 MIME，未知图片回退为 application/octet-stream。
fn image_mime(path: &Path) -> String {
    match image_extension(path).as_deref() {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("ico") => "image/x-icon",
        Some("svg") => "image/svg+xml",
        Some("tif") | Some("tiff") => "image/tiff",
        Some("avif") => "image/avif",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// Business Logic（为什么需要这个函数）:
///     文件扩展名比较需要大小写不敏感，否则 PNG/JPG 等大写扩展会被误判。
///
/// Code Logic（这个函数做什么）:
///     提取路径扩展名并转换为 ASCII 小写字符串。
fn image_extension(path: &Path) -> Option<String> {
    path.extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
}

/// Business Logic（为什么需要这个函数）:
///     `.tsv` 文件与 CSV 共用表格预览，但分隔符应按扩展名切换。
///
/// Code Logic（这个函数做什么）:
///     TSV 返回 tab 分隔符，其余文件使用逗号分隔符。
fn csv_delimiter(path: &Path) -> u8 {
    if path
        .extension()
        .map(|value| value.to_string_lossy().eq_ignore_ascii_case("tsv"))
        .unwrap_or(false)
    {
        b'\t'
    } else {
        b','
    }
}

/// Business Logic（为什么需要这个函数）:
///     csv crate 的错误类型不能直接跨应用错误边界，需转换为统一中文业务错误。
///
/// Code Logic（这个函数做什么）:
///     把 csv::Error 包装为 AppError::generic，并保留原始错误文本便于排查。
fn csv_error(err: csv::Error) -> AppError {
    AppError::generic(format!("CSV 读取失败: {err}"))
}

/// Business Logic（为什么需要这个函数）:
///     CSV header 部分列为空时，前端仍需要非空列名展示和定位。
///
/// Code Logic（这个函数做什么）:
///     在目标列数范围内读取 header，空白或缺失字段回退为 column_N。
fn columns_from_headers(headers: &csv::StringRecord, column_count: usize) -> Vec<String> {
    (0..column_count)
        .map(|index| {
            headers
                .get(index)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| fallback_column_name(index))
        })
        .collect()
}

/// Business Logic（为什么需要这个函数）:
///     空 header 或无 header 的 CSV 仍需要稳定列名，避免表格列头为空。
///
/// Code Logic（这个函数做什么）:
///     按 1-based 序号生成 column_1、column_2 等 fallback 列名。
fn fallback_columns(column_count: usize) -> Vec<String> {
    (0..column_count).map(fallback_column_name).collect()
}

/// Business Logic（为什么需要这个函数）:
///     fallback 列名需要集中生成，保证 header 缺失和 header 局部空白时命名一致。
///
/// Code Logic（这个函数做什么）:
///     把 0-based index 转换为用户可读的 `column_N`。
fn fallback_column_name(index: usize) -> String {
    format!("column_{}", index + 1)
}

#[cfg(test)]
mod tests {
    use super::super::models::{WorkbenchDetectedFileType, WorkbenchFileMode};
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Business Logic（为什么需要这个函数）:
    ///     文件预览测试需要真实文件路径，同时不能在开发机留下临时文件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 RAII 临时目录，测试结束后由 tempfile 自动清理。
    fn temp_dir() -> TempDir {
        tempfile::tempdir().expect("create temp dir")
    }

    /// Business Logic（为什么需要这个测试）:
    ///     文件类型决定 Workbench 使用编辑器、预览器还是拒绝打开，必须覆盖常用扩展名和 jsonc 例外。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对 markdown/json/toml/csv/sqlite/image/code/text/jsonc/未知扩展名分别断言检测结果。
    #[test]
    fn detect_file_type_covers_supported_extensions_and_jsonc_exception() {
        assert_eq!(
            detect_file_type("README.md"),
            WorkbenchDetectedFileType::Markdown
        );
        assert_eq!(
            detect_file_type("package.json"),
            WorkbenchDetectedFileType::Json
        );
        assert_eq!(
            detect_file_type("Cargo.toml"),
            WorkbenchDetectedFileType::Toml
        );
        assert_eq!(
            detect_file_type("table.csv"),
            WorkbenchDetectedFileType::Csv
        );
        assert_eq!(
            detect_file_type("table.tsv"),
            WorkbenchDetectedFileType::Csv
        );
        assert_eq!(
            detect_file_type("app.sqlite"),
            WorkbenchDetectedFileType::Sqlite
        );
        assert_eq!(
            detect_file_type("app.sqlite3"),
            WorkbenchDetectedFileType::Sqlite
        );
        assert_eq!(
            detect_file_type("app.db"),
            WorkbenchDetectedFileType::Sqlite
        );
        assert_eq!(
            detect_file_type("image.png"),
            WorkbenchDetectedFileType::Image
        );
        assert_eq!(detect_file_type("main.rs"), WorkbenchDetectedFileType::Code);
        assert_eq!(
            detect_file_type("notes.txt"),
            WorkbenchDetectedFileType::Text
        );
        assert_eq!(detect_file_type("README"), WorkbenchDetectedFileType::Text);
        assert_eq!(
            detect_file_type("config.jsonc"),
            WorkbenchDetectedFileType::Unsupported
        );
        assert_eq!(
            detect_file_type("archive.unknown"),
            WorkbenchDetectedFileType::Unsupported
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     只读预览、Markdown 模式和结构化文件校验能力会直接控制前端工具栏按钮。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 CSV/SQLite 不可编辑，Markdown 可用 source/wysiwyg/split，JSON/TOML 可格式化且保存前校验。
    #[test]
    fn capabilities_match_preview_and_editor_modes() {
        let csv = capabilities_for_type(&WorkbenchDetectedFileType::Csv);
        assert!(csv.can_preview);
        assert!(!csv.can_edit);
        assert_eq!(csv.default_mode, WorkbenchFileMode::Viewer);
        assert_eq!(csv.available_modes, vec![WorkbenchFileMode::Viewer]);

        let sqlite = capabilities_for_type(&WorkbenchDetectedFileType::Sqlite);
        assert!(sqlite.can_preview);
        assert!(!sqlite.can_edit);
        assert_eq!(sqlite.default_mode, WorkbenchFileMode::Viewer);

        let markdown = capabilities_for_type(&WorkbenchDetectedFileType::Markdown);
        assert!(markdown.can_preview);
        assert!(markdown.can_edit);
        assert!(!markdown.can_format);
        assert_eq!(markdown.default_mode, WorkbenchFileMode::Wysiwyg);
        assert_eq!(
            markdown.available_modes,
            vec![
                WorkbenchFileMode::Source,
                WorkbenchFileMode::Wysiwyg,
                WorkbenchFileMode::Split,
            ]
        );

        for kind in [
            WorkbenchDetectedFileType::Json,
            WorkbenchDetectedFileType::Toml,
        ] {
            let capabilities = capabilities_for_type(&kind);
            assert!(capabilities.can_preview);
            assert!(capabilities.can_edit);
            assert!(capabilities.can_format);
            assert!(capabilities.must_validate_before_save);
            assert_eq!(capabilities.default_mode, WorkbenchFileMode::Editor);
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     CSV 正常带表头时，前端表格需要稳定 columns 与 rows。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入带 header 的 CSV，断言预览返回 header、两行数据且没有截断。
    #[test]
    fn preview_csv_file_reads_headers_and_rows() {
        let dir = temp_dir();
        let path = dir.path().join("people.csv");
        fs::write(&path, "name,age\nAda,37\nLinus,54\n").expect("write csv");

        let preview = preview_csv_file(&path, 10).expect("preview csv");

        assert_eq!(preview.columns, vec!["name", "age"]);
        assert_eq!(
            preview.rows,
            vec![
                vec!["Ada".to_string(), "37".to_string()],
                vec!["Linus".to_string(), "54".to_string()]
            ]
        );
        assert!(!preview.truncated);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     大 CSV 只显示前 N 行时，前端需要提示用户还有更多内容未展示。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入三行数据并限制两行，断言 rows 只有两行且 truncated=true。
    #[test]
    fn preview_csv_file_marks_truncated_when_rows_exceed_limit() {
        let dir = temp_dir();
        let path = dir.path().join("many.csv");
        fs::write(&path, "name\none\ntwo\nthree\n").expect("write csv");

        let preview = preview_csv_file(&path, 2).expect("preview csv");

        assert_eq!(preview.rows.len(), 2);
        assert!(preview.truncated);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     空表头 CSV 不能把前端表格列名渲染为空白，应自动 fallback 到 column_N。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入空 header 和 flexible 行，断言列名按最大数据列数生成。
    #[test]
    fn preview_csv_file_falls_back_columns_for_empty_headers() {
        let dir = temp_dir();
        let path = dir.path().join("empty-header.csv");
        fs::write(&path, ",,\n1,2\n3,4,5\n").expect("write csv");

        let preview = preview_csv_file(&path, 10).expect("preview csv");

        assert_eq!(preview.columns, vec!["column_1", "column_2", "column_3"]);
        assert_eq!(
            preview.rows,
            vec![
                vec!["1".to_string(), "2".to_string()],
                vec!["3".to_string(), "4".to_string(), "5".to_string()],
            ]
        );
        assert!(!preview.truncated);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     PNG 图片应能在 Workbench 内直接渲染且显示尺寸信息，避免用户只看到文件名。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 image crate 生成 1x1 PNG，断言 data URL、MIME 和宽高正确。
    #[test]
    fn preview_image_file_returns_png_data_url_and_dimensions() {
        let dir = temp_dir();
        let path = dir.path().join("pixel.png");
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]));
        image.save(&path).expect("save png");

        let preview = preview_image_file(&path).expect("preview image");

        assert_eq!(preview.mime, "image/png");
        assert_eq!(preview.width, Some(1));
        assert_eq!(preview.height, Some(1));
        assert!(preview.data_url.starts_with("data:image/png;base64,"));
    }
}
