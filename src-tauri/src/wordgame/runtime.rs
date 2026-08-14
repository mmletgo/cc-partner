//! 记单词后台 ingest / preheat。
//!
//! Business Logic（为什么需要这个模块）:
//!     启动后要补扫历史并预热最高频 10 个生词；开玩后保持缓存超前 10 个。
//!     某词生成失败必须堵住队列直到成功。
//!
//! Code Logic（这个模块做什么）:
//!     用模块级 cancel token 避免给 AppState 加字段；周期扫描本机 + 远端 extract-delta，
//!     再按堵塞策略生成题目。

use super::generate::generate_card_for_lemma;
use super::ingest::extract_local_delta;
use super::models::{IngestCursor, LemmaCount, PreheatStatus};
use crate::error::AppError;
use crate::net::protocol::CAPABILITY_WORKBENCH_WORDGAME_EXTRACT_V1;
use crate::state::AppState;
use crate::storage::wordgame_repo::WordGameRepo;
use crate::workbench::remote_client::RemoteWorkbenchClient;
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// 供 shutdown 取消的运行时令牌。
pub static WORDGAME_RUNTIME_CANCEL: OnceLock<Mutex<Option<CancellationToken>>> = OnceLock::new();

const INGEST_INTERVAL: Duration = Duration::from_secs(45);
const PREHEAT_IDLE: Duration = Duration::from_secs(8);
const PREHEAT_RETRY: Duration = Duration::from_secs(20);

/// 启动 ingest + preheat。重复调用会先取消旧任务。
pub fn start_wordgame_runtime(state: AppState) -> CancellationToken {
    let cancel = CancellationToken::new();
    let slot = WORDGAME_RUNTIME_CANCEL.get_or_init(|| Mutex::new(None));
    if let Some(old) = slot.lock().unwrap().replace(cancel.clone()) {
        old.cancel();
    }
    let ingest_state = state.clone();
    let preheat_state = state;
    let ingest_cancel = cancel.clone();
    let preheat_cancel = cancel.clone();
    tauri::async_runtime::spawn(async move {
        ingest_loop(ingest_state, ingest_cancel).await;
    });
    tauri::async_runtime::spawn(async move {
        preheat_loop(preheat_state, preheat_cancel).await;
    });
    cancel
}

/// 退出时取消 ingest / preheat。
pub fn cancel_wordgame_runtime() {
    if let Some(slot) = WORDGAME_RUNTIME_CANCEL.get() {
        if let Some(token) = slot.lock().unwrap().take() {
            token.cancel();
            tracing::info!("Wordgame runtime 已取消");
        }
    }
}

fn repo_of(state: &AppState) -> WordGameRepo {
    WordGameRepo::with_gate(state.db.clone(), state.maintenance_gate.clone())
}

async fn ingest_loop(state: AppState, cancel: CancellationToken) {
    loop {
        if let Err(err) = ingest_once(&state).await {
            tracing::warn!("wordgame ingest 失败: {err}");
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(INGEST_INTERVAL) => {}
        }
    }
}

/// 跑一轮本机 + 远端增量。
pub async fn ingest_once(state: &AppState) -> Result<(), AppError> {
    let repo = repo_of(state);
    let known = repo.list_cursors().await.unwrap_or_default();
    let today = super::schedule::local_today();
    let now = chrono::Utc::now().to_rfc3339();

    let local = extract_local_delta(state.device_id.as_str(), &known).await?;
    apply_delta(&repo, &today, &now, &local.lemmas, &local.cursors).await?;

    if let Err(err) = ingest_remote_shortcuts(state, &repo, &known, &today, &now).await {
        tracing::warn!("wordgame 远端抽取失败: {err}");
        if let Ok(current) = repo.get_preheat().await {
            if current.status != "blocked" {
                let _ = repo
                    .upsert_preheat(
                        &PreheatStatus {
                            head_lemma: current.head_lemma,
                            status: current.status,
                            error: Some(format!("远端未计入词频: {err}")),
                            retry_count: current.retry_count,
                        },
                        &now,
                    )
                    .await;
            }
        }
    }
    Ok(())
}

async fn apply_delta(
    repo: &WordGameRepo,
    today: &str,
    now: &str,
    lemmas: &[LemmaCount],
    cursors: &[IngestCursor],
) -> Result<(), AppError> {
    let counts: Vec<(String, i64)> = lemmas
        .iter()
        .map(|item| (item.lemma.clone(), item.count))
        .collect();
    repo.bump_counts(today, &counts).await?;
    for cursor in cursors {
        repo.upsert_cursor(cursor, now).await?;
    }
    Ok(())
}

