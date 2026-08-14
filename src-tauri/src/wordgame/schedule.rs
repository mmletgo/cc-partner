//! 记单词调度与答题状态转移（纯函数）。
//!
//! Business Logic（为什么需要这个模块）:
//!     到期顺序、熟悉晋级、答错清题型必须可单测，不能散落在 SQL 与命令层。
//!
//! Code Logic（这个模块做什么）:
//!     给定词库快照与「今天」，选出下一题；应用答对/答错后返回新进度。

use super::models::{
    QuestionType, WordLemma, CORRECT_TODAY_CAP, CORRECT_TO_PASS, FAMILIAR_INTERVAL_DAYS,
};

/// 某词某题型的进度。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeProgress {
    pub lemma: String,
    pub question_type: QuestionType,
    pub correct_total: i64,
    pub correct_today: i64,
    pub last_correct_date: Option<String>,
}

/// 一张待出的卡。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueCard {
    pub lemma: String,
    pub question_type: QuestionType,
    pub total_count: i64,
}

/// 答题后的词级状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GradeOutcome {
    pub lemma: WordLemma,
    pub progress: TypeProgress,
    pub correct: bool,
}

/// 归一化当日进度：跨日则清零当天计数。
///
/// Business Logic:
///     当天上限按本地日计算，隔夜必须重置。
pub fn normalize_today(progress: &TypeProgress, today: &str) -> TypeProgress {
    let mut next = progress.clone();
    if progress.last_correct_date.as_deref() != Some(today) {
        next.correct_today = 0;
    }
    next
}

/// 该题型今天是否还能出。
pub fn type_available_today(progress: &TypeProgress, today: &str) -> bool {
    normalize_today(progress, today).correct_today < CORRECT_TODAY_CAP
}

/// 该词是否已熟悉（7 种题各满 2 次）。
pub fn is_familiar(progresses: &[TypeProgress]) -> bool {
    QuestionType::ALL.iter().all(|qt| {
        progresses
            .iter()
            .find(|p| p.question_type == *qt)
            .map(|p| p.correct_total >= CORRECT_TO_PASS)
            .unwrap_or(false)
    })
}

/// 词是否在今天到期（due_date <= today）。
pub fn is_due(lemma: &WordLemma, today: &str) -> bool {
    lemma.due_date.as_str() <= today
}

/// 从到期词里选出下一题。
///
/// Business Logic:
///     先到期，再词频高到低，再 lemma，再题型顺序；跳过当天已满 2 次的题型。
pub fn pick_next_card(
    lemmas: &[WordLemma],
    progresses: &[TypeProgress],
    today: &str,
) -> Option<DueCard> {
    let mut due: Vec<&WordLemma> = lemmas.iter().filter(|w| is_due(w, today)).collect();
    due.sort_by(|a, b| {
        b.total_count
            .cmp(&a.total_count)
            .then_with(|| a.lemma.cmp(&b.lemma))
    });
    for word in due {
        for qt in QuestionType::ALL {
            let progress = progresses
                .iter()
                .find(|p| p.lemma == word.lemma && p.question_type == qt)
                .cloned()
                .unwrap_or_else(|| TypeProgress {
                    lemma: word.lemma.clone(),
                    question_type: qt,
                    correct_total: 0,
                    correct_today: 0,
                    last_correct_date: None,
                });
            if type_available_today(&progress, today) {
                return Some(DueCard {
                    lemma: word.lemma.clone(),
                    question_type: qt,
                    total_count: word.total_count,
                });
            }
        }
    }
    None
}

/// 应用一次答题。
///
/// Business Logic:
///     答对累加题型计数，满 7×2 标熟悉并按阶梯拉长间隔；
///     答错只清当前题型，必要时取消熟悉并回到当天可出。
pub fn apply_answer(
    lemma: &WordLemma,
    progress: &TypeProgress,
    all_progress: &[TypeProgress],
    today: &str,
    user_correct: bool,
) -> GradeOutcome {
    let mut next_progress = normalize_today(progress, today);
    let mut next_lemma = lemma.clone();
    next_lemma.last_seen_at = Some(today.to_string());

    if user_correct {
        next_progress.correct_total += 1;
        next_progress.correct_today += 1;
        next_progress.last_correct_date = Some(today.to_string());
    } else {
        next_progress.correct_total = 0;
        next_progress.last_correct_date = Some(today.to_string());
    }

    let mut merged: Vec<TypeProgress> = all_progress
        .iter()
        .filter(|p| !(p.lemma == lemma.lemma && p.question_type == progress.question_type))
        .cloned()
        .collect();
    merged.push(next_progress.clone());
    let familiar_now = is_familiar(&merged);
    next_lemma.familiar = familiar_now;

    if user_correct && familiar_now {
        let next_step = (lemma.interval_step + 1).clamp(1, FAMILIAR_INTERVAL_DAYS.len() as i64);
        next_lemma.interval_step = next_step;
        let days =
            FAMILIAR_INTERVAL_DAYS[(next_step as usize - 1).min(FAMILIAR_INTERVAL_DAYS.len() - 1)];
        next_lemma.due_date = add_days(today, days);
    } else if !familiar_now {
        next_lemma.interval_step = 0;
        next_lemma.due_date = today.to_string();
    }

    GradeOutcome {
        lemma: next_lemma,
        progress: next_progress,
        correct: user_correct,
    }
}

