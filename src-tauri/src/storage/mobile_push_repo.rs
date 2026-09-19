//! storage/mobile_push_repo.rs — Flutter App APNs/FCM token 登记
//!
//! Business Logic: 地址簿里每台 PC 保存手机 token，Attention 变化时经中转发推。
//!     未配置中转则跳过发送，不挡局域网工作台。载荷不含终端/路径/Prompt。
//! Code Logic: SQLite upsert by mobile_device_id；relay 配置空则 Skip。

use crate::error::AppError;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePool;
use sqlx::Row;

/// 建表 SQL。
pub const MOBILE_PUSH_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mobile_push_registrations (
    mobile_device_id TEXT PRIMARY KEY NOT NULL,
    platform TEXT NOT NULL,
    token TEXT NOT NULL,
    app_build TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL
)";

pub const MOBILE_PUSH_SETTINGS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mobile_push_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    relay_url TEXT NOT NULL DEFAULT '',
    relay_token TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL
)";

/// 可选中转配置（空 URL = 不发送）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MobilePushRelay {
    pub url: String,
    pub token: String,
}

impl MobilePushRelay {
    pub fn from_optional(url: Option<&str>, token: Option<&str>) -> Option<Self> {
        let url = url.map(str::trim).filter(|s| !s.is_empty())?;
        Some(Self {
            url: url.to_string(),
            token: token.unwrap_or("").to_string(),
        })
    }

    pub fn is_configured(&self) -> bool {
        !self.url.trim().is_empty()
    }
}

/// 通知载荷（仅导航字段）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MobilePushPayload {
    pub pc_device_id: String,
    pub category: String,
    pub title: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
}

impl MobilePushPayload {
    /// 拒绝终端/路径/Prompt 字段。
    pub fn validate(&self) -> Result<(), AppError> {
        let blob = serde_json::to_value(self).map_err(|e| AppError::Bad(e.to_string()))?;
        reject_sensitive_push_fields(&blob)
    }
}

const FORBIDDEN_PUSH_KEYS: &[&str] = &[
    "terminal",
    "output",
    "prompt",
    "path",
    "filePath",
    "hostPath",
    "cwd",
    "transcript",
];

/// 载荷不得包含终端字节、路径或 Prompt。
pub fn reject_sensitive_push_fields(value: &serde_json::Value) -> Result<(), AppError> {
    if let Some(obj) = value.as_object() {
        for key in obj.keys() {
            if FORBIDDEN_PUSH_KEYS.contains(&key.as_str()) {
                return Err(AppError::Validation(format!(
                    "push payload must not include {key}"
                )));
            }
        }
    }
    Ok(())
}

/// 发送结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MobilePushSendOutcome {
    SkippedUnconfigured,
    WouldSend,
}

/// 未配置中转则跳过；配置了也不在单测里打真实 APNs。
pub fn maybe_queue_notify(
    relay: Option<&MobilePushRelay>,
    payload: &MobilePushPayload,
) -> Result<MobilePushSendOutcome, AppError> {
    payload.validate()?;
    match relay {
        Some(relay) if relay.is_configured() => Ok(MobilePushSendOutcome::WouldSend),
        _ => Ok(MobilePushSendOutcome::SkippedUnconfigured),
    }
}

/// 已登记的手机 token 行。
#[derive(Debug, Clone)]
pub struct MobilePushRegistrationRow {
    pub mobile_device_id: String,
    pub platform: String,
    pub token: String,
}

/// 向中转发出通知（工作台流量仍走 LAN）。
pub async fn send_notify(
    relay: &MobilePushRelay,
    registration: &MobilePushRegistrationRow,
    payload: &MobilePushPayload,
) -> Result<(), AppError> {
    payload.validate()?;
    if !relay.is_configured() {
        return Ok(());
    }
    let client = reqwest::Client::new();
    let mut request = client.post(&relay.url).json(&serde_json::json!({
        "platform": registration.platform,
        "token": registration.token,
        "collapseId": format!("{}:{}", payload.pc_device_id, registration.mobile_device_id),
        "payload": payload,
    }));
    if !relay.token.is_empty() {
        request = request.bearer_auth(&relay.token);
    }
    let response = request
        .send()
        .await
        .map_err(|e| AppError::Bad(e.to_string()))?;
    if !response.status().is_success() {
        return Err(AppError::Bad(format!(
            "push relay HTTP {}",
            response.status()
        )));
    }
    Ok(())
}

/// Token 登记仓储。
pub struct MobilePushRepo {
    db: SqlitePool,
}

impl MobilePushRepo {
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    pub async fn ensure_schema(pool: &SqlitePool) -> Result<(), AppError> {
        sqlx::query(MOBILE_PUSH_SCHEMA).execute(pool).await?;
        sqlx::query(MOBILE_PUSH_SETTINGS_SCHEMA)
            .execute(pool)
            .await?;
        Ok(())
    }

    pub async fn load_relay(&self) -> Result<MobilePushRelay, AppError> {
        let row =
            sqlx::query("SELECT relay_url, relay_token FROM mobile_push_settings WHERE id = 1")
                .fetch_optional(&self.db)
                .await?;
        Ok(match row {
            Some(row) => MobilePushRelay {
                url: row.get::<String, _>("relay_url"),
                token: row.get::<String, _>("relay_token"),
            },
            None => MobilePushRelay::default(),
        })
    }

