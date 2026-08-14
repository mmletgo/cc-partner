//! wordgame_repo — 记单词 SQLite 仓储。
//!
//! Business Logic（为什么需要这个模块）:
//!     词频、题型进度、题缓存、ingest 水位和预热状态必须落在本机 data.db，
//!     重启后预热队列与去重水位不能丢。
//!
//! Code Logic（这个模块做什么）:
//!     `CREATE TABLE IF NOT EXISTS` 建五张表；读写经 shared write lease。

use crate::error::AppError;
use crate::storage::maintenance_gate::{with_shared_write_lease, DatabaseMaintenanceGate};
use crate::wordgame::models::{CardPayload, IngestCursor, PreheatStatus, QuestionType, WordLemma};
use crate::wordgame::schedule::TypeProgress;
use sqlx::{Row, SqlitePool};
use std::sync::Arc;

const LEMMAS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS wordgame_lemmas (
    lemma TEXT PRIMARY KEY,
    total_count INTEGER NOT NULL,
    familiar INTEGER NOT NULL,
    interval_step INTEGER NOT NULL,
    due_date TEXT NOT NULL,
    last_seen_at TEXT
)";

const PROGRESS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS wordgame_type_progress (
    lemma TEXT NOT NULL,
    question_type TEXT NOT NULL,
    correct_total INTEGER NOT NULL,
    correct_today INTEGER NOT NULL,
    last_correct_date TEXT,
    PRIMARY KEY (lemma, question_type)
)";

const CARDS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS wordgame_cards (
    lemma TEXT PRIMARY KEY,
    payload_json TEXT NOT NULL,
    generated_at TEXT NOT NULL
)";

const CURSOR_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS wordgame_ingest_cursor (
    device_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    session_id TEXT NOT NULL,
    record_id TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (device_id, provider, session_id)
)";

const PREHEAT_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS wordgame_preheat (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    head_lemma TEXT,
    status TEXT NOT NULL,
    error TEXT,
    retry_count INTEGER NOT NULL,
    updated_at TEXT NOT NULL
)";

/// 记单词仓储。
pub struct WordGameRepo {
    db: SqlitePool,
    gate: Arc<DatabaseMaintenanceGate>,
}

impl WordGameRepo {
    /// 测试用独立 gate。
    #[cfg(test)]
    pub fn new(db: SqlitePool) -> Self {
        Self::with_gate(db, Arc::new(DatabaseMaintenanceGate::new()))
    }

    /// 生产构造：共享 AppState maintenance_gate。
    pub fn with_gate(db: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { db, gate }
    }

    /// 幂等建表。
    ///
    /// Business Logic:
    ///     旧库无 wordgame 表时首次启动必须可升级，禁止 sqlx::migrate!。
    pub async fn ensure_schema(pool: &SqlitePool) -> Result<(), AppError> {
        for sql in [
            LEMMAS_SCHEMA,
            PROGRESS_SCHEMA,
            CARDS_SCHEMA,
            CURSOR_SCHEMA,
            PREHEAT_SCHEMA,
        ] {
            sqlx::query(sql).execute(pool).await?;
        }
        Ok(())
    }

    /// 累加 lemma 词频；新词 due_date=today。
    pub async fn bump_counts(&self, today: &str, counts: &[(String, i64)]) -> Result<(), AppError> {
        if counts.is_empty() {
            return Ok(());
        }
        let today = today.to_string();
        let counts = counts.to_vec();
        with_shared_write_lease(&self.gate, async {
            for (lemma, count) in counts {
                if count <= 0 {
                    continue;
                }
                sqlx::query(
                    "INSERT INTO wordgame_lemmas (lemma, total_count, familiar, interval_step, due_date, last_seen_at)
                     VALUES (?, ?, 0, 0, ?, NULL)
                     ON CONFLICT(lemma) DO UPDATE SET total_count = total_count + excluded.total_count",
                )
                .bind(&lemma)
                .bind(count)
                .bind(&today)
                .execute(&self.db)
                .await?;
            }
            Ok(())
        })
        .await
    }

