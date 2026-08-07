//! workbench/project_order.rs — 项目列表自定义排序纯函数
//!
//! Business Logic（为什么需要这个模块）:
//!     用户拖拽侧栏项目顺序后，顺序应跨重启与跨设备保持；项目实体本身不跨设备共享，
//!     因此顺序是一份独立的 `projectId[]` 偏好，按本地存在的 id 投影展示。
//!
//! Code Logic（这个模块做什么）:
//!     - `apply_project_order`：ordered_ids 中本地存在的 id 按序在前，未入表本地项目按默认顺序接在顶部（新项目优先可见）。
//!     - `prepend_project_id` / `remove_project_id`：添加/删除时维护顺序列表。
//!     - `order_document_wins`：整表 LWW（updated_at 新者胜，相等时 device_id 字典序）。

use std::collections::HashSet;

/// 跨设备同步的项目顺序列表文档（单例偏好）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectOrderDocument {
    /// 全局 projectId 顺序（可含本机不存在的 id，便于跨设备共享相对序）。
    pub ordered_ids: Vec<String>,
    /// 文档最后写入时间（RFC3339），LWW 主键。
    pub updated_at: String,
    /// 最后写入设备 id，LWW tie-break。
    pub device_id: String,
}

/**
 * Business Logic（为什么需要这个函数）:
 *   list 返回的项目必须同时尊重用户拖拽顺序与「未入顺序表的新项目置顶」。
 *
 * Code Logic（这个函数做什么）:
 *   1) 本地项目按 id 建索引；
 *   2) ordered_ids 中存在于本地的 id 按序收集（去重）；
 *   3) 未出现在 ordered_ids 的本地项目按 `default_ids` 顺序接在**前面**；
 *   4) 按收集的 id 序列从索引取回项目。
 */
pub fn apply_project_order<T, F>(
    projects: Vec<T>,
    ordered_ids: &[String],
    project_id: F,
) -> Vec<T>
where
    F: Fn(&T) -> &str,
{
    if projects.is_empty() {
        return projects;
    }
    if ordered_ids.is_empty() {
        return projects;
    }

    let mut by_id: std::collections::HashMap<String, T> = std::collections::HashMap::with_capacity(projects.len());
    let mut default_ids: Vec<String> = Vec::with_capacity(projects.len());
    for project in projects {
        let id = project_id(&project).to_string();
        default_ids.push(id.clone());
        by_id.insert(id, project);
    }

    let mut seen: HashSet<String> = HashSet::with_capacity(default_ids.len());
    let mut ordered_present: Vec<String> = Vec::with_capacity(default_ids.len());
    for id in ordered_ids {
        if by_id.contains_key(id) && seen.insert(id.clone()) {
            ordered_present.push(id.clone());
        }
    }

    let mut missing: Vec<String> = Vec::new();
    for id in &default_ids {
        if !seen.contains(id) {
            missing.push(id.clone());
        }
    }

    // 未入表本地项目置顶，保持其 default 相对顺序（created_at DESC）。
    let mut final_ids = missing;
    final_ids.extend(ordered_present);

    let mut out = Vec::with_capacity(final_ids.len());
    for id in final_ids {
        if let Some(project) = by_id.remove(&id) {
            out.push(project);
        }
    }
    out
}

/**
 * Business Logic（为什么需要这个函数）:
 *   新添加的项目应出现在列表顶部，且写入共享顺序列表，便于跨设备对齐。
 *
 * Code Logic（这个函数做什么）:
 *   若 id 已在列表则先移除，再插入到开头；其余相对顺序不变。
 */
