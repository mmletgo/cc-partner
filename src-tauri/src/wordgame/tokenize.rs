//! 从 assistant 正文抽出候选英语词。
//!
//! Business Logic（为什么需要这个模块）:
//!     终端/jsonl 正文混着代码、路径和标识符；闪卡只应收「像真单词」的英文。
//!
//! Code Logic（这个模块做什么）:
//!     剥 fence/inline code 与 URL/路径后，按字母切词，拒绝 camelCase、snake_case、
//!     含数字和短全大写缩写。

/// 从一段 assistant 文本抽出小写候选词（尚未 lemma / 停用词 / 词典过滤）。
///
/// Business Logic:
///     ingest 需要稳定、可测的抽词，避免代码标识符污染词库。
///
/// Code Logic:
///     先剥代码与 URL，再按非字母切分，通过 `is_prose_word` 过滤。
pub fn extract_candidate_words(text: &str) -> Vec<String> {
    let stripped = strip_noise(text);
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in stripped.chars() {
        if ch.is_ascii_alphabetic() {
            current.push(ch);
            continue;
        }
        flush_token(&mut current, &mut out);
    }
    flush_token(&mut current, &mut out);
    out
}

fn flush_token(current: &mut String, out: &mut Vec<String>) {
    if current.is_empty() {
        return;
    }
    if is_prose_word(current) {
        out.push(current.to_ascii_lowercase());
    }
    current.clear();
}

/// 去掉 fence、inline code、URL 与常见路径。
fn strip_noise(text: &str) -> String {
    let without_fences = strip_fenced_code(text);
    let without_inline = strip_inline_code(&without_fences);
    strip_urls_and_paths(&without_inline)
}

fn strip_fenced_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("```") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 3..];
        if let Some(end) = after.find("```") {
            rest = &after[end + 3..];
        } else {
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out
}

fn strip_inline_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_code = false;
    for ch in text.chars() {
        if ch == '`' {
            in_code = !in_code;
            continue;
        }
        if !in_code {
            out.push(ch);
        }
    }
    out
}

fn strip_urls_and_paths(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for raw in text.split_whitespace() {
        let lower = raw.to_ascii_lowercase();
        if lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("www.")
            || raw.contains("://")
            || raw.contains('/')
            || raw.contains('\\')
            || looks_like_dotted_path(raw)
        {
            out.push(' ');
            continue;
        }
        out.push_str(raw);
        out.push(' ');
    }
    out
}

fn looks_like_dotted_path(token: &str) -> bool {
    let stripped = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '_');
    if !stripped.contains('.') {
        return false;
    }
    let ext = stripped.rsplit('.').next().unwrap_or("");
    matches!(
        ext,
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "json"
            | "md"
            | "css"
            | "html"
            | "toml"
            | "yml"
            | "yaml"
            | "lock"
            | "sql"
    )
}

/// 判断切出的 token 是否像散文单词。
///
/// Business Logic:
///     camelCase / SNAKE / 缩写不应进入闪卡。
fn is_prose_word(token: &str) -> bool {
    if token.len() < 3 || token.len() > 32 {
        return false;
    }
    if !token.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    if token.chars().all(|c| c.is_ascii_uppercase()) {
        return false;
    }
    let letters: Vec<char> = token.chars().collect();
    let has_lower = letters.iter().any(|c| c.is_ascii_lowercase());
    let has_upper_after_first = letters.iter().skip(1).any(|c| c.is_ascii_uppercase());
    if has_lower && has_upper_after_first {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_prose_and_drops_code_paths() {
        let text = "Please implement the feature in `src/App.tsx`.\n```\nfn foo() {}\n```\nSee https://example.com/docs and useWorktree.";
        let words = extract_candidate_words(text);
        assert!(words.contains(&"please".to_string()));
        assert!(words.contains(&"implement".to_string()));
        assert!(words.contains(&"feature".to_string()));
        assert!(!words
            .iter()
            .any(|w| w == "foo" || w == "useworktree" || w == "app"));
        assert!(!words.iter().any(|w| w.contains("tsx")));
    }

    #[test]
    fn rejects_short_and_all_caps() {
        assert!(!is_prose_word("to"));
        assert!(!is_prose_word("API"));
        assert!(is_prose_word("Implement"));
    }
}