    /// 列出全部 lemma。
    pub async fn list_lemmas(&self) -> Result<Vec<WordLemma>, AppError> {
        let rows = sqlx::query(
            "SELECT lemma, total_count, familiar, interval_step, due_date, last_seen_at
             FROM wordgame_lemmas ORDER BY total_count DESC, lemma ASC",
        )
        .fetch_all(&self.db)
        .await?;
        Ok(rows.into_iter().map(row_to_lemma).collect())
    }

    /// 未熟悉生词按词频降序。
    #[allow(dead_code)]
    pub async fn list_unfamiliar_by_freq(&self, limit: i64) -> Result<Vec<WordLemma>, AppError> {
        let rows = sqlx::query(
            "SELECT lemma, total_count, familiar, interval_step, due_date, last_seen_at
             FROM wordgame_lemmas WHERE familiar = 0
             ORDER BY total_count DESC, lemma ASC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.db)
        .await?;
        Ok(rows.into_iter().map(row_to_lemma).collect())
    }

    /// 读取一词。
    pub async fn get_lemma(&self, lemma: &str) -> Result<Option<WordLemma>, AppError> {
        let row = sqlx::query(
            "SELECT lemma, total_count, familiar, interval_step, due_date, last_seen_at
             FROM wordgame_lemmas WHERE lemma = ?",
        )
        .bind(lemma)
        .fetch_optional(&self.db)
        .await?;
        Ok(row.map(row_to_lemma))
    }

