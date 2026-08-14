//! 用内部 Claude provider 为一词生成 7 种题。
//!
//! Business Logic（为什么需要这个模块）:
//!     闪卡题面必须由内部 Claude 生成，且与 Prompt 优化共用 CLI / provider。
//!
//! Code Logic（这个模块做什么）:
//!     只调用 `run_structured_json` + `resolve_internal_provider_config_dir`，
//!     校验 7 种题齐全后再返回。

use super::models::{CardPayload, QuestionType, QuizItem};
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use serde_json::json;

const GENERATE_TIMEOUT_SECS: u64 = 180;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeneratedCard {
    lemma: String,
    items: Vec<GeneratedItem>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeneratedItem {
    question_type: QuestionType,
    prompt: String,
    #[serde(default)]
    options: Vec<String>,
    answer: String,
}

/// 为一词生成并校验 7 题。
///
/// Business Logic:
///     预热队列堵在失败词上，所以这里必须 fail-closed：缺题、空答案、选择缺选项都失败。
pub async fn generate_card_for_lemma(
    state: &crate::state::AppState,
    lemma: &str,
) -> Result<CardPayload, AppError> {
    let lemma = lemma.trim();
    if lemma.is_empty() {
        return Err(AppError::validation("lemma 不能为空"));
    }
    let (cli_path, model, provider_id) = {
        let cfg = state.config.read().unwrap();
        (
            cfg.github_trending.claude_cli_path.clone(),
            cfg.github_trending.claude_model.clone(),
            cfg.internal_claude.provider_id.clone(),
        )
    };
    let provider_dir =
        crate::internal_claude::resolve_internal_provider_config_dir(provider_id.as_deref())
            .await?;
    let schema = card_schema();
    let prompt = build_prompt(lemma);
    let raw = crate::claude_cli::run_structured_json::<GeneratedCard>(
        &cli_path,
        &model,
        provider_dir.as_deref(),
        &schema.to_string(),
        &prompt,
        GENERATE_TIMEOUT_SECS,
        "生成记单词题目",
    )
    .await?;
    validate_generated(lemma, raw)
}

fn validate_generated(expected_lemma: &str, raw: GeneratedCard) -> Result<CardPayload, AppError> {
    if raw.items.len() != QuestionType::ALL.len() {
        return Err(AppError::generic("记单词题目数量不完整"));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut items = Vec::new();
    for item in raw.items {
        if !seen.insert(item.question_type) {
            return Err(AppError::generic("记单词题目类型重复"));
        }
        if item.prompt.trim().is_empty() || item.answer.trim().is_empty() {
            return Err(AppError::generic("记单词题目缺少题干或答案"));
        }
        if item.question_type.kind() == super::models::QuestionKind::Choice
            && item.options.len() < 2
        {
            return Err(AppError::generic("记单词选择题缺少选项"));
        }
        items.push(QuizItem {
            question_type: item.question_type,
            prompt: item.prompt.trim().to_string(),
            options: item
                .options
                .into_iter()
                .map(|s| s.trim().to_string())
                .collect(),
            answer: item.answer.trim().to_string(),
        });
    }
    if seen.len() != QuestionType::ALL.len() {
        return Err(AppError::generic("记单词题目类型不齐全"));
    }
    items.sort_by_key(|item| {
        QuestionType::ALL
            .iter()
            .position(|qt| *qt == item.question_type)
            .unwrap_or(99)
    });
    Ok(CardPayload {
        lemma: expected_lemma.to_string(),
        items,
    })
}

fn card_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["lemma", "items"],
        "properties": {
            "lemma": { "type": "string" },
            "items": {
                "type": "array",
                "minItems": 7,
                "maxItems": 7,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["questionType", "prompt", "options", "answer"],
                    "properties": {
                        "questionType": {
                            "type": "string",
                            "enum": [
                                "enToZh",
                                "zhToEn",
                                "chooseGloss",
                                "cloze",
                                "synonym",
                                "collocation",
                                "errorCorrection"
                            ]
                        },
                        "prompt": { "type": "string" },
                        "options": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "answer": { "type": "string" }
                    }
                }
            }
        }
    })
}

fn build_prompt(lemma: &str) -> String {
    format!(
        "Generate a vocabulary quiz for the English lemma `{lemma}` for a Chinese software engineer.\n\
         Return exactly 7 items, one per questionType: enToZh, zhToEn, chooseGloss, cloze, synonym, collocation, errorCorrection.\n\
         Rules:\n\
         - enToZh: show the English word, 4 Chinese options, answer is the correct Chinese meaning.\n\
         - zhToEn: show a Chinese prompt, no options, answer is the English lemma.\n\
         - chooseGloss: English stem + 4 English gloss options.\n\
         - cloze: one English sentence with a blank, answer is the lemma.\n\
         - synonym: ask for the closest synonym, 4 English options.\n\
         - collocation: ask which collocation is natural, 4 options, answer is the correct phrase.\n\
         - errorCorrection: give one English sentence with a lexical error involving this word family; answer is the corrected sentence.\n\
         Keep language natural. Do not invent extra fields."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_incomplete_card() {
        let raw = GeneratedCard {
            lemma: "feature".into(),
            items: vec![],
        };
        assert!(validate_generated("feature", raw).is_err());
    }
}
