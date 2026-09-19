//! attention/mobile_push_watch.rs — 未读 Attention 变化时经中转发推
//!
//! Business Logic: App 被杀也能收到待处理/Agent 等输入；不含 tmux 依赖。
//! Code Logic: 15s 轮询 v2 快照，对新未读 id 向已登记 token POST 中转。

use crate::attention::models::{AttentionSourceKind, AttentionTargetDto};
use crate::commands::attention::list_attention_items_v2_for_state;
use crate::state::AppState;
use crate::storage::mobile_push_repo::{
    maybe_queue_notify, send_notify, MobilePushPayload, MobilePushRepo,
};
use std::collections::HashSet;
use std::time::Duration;

/// 启动 Attention → 推送中转循环。
pub fn start_mobile_push_watch(state: AppState) {
    tauri::async_runtime::spawn(async move {
        let mut notified: HashSet<String> = HashSet::new();
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            if let Err(error) = tick(&state, &mut notified).await {
                tracing::debug!("mobile push watch: {error}");
            }
        }
    });
}

async fn tick(
    state: &AppState,
    notified: &mut HashSet<String>,
) -> Result<(), crate::error::AppError> {
    let snapshot = list_attention_items_v2_for_state(state).await?;
    let repo = MobilePushRepo::new(state.db.clone());
    let relay = repo.load_relay().await?;
    let registrations = repo.list_all().await?;
    if registrations.is_empty() {
        return Ok(());
    }
    let mut current_unread = HashSet::new();
    for item in &snapshot.items {
        if item.read_at.is_some() {
            continue;
        }
        if item.source_kind == AttentionSourceKind::WorkbenchDependency {
            continue;
        }
        current_unread.insert(item.id.clone());
        if !notified.insert(item.id.clone()) {
            continue;
        }
        let (project_id, session_id, worktree_id) = match &item.target {
            AttentionTargetDto::AgentSession {
                project_id,
                worktree_id,
                terminal_session_id,
                ..
            } => (
                Some(project_id.clone()),
                Some(terminal_session_id.clone()),
                worktree_id.clone(),
            ),
            AttentionTargetDto::OrchestratorTask { project_id, .. }
            | AttentionTargetDto::RemoteOutbox { project_id, .. }
            | AttentionTargetDto::Experiment { project_id, .. } => {
                (Some(project_id.clone()), None, None)
            }
            _ => (item.project.as_ref().map(|p| p.id.clone()), None, None),
        };
        let payload = MobilePushPayload {
            pc_device_id: state.device_id.as_str().to_string(),
            category: serde_json::to_value(item.source_kind)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_else(|| "attention".to_string()),
            title: "待处理".into(),
            body: item.title.clone(),
            project_id,
            session_id,
            worktree_id,
        };
        if maybe_queue_notify(Some(&relay), &payload)?
            != crate::storage::mobile_push_repo::MobilePushSendOutcome::WouldSend
        {
            continue;
        }
        for registration in &registrations {
            if let Err(error) = send_notify(&relay, registration, &payload).await {
                tracing::debug!(
                    "mobile push send failed device={} err={error}",
                    registration.mobile_device_id
                );
            }
        }
    }
    notified.retain(|id| current_unread.contains(id));
    Ok(())
}
