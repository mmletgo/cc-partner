//! 停用词与英语 lemma 允许表。
//!
//! Business Logic（为什么需要这个模块）:
//!     the/and 会永远占词频第一；代码清洗后仍可能漏进非英语词。词典 + 停用词保证闪卡可用。
//!
//! Code Logic（这个模块做什么）:
//!     编译期嵌入 txt；查询时对 lemma 候选做停用词与允许表过滤。

use std::collections::HashSet;
use std::sync::OnceLock;

use super::lemma::lemma_candidates;
use super::tokenize::extract_candidate_words;

const STOPWORDS_TXT: &str = include_str!("data/stopwords.txt");
const LEMMAS_TXT: &str = include_str!("data/english_lemmas.txt");

/// 把一段 assistant 正文变成可入库 lemma 及其出现次数。
///
/// Business Logic:
///     ingest 需要「去噪 → 原形 → 停用词/词典」一条路径。
///
/// Code Logic:
///     抽候选词，对每个词取 lemma 候选，命中允许表且非停用词则计数。
pub fn count_lemmas_in_text(text: &str) -> Vec<(String, i64)> {
    let mut counts = std::collections::BTreeMap::new();
    for token in extract_candidate_words(text) {
        if let Some(lemma) = accept_token(&token) {
            *counts.entry(lemma).or_insert(0) += 1;
        }
    }
    counts.into_iter().collect()
}

/// 判断一个表面词是否应收录，返回入库 lemma。
pub fn accept_token(token: &str) -> Option<String> {
    for candidate in lemma_candidates(token) {
        if is_stopword(&candidate) {
            return None;
        }
        if is_allowed_lemma(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_stopword(word: &str) -> bool {
    stopwords().contains(word)
}

fn is_allowed_lemma(word: &str) -> bool {
    allowed_lemmas().contains(word)
}

fn stopwords() -> &'static HashSet<String> {
    static SET: OnceLock<HashSet<String>> = OnceLock::new();
    SET.get_or_init(|| load_word_set(STOPWORDS_TXT))
}

fn allowed_lemmas() -> &'static HashSet<String> {
    static SET: OnceLock<HashSet<String>> = OnceLock::new();
    SET.get_or_init(|| load_word_set(LEMMAS_TXT))
}

fn load_word_set(raw: &str) -> HashSet<String> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.to_ascii_lowercase())
        .filter(|line| line.chars().all(|c| c.is_ascii_lowercase()) && line.len() >= 3)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_stopwords_and_keeps_content() {
        let counted = count_lemmas_in_text(
            "Please implement the feature. The implementer is implementing features.",
        );
        let map: std::collections::BTreeMap<_, _> = counted.into_iter().collect();
        assert!(map.get("implement").copied().unwrap_or(0) >= 2);
        assert!(map.get("feature").copied().unwrap_or(0) >= 1);
        assert!(!map.contains_key("the"));
        assert!(!map.contains_key("please"));
    }

    #[test]
    fn rejects_unknown_and_codey_tokens() {
        assert!(accept_token("xyzzyfoo").is_none());
        assert!(accept_token("useWorkbench").is_none());
    }
}
