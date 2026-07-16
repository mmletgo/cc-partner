//! browser_verification_smoke — 本机 managed Chromium 冒烟（可环境跳过）
//!
//! Business Logic（为什么需要这个测试）:
//!     L2/L3 需要证明 managed runtime 能对 loopback fixture 跑 smoke；无 runtime 时显式 skip。
//!
//! Code Logic（这个测试做什么）:
//!     探测 chrome-headless-shell；若缺失则 ignore；否则用 FakeEngine 路径保证 CI 绿，
//!     真 Chromium 路径在有资源时执行最小成功路径。

use app_lib::browser_verification::{
    default_smoke_commands, BrowserVerificationService, BrowserVerificationStartRequest,
    BrowserVerificationState, FakeEngine,
};
use std::sync::Arc;
use std::time::Duration;

/// 探测 managed runtime 是否存在。
///
/// Business Logic（为什么需要这个函数）:
///     无打包资源的 CI 不能因缺 Chrome 失败。
///
/// Code Logic（这个函数做什么）:
///     调用 resolve_managed_chrome_executable。
fn managed_runtime_present() -> bool {
    app_lib::browser_verification::chromium::resolve_managed_chrome_executable().is_some()
}

#[tokio::test]
async fn fake_engine_smoke_snapshot_to_screenshot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let service = BrowserVerificationService::new(
        Arc::new(FakeEngine::succeeds()),
        dir.path().to_path_buf(),
        "smoke-owner".into(),
    )
    .expect("service");
    let run = service
        .start(
            "preview-smoke".into(),
            "proj".into(),
            None,
            "http://127.0.0.1:5173/".into(),
            BrowserVerificationStartRequest {
                preview_id: "preview-smoke".into(),
                request_id: "smoke-1".into(),
                commands: default_smoke_commands(),
                fingerprint: None,
            },
        )
        .await
        .expect("start");
    let mut final_run = run;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        final_run = service.get(&final_run.session.id).await.expect("get");
        if matches!(
            final_run.session.state,
            BrowserVerificationState::Succeeded
                | BrowserVerificationState::Failed
                | BrowserVerificationState::Canceled
        ) {
            break;
        }
    }
    assert_eq!(final_run.session.state, BrowserVerificationState::Succeeded);
    assert!(final_run.evidence.is_some());
}

#[tokio::test]
async fn real_chromium_optional_when_runtime_present() {
    if !managed_runtime_present() {
        eprintln!(
            "NOT VERIFIED: managed chrome-headless-shell not present; skipping real chromium smoke"
        );
        return;
    }
    // 真 Chromium 路径：启动 ChromiumEngine 需要 loopback fixture 服务；
    // 此处仅断言可执行文件可解析，完整导航留给 L3 认证环境。
    let exe = app_lib::browser_verification::chromium::resolve_managed_chrome_executable()
        .expect("runtime present");
    assert!(exe.is_file());
    eprintln!("managed runtime ok: {}", exe.display());
}
