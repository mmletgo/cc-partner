//! orchestrator/browser_verification — 浏览器验证 evidence 适配
//!
//! Business Logic（为什么需要这个模块）:
//!     Orchestrator 需要把中立 BrowserVerificationEvidence 写成 task evidence，
//!     且非 Web 任务无 preview 时记 not_applicable，不误杀。
//!
//! Code Logic（这个模块做什么）:
//!     定义 evidence kind、摘要序列化与 not_applicable helper。

use crate::error::AppError;
use crate::workbench::browser_verification::models::BrowserVerificationEvidence;
use serde_json::json;

/// Orchestrator evidence kind：browserVerification。
pub const EVIDENCE_KIND_BROWSER_VERIFICATION: &str = "browserVerification";

/// 将浏览器 evidence 转为落库 content（仅摘要 + screenshot id，无 fill value）。
///
/// Business Logic（为什么需要这个函数）:
///     experiment/比较只消费结构化摘要，禁止页面正文与输入 value。
///
/// Code Logic（这个函数做什么）:
///     序列化 assertions/console error count/screenshot_id/url_path/truncated。
pub fn sanitize_evidence_content(evidence: &BrowserVerificationEvidence) -> Result<String, AppError> {
    let value = json!({
        "sessionId": evidence.session_id,
        "urlPath": evidence.url_path,
        "pageTitle": evidence.page_title,
        "assertionCount": evidence.assertions.len(),
        "assertions": evidence.assertions,
        "consoleErrorCount": evidence.console_errors.len(),
        "screenshotId": evidence.screenshot_id,
        "truncated": evidence.truncated,
        "capturedAt": evidence.captured_at,
    });
    Ok(serde_json::to_string(&value)?)
}

/// 无 preview 时的 not_applicable 摘要。
///
/// Business Logic（为什么需要这个函数）:
///     普通非 Web 任务不应因缺浏览器验证失败。
///
/// Code Logic（这个函数做什么）:
///     返回固定 JSON 字符串。
pub fn not_applicable_content(reason: &str) -> String {
    json!({
        "status": "not_applicable",
        "reason": reason,
    })
    .to_string()
}

/// 判断 evidence content 是否 not_applicable。
///
/// Business Logic（为什么需要这个函数）:
///     调度/展示需要区分跳过与真实失败。
///
/// Code Logic（这个函数做什么）:
///     解析 JSON status 字段。
pub fn is_not_applicable(content: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(|s| s == "not_applicable"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::browser_verification::models::BrowserAssertionResult;

    #[test]
    fn sanitize_does_not_include_fill_values() {
        let evidence = BrowserVerificationEvidence {
            session_id: "s1".into(),
            url_path: "/login".into(),
            page_title: Some("Login".into()),
            assertions: vec![BrowserAssertionResult {
                name: "loaded".into(),
                passed: true,
                detail: None,
            }],
            console_errors: vec![],
            screenshot_id: Some("art1".into()),
            truncated: false,
            captured_at: "t".into(),
        };
        let content = sanitize_evidence_content(&evidence).unwrap();
        assert!(!content.contains("value"));
        assert!(!content.contains("password"));
        assert!(content.contains("art1"));
        assert!(content.contains("browserVerification") || content.contains("sessionId"));
    }

    #[test]
    fn not_applicable_marker() {
        let c = not_applicable_content("no_preview");
        assert!(is_not_applicable(&c));
        assert!(!is_not_applicable(r#"{"status":"failed"}"#));
    }
}
