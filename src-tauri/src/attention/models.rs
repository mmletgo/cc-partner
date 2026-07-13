//! attention/models.rs — Attention 快照与条目 DTO。
//!
//! Business Logic（为什么需要这个模块）:
//!     全局 Inbox 需要在桌面与移动端共享稳定的实时投影协议，分类/来源/跳转目标必须精确
//!     序列化为 TypeScript 字面量，避免两端各自猜测字段名或拼装后端 URL。
//!
//! Code Logic（这个模块做什么）:
//!     定义 category/freshness/sourceKind/target/item/counts/snapshot 的 camelCase DTO，
//!     并通过序列化单测锁定 TS 契约（无后端 URL、cachedAt 可空）。

use serde::{Deserialize, Serialize};

/// Attention 条目分类。
///
/// Business Logic（为什么需要这个枚举）:
///     Inbox 按“需要决策 / 被阻塞 / 环境依赖”三档呈现，badge 与排序都依赖稳定分类值。
///
/// Code Logic（这个枚举做什么）:
///     序列化为 `'decision' | 'blocked' | 'environment'`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttentionCategory {
    Decision,
    Blocked,
    Environment,
}

impl AttentionCategory {
    /// Business Logic（为什么需要这个函数）:
    ///     聚合排序固定为 decision → blocked → environment，需要稳定整数序。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回分类排序权重，数值越小越靠前。
    pub fn sort_rank(self) -> u8 {
        match self {
            Self::Decision => 0,
            Self::Blocked => 1,
            Self::Environment => 2,
        }
    }
}

/// Attention 条目新鲜度。
///
/// Business Logic（为什么需要这个枚举）:
///     远端 mirror 回退时用户需要知道条目是 live 还是 cached，避免把陈旧数据当实时状态。
///
/// Code Logic（这个枚举做什么）:
///     序列化为 `'live' | 'cached'`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttentionFreshness {
    Live,
    Cached,
}

/// Attention 条目来源类型。
///
/// Business Logic（为什么需要这个枚举）:
///     前端按 sourceKind 映射操作文案与图标，source 投影必须输出稳定字面量。
///
/// Code Logic（这个枚举做什么）:
///     序列化为 orchestratorHumanReview / orchestratorBlocked / remoteOutboxFailed / workbenchDependency。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttentionSourceKind {
    OrchestratorHumanReview,
    OrchestratorBlocked,
    RemoteOutboxFailed,
    WorkbenchDependency,
}

/// Attention 条目所属项目摘要。
///
/// Business Logic（为什么需要这个结构体）:
///     列表需要展示项目名与 local/remote 语义，但不能携带后端 base URL。
///
/// Code Logic（这个结构体做什么）:
///     输出 `{id,name,kind}`，kind 为 local/remote。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionProjectRef {
    pub id: String,
    pub name: String,
    pub kind: AttentionProjectKind,
}

/// 项目 kind 字面量。
///
/// Business Logic（为什么需要这个枚举）:
///     前端区分本机项目与远端快捷方式，决定导航与标签。
///
/// Code Logic（这个枚举做什么）:
///     序列化为 `'local' | 'remote'`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttentionProjectKind {
    Local,
    Remote,
}

/// Attention 条目所属设备摘要。
///
/// Business Logic（为什么需要这个结构体）:
///     远端任务/outbox 需要展示 owning device，但不暴露连接 URL。
///
/// Code Logic（这个结构体做什么）:
///     输出 `{id,name}`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionDeviceRef {
    pub id: String,
    pub name: String,
}

/// 语义化跳转目标。
///
/// Business Logic（为什么需要这个枚举）:
///     后端只返回语义 target，由桌面/移动端各自映射导航；禁止返回后端 URL。
///
/// Code Logic（这个枚举做什么）:
///     用内部 tag `kind` 序列化为三种目标：orchestratorTask / remoteOutbox / settings。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AttentionTargetDto {
    OrchestratorTask {
        #[serde(rename = "projectId")]
        project_id: String,
        #[serde(rename = "taskId")]
        task_id: String,
    },
    RemoteOutbox {
        #[serde(rename = "projectId")]
        project_id: String,
        #[serde(rename = "outboxId")]
        outbox_id: String,
    },
    Settings {
        tab: AttentionSettingsTab,
    },
}

