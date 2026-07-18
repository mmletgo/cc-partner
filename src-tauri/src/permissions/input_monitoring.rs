use serde::{Deserialize, Serialize};

/// 内部稳定版 Bundle Identifier；必须与 Tauri internal overlay 完全一致。
pub const INTERNAL_BUNDLE_IDENTIFIER: &str = "com.cc-partner.app.internal";
/// 内部开发版 Bundle Identifier；必须与 macOS Dev 壳生成脚本完全一致。
pub const INTERNAL_DEV_BUNDLE_IDENTIFIER: &str = "com.cc-partner.app.internal.dev";

/// 输入监控授权状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InputMonitoringState {
    /// IOHID 明确报告已授权。
    Granted,
    /// IOHID 明确报告已拒绝，用户必须显式打开系统设置。
    Denied,
    /// IOHID 尚未记录决定，允许用户显式请求一次系统授权。
    NotDetermined,
    /// 当前构建不是稳定内部签名主体，或系统返回未知状态。
    Unavailable,
}

/// 输入监控权限的前端 DTO。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputMonitoringPermissionState {
    /// 兼容消费方的快速判据；仅 `state=granted` 时为 true。
    pub granted: bool,
    /// 用于决定 Welcome 显示“请求授权”“打开设置”还是构建说明。
    pub state: InputMonitoringState,
}

impl From<InputMonitoringState> for InputMonitoringPermissionState {
    fn from(state: InputMonitoringState) -> Self {
        Self {
            granted: state == InputMonitoringState::Granted,
            state,
        }
    }
}

/// 判断当前进程是否属于可稳定登记的内部 TCC 主体。
///
/// 公开源码/ad-hoc 构建故意 fail closed，避免把用户带到没有应用条目的设置页。
pub(crate) fn is_supported_subject(bundle_id: Option<&str>) -> bool {
    matches!(
        bundle_id,
        Some(INTERNAL_BUNDLE_IDENTIFIER | INTERNAL_DEV_BUNDLE_IDENTIFIER)
    )
}

/// 把公开 IOHID 返回值映射为产品四态。
///
/// `supported=false` 或未知系统值一律返回 `Unavailable`，不猜测、不假绿。
pub(crate) fn state_from_raw(supported: bool, raw: u32) -> InputMonitoringState {
    if !supported {
        return InputMonitoringState::Unavailable;
    }
    match raw {
        0 => InputMonitoringState::Granted,
        1 => InputMonitoringState::Denied,
        2 => InputMonitoringState::NotDetermined,
        _ => InputMonitoringState::Unavailable,
    }
}

/// 输入监控 Request 路径本次真正执行的操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InputMonitoringOperation {
    /// 调用了一次公开 `IOHIDRequestAccess`。
    Request,
    /// 当前状态不允许请求，未产生系统副作用。
    Noop,
}

/// 输入监控显式 Request 的前后状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputMonitoringRequestResult {
    pub operation: InputMonitoringOperation,
    pub before: InputMonitoringState,
    pub after: InputMonitoringState,
}

/// 隔离公开 IOHID 系统调用，便于用纯内存 provider 验证状态机而不触碰宿主 TCC。
pub(crate) trait InputMonitoringAccessProvider {
    fn check(&self) -> u32;
    fn request(&self) -> bool;
}

/// 使用指定 provider 查询输入监控状态，不产生系统副作用。
pub(crate) fn check_with_provider<P: InputMonitoringAccessProvider>(
    provider: &P,
    supported: bool,
) -> InputMonitoringPermissionState {
    if !supported {
        return InputMonitoringState::Unavailable.into();
    }
    state_from_raw(true, provider.check()).into()
}