/// 比较用户输入与标准答案。
///
/// Business Logic:
///     填空/改错允许大小写与常见标点差，避免因逗号扣分。
pub fn answers_match(expected: &str, actual: &str) -> bool {
    normalize_answer(expected) == normalize_answer(actual)
}

fn normalize_answer(raw: &str) -> String {
    raw.chars()
        .filter(|c| {
            !matches!(
                c,
                ',' | '.' | ';' | ':' | '!' | '?' | '"' | '\'' | '“' | '”' | '‘' | '’'
            )
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn add_days(today: &str, days: i64) -> String {
    let parsed = chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d")
        .unwrap_or_else(|_| chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch"));
    parsed
        .checked_add_signed(chrono::Duration::days(days))
        .unwrap_or(parsed)
        .format("%Y-%m-%d")
        .to_string()
}

/// 今天的本地日历日（YYYY-MM-DD）。
pub fn local_today() -> String {
    chrono::Local::now()
        .date_naive()
        .format("%Y-%m-%d")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(lemma: &str, count: i64, due: &str, familiar: bool) -> WordLemma {
        WordLemma {
            lemma: lemma.to_string(),
            total_count: count,
            familiar,
            interval_step: if familiar { 1 } else { 0 },
            due_date: due.to_string(),
            last_seen_at: None,
        }
    }

    fn empty_progress(lemma: &str, qt: QuestionType) -> TypeProgress {
        TypeProgress {
            lemma: lemma.to_string(),
            question_type: qt,
            correct_total: 0,
            correct_today: 0,
            last_correct_date: None,
        }
    }

    #[test]
    fn picks_higher_frequency_due_word_first() {
        let lemmas = vec![
            word("alpha", 3, "2026-08-14", false),
            word("zeta", 10, "2026-08-14", false),
            word("future", 99, "2026-08-20", false),
        ];
        let next = pick_next_card(&lemmas, &[], "2026-08-14").expect("card");
        assert_eq!(next.lemma, "zeta");
        assert_eq!(next.question_type, QuestionType::EnToZh);
    }

    #[test]
    fn wrong_answer_clears_only_current_type() {
        let lemma = word("feature", 5, "2026-08-14", false);
        let current = TypeProgress {
            lemma: "feature".into(),
            question_type: QuestionType::EnToZh,
            correct_total: 1,
            correct_today: 1,
            last_correct_date: Some("2026-08-14".into()),
        };
        let other = TypeProgress {
            lemma: "feature".into(),
            question_type: QuestionType::ZhToEn,
            correct_total: 2,
            correct_today: 0,
            last_correct_date: Some("2026-08-13".into()),
        };
        let out = apply_answer(
            &lemma,
            &current,
            &[current.clone(), other.clone()],
            "2026-08-14",
            false,
        );
        assert!(!out.correct);
        assert_eq!(out.progress.correct_total, 0);
        assert!(!out.lemma.familiar);
        assert_eq!(out.lemma.due_date, "2026-08-14");
    }

    #[test]
    fn familiar_after_all_types_pass_twice() {
        let lemma = word("feature", 5, "2026-08-14", false);
        let mut all: Vec<TypeProgress> = QuestionType::ALL
            .iter()
            .map(|qt| TypeProgress {
                lemma: "feature".into(),
                question_type: *qt,
                correct_total: 2,
                correct_today: 0,
                last_correct_date: Some("2026-08-13".into()),
            })
            .collect();
        all[0].correct_total = 1;
        let out = apply_answer(&lemma, &all[0], &all, "2026-08-14", true);
        assert!(out.lemma.familiar);
        assert_eq!(out.lemma.due_date, "2026-08-15");
        assert_eq!(out.lemma.interval_step, 1);
    }

    #[test]
    fn answer_compare_ignores_case_and_punct() {
        assert!(answers_match("Implement, please.", "implement please"));
        assert!(!answers_match("feature", "implement"));
    }

    #[test]
    fn today_cap_skips_type() {
        let lemmas = vec![word("feature", 5, "2026-08-14", false)];
        let mut progresses = vec![empty_progress("feature", QuestionType::EnToZh)];
        progresses[0].correct_today = 2;
        progresses[0].last_correct_date = Some("2026-08-14".into());
        let next = pick_next_card(&lemmas, &progresses, "2026-08-14").expect("card");
        assert_eq!(next.question_type, QuestionType::ZhToEn);
    }
}