async fn ingest_remote_shortcuts(
    state: &AppState,
    repo: &WordGameRepo,
    known: &[IngestCursor],
    today: &str,
    now: &str,
) -> Result<(), AppError> {
    let projects = state
        .workbench_project_repo
        .list()
        .await
        .unwrap_or_default();
    let mut seen_devices = std::collections::BTreeSet::new();
    for project in projects {
        if project.kind != "remote" {
            continue;
        }
        if project.device_id.is_empty() {
            continue;
        }
        let device_id = project.device_id.clone();
        if !seen_devices.insert(device_id.clone()) {
            continue;
        }
        let Ok(base_url) = crate::commands::workbench::device_base_url(state, &device_id) else {
            continue;
        };
        if state
            .peer_client
            .require_capability(&base_url, CAPABILITY_WORKBENCH_WORDGAME_EXTRACT_V1)
            .await
            .is_err()
        {
            continue;
        }
        let cursors: Vec<IngestCursor> = known
            .iter()
            .filter(|c| c.device_id == device_id)
            .cloned()
            .collect();
        let client = RemoteWorkbenchClient::new().with_expected_device_id(&device_id);
        let resp = client
            .extract_wordgame_delta(&base_url, ExtractDeltaReq { cursors })
            .await?;
        apply_delta(repo, today, now, &resp.lemmas, &resp.cursors).await?;
    }
    Ok(())
}

async fn preheat_loop(state: AppState, cancel: CancellationToken) {
    loop {
        match preheat_once(&state).await {
            Ok(PreheatTick::Idle) => {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(PREHEAT_IDLE) => {}
                }
            }
            Ok(PreheatTick::Blocked) => {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(PREHEAT_RETRY) => {}
                }
            }
            Ok(PreheatTick::Progressed) => {
                if cancel.is_cancelled() {
                    break;
                }
            }
            Err(err) => {
                tracing::warn!("wordgame preheat 失败: {err}");
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(PREHEAT_RETRY) => {}
                }
            }
        }
    }
}

enum PreheatTick {
    Idle,
    Progressed,
    Blocked,
}

async fn preheat_once(state: &AppState) -> Result<PreheatTick, AppError> {
    let repo = repo_of(state);
    let now = chrono::Utc::now().to_rfc3339();
    let cached = repo.count_cached_unfamiliar().await?;
    let ahead_needed = super::models::PREHEAT_AHEAD as i64;
    if cached >= ahead_needed {
        if let Some(next) = repo.next_uncached_unfamiliar().await? {
            // 仍超前生成，但已满足开局门槛时可稍慢。
            let _ = next;
        } else {
            repo.upsert_preheat(
                &PreheatStatus {
                    head_lemma: None,
                    status: "ready".into(),
                    error: None,
                    retry_count: 0,
                },
                &now,
            )
            .await?;
            return Ok(PreheatTick::Idle);
        }
    }
    let Some(lemma) = repo.next_uncached_unfamiliar().await? else {
        repo.upsert_preheat(
            &PreheatStatus {
                head_lemma: None,
                status: "waiting_for_words".into(),
                error: None,
                retry_count: 0,
            },
            &now,
        )
        .await?;
        return Ok(PreheatTick::Idle);
    };
    let previous = repo.get_preheat().await?;
    let retry_count = if previous.head_lemma.as_deref() == Some(lemma.as_str()) {
        previous.retry_count
    } else {
        0
    };
    repo.upsert_preheat(
        &PreheatStatus {
            head_lemma: Some(lemma.clone()),
            status: "generating".into(),
            error: None,
            retry_count,
        },
        &now,
    )
    .await?;
    match generate_card_for_lemma(state, &lemma).await {
        Ok(card) => {
            repo.save_card(&card, &now).await?;
            repo.upsert_preheat(
                &PreheatStatus {
                    head_lemma: Some(lemma),
                    status: "generating".into(),
                    error: None,
                    retry_count: 0,
                },
                &now,
            )
            .await?;
            Ok(PreheatTick::Progressed)
        }
        Err(err) => {
            repo.upsert_preheat(
                &PreheatStatus {
                    head_lemma: Some(lemma),
                    status: "blocked".into(),
                    error: Some(err.to_string()),
                    retry_count: retry_count + 1,
                },
                &now,
            )
            .await?;
            Ok(PreheatTick::Blocked)
        }
    }
}

/// 用户点「重试预热」。
pub async fn retry_preheat(state: &AppState) -> Result<PreheatStatus, AppError> {
    let _ = preheat_once(state).await?;
    repo_of(state).get_preheat().await
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractDeltaReq {
    #[serde(default)]
    pub cursors: Vec<IngestCursor>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractDeltaResp {
    pub lemmas: Vec<LemmaCount>,
    pub cursors: Vec<IngestCursor>,
}

/// owner-local 抽取入口，供 P2P 路由复用。
pub async fn extract_delta_for_state(
    state: &AppState,
    req: ExtractDeltaReq,
) -> Result<ExtractDeltaResp, AppError> {
    let delta = extract_local_delta(state.device_id.as_str(), &req.cursors).await?;
    Ok(ExtractDeltaResp {
        lemmas: delta.lemmas,
        cursors: delta.cursors,
    })
}
