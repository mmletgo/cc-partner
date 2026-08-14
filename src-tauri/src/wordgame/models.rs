//! wordgame 领域模型与题型枚举。
//!
//! Business Logic（为什么需要这个模块）:
//!     调度、仓储、出题和前端 DTO 必须共用同一套题型与进度语义，避免字符串漂移。
//!
//! Code Logic（这个模块做什么）:
//!     定义 7 种题型、卡片 JSON、lemma 行与预热状态；提供稳定 wire token。

use serde::{Deserialize, Serialize};

/// 记单词七种题型（调度与熟悉门槛按此全集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QuestionType {
    EnToZh,
    ZhToEn,
    ChooseGloss,
    Cloze,
    Synonym,
    Collocation,
    ErrorCorrection,
}

impl QuestionType {
    /// 固定调度顺序。
    pub const ALL: [QuestionType; 7] = [
        QuestionType::EnToZh,
        QuestionType::ZhToEn,
        QuestionType::ChooseGloss,
        QuestionType::Cloze,
        QuestionType::Synonym,
        QuestionType::Collocation,
        QuestionType::ErrorCorrection,
    ];

    /// 稳定 wire / SQLite token。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EnToZh => "enToZh",
            Self::ZhToEn => "zhToEn",
            Self::ChooseGloss => "chooseGloss",
            Self::Cloze => "cloze",
            Self::Synonym => "synonym",
            Self::Collocation => "collocation",
            Self::ErrorCorrection => "errorCorrection",
        }
    }

    /// 从 wire token 解析；未知值返回 None。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "enToZh" => Some(Self::EnToZh),
            "zhToEn" => Some(Self::ZhToEn),
            "chooseGloss" => Some(Self::ChooseGloss),
            "cloze" => Some(Self::Cloze),
            "synonym" => Some(Self::Synonym),
            "collocation" => Some(Self::Collocation),
            "errorCorrection" => Some(Self::ErrorCorrection),
            _ => None,
        }
    }

    /// 选择还是填空。
    pub fn kind(self) -> QuestionKind {
        match self {
            Self::EnToZh | Self::ChooseGloss | Self::Synonym | Self::Collocation => {
                QuestionKind::Choice
            }
            Self::ZhToEn | Self::Cloze | Self::ErrorCorrection => QuestionKind::Input,
        }
    }
}

/// 前端渲染分支。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QuestionKind {
    Choice,
    Input,
}

/// 单个缓存题目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct QuizItem {
    pub question_type: QuestionType,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    pub answer: String,
}

/// 一词 7 题缓存载荷。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CardPayload {
    pub lemma: String,
    pub items: Vec<QuizItem>,
}

/// lemma 词频与复习状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordLemma {
    pub lemma: String,
    pub total_count: i64,
    pub familiar: bool,
    pub interval_step: i64,
    pub due_date: String,
    pub last_seen_at: Option<String>,
}

/// 远端/本机增量计数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LemmaCount {
    pub lemma: String,
    pub count: i64,
}

/// ingest 水位。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestCursor {
    pub device_id: String,
    pub provider: String,
    pub session_id: String,
    pub record_id: String,
}

/// 预热单行状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreheatStatus {
    pub head_lemma: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub retry_count: i64,
}

/// 熟悉后的间隔天数阶梯。
pub const FAMILIAR_INTERVAL_DAYS: [i64; 6] = [1, 2, 4, 7, 15, 30];

/// 进入游戏所需的已缓存生词数。
pub const REQUIRED_CACHED_NEW_WORDS: usize = 10;

/// 开玩后缓存超前的生词数。
pub const PREHEAT_AHEAD: usize = 10;

/// 每种题型累计答对多少次算过关。
pub const CORRECT_TO_PASS: i64 = 2;

/// 某题型当天答对上限。
pub const CORRECT_TODAY_CAP: i64 = 2;
