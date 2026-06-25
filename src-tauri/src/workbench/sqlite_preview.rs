//! workbench/sqlite_preview.rs — 工作台 SQLite 只读预览能力
//!
//! Business Logic（为什么需要这个模块）:
//!     Workbench 文件查看器需要只读浏览 SQLite 数据库的用户表和少量行，避免误写用户项目数据。
//!
//! Code Logic（这个模块做什么）:
//!     定义 SQLite 文件大小上限、只读连接、用户表枚举、identifier 引用和单表行预览。

#![allow(dead_code)]

use std::path::Path;

use crate::error::AppError;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, TypeInfo, ValueRef};

use super::models::WorkbenchSqlitePreview;

/// 单个 SQLite 预览文件的最大字节数。
///
/// Business Logic（为什么需要这个常量）:
///     数据库文件可能很大，Workbench 第一版只做轻量只读预览，避免打开超大文件拖慢 UI。
///
/// Code Logic（这个常量做什么）:
///     以字节为单位定义 100MB SQLite 预览硬上限。
pub const MAX_SQLITE_BYTES: u64 = 100 * 1024 * 1024;

/// Business Logic（为什么需要这个函数）:
///     用户选择 SQLite 文件时需要先浏览表名和前几行数据，但不能执行任意 SQL 或写入数据库。
///
/// Code Logic（这个函数做什么）:
///     拒绝超过 100MB 的数据库文件，用 read_only SQLite 连接列出用户表，校验 selected_table，
///     对选中表查询 LIMIT+1 行并把单元格转换为字符串矩阵。
pub async fn preview_sqlite_file(
    path: &Path,
    selected_table: Option<String>,
    limit_rows: i64,
) -> Result<WorkbenchSqlitePreview, AppError> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > MAX_SQLITE_BYTES {
        return Err(AppError::generic(format!(
            "SQLite 文件超过 {} 字节上限，无法在 Workbench 中预览",
            MAX_SQLITE_BYTES
        )));
    }

    let pool = open_readonly_pool(path).await?;
    let tables = list_user_tables(&pool).await?;
    let selected_table = select_table(selected_table, &tables)?;

    let Some(table_name) = selected_table else {
        return Ok(WorkbenchSqlitePreview {
            tables,
            selected_table: None,
            columns: Vec::new(),
            rows: Vec::new(),
            truncated: false,
        });
    };

    let quoted_table = quote_identifier(&table_name);
    let columns = table_columns(&pool, &quoted_table).await?;
    let clamped_limit = limit_rows.clamp(0, 500);
    let fetch_limit = clamped_limit + 1;
    let query = format!("SELECT * FROM {quoted_table} LIMIT ?");
    let fetched_rows = sqlx::query(&query)
        .bind(fetch_limit)
        .fetch_all(&pool)
        .await?;
    let truncated = fetched_rows.len() as i64 > clamped_limit;
    let rows = fetched_rows
        .iter()
        .take(clamped_limit as usize)
        .map(row_to_strings)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(WorkbenchSqlitePreview {
        tables,
        selected_table: Some(table_name),
        columns,
        rows,
        truncated,
    })
}

/// Business Logic（为什么需要这个函数）:
///     SQLite 表名来自用户文件，查询时必须防止 identifier 注入和特殊字符破坏 SQL。
///
/// Code Logic（这个函数做什么）:
///     用双引号包裹 identifier，并把内部双引号转义为两个双引号。
pub(crate) fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

/// Business Logic（为什么需要这个函数）:
///     SQLite 预览必须使用只读连接打开用户文件，避免 Workbench 文件查看器具备写库能力。
///
/// Code Logic（这个函数做什么）:
///     用 SqliteConnectOptions 指向文件路径，设置 read_only=true/create_if_missing=false，并创建单连接 pool。
async fn open_readonly_pool(path: &Path) -> Result<sqlx::SqlitePool, AppError> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .create_if_missing(false);

    Ok(SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?)
}

/// Business Logic（为什么需要这个函数）:
///     SQLite 文件可能包含系统表，Workbench 只应展示用户实际创建的数据表。
///
/// Code Logic（这个函数做什么）:
///     查询 sqlite_master 中 type='table' 且 name 不以 sqlite_ 开头的表名，并按名称排序。
async fn list_user_tables(pool: &sqlx::SqlitePool) -> Result<Vec<String>, AppError> {
    let rows = sqlx::query(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
         ORDER BY name COLLATE NOCASE, name",
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| row.try_get::<String, _>("name").map_err(AppError::from))
        .collect()
}

/// Business Logic（为什么需要这个函数）:
///     用户未选择表时应默认打开第一张用户表；选择缺失表时必须给出明确业务错误。
///
/// Code Logic（这个函数做什么）:
///     对 selected_table 做空字符串归一，并在 tables 白名单中验证存在性。
fn select_table(
    selected_table: Option<String>,
    tables: &[String],
) -> Result<Option<String>, AppError> {
    let normalized = selected_table.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(value)
        }
    });

    match normalized {
        Some(table) if tables.iter().any(|candidate| candidate == &table) => Ok(Some(table)),
        Some(table) => Err(AppError::generic(format!("SQLite 表不存在: {table}"))),
        None => Ok(tables.first().cloned()),
    }
}

