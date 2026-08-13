//! window_registry — GUI 进程内工作台窗口 occupancy。
//!
//! Business Logic（为什么需要这个模块）:
//!     第一期禁止同一项目同时出现在两个 OS 窗；再开必须聚焦已有窗。
//!     occupancy 只活在 GUI 进程内存，不进 SQLite / sidecar / LAN。
//!
//! Code Logic（这个模块做什么）:
//!     维护 projectId↔windowLabel 双向表，分配空闲 `workbench-1..4`，并提供 claim/release/snapshot。

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::AppError;
use crate::workbench::workspace_layout::{
    parse_satellite_window_slot, MAIN_WINDOW_LABEL, MAX_WORKBENCH_SATELLITE_WINDOWS,
    WORKBENCH_WINDOW_LABEL_PREFIX,
};

/// 打开卫星窗时的分配结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowOpenDecision {
    /// 项目已被某活窗占用，应 show+focus。
    FocusExisting { label: String },
    /// 分配了空闲卫星 slot，调用方负责建窗。
    Create { label: String },
}

/// claim 结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimResult {
    /// 本窗新占用该项目。
    Claimed,
    /// 本窗已占用同一项目。
    Unchanged,
    /// 项目已被另一窗占用。
    OccupiedByOther { label: String },
}

/// occupancy 快照行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkbenchWindowOccupancy {
    /// 占用的项目。
    pub project_id: String,
    /// 占用窗口 label。
    pub window_label: String,
}

#[derive(Default)]
struct Inner {
    by_project: HashMap<String, String>,
    by_label: HashMap<String, String>,
}

/// GUI 工作台窗口占用表。
pub struct WorkbenchWindowRegistry {
    inner: Mutex<Inner>,
}

impl Default for WorkbenchWindowRegistry {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
        }
    }
}