    /// 写回一词复习状态。
    pub async fn upsert_lemma(&self, word: &WordLemma) -> Result<(), AppError> {
        let word = word.clone();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO wordgame_lemmas (lemma, total_count, familiar, interval_step, due_date, last_seen_at)
                 VALUES (?, ?, ?, ?, ?, ?)
                 ON CONFLICT(lemma) DO UPDATE SET
                    familiar = excluded.familiar,
                    interval_step = excluded.interval_step,
                    due_date = excluded.due_date,
                    last_seen_at = excluded.last_seen_at",
            )
            .bind(&word.lemma)
            .bind(word.total_count)
            .bind(i64::from(word.familiar))
            .bind(word.interval_step)
            .bind(&word.due_date)
            .bind(word.last_seen_at.as_deref())
            .execute(&self.db)
            .await?;
            Ok(())
        })
        .await
    }

    /// 读取一词全部题型进度。
    pub async fn list_progress_for(&self, lemma: &str) -> Result<Vec<TypeProgress>, AppError> {
        let rows = sqlx::query(
            "SELECT lemma, question_type, correct_total, correct_today, last_correct_date
             FROM wordgame_type_progress WHERE lemma = ?",
        )
        .bind(lemma)
        .fetch_all(&self.db)
        .await?;
        Ok(rows.into_iter().filter_map(row_to_progress).collect())
    }

    /// 读取全部进度（调度用）。
    pub async fn list_all_progress(&self) -> Result<Vec<TypeProgress>, AppError> {
        let rows = sqlx::query(
            "SELECT lemma, question_type, correct_total, correct_today, last_correct_date
             FROM wordgame_type_progress",
        )
        .fetch_all(&self.db)
        .await?;
        Ok(rows.into_iter().filter_map(row_to_progress).collect())
    }

    /// 写回题型进度。
    pub async fn upsert_progress(&self, progress: &TypeProgress) -> Result<(), AppError> {
        let progress = progress.clone();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO wordgame_type_progress
                    (lemma, question_type, correct_total, correct_today, last_correct_date)
                 VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT(lemma, question_type) DO UPDATE SET
                    correct_total = excluded.correct_total,
                    correct_today = excluded.correct_today,
                    last_correct_date = excluded.last_correct_date",
            )
            .bind(&progress.lemma)
            .bind(progress.question_type.as_str())
            .bind(progress.correct_total)
            .bind(progress.correct_today)
            .bind(progress.last_correct_date.as_deref())
            .execute(&self.db)
            .await?;
            Ok(())
        })
        .await
    }

    /// 保存 7 题缓存。
    pub async fn save_card(
        &self,
        payload: &CardPayload,
        generated_at: &str,
    ) -> Result<(), AppError> {
        let json = serde_json::to_string(payload)?;
        let lemma = payload.lemma.clone();
        let generated_at = generated_at.to_string();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO wordgame_cards (lemma, payload_json, generated_at)
                 VALUES (?, ?, ?)
                 ON CONFLICT(lemma) DO UPDATE SET
                    payload_json = excluded.payload_json,
                    generated_at = excluded.generated_at",
            )
            .bind(&lemma)
            .bind(&json)
            .bind(&generated_at)
            .execute(&self.db)
            .await?;
            Ok(())
        })
        .await
    }

    /// 读取一词题缓存。
    pub async fn get_card(&self, lemma: &str) -> Result<Option<CardPayload>, AppError> {
        let row = sqlx::query("SELECT payload_json FROM wordgame_cards WHERE lemma = ?")
            .bind(lemma)
            .fetch_optional(&self.db)
            .await?;
        match row {
            Some(row) => {
                let json: String = row.try_get("payload_json")?;
                Ok(Some(serde_json::from_str(&json)?))
            }
            None => Ok(None),
        }
    }

    /// 已缓存的未熟悉生词数。
    pub async fn count_cached_unfamiliar(&self) -> Result<i64, AppError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM wordgame_lemmas l
             INNER JOIN wordgame_cards c ON c.lemma = l.lemma
             WHERE l.familiar = 0",
        )
        .fetch_one(&self.db)
        .await?;
        Ok(count)
    }

    /// 未熟悉生词总数。
    pub async fn count_unfamiliar(&self) -> Result<i64, AppError> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM wordgame_lemmas WHERE familiar = 0")
                .fetch_one(&self.db)
                .await?;
        Ok(count)
    }

    /// 下一个需要出题的高词频生词（尚无卡片）。
    pub async fn next_uncached_unfamiliar(&self) -> Result<Option<String>, AppError> {
        let lemma: Option<String> = sqlx::query_scalar(
            "SELECT l.lemma FROM wordgame_lemmas l
             LEFT JOIN wordgame_cards c ON c.lemma = l.lemma
             WHERE l.familiar = 0 AND c.lemma IS NULL
             ORDER BY l.total_count DESC, l.lemma ASC LIMIT 1",
        )
        .fetch_optional(&self.db)
        .await?;
        Ok(lemma)
    }

    /// 读取 ingest 水位。
    pub async fn list_cursors(&self) -> Result<Vec<IngestCursor>, AppError> {
        let rows = sqlx::query(
            "SELECT device_id, provider, session_id, record_id FROM wordgame_ingest_cursor",
        )
        .fetch_all(&self.db)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| IngestCursor {
                device_id: row.get("device_id"),
                provider: row.get("provider"),
                session_id: row.get("session_id"),
                record_id: row.get("record_id"),
            })
            .collect())
    }

    /// 推进水位。
    pub async fn upsert_cursor(
        &self,
        cursor: &IngestCursor,
        updated_at: &str,
    ) -> Result<(), AppError> {
        let cursor = cursor.clone();
        let updated_at = updated_at.to_string();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO wordgame_ingest_cursor (device_id, provider, session_id, record_id, updated_at)
                 VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT(device_id, provider, session_id) DO UPDATE SET
                    record_id = excluded.record_id,
                    updated_at = excluded.updated_at",
            )
            .bind(&cursor.device_id)
            .bind(&cursor.provider)
            .bind(&cursor.session_id)
            .bind(&cursor.record_id)
            .bind(&updated_at)
            .execute(&self.db)
            .await?;
            Ok(())
        })
        .await
    }

    /// 读取预热单行。
    pub async fn get_preheat(&self) -> Result<PreheatStatus, AppError> {
        let row = sqlx::query(
            "SELECT head_lemma, status, error, retry_count FROM wordgame_preheat WHERE id = 1",
        )
        .fetch_optional(&self.db)
        .await?;
        Ok(match row {
            Some(row) => PreheatStatus {
                head_lemma: row.get("head_lemma"),
                status: row.get("status"),
                error: row.get("error"),
                retry_count: row.get("retry_count"),
            },
            None => PreheatStatus {
                head_lemma: None,
                status: "idle".into(),
                error: None,
                retry_count: 0,
            },
        })
    }

    /// 写预热单行。
    pub async fn upsert_preheat(
        &self,
        status: &PreheatStatus,
        updated_at: &str,
    ) -> Result<(), AppError> {
        let status = status.clone();
        let updated_at = updated_at.to_string();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO wordgame_preheat (id, head_lemma, status, error, retry_count, updated_at)
                 VALUES (1, ?, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET
                    head_lemma = excluded.head_lemma,
                    status = excluded.status,
                    error = excluded.error,
                    retry_count = excluded.retry_count,
                    updated_at = excluded.updated_at",
            )
            .bind(status.head_lemma.as_deref())
            .bind(&status.status)
            .bind(status.error.as_deref())
            .bind(status.retry_count)
            .bind(&updated_at)
            .execute(&self.db)
            .await?;
            Ok(())
        })
        .await
    }
}