/// Business Logic（为什么需要这个函数）:
///     前端表格需要列名才能渲染表头，且列名必须来自 SQLite schema 而不是用户输入。
///
/// Code Logic（这个函数做什么）:
///     用 PRAGMA table_info 读取指定已转义表名的列定义，并按 cid 顺序返回 name 字段。
async fn table_columns(
    pool: &sqlx::SqlitePool,
    quoted_table: &str,
) -> Result<Vec<String>, AppError> {
    let query = format!("PRAGMA table_info({quoted_table})");
    let rows = sqlx::query(&query).fetch_all(pool).await?;
    rows.into_iter()
        .map(|row| row.try_get::<String, _>("name").map_err(AppError::from))
        .collect()
}

/// Business Logic（为什么需要这个函数）:
///     SQLite 行包含多种动态类型，前端预览表格只接受 JSON-safe 字符串矩阵。
///
/// Code Logic（这个函数做什么）:
///     遍历 row 的所有列，逐个调用 sqlite_cell_to_string 转换为字符串。
fn row_to_strings(row: &SqliteRow) -> Result<Vec<String>, AppError> {
    (0..row.columns().len())
        .map(|index| sqlite_cell_to_string(row, index))
        .collect()
}

/// Business Logic（为什么需要这个函数）:
///     数据库单元格可能是 NULL、文本、数值或 BLOB，预览不能 panic 或泄露二进制内容。
///
/// Code Logic（这个函数做什么）:
///     根据 sqlx 暴露的 SQLite 值类型转换为字符串；BLOB 仅显示字节数，NULL 显示空字符串。
fn sqlite_cell_to_string(row: &SqliteRow, index: usize) -> Result<String, AppError> {
    let type_name = {
        let raw = row.try_get_raw(index)?;
        if raw.is_null() {
            return Ok(String::new());
        }
        raw.type_info().name().to_ascii_uppercase()
    };

    match type_name.as_str() {
        "TEXT" => Ok(row.try_get::<String, _>(index)?),
        "INTEGER" => Ok(row.try_get::<i64, _>(index)?.to_string()),
        "REAL" => Ok(row.try_get::<f64, _>(index)?.to_string()),
        "BLOB" => {
            let bytes = row.try_get::<Vec<u8>, _>(index)?;
            Ok(format!("<binary {} bytes>", bytes.len()))
        }
        _ => sqlite_cell_to_string_by_fallback(row, index, &type_name),
    }
}