/// Settings target 的 tab 字面量。
///
/// Business Logic（为什么需要这个枚举）:
///     环境依赖条目固定跳到 dependencies tab，避免自由字符串漂移。
///
/// Code Logic（这个枚举做什么）:
///     序列化为 `'dependencies'`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttentionSettingsTab {
    Dependencies,
}

/// 单条 Attention 条目 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     Inbox 列表、badge 与 Provider 都以条目为最小展示单元，字段契约必须跨端一致。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 输出完整条目，含可空 project/device/cachedAt。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionItemDto {
    pub id: String,
    pub category: AttentionCategory,
    pub source_kind: AttentionSourceKind,
    pub title: String,
    pub summary: String,
    pub updated_at: String,
    pub freshness: AttentionFreshness,
    pub cached_at: Option<String>,
    pub project: Option<AttentionProjectRef>,
    pub device: Option<AttentionDeviceRef>,
    pub target: AttentionTargetDto,
}

/// 分类计数。
///
/// Business Logic（为什么需要这个结构体）:
///     前端 badge 与分组空态依赖 total/decision/blocked/environment 一致性。
///
/// Code Logic（这个结构体做什么）:
///     输出四个计数字段，聚合器保证 total 等于三类之和且等于 items 长度。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionCountsDto {
    pub total: u32,
    pub decision: u32,
    pub blocked: u32,
    pub environment: u32,
}