pub fn prepend_project_id(ordered_ids: &[String], project_id: &str) -> Vec<String> {
    let mut next: Vec<String> = Vec::with_capacity(ordered_ids.len() + 1);
    next.push(project_id.to_string());
    for id in ordered_ids {
        if id != project_id {
            next.push(id.clone());
        }
    }
    next
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移除项目后顺序文档不应继续保留已删 id（可减少文档膨胀；跨设备仍可带未知 id）。
 *
 * Code Logic（这个函数做什么）:
 *   过滤掉匹配 project_id 的条目。
 */
pub fn remove_project_id(ordered_ids: &[String], project_id: &str) -> Vec<String> {
    ordered_ids
        .iter()
        .filter(|id| id.as_str() != project_id)
        .cloned()
        .collect()
}

/**
 * Business Logic（为什么需要这个函数）:
 *   两台设备并发改排序时整表 LWW：后写覆盖，时间戳相等用 device_id 字典序。
 *
 * Code Logic（这个函数做什么）:
 *   返回 true 表示 remote 胜出应覆盖 local。
 */
pub fn order_document_wins(local: &ProjectOrderDocument, remote: &ProjectOrderDocument) -> bool {
    if remote.updated_at > local.updated_at {
        return true;
    }
    if remote.updated_at < local.updated_at {
        return false;
    }
    remote.device_id > local.device_id
}

/**
 * Business Logic（为什么需要这个函数）:
 *   reorder API 需要去掉空白/重复 id，避免脏数据污染顺序文档。
 *
 * Code Logic（这个函数做什么）:
 *   trim 非空 id，首次出现保留，后续重复丢弃。
 */
pub fn normalize_ordered_ids(ids: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for id in ids {
        let trimmed = id.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.clone()) {
            out.push(trimmed);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct P {
        id: String,
    }

    fn p(id: &str) -> P {
        P {
            id: id.to_string(),
        }
    }

    #[test]
    fn apply_order_puts_missing_local_projects_on_top() {
        let projects = vec![p("a"), p("b"), p("c")];
        // default list is a,b,c (already sorted by caller)
        let ordered = vec!["b".to_string(), "a".to_string()];
        let out = apply_project_order(projects, &ordered, |x| &x.id);
        assert_eq!(
            out.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
            vec!["c", "b", "a"]
        );
    }

    #[test]
    fn apply_order_ignores_unknown_remote_ids() {
        let projects = vec![p("a"), p("b")];
        let ordered = vec![
            "x".to_string(),
            "b".to_string(),
            "a".to_string(),
            "y".to_string(),
        ];
        let out = apply_project_order(projects, &ordered, |x| &x.id);
        assert_eq!(
            out.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
            vec!["b", "a"]
        );
    }

    #[test]
    fn empty_order_keeps_default() {
        let projects = vec![p("a"), p("b")];
        let out = apply_project_order(projects.clone(), &[], |x| &x.id);
        assert_eq!(out, projects);
    }

    #[test]
    fn prepend_moves_existing_to_top() {
        let ordered = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(
            prepend_project_id(&ordered, "b"),
            vec!["b".to_string(), "a".to_string(), "c".to_string()]
        );
        assert_eq!(
            prepend_project_id(&ordered, "d"),
            vec![
                "d".to_string(),
                "a".to_string(),
                "b".to_string(),
                "c".to_string()
            ]
        );
    }

    #[test]
    fn lww_prefers_newer_updated_at() {
        let local = ProjectOrderDocument {
            ordered_ids: vec!["a".into()],
            updated_at: "2026-01-01T00:00:00Z".into(),
            device_id: "z".into(),
        };
        let remote = ProjectOrderDocument {
            ordered_ids: vec!["b".into()],
            updated_at: "2026-01-02T00:00:00Z".into(),
            device_id: "a".into(),
        };
        assert!(order_document_wins(&local, &remote));
        assert!(!order_document_wins(&remote, &local));
    }

    #[test]
    fn lww_tie_break_device_id() {
        let local = ProjectOrderDocument {
            ordered_ids: vec!["a".into()],
            updated_at: "2026-01-01T00:00:00Z".into(),
            device_id: "device-a".into(),
        };
        let remote = ProjectOrderDocument {
            ordered_ids: vec!["b".into()],
            updated_at: "2026-01-01T00:00:00Z".into(),
            device_id: "device-b".into(),
        };
        assert!(order_document_wins(&local, &remote));
        assert!(!order_document_wins(&remote, &local));
    }

    #[test]
    fn normalize_dedupes_and_trims() {
        assert_eq!(
            normalize_ordered_ids([
                " a ".to_string(),
                "b".to_string(),
                "a".to_string(),
                "".to_string(),
                "  ".to_string(),
                "c".to_string()
            ]),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }
}
