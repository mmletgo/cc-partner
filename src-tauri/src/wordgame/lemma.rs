//! 英语词形还原（轻量规则 + 不规则表）。
//!
//! Business Logic（为什么需要这个模块）:
//!     implementing / implemented / Implement 应记成同一个词，否则词频被稀释。
//!
//! Code Logic（这个模块做什么）:
//!     小写后查不规则表；否则剥 -ies/-es/-s/-ing/-ed。-ing 只去后缀与重叠辅音，
//!     是否补 e 交给 lexicon 在候选里选择。

use std::collections::HashMap;
use std::sync::OnceLock;

/// 把表面词还原成首选 lemma。
///
/// Business Logic:
///     ingest 在词典过滤前需要稳定原形。
///
/// Code Logic:
///     不规则优先，再剥屈折后缀；失败返回小写原文。
#[cfg(test)]
pub fn lemmatize(word: &str) -> String {
    lemma_candidates(word)
        .into_iter()
        .next()
        .unwrap_or_else(|| word.to_ascii_lowercase())
}

/// 生成 lemma 候选（先不规则/去后缀，再尝试补 e）。
///
/// Business Logic:
///     making → mak/make，由词典决定收哪一个，避免 implementing → implemente。
///
/// Code Logic:
///     返回去重后的小写候选，首选是剥后缀结果。
pub fn lemma_candidates(word: &str) -> Vec<String> {
    let lower = word.to_ascii_lowercase();
    let primary = if let Some(mapped) = irregular_map().get(lower.as_str()) {
        (*mapped).to_string()
    } else if let Some(stem) = strip_inflection(&lower) {
        stem
    } else {
        lower.clone()
    };
    let mut out = vec![primary.clone()];
    if !primary.ends_with('e') && primary.len() >= 3 {
        out.push(format!("{primary}e"));
    }
    if primary != lower {
        out.push(lower);
    }
    out.dedup();
    out
}

fn irregular_map() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        HashMap::from([
            ("was", "be"),
            ("were", "be"),
            ("been", "be"),
            ("is", "be"),
            ("are", "be"),
            ("am", "be"),
            ("has", "have"),
            ("had", "have"),
            ("having", "have"),
            ("does", "do"),
            ("did", "do"),
            ("done", "do"),
            ("doing", "do"),
            ("went", "go"),
            ("gone", "go"),
            ("going", "go"),
            ("goes", "go"),
            ("made", "make"),
            ("making", "make"),
            ("makes", "make"),
            ("took", "take"),
            ("taken", "take"),
            ("taking", "take"),
            ("takes", "take"),
            ("came", "come"),
            ("coming", "come"),
            ("comes", "come"),
            ("saw", "see"),
            ("seen", "see"),
            ("seeing", "see"),
            ("sees", "see"),
            ("knew", "know"),
            ("known", "know"),
            ("knowing", "know"),
            ("knows", "know"),
            ("thought", "think"),
            ("thinking", "think"),
            ("thinks", "think"),
            ("got", "get"),
            ("gotten", "get"),
            ("getting", "get"),
            ("gets", "get"),
            ("gave", "give"),
            ("given", "give"),
            ("giving", "give"),
            ("gives", "give"),
            ("ran", "run"),
            ("running", "run"),
            ("runs", "run"),
            ("said", "say"),
            ("saying", "say"),
            ("says", "say"),
            ("told", "tell"),
            ("telling", "tell"),
            ("tells", "tell"),
            ("wrote", "write"),
            ("written", "write"),
            ("writing", "write"),
            ("writes", "write"),
            ("using", "use"),
            ("used", "use"),
            ("uses", "use"),
            ("children", "child"),
            ("men", "man"),
            ("women", "woman"),
            ("people", "person"),
            ("better", "good"),
            ("best", "good"),
            ("worse", "bad"),
            ("worst", "bad"),
        ])
    })
}

fn strip_inflection(word: &str) -> Option<String> {
    if word.len() < 5 {
        return None;
    }
    if let Some(stem) = word.strip_suffix("ies") {
        if stem.len() >= 2 {
            return Some(format!("{stem}y"));
        }
    }
    if word.ends_with("sses")
        || word.ends_with("shes")
        || word.ends_with("ches")
        || word.ends_with("xes")
        || word.ends_with("zes")
    {
        return Some(word[..word.len() - 2].to_string());
    }
    if let Some(stem) = word.strip_suffix("es") {
        if stem.len() >= 3 && !stem.ends_with('e') {
            return Some(stem.to_string());
        }
    }
    if let Some(stem) = word.strip_suffix('s') {
        if stem.len() >= 3 && !stem.ends_with('s') {
            return Some(stem.to_string());
        }
    }
    if let Some(stem) = word.strip_suffix("ing") {
        if stem.len() >= 3 {
            return Some(undouble_final_consonant(stem));
        }
    }
    if let Some(stem) = word.strip_suffix("ed") {
        if stem.len() >= 3 {
            if let Some(without_i) = stem.strip_suffix('i') {
                return Some(format!("{without_i}y"));
            }
            return Some(undouble_final_consonant(stem));
        }
    }
    None
}

fn undouble_final_consonant(stem: &str) -> String {
    let mut chars = stem.chars().rev();
    let last = chars.next();
    let prev = chars.next();
    if let (Some(a), Some(b)) = (last, prev) {
        if a == b && "bcdgklmnprst".contains(a) && stem.len() >= 4 {
            return stem[..stem.len() - 1].to_string();
        }
    }
    stem.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_common_inflections() {
        assert_eq!(lemmatize("implementing"), "implement");
        assert_eq!(lemmatize("implemented"), "implement");
        assert_eq!(lemmatize("cities"), "city");
        assert_eq!(lemmatize("running"), "run");
        assert_eq!(lemmatize("went"), "go");
        assert_eq!(lemmatize("children"), "child");
        assert!(lemma_candidates("making").contains(&"make".to_string()));
    }
}