    pub async fn save_relay(&self, url: &str, token: &str) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO mobile_push_settings (id, relay_url, relay_token, updated_at)
             VALUES (1, ?, ?, datetime('now'))
             ON CONFLICT(id) DO UPDATE SET
                relay_url = excluded.relay_url,
                relay_token = excluded.relay_token,
                updated_at = excluded.updated_at",
        )
        .bind(url.trim())
        .bind(token.trim())
        .execute(&self.db)
        .await?;
        Ok(())
    }

    pub async fn list_all(&self) -> Result<Vec<MobilePushRegistrationRow>, AppError> {
        let rows =
            sqlx::query("SELECT mobile_device_id, platform, token FROM mobile_push_registrations")
                .fetch_all(&self.db)
                .await?;
        Ok(rows
            .into_iter()
            .map(|row| MobilePushRegistrationRow {
                mobile_device_id: row.get("mobile_device_id"),
                platform: row.get("platform"),
                token: row.get("token"),
            })
            .collect())
    }

    pub async fn register(
        &self,
        mobile_device_id: &str,
        platform: &str,
        token: &str,
        app_build: &str,
    ) -> Result<(), AppError> {
        let id = mobile_device_id.trim();
        let platform = platform.trim();
        let token = token.trim();
        if id.is_empty() || platform.is_empty() || token.is_empty() {
            return Err(AppError::Validation(
                "mobileDeviceId, platform and token are required".into(),
            ));
        }
        if platform != "ios" && platform != "android" {
            return Err(AppError::Validation(
                "platform must be ios or android".into(),
            ));
        }
        sqlx::query(
            "INSERT INTO mobile_push_registrations
                (mobile_device_id, platform, token, app_build, updated_at)
             VALUES (?, ?, ?, ?, datetime('now'))
             ON CONFLICT(mobile_device_id) DO UPDATE SET
                platform = excluded.platform,
                token = excluded.token,
                app_build = excluded.app_build,
                updated_at = excluded.updated_at",
        )
        .bind(id)
        .bind(platform)
        .bind(token)
        .bind(app_build)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    pub async fn unregister(&self, mobile_device_id: &str) -> Result<(), AppError> {
        let id = mobile_device_id.trim();
        if id.is_empty() {
            return Err(AppError::Validation("mobileDeviceId is required".into()));
        }
        sqlx::query("DELETE FROM mobile_push_registrations WHERE mobile_device_id = ?")
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    #[cfg(test)]
    pub async fn token_for(&self, mobile_device_id: &str) -> Result<Option<String>, AppError> {
        let row =
            sqlx::query("SELECT token FROM mobile_push_registrations WHERE mobile_device_id = ?")
                .bind(mobile_device_id)
                .fetch_optional(&self.db)
                .await?;
        Ok(row.map(|r| r.get::<String, _>("token")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn setup() -> MobilePushRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        MobilePushRepo::ensure_schema(&pool).await.unwrap();
        MobilePushRepo::new(pool)
    }

    #[tokio::test]
    async fn register_upserts_same_mobile_device_id() {
        let repo = setup().await;
        repo.register("phone-1", "ios", "tok-a", "1.0.0")
            .await
            .unwrap();
        repo.register("phone-1", "ios", "tok-b", "1.0.1")
            .await
            .unwrap();
        assert_eq!(
            repo.token_for("phone-1").await.unwrap().as_deref(),
            Some("tok-b")
        );
    }

    #[tokio::test]
    async fn unregister_removes_row() {
        let repo = setup().await;
        repo.register("phone-1", "android", "tok", "1")
            .await
            .unwrap();
        repo.unregister("phone-1").await.unwrap();
        assert!(repo.token_for("phone-1").await.unwrap().is_none());
    }

    #[test]
    fn unconfigured_relay_does_not_send() {
        let payload = MobilePushPayload {
            pc_device_id: "pc".into(),
            category: "agentNeedsInput".into(),
            title: "待处理".into(),
            body: "需要输入".into(),
            project_id: Some("p1".into()),
            session_id: None,
            worktree_id: None,
        };
        let outcome = maybe_queue_notify(None, &payload).unwrap();
        assert_eq!(outcome, MobilePushSendOutcome::SkippedUnconfigured);
        let empty = MobilePushRelay {
            url: String::new(),
            token: String::new(),
        };
        assert_eq!(
            maybe_queue_notify(Some(&empty), &payload).unwrap(),
            MobilePushSendOutcome::SkippedUnconfigured
        );
    }

    #[test]
    fn configured_relay_would_send_without_sensitive_fields() {
        let payload = MobilePushPayload {
            pc_device_id: "pc".into(),
            category: "agentNeedsInput".into(),
            title: "待处理".into(),
            body: "需要输入".into(),
            project_id: Some("p1".into()),
            session_id: Some("s1".into()),
            worktree_id: None,
        };
        payload.validate().unwrap();
        let json = serde_json::to_value(&payload).unwrap();
        assert!(json.get("terminal").is_none());
        assert!(json.get("path").is_none());
        assert!(json.get("prompt").is_none());
        let relay = MobilePushRelay {
            url: "https://push.example.internal/v1/notify".into(),
            token: "secret".into(),
        };
        assert_eq!(
            maybe_queue_notify(Some(&relay), &payload).unwrap(),
            MobilePushSendOutcome::WouldSend
        );
    }

    #[test]
    fn reject_sensitive_push_fields_blocks_prompt() {
        let value = serde_json::json!({"prompt": "do it"});
        assert!(reject_sensitive_push_fields(&value).is_err());
    }

    #[tokio::test]
    async fn relay_settings_round_trip() {
        let repo = setup().await;
        repo.save_relay("https://push.example.internal/v1/notify", "secret")
            .await
            .unwrap();
        let loaded = repo.load_relay().await.unwrap();
        assert_eq!(loaded.url, "https://push.example.internal/v1/notify");
        assert_eq!(loaded.token, "secret");
    }
}