/// Attention 快照 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     一次聚合成功后才产出完整快照，失败不得返回部分列表。
///
/// Code Logic（这个结构体做什么）:
///     输出 generatedAt + counts + items。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionSnapshotDto {
    pub generated_at: String,
    pub counts: AttentionCountsDto,
    pub items: Vec<AttentionItemDto>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// Business Logic: 构造一条覆盖全部字段的样例条目，供序列化契约测试复用。
    /// Code Logic: 返回含 project/device/cachedAt 的 decision 条目。
    fn sample_item() -> AttentionItemDto {
        AttentionItemDto {
            id: "orchestrator:human-review:task-1".to_string(),
            category: AttentionCategory::Decision,
            source_kind: AttentionSourceKind::OrchestratorHumanReview,
            title: "等待人工复核".to_string(),
            summary: "任务已完成，等待交付确认".to_string(),
            updated_at: "2026-07-11T10:00:00Z".to_string(),
            freshness: AttentionFreshness::Cached,
            cached_at: Some("2026-07-11T09:59:00Z".to_string()),
            project: Some(AttentionProjectRef {
                id: "proj-1".to_string(),
                name: "demo".to_string(),
                kind: AttentionProjectKind::Remote,
            }),
            device: Some(AttentionDeviceRef {
                id: "dev-1".to_string(),
                name: "Mac Mini".to_string(),
            }),
            target: AttentionTargetDto::OrchestratorTask {
                project_id: "proj-1".to_string(),
                task_id: "remote:dev-1:task-1".to_string(),
            },
        }
    }

    /// Business Logic: 递归确认 JSON 值中不出现后端 URL 字段或 URL 形态字符串。
    /// Code Logic: 拒绝 key 含 url/baseUrl/href 的字段，并拒绝 http(s) 字符串叶子。
    fn assert_no_backend_urls(value: &Value) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    let lower = key.to_ascii_lowercase();
                    assert!(
                        !lower.contains("url") && !lower.contains("href") && lower != "base",
                        "DTO 不得包含后端 URL 字段: {key}"
                    );
                    assert_no_backend_urls(child);
                }
            }
            Value::Array(items) => {
                for child in items {
                    assert_no_backend_urls(child);
                }
            }
            Value::String(text) => {
                let lower = text.to_ascii_lowercase();
                assert!(
                    !lower.starts_with("http://") && !lower.starts_with("https://"),
                    "DTO 不得包含后端 URL 字符串: {text}"
                );
            }
            _ => {}
        }
    }

    #[test]
    fn enum_literals_match_typescript_contract() {
        assert_eq!(
            serde_json::to_value(AttentionCategory::Decision).unwrap(),
            json!("decision")
        );
        assert_eq!(
            serde_json::to_value(AttentionCategory::Blocked).unwrap(),
            json!("blocked")
        );
        assert_eq!(
            serde_json::to_value(AttentionCategory::Environment).unwrap(),
            json!("environment")
        );

        assert_eq!(
            serde_json::to_value(AttentionFreshness::Live).unwrap(),
            json!("live")
        );
        assert_eq!(
            serde_json::to_value(AttentionFreshness::Cached).unwrap(),
            json!("cached")
        );

        assert_eq!(
            serde_json::to_value(AttentionSourceKind::OrchestratorHumanReview).unwrap(),
            json!("orchestratorHumanReview")
        );
        assert_eq!(
            serde_json::to_value(AttentionSourceKind::OrchestratorBlocked).unwrap(),
            json!("orchestratorBlocked")
        );
        assert_eq!(
            serde_json::to_value(AttentionSourceKind::RemoteOutboxFailed).unwrap(),
            json!("remoteOutboxFailed")
        );
        assert_eq!(
            serde_json::to_value(AttentionSourceKind::WorkbenchDependency).unwrap(),
            json!("workbenchDependency")
        );

        assert_eq!(
            serde_json::to_value(AttentionProjectKind::Local).unwrap(),
            json!("local")
        );
        assert_eq!(
            serde_json::to_value(AttentionProjectKind::Remote).unwrap(),
            json!("remote")
        );

        assert_eq!(
            serde_json::to_value(AttentionSettingsTab::Dependencies).unwrap(),
            json!("dependencies")
        );
    }

    #[test]
    fn target_and_item_fields_are_camel_case() {
        let task_target = serde_json::to_value(AttentionTargetDto::OrchestratorTask {
            project_id: "p1".to_string(),
            task_id: "t1".to_string(),
        })
        .unwrap();
        assert_eq!(
            task_target,
            json!({
                "kind": "orchestratorTask",
                "projectId": "p1",
                "taskId": "t1",
            })
        );

        let outbox_target = serde_json::to_value(AttentionTargetDto::RemoteOutbox {
            project_id: "p2".to_string(),
            outbox_id: "o1".to_string(),
        })
        .unwrap();
        assert_eq!(
            outbox_target,
            json!({
                "kind": "remoteOutbox",
                "projectId": "p2",
                "outboxId": "o1",
            })
        );

        let settings_target = serde_json::to_value(AttentionTargetDto::Settings {
            tab: AttentionSettingsTab::Dependencies,
        })
        .unwrap();
        assert_eq!(
            settings_target,
            json!({
                "kind": "settings",
                "tab": "dependencies",
            })
        );

        let item = serde_json::to_value(sample_item()).unwrap();
        assert!(item.get("sourceKind").is_some());
        assert!(item.get("updatedAt").is_some());
        assert!(item.get("cachedAt").is_some());
        assert!(item.get("source_kind").is_none());
        assert!(item.get("updated_at").is_none());
        assert!(item.get("cached_at").is_none());

        let snapshot = AttentionSnapshotDto {
            generated_at: "2026-07-11T10:01:00Z".to_string(),
            counts: AttentionCountsDto {
                total: 1,
                decision: 1,
                blocked: 0,
                environment: 0,
            },
            items: vec![sample_item()],
        };
        let snapshot_json = serde_json::to_value(snapshot).unwrap();
        assert!(snapshot_json.get("generatedAt").is_some());
        assert!(snapshot_json.get("generated_at").is_none());
        assert_eq!(snapshot_json["counts"]["total"], 1);
    }

    #[test]
    fn cached_at_is_nullable_and_dto_has_no_backend_urls() {
        let mut item = sample_item();
        item.cached_at = None;
        item.freshness = AttentionFreshness::Live;
        let json = serde_json::to_value(&item).unwrap();
        assert!(json.get("cachedAt").unwrap().is_null());
        assert_no_backend_urls(&json);

        item.cached_at = Some("2026-07-11T09:00:00Z".to_string());
        item.freshness = AttentionFreshness::Cached;
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["cachedAt"], "2026-07-11T09:00:00Z");
        assert_no_backend_urls(&json);

        let snapshot = AttentionSnapshotDto {
            generated_at: "2026-07-11T10:01:00Z".to_string(),
            counts: AttentionCountsDto {
                total: 1,
                decision: 1,
                blocked: 0,
                environment: 0,
            },
            items: vec![item],
        };
        assert_no_backend_urls(&serde_json::to_value(snapshot).unwrap());
    }
}