/// 在指定 IOHID provider 上执行一次显式 Request 状态机。
///
/// 仅 `NotDetermined` 调 Request；Denied/Granted/Unavailable 都是无副作用 noop。
pub(crate) fn request_with_provider<P: InputMonitoringAccessProvider>(
    provider: &P,
    supported: bool,
) -> InputMonitoringRequestResult {
    if !supported {
        return InputMonitoringRequestResult {
            operation: InputMonitoringOperation::Noop,
            before: InputMonitoringState::Unavailable,
            after: InputMonitoringState::Unavailable,
        };
    }

    let before = state_from_raw(true, provider.check());
    if before != InputMonitoringState::NotDetermined {
        return InputMonitoringRequestResult {
            operation: InputMonitoringOperation::Noop,
            before,
            after: before,
        };
    }

    let _ = provider.request();
    InputMonitoringRequestResult {
        operation: InputMonitoringOperation::Request,
        before,
        after: state_from_raw(true, provider.check()),
    }
}

#[cfg(target_os = "macos")]
#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDCheckAccess(request_type: u32) -> u32;
    fn IOHIDRequestAccess(request_type: u32) -> bool;
}

#[cfg(target_os = "macos")]
const K_IOHID_REQUEST_TYPE_LISTEN_EVENT: u32 = 1;

#[cfg(target_os = "macos")]
struct SystemInputMonitoringProvider;

#[cfg(target_os = "macos")]
impl InputMonitoringAccessProvider for SystemInputMonitoringProvider {
    fn check(&self) -> u32 {
        unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) }
    }

    fn request(&self) -> bool {
        unsafe { IOHIDRequestAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) }
    }
}

/// 查询当前进程的输入监控四态。
///
/// macOS 仅接受两个内部 Bundle ID；其它平台保持既有跨平台语义，视为已授权。
#[cfg(target_os = "macos")]
pub fn check_input_monitoring_state(bundle_id: Option<&str>) -> InputMonitoringPermissionState {
    check_with_provider(
        &SystemInputMonitoringProvider,
        is_supported_subject(bundle_id),
    )
}

#[cfg(not(target_os = "macos"))]
pub fn check_input_monitoring_state(_bundle_id: Option<&str>) -> InputMonitoringPermissionState {
    InputMonitoringState::Granted.into()
}

/// 显式请求输入监控授权。
///
/// 仅 macOS + 稳定内部主体 + NotDetermined 会调用一次公开 IOHID Request。
#[cfg(target_os = "macos")]
pub fn request_input_monitoring_access(bundle_id: Option<&str>) -> InputMonitoringRequestResult {
    request_with_provider(
        &SystemInputMonitoringProvider,
        is_supported_subject(bundle_id),
    )
}

