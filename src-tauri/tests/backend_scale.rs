//! S5 Backend Scale & Observability 集成测试。
//!
//! Business Logic: Task6 要求 mixed-version 组合在独立集成二进制中可过滤运行。
//! Code Logic: 委托 app_lib::mixed_version_harness 同步入口执行完整场景。

/// new↔new 仅 paged。
#[test]
fn cc_history_mixed_version_new_to_new_uses_only_paged_routes() {
    app_lib::mixed_version_harness::assert_new_to_new_uses_only_paged_routes();
}

/// new↔legacy 仅 legacy。
#[test]
fn cc_history_mixed_version_new_to_legacy_uses_only_legacy_routes() {
    app_lib::mixed_version_harness::assert_new_to_legacy_uses_only_legacy_routes();
}

/// 畸形 paged 失败本轮。
#[test]
fn cc_history_mixed_version_malformed_paged_fails_round_not_empty_success() {
    app_lib::mixed_version_harness::assert_malformed_paged_fails_round_not_empty_success();
}

/// legacy body 对新服务端仍可用。
#[test]
fn cc_history_mixed_version_legacy_bodies_work_against_new_server() {
    app_lib::mixed_version_harness::assert_legacy_bodies_work_against_new_server();
}