fn row_to_lemma(row: sqlx::sqlite::SqliteRow) -> WordLemma {
    WordLemma {
        lemma: row.get("lemma"),
        total_count: row.get("total_count"),
        familiar: row.get::<i64, _>("familiar") != 0,
        interval_step: row.get("interval_step"),
        due_date: row.get("due_date"),
        last_seen_at: row.get("last_seen_at"),
    }
}

fn row_to_progress(row: sqlx::sqlite::SqliteRow) -> Option<TypeProgress> {
    let question_type = QuestionType::parse(row.get::<String, _>("question_type").as_str())?;
    Some(TypeProgress {
        lemma: row.get("lemma"),
        question_type,
        correct_total: row.get("correct_total"),
        correct_today: row.get("correct_today"),
        last_correct_date: row.get("last_correct_date"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wordgame::models::{QuestionType, QuizItem};
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn repo() -> WordGameRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        WordGameRepo::ensure_schema(&pool).await.unwrap();
        WordGameRepo::new(pool)
    }

    #[tokio::test]
    async fn bump_is_idempotent_on_conflict_and_schema_is_repeatable() {
        let repo = repo().await;
        WordGameRepo::ensure_schema(&repo.db).await.unwrap();
        repo.bump_counts("2026-08-14", &[("feature".into(), 2)])
            .await
            .unwrap();
        repo.bump_counts("2026-08-14", &[("feature".into(), 3)])
            .await
            .unwrap();
        let word = repo.get_lemma("feature").await.unwrap().unwrap();
        assert_eq!(word.total_count, 5);
        assert_eq!(word.due_date, "2026-08-14");
    }

    #[tokio::test]
    async fn card_round_trip() {
        let repo = repo().await;
        let payload = CardPayload {
            lemma: "feature".into(),
            items: vec![QuizItem {
                question_type: QuestionType::EnToZh,
                prompt: "feature".into(),
                options: vec!["特征".into(), "错误".into()],
                answer: "特征".into(),
            }],
        };
        repo.save_card(&payload, "2026-08-14T00:00:00Z")
            .await
            .unwrap();
        let loaded = repo.get_card("feature").await.unwrap().unwrap();
        assert_eq!(loaded, payload);
    }
}
