//! commands/wordgame.rs — 记单词大厅与闪卡命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     前端 GameHub / 闪卡只通过 invoke 读写本机词库；判题必须在后端完成。
//!
//! Code Logic（这个模块做什么）:
//!     封装 hub 状态、预热重试、开局抽题与提交答案；DTO camelCase。

use crate::error::AppError;
use crate::state::AppState;
use crate::storage::wordgame_repo::WordGameRepo;
use crate::wordgame::models::{QuestionKind, QuestionType, QuizItem, REQUIRED_CACHED_NEW_WORDS};
use crate::wordgame::runtime::retry_preheat;
use crate::wordgame::schedule::{
    TypeProgress, answers_match, apply_answer, local_today, pick_next_card,
};
use serde::{Deserialize, Serialize};
use tauri::State;

fn repo(state: &AppState) -> WordGameRepo {
    WordGameRepo::with_gate(state.db.clone(), state.maintenance_gate.clone())
}

/// 大厅状态。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WordgameHubStatusDto {
    pub unfamiliar_count: i64,
    pub cached_unfamiliar_count: i64,
    pub can_enter: bool,
    pub required_cached: i64,
    pub preheat_status: String,
    pub preheat_lemma: Option<String>,
    pub preheat_error: Option<String>,
    pub remote_hint: Option<String>,
}

/// 一张闪卡。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WordgameCardDto {
    pub lemma: String,
    pub question_type: QuestionType,
    pub kind: QuestionKind,
    pub prompt: String,
    pub options: Vec<String>,
}

/// 提交答案后的反馈。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WordgameSubmitResultDto {
    pub correct: bool,
    pub expected: String,
    pub familiar: bool,
    pub next: Option<WordgameCardDto>,
    pub done: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitWordgameAnswerReq {
    pub lemma: String,
    pub question_type: QuestionType,
    pub answer: String,
}

/// 读取大厅门槛与预热状态。
#[tauri::command]
pub async fn get_wordgame_hub_status(
    state: State<'_, AppState>,
) -> Result<WordgameHubStatusDto, AppError> {
    hub_status_for_state(&state).await
}

/// 重试当前卡住的预热词。
#[tauri::command]
pub async fn retry_wordgame_preheat(
    state: State<'_, AppState>,
) -> Result<WordgameHubStatusDto, AppError> {
    retry_preheat(&state).await?;
    hub_status_for_state(&state).await
}

/// 校验门槛后抽出第一张到期卡。
#[tauri::command]
pub async fn start_wordgame_round(state: State<'_, AppState>) -> Result<WordgameCardDto, AppError> {
    let status = hub_status_for_state(&state).await?;
    if !status.can_enter {
        return Err(AppError::validation("记单词尚未就绪：需要 10 个已缓存生词"));
    }
    next_card_for_state(&state, None)
        .await?
        .ok_or_else(|| AppError::generic("今天暂时没有到期的单词"))
}

/// 判题并返回下一张。
#[tauri::command]
pub async fn submit_wordgame_answer(
    state: State<'_, AppState>,
    req: SubmitWordgameAnswerReq,
) -> Result<WordgameSubmitResultDto, AppError> {
    let repo = repo(&state);
    let today = local_today();
    let lemma = repo
        .get_lemma(&req.lemma)
        .await?
        .ok_or_else(|| AppError::not_found("单词不存在"))?;
    let card = repo
        .get_card(&req.lemma)
        .await?
        .ok_or_else(|| AppError::generic("该词题目尚未生成"))?;
    let item = card
        .items
        .iter()
        .find(|item| item.question_type == req.question_type)
        .ok_or_else(|| AppError::generic("找不到对应题型"))?;
    let correct = answers_match(&item.answer, &req.answer);
    let all = repo.list_progress_for(&req.lemma).await?;
    let current = all
        .iter()
        .find(|p| p.question_type == req.question_type)
        .cloned()
        .unwrap_or(TypeProgress {
            lemma: req.lemma.clone(),
            question_type: req.question_type,
            correct_total: 0,
            correct_today: 0,
            last_correct_date: None,
        });
    let outcome = apply_answer(&lemma, &current, &all, &today, correct);
    repo.upsert_lemma(&outcome.lemma).await?;
    repo.upsert_progress(&outcome.progress).await?;
    let next = next_card_for_state(&state, Some((req.lemma.as_str(), req.question_type))).await?;
    Ok(WordgameSubmitResultDto {
        correct: outcome.correct,
        expected: item.answer.clone(),
        familiar: outcome.lemma.familiar,
        done: next.is_none(),
        next,
    })
}

/// 前端关闭游戏面；进度已在 submit 时落库。
#[tauri::command]
pub async fn abandon_wordgame_round() -> Result<(), AppError> {
    Ok(())
}

async fn hub_status_for_state(state: &AppState) -> Result<WordgameHubStatusDto, AppError> {
    let repo = repo(state);
    let unfamiliar_count = repo.count_unfamiliar().await?;
    let cached_unfamiliar_count = repo.count_cached_unfamiliar().await?;
    let preheat = repo.get_preheat().await?;
    let remote_hint = preheat
        .error
        .as_deref()
        .filter(|msg| msg.contains("远端"))
        .map(str::to_string);
    let blocked_error = if preheat.status == "blocked" {
        preheat.error.clone()
    } else {
        None
    };
    Ok(WordgameHubStatusDto {
        unfamiliar_count,
        cached_unfamiliar_count,
        can_enter: cached_unfamiliar_count >= REQUIRED_CACHED_NEW_WORDS as i64,
        required_cached: REQUIRED_CACHED_NEW_WORDS as i64,
        preheat_status: preheat.status,
        preheat_lemma: preheat.head_lemma,
        preheat_error: blocked_error.or(remote_hint.clone()),
        remote_hint,
    })
}

async fn next_card_for_state(
    state: &AppState,
    exclude: Option<(&str, QuestionType)>,
) -> Result<Option<WordgameCardDto>, AppError> {
    let repo = repo(state);
    let today = local_today();
    let lemmas = repo.list_lemmas().await?;
    let progresses = repo.list_all_progress().await?;
    let Some(due) = pick_next_card(&lemmas, &progresses, &today, exclude) else {
        return Ok(None);
    };
    let Some(payload) = repo.get_card(&due.lemma).await? else {
        return Ok(None);
    };
    let Some(item) = payload
        .items
        .into_iter()
        .find(|item| item.question_type == due.question_type)
    else {
        return Ok(None);
    };
    Ok(Some(to_card_dto(due.lemma, item)))
}

fn to_card_dto(lemma: String, item: QuizItem) -> WordgameCardDto {
    WordgameCardDto {
        lemma,
        question_type: item.question_type,
        kind: item.question_type.kind(),
        prompt: item.prompt,
        options: item.options,
    }
}