#[cfg(not(target_os = "macos"))]
pub fn request_input_monitoring_access(_bundle_id: Option<&str>) -> InputMonitoringRequestResult {
    InputMonitoringRequestResult {
        operation: InputMonitoringOperation::Noop,
        before: InputMonitoringState::Granted,
        after: InputMonitoringState::Granted,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::{
        check_with_provider, is_supported_subject, request_with_provider, state_from_raw,
        InputMonitoringAccessProvider, InputMonitoringOperation, InputMonitoringState,
        INTERNAL_BUNDLE_IDENTIFIER, INTERNAL_DEV_BUNDLE_IDENTIFIER,
    };

    struct FakeProvider {
        state: Cell<u32>,
        state_after_request: u32,
        request_calls: Cell<usize>,
    }

    impl FakeProvider {
        fn new(state: u32, state_after_request: u32) -> Self {
            Self {
                state: Cell::new(state),
                state_after_request,
                request_calls: Cell::new(0),
            }
        }
    }

    impl InputMonitoringAccessProvider for FakeProvider {
        fn check(&self) -> u32 {
            self.state.get()
        }

        fn request(&self) -> bool {
            self.request_calls.set(self.request_calls.get() + 1);
            self.state.set(self.state_after_request);
            true
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Welcome 必须区分已授权、已拒绝、未登记和构建不可用，不能再用一个 bool
    ///     把“未登记”和“身份不稳定”混成同一条错误引导。
    ///
    /// Code Logic（这个测试做什么）:
    ///     把公开 IOHID API 的三个稳定返回值映射到前端四态；未知值 fail closed。
    #[test]
    fn maps_iohid_state_only_for_supported_subject() {
        assert_eq!(state_from_raw(false, 0), InputMonitoringState::Unavailable);
        assert_eq!(state_from_raw(true, 0), InputMonitoringState::Granted);
        assert_eq!(state_from_raw(true, 1), InputMonitoringState::Denied);
        assert_eq!(state_from_raw(true, 2), InputMonitoringState::NotDetermined);
        assert_eq!(state_from_raw(true, 99), InputMonitoringState::Unavailable);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     只有固定内部签名通道才是可稳定登记的 TCC 主体；公开源码/ad-hoc 构建不得假装
    ///     输入监控可用，否则“去设置”仍会打开一个没有 cc-partner 的空列表。
    ///
    /// Code Logic（这个测试做什么）:
    ///     仅接受内部稳定版和内部开发版两个精确 Bundle ID。
    #[test]
    fn accepts_only_internal_bundle_ids() {
        assert!(is_supported_subject(Some(INTERNAL_BUNDLE_IDENTIFIER)));
        assert!(is_supported_subject(Some(INTERNAL_DEV_BUNDLE_IDENTIFIER)));
        assert!(!is_supported_subject(Some("com.cc-partner.app")));
        assert!(!is_supported_subject(Some("app")));
        assert!(!is_supported_subject(None));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     已拒绝时系统不会再次弹授权框；产品必须等待用户点“打开系统设置”，不能偷偷
    ///     reset TCC、重签或反复请求。
    ///
    /// Code Logic（这个测试做什么）:
    ///     注入 IOHID=Denied，断言 Request 操作为 noop 且 provider 从未收到 request。
    #[test]
    fn denied_request_is_noop() {
        let provider = FakeProvider::new(1, 0);
        let result = request_with_provider(&provider, true);

        assert_eq!(result.operation, InputMonitoringOperation::Noop);
        assert_eq!(result.before, InputMonitoringState::Denied);
        assert_eq!(result.after, InputMonitoringState::Denied);
        assert_eq!(provider.request_calls.get(), 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     只有“未决定”状态允许请求一次，让 macOS 登记稳定签名的应用主体。
    ///
    /// Code Logic（这个测试做什么）:
    ///     注入 IOHID=NotDetermined，请求后切到 Denied，断言只调用一次公开 Request 并复查状态。
    #[test]
    fn not_determined_requests_exactly_once_then_rechecks() {
        let provider = FakeProvider::new(2, 1);
        let result = request_with_provider(&provider, true);

        assert_eq!(result.operation, InputMonitoringOperation::Request);
        assert_eq!(result.before, InputMonitoringState::NotDetermined);
        assert_eq!(result.after, InputMonitoringState::Denied);
        assert_eq!(provider.request_calls.get(), 1);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     社区/ad-hoc 构建不是稳定 TCC 主体，任何 Request 都只会制造空列表或漂移条目。
    ///
    /// Code Logic（这个测试做什么）:
    ///     即使底层伪装成 NotDetermined，只要主体不受支持就返回 unavailable/noop。
    #[test]
    fn unsupported_subject_never_requests() {
        let provider = FakeProvider::new(2, 0);
        let result = request_with_provider(&provider, false);

        assert_eq!(result.operation, InputMonitoringOperation::Noop);
        assert_eq!(result.before, InputMonitoringState::Unavailable);
        assert_eq!(result.after, InputMonitoringState::Unavailable);
        assert_eq!(provider.request_calls.get(), 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     `granted` 兼容字段只能由四态中的 Granted 推导，NotDetermined 不能因为系统 API
    ///     可调用就被误判为已授权。
    ///
    /// Code Logic（这个测试做什么）:
    ///     查询受支持主体的 IOHID=NotDetermined，断言 DTO 同时携带 state 且 granted=false。
    #[test]
    fn check_reports_not_determined_as_not_granted() {
        let provider = FakeProvider::new(2, 0);
        let result = check_with_provider(&provider, true);

        assert_eq!(result.state, InputMonitoringState::NotDetermined);
        assert!(!result.granted);
        assert_eq!(provider.request_calls.get(), 0);
    }
}