impl WorkbenchWindowRegistry {
    /// Business Logic（为什么需要这个函数）:
    ///     「在新窗口打开」必须先看项目是否已被占用，避免双开。
    ///
    /// Code Logic（这个函数做什么）:
    ///     已占用 → FocusExisting；否则分配 `workbench-1..4` 并立即 claim；满员 Validation。
    pub fn focus_or_allocate(&self, project_id: &str) -> Result<WindowOpenDecision, AppError> {
        let project_id = normalize_project_id(project_id)?;
        let mut inner = lock_inner(&self.inner)?;
        if let Some(label) = inner.by_project.get(&project_id).cloned() {
            return Ok(WindowOpenDecision::FocusExisting { label });
        }
        let label = allocate_satellite_label(&inner)?;
        bind(&mut inner, &label, &project_id);
        Ok(WindowOpenDecision::Create { label })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     主窗切项目与卫星窗加载都必须登记占用，冲突时改聚焦已有窗。
    ///
    /// Code Logic（这个函数做什么）:
    ///     同窗同项目 Unchanged；他窗占用 OccupiedByOther；否则释放本窗旧项目再绑定。
    pub fn claim(&self, label: &str, project_id: &str) -> Result<ClaimResult, AppError> {
        let project_id = normalize_project_id(project_id)?;
        validate_workbench_window_label(label)?;
        let mut inner = lock_inner(&self.inner)?;
        if let Some(owner) = inner.by_project.get(&project_id) {
            if owner == label {
                return Ok(ClaimResult::Unchanged);
            }
            return Ok(ClaimResult::OccupiedByOther {
                label: owner.clone(),
            });
        }
        unbind_label(&mut inner, label);
        bind(&mut inner, label, &project_id);
        Ok(ClaimResult::Claimed)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     关卫星窗或主窗清空选中后必须释放占用，否则项目会永久锁死。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 label 拆除双向映射；未知 label 幂等。
    pub fn release_label(&self, label: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            unbind_label(&mut inner, label);
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Rail / Attention 需要知道每个项目现在在哪扇窗。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 windowLabel 字典序返回快照。
    pub fn snapshot(&self) -> Vec<WorkbenchWindowOccupancy> {
        let Ok(inner) = self.inner.lock() else {
            return Vec::new();
        };
        let mut rows: Vec<WorkbenchWindowOccupancy> = inner
            .by_label
            .iter()
            .map(|(window_label, project_id)| WorkbenchWindowOccupancy {
                project_id: project_id.clone(),
                window_label: window_label.clone(),
            })
            .collect();
        rows.sort_by(|a, b| a.window_label.cmp(&b.window_label));
        rows
    }
}

/// Business Logic（为什么需要这个函数）:
///     occupancy 只服务主窗与 4 个卫星窗，overlay 不得 claim。
///
/// Code Logic（这个函数做什么）:
///     接受 `main` 或 `workbench-1..4`。
pub fn validate_workbench_window_label(label: &str) -> Result<(), AppError> {
    if label == MAIN_WINDOW_LABEL || parse_satellite_window_slot(label).is_some() {
        return Ok(());
    }
    Err(AppError::validation(format!(
        "workbench_window_invalid_label:{label}"
    )))
}

fn normalize_project_id(project_id: &str) -> Result<String, AppError> {
    let trimmed = project_id.trim();
    if trimmed.is_empty() {
        return Err(AppError::validation(
            "workbench_window_project_required".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

fn lock_inner(inner: &Mutex<Inner>) -> Result<std::sync::MutexGuard<'_, Inner>, AppError> {
    inner
        .lock()
        .map_err(|_| AppError::generic("workbench_window_registry_lock_poisoned"))
}

fn allocate_satellite_label(inner: &Inner) -> Result<String, AppError> {
    for slot in 1..=MAX_WORKBENCH_SATELLITE_WINDOWS {
        let label = format!("{WORKBENCH_WINDOW_LABEL_PREFIX}{slot}");
        if !inner.by_label.contains_key(&label) {
            return Ok(label);
        }
    }
    Err(AppError::validation("workbench_window_limit".to_string()))
}

fn bind(inner: &mut Inner, label: &str, project_id: &str) {
    if let Some(previous) = inner.by_label.insert(label.to_string(), project_id.to_string()) {
        inner.by_project.remove(&previous);
    }
    inner.by_project.insert(project_id.to_string(), label.to_string());
}

fn unbind_label(inner: &mut Inner, label: &str) {
    if let Some(project_id) = inner.by_label.remove(label) {
        if inner.by_project.get(&project_id).map(String::as_str) == Some(label) {
            inner.by_project.remove(&project_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_same_project_twice_focuses_existing() {
        let registry = WorkbenchWindowRegistry::default();
        let first = registry.focus_or_allocate("p1").unwrap();
        assert_eq!(
            first,
            WindowOpenDecision::Create {
                label: "workbench-1".into()
            }
        );
        let second = registry.focus_or_allocate("p1").unwrap();
        assert_eq!(
            second,
            WindowOpenDecision::FocusExisting {
                label: "workbench-1".into()
            }
        );
    }

    #[test]
    fn fifth_satellite_hits_window_limit() {
        let registry = WorkbenchWindowRegistry::default();
        for i in 1..=4 {
            let decision = registry.focus_or_allocate(&format!("p{i}")).unwrap();
            assert_eq!(
                decision,
                WindowOpenDecision::Create {
                    label: format!("workbench-{i}")
                }
            );
        }
        let err = registry.focus_or_allocate("p5").unwrap_err();
        assert_eq!(err.code(), "workbench_window_limit");
    }

    #[test]
    fn release_allows_reallocating_slot() {
        let registry = WorkbenchWindowRegistry::default();
        registry.focus_or_allocate("p1").unwrap();
        registry.release_label("workbench-1");
        let again = registry.focus_or_allocate("p2").unwrap();
        assert_eq!(
            again,
            WindowOpenDecision::Create {
                label: "workbench-1".into()
            }
        );
    }

    #[test]
    fn claim_detects_other_window_and_main_can_claim() {
        let registry = WorkbenchWindowRegistry::default();
        assert_eq!(registry.claim("main", "alpha").unwrap(), ClaimResult::Claimed);
        assert_eq!(
            registry.claim("main", "alpha").unwrap(),
            ClaimResult::Unchanged
        );
        match registry.claim("workbench-1", "alpha").unwrap() {
            ClaimResult::OccupiedByOther { label } => assert_eq!(label, "main"),
            other => panic!("expected occupied, got {other:?}"),
        }
        assert_eq!(registry.claim("main", "beta").unwrap(), ClaimResult::Claimed);
        let snap = registry.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].project_id, "beta");
        assert_eq!(snap[0].window_label, "main");
    }

    #[test]
    fn empty_project_and_overlay_label_are_rejected() {
        let registry = WorkbenchWindowRegistry::default();
        assert_eq!(
            registry.claim("main", "  ").unwrap_err().code(),
            "workbench_window_project_required"
        );
        assert!(registry
            .claim("screenshot-overlay-0", "p1")
            .unwrap_err()
            .code()
            .contains("workbench_window_invalid_label"));
    }
}