/// Business Logic（为什么需要这个函数）:
///     用户数据库可能包含 SQLite 扩展类型名，预览应尽量转成可展示文本而不是直接失败。
///
/// Code Logic（这个函数做什么）:
///     依次尝试 String/i64/f64/Vec<u8> 解码，均失败时返回包含类型名的业务错误。
fn sqlite_cell_to_string_by_fallback(
    row: &SqliteRow,
    index: usize,
    type_name: &str,
) -> Result<String, AppError> {
    if let Ok(value) = row.try_get::<String, _>(index) {
        return Ok(value);
    }
    if let Ok(value) = row.try_get::<i64, _>(index) {
        return Ok(value.to_string());
    }
    if let Ok(value) = row.try_get::<f64, _>(index) {
        return Ok(value.to_string());
    }
    if let Ok(bytes) = row.try_get::<Vec<u8>, _>(index) {
        return Ok(format!("<binary {} bytes>", bytes.len()));
    }

    Err(AppError::generic(format!(
        "暂不支持预览 SQLite 单元格类型: {type_name}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::SqlitePool;
    use tempfile::TempDir;

    /// Business Logic（为什么需要这个函数）:
    ///     SQLite 预览测试需要真实数据库文件验证 sqlx 行读取和只读打开行为。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 RAII 临时目录，测试结束后自动清理数据库文件。
    fn temp_dir() -> TempDir {
        tempfile::tempdir().expect("create temp dir")
    }

    /// Business Logic（为什么需要这个函数）:
    ///     测试数据准备需要可写连接，预览函数自身则必须另开只读连接。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用 sqlx SQLite pool 创建或打开指定路径的数据库文件。
    async fn writable_pool(path: &Path) -> SqlitePool {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .expect("connect sqlite")
    }

    /// Business Logic（为什么需要这个测试）:
    ///     未指定表时，Workbench 应列出所有用户表并默认展示第一张表，系统表不应出现在 UI。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建两张用户表，断言 tables、selected_table、columns 与 rows 使用第一张表。
    #[tokio::test]
    async fn preview_sqlite_file_lists_tables_and_defaults_to_first_table() {
        let dir = temp_dir();
        let path = dir.path().join("data.sqlite");
        let pool = writable_pool(&path).await;
        sqlx::query("CREATE TABLE beta (id INTEGER PRIMARY KEY, label TEXT)")
            .execute(&pool)
            .await
            .expect("create beta");
        sqlx::query("CREATE TABLE alpha (id INTEGER PRIMARY KEY, name TEXT)")
            .execute(&pool)
            .await
            .expect("create alpha");
        sqlx::query("INSERT INTO alpha (id, name) VALUES (1, 'Ada')")
            .execute(&pool)
            .await
            .expect("insert alpha");
        pool.close().await;

        let preview = preview_sqlite_file(&path, None, 10)
            .await
            .expect("preview sqlite");

        assert_eq!(preview.tables, vec!["alpha", "beta"]);
        assert_eq!(preview.selected_table.as_deref(), Some("alpha"));
        assert_eq!(preview.columns, vec!["id", "name"]);
        assert_eq!(preview.rows, vec![vec!["1".to_string(), "Ada".to_string()]]);
        assert!(!preview.truncated);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户切换表时，预览必须展示所选表，并在超出行数限制时提示截断。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建三行数据，选择该表并限制两行，断言 rows 两行且 truncated=true。
    #[tokio::test]
    async fn preview_sqlite_file_selects_table_and_marks_truncated() {
        let dir = temp_dir();
        let path = dir.path().join("data.sqlite");
        let pool = writable_pool(&path).await;
        sqlx::query("CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT)")
            .execute(&pool)
            .await
            .expect("create items");
        for id in 1..=3 {
            sqlx::query("INSERT INTO items (id, label) VALUES (?, ?)")
                .bind(id)
                .bind(format!("item-{id}"))
                .execute(&pool)
                .await
                .expect("insert item");
        }
        pool.close().await;

        let preview = preview_sqlite_file(&path, Some("items".to_string()), 2)
            .await
            .expect("preview sqlite");

        assert_eq!(preview.selected_table.as_deref(), Some("items"));
        assert_eq!(preview.rows.len(), 2);
        assert!(preview.truncated);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     前端传入不存在的表名时，后端必须返回业务错误，而不是拼接 SQL 后返回空数据。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建一张表后选择缺失表，断言错误消息包含不存在提示。
    #[tokio::test]
    async fn preview_sqlite_file_rejects_missing_selected_table() {
        let dir = temp_dir();
        let path = dir.path().join("data.sqlite");
        let pool = writable_pool(&path).await;
        sqlx::query("CREATE TABLE existing (id INTEGER)")
            .execute(&pool)
            .await
            .expect("create table");
        pool.close().await;

        let err = preview_sqlite_file(&path, Some("missing".to_string()), 10)
            .await
            .expect_err("missing table rejected");

        assert!(err.to_string().contains("不存在"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     SQLite 表名可能包含双引号等特殊字符，预览不能因此 SQL 注入或查询失败。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建包含双引号的表名，断言 quote_identifier 转义正确且预览能查到数据。
    #[tokio::test]
    async fn preview_sqlite_file_quotes_table_names_with_double_quotes() {
        let dir = temp_dir();
        let path = dir.path().join("data.sqlite");
        let pool = writable_pool(&path).await;
        sqlx::query("CREATE TABLE \"odd\"\"name\" (value TEXT)")
            .execute(&pool)
            .await
            .expect("create special table");
        sqlx::query("INSERT INTO \"odd\"\"name\" (value) VALUES ('ok')")
            .execute(&pool)
            .await
            .expect("insert special table");
        pool.close().await;

        assert_eq!(quote_identifier("odd\"name"), "\"odd\"\"name\"");

        let preview = preview_sqlite_file(&path, Some("odd\"name".to_string()), 10)
            .await
            .expect("preview sqlite");

        assert_eq!(preview.selected_table.as_deref(), Some("odd\"name"));
        assert_eq!(preview.rows, vec![vec!["ok".to_string()]]);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     SQLite 单元格类型多样，前端表格只接收字符串矩阵且不能因 NULL/BLOB panic。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入 TEXT/INTEGER/REAL/BLOB/NULL，断言转换为稳定字符串。
    #[tokio::test]
    async fn preview_sqlite_file_stringifies_common_cell_types() {
        let dir = temp_dir();
        let path = dir.path().join("data.sqlite");
        let pool = writable_pool(&path).await;
        sqlx::query("CREATE TABLE values_table (text_value TEXT, int_value INTEGER, real_value REAL, blob_value BLOB, null_value TEXT)")
            .execute(&pool)
            .await
            .expect("create values table");
        sqlx::query("INSERT INTO values_table VALUES ('text', 42, 3.5, x'010203', NULL)")
            .execute(&pool)
            .await
            .expect("insert values");
        pool.close().await;

        let preview = preview_sqlite_file(&path, Some("values_table".to_string()), 10)
            .await
            .expect("preview sqlite");

        assert_eq!(
            preview.rows,
            vec![vec![
                "text".to_string(),
                "42".to_string(),
                "3.5".to_string(),
                "<binary 3 bytes>".to_string(),
                "".to_string(),
            ]]
        );
    }
}
